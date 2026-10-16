import { type Node, type NodeProps, useStoreApi } from "@xyflow/react";
import { memo, useEffect, useRef, useState } from "react";

import { MoreHorizIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import type { GroupColor } from "../layout";
import { type EditContextValue, useEdit } from "./editing/context";
import { groupNameProblem, tablesLabel } from "./editing/group-labels";
import { InlineName } from "./editing/inline-name";
import { GROUP_PADDING } from "./geometry";
import { useGraphUi } from "./graph-ui";

export interface GroupNodeData extends Record<string, unknown> {
  name: string;
  color: GroupColor;
  /** The area of a schema (named in code type), not a group someone made. */
  schema?: boolean;
  /** A group someone made: its id in the groups map. */
  group?: string;
  /** How many tables it draws. */
  count?: number;
  /** The table being dragged joins it if dropped now. */
  dropTarget?: boolean;
}

// Not "group": React Flow styles a node type of that name itself.
export type GroupFlowNode = Node<GroupNodeData, "tableGroup">;

const TONE: Record<GroupColor, string> = {
  stone: "border-group-stone/35 bg-group-stone/6 text-group-stone",
  red: "border-group-red/35 bg-group-red/6 text-group-red",
  amber: "border-group-amber/35 bg-group-amber/6 text-group-amber",
  green: "border-group-green/35 bg-group-green/6 text-group-green",
  teal: "border-group-teal/35 bg-group-teal/6 text-group-teal",
  blue: "border-group-blue/35 bg-group-blue/6 text-group-blue",
  violet: "border-group-violet/35 bg-group-violet/6 text-group-violet",
  pink: "border-group-pink/35 bg-group-pink/6 text-group-pink",
};

const DROP_TONE: Record<GroupColor, string> = {
  stone: "border-solid border-group-stone/80 bg-group-stone/14",
  red: "border-solid border-group-red/80 bg-group-red/14",
  amber: "border-solid border-group-amber/80 bg-group-amber/14",
  green: "border-solid border-group-green/80 bg-group-green/14",
  teal: "border-solid border-group-teal/80 bg-group-teal/14",
  blue: "border-solid border-group-blue/80 bg-group-blue/14",
  violet: "border-solid border-group-violet/80 bg-group-violet/14",
  pink: "border-solid border-group-pink/80 bg-group-pink/14",
};

const HANDLE_TONE: Record<GroupColor, string> = {
  stone: "hover:bg-group-stone/8",
  red: "hover:bg-group-red/8",
  amber: "hover:bg-group-amber/8",
  green: "hover:bg-group-green/8",
  teal: "hover:bg-group-teal/8",
  blue: "hover:bg-group-blue/8",
  violet: "hover:bg-group-violet/8",
  pink: "hover:bg-group-pink/8",
};

/** Grows as the graph zooms out (--graph-zoom), within the padding above the tables. */
const LABEL_SIZE = "min(2.5rem, calc(1rem / var(--graph-zoom, 1)))";

/**
 * A tinted area behind a group's tables, sized to them, named at the top.
 * In a draft's editor its label is a handle: drag it to move the group with
 * all its tables, double-click the name to rename it, ⋯ or a right click for
 * the rest.
 */
function GroupNodeView({ data, width, height }: NodeProps<GroupFlowNode>) {
  const edit = useEdit();
  const editable = !!edit?.editor.groups && data.group !== undefined;
  return (
    <div
      data-group={data.group}
      data-color={data.color}
      data-drop-target={data.dropTarget ? "" : undefined}
      className={cn(
        "rounded-xl border-[0.125rem] border-dashed transition-colors duration-(--duration-fast)",
        TONE[data.color],
        data.dropTarget && DROP_TONE[data.color],
      )}
      style={{ width: width ?? undefined, height: height ?? undefined }}
    >
      {editable ? (
        <GroupHandle edit={edit} id={data.group!} data={data} />
      ) : (
        // Fades out where the area label takes over.
        <span
          className={cn(
            "block truncate px-4 pt-3 leading-none font-semibold",
            data.schema && "font-mono",
          )}
          style={{ fontSize: LABEL_SIZE, opacity: `calc(1 - ${AREA_LABEL_OPACITY})` }}
        >
          {data.name}
        </span>
      )}
    </div>
  );
}

/** Pointer travel (px) before a press on the label becomes a drag. */
const DRAG_THRESHOLD = 4;

/**
 * Dragging a group from one of its handles (the label on its frame, the area
 * label of a zoomed-out graph): the press becomes a drag past a few pixels,
 * Esc puts the group back.
 */
function useGroupDrag(edit: EditContextValue, id: string) {
  const flowStore = useStoreApi();
  const [dragging, setDragging] = useState(false);
  // A drag still running when the handle unmounts is put back.
  const stop = useRef<(() => void) | null>(null);
  useEffect(() => () => stop.current?.(), []);

  const onPointerDown = (event: React.PointerEvent<HTMLElement>) => {
    if (event.button !== 0 || stop.current) return;
    if ((event.target as Element).closest("button, input")) return;
    const zoom = flowStore.getState().transform[2];
    const from = { x: event.clientX, y: event.clientY };
    const pointer = event.pointerId;
    let started = false;
    let delta = { x: 0, y: 0 };
    const style = {
      userSelect: document.body.style.userSelect,
      cursor: document.body.style.cursor,
    };

    const move = (e: PointerEvent) => {
      if (e.pointerId !== pointer) return;
      if (!started && Math.hypot(e.clientX - from.x, e.clientY - from.y) < DRAG_THRESHOLD) return;
      delta = { x: (e.clientX - from.x) / zoom, y: (e.clientY - from.y) / zoom };
      if (!started) {
        started = true;
        setDragging(true);
        document.body.style.userSelect = "none";
        document.body.style.cursor = "grabbing";
        edit.dragGroup(id, "start");
      }
      edit.dragGroup(id, "move", delta.x, delta.y);
    };
    const finish = (phase: "end" | "cancel") => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("keydown", key, true);
      stop.current = null;
      document.body.style.userSelect = style.userSelect;
      document.body.style.cursor = style.cursor;
      setDragging(false);
      if (started) edit.dragGroup(id, phase, delta.x, delta.y);
    };
    const up = (e: PointerEvent) => e.pointerId === pointer && finish("end");
    const cancel = (e: PointerEvent) => e.pointerId === pointer && finish("cancel");
    const key = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      finish("cancel");
    };
    stop.current = () => finish("cancel");
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    // Capture: Esc puts the group back before the graph lets go of the selection.
    window.addEventListener("keydown", key, true);
  };
  return { onPointerDown, dragging };
}

