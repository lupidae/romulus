// What groups are called and colored as in the editing UI (menus, the
// groups panel, frames): one place for the words and the swatches.

import type { GroupColor } from "../../layout";

export const SWATCH: Record<GroupColor, string> = {
  stone: "bg-group-stone",
  red: "bg-group-red",
  amber: "bg-group-amber",
  green: "bg-group-green",
  teal: "bg-group-teal",
  blue: "bg-group-blue",
  violet: "bg-group-violet",
  pink: "bg-group-pink",
};

export const colorLabel = (color: string) => color[0]!.toUpperCase() + color.slice(1);

export const tablesLabel = (n: number) => `${n} ${n === 1 ? "table" : "tables"}`;

export const GROUP_NAME_MAX = 60;

/** Why a group name cannot be used, or null. */
export function groupNameProblem(name: string): string | null {
  if (!name.trim()) return "A group needs a name.";
  if (name.trim().length > GROUP_NAME_MAX) return `${GROUP_NAME_MAX} characters at most.`;
  return null;
}
