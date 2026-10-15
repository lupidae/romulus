// Presentation that is not schema: where tables sit and how they are grouped.
// In a draft both live in Yjs (`layout` and `groups` maps); a version stores a
// snapshot of them. Keys are qualified names (see `qualifiedName`).

export interface Position {
  x: number;
  y: number;
}

/** Top-left corner of each table, in graph coordinates, by qualified name. */
export type Layout = Record<string, Position>;

/**
 * Group colors are names, not values: the design tokens `--group-<color>` hold
 * the actual colors for both themes. An unknown name draws as `stone`.
 */
export const GROUP_COLORS = [
  "stone",
  "red",
  "amber",
  "green",
  "teal",
  "blue",
  "violet",
  "pink",
] as const;
export type GroupColor = (typeof GROUP_COLORS)[number];

export interface Group {
  name: string;
  color: string;
  /** Qualified names. A table in two groups is drawn in the first (by id). */
  members: string[];
}

/** By group id, as the draft's `groups` Y.Map holds them. */
export type Groups = Record<string, Group>;

export function groupColor(color: string): GroupColor {
  return (GROUP_COLORS as readonly string[]).includes(color) ? (color as GroupColor) : "stone";
}
