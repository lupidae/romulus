import type { EditorView } from "@codemirror/view";
import { useLayoutEffect, useMemo, useRef } from "react";

import { describe } from "@/features/schema/edits/edits";
import type { ElementMark, GraphEditor } from "@/features/schema/graph/editing/types";
import type { Schema } from "@/features/schema/model";

import { showGraphEdit } from "./collab";
import { displayModel, type EditEntry, judge, marksOf } from "./graph-edits";
import type { CompileView, DraftSession } from "./session";

const NO_EDITS: readonly EditEntry[] = [];

/**
 * The graph's side of schema edits in a draft: the model it shows (the
 * compiled one, with this tab's edits in flight applied), where edits are
 * pending or failed, and the editor that writes them into the doc. No
 * editor for someone who may not edit.
 */
export function useGraphEdits({
  session,
  compile,
  edits = NO_EDITS,
  sql,
  view,
  canEdit,
}: {
  session: DraftSession | null;
  compile: CompileView;
  edits?: readonly EditEntry[];
  sql: string;
  view: EditorView | null;
  canEdit: boolean;
}): { schema: Schema | null; editor: GraphEditor | undefined } {
  const states = useMemo(() => judge(edits, sql, compile), [edits, sql, compile]);
  // Judged again on every keystroke; the model and marks change only when a
  // judgement does (a new model redraws the whole graph).
  const verdict = [...states].map(([id, s]) => `${id}:${s.status}`).join(",");
  const schema = useMemo(
    () => displayModel(compile.model, edits, states),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `states` matters through `verdict`
    [compile.model, edits, verdict],
  );
  const latest = useMemo(() => marksOf(edits, states, describe).byElement, [edits, states]);
  const marksKey = [...latest]
    .map(([k, m]) => `${k}=${m.id}:${m.status}:${m.message ?? ""}`)
    .join("|");
  const marks: ReadonlyMap<string, ElementMark> = useMemo(
    () => latest,
    // eslint-disable-next-line react-hooks/exhaustive-deps -- a new map only when a mark changed
    [marksKey],
  );

  // Read when an edit is made, not when the editor is created.
  const shownRef = useRef(schema);
  const viewRef = useRef(view);
  useLayoutEffect(() => {
    shownRef.current = schema;
    viewRef.current = view;
  }, [schema, view]);
  const editor = useMemo<GraphEditor | undefined>(
    () =>
      session && canEdit
        ? {
            onEdit: (edit, options) => {
              const model = shownRef.current;
              if (!model) return;
              const entry = session.editSchema(edit, model, options?.position);
              requestAnimationFrame(() => {
                if (viewRef.current) showGraphEdit(viewRef.current, entry.sql);
              });
            },
            marks,
            onUndo: (id) => session.undoSchemaEdit(id),
            groups: {
              create: (members) => session.createGroup(members),
              update: (id, patch) => session.updateGroup(id, patch),
              remove: (id) => session.deleteGroup(id),
              assign: (tables, group) => session.assignGroup(tables, group),
              place: (positions, membership) => session.placeTables(positions, membership),
            },
          }
        : undefined,
    [session, canEdit, marks],
  );
  return { schema, editor };
}

export type { CompileView };
