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

const START = "-- <<<<<<<";
const MIDDLE = "-- =======";
const END = "-- >>>>>>>";
/** On the first marker when keeping both sides defines nothing twice. */
export const BOTH_NOTE = "(both can be kept)";

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

interface Line extends Range {
  /** Where the next line starts. */
  next: number;
}

function lines(text: string): Line[] {
  const out: Line[] = [];
  let at = 0;
  while (at < text.length) {
    const end = text.indexOf("\n", at);
    const to = end === -1 ? text.length : end;
    out.push({ from: at, to, next: Math.min(to + 1, text.length) });
    at = to + 1;
  }
  return out;
}

/** The complete blocks of a text (stray markers are not blocks). */
export function findConflicts(text: string): ConflictBlock[] {
  if (!text.includes(START)) return [];
  const blocks: ConflictBlock[] = [];
  let start: Line | null = null;
  let middle: Line | null = null;
  const content = (from: number, to: number): Range => ({
    from,
    to: to > from && text[to - 1] === "\n" ? to - 1 : Math.max(to, from),
  });
  for (const line of lines(text)) {
    const body = text.slice(line.from, line.to).trim();
    if (body.startsWith(START)) {
      start = line;
      middle = null;
    } else if (body.startsWith(MIDDLE)) {
      if (start && !middle) middle = line;
      else start = middle = null;
    } else if (body.startsWith(END)) {
      if (start && middle) {
        blocks.push({
          from: start.from,
          to: line.to,
          yours: content(start.next, middle.from),
          theirs: content(middle.next, line.from),
          label: body.slice(END.length).trim(),
          both: text.slice(start.from, start.to).includes(BOTH_NOTE),
        });
      }
      start = middle = null;
    }
  }
  return blocks;
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
