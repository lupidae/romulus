import { useReactFlow, useStoreApi, ViewportPortal } from "@xyflow/react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";

import { checkForeignKey } from "../../edits/inspect";
import { primaryKey } from "../../model";
import { useEdit } from "./context";

interface Point {
  x: number;
  y: number;
}

interface Drag {
  table: string;
  column: string;
  start: Point;
  pointer: Point;
  ok: boolean | null;
}

/** The column under a screen point, as `{table, column}`; a table's header means its primary key. */
function columnAt(x: number, y: number, keyOf: (table: string) => string | undefined) {
  const element = document.elementFromPoint(x, y);
  const node = element?.closest<HTMLElement>(".react-flow__node[data-id]");
  if (!element || !node) return null;
  const table = node.dataset.id!;
  const row = element.closest<HTMLElement>("[data-column]");
  if (row) return { table, column: row.dataset.column!, element: row };
  const header = element.closest<HTMLElement>("[data-table-header]");
  const pk = keyOf(table);
  if (header && pk) return { table, column: pk, element: header };
  return null;
}

/**
 * Drawing a foreign key: from a column's grip (the dot at the end of its row)
 * to the column it references. The line follows the pointer; the row under
 * it says whether it can be referenced; letting go there asks for ON DELETE
 * (and explains when it cannot be done). Esc, or letting go elsewhere, cancels.
 */
export function LinkDrag() {
  const edit = useEdit();
  const flowStore = useStoreApi();
  const flow = useReactFlow();
  const [drag, setDrag] = useState<Drag | null>(null);
  const editRef = useRef(edit);
  useLayoutEffect(() => {
    editRef.current = edit;
  }, [edit]);

  useEffect(() => {
    const pane = flowStore.getState().domNode;
    if (!pane) return;
    let current: Drag | null = null;
    let marked: HTMLElement | null = null;
    let over: { table: string; column: string } | null = null;

    const keyOf = (table: string) => {
      const entity = editRef.current?.graph.byKey.get(table)?.entity;
      const pk = entity && primaryKey(entity);
      return pk?.columns?.length === 1 ? pk.columns[0] : undefined;
    };
    const unmark = () => {
      marked?.removeAttribute("data-link-target");
      marked = null;
    };
    const move = (event: PointerEvent) => {
      const e = editRef.current;
      if (!current || !e) return;
      const pointer = flow.screenToFlowPosition({ x: event.clientX, y: event.clientY });
      const target = columnAt(event.clientX, event.clientY, keyOf);
      let ok: boolean | null = null;
      unmark();
      over = null;
      if (target && !(target.table === current.table && target.column === current.column)) {
        const from = e.graph.byKey.get(current.table)?.entity;
        const to = e.graph.byKey.get(target.table)?.entity;
        if (from && to) {
          ok = checkForeignKey(e.schema, from, current.column, to, target.column).ok;
          marked = target.element;
          marked.setAttribute("data-link-target", ok ? "ok" : "bad");
          over = { table: target.table, column: target.column };
        }
      }
      current = { ...current, pointer, ok };
      setDrag(current);
    };
    const end = (event: PointerEvent | KeyboardEvent) => {
      const done = current;
      current = null;
      unmark();
      setDrag(null);
      document.body.style.removeProperty("user-select");
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      window.removeEventListener("keydown", onKey, true);
      if (!done || event.type !== "pointerup" || !over) return;
      editRef.current?.start({
        kind: "foreignKey",
        table: done.table,
        column: done.column,
        to: over,
      });
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      over = null;
      end(event);
    };
    const down = (event: PointerEvent) => {
      if (event.button !== 0 || !editRef.current) return;
      const grip = (event.target as Element | null)?.closest<HTMLElement>("[data-link-grip]");
      const row = grip?.closest<HTMLElement>("[data-column]");
      const node = grip?.closest<HTMLElement>(".react-flow__node[data-id]");
      if (!grip || !row || !node) return;
      event.preventDefault();
      event.stopPropagation();
      const box = grip.getBoundingClientRect();
      const start = flow.screenToFlowPosition({
        x: box.x + box.width / 2,
        y: box.y + box.height / 2,
      });
      current = {
        table: node.dataset.id!,
        column: row.dataset.column!,
        start,
        pointer: start,
        ok: null,
      };
      setDrag(current);
      document.body.style.setProperty("user-select", "none");
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", end);
      window.addEventListener("pointercancel", end);
      window.addEventListener("keydown", onKey, true);
    };
    pane.addEventListener("pointerdown", down, true);
    return () => {
      pane.removeEventListener("pointerdown", down, true);
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", end);
      window.removeEventListener("pointercancel", end);
      window.removeEventListener("keydown", onKey, true);
      unmark();
    };
  }, [flowStore, flow]);

  if (!drag) return null;
  const { start, pointer } = drag;
  const bend = Math.max(40, Math.abs(pointer.x - start.x) / 2);
  return (
    <ViewportPortal>
      <svg
        aria-hidden
        className="pointer-events-none absolute top-0 left-0 z-50 overflow-visible"
        width={1}
        height={1}
      >
        <path
          d={`M ${start.x} ${start.y} C ${start.x + bend} ${start.y}, ${pointer.x - bend} ${pointer.y}, ${pointer.x} ${pointer.y}`}
          fill="none"
          strokeWidth={2}
          strokeDasharray={drag.ok === false ? "6 4" : undefined}
          vectorEffect="non-scaling-stroke"
          className={drag.ok === false ? "stroke-destructive" : "stroke-primary"}
        />
        <circle cx={start.x} cy={start.y} r={4} className="fill-primary" />
        <circle
          cx={pointer.x}
          cy={pointer.y}
          r={4}
          className={drag.ok === false ? "fill-destructive" : "fill-primary"}
        />
      </svg>
    </ViewportPortal>
  );
}
