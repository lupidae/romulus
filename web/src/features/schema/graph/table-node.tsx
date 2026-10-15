import { Handle, type Node, type NodeProps, Position } from "@xyflow/react";
import { memo, useEffect, useMemo, useState } from "react";

import {
  FingerprintIcon,
  HashtagIcon,
  KeyIcon,
  LinkIcon,
  MoreHorizIcon,
  NavArrowDownIcon,
  NavArrowRightIcon,
  PlusIcon,
  SparksIcon,
  WarningTriangleIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import { type ChangeStatus, statusGlyph } from "../diff";
import { columnNameTaken, relationNameTaken } from "../edits/inspect";
import { targetOf } from "../edits/plan";
import { nameProblem } from "../edits/quote";
import type { ColumnModel, EdgeModel, TableModel } from "./build";
import { requestDetail } from "./detail-scheduler";
import { type EditContextValue, useEdit } from "./editing/context";
import { InlineName } from "./editing/inline-name";
import { type ElementMark, markKey } from "./editing/types";
import { hasFooter, NODE, showsNullable, visibleColumnCount } from "./geometry";
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
  /** Its schema can be edited from the graph (a draft's editors; not foreign tables). */
  editable?: boolean;
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
  const edit = useEdit();
  const canEdit = !!edit && data.editable === true;
  const renaming = useGraphUi(
    (s) => canEdit && s.editing?.kind === "renameTable" && s.editing.table === id,
  );
  // The + and ⋯ of the header: on the table under the pointer or selected.
  const tools = useGraphUi((s) => canEdit && !s.editing && (s.hovered === id || s.selected === id));
  const mark = useGraphUi((s) => (canEdit ? s.marks.get(id) : undefined));

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

  const picked = useGraphUi((s) => s.picked.includes(id));
  const Icon = KIND_ICON[table.kind];
  const kindLabel = KIND_LABEL[table.kind];
  const isView = table.kind === "view" || table.kind === "materialized";
  const quiet = diffMode && !table.status && emphasis !== "selected";

  return (
    <div
      data-selected={emphasis === "selected" ? "" : undefined}
      data-picked={picked ? "" : undefined}
      className={cn(
        "sg-node relative flex flex-col overflow-hidden rounded-lg border bg-card text-card-foreground shadow-raised",
        "transition-[opacity,box-shadow,border-color] duration-(--duration-fast)",
        table.status ? BORDER_STATUS[table.status] : "border-border-strong",
        emphasis === "selected" && "border-primary shadow-popover ring-3 ring-primary/20",
        picked && "border-primary ring-2 ring-primary/45",
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
          "sg-header relative flex shrink-0 items-center gap-2 border-b border-border px-3",
          isView ? "bg-muted" : "bg-card",
          mark?.status === "pending" && "bg-primary/6",
          mark?.status === "failed" && "bg-destructive/8",
          "data-[link-target=bad]:bg-destructive/10 data-[link-target=ok]:bg-success/15",
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
        {renaming && edit ? (
          <InlineName
            aria-label={`Rename ${table.name}`}
            initial={table.name}
            validate={(name) =>
              nameProblem(name) ?? relationNameTaken(edit.schema, table.schema, name)
            }
            onCommit={(to) => edit.emit({ kind: "renameTable", table: targetOf(table.entity), to })}
            onCancel={edit.stop}
            className="font-mono text-sm font-semibold sm:text-sm"
          />
        ) : (
          // A pointer shortcut: F2 and the menu are the keyboard's.
          <span
            className={cn(
              "sg-name min-w-0 flex-1 truncate font-mono text-sm font-semibold text-foreground",
              table.status === "removed" && "line-through decoration-destructive/60",
            )}
            onDoubleClick={
              canEdit
                ? (event) => {
                    event.stopPropagation();
                    edit.start({ kind: "renameTable", table: id });
                  }
                : undefined
            }
          >
            {multiSchema && (
              <span className="font-normal text-faint-foreground">{table.schema}.</span>
            )}
            {table.name}
          </span>
        )}
        {kindLabel && (
          <span className="sg-kind shrink-0 rounded-sm px-1.5 text-xs text-muted-foreground ring-1 ring-border-strong ring-inset">
            {kindLabel === "materialized view"
              ? "materialized"
              : kindLabel === "foreign table"
                ? "foreign"
                : kindLabel}
          </span>
        )}
        {table.status && <StatusBadge status={table.status} />}
        {mark && edit && <MarkIcon mark={mark} edit={edit} table={id} />}
        {tools && edit && !renaming && <HeaderTools edit={edit} table={table} />}
      </div>

      <OverviewBody table={table} width={width ?? NODE.minWidth} />
      <DetailSlot
        id={id}
        table={table}
        edges={edges}
        diffMode={diffMode}
        related={related}
        canEdit={canEdit}
      />
    </div>
  );
}

/**
 * The rows, mounted the first time the graph is at a readable zoom (a few
 * nodes per frame, see detail-scheduler), then kept: later zooms in and out
 * only flip CSS (`data-lod` on the graph), so crossing the threshold
 * re-renders this small component per node, never the node itself.
 */
function DetailSlot(props: DetailProps) {
  const [mounted, setMounted] = useState(false);
  const wanted = useGraphUi((s) => mounted || s.lod === "detail");
  useEffect(() => {
    if (!wanted || mounted) return;
    return requestDetail(() => setMounted(true));
  }, [wanted, mounted]);
  return mounted ? <DetailBody {...props} /> : null;
}

interface DetailProps {
  id: string;
  table: TableModel;
  edges: EdgeModel[];
  diffMode: boolean;
  related: string | null;
  canEdit: boolean;
}

/** Comment, rows and footer: what a readable zoom shows. */
const DetailBody = memo(function DetailBody({
  id,
  table,
  edges,
  diffMode,
  related,
  canEdit,
}: DetailProps) {
  const store = useGraphUiStore();
  const edit = useEdit();
  const nodeState = useGraphUi((s) => s.nodeState[id]);
  // Editing: the column renamed in place here, and the marks of edits in flight.
  const renaming = useGraphUi((s) =>
    canEdit && s.editing?.kind === "renameColumn" && s.editing.table === id
      ? s.editing.column
      : null,
  );
  const marks = useGraphUi((s) => (canEdit ? s.marks : null));
  const visible = visibleColumnCount(table, nodeState);

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

  const toggle = (part: "allColumns" | "partitions") =>
    store.set({
      nodeState: {
        ...store.get().nodeState,
        [id]: { ...nodeState, [part]: !nodeState?.[part] },
      },
    });

  return (
    <div className="sg-detail flex min-h-0 flex-col">
      {table.comment && (
        <p
          className="shrink-0 truncate border-b border-border bg-muted/60 px-3 text-xs leading-5 text-muted-foreground"
          style={{ height: NODE.comment }}
          title={table.comment}
          onDoubleClick={
            canEdit
              ? (event) => {
                  event.stopPropagation();
                  edit?.start({ kind: "comment", table: id });
                }
              : undefined
          }
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
            editing={
              canEdit
                ? {
                    table: id,
                    renaming: renaming === c.name,
                    mark: marks?.get(markKey(id, c.name)),
                    linkable: !isViewTable(table),
                  }
                : undefined
            }
          />
        ))}
      </ul>
      {hasFooter(table) && (
        <Footer
          table={table}
          hidden={table.columns.length - visible}
          partitionsOpen={nodeState?.partitions === true}
          onToggleColumns={() => toggle("allColumns")}
          onTogglePartitions={() => toggle("partitions")}
        />
      )}
    </div>
  );
});

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
    <div className="sg-overview flex min-h-0 flex-1 flex-col items-center justify-center gap-[0.25em] overflow-hidden px-3 text-center">
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

interface RowEditing {
  table: string;
  /** Its name is being edited in place. */
  renaming: boolean;
  /** An edit in flight here, or one that failed. */
  mark?: ElementMark;
  /** It may reference another column (a table's, not a view's). */
  linkable: boolean;
}

const isViewTable = (table: TableModel) => table.kind === "view" || table.kind === "materialized";

const ColumnRow = memo(function ColumnRow({
  column: c,
  joined,
  diffMode,
  nullable,
  editing,
}: {
  column: ColumnModel;
  joined: boolean;
  diffMode: boolean;
  /** Mark nullable columns (not in views, where every column is). */
  nullable: boolean;
  /** Set when the schema can be edited from the graph. */
  editing?: RowEditing;
}) {
  const edit = useEdit();
  const mark = editing?.mark;
  const start = (kind: "renameColumn" | "type") => (event: React.MouseEvent) => {
    if (!editing || !edit) return;
    event.stopPropagation();
    edit.start({ kind, table: editing.table, column: c.name });
  };
  return (
    <li
      data-column={c.name}
      data-pending={mark?.status === "pending" ? "" : undefined}
      className={cn(
        "group/row relative flex items-center gap-1.5 pr-3 font-mono text-xs",
        diffMode ? "pl-4" : "pl-3",
        "before:absolute before:inset-y-0 before:left-0 before:w-0.5",
        c.status && ROW_STATUS[c.status],
        joined && !c.status && "bg-primary/8",
        mark?.status === "pending" && "bg-primary/6",
        mark?.status === "failed" && "bg-destructive/8 before:bg-destructive",
        "data-[link-target=ok]:bg-success/15 data-[link-target=ok]:before:bg-success",
        "data-[link-target=bad]:bg-destructive/10 data-[link-target=bad]:before:bg-destructive",
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
      {editing?.renaming && edit ? (
        <InlineName
          aria-label={`Rename ${c.name}`}
          initial={c.name}
          validate={(name) => {
            const entity = edit.graph.byKey.get(editing.table)?.entity;
            return nameProblem(name) ?? (entity ? columnNameTaken(entity, name) : null);
          }}
          onCommit={(to) => {
            const entity = edit.graph.byKey.get(editing.table)?.entity;
            if (entity)
              edit.emit({ kind: "renameColumn", table: targetOf(entity), column: c.name, to });
          }}
          onCancel={edit.stop}
          className="h-5 text-xs sm:h-5 sm:text-xs"
        />
      ) : (
        <span
          className={cn(
            "min-w-0 truncate",
            joined ? "font-semibold text-foreground" : "text-foreground",
            c.status === "removed" &&
              "text-muted-foreground line-through decoration-destructive/60",
          )}
          onDoubleClick={editing ? start("renameColumn") : undefined}
        >
          {c.name}
        </span>
      )}
      {(c.unique || c.identity || c.generated) && (
        <span className="flex shrink-0 items-center gap-1 text-faint-foreground [&_svg]:size-3">
          {c.unique && <FingerprintIcon aria-label="Unique" />}
          {c.identity && <HashtagIcon aria-label="Identity" />}
          {c.generated && <SparksIcon aria-label="Generated" />}
        </span>
      )}
      {mark && edit && editing && (
        <MarkIcon mark={mark} edit={edit} table={editing.table} column={c.name} />
      )}
      <span
        className={cn(
          "ml-auto min-w-0 shrink truncate pl-3 text-right",
          c.userType
            ? "text-syntax-type underline decoration-dotted underline-offset-2"
            : "text-faint-foreground",
        )}
        onDoubleClick={editing && !editing.renaming ? start("type") : undefined}
      >
        {c.display}
        {nullable && !c.notNull && <span className="text-faint-foreground">?</span>}
      </span>
      {editing?.linkable && !editing.renaming && (
        <span
          data-link-grip=""
          aria-hidden
          title="Drag to a column to add a foreign key"
          className={cn(
            "nodrag nopan absolute top-1/2 right-0.5 size-2.5 -translate-y-1/2 cursor-crosshair rounded-full border-2 border-info bg-card",
            "opacity-0 transition-opacity duration-(--duration-fast) group-hover/row:opacity-100",
            "pointer-coarse:hidden",
          )}
        />
      )}
    </li>
  );
});

/** A pending edit's quiet dot, or a failed one's warning (which opens its message). */
function MarkIcon({
  mark,
  edit,
  table,
  column,
}: {
  mark: ElementMark;
  edit: EditContextValue;
  table: string;
  column?: string;
}) {
  if (mark.status === "pending") {
    return (
      <span className="flex shrink-0 items-center" data-testid="edit-pending">
        <span aria-hidden className="size-1.5 rounded-full bg-primary motion-safe:animate-pulse" />
        <span className="sr-only">{mark.label}: being applied</span>
      </span>
    );
  }
  return (
    <Button
      variant="ghost"
      size="icon-sm"
      {...outOfTabOrder}
      aria-label={`${mark.label} failed: ${mark.message ?? ""}`}
      data-testid="edit-failed"
      onClick={(event) => {
        event.stopPropagation();
        edit.start({ kind: "failure", table, column });
      }}
      className="nodrag -my-1 size-5 shrink-0 text-destructive hover:bg-destructive/10 hover:text-destructive [&_svg]:size-3.5"
    >
      <WarningTriangleIcon />
    </Button>
  );
}

/** Add a column, and the table's actions: on the header of the table under the pointer. */
function HeaderTools({ edit, table }: { edit: EditContextValue; table: TableModel }) {
  const view = isViewTable(table);
  return (
    <div className="nodrag absolute inset-y-0 right-0 flex items-center gap-0.5 bg-linear-to-l from-card from-70% pr-1.5 pl-6">
      {!view && (
        <Button
          variant="ghost"
          size="icon-sm"
          {...outOfTabOrder}
          aria-label={`Add a column to ${table.name}`}
          title="Add column (A)"
          onClick={(event) => {
            event.stopPropagation();
            edit.start({ kind: "addColumn", table: table.key });
          }}
          className="size-6"
        >
          <PlusIcon />
        </Button>
      )}
      <Button
        variant="ghost"
        size="icon-sm"
        {...outOfTabOrder}
        aria-label={`Actions for ${table.name}`}
        title="Actions (M)"
        onClick={(event) => {
          event.stopPropagation();
          const box = event.currentTarget.getBoundingClientRect();
          edit.openMenu({ table: table.key, x: box.left, y: box.bottom });
        }}
        className="size-6"
      >
        <MoreHorizIcon />
      </Button>
    </div>
  );
}

/**
 * The graph is one tab stop (arrow keys between tables, E unfolds the
 * selected one): buttons inside a node stay out of the tab order and do not
 * take focus when clicked, so the keys keep working.
 */
const outOfTabOrder = {
  tabIndex: -1,
  onMouseDown: (event: React.MouseEvent) => event.preventDefault(),
} as const;

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
          {...outOfTabOrder}
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
            {...outOfTabOrder}
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
                  {p.partitionKey && (
                    <span
                      className="shrink-0 truncate text-faint-foreground"
                      title={`Partitioned in turn by ${p.partitionKey}`}
                    >
                      by {p.partitionKey}
                    </span>
                  )}
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
