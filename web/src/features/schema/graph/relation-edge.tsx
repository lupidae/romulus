import type { Edge, EdgeProps } from "@xyflow/react";
import { memo } from "react";

import { cn } from "@/lib/utils";

import type { ChangeStatus } from "../diff";
import type { EdgeModel } from "./build";
import type { Cardinality } from "./cardinality";
import { useGraphUi } from "./graph-ui";
import { type Route, roundedPath } from "./routing";

export interface RelationEdgeData extends Record<string, unknown> {
  model: EdgeModel;
  /** Computed by the graph from every table's position (see routing.ts). */
  route: Route;
}

export type RelationFlowEdge = Edge<RelationEdgeData, "relation">;

const STATUS_STROKE: Record<ChangeStatus, string> = {
  added: "stroke-success",
  removed: "stroke-destructive",
  changed: "stroke-warning",
};

/**
 * The crow's foot at one end of an edge, drawn outward from the node border at
 * (x, y) along `dir` (+1 right, -1 left). Nearest the table: the maximum (a
 * bar for one, the foot for many); further out: the minimum (a bar for one, a
 * circle for zero).
 */
export function EndMark({
  x,
  y,
  dir,
  mark,
}: {
  x: number;
  y: number;
  dir: 1 | -1;
  mark: Cardinality["parent"] | Cardinality["child"];
}) {
  const at = (d: number) => x + dir * d;
  const bar = (d: number) => `M ${at(d)} ${y - 6} L ${at(d)} ${y + 6}`;
  const circle = (d: number) => <circle cx={at(d)} cy={y} r={3.5} className="fill-card" />;
  switch (mark) {
    case "||":
      return <path d={`${bar(7)} ${bar(12)}`} />;
    // Mermaid writes zero-or-one `|o` on the parent side and `o|` on the
    // child side: the same mark, mirrored by where it sits.
    case "|o":
    case "o|":
      return (
        <>
          <path d={bar(7)} />
          {circle(16)}
        </>
      );
    case "o{":
      return (
        <>
          <path
            d={`M ${at(0)} ${y - 6} L ${at(12)} ${y} L ${at(0)} ${y + 6} M ${at(0)} ${y} L ${at(12)} ${y}`}
          />
          {circle(17)}
        </>
      );
  }
}

function RelationEdgeView({ id, source, target, data }: EdgeProps<RelationFlowEdge>) {
  const emphasis = useGraphUi((st) => {
    if (st.selected) return st.selected === source || st.selected === target ? "active" : "dimmed";
    if (st.hovered) return st.hovered === source || st.hovered === target ? "hover" : "normal";
    return "normal";
  });
  if (!data) return null;
  const { model, route } = data;
  const points = route.points;
  const S = points[0]!;
  const T = points[points.length - 1]!;
  const path = roundedPath(points);
  const reads = model.kind === "reads";

  const stroke = model.status
    ? STATUS_STROKE[model.status]
    : emphasis === "active"
      ? "stroke-primary"
      : emphasis === "hover"
        ? "stroke-foreground/60"
        : "stroke-border-strong";

  return (
    <g
      className={cn(
        "fill-none transition-opacity duration-(--duration-fast)",
        stroke,
        emphasis === "dimmed" && "opacity-20",
      )}
      data-edge={id}
    >
      <path
        d={path}
        className={cn(
          emphasis === "active" ? "stroke-2" : "stroke-[1.5]",
          (reads || model.status === "removed") && "[stroke-dasharray:5_4]",
        )}
      />
      {/* Hidden by CSS when zoomed out (`data-lod`): too small to read, costly to paint. */}
      {model.cardinality && (
        <g className="sg-marks stroke-[1.5]">
          <EndMark
            x={S.x}
            y={S.y}
            dir={route.sourceSide === "right" ? 1 : -1}
            mark={model.cardinality.child}
          />
          <EndMark
            x={T.x}
            y={T.y}
            dir={route.targetSide === "right" ? 1 : -1}
            mark={model.cardinality.parent}
          />
        </g>
      )}
    </g>
  );
}

export const RelationEdge = memo(RelationEdgeView);
