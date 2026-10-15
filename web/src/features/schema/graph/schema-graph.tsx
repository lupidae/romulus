import "@xyflow/react/dist/base.css";
import "./schema-graph.css";

import {
  Background,
  BackgroundVariant,
  type Edge,
  type Node,
  type NodeChange,
  Panel,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useStoreApi,
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

import { PlusIcon, TableIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

import type { ChangeStatus, SchemaDiffInput } from "../diff";
import { type Groups, groupColor, type GroupColor, type Layout } from "../layout";
import type { Schema } from "../model";
import type { EdgeModel, GraphModel } from "./build";
import type { GraphEdit } from "../edits/edits";
import { ActionsMenu } from "./editing/actions-menu";
import {
  EditContext,
  type EditContextValue,
  editableKind,
  type MenuTarget,
} from "./editing/context";
import { EditPopovers } from "./editing/edit-popovers";
import { LinkDrag } from "./editing/link-drag";
import { NewTable } from "./editing/new-table";
import type { Editing, ElementMark, GraphEditor } from "./editing/types";
import { DiffLegend, GraphKey, SearchButton, toolbarClass, ZoomControls } from "./graph-controls";
import { graphOf } from "./graph-of";
import { createGraphUiStore, GraphUiContext, type GraphUiStore, useGraphUi } from "./graph-ui";
import { AreaLabel, type AreaLabelFlowNode, GroupNode, type GroupFlowNode } from "./group-node";
import {
  columnAnchorY,
  FIT_VIEW,
  freeSpot,
  GROUP_PADDING,
  NODE,
  type NodeState,
  nextLod,
  tableSize,
} from "./geometry";
import { type DetailsTarget, HoverDetails } from "./hover-details";
import { NavigatorMap } from "./navigator-map";
import { KIND_LABEL } from "./kind";
import { type Box, type Direction, nearestInDirection, nearestTo } from "./navigation";
import { RelationEdge, type RelationFlowEdge } from "./relation-edge";
import { type Rect, type Route, route } from "./routing";
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
  /**
   * Zoomed to whenever a new object is passed (a change picked in a list):
   * the table centered, at a zoom that shows it whole.
   */
  focus?: { key: string } | null;
  /**
   * Tables picked for an action on several (a draft's groups): shift- or
   * ⌘-click adds or removes one, Esc clears. Without `onPickedChange`, those
   * clicks select like any other.
   */
  picked?: readonly string[];
  onPickedChange?: (keys: string[]) => void;
  /** ⌘K and / open the table search. Turn off when two graphs share a page. */
  shortcuts?: boolean;
  /** Diff mode's legend (counts, next added table…); off when a list beside the graph has them. */
  legend?: boolean;
  /** Shown instead of the graph when the schema has no tables. */
  empty?: ReactNode;
  /** More buttons in the top-right toolbar (show the text pane, full screen…). */
  controls?: ReactNode;
  /** Rendered inside the flow, e.g. remote cursors in a `ViewportPortal`. */
  children?: ReactNode;
  /**
   * Schema editing (a draft's editors): actions on tables and columns (menu,
   * double-click, keys, drawing foreign keys), handed out as edits.
   */
  editor?: GraphEditor;
  className?: string;
}

const nodeTypes = { table: TableNode, tableGroup: GroupNode, areaLabel: AreaLabel };
const edgeTypes = { relation: RelationEdge };
const FIT = FIT_VIEW;
/** The first layout's shape when its positions are persisted (see aspect below). */
const PERSISTED_ASPECT = 1.4;
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
  focus,
  picked,
  onPickedChange,
  shortcuts = true,
  legend = true,
  empty,
  controls,
  children,
  editor,
  className,
}: SchemaGraphProps) {
  const graph = useMemo(() => graphOf(schema, diff), [schema, diff]);
  const canEdit = editor !== undefined;
  const [store] = useState<GraphUiStore>(() => createGraphUiStore({ selected: selected ?? null }));
  const nodeState = useSyncExternalStore(store.subscribe, () => store.get().nodeState);
  const wrapper = useRef<HTMLDivElement>(null);
  // The first layout takes the pane's shape, measured before it starts, unless
  // its positions are written back (a draft): every collaborator placing the
  // same tables must then compute the same positions, whatever their screen.
  const aspect = useRef(PERSISTED_ASPECT);
  useLayoutEffect(() => {
    const el = wrapper.current;
    if (!onLayoutChange && el && el.clientHeight > 0)
      aspect.current = el.clientWidth / el.clientHeight;
  }, [onLayoutChange]);
  const { positions, pending } = useAutoLayout(graph, layout, groups, onLayoutChange, aspect);
  // The flow mounts once every table has a place, so its first fit shows them all.
  const [ready, setReady] = useState(false);
  if (!ready && pending === 0) setReady(true);
  // Hand moves: `dragging` while the pointer is down; `dropped` after, until
  // `layout` changes for that table (the move written back, a peer's move, an
  // undo): the layout map then wins again.
  const [dragging, setDragging] = useState<Layout>({});
  const [dropped, setDropped] = useState<Record<string, Dropped>>({});
  const [layoutSeen, setLayoutSeen] = useState(layout);
  if (layoutSeen !== layout) {
    setLayoutSeen(layout);
    const kept = Object.entries(dropped).filter(([key, d]) => samePosition(layout?.[key], d.base));
    if (kept.length !== Object.keys(dropped).length) setDropped(Object.fromEntries(kept));
  }
  const moved = useMemo<Layout>(() => {
    const out: Layout = {};
    for (const [key, d] of Object.entries(dropped)) out[key] = d.position;
    return { ...out, ...dragging };
  }, [dropped, dragging]);
  const flow = useReactFlow();
  const flowStore = useStoreApi();
  const [pinned, setPinned] = useState<DetailsTarget | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const hintId = useId();

  // ---- Selection: controlled through `selected`, or owned here. -----------
  const localPick = useRef<string | null | undefined>(undefined);
  const select = useCallback(
    (key: string | null) => {
      localPick.current = key;
      setPinned(null);
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

  useEffect(() => {
    if (ready) performance.mark("schema graph: drawn");
  }, [ready]);

  useEffect(() => {
    store.set({ picked: picked ?? [] });
  }, [picked, store]);

  // Zoom-driven styling, written to the element (see ZoomLevel).
  const applyZoom = useCallback(
    function apply(zoom: number) {
      const el = wrapper.current;
      // The flow can mount in the same commit as the graph's element (the
      // first table of an empty draft), before its ref is set: next frame.
      if (!el) {
        requestAnimationFrame(() => {
          if (wrapper.current) apply(zoom);
        });
        return;
      }
      el.style.setProperty("--graph-zoom", String(zoom));
      const lod = nextLod(store.get().lod, zoom);
      if (el.dataset.lod !== lod) el.dataset.lod = lod;
      store.set({ lod });
    },
    [store],
  );

  // The view keeps fitting the pane (a pane shown after mounting hidden, a
  // resized split, a rotated phone) until the reader moves it themselves.
  const autoFit = useRef(true);
  useEffect(() => {
    const el = wrapper.current;
    if (!el || !ready) return;
    let size = `${el.clientWidth}x${el.clientHeight}`;
    const observer = new ResizeObserver(() => {
      const next = `${el.clientWidth}x${el.clientHeight}`;
      if (next === size) return;
      size = next;
      // Next frame: fitting inside the observer's callback would resize
      // observed content again ("ResizeObserver loop" warnings).
      if (autoFit.current && el.clientWidth > 0 && el.clientHeight > 0)
        requestAnimationFrame(() => void flow.fitView({ ...FIT }));
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [ready, flow]);

  /**
   * How long a move to (cx, cy) at `zoom` animates: a short glide to what is
   * near, a jump to what is far. Flying across a few hundred tables draws
   * them all on the way (and zooms out mid-flight), frames of 100 ms and more.
   */
  const travel = useCallback(
    (cx: number, cy: number, zoom: number, ms = 300) => {
      const { width, height, transform } = flowStore.getState();
      const [x, y, k] = transform;
      const here = { x: (width / 2 - x) / k, y: (height / 2 - y) / k };
      const far =
        Math.hypot(cx - here.x, cy - here.y) > Math.max(width, height) / Math.min(k, zoom);
      return far || Math.max(k, zoom) / Math.min(k, zoom) > 2.5 ? 0 : ms;
    },
    [flowStore],
  );

  /** Pans (never zooms) just enough to bring a table into view. */
  const reveal = useCallback(
    (key: string) => {
      const box = boxOf(key);
      // The pane's size as React Flow measured it: reading the element's
      // would force a layout of the whole page on every pick. A hidden graph
      // has none to center in (React Flow would go NaN).
      const { width, height } = flowStore.getState();
      if (!box || !width || !height) return;
      autoFit.current = false;
      const { x, y, zoom } = flow.getViewport();
      const left = box.x * zoom + x;
      const top = box.y * zoom + y;
      const margin = 48;
      const inside =
        left >= margin &&
        top >= margin &&
        left + box.width * zoom <= width - margin &&
        top + Math.min(box.height, 320) * zoom <= height - margin;
      if (!inside) {
        const cx = box.x + box.width / 2;
        const cy = box.y + Math.min(box.height, 320) / 2;
        void flow.setCenter(cx, cy, { zoom, duration: travel(cx, cy, zoom) });
      }
    },
    [boxOf, flow, flowStore, travel],
  );

  /** Centers a table at a zoom that shows it whole, never past 100%. */
  const zoomToTable = useCallback(
    (key: string) => {
      const box = boxOf(key);
      const { width, height } = flowStore.getState();
      if (!box || !width || !height) return;
      autoFit.current = false;
      const fit = Math.min(width / (box.width + 320), height / (box.height + 200));
      const zoom = Math.max(0.45, Math.min(1, fit));
      const cx = box.x + box.width / 2;
      const cy = box.y + box.height / 2;
      void flow.setCenter(cx, cy, { zoom, duration: travel(cx, cy, zoom, 400) });
    },
    [boxOf, flow, flowStore, travel],
  );

  // Once the flow has fitted its first view, so that fit does not undo it.
  const [initialized, setInitialized] = useState(false);
  const zoomTo = useRef(zoomToTable);
  useEffect(() => {
    zoomTo.current = zoomToTable;
  });
  // The table a zoom is on its way to: no pan to it first.
  const zooming = useRef<string | null>(null);
  useEffect(() => {
    if (!focus || !initialized) return;
    zooming.current = focus.key;
    const frame = requestAnimationFrame(() => {
      zooming.current = null;
      zoomTo.current(focus.key);
    });
    return () => cancelAnimationFrame(frame);
  }, [focus, initialized]);

  // Read when the selection changes, not a reason to run again: a new model
  // (each compile, each edit) must not pan the view back to the selection.
  const revealRef = useRef(reveal);
  useLayoutEffect(() => {
    revealRef.current = reveal;
  }, [reveal]);
  useEffect(() => {
    if (selected === undefined) return;
    store.set({ selected });
    // A selection made elsewhere (the text pane) brings the table into view.
    if (selected && localPick.current !== selected && zooming.current !== selected)
      revealRef.current(selected);
    localPick.current = undefined;
  }, [selected, store]);

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
          editable: canEdit && editableKind(table.entity),
        },
      ]),
    );
  }, [graph, schemaColors, diff, canEdit]);

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
        // Sizes are computed, not measured: saying so spares React Flow a
        // measure-and-update round on every new node object (each drag frame).
        measured: size,
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
      const width = x1 - x0 + GROUP_PADDING.side * 2;
      const height = y1 - y0 + GROUP_PADDING.top + GROUP_PADDING.bottom;
      boxes.push({
        id: `group:${area.id}`,
        type: "tableGroup",
        position: { x: x0 - GROUP_PADDING.side, y: y0 - GROUP_PADDING.top },
        width,
        height,
        measured: { width, height },
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
          measured: { width: 1, height: 1 },
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

  // Every placed table's box: the edges route around them. Routing all edges
  // costs tens of milliseconds at 300 tables, so while tables are dragged only
  // the edges touching them are routed again; the rest keep the routes of the
  // last drop, and everything is routed once the pointer is up.
  const rectsOf = useCallback(
    (live: Layout) => {
      const out = new Map<string, Rect>();
      for (const table of graph.tables) {
        const p = live[table.key] ?? positions[table.key];
        if (p)
          out.set(table.key, {
            ...p,
            ...tableSize(table, graph.multiSchema, nodeState[table.key]),
          });
      }
      return out;
    },
    [graph, positions, nodeState],
  );
  const settledRects = useMemo(() => {
    const settled: Layout = {};
    for (const [key, d] of Object.entries(dropped)) settled[key] = d.position;
    return rectsOf(settled);
  }, [rectsOf, dropped]);
  const settledRoutes = useMemo(
    () => routeAll(graph, settledRects, nodeState, graph.edges),
    [graph, settledRects, nodeState],
  );
  const edges = useMemo<RelationFlowEdge[]>(() => {
    const live = Object.keys(dragging).length ? rectsOf(moved) : settledRects;
    const touched = graph.edges.filter((e) => dragging[e.source] || dragging[e.target]);
    const fresh = touched.length ? routeAll(graph, live, nodeState, touched) : undefined;
    return graph.edges
      .filter((e) => live.has(e.source) && live.has(e.target))
      .map((e) => ({
        id: e.id,
        source: e.source,
        target: e.target,
        type: "relation",
        data: { model: e, route: fresh?.get(e.id) ?? settledRoutes.get(e.id)! },
        selectable: false,
        focusable: false,
        zIndex: 1,
      }));
  }, [graph, dragging, moved, rectsOf, settledRects, settledRoutes, nodeState]);

  // ---- Editing: moving tables ----------------------------------------------
  // Drag stop can fire before React re-renders: the latest moves live here.
  const draggingRef = useRef<Layout>({});
  const onNodesChange = useCallback(
    (changes: NodeChange<TableFlowNode | GroupFlowNode | AreaLabelFlowNode>[]) => {
      const next: Layout = {};
      for (const c of changes) if (c.type === "position" && c.position) next[c.id] = c.position;
      if (Object.keys(next).length) {
        draggingRef.current = { ...draggingRef.current, ...next };
        setDragging(draggingRef.current);
      }
    },
    [],
  );
  const onNodeDragStop = useCallback(
    (_: unknown, _node: Node, moves: Node[]) => {
      const out: Layout = {};
      const drops: Record<string, Dropped> = {};
      for (const n of moves) {
        const position = draggingRef.current[n.id] ?? n.position;
        out[n.id] = position;
        drops[n.id] = { position, base: layout?.[n.id] };
      }
      draggingRef.current = {};
      setDragging({});
      setDropped((current) => ({ ...current, ...drops }));
      onLayoutChange?.(out);
    },
    [onLayoutChange, layout],
  );

  // ---- Editing the schema ----------------------------------------------------
  const [menu, setMenu] = useState<MenuTarget | null>(null);
  const editorRef = useRef(editor);
  useEffect(() => {
    editorRef.current = editor;
  }, [editor]);
  const marks = editor?.marks ?? NO_MARKS;
  const editingNow = useSyncExternalStore(store.subscribe, () => store.get().editing !== null);
  useEffect(() => {
    store.set({ marks });
  }, [marks, store]);
  // A failure shows its message once, where it happened.
  const seenFailures = useRef(new Set<number>());
  useEffect(() => {
    for (const [key, mark] of marks) {
      if (mark.status !== "failed" || seenFailures.current.has(mark.id)) continue;
      seenFailures.current.add(mark.id);
      if (store.get().editing) continue;
      const [table, column] = key.split("\u0000") as [string, string | undefined];
      store.set({ editing: { kind: "failure", table: table, column } });
    }
  }, [marks, store]);

  const focusGraph = useCallback(() => {
    requestAnimationFrame(() => wrapper.current?.focus({ preventScroll: true }));
  }, []);
  const center = useCallback(() => {
    const el = wrapper.current;
    const { x, y, zoom } = flow.getViewport();
    if (!el) return { x: 0, y: 0 };
    return {
      x: (el.clientWidth / 2 - x) / zoom - NODE.minWidth / 2,
      y: (el.clientHeight / 2 - y) / zoom - NODE.header,
    };
  }, [flow]);
  const startEditing = useCallback(
    (asked: Editing) => {
      setMenu(null);
      setPinned(null);
      // A new table goes where it overlaps nothing, nearest to where it was asked for.
      const editing: Editing =
        asked.kind === "newTable"
          ? {
              ...asked,
              position: freeSpot(
                asked.position,
                { width: NODE.minWidth, height: NODE.header + NODE.row + NODE.padding * 2 },
                graph.tables.map((t) => boxOf(t.key)).filter((b): b is Box => b !== undefined),
              ),
            }
          : asked;
      if ("table" in editing) {
        if (store.get().selected !== editing.table) select(editing.table);
        // Rows are drawn from a readable zoom only: get there first.
        const box = boxOf(editing.table);
        const { zoom } = flow.getViewport();
        if (box && store.get().lod === "overview") {
          autoFit.current = false;
          void flow.setCenter(box.x + box.width / 2, box.y + Math.min(box.height, 320) / 2, {
            zoom: Math.max(zoom, 0.75),
            duration: 300,
          });
        } else if (box) {
          // Only if some of it is off screen: a table in view stays put.
          const el = wrapper.current;
          const left = box.x * zoom + flow.getViewport().x;
          const top = box.y * zoom + flow.getViewport().y;
          const visible =
            !!el &&
            left >= 0 &&
            top >= 0 &&
            left + box.width * zoom <= el.clientWidth &&
            top + Math.min(box.height, 320) * zoom <= el.clientHeight;
          if (!visible) reveal(editing.table);
        }
      }
      store.set({ editing });
    },
    [store, select, boxOf, flow, reveal, graph],
  );
  const stopEditing = useCallback(() => {
    store.set({ editing: null });
    focusGraph();
  }, [store, focusGraph]);
  const emit = useCallback(
    (edit: GraphEdit, options?: { position?: { x: number; y: number } }) => {
      editorRef.current?.onEdit(edit, options);
      store.set({ editing: null });
      if (edit.kind === "addTable") select(`${edit.table.schema}.${edit.table.name}`);
      if (edit.kind === "renameTable") select(`${edit.table.schema}.${edit.to}`);
      focusGraph();
    },
    [store, select, focusGraph],
  );
  const elementOf = useCallback((table: string, column?: string) => {
    const node = wrapper.current?.querySelector(
      `.react-flow__node[data-id="${CSS.escape(table)}"]`,
    );
    if (!node || column === undefined) return node ?? null;
    return node.querySelector(`[data-column="${CSS.escape(column)}"]`) ?? node;
  }, []);
  // The context's actions are stable (they read the latest state when
  // called): a context value changing with every drag frame would redraw
  // every node of a draft at each one.
  const latestActions = useRef({ startEditing, stopEditing, emit, center });
  useLayoutEffect(() => {
    latestActions.current = { startEditing, stopEditing, emit, center };
  }, [startEditing, stopEditing, emit, center]);
  const actions = useMemo(
    () => ({
      start: (editing: Editing) => latestActions.current.startEditing(editing),
      stop: () => latestActions.current.stopEditing(),
      emit: (edit: GraphEdit, options?: { position?: { x: number; y: number } }) =>
        latestActions.current.emit(edit, options),
      center: () => latestActions.current.center(),
    }),
    [],
  );
  const editContext = useMemo<EditContextValue | null>(
    () =>
      editor ? { schema, graph, editor, ...actions, element: elementOf, openMenu: setMenu } : null,
    [editor, schema, graph, actions, elementOf],
  );

  /** Right click: the menu of what is under the pointer (a column, a table, the canvas). */
  const onContextMenu = (event: React.MouseEvent) => {
    if (!editContext) return;
    const target = event.target as Element;
    if (isEditableTarget(target) || target.closest("[role=dialog], [role=menu]")) return;
    event.preventDefault();
    const node = target.closest<HTMLElement>(".react-flow__node[data-id]");
    const table = node?.dataset.id;
    if (table && graph.byKey.has(table)) {
      if (!editableKind(graph.byKey.get(table)!.entity)) return;
      select(table);
      const column = target.closest<HTMLElement>("[data-column]")?.dataset.column;
      setMenu({ table, column, x: event.clientX, y: event.clientY });
    } else if (target.closest(".react-flow__pane")) {
      const at = flow.screenToFlowPosition({ x: event.clientX, y: event.clientY });
      setMenu({ at, x: event.clientX, y: event.clientY });
    }
  };

  /** Keys for editing the selected table (none for a read-only graph). */
  const onEditKey = (event: React.KeyboardEvent, current: string | null): boolean => {
    if (!editContext) return false;
    const table = current ? graph.byKey.get(current) : undefined;
    const editable = table && editableKind(table.entity);
    const key = event.key;
    if ((key === "n" || key === "N") && !event.shiftKey) {
      startEditing({ kind: "newTable", position: center() });
    } else if (!editable || !table) {
      return false;
    } else if (key === "F2") {
      startEditing({ kind: "renameTable", table: table.key });
    } else if (
      (key === "a" || key === "A") &&
      table.kind !== "view" &&
      table.kind !== "materialized"
    ) {
      startEditing({ kind: "addColumn", table: table.key });
    } else if (key === "Delete" || key === "Backspace") {
      startEditing({ kind: "drop", table: table.key });
    } else if (
      key === "m" ||
      key === "M" ||
      key === "ContextMenu" ||
      (key === "F10" && event.shiftKey)
    ) {
      const header = wrapper.current?.querySelector(
        `.react-flow__node[data-id="${CSS.escape(table.key)}"] [data-table-header]`,
      );
      const box = header?.getBoundingClientRect();
      if (!box) return false;
      setMenu({ table: table.key, x: box.left + 12, y: box.bottom });
    } else {
      return false;
    }
    return true;
  };

  // ---- Keyboard -------------------------------------------------------------
  const boxes = useCallback(
    () => graph.tables.map((t) => boxOf(t.key)).filter((b): b is Box => b !== undefined),
    [graph, boxOf],
  );

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
    const target = event.target as HTMLElement;
    // Toolbar buttons keep their keys; a node's buttons never hold focus.
    if (isEditableTarget(target) || target.closest("button, a, [role=dialog]")) return;
    const current = store.get().selected;
    if (onEditKey(event, current)) {
      event.preventDefault();
      return;
    }
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
    } else if (event.key === " " && current) {
      // The keyboard's way to the details a pointer gets by hovering.
      const header = wrapper.current?.querySelector(
        `.react-flow__node[data-id="${CSS.escape(current)}"] [data-table-header]`,
      );
      const table = graph.byKey.get(current);
      setPinned(
        pinned?.table.key === current || !header || !table
          ? null
          : {
              table,
              rect: header.getBoundingClientRect(),
              transform: flowStore.getState().transform,
            },
      );
    } else if ((event.key === "e" || event.key === "E") && current) {
      // Unfold (or fold) the selection's partitions and extra columns: the
      // keyboard's way to the buttons in a node, which are not tab stops.
      const table = graph.byKey.get(current);
      const state = store.get().nodeState[current];
      if (!table) return;
      const open = !(state?.partitions || state?.allColumns);
      store.set({
        nodeState: {
          ...store.get().nodeState,
          [current]: {
            partitions: open && table.partitions.length > 0,
            allColumns: open && table.columns.length > NODE.maxColumns,
          },
        },
      });
    } else if ((event.key === "p" || event.key === "P") && current && onPickedChange) {
      // The keyboard's shift-click: pick (or let go of) the selection.
      const list = picked ?? [];
      onPickedChange(
        list.includes(current) ? list.filter((key) => key !== current) : [...list, current],
      );
    } else if (event.key === "Escape" && pinned) {
      setPinned(null);
    } else if (event.key === "Escape" && picked?.length && onPickedChange) {
      onPickedChange([]);
    } else if (event.key === "Escape" && current) {
      select(null);
    } else if (event.key === "+" || event.key === "=") {
      void flow.zoomIn({ duration: 200 });
    } else if (event.key === "-" || event.key === "_") {
      void flow.zoomOut({ duration: 200 });
    } else if (event.key === "0") {
      autoFit.current = true;
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
      <EditContext.Provider value={editContext}>
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
          onContextMenu={onContextMenu}
          className={cn(
            "schema-graph relative h-full min-h-0 w-full bg-background outline-none",
            "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
            className,
          )}
        >
          <p id={hintId} className="sr-only">
            {graph.tables.length} tables. Arrow keys move between tables, Enter zooms to the
            selected one, slash finds a table.
            {onPickedChange && " P picks the selected table for a group."}
            {editor &&
              " F2 renames the selected table, A adds a column, Delete drops it, M opens its actions; N adds a table."}
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
              onNodeClick={(event, node) => {
                if (node.type !== "table") return;
                if (onPickedChange && (event.shiftKey || event.metaKey || event.ctrlKey)) {
                  const current = picked ?? [];
                  onPickedChange(
                    current.includes(node.id)
                      ? current.filter((key) => key !== node.id)
                      : [...current, node.id],
                  );
                  return;
                }
                select(node.id);
              }}
              onNodeDoubleClick={(_, node) => {
                if (node.type === "table") zoomToTable(node.id);
              }}
              onNodeMouseEnter={(_, node) => {
                if (node.type === "table") store.set({ hovered: node.id });
              }}
              onNodeMouseLeave={() => store.set({ hovered: null })}
              onPaneClick={() => select(null)}
              onMoveStart={(event) => {
                // A wheel, pinch or drag of the reader's: stop fitting for them.
                if (event) autoFit.current = false;
              }}
              onInit={(instance) => {
                // Mounted inside a hidden pane: fit for real once it has a size.
                const el = wrapper.current;
                if (el && el.clientWidth > 0) void instance.fitView({ ...FIT });
                setInitialized(true);
              }}
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
              // Zoomed in, only the tables on screen are mounted; zoomed out,
              // they are, but light (overview bodies, no shadows, no marks).
              onlyRenderVisibleElements
              attributionPosition="bottom-right"
            >
              <Background variant={BackgroundVariant.Dots} gap={22} size={1.25} />
              <ZoomLevel onZoom={applyZoom} />
              {/* One bar across the top: search left, legend and controls right,
                wrapping on a phone. The bar itself lets drags reach the pane. */}
              <Panel
                position="top-left"
                className="pointer-events-none !inset-x-3 !top-3 !m-0 flex items-start justify-between gap-2 *:pointer-events-auto"
              >
                <SearchButton onClick={() => setSearchOpen(true)} />
                <div className="pointer-events-none flex flex-wrap-reverse items-center justify-end gap-2 *:pointer-events-auto">
                  {legend && graph.counts && (
                    <DiffLegend counts={graph.counts} onNext={nextWithStatus} />
                  )}
                  <div className={toolbarClass}>
                    {editor && (
                      <Tooltip content="Add a table" shortcut={["N"]}>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          aria-label="Add a table"
                          onClick={() => startEditing({ kind: "newTable", position: center() })}
                        >
                          <PlusIcon />
                        </Button>
                      </Tooltip>
                    )}
                    <GraphKey diff={graph.counts !== undefined} />
                    {controls}
                  </div>
                </div>
              </Panel>
              <Panel position="bottom-left" className="!m-3">
                <ZoomControls />
              </Panel>
              <NavigatorMap
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
              {editor && (
                <>
                  <NewTable />
                  <LinkDrag />
                </>
              )}
              {children}
            </ReactFlow>
          )}
          {editor && (
            <>
              <EditPopovers />
              <ActionsMenu target={menu} onClose={() => setMenu(null)} />
            </>
          )}
          <HoverDetails
            container={wrapper}
            graph={graph}
            pinned={pinned}
            hidden={editingNow || menu !== null}
          />
          <TableSearch
            open={searchOpen}
            onOpenChange={setSearchOpen}
            graph={graph}
            onPick={(key) => {
              select(key);
              zoomToTable(key);
            }}
            // After a pick, keys go on to the graph (arrows, Enter, E), not
            // back to the search button.
            pickedFocus={wrapper}
          />
        </div>
      </EditContext.Provider>
    </GraphUiContext.Provider>
  );
}

