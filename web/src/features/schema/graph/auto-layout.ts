// Positions for tables missing from the layout map. With no layout at all,
// elkjs lays out the whole graph: small clusters of related tables, each left
// to right (referenced tables first), packed into a screen-shaped area, groups
// and schemas each kept together. Afterwards only new tables are placed, next
// to their foreign-key neighbours, so a table added in a draft lands where
// people will look for it and nothing else moves.
//
// Pure apart from the injected `elk` (elkjs in a web worker in the app, the
// in-process build in tests). Deterministic: inputs are sorted, ELK is seeded,
// and free-spot search walks candidates in a fixed order.

import type { ElkNode, LayoutOptions } from "elkjs/lib/elk-api";

import type { Layout, Position } from "../layout";

export interface LayoutNode {
  id: string;
  width: number;
  height: number;
  /** The first layout keeps each partition (a schema) in its own area. */
  partition?: string;
}

export interface LayoutEdge {
  /** The referencing table. */
  source: string;
  /** The referenced table. */
  target: string;
}

export interface LayoutRequest {
  nodes: LayoutNode[];
  edges: LayoutEdge[];
  /** Known positions; only nodes missing here are placed. */
  positions: Layout;
  /** Group members, by group id: kept together by the first layout. */
  groups?: Record<string, string[]>;
  /** Width over height of the first layout, ideally the pane's. Default 1.6. */
  aspectRatio?: number;
}

export interface ElkLike {
  layout(graph: ElkNode): Promise<ElkNode>;
}

const GAP = 64;
/** Tables per cluster: small enough that a layered layout stays readable. */
const MAX_CLUSTER = 14;

const LAYERED: LayoutOptions = {
  "elk.algorithm": "layered",
  "elk.direction": "RIGHT",
  "elk.randomSeed": "1",
  "elk.spacing.nodeNode": "40",
  "elk.layered.spacing.nodeNodeBetweenLayers": "88",
  "elk.spacing.componentComponent": "56",
  "elk.separateConnectedComponents": "true",
  "elk.aspectRatio": "1.4",
  "elk.layered.nodePlacement.strategy": "BRANDES_KOEPF",
  "elk.layered.crossingMinimization.strategy": "LAYER_SWEEP",
  "elk.padding": "[top=0,left=0,bottom=0,right=0]",
};

/** Clusters, groups and schemas packed into rows, about as wide as a screen. */
const PACKED: LayoutOptions = {
  "elk.algorithm": "rectpacking",
  "elk.aspectRatio": "1.6",
  "elk.spacing.nodeNode": "88",
  "elk.padding": "[top=0,left=0,bottom=0,right=0]",
};

/** Room around a group's members for its tinted area and label (see GROUP_PADDING). */
const GROUP_FRAME = "[top=80,left=40,bottom=40,right=40]";

interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

const overlaps = (a: Rect, b: Rect, margin: number) =>
  a.x < b.x + b.width + margin &&
  b.x < a.x + a.width + margin &&
  a.y < b.y + b.height + margin &&
  b.y < a.y + a.height + margin;

const byId = <T extends { id: string }>(a: T, b: T) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0);

/**
 * The edges that shape the layout, parent first (ELK puts sources left, so
 * referenced tables sit left of the tables referencing them). Dropped: edges
 * into hubs, or every table would line up next to `users`; and edges between
 * partitions (schemas), which are laid out apart.
 */
