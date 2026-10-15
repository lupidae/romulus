// Graph edits in flight in this tab: what the graph shows between an edit
// and the compile that confirms it (docs/MVP.md "Schema edits from the graph
// append SQL"). Derived, never stored as truth: each time the text or the
// compile changes, every edit is judged again from what the text holds and
// what the compiled model says, so undo, redo and a collaborator's typing
// all come out right.
//
// - pending: its statements are in the text, the model does not show them
//   yet: the graph shows the edit anyway (applyEdit), marked as pending;
// - failed: the compile of this very text reports an error inside its
//   statements: the graph still shows the edit (a drop excepted), with the
//   error on it, so nothing jumps while someone decides what to do;
// - reverting: undone (its statements left the text) while the model still
//   shows it: the graph shows the element as it was before the edit;
// - idle: nothing to show (confirmed, or undone and confirmed).

import { applyEdit, type EditMark, markOf, reflects } from "@/features/schema/edits/apply";
import type { GraphEdit } from "@/features/schema/edits/edits";
import { qualifiedName, type Entity, type Schema } from "@/features/schema/model";

import type { Diagnostic } from "./queries";

/** One graph edit made in this tab. */
export interface EditEntry {
  id: number;
  edit: GraphEdit;
  /** Its statements as appended: found again in the text by their text. */
  sql: string;
  /** The entities it changed, as they were and as they became (null: absent). */
  touched: { key: string; before: Entity | null; after: Entity | null }[];
}

export type EditState =
  | { status: "pending" }
  | { status: "failed"; error: string }
  | { status: "reverting" }
  | { status: "idle" };

/** What the judge needs to know about the compile. */
export interface CompileFacts {
  state: "ok" | "failed" | "unavailable" | "pending" | "waiting";
  /** The result is about another text than this tab's. */
  stale: boolean;
  /** Of this text (when not stale). */
  diagnostics: Diagnostic[];
  /** The last model that compiled. */
  model: Schema | null;
}

/** Entries kept for undo and redo; older ones are forgotten. */
export const KEPT_EDITS = 100;

/** The entities `after` differs from `before` in, by qualified name. */
export function touchedEntities(before: Schema, after: Schema): EditEntry["touched"] {
  const old = new Map((before.entities ?? []).map((e) => [qualifiedName(e), e]));
  const next = new Map((after.entities ?? []).map((e) => [qualifiedName(e), e]));
  const out: EditEntry["touched"] = [];
  for (const key of new Set([...old.keys(), ...next.keys()])) {
    const a = old.get(key) ?? null;
    const b = next.get(key) ?? null;
    if (a === b) continue;
    if (a && b && JSON.stringify(a) === JSON.stringify(b)) continue;
    out.push({ key, before: a, after: b });
  }
  return out;
}

/** Where an entry's statements are in the text: the last copy (appended last). */
function rangeOf(entry: EditEntry, text: string): { from: number; to: number } | null {
  const at = text.lastIndexOf(entry.sql);
  return at < 0 ? null : { from: at, to: at + entry.sql.length };
}

/** Every entry's state now. */
export function judge(
  entries: readonly EditEntry[],
  text: string,
  compile: CompileFacts,
): Map<number, EditState> {
  const out = new Map<number, EditState>();
  const current = !compile.stale && (compile.state === "ok" || compile.state === "failed");
  for (const entry of entries) {
    const range = rangeOf(entry, text);
    // The model of exactly this text: it shows whatever is in it.
    if (current && compile.state === "ok") {
      out.set(entry.id, { status: "idle" });
      continue;
    }
    const shown = compile.model ? reflects(compile.model, entry.edit) : false;
    if (!range) {
      out.set(entry.id, { status: shown === false ? "idle" : "reverting" });
      continue;
    }
    if (current) {
      const error = compile.diagnostics.find(
        (d) =>
          d.severity === "error" &&
          !!d.span &&
          d.span.utf16_start < range.to &&
          Math.max(d.span.utf16_end, d.span.utf16_start + 1) > range.from,
      );
      if (error) {
        out.set(entry.id, { status: "failed", error: error.message });
        continue;
      }
    }
    out.set(entry.id, { status: shown === true ? "idle" : "pending" });
  }
  return out;
}

/** The compiled model with pending edits applied and reverted ones undone. */
export function displayModel(
  model: Schema | null,
  entries: readonly EditEntry[],
  states: Map<number, EditState>,
): Schema | null {
  if (!model) return null;
  let out = model;
  // Undone ones first, newest first: back to before them…
  for (const entry of [...entries].reverse()) {
    if (states.get(entry.id)?.status !== "reverting") continue;
    out = revert(out, entry);
  }
  // …then the ones on their way (failed ones too: drops excepted), in order.
  for (const entry of entries) {
    const status = states.get(entry.id)?.status;
    const drop = entry.edit.kind === "dropTable" || entry.edit.kind === "dropColumn";
    if (status !== "pending" && !(status === "failed" && !drop)) continue;
    try {
      out = applyEdit(out, entry.edit);
    } catch (error) {
      console.error("draft: an edit does not apply to the model", error);
    }
  }
  return out;
}

/** The model with an edit's entities put back as they were. */
function revert(model: Schema, entry: EditEntry): Schema {
  const entities = [...(model.entities ?? [])];
  for (const { key, before, after } of entry.touched) {
    const at = entities.findIndex((e) => qualifiedName(e) === (after ? qualifiedName(after) : key));
    if (at >= 0) entities.splice(at, 1);
    if (before && !entities.some((e) => qualifiedName(e) === key)) entities.push(before);
  }
  return { ...model, entities };
}

/** An element's state, for the graph to draw. */
export interface MarkState {
  status: "pending" | "failed";
  /** The compile's message, for a failed edit. */
  message?: string;
  /** The edit, for "undo" on a failed one. */
  id: number;
  /** What the edit was, in a few words. */
  label: string;
}

export interface Marks {
  /** By table key, and `table\u0000column` for columns. */
  byElement: Map<string, MarkState>;
}

export const markKey = (mark: EditMark) =>
  mark.column === undefined ? mark.table : `${mark.table}\u0000${mark.column}`;

/** Where pending and failed edits show in the graph. The newest wins an element. */
export function marksOf(
  entries: readonly EditEntry[],
  states: Map<number, EditState>,
  label: (edit: GraphEdit) => string,
): Marks {
  const byElement = new Map<string, MarkState>();
  for (const entry of entries) {
    const state = states.get(entry.id);
    if (state?.status === "pending")
      byElement.set(markKey(markOf(entry.edit)), {
        status: "pending",
        id: entry.id,
        label: label(entry.edit),
      });
    else if (state?.status === "failed")
      byElement.set(markKey(markOf(entry.edit)), {
        status: "failed",
        message: state.error,
        id: entry.id,
        label: label(entry.edit),
      });
  }
  return { byElement };
}