function GroupHandle({
  edit,
  id,
  data,
}: {
  edit: EditContextValue;
  id: string;
  data: GroupNodeData;
}) {
  const renaming = useGraphUi((s) => s.editing?.kind === "renameGroup" && s.editing.group === id);
  const { onPointerDown, dragging } = useGroupDrag(edit, id);

  return (
    // A pointer handle: the keyboard reaches the same actions through the
    // toolbar's groups list.
    <div
      data-group-handle=""
      className={cn(
        "nopan group/handle flex rounded-t-[0.625rem] transition-colors duration-(--duration-fast)",
        renaming ? "cursor-auto" : dragging ? "cursor-grabbing" : "cursor-grab",
        !renaming && HANDLE_TONE[data.color],
      )}
      style={{ height: GROUP_PADDING.top - GROUP_PADDING.side / 2 }}
      onPointerDown={renaming ? undefined : onPointerDown}
    >
      <div
        className="flex min-w-0 items-center gap-[0.4em] px-4 pt-3 leading-none"
        style={{ fontSize: LABEL_SIZE, opacity: `calc(1 - ${AREA_LABEL_OPACITY})` }}
      >
        {renaming ? (
          <InlineName
            aria-label={`Rename ${data.name}`}
            initial={data.name}
            validate={groupNameProblem}
            onCommit={(name) => {
              edit.editor.groups?.update(id, { name });
              edit.stop();
            }}
            onCancel={edit.stop}
            className="-my-1 h-[1.5em] min-w-[8em] text-[length:inherit] font-semibold sm:h-[1.5em] sm:text-[length:inherit]"
          />
        ) : (
          <span
            className="min-w-0 truncate font-semibold"
            onDoubleClick={(event) => {
              event.stopPropagation();
              edit.start({ kind: "renameGroup", group: id });
            }}
          >
            {data.name}
          </span>
        )}
        {!renaming && (
          <>
            <span className="shrink-0 text-[0.75em] font-medium whitespace-nowrap tabular opacity-75">
              {tablesLabel(data.count ?? 0)}
            </span>
            <Button
              variant="ghost"
              size="icon-sm"
              tabIndex={-1}
              aria-label={`Actions for ${data.name}`}
              title="Group actions"
              onMouseDown={(event) => event.preventDefault()}
              onClick={(event) => {
                event.stopPropagation();
                const box = event.currentTarget.getBoundingClientRect();
                edit.openMenu({ group: id, x: box.left, y: box.bottom });
              }}
              className={cn(
                "size-[1.5em] shrink-0 text-current opacity-0 hover:bg-current/10 hover:text-current [&_svg]:size-[0.9em]",
                "group-hover/handle:opacity-100 focus-visible:opacity-100 pointer-coarse:opacity-100",
              )}
            >
              <MoreHorizIcon />
            </Button>
          </>
        )}
      </div>
    </div>
  );
}

