// Groups on the graph, as geometry and membership: where a group's frame is
// (around its tables, so it follows them), which group a dropped table lands
// in, and what the groups map becomes when tables join or leave one. Pure:
// the graph draws and drags with it, the draft session writes with it, and
// tests check it without a DOM.

import type { Group, Groups, Layout } from "../layout";
import { GROUP_PADDING } from "./geometry";
import type { Rect } from "./routing";

/**
 * The frame around these tables: their bounding box, padded for the label
 * above them. Null when none of them is placed yet.
 */
export function frameOf(boxes: Iterable<Rect>): Rect | null {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const b of boxes) {
    x0 = Math.min(x0, b.x);
    y0 = Math.min(y0, b.y);
    x1 = Math.max(x1, b.x + b.width);
    y1 = Math.max(y1, b.y + b.height);
  }
  if (x0 === Infinity) return null;
  return {
    x: x0 - GROUP_PADDING.side,
    y: y0 - GROUP_PADDING.top,
    width: x1 - x0 + GROUP_PADDING.side * 2,
    height: y1 - y0 + GROUP_PADDING.top + GROUP_PADDING.bottom,
  };
}

/**
 * The group each table is drawn in. A table should be in one group at most;
 * when the map says otherwise (two people grouping at once), the first group
 * by id wins, as the graph draws it.
 */
export function groupOfTable(groups: Groups): Map<string, string> {
  const out = new Map<string, string>();
  for (const id of Object.keys(groups).sort())
    for (const member of groups[id]!.members) if (!out.has(member)) out.set(member, id);
  return out;
}

/** The members a group is drawn with: the tables it lists that no earlier group has. */
export function drawnMembers(groups: Groups, owner = groupOfTable(groups)): Map<string, string[]> {
  const out = new Map<string, string[]>(Object.keys(groups).map((id) => [id, []]));
  for (const [table, id] of owner) out.get(id)!.push(table);
  return out;
}

const contains = (r: Rect, p: { x: number; y: number }) =>
  p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height;

/** The group under a point: the smallest frame holding it (frames can overlap), or null. */
export function groupAt(point: { x: number; y: number }, frames: Map<string, Rect>): string | null {
  let best: string | null = null;
  let area = Infinity;
  for (const [id, frame] of frames) {
    if (!contains(frame, point)) continue;
    const a = frame.width * frame.height;
    if (a < area || (a === area && best !== null && id < best)) {
      best = id;
      area = a;
    }
  }
  return best;
}

/** What a drop of tables does to their group: nothing, or the group they belong to now. */
export type DropMembership = { group: string | null } | null;

/**
 * A table dropped with the pointer at `point`: joins the group under it,
 * leaves its own when dropped outside it, or stays. `frames` are drawn
 * without the dragged tables (a group shrinks around the others while one
 * is dragged out of it). A group whose every member is being dragged is
 * carried along, not left: its own frame is missing from `frames`.
 */
export function dropMembership(
  current: string | null,
  point: { x: number; y: number },
  frames: Map<string, Rect>,
): DropMembership {
  const target = groupAt(point, frames);
  if (target === current) return null;
  if (target === null && current !== null && !frames.has(current)) return null;
  return { group: target };
}

/** A minimal change to the groups map: entries written, ids deleted. */
export interface GroupsChange {
  set: Record<string, Group>;
  deleted: string[];
}

/**
 * The groups once `tables` belong to `group` (null: to none). Every other
 * group lets go of them; a group left with no tables is deleted, as a
 * group of nothing would be invisible.
 */
export function regroup(
  groups: Groups,
  tables: readonly string[],
  group: string | null,
): GroupsChange {
  const moving = new Set(tables);
  const change: GroupsChange = { set: {}, deleted: [] };
  for (const [id, g] of Object.entries(groups)) {
    if (id === group) continue;
    const kept = g.members.filter((m) => !moving.has(m));
    if (kept.length === g.members.length) continue;
    if (kept.length === 0) change.deleted.push(id);
    else change.set[id] = { ...g, members: kept };
  }
  if (group !== null) {
    const target = groups[group];
    if (target) {
      const members = [...new Set([...target.members, ...tables])];
      if (members.length !== target.members.length) change.set[group] = { ...target, members };
    }
  }
  return change;
}

/** The change applied: the groups map it describes. */
export function applyGroupsChange(groups: Groups, change: GroupsChange): Groups {
  const out: Groups = { ...groups, ...change.set };
  for (const id of change.deleted) delete out[id];
  return out;
}

/**
 * A group moved by (dx, dy): its members' new positions, rounded as the
 * layout map stores them. Members without a position stay unplaced.
 */
export function movedMembers(
  positions: Layout,
  members: readonly string[],
  dx: number,
  dy: number,
): Layout {
  const out: Layout = {};
  for (const key of members) {
    const p = positions[key];
    if (p) out[key] = { x: Math.round(p.x + dx), y: Math.round(p.y + dy) };
  }
  return out;
}

/** A name for a new group that none of the others has: "Group 3". */
export function nextGroupName(groups: Groups): string {
  const names = new Set(Object.values(groups).map((g) => g.name));
  for (let n = Object.keys(groups).length + 1; ; n++)
    if (!names.has(`Group ${n}`)) return `Group ${n}`;
}
