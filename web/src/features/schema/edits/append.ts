// Where graph edits land in a draft's SQL: at the end, under one marker
// comment written the first time (docs/MVP.md "Schema edits from the graph
// append SQL"). Appending, never rewriting in place, means Postgres itself
// carries a rename to every constraint, index, comment and view, and a
// graph edit never fights someone typing in the middle of the text.

/** The line the first graph edit writes above itself. */
export const MARKER = "-- edits from the graph";

const MARKER_LINE = /^-- edits from the graph[ \t]*$/m;

export interface Append {
  /** Where to insert: the end of the text. */
  at: number;
  /** What to insert: separation, the marker if new, the statements. */
  insert: string;
  /** The statements' range in the text after the insert (no final newline). */
  from: number;
  to: number;
}

/** How to append statements to a text. */
export function appendTo(text: string, statements: readonly string[]): Append {
  let prefix = "";
  if (text.length > 0 && !text.endsWith("\n")) prefix += "\n";
  if (!MARKER_LINE.test(text)) {
    // A blank line between what is there and the marker.
    if (text.trim().length > 0 && !(text + prefix).endsWith("\n\n")) prefix += "\n";
    prefix += `${MARKER}\n`;
  }
  const body = statements.join("\n");
  const at = text.length;
  return {
    at,
    insert: `${prefix}${body}\n`,
    from: at + prefix.length,
    to: at + prefix.length + body.length,
  };
}