const NO_MARKS: ReadonlyMap<string, ElementMark> = new Map();

interface Dropped {
  position: { x: number; y: number };
  /** `layout` for the table when it was dropped: a change there ends the override. */
  base: { x: number; y: number } | undefined;
}

function samePosition(
  a: { x: number; y: number } | undefined,
  b: { x: number; y: number } | undefined,
) {
  return a === b || (!!a && !!b && a.x === b.x && a.y === b.y);
}

/** Routes for `edges` among the tables in `rects` (see routing.ts). */
function routeAll(
  graph: GraphModel,
  rects: Map<string, Rect>,
  nodeState: Record<string, NodeState>,
  edges: EdgeModel[],
): Map<string, Route> {
  const obstacles = [...rects.values()];
  const anchor = (key: string, column: string | undefined, kind: EdgeModel["kind"]) =>
    rects.get(key)!.y +
    (kind === "reads"
      ? NODE.header / 2
      : columnAnchorY(graph.byKey.get(key)!, column, nodeState[key]));
  const out = new Map<string, Route>();
  for (const e of edges) {
    if (!rects.has(e.source) || !rects.has(e.target)) continue;
    out.set(
      e.id,
      route(
        {
          id: e.id,
          source: rects.get(e.source)!,
          target: rects.get(e.target)!,
          sourceY: anchor(e.source, e.sourceColumn, e.kind),
          targetY: anchor(e.target, e.targetColumn, e.kind),
        },
        obstacles,
      ),
    );
  }
  return out;
}

/**
 * The zoom, written straight to the graph's element: `--graph-zoom` (zoomed-out
 * labels scale against it) and `data-lod` (which body the nodes show), with
 * no React render per zoom step. The level of detail also goes to the store,
 * once per crossing, for nodes that have not mounted their rows yet.
 */
function ZoomLevel({ onZoom }: { onZoom: (zoom: number) => void }) {
  const flowStore = useStoreApi();
  useLayoutEffect(() => {
    let last = flowStore.getState().transform[2];
    onZoom(last);
    return flowStore.subscribe((s) => {
      if (s.transform[2] === last) return;
      last = s.transform[2];
      onZoom(last);
    });
  }, [flowStore, onZoom]);
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
      table.comment,
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
