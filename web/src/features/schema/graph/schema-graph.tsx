import "@xyflow/react/dist/base.css";
import "./schema-graph.css";

import {
  Background,
  BackgroundVariant,
  type Edge,
  MiniMap,
  type Node,
  type NodeChange,
  Panel,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useStore,
} from "@xyflow/react";
import {
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";

import { TableIcon } from "@/components/icons";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import type { ChangeStatus, SchemaDiffInput } from "../diff";
import { type Groups, groupColor, type GroupColor, type Layout } from "../layout";
import type { Schema } from "../model";
import type { EdgeModel, GraphModel } from "./build";
import { DiffLegend, GraphKey, SearchButton, toolbarClass, ZoomControls } from "./graph-controls";
import { graphOf } from "./graph-of";
import { createGraphUiStore, GraphUiContext, type GraphUiStore, useGraphUi } from "./graph-ui";
import { AreaLabel, type AreaLabelFlowNode, GroupNode, type GroupFlowNode } from "./group-node";
import { columnAnchorY, GROUP_PADDING, NODE, tableSize } from "./geometry";
import { HoverDetails } from "./hover-details";
import { KIND_LABEL } from "./kind";
import { type Box, type Direction, nearestInDirection, nearestTo } from "./navigation";
import { RelationEdge, type RelationFlowEdge } from "./relation-edge";
import { type Rect, route } from "./routing";
import { TableNode, type TableFlowNode, type TableNodeData } from "./table-node";
import { TableSearch } from "./table-search";
import { useAutoLayout } from "./use-auto-layout";

export interface SchemaGraphProps {
  /** The model drawn; in diff mode, the new side. */
  schema: Schema;
  /** Diff mode: draw the union with `diff.base`, styled by `diff.status`. */
  diff?: SchemaDiffInput;
  /** Known positions by qualified name. Missing tables are placed automatically. */
  layout?: Layout;
  groups?: Groups;
  /**
   * Positions this graph decided: tables it placed because `layout` lacked
   * them, and (when `editable`) tables moved by hand. A draft writes them into
   * its layout map; without this callback they last as long as the view.
   */
  onLayoutChange?: (positions: Layout) => void;
  /** Tables can be dragged. Read-only by default. */
  editable?: boolean;
  /** The selected table's qualified name. Leave undefined to let the graph own it. */
  selected?: string | null;
  onSelectedChange?: (key: string | null) => void;
  /** ⌘K and / open the table search. Turn off when two graphs share a page. */
  shortcuts?: boolean;
  /** Shown instead of the graph when the schema has no tables. */
  empty?: ReactNode;
  /** More buttons in the top-right toolbar (show the text pane, full screen…). */
  controls?: ReactNode;
  /** Rendered inside the flow, e.g. remote cursors in a `ViewportPortal`. */
  children?: ReactNode;
  className?: string;
}

const nodeTypes = { table: TableNode, tableGroup: GroupNode, areaLabel: AreaLabel };
const edgeTypes = { relation: RelationEdge };
const FIT = { padding: 0.12, maxZoom: 1 } as const;
// Schemas get colors in this order, so the first two are the most distinct.
const SCHEMA_COLORS: GroupColor[] = [
  "blue",
  "amber",
  "green",
  "violet",
  "pink",
  "teal",
  "red",
  "stone",
];

const MINIMAP_FILL: Record<string, string> = {
  added: "fill-success",
  removed: "fill-destructive",
  changed: "fill-warning",
  stone: "fill-group-stone/60",
  red: "fill-group-red/60",
  amber: "fill-group-amber/60",
  green: "fill-group-green/60",
  teal: "fill-group-teal/60",
  blue: "fill-group-blue/60",
  violet: "fill-group-violet/60",
  pink: "fill-group-pink/60",
};

/**
 * The schema as a graph: one node per table or view, foreign keys as
 * crow's-foot edges between the columns they join, groups behind their
 * tables. Read-only unless `editable`; every edit leaves through callbacks.
 */
export function SchemaGraph(props: SchemaGraphProps) {
  return (
    <ReactFlowProvider>
      <GraphCanvas {...props} />
    </ReactFlowProvider>
  );
}

function isEditableTarget(target: EventTarget | null) {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName))
  );
}