function shapingEdges(nodes: LayoutNode[], edges: LayoutEdge[]): LayoutEdge[] {
  const byNode = new Map(nodes.map((n) => [n.id, n]));
  const inDegree = new Map<string, number>();
  for (const e of edges) inDegree.set(e.target, (inDegree.get(e.target) ?? 0) + 1);
  const hubLimit = Math.max(5, Math.ceil(nodes.length * 0.08));
  const seen = new Set<string>();
  const out: LayoutEdge[] = [];
  for (const e of [...edges].sort((a, b) =>
    (a.source + "\n" + a.target).localeCompare(b.source + "\n" + b.target),
  )) {
    const s = byNode.get(e.source);
    const t = byNode.get(e.target);
    if (!s || !t || e.source === e.target || s.partition !== t.partition) continue;
    if ((inDegree.get(e.target) ?? 0) > hubLimit) continue;
    const key = `${e.target}\n${e.source}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(e);
  }
  return out;
}

/**
 * Splits `ids` into clusters of at most MAX_CLUSTER tables that hang
 * together: connected components, and big ones cut at their best-connected
 * table, which then joins the piece holding most of its neighbours.
 */
export function clusters(ids: string[], edges: LayoutEdge[]): string[][] {
  const inside = new Set(ids);
  const adjacency = new Map<string, Set<string>>(ids.map((id) => [id, new Set()]));
  for (const e of edges) {
    if (!inside.has(e.source) || !inside.has(e.target)) continue;
    adjacency.get(e.source)!.add(e.target);
    adjacency.get(e.target)!.add(e.source);
  }
  const components = (members: string[]) => {
    const pool = new Set(members);
    const out: string[][] = [];
    for (const start of [...members].sort()) {
      if (!pool.has(start)) continue;
      const component: string[] = [];
      const queue = [start];
      pool.delete(start);
      while (queue.length) {
        const id = queue.shift()!;
        component.push(id);
        for (const next of [...adjacency.get(id)!].sort()) {
          if (pool.delete(next)) queue.push(next);
        }
      }
      out.push(component.sort());
    }
    return out;
  };

  const done: string[][] = [];
  const todo = components(ids);
  while (todo.length) {
    const c = todo.shift()!;
    if (c.length <= MAX_CLUSTER) {
      done.push(c);
      continue;
    }
    // Cut at the best-connected table: what is left falls apart into pieces,
    // and the hub joins the piece holding most of its neighbours.
    const members = new Set(c);
    const degree = (id: string) => [...adjacency.get(id)!].filter((n) => members.has(n)).length;
    const hub = [...c].sort((a, b) => degree(b) - degree(a) || (a < b ? -1 : 1))[0]!;
    const pieces = components(c.filter((id) => id !== hub));
    const neighbours = adjacency.get(hub)!;
    const home = pieces
      .map((piece) => ({ piece, shared: piece.filter((id) => neighbours.has(id)).length }))
      .filter((p) => p.shared > 0 && p.piece.length < MAX_CLUSTER)
      .sort((a, b) => b.shared - a.shared || a.piece.length - b.piece.length)[0];
    if (home) home.piece.push(hub);
    else pieces.push([hub]);
    for (const piece of pieces) todo.push(piece.sort());
  }
  return done.sort((a, b) => b.length - a.length || (a[0]! < b[0]! ? -1 : 1));
}

/** A cluster laid out left to right, or a single table as it is. */
function clusterNode(id: string, members: LayoutNode[], edges: LayoutEdge[]): ElkNode {
  if (members.length === 1)
    return { id: members[0]!.id, width: members[0]!.width, height: members[0]!.height };
  const ids = new Set(members.map((m) => m.id));
  return {
    id,
    layoutOptions: LAYERED,
    children: members.map((m) => ({ id: m.id, width: m.width, height: m.height })),
    edges: edges
      .filter((e) => ids.has(e.source) && ids.has(e.target))
      .map((e, i) => ({ id: `${id}:e${i}`, sources: [e.target], targets: [e.source] })),
  };
}

/** Clusters of `members`, packed. */
function containerNode(
  id: string,
  members: LayoutNode[],
  edges: LayoutEdge[],
  padding?: string,
): ElkNode {
  const byNode = new Map(members.map((m) => [m.id, m]));
  const parts = clusters(
    members.map((m) => m.id),
    edges,
  ).map((ids, i) =>
    clusterNode(
      `${id}/c${i}`,
      ids.map((x) => byNode.get(x)!),
      edges,
    ),
  );
  const options = padding ? { ...PACKED, "elk.padding": padding } : PACKED;
  return { id, layoutOptions: options, children: parts };
}

/**
 * The first layout: groups (and, across schemas, each schema) packed side by
 * side, each made of small clusters of related tables laid out left to right.
 */
async function elkLayout(
  elk: ElkLike,
  nodes: LayoutNode[],
  edges: LayoutEdge[],
  groups: Record<string, string[]> = {},
  aspectRatio = 1.6,
): Promise<Layout> {
  const sorted = [...nodes].sort(byId);
  const shaping = shapingEdges(sorted, edges);
  const ids = new Set(sorted.map((n) => n.id));
  const groupOf = new Map<string, string>();
  for (const id of Object.keys(groups).sort()) {
    for (const member of groups[id]!)
      if (ids.has(member) && !groupOf.has(member)) groupOf.set(member, id);
  }

  const parts: ElkNode[] = [];
  for (const id of Object.keys(groups).sort()) {
    const members = sorted.filter((n) => groupOf.get(n.id) === id);
    if (members.length) parts.push(containerNode(`group:${id}`, members, shaping, GROUP_FRAME));
  }
  const rest = sorted.filter((n) => !groupOf.has(n.id));
  const partitions = [...new Set(rest.map((n) => n.partition ?? ""))].sort();
  for (const partition of partitions) {
    const members = rest.filter((n) => (n.partition ?? "") === partition);
    // One partition and no groups: its clusters are the top level.
    if (partitions.length === 1 && !parts.length)
      parts.push(...containerNode("root", members, shaping).children!);
    else parts.push(containerNode(`part:${partition}`, members, shaping));
  }
  const aspect = String(Math.min(2.4, Math.max(0.8, aspectRatio)).toFixed(2));
  const root: ElkNode = {
    id: "root",
    layoutOptions: { ...PACKED, "elk.aspectRatio": aspect },
    children: parts,
  };
  const result = await elk.layout(root);
  const positions: Layout = {};
  const walk = (node: ElkNode, dx: number, dy: number) => {
    for (const child of node.children ?? []) {
      const x = dx + (child.x ?? 0);
      const y = dy + (child.y ?? 0);
      if (child.children) walk(child, x, y);
      else positions[child.id] = { x: Math.round(x), y: Math.round(y) };
    }
  };
  walk(result, 0, 0);
  return positions;
}

const angle = (p: Position, from: Position) =>
  (Math.atan2(p.y - from.y, p.x - from.x) + 2 * Math.PI) % (2 * Math.PI);

/**
 * The first free spot for `block` nearest to `target`: candidates on a grid,
 * by distance, ties broken by angle so the order never depends on the engine.
 */
function freeSpot(
  target: Position,
  block: { width: number; height: number },
  taken: Rect[],
): Position {
  const step = 48;
  for (let ring = 0; ring < 200; ring++) {
    const candidates: Position[] = [];
    for (let i = -ring; i <= ring; i++) {
      for (let j = -ring; j <= ring; j++) {
        if (Math.max(Math.abs(i), Math.abs(j)) !== ring) continue;
        candidates.push({ x: target.x + i * step, y: target.y + j * step });
      }
    }
    candidates.sort((a, b) => {
      const da = Math.hypot(a.x - target.x, a.y - target.y);
      const db = Math.hypot(b.x - target.x, b.y - target.y);
      if (da !== db) return da - db;
      // Equally near: right of the target first, then clockwise (below, left, above).
      return angle(a, target) - angle(b, target);
    });
    for (const c of candidates) {
      const rect = { ...c, ...block };
      if (!taken.some((t) => overlaps(rect, t, GAP / 2)))
        return { x: Math.round(c.x), y: Math.round(c.y) };
    }
  }
  return target;
}

/** Connected components of the missing nodes, each sorted, in order of their first id. */
function components(missing: LayoutNode[], edges: LayoutEdge[]): LayoutNode[][] {
  const ids = new Map(missing.map((n) => [n.id, n]));
  const adjacent = new Map<string, string[]>();
  for (const e of edges) {
    if (!ids.has(e.source) || !ids.has(e.target) || e.source === e.target) continue;
    adjacent.set(e.source, [...(adjacent.get(e.source) ?? []), e.target]);
    adjacent.set(e.target, [...(adjacent.get(e.target) ?? []), e.source]);
  }
  const seen = new Set<string>();
  const out: LayoutNode[][] = [];
  for (const n of [...missing].sort(byId)) {
    if (seen.has(n.id)) continue;
    const component: LayoutNode[] = [];
    const queue = [n.id];
    seen.add(n.id);
    while (queue.length) {
      const id = queue.shift()!;
      component.push(ids.get(id)!);
      for (const next of (adjacent.get(id) ?? []).sort()) {
        if (!seen.has(next)) {
          seen.add(next);
          queue.push(next);
        }
      }
    }
    out.push(component.sort(byId));
  }
  return out;
}

/** Positions for every node missing from `request.positions` (and only those). */
export async function placeMissing(request: LayoutRequest, elk: ElkLike): Promise<Layout> {
  const { nodes, edges, positions, groups = {}, aspectRatio } = request;
  const missing = nodes.filter((n) => !positions[n.id]);
  if (!missing.length) return {};
  const placedNodes = nodes.filter((n) => positions[n.id]);
  if (!placedNodes.length) return elkLayout(elk, nodes, edges, groups, aspectRatio);

  const size = new Map(nodes.map((n) => [n.id, n]));
  const taken: Rect[] = placedNodes.map((n) => ({
    ...positions[n.id]!,
    width: n.width,
    height: n.height,
  }));
  const known: Layout = { ...positions };
  const out: Layout = {};

  const neighbours = (ids: Set<string>) => {
    const found = new Set<string>();
    for (const e of edges) {
      if (ids.has(e.source) && known[e.target] && !ids.has(e.target)) found.add(e.target);
      if (ids.has(e.target) && known[e.source] && !ids.has(e.source)) found.add(e.source);
    }
    if (found.size) return [...found].sort();
    // No placed reference: sit with the rest of the table's group.
    for (const members of Object.values(groups)) {
      if (members.some((m) => ids.has(m)))
        for (const m of members) if (known[m] && !ids.has(m)) found.add(m);
    }
    return [...found].sort();
  };

  for (const component of components(missing, edges)) {
    const ids = new Set(component.map((n) => n.id));
    const relative =
      component.length === 1
        ? { [component[0]!.id]: { x: 0, y: 0 } }
        : await elkLayout(elk, component, edges);
    let width = 0;
    let height = 0;
    for (const n of component) {
      const p = relative[n.id]!;
      width = Math.max(width, p.x + n.width);
      height = Math.max(height, p.y + n.height);
    }

    const anchors = neighbours(ids);
    let target: Position;
    if (anchors.length) {
      // Right of the neighbours, vertically centred on them: the reading
      // direction of the first layout (referenced tables on the left).
      let right = -Infinity;
      let cy = 0;
      for (const a of anchors) {
        const n = size.get(a)!;
        right = Math.max(right, known[a]!.x + n.width);
        cy += known[a]!.y + n.height / 2;
      }
      target = { x: right + GAP, y: cy / anchors.length - height / 2 };
    } else {
      // Unrelated to anything placed: a new row under the diagram.
      const left = Math.min(...taken.map((t) => t.x));
      const bottom = Math.max(...taken.map((t) => t.y + t.height));
      target = { x: left, y: bottom + GAP * 1.5 };
    }
    const at = freeSpot(target, { width, height }, taken);
    for (const n of component) {
      const p = { x: at.x + relative[n.id]!.x, y: at.y + relative[n.id]!.y };
      out[n.id] = p;
      known[n.id] = p;
      taken.push({ ...p, width: n.width, height: n.height });
    }
  }
  return out;
}
