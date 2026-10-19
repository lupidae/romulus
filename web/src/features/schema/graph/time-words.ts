import type { TimeMark } from "./build";

/**
 * A time mark in words, "added in v4, changed": what the tint and the marks
 * say to the eye, for accessible names (tables, rows, edges).
 */
export function timeWords(time: TimeMark | undefined): string {
  if (!time) return "";
  return [
    time.note?.startsWith("+") && `added in ${time.note.slice(1)}`,
    time.note?.startsWith("−") && `removed in ${time.note.slice(1)}`,
    time.changed && "changed",
  ]
    .filter(Boolean)
    .join(", ");
}
