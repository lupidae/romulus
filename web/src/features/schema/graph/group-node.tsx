import type { Node, NodeProps } from "@xyflow/react";
import { memo } from "react";

import { cn } from "@/lib/utils";

import type { GroupColor } from "../layout";

export interface GroupNodeData extends Record<string, unknown> {
  name: string;
  color: GroupColor;
  /** The area of a schema (named in code type), not a group someone made. */
  schema?: boolean;
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

/** A tinted area behind a group's tables, sized to them, named at the top. */
function GroupNodeView({ data, width, height }: NodeProps<GroupFlowNode>) {
  return (
    <div
      className={cn("rounded-xl border-[0.125rem] border-dashed", TONE[data.color])}
      style={{ width: width ?? undefined, height: height ?? undefined }}
    >
      {/* Grows as the graph zooms out (--graph-zoom), within the padding
          above the tables; fades out where the area label takes over. */}
      <span
        className={cn(
          "block truncate px-4 pt-3 leading-none font-semibold",
          data.schema && "font-mono",
        )}
        style={{
          fontSize: "min(2.5rem, calc(1rem / var(--graph-zoom, 1)))",
          opacity: `calc(1 - ${AREA_LABEL_OPACITY})`,
        }}
      >
        {data.name}
      </span>
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
 */
function AreaLabelView({ data }: NodeProps<AreaLabelFlowNode>) {
  return (
    <div
      aria-hidden
      className={cn(
        "pointer-events-none absolute -translate-1/2 rounded-full border border-border bg-card/90 px-[0.6em] py-[0.2em] whitespace-nowrap shadow-raised",
        "leading-tight font-semibold",
        data.schema && "font-mono",
        LABEL_TONE[data.color],
      )}
      style={{ fontSize: "calc(0.9375rem / var(--graph-zoom, 1))", opacity: AREA_LABEL_OPACITY }}
    >
      {data.name}
    </div>
  );
}

export const AreaLabel = memo(AreaLabelView);
