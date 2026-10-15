// Per-graph UI state that changes often (selection, hover, unfolded nodes),
// kept out of React Flow's node objects: a selection change re-renders the
// handful of nodes and edges whose emphasis changes, not all 300.

import { createContext, useContext, useSyncExternalStore } from "react";

import type { NodeState } from "./geometry";

/** Level of detail: rows are drawn only when they can be read. */
export type Lod = "overview" | "detail";

export interface GraphUiState {
  selected: string | null;
  /** Tables picked for an action on several (shift- or ⌘-click), by key. */
  picked: readonly string[];
  hovered: string | null;
  /** Unfolded columns or partitions, by table key. */
  nodeState: Record<string, NodeState>;
  lod: Lod;
}

export interface GraphUiStore {
  get: () => GraphUiState;
  set: (patch: Partial<GraphUiState>) => void;
  subscribe: (listener: () => void) => () => void;
}

export function createGraphUiStore(initial: Partial<GraphUiState> = {}): GraphUiStore {
  // Overview until the zoom is known: no node mounts rows it may not show.
  let state: GraphUiState = {
    selected: null,
    picked: [],
    hovered: null,
    nodeState: {},
    lod: "overview",
    ...initial,
  };
  const listeners = new Set<() => void>();
  return {
    get: () => state,
    set: (patch) => {
      const next = { ...state, ...patch };
      if (
        Object.keys(patch).every(
          (k) => next[k as keyof GraphUiState] === state[k as keyof GraphUiState],
        )
      )
        return;
      state = next;
      listeners.forEach((l) => l());
    },
    subscribe: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  };
}

export const GraphUiContext = createContext<GraphUiStore | null>(null);

/** A slice of the graph's UI state; re-renders only when the slice changes (compare primitives). */
export function useGraphUi<T>(select: (state: GraphUiState) => T): T {
  const store = useContext(GraphUiContext);
  if (!store) throw new Error("useGraphUi outside a SchemaGraph");
  return useSyncExternalStore(
    store.subscribe,
    () => select(store.get()),
    () => select(store.get()),
  );
}

export function useGraphUiStore(): GraphUiStore {
  const store = useContext(GraphUiContext);
  if (!store) throw new Error("useGraphUiStore outside a SchemaGraph");
  return store;
}
