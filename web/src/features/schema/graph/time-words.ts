import type { TimeMark } from "./build";

/**
 * A time mark in words, "added in v4, changed": what the color and the marks
 * say to the eye, for accessible names (tables, rows, edges).
 */
export function timeWords(time: TimeMark | undefined): string {
  if (!time) return "";
  // Removed before it appeared (version numbers): it comes back.
  const back = time.added !== undefined && time.removed !== undefined && time.removed < time.added;
  return [
    back && `removed in v${time.removed}, back in v${time.added}`,
    !back && time.added !== undefined && `added in v${time.added}`,
    !back && time.removed !== undefined && `removed in v${time.removed}`,
    time.changed && "changed",
    time.ghost && "gone at the cursor",
    time.later && "not there yet at the cursor",
  ]
    .filter(Boolean)
    .join(", ");
}