function GraphCanvas({
  schema,
  diff,
  layout,
  groups,
  onLayoutChange,
  editable = false,
  selected,
  onSelectedChange,
  shortcuts = true,
  empty,
  controls,
  children,
  className,
}: SchemaGraphProps) {
  const graph = useMemo(() => graphOf(schema, diff), [schema, diff]);
  const [store] = useState<GraphUiStore>(() => createGraphUiStore({ selected: selected ?? null }));
  const nodeState = useSyncExternalStore(store.subscribe, () => store.get().nodeState);
  const wrapper = useRef<HTMLDivElement>(null);
  // The first layout takes the pane's shape: measured before the layout starts.
  const aspect = useRef(1.6);
  useLayoutEffect(() => {
    const el = wrapper.current;
    if (el && el.clientHeight > 0) aspect.current = el.clientWidth / el.clientHeight;
  }, []);
  const { positions, pending } = useAutoLayout(graph, layout, groups, onLayoutChange, aspect);
  // The flow mounts once every table has a place, so its first fit shows them all.
  const [ready, setReady] = useState(false);
  if (!ready && pending === 0) setReady(true);
  const [moved, setMoved] = useState<Layout>({});
  const flow = useReactFlow();
  const [searchOpen, setSearchOpen] = useState(false);
  const hintId = useId();

  // ---- Selection: controlled through `selected`, or owned here. -----------
  const localPick = useRef<string | null | undefined>(undefined);
  const select = useCallback(
    (key: string | null) => {
      localPick.current = key;
      if (selected === undefined) store.set({ selected: key });
      onSelectedChange?.(key);
    },
    [selected, store, onSelectedChange],
  );

  const boxOf = useCallback(
    (key: string): Box | undefined => {
      const table = graph.byKey.get(key);
      const p = moved[key] ?? positions[key];
      if (!table || !p) return undefined;
      return { id: key, ...p, ...tableSize(table, graph.multiSchema, nodeState[key]) };
    },
    [graph, moved, positions, nodeState],
  );

  /** Pans (never zooms) just enough to bring a table into view. */
  const reveal = useCallback(
    (key: string) => {
      const box = boxOf(key);
      const el = wrapper.current;
      if (!box || !el) return;
      const { x, y, zoom } = flow.getViewport();
      const left = box.x * zoom + x;
      const top = box.y * zoom + y;
      const margin = 48;
      const inside =
        left >= margin &&
        top >= margin &&
        left + box.width * zoom <= el.clientWidth - margin &&
        top + Math.min(box.height, 320) * zoom <= el.clientHeight - margin;
      if (!inside) {
        void flow.setCenter(box.x + box.width / 2, box.y + Math.min(box.height, 320) / 2, {
          zoom,
          duration: 300,
        });
      }
    },
    [boxOf, flow],
  );

  /** Centers a table at a zoom that shows it whole, never past 100%. */
  const zoomToTable = useCallback(
    (key: string) => {
      const box = boxOf(key);
      const el = wrapper.current;
      if (!box || !el) return;
      const fit = Math.min(
        el.clientWidth / (box.width + 320),
        el.clientHeight / (box.height + 200),
      );
      const zoom = Math.max(0.45, Math.min(1, fit));
      void flow.setCenter(box.x + box.width / 2, box.y + box.height / 2, { zoom, duration: 400 });
    },
    [boxOf, flow],
  );

  useEffect(() => {
    if (selected === undefined) return;
    store.set({ selected });
    // A selection made elsewhere (the text pane) brings the table into view.
    if (selected && localPick.current !== selected) reveal(selected);
    localPick.current = undefined;
  }, [selected, store, reveal]);

  // ---- Nodes and edges ------------------------------------------------------
  const schemaColors = useMemo(() => {
    if (!graph.multiSchema) return new Map<string, GroupColor>();
    const names = [...new Set(graph.tables.map((t) => t.schema))].sort();
    return new Map(names.map((s, i) => [s, SCHEMA_COLORS[i % SCHEMA_COLORS.length]!]));
  }, [graph]);

  const nodeData = useMemo(() => {
    const byTable = new Map<string, EdgeModel[]>();
    for (const e of graph.edges) {
      byTable.set(e.source, [...(byTable.get(e.source) ?? []), e]);
      if (e.target !== e.source) byTable.set(e.target, [...(byTable.get(e.target) ?? []), e]);
    }
    return new Map<string, TableNodeData>(
      graph.tables.map((table) => [
        table.key,
        {
          table,
          multiSchema: graph.multiSchema,
          schemaColor: schemaColors.get(table.schema),
          edges: byTable.get(table.key) ?? [],
          diffMode: diff !== undefined,
        },
      ]),
    );
  }, [graph, schemaColors, diff]);

  const nodes = useMemo(() => {
    const out: (TableFlowNode | GroupFlowNode | AreaLabelFlowNode)[] = [];
    const sizes = new Map<string, { width: number; height: number }>();
    const tables: TableFlowNode[] = [];
    for (const table of graph.tables) {
      const position = moved[table.key] ?? positions[table.key];
      if (!position) continue;
      const size = tableSize(table, graph.multiSchema, nodeState[table.key]);
      sizes.set(table.key, size);
      // `data` objects are stable per table, so the memoized nodes skip
      // re-rendering when only the array around them changed.
      tables.push({
        id: table.key,
        type: "table",
        position,
        width: size.width,
        height: size.height,
        data: nodeData.get(table.key)!,
        draggable: editable,
        selectable: false,
        ariaLabel: `${KIND_LABEL[table.kind] ?? "table"} ${table.key}, ${table.columns.length} columns`,
        // Unfolded partitions overlap what is below: draw them on top.
        zIndex: nodeState[table.key]?.partitions || nodeState[table.key]?.allColumns ? 2 : 1,
      });
    }
    // Groups behind their tables, sized to the members placed so far. Without
    // groups, a graph over several schemas shows each schema's area instead,
    // as long as no two areas overlap (hand-made layouts can mix them).
    const userGroups = Object.entries(groups ?? {});
    const labels: AreaLabelFlowNode[] = [];
    const areas: {
      id: string;
      name: string;
      color: GroupColor;
      members: string[];
      schema: boolean;
    }[] =
      userGroups.length > 0
        ? userGroups.map(([id, g]) => ({
            id,
            name: g.name,
            color: groupColor(g.color),
            members: g.members,
            schema: false,
          }))
        : graph.multiSchema
          ? [...schemaColors].map(([schema, color]) => ({
              id: `schema:${schema}`,
              name: schema,
              color,
              members: graph.tables.filter((t) => t.schema === schema).map((t) => t.key),
              schema: true,
            }))
          : [];
    const boxes: (GroupFlowNode & { width: number; height: number })[] = [];
    for (const area of areas.sort((a, b) => a.id.localeCompare(b.id))) {
      let x0 = Infinity;
      let y0 = Infinity;
      let x1 = -Infinity;
      let y1 = -Infinity;
      for (const member of area.members) {
        const p = moved[member] ?? positions[member];
        const size = sizes.get(member);
        if (!p || !size) continue;
        x0 = Math.min(x0, p.x);
        y0 = Math.min(y0, p.y);
        x1 = Math.max(x1, p.x + size.width);
        y1 = Math.max(y1, p.y + size.height);
      }
      if (x0 === Infinity) continue;
      boxes.push({
        id: `group:${area.id}`,
        type: "tableGroup",
        position: { x: x0 - GROUP_PADDING.side, y: y0 - GROUP_PADDING.top },
        width: x1 - x0 + GROUP_PADDING.side * 2,
        height: y1 - y0 + GROUP_PADDING.top + GROUP_PADDING.bottom,
        data: { name: area.name, color: area.color, schema: area.schema },
        draggable: false,
        selectable: false,
        focusable: false,
        zIndex: 0,
      });
    }
    const overlap = boxes.some((a, i) =>
      boxes.some(
        (b, j) =>
          j > i &&
          a.position.x < b.position.x + b.width &&
          b.position.x < a.position.x + a.width &&
          a.position.y < b.position.y + b.height &&
          b.position.y < a.position.y + a.height,
      ),
    );
    if (!(overlap && userGroups.length === 0)) {
      out.push(...boxes);
      for (const box of boxes) {
        labels.push({
          id: `label:${box.id}`,
          type: "areaLabel",
          position: { x: box.position.x + box.width / 2, y: box.position.y + box.height / 2 },
          width: 1,
          height: 1,
          data: box.data,
          draggable: false,
          selectable: false,
          focusable: false,
          zIndex: 3,
        });
      }
    }
    return [...out, ...tables, ...labels];
  }, [graph, positions, moved, nodeState, nodeData, groups, editable, schemaColors]);

  // Every placed table's box: the edges route around them.
  const rects = useMemo(() => {
    const out = new Map<string, Rect>();
    for (const table of graph.tables) {
      const p = moved[table.key] ?? positions[table.key];
      if (p)
        out.set(table.key, { ...p, ...tableSize(table, graph.multiSchema, nodeState[table.key]) });
    }
    return out;
  }, [graph, positions, moved, nodeState]);

  const edges = useMemo<RelationFlowEdge[]>(() => {
    const obstacles = [...rects.values()];
    const anchor = (key: string, column: string | undefined, kind: EdgeModel["kind"]) =>
      rects.get(key)!.y +
      (kind === "reads"
        ? NODE.header / 2
        : columnAnchorY(graph.byKey.get(key)!, column, nodeState[key]));
    return graph.edges
      .filter((e) => rects.has(e.source) && rects.has(e.target))
      .map((e) => ({
        id: e.id,
        source: e.source,
        target: e.target,
        type: "relation",
        data: {
          model: e,
          route: route(
            {
              id: e.id,
              source: rects.get(e.source)!,
              target: rects.get(e.target)!,
              sourceY: anchor(e.source, e.sourceColumn, e.kind),
              targetY: anchor(e.target, e.targetColumn, e.kind),
            },
            obstacles,
          ),
        },
        selectable: false,
        focusable: false,
        zIndex: 1,
      }));
  }, [graph, rects, nodeState]);

  // ---- Editing: moving tables ----------------------------------------------
  // Drag stop can fire before React re-renders: the latest moves live here.
  const movedRef = useRef<Layout>({});
  const onNodesChange = useCallback(
    (changes: NodeChange<TableFlowNode | GroupFlowNode | AreaLabelFlowNode>[]) => {
      const next: Layout = {};
      for (const c of changes) if (c.type === "position" && c.position) next[c.id] = c.position;
      if (Object.keys(next).length) {
        movedRef.current = { ...movedRef.current, ...next };
        setMoved(movedRef.current);
      }
    },
    [],
  );
  const onNodeDragStop = useCallback(
    (_: unknown, _node: Node, dragged: Node[]) => {
      const out: Layout = {};
      for (const n of dragged) out[n.id] = movedRef.current[n.id] ?? n.position;
      onLayoutChange?.(out);
    },
    [onLayoutChange],
  );

  // ---- Keyboard -------------------------------------------------------------
  const boxes = useCallback(
    () => graph.tables.map((t) => boxOf(t.key)).filter((b): b is Box => b !== undefined),
    [graph, boxOf],
  );

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
    if (
      isEditableTarget(event.target) ||
      (event.target as HTMLElement).closest("button, a, [role=dialog]")
    )
      return;
    const current = store.get().selected;
    const arrows: Record<string, Direction> = {
      ArrowLeft: "left",
      ArrowRight: "right",
      ArrowUp: "up",
      ArrowDown: "down",
    };
    const direction = arrows[event.key];
    if (direction) {
      const all = boxes();
      const from = current ? all.find((b) => b.id === current) : undefined;
      let next: Box | undefined;
      if (from) next = nearestInDirection(from, all, direction);
      else {
        const { x, y, zoom } = flow.getViewport();
        const el = wrapper.current!;
        next = nearestTo(
          { x: (el.clientWidth / 2 - x) / zoom, y: (el.clientHeight / 2 - y) / zoom },
          all,
        );
      }
      if (next) {
        select(next.id);
        reveal(next.id);
      }
    } else if (event.key === "Enter" && current) {
      zoomToTable(current);
    } else if (event.key === "Escape" && current) {
      select(null);
    } else if (event.key === "+" || event.key === "=") {
      void flow.zoomIn({ duration: 200 });
    } else if (event.key === "-" || event.key === "_") {
      void flow.zoomOut({ duration: 200 });
    } else if (event.key === "0") {
      void flow.fitView({ ...FIT, duration: 300 });
    } else {
      return;
    }
    event.preventDefault();
  };

  useEffect(() => {
    if (!shortcuts) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || !wrapper.current?.offsetParent) return;
      const command = (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k";
      const slash =
        event.key === "/" && !event.metaKey && !event.ctrlKey && !isEditableTarget(event.target);
      if (!command && !slash) return;
      if (document.querySelector("[role=dialog]")) return;
      event.preventDefault();
      setSearchOpen(true);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [shortcuts]);

  const nextWithStatus = (status: ChangeStatus) => {
    const list = graph.tables.filter((t) => t.status === status);
    if (!list.length) return;
    const at = list.findIndex((t) => t.key === store.get().selected);
    const next = list[(at + 1) % list.length]!;
    select(next.key);
    zoomToTable(next.key);
  };

  // ---- Render ---------------------------------------------------------------
  if (graph.tables.length === 0) {
    return (
      <div
        className={cn(
          "schema-graph flex h-full items-center justify-center bg-background",
          className,
        )}
      >
        {empty ?? (
          <EmptyState
            icon={<TableIcon />}
            title="No tables yet"
            description="Tables, views and the foreign keys between them appear here once the schema defines some."
          />
        )}
      </div>
    );
  }

  const arranging = !ready;

  return (
    <GraphUiContext.Provider value={store}>
      {/* A keyboard-driven canvas: one tab stop, arrow keys between tables.
          `application` makes screen readers pass those keys through; jsx-a11y
          does not count it as interactive. */}
      {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions */}
      <div
        ref={wrapper}
        role="application"
        aria-roledescription="schema graph"
        aria-label="Schema graph"
        aria-describedby={hintId}
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
        tabIndex={0}
        onKeyDown={onKeyDown}
        className={cn(
          "schema-graph relative h-full min-h-0 w-full bg-background outline-none",
          "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
          className,
        )}
      >
        <p id={hintId} className="sr-only">
          {graph.tables.length} tables. Arrow keys move between tables, Enter zooms to the selected
          one, slash finds a table.
        </p>
        <Announcer graph={graph} />
        {arranging ? (
          <div className="flex h-full items-center justify-center gap-2.5 text-base text-muted-foreground">
            <Spinner label={`Arranging ${graph.tables.length} tables`} />
            <span aria-hidden>
              Arranging {graph.tables.length} {graph.tables.length === 1 ? "table" : "tables"}…
            </span>
          </div>
        ) : (
          <ReactFlow<TableFlowNode | GroupFlowNode | AreaLabelFlowNode, Edge>
            nodes={nodes}
            edges={edges}
            nodeTypes={nodeTypes}
            edgeTypes={edgeTypes}
            onNodesChange={editable ? onNodesChange : undefined}
            onNodeDragStop={editable ? onNodeDragStop : undefined}
            onNodeClick={(_, node) => {
              if (node.type === "table") select(node.id);
            }}
            onNodeDoubleClick={(_, node) => {
              if (node.type === "table") zoomToTable(node.id);
            }}
            onNodeMouseEnter={(_, node) => {
              if (node.type === "table") store.set({ hovered: node.id });
            }}
            onNodeMouseLeave={() => store.set({ hovered: null })}
            onPaneClick={() => select(null)}
            fitView
            fitViewOptions={FIT}
            minZoom={0.05}
            maxZoom={2}
            nodesDraggable={editable}
            nodesConnectable={false}
            nodesFocusable={false}
            edgesFocusable={false}
            elementsSelectable={false}
            disableKeyboardA11y
            deleteKeyCode={null}
            selectionKeyCode={null}
            multiSelectionKeyCode={null}
            panOnScroll
            zoomOnPinch
            zoomActivationKeyCode={["Meta", "Control"]}
            // Every table stays mounted: panning is then a CSS transform with
            // no React work, which measured smoother at 300 tables than
            // mounting and unmounting tables at the viewport's edges.
            onlyRenderVisibleElements={false}
            attributionPosition="bottom-right"
          >
            <Background variant={BackgroundVariant.Dots} gap={22} size={1.25} />
            <ZoomVariable target={wrapper} />
            <Panel position="top-left" className="!m-3">
              <SearchButton onClick={() => setSearchOpen(true)} />
            </Panel>
            <Panel position="top-right" className="!m-3 flex items-center gap-2">
              {graph.counts && <DiffLegend counts={graph.counts} onNext={nextWithStatus} />}
              <div className={toolbarClass}>
                <GraphKey diff={graph.counts !== undefined} />
                {controls}
              </div>
            </Panel>
            <Panel position="bottom-left" className="!m-3">
              <ZoomControls />
            </Panel>
            <MiniMap
              pannable
              zoomable
              ariaLabel="Overview of the graph"
              // Above React Flow's attribution, which sits in the corner.
              className="!mx-3 !mt-3 !mb-7 hidden md:block"
              style={{ width: 176, height: 120 }}
              nodeBorderRadius={6}
              nodeClassName={(node) => {
                if (node.type !== "table") return "fill-transparent";
                const data = node.data as TableNodeData;
                const key = data.table.status ?? data.schemaColor;
                return key ? (MINIMAP_FILL[key] ?? "") : "";
              }}
            />
            {children}
          </ReactFlow>
        )}
        <HoverDetails container={wrapper} graph={graph} />
        <TableSearch
          open={searchOpen}
          onOpenChange={setSearchOpen}
          graph={graph}
          onPick={(key) => {
            select(key);
            zoomToTable(key);
            wrapper.current?.focus({ preventScroll: true });
          }}
        />
      </div>
    </GraphUiContext.Provider>
  );
}

