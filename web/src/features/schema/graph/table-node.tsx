import { Handle, type Node, type NodeProps, Position, useStore } from "@xyflow/react";
import { memo, useMemo } from "react";

import {
  FingerprintIcon,
  HashtagIcon,
  KeyIcon,
  LinkIcon,
  NavArrowDownIcon,
  NavArrowRightIcon,
  SparksIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import { type ChangeStatus, statusGlyph } from "../diff";
import type { ColumnModel, EdgeModel, TableModel } from "./build";
import { hasFooter, NODE, OVERVIEW_ZOOM, showsNullable, visibleColumnCount } from "./geometry";
import { useGraphUi, useGraphUiStore } from "./graph-ui";
import { KIND_ICON, KIND_LABEL } from "./kind";
import { StatusBadge } from "./status";

export interface TableNodeData extends Record<string, unknown> {
  table: TableModel;
  multiSchema: boolean;
  /** Group-palette color of the table's schema, when the graph spans several. */
  schemaColor?: string;
  /** Every edge touching this table, to light up the columns they join. */
  edges: EdgeModel[];
  /** Diff mode: unchanged tables step back so changes stand out. */
  diffMode: boolean;
}

export type TableFlowNode = Node<TableNodeData, "table">;

const SCHEMA_CHIP: Record<string, string> = {
  stone: "bg-group-stone",
  red: "bg-group-red",
  amber: "bg-group-amber",
  green: "bg-group-green",
  teal: "bg-group-teal",
  blue: "bg-group-blue",
  violet: "bg-group-violet",
  pink: "bg-group-pink",
};

const ROW_STATUS: Record<ChangeStatus, string> = {
  added: "bg-success/8 before:bg-success",
  removed: "bg-destructive/6 before:bg-destructive text-muted-foreground",
  changed: "bg-warning/8 before:bg-warning",
};

const BORDER_STATUS: Record<ChangeStatus, string> = {
  added: "border-success/70",
  removed: "border-destructive/60 border-dashed",
  changed: "border-warning/70",
};

type Emphasis = "selected" | "neighbour" | "dimmed" | "normal";
const ELSEWHERE = "\u0000elsewhere";

function TableNodeView({ id, data, width, height }: NodeProps<TableFlowNode>) {
  const { table, multiSchema, schemaColor, edges, diffMode } = data;
  const store = useGraphUiStore();
  const overview = useStore((s) => s.transform[2] < OVERVIEW_ZOOM);
  const nodeState = useGraphUi((s) => s.nodeState[id]);
  // The selected table when it is this one or a neighbour, "elsewhere" when it
  // is unrelated: the node re-renders only when that answer changes.
  const related = useGraphUi((s) => {
    const focus = s.selected;
    if (!focus) return null;
    if (focus === id || edges.some((e) => e.source === focus || e.target === focus)) return focus;
    return ELSEWHERE;
  });
  const emphasis: Emphasis =
    related === null
      ? "normal"
      : related === id
        ? "selected"
        : related === ELSEWHERE
          ? "dimmed"
          : "neighbour";

  // Columns that join this table to the selected one (or, selected, to anything).
  const joined = useMemo(() => {
    const set = new Set<string>();
    if (!related || related === ELSEWHERE) return set;
    for (const e of edges) {
      if (e.kind !== "fk") continue;
      if (e.source === id && (related === id || e.target === related))
        e.columns.forEach((c) => set.add(c));
      if (e.target === id && (related === id || e.source === related))
        e.refColumns.forEach((c) => set.add(c));
    }
    return set;
  }, [edges, id, related]);

  const Icon = KIND_ICON[table.kind];
  const kindLabel = KIND_LABEL[table.kind];
  const isView = table.kind === "view" || table.kind === "materialized";
  const visible = visibleColumnCount(table, nodeState);
  const quiet = diffMode && !table.status && emphasis !== "selected";

  return (
    <div
      className={cn(
        "relative flex flex-col overflow-hidden rounded-lg border bg-card text-card-foreground",
        // Shadows are costly to paint 300 times and invisible when zoomed out.
        overview ? "shadow-none" : "shadow-raised",
        "transition-[opacity,box-shadow,border-color] duration-(--duration-fast)",
        table.status ? BORDER_STATUS[table.status] : "border-border-strong",
        emphasis === "selected" && "border-primary shadow-popover ring-3 ring-primary/20",
        emphasis === "dimmed" && "opacity-35",
        quiet && emphasis === "normal" && "opacity-60",
      )}
      style={{ width: width ?? undefined, height: height ?? undefined }}
    >
      {/* React Flow needs a handle on each side to draw an edge at all; the
          edges compute their own anchors from the column geometry. */}
      <Handle
        type="source"
        position={Position.Right}
        isConnectable={false}
        className="!opacity-0"
      />
      <Handle type="target" position={Position.Left} isConnectable={false} className="!opacity-0" />

      <div
        data-table-header=""
        className={cn(
          "flex shrink-0 items-center gap-2 border-b border-border px-3",
          isView ? "bg-muted" : "bg-card",
          overview && "border-b-0",
        )}
        style={{ height: NODE.header }}
      >
        {schemaColor ? (
          <span
            aria-hidden
            className={cn("size-2 shrink-0 rounded-xs", SCHEMA_CHIP[schemaColor])}
          />
        ) : null}
        <Icon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
        <span
          className={cn(
            "min-w-0 flex-1 truncate font-mono text-sm font-semibold text-foreground",
            table.status === "removed" && "line-through decoration-destructive/60",
            overview && "invisible",
          )}
        >
          {multiSchema && (
            <span className="font-normal text-faint-foreground">{table.schema}.</span>
          )}
          {table.name}
        </span>
        {kindLabel && !overview && (
          <span className="shrink-0 rounded-sm px-1.5 text-xs text-muted-foreground ring-1 ring-border-strong ring-inset">
            {kindLabel === "materialized view"
              ? "materialized"
              : kindLabel === "foreign table"
                ? "foreign"
                : kindLabel}
          </span>
        )}
        {table.status && <StatusBadge status={table.status} />}
      </div>

      {overview ? (
        <OverviewBody table={table} width={width ?? NODE.minWidth} />
      ) : (
        <>
          {table.comment && (
            <p
              className="shrink-0 truncate border-b border-border bg-muted/60 px-3 text-xs leading-5 text-muted-foreground"
              style={{ height: NODE.comment }}
            >
              {table.comment}
            </p>
          )}
          <ul className="shrink-0" style={{ paddingBlock: NODE.padding }}>
            {table.columns.slice(0, visible).map((c) => (
              <ColumnRow
                key={c.name}
                column={c}
                joined={joined.has(c.name)}
                diffMode={diffMode}
                nullable={showsNullable(table)}
              />
            ))}
          </ul>
          {hasFooter(table) && (
            <Footer
              table={table}
              hidden={table.columns.length - visible}
              partitionsOpen={nodeState?.partitions === true}
              onToggleColumns={() =>
                store.set({
                  nodeState: {
                    ...store.get().nodeState,
                    [id]: { ...nodeState, allColumns: !nodeState?.allColumns },
                  },
                })
              }
              onTogglePartitions={() =>
                store.set({
                  nodeState: {
                    ...store.get().nodeState,
                    [id]: { ...nodeState, partitions: !nodeState?.partitions },
                  },
                })
              }
            />
          )}
        </>
      )}
    </div>
  );
}

/**
 * Zoomed far out the rows are unreadable: the node shows its name instead,
 * sized against the zoom (`--graph-zoom`) so it reads like a map label.
 */
function OverviewBody({ table, width }: { table: TableModel; width: number }) {
  // As large as legible at this zoom, never wider than the node (a monospace
  // character is 0.6em wide). Long snake_case names break at an underscore.
  const lines = overviewLines(table.name);
  const longest = Math.max(...lines.map((l) => l.length));
  const fits = Math.floor((width - 32) / (longest * 0.62));
  return (
    <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-[0.25em] overflow-hidden px-3 text-center">
      <span
        className="max-w-full font-mono leading-tight font-semibold text-foreground"
        style={{ fontSize: `min(${Math.min(fits, 64)}px, calc(0.875rem / var(--graph-zoom, 1)))` }}
      >
        {lines.map((line, i) => (
          <span key={i} className="block truncate">
            {line}
          </span>
        ))}
      </span>
      <span
        className="text-faint-foreground"
        style={{ fontSize: "min(1.75rem, calc(0.6875rem / var(--graph-zoom, 1)))" }}
      >
        {table.columns.length} {table.columns.length === 1 ? "column" : "columns"}
      </span>
    </div>
  );
}

/** A name over 14 characters splits at its most central underscore. */
function overviewLines(name: string): string[] {
  if (name.length <= 14) return [name];
  let best = -1;
  for (let i = name.indexOf("_"); i >= 0; i = name.indexOf("_", i + 1)) {
    if (best < 0 || Math.abs(i - name.length / 2) < Math.abs(best - name.length / 2)) best = i;
  }
  return best < 0 ? [name] : [name.slice(0, best + 1), name.slice(best + 1)];
}

const ColumnRow = memo(function ColumnRow({
  column: c,
  joined,
  diffMode,
  nullable,
}: {
  column: ColumnModel;
  joined: boolean;
  diffMode: boolean;
  /** Mark nullable columns (not in views, where every column is). */
  nullable: boolean;
}) {
  return (
    <li
      data-column={c.name}
      className={cn(
        "relative flex items-center gap-1.5 pr-3 font-mono text-xs",
        diffMode ? "pl-4" : "pl-3",
        "before:absolute before:inset-y-0 before:left-0 before:w-0.5",
        c.status && ROW_STATUS[c.status],
        joined && !c.status && "bg-primary/8",
      )}
      style={{ height: NODE.row }}
    >
      {diffMode && (
        <span
          aria-hidden
          className={cn(
            "absolute left-1.5 w-2 text-center font-semibold",
            c.status === "added" && "text-success",
            c.status === "removed" && "text-destructive",
            c.status === "changed" && "text-warning",
          )}
        >
          {c.status ? statusGlyph(c.status) : ""}
        </span>
      )}
      <span className="flex w-3.5 shrink-0 justify-center [&_svg]:size-3.5">
        {c.pk ? (
          <KeyIcon aria-label="Primary key" className="text-primary" />
        ) : c.fk ? (
          <LinkIcon aria-label="Foreign key" className="text-info" />
        ) : null}
      </span>
      <span
        className={cn(
          "min-w-0 truncate",
          joined ? "font-semibold text-foreground" : "text-foreground",
          c.status === "removed" && "text-muted-foreground line-through decoration-destructive/60",
        )}
      >
        {c.name}
      </span>
      {(c.unique || c.identity || c.generated) && (
        <span className="flex shrink-0 items-center gap-1 text-faint-foreground [&_svg]:size-3">
          {c.unique && <FingerprintIcon aria-label="Unique" />}
          {c.identity && <HashtagIcon aria-label="Identity" />}
          {c.generated && <SparksIcon aria-label="Generated" />}
        </span>
      )}
      <span
        className={cn(
          "ml-auto min-w-0 shrink truncate pl-3 text-right",
          c.userType
            ? "text-syntax-type underline decoration-dotted underline-offset-2"
            : "text-faint-foreground",
        )}
      >
        {c.display}
        {nullable && !c.notNull && <span className="text-faint-foreground">?</span>}
      </span>
    </li>
  );
});

function Footer({
  table,
  hidden,
  partitionsOpen,
  onToggleColumns,
  onTogglePartitions,
}: {
  table: TableModel;
  hidden: number;
  partitionsOpen: boolean;
  onToggleColumns: () => void;
  onTogglePartitions: () => void;
}) {
  const manyColumns = table.columns.length > NODE.maxColumns;
  return (
    <div className="nodrag flex shrink-0 flex-col border-t border-border bg-muted/50">
      {manyColumns && (
        <Button
          variant="ghost"
          size="sm"
          onClick={onToggleColumns}
          className="h-7.5 w-full justify-start gap-1.5 rounded-none px-3 font-normal"
        >
          {hidden > 0 ? <NavArrowRightIcon /> : <NavArrowDownIcon />}
          {hidden > 0 ? `${hidden} more columns` : "Fewer columns"}
        </Button>
      )}
      {table.partitions.length > 0 && (
        <>
          <Button
            variant="ghost"
            size="sm"
            aria-expanded={partitionsOpen}
            onClick={onTogglePartitions}
            className="h-7.5 w-full justify-start gap-1.5 rounded-none px-3 font-normal"
          >
            {partitionsOpen ? <NavArrowDownIcon /> : <NavArrowRightIcon />}
            {table.partitions.length} {table.partitions.length === 1 ? "partition" : "partitions"}
            {table.partitionKey && (
              <span className="ml-auto min-w-0 truncate font-mono text-xs text-faint-foreground">
                {table.partitionKey}
              </span>
            )}
          </Button>
          {partitionsOpen && (
            <ul className="pb-1">
              {table.partitions.map((p) => (
                <li
                  key={p.key}
                  className={cn(
                    "flex items-center gap-2 pr-3 font-mono text-xs",
                    p.status === "removed" && "text-muted-foreground line-through",
                  )}
                  style={{ height: NODE.partitionRow, paddingLeft: `${0.75 + p.depth * 0.75}rem` }}
                >
                  {p.status && (
                    <span
                      aria-hidden
                      className={cn(
                        "w-2 font-semibold",
                        p.status === "added" && "text-success",
                        p.status === "removed" && "text-destructive",
                        p.status === "changed" && "text-warning",
                      )}
                    >
                      {statusGlyph(p.status)}
                    </span>
                  )}
                  <span className="min-w-0 truncate text-foreground">{p.name}</span>
                  <span className="ml-auto min-w-0 truncate text-faint-foreground">
                    {p.bound ?? "DEFAULT"}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </div>
  );
}

export const TableNode = memo(TableNodeView);
