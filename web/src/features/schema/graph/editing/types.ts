// Editing the schema from the graph: what the graph offers a draft's editors
// and what it hands back (docs/MVP.md "Schema edits from the graph append
// SQL"). The graph only emits `GraphEdit`s; turning them into SQL, sending
// them and tracking them is the draft's business.

import type { GraphEdit } from "../../edits/edits";
import type { Position } from "../../layout";

/** An element with an edit in flight, or one whose compile failed. */
export interface ElementMark {
  status: "pending" | "failed";
  /** The compile's message, when failed. */
  message?: string;
  /** The edit's id, to take it back. */
  id: number;
  /** What the edit was, in a few words. */
  label: string;
}

export interface GraphEditor {
  /** Apply an edit (already checked against the model). A new table goes at `position`. */
  onEdit: (edit: GraphEdit, options?: { position?: Position }) => void;
  /** Elements with an edit in flight or failed, by table key or `table\u0000column`. */
  marks?: ReadonlyMap<string, ElementMark>;
  /** Takes back the edit `id` (a failed one). */
  onUndo?: (id: number) => void;
}

/** What is being edited in place, if anything: one thing at a time. */
export type Editing =
  | { kind: "renameTable"; table: string }
  | { kind: "renameColumn"; table: string; column: string }
  | { kind: "addColumn"; table: string }
  | { kind: "type"; table: string; column: string }
  | { kind: "default"; table: string; column: string }
  | { kind: "comment"; table: string; column?: string }
  | { kind: "drop"; table: string; column?: string }
  /** A foreign key from a column; `to` once dropped on (or picked). */
  | { kind: "foreignKey"; table: string; column: string; to?: { table: string; column: string } }
  /** A table on its way: its name is asked for in place. */
  | { kind: "newTable"; position: Position }
  /** A failed edit's message, opened on its element. */
  | { kind: "failure"; table: string; column?: string };

export const markKey = (table: string, column?: string) =>
  column === undefined ? table : `${table}\u0000${column}`;
