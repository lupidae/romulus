// Conflict blocks that "Update draft" leaves in a draft's SQL, as the server
// writes them (crates/romulus-sandbox/src/merge.rs):
//
//   -- <<<<<<< yours [(both can be kept)]
//   …the draft's statements…
//   -- ======= v3
//   …the version's statements…
//   -- >>>>>>> v3
//
// The markers are SQL comments; the compiler refuses a text that has any.
// Offsets are string indices (UTF-16), like CodeMirror's and Y.Text's.

/** On the first marker when keeping both sides defines nothing twice. */
export const BOTH_NOTE = "(both can be kept)";
const LABEL = String.raw`v\d+`;
const MARKER = new RegExp(
  String.raw`^(?:(-- <<<<<<< yours)(?: (\(both can be kept\)))?|-- ======= (${LABEL})|-- >>>>>>> (${LABEL}))$`,
);

export interface Range {
  from: number;
  to: number;
}

export interface ConflictBlock extends Range {
  /** Between the first two markers (without the line break before the second). */
  yours: Range;
  /** Between the last two. */
  theirs: Range;
  /** How the markers name the version: `v3`. */
  label: string;
  /** Keeping both sides (theirs, then yours) is offered. */
  both: boolean;
}

export type Choice = "yours" | "theirs" | "both";

type Marker =
  | { kind: "yours"; both: boolean }
  | { kind: "theirs"; label: string }
  | { kind: "end"; label: string };

interface Line extends Range {
  /** Where the next line starts. */
  next: number;
}

const identifierChar = /[\p{L}\p{N}_$]/u;

/**
 * The lines that may hold a marker: those that start outside any string,
 * quoted name, dollar-quoted body, block comment and statement (after a
 * statement's `;`, before the next one's first token). The server reads
 * the same with Postgres's scanner and parser; the shared vectors
 * (`conflict-markers.json`) keep the two in step.
 */
function candidateLines(text: string): Line[] {
  const out: Line[] = [];
  let state: "code" | "single" | "escaped" | "double" | "dollar" | "block" | "line" = "code";
  let tag = "";
  let depth = 0;
  let inStatement = false;
  let lineStart = 0;
  let lineOpen = true; // the current line started in code, outside a statement
  for (let i = 0; i <= text.length; i++) {
    const c = text[i];
    if (c === "\n" || i === text.length) {
      if (lineOpen) out.push({ from: lineStart, to: i, next: Math.min(i + 1, text.length) });
      if (state === "line") state = "code";
      lineStart = i + 1;
      lineOpen = state === "code" && !inStatement;
      continue;
    }
    switch (state) {
      case "code": {
        const next = text[i + 1];
        if (c === "-" && next === "-") {
          state = "line";
          i++;
        } else if (c === "/" && next === "*") {
          state = "block";
          depth = 1;
          i++;
        } else if (c === "'") {
          const prefix = text[i - 1];
          state =
            (prefix === "E" || prefix === "e") && !identifierChar.test(text[i - 2] ?? "")
              ? "escaped"
              : "single";
          inStatement = true;
        } else if (c === '"') {
          state = "double";
          inStatement = true;
        } else if (c === "$" && !identifierChar.test(text[i - 1] ?? "")) {
          const open = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(text.slice(i));
          if (open) {
            state = "dollar";
            tag = open[0];
            i += tag.length - 1;
          }
          inStatement = true;
        } else if (c === ";") {
          inStatement = false;
        } else if (!/\s/.test(c ?? "")) {
          inStatement = true;
        }
        break;
      }
      case "single":
        if (c === "'") {
          if (text[i + 1] === "'") i++;
          else state = "code";
        }
        break;
      case "escaped":
        if (c === "\\") i++;
        else if (c === "'") {
          if (text[i + 1] === "'") i++;
          else state = "code";
        }
        break;
      case "double":
        if (c === '"') {
          if (text[i + 1] === '"') i++;
          else state = "code";
        }
        break;
      case "dollar":
        if (c === "$" && text.startsWith(tag, i)) {
          i += tag.length - 1;
          state = "code";
        }
        break;
      case "block":
        if (c === "/" && text[i + 1] === "*") {
          depth++;
          i++;
        } else if (c === "*" && text[i + 1] === "/") {
          depth--;
          i++;
          if (depth === 0) state = "code";
        }
        break;
      case "line":
        break;
    }
  }
  return out;
}

function readMarker(line: string): Marker | null {
  const m = MARKER.exec(line.trim());
  if (!m) return null;
  if (m[1]) return { kind: "yours", both: m[2] !== undefined };
  if (m[3]) return { kind: "theirs", label: m[3] };
  return { kind: "end", label: m[4]! };
}

/**
 * The blocks of a text and the marker lines that belong to none, as the
 * server reads them (`romulus_sandbox::merge::conflicts`): exact marker
 * lines only, between statements; yours, theirs and end in that order, the
 * last two with one label.
 */
export function scanConflicts(text: string): { blocks: ConflictBlock[]; stray: Range[] } {
  const blocks: ConflictBlock[] = [];
  const stray: Range[] = [];
  if (!text.includes("-- <<<<<<<") && !text.includes("-- =======") && !text.includes("-- >>>>>>>"))
    return { blocks, stray };
  const content = (from: number, to: number): Range => ({
    from,
    to: to > from && text[to - 1] === "\n" ? to - 1 : Math.max(to, from),
  });
  let open: { line: Line; marker: Marker }[] = [];
  const drop = () => {
    stray.push(...open.map(({ line }) => ({ from: line.from, to: line.to })));
    open = [];
  };
  for (const line of candidateLines(text)) {
    const marker = readMarker(text.slice(line.from, line.to));
    if (!marker) continue;
    const [first, second] = open;
    if (marker.kind === "yours") {
      drop();
      open = [{ line, marker }];
    } else if (marker.kind === "theirs" && open.length === 1 && first?.marker.kind === "yours") {
      open.push({ line, marker });
    } else if (
      marker.kind === "end" &&
      first?.marker.kind === "yours" &&
      second?.marker.kind === "theirs" &&
      second.marker.label === marker.label
    ) {
      blocks.push({
        from: first.line.from,
        to: line.to,
        yours: content(first.line.next, second.line.from),
        theirs: content(second.line.next, line.from),
        label: marker.label,
        both: first.marker.both,
      });
      open = [];
    } else {
      drop();
      stray.push({ from: line.from, to: line.to });
    }
  }
  drop();
  stray.sort((a, b) => a.from - b.from);
  return { blocks, stray };
}

/** The complete blocks of a text (stray markers are not blocks). */
export function findConflicts(text: string): ConflictBlock[] {
  return scanConflicts(text).blocks;
}

/**
 * What resolving a block puts in its place, and the range to replace (the
 * block, with its line break when nothing is kept).
 */
export function resolution(
  text: string,
  block: ConflictBlock,
  choice: Choice,
): { from: number; to: number; insert: string } {
  const yours = text.slice(block.yours.from, block.yours.to);
  const theirs = text.slice(block.theirs.from, block.theirs.to);
  const insert =
    choice === "yours"
      ? yours
      : choice === "theirs"
        ? theirs
        : [theirs, yours].filter((side) => side.length > 0).join("\n");
  const to = insert === "" && text[block.to] === "\n" ? block.to + 1 : block.to;
  return { from: block.from, to, insert };
}
