// Where statements start and end in the console's text, for Explain (the
// statement under the cursor) and for Run with a cursor but no selection.
// A small scanner: quotes, dollar quotes, quoted identifiers and comments
// hide their semicolons. The playground host splits with Postgres's own
// parser; this only picks what to send.

export interface Piece {
  from: number;
  to: number;
  text: string;
}

/** The statements of `text`, trimmed, with their offsets (UTF-16, as CodeMirror counts). */
export function statements(text: string): Piece[] {
  const pieces: Piece[] = [];
  let start = 0;
  let i = 0;
  const push = (end: number) => {
    const raw = text.slice(start, end);
    const lead = raw.length - raw.trimStart().length;
    const trimmed = raw.trim();
    if (trimmed && !onlyComments(trimmed)) {
      pieces.push({ from: start + lead, to: start + lead + trimmed.length, text: trimmed });
    }
  };
  while (i < text.length) {
    const c = text[i];
    const next = text[i + 1];
    if (c === "-" && next === "-") {
      const end = text.indexOf("\n", i);
      i = end < 0 ? text.length : end + 1;
    } else if (c === "/" && next === "*") {
      const end = text.indexOf("*/", i + 2);
      i = end < 0 ? text.length : end + 2;
    } else if (c === "'" || c === '"') {
      i = closing(text, i, c);
    } else if (c === "$") {
      const tag = /^\$[A-Za-z_]*\$/.exec(text.slice(i))?.[0];
      if (tag) {
        const end = text.indexOf(tag, i + tag.length);
        i = end < 0 ? text.length : end + tag.length;
      } else i += 1;
    } else if (c === ";") {
      push(i);
      start = i + 1;
      i += 1;
    } else i += 1;
  }
  push(text.length);
  return pieces;
}

/** Past the quote opened at `i` (a doubled quote stays inside). */
function closing(text: string, i: number, quote: string): number {
  let j = i + 1;
  while (j < text.length) {
    if (text[j] === quote) {
      if (text[j + 1] === quote) j += 2;
      else return j + 1;
    } else j += 1;
  }
  return text.length;
}

function onlyComments(text: string): boolean {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .split("\n")
    .every((line) => line.trim() === "" || line.trim().startsWith("--"));
}

/** The statement containing (or just before) `offset`. */
export function statementAt(text: string, offset: number): Piece | null {
  const all = statements(text);
  if (all.length === 0) return null;
  return (
    all.find((p) => offset >= p.from && offset <= p.to + 1) ??
    [...all].reverse().find((p) => p.to <= offset) ??
    all[0] ??
    null
  );
}
