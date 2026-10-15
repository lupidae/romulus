import type { SchemaDiffInput } from "../diff";
import type { Schema } from "../model";
import { buildGraph, type GraphModel } from "./build";

const cache = new WeakMap<Schema, WeakMap<object, GraphModel>>();
const NO_DIFF = {};

/**
 * `buildGraph`, once per (schema, diff) pair: the graph and the text pane of
 * one viewer share the model instead of building it twice.
 */
export function graphOf(schema: Schema, diff?: SchemaDiffInput): GraphModel {
  let byDiff = cache.get(schema);
  if (!byDiff) {
    byDiff = new WeakMap();
    cache.set(schema, byDiff);
  }
  const key = diff ?? NO_DIFF;
  let graph = byDiff.get(key);
  if (!graph) {
    graph = buildGraph({ schema, diff });
    byDiff.set(key, graph);
  }
  return graph;
}
