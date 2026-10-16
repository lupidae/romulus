import { createContext, useContext } from "react";

import type { GraphEdit } from "../../edits/edits";
import type { Position } from "../../layout";
import type { Schema } from "../../model";
import type { GraphModel } from "../build";
import type { Editing, GraphEditor } from "./types";

/** Where the actions menu opens, and on what. */
export interface MenuTarget {
  /** A table (and one of its columns), a group, or the canvas (a new table there). */
  table?: string;
  column?: string;
  group?: string;
  /** In graph coordinates, for "add table here". */
  at?: Position;
  /** Where the menu shows, in screen pixels. */
  x: number;
  y: number;
}

/** What the graph's editing parts share: the model, and how to act on it. */
export interface EditContextValue {
  schema: Schema;
  graph: GraphModel;
  editor: GraphEditor;
  /** Opens an editor in place (zooming to the table first if it is too small to read). */
  start: (editing: Editing) => void;
  /** Closes it, and gives the keys back to the graph. */
  stop: () => void;
  /** Sends an edit, closes the editor, keys back to the graph. */
  emit: (edit: GraphEdit, options?: { position?: Position }) => void;
  /** The element a table (or column) is drawn as, for anchoring popovers. */
  element: (table: string, column?: string) => Element | null;
  openMenu: (target: MenuTarget) => void;
  /** The center of the visible graph, in graph coordinates. */
  center: () => Position;
  /** Groups these tables (picked ones let go) and starts naming the group. */
  makeGroup: (tables: string[]) => void;
  /** Sets the picked tables (shift-click's list). */
  pick: (tables: string[]) => void;
  /**
   * A group dragged by its label: (dx, dy) from where the drag started, in
   * graph units. "end" writes the move; "cancel" (Esc) puts it back.
   */
  dragGroup: (
    id: string,
    phase: "start" | "move" | "end" | "cancel",
    dx?: number,
    dy?: number,
  ) => void;
}

export const EditContext = createContext<EditContextValue | null>(null);

/** The graph's editing context; null when the graph is read-only. */
export function useEdit(): EditContextValue | null {
  return useContext(EditContext);
}

/** Whether a relation's schema can be edited from the graph at all (not a foreign table's). */
export function editableKind(entity: { kind: string }): boolean {
  return entity.kind !== "f";
}

/** A measurable stand-in for an element that may re-render: popovers follow it. */
export function virtualAnchor(find: () => Element | null) {
  return {
    current: {
      getBoundingClientRect: () =>
        find()?.getBoundingClientRect() ?? new DOMRect(-9999, -9999, 0, 0),
    },
  };
}
