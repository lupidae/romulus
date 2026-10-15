import { useEffect, useMemo, useRef, useState } from "react";

import type { Groups, Layout } from "../layout";
import { type ElkLike, type LayoutRequest, placeMissing } from "./auto-layout";
import type { GraphModel } from "./build";
import { tableSize } from "./geometry";

let elk: Promise<ElkLike> | undefined;

/**
 * elkjs, loaded on first use, its algorithm running in a web worker (1.6 MB
 * that only a graph with unplaced tables ever needs).
 */
function loadElk(): Promise<ElkLike> {
  elk ??= Promise.all([
    import("elkjs/lib/elk-api"),
    import("elkjs/lib/elk-worker.min.js?url"),
  ]).then(
    ([{ default: ELK }, { default: workerUrl }]) =>
      new ELK({ workerFactory: () => new Worker(workerUrl) }),
  );
  return elk;
}

/** A grid, when ELK cannot load: every table still shows up. */
function gridFallback(request: LayoutRequest): Layout {
  const out: Layout = {};
  const columns = Math.ceil(Math.sqrt(request.nodes.length));
  request.nodes
    .filter((n) => !request.positions[n.id])
    .forEach((n, i) => {
      out[n.id] = { x: (i % columns) * 400, y: Math.floor(i / columns) * 420 };
    });
  return out;
}

export interface AutoLayout {
  /** `layout` plus the computed positions of every table missing from it. */
  positions: Layout;
  /** Tables still waiting for a position. */
  pending: number;
}

/**
 * Places tables missing from `layout` (all of them the first time) and reports
 * the new positions through `onPlaced`, so a draft can write them into its
 * layout map. Without a callback they are kept here, for this view only.
 */
export function useAutoLayout(
  graph: GraphModel,
  layout: Layout | undefined,
  groups: Groups | undefined,
  onPlaced?: (positions: Layout) => void,
  /** The pane's width over height when layout starts, read once per layout. */
  aspectRatio?: React.RefObject<number>,
): AutoLayout {
  const [computed, setComputed] = useState<Layout>({});
  const known = useMemo(() => ({ ...computed, ...layout }), [computed, layout]);
  const missing = useMemo(
    () => graph.tables.filter((t) => !known[t.key]).map((t) => t.key),
    [graph, known],
  );
  const missingKey = missing.join("\n");

  // The job reads the latest graph and positions when it starts; it is keyed
  // by the set of missing tables only, so a recompile that leaves the same
  // tables unplaced (a draft compiles on every pause in typing) does not
  // throw away a layout in flight.
  const latest = useRef({ graph, known, groups, onPlaced });
  useEffect(() => {
    latest.current = { graph, known, groups, onPlaced };
  });

  useEffect(() => {
    if (!missingKey) return;
    let cancelled = false;
    const started = performance.now();
    const { graph, known, groups } = latest.current;
    const request: LayoutRequest = {
      nodes: graph.tables.map((t) => ({
        id: t.key,
        ...tableSize(t, graph.multiSchema),
        partition: graph.multiSchema ? t.schema : undefined,
      })),
      edges: graph.edges.map((e) => ({ source: e.source, target: e.target })),
      positions: known,
      groups:
        groups && Object.fromEntries(Object.entries(groups).map(([id, g]) => [id, g.members])),
      aspectRatio: aspectRatio?.current,
    };
    void loadElk()
      .then((engine) => placeMissing(request, engine))
      .catch((error: unknown) => {
        console.error("schema graph: automatic layout failed, using a grid", error);
        return gridFallback(request);
      })
      .then((placed) => {
        if (cancelled) return;
        // User Timing, for the perf spec and the browser's profiler.
        performance.measure(`schema graph: layout of ${Object.keys(placed).length} tables`, {
          start: started,
        });
        setComputed((prev) => ({ ...prev, ...placed }));
        latest.current.onPlaced?.(placed);
      });
    return () => {
      cancelled = true;
    };
  }, [missingKey, aspectRatio]);

  return { positions: known, pending: missing.length };
}