/**
 * The zoom as a CSS variable on the graph (`--graph-zoom`): zoomed-out labels
 * scale against it to stay legible, with no React render per zoom step.
 */
function ZoomVariable({ target }: { target: React.RefObject<HTMLDivElement | null> }) {
  const zoom = useStore((s) => s.transform[2]);
  useEffect(() => {
    target.current?.style.setProperty("--graph-zoom", String(zoom));
  }, [target, zoom]);
  return null;
}

/** Says which table is selected, and what it touches, to screen readers. */
function Announcer({ graph }: { graph: GraphModel }) {
  const selected = useGraphUi((s) => s.selected);
  let text = "";
  const table = selected ? graph.byKey.get(selected) : undefined;
  if (table) {
    const refs = graph.edges
      .filter((e) => e.kind === "fk" && e.source === table.key)
      .map((e) => e.target);
    const by = graph.edges
      .filter((e) => e.kind === "fk" && e.target === table.key)
      .map((e) => e.source);
    text = [
      `${table.key}, ${KIND_LABEL[table.kind] ?? "table"}, ${table.columns.length} columns`,
      table.status && `${table.status} in this diff`,
      refs.length > 0 && `references ${[...new Set(refs)].join(", ")}`,
      by.length > 0 && `referenced by ${[...new Set(by)].join(", ")}`,
    ]
      .filter(Boolean)
      .join("; ");
  }
  return (
    <p aria-live="polite" className="sr-only">
      {text}
    </p>
  );
}
