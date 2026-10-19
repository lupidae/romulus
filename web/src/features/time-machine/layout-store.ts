// Where the time machine placed the tables no version placed: kept in this
// browser, per database, so the first layout of a long history (seconds of
// elk at a few hundred tables) is computed once, not on every visit.
// Versions are immutable and so are their layouts: these positions cannot go
// there. Positions a version saved always win over these.

import type { GraphModel } from "@/features/schema/graph/build";
import type { Layout, Position } from "@/features/schema/layout";

const PREFIX = "romulus:time-machine:layout:";

/** The positions kept for `key` (a database's id); empty when none or unreadable. */
export function readPlaced(key: string | undefined): Layout {
  if (!key) return {};
  try {
    const raw = localStorage.getItem(PREFIX + key);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return {};
    const out: Layout = {};
    for (const [table, p] of Object.entries(parsed as Record<string, unknown>)) {
      const { x, y } = (p ?? {}) as Partial<Position>;
      if (typeof x === "number" && typeof y === "number") out[table] = { x, y };
    }
    return out;
  } catch {
    return {};
  }
}

/**
 * Keeps `placed` for `key`, with what was kept before for tables the
 * history still has (`graph`): a table gone from every version is dropped.
 */
export function keepPlaced(key: string | undefined, placed: Layout, graph: GraphModel) {
  if (!key) return;
  try {
    const all = { ...readPlaced(key), ...placed };
    const kept: Layout = {};
    for (const table of graph.tables) {
      const p = all[table.key];
      if (p) kept[table.key] = { x: Math.round(p.x), y: Math.round(p.y) };
    }
    localStorage.setItem(PREFIX + key, JSON.stringify(kept));
  } catch {
    // Storage full or off (a private window): the layout is computed again next time.
  }
}

/**
 * `graph` with only the tables that have a place (and the edges between
 * them): the same objects, so the graph's caches still hold. While new
 * tables wait for theirs, the rest stays on screen.
 */
export function placedOnly(graph: GraphModel, positions: Layout): GraphModel {
  if (graph.tables.every((t) => positions[t.key])) return graph;
  const tables = graph.tables.filter((t) => positions[t.key]);
  return {
    ...graph,
    tables,
    byKey: new Map(tables.map((t) => [t.key, t])),
    edges: graph.edges.filter((e) => positions[e.source] && positions[e.target]),
  };
}