export const GroupNode = memo(GroupNodeView);

/** 0 above 18% zoom, 1 below 12%: when tables become dots, areas name themselves. */
const AREA_LABEL_OPACITY = "clamp(0, (0.18 - var(--graph-zoom, 1)) * 16, 1)";

export type AreaLabelFlowNode = Node<GroupNodeData, "areaLabel">;

const LABEL_TONE: Record<GroupColor, string> = {
  stone: "text-group-stone",
  red: "text-group-red",
  amber: "text-group-amber",
  green: "text-group-green",
  teal: "text-group-teal",
  blue: "text-group-blue",
  violet: "text-group-violet",
  pink: "text-group-pink",
};

/**
 * A group's (or schema's) name over the middle of its area, drawn above the
 * tables and sized against the zoom: the map labels of a zoomed-out graph.
 * A group's is a handle there too (the frame's own label is too small to
 * grab): see `sg-area-handle` in schema-graph.css.
 */
function AreaLabelView({ data }: NodeProps<AreaLabelFlowNode>) {
  const edit = useEdit();
  const editable = !!edit?.editor.groups && data.group !== undefined;
  const className = cn(
    "absolute -translate-1/2 rounded-full border border-border bg-card/90 px-[0.6em] py-[0.2em] whitespace-nowrap shadow-raised",
    "leading-tight font-semibold",
    data.schema && "font-mono",
    LABEL_TONE[data.color],
  );
  const style = { fontSize: "calc(0.9375rem / var(--graph-zoom, 1))", opacity: AREA_LABEL_OPACITY };
  if (!editable) {
    return (
      <div aria-hidden className={cn("pointer-events-none", className)} style={style}>
        {data.name}
      </div>
    );
  }
  return (
    <AreaHandle edit={edit} id={data.group!} name={data.name} className={className} style={style} />
  );
}

function AreaHandle({
  edit,
  id,
  name,
  className,
  style,
}: {
  edit: EditContextValue;
  id: string;
  name: string;
  className: string;
  style: React.CSSProperties;
}) {
  const { onPointerDown, dragging } = useGroupDrag(edit, id);
  return (
    // The pointer's way to move a group at a distance; the keyboard has the groups list.
    <div
      aria-hidden
      data-area-handle=""
      className={cn(
        "sg-area-handle nopan pointer-events-none",
        dragging && "cursor-grabbing",
        className,
      )}
      style={style}
      onPointerDown={onPointerDown}
    >
      {name}
    </div>
  );
}

export const AreaLabel = memo(AreaLabelView);
