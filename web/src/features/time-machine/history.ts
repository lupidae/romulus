// A database's history object by object, as the API's timeline sends it
// (crates/romulus-api/src/versions/timeline): each table, column, constraint,
// index, policy, type, partition and extension with its states, each state
// what the object was from one version (an index into `versions`) until
// another.

import type { components } from "@/api/schema.gen";
import type { Groups, Layout } from "@/features/schema/layout";
import {
  type Column,
  type CompositeType,
  type Constraint,
  type DomainType,
  type Entity,
  type EnumType,
  type Extension,
  type Index,
  type Partition,
  type Policy,
  SchemaParseError,
} from "@/features/schema/model";

export type Timeline = components["schemas"]["Timeline"];
export type TimelineVersion = components["schemas"]["TimelineVersion"];
export type ObjectChange = components["schemas"]["ObjectChange"];

/** What an object was from version `from` (an index) until version `to` (`null`: still is). */
export interface State<V> {
  from: number;
  to: number | null;
  value: V;
  /** What changed into it, when it directly follows another state. */
  changes: ObjectChange[];
}

/** A table's own state: the entity without its parts, its columns in order. */
export type TableHead = Omit<Entity, "columns" | "constraints" | "indexes" | "policies"> & {
  column_order?: string[];
};

export interface PartTrack<V> {
  /** Unique in its table and kind: its last name. */
  id: string;
  states: State<V>[];
}

export interface TableTrack {
  /** Unique in the history: `schema.name` of its last state (`#2`… when two end alike). */
  id: string;
  /** Its schema and name in its last state. */
  schema: string;
  name: string;
  states: State<TableHead>[];
  columns: PartTrack<Column>[];
  constraints: PartTrack<Constraint>[];
  indexes: PartTrack<Index>[];
  policies: PartTrack<Policy>[];
}

export type UserTypeKind = "enum" | "domain" | "composite";

export interface TypeTrack {
  kind: UserTypeKind;
  schema: string;
  name: string;
  states: State<EnumType | DomainType | CompositeType>[];
}

export interface ObjectTrack<V> {
  schema: string | null;
  name: string;
  states: State<V>[];
}

export interface History {
  /** Oldest first. */
  versions: TimelineVersion[];
  /** Versions older than the first one, left out by the API. */
  earlier: number;
  tables: TableTrack[];
  types: TypeTrack[];
  partitions: ObjectTrack<Partition>[];
  extensions: ObjectTrack<Extension>[];
  /** Where each table of the history sits, as versions saved it. */
  layout: Layout;
  groups: Groups;
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/**
 * Types the API's timeline, checking what the time machine relies on: every
 * state's span within the versions, and a value that is an object. The
 * values are remus JSON as the API stores it (see `parseSchema`).
 */
export function parseHistory(timeline: Timeline): History {
  const count = timeline.versions.length;
  const states = <V>(list: unknown, path: string): State<V>[] => {
    if (!Array.isArray(list)) throw new SchemaParseError(`${path} should be a list`);
    let last = -1;
    for (const [i, s] of list.entries()) {
      const at = `${path}[${i}]`;
      if (!isObject(s) || !isObject(s.value) || !Array.isArray(s.changes))
        throw new SchemaParseError(`${at} should be a state`);
      const { from, to } = s;
      if (typeof from !== "number" || from < last || from >= count)
        throw new SchemaParseError(`${at}.from is out of the timeline`);
      if (to !== null && (typeof to !== "number" || to <= from || to >= count))
        throw new SchemaParseError(`${at}.to is out of the timeline`);
      last = to ?? count;
    }
    return list as State<V>[];
  };
  const tables = timeline.history.tables.map((t, i) => {
    const path = `tables[${i}]`;
    const parts = <V>(list: { id: string; states: unknown }[], kind: string) =>
      list.map((p, j) => ({ id: p.id, states: states<V>(p.states, `${path}.${kind}[${j}]`) }));
    return {
      id: t.id,
      schema: t.schema,
      name: t.name,
      states: states<TableHead>(t.states, `${path}.states`),
      columns: parts<Column>(t.columns, "columns"),
      constraints: parts<Constraint>(t.constraints, "constraints"),
      indexes: parts<Index>(t.indexes, "indexes"),
      policies: parts<Policy>(t.policies, "policies"),
    };
  });
  return {
    versions: timeline.versions,
    earlier: timeline.earlier,
    tables,
    types: timeline.history.types.map((t, i) => ({
      kind: t.kind,
      schema: t.schema,
      name: t.name,
      states: states(t.states, `types[${i}]`),
    })),
    partitions: timeline.history.partitions.map((p, i) => ({
      schema: p.schema,
      name: p.name,
      states: states<Partition>(p.states, `partitions[${i}]`),
    })),
    extensions: timeline.history.extensions.map((x, i) => ({
      schema: x.schema,
      name: x.name,
      states: states<Extension>(x.states, `extensions[${i}]`),
    })),
    layout: timeline.layout.layout,
    groups: timeline.layout.groups,
  };
}

/** Whether a state covers version `at`. */
export const covers = (s: State<unknown>, at: number) =>
  s.from <= at && (s.to === null || s.to > at);

/** The state at version `at`, if the object exists then. */
export function stateAt<V>(states: readonly State<V>[], at: number): State<V> | undefined {
  // States are few and sorted: the last one starting by then, if it lasts.
  for (let i = states.length - 1; i >= 0; i--) {
    const s = states[i]!;
    if (s.from <= at) return s.to === null || s.to > at ? s : undefined;
  }
  return undefined;
}

/** The last state that existed between versions `from` and `to` (inclusive). */
export function lastWithin<V>(
  states: readonly State<V>[],
  from: number,
  to: number,
): State<V> | undefined {
  for (let i = states.length - 1; i >= 0; i--) {
    const s = states[i]!;
    if (s.from <= to && (s.to === null || s.to > from)) return s;
  }
  return undefined;
}

/** The first state that existed between versions `from` and `to` (inclusive). */
export function firstWithin<V>(
  states: readonly State<V>[],
  from: number,
  to: number,
): State<V> | undefined {
  return states.find((s) => s.from <= to && (s.to === null || s.to > from));
}

/**
 * Where the object first appeared within (`from`, `to`]: the version index,
 * or `undefined` when it was there at `from` already, or never came then.
 */
export function bornWithin(states: readonly State<unknown>[], from: number, to: number) {
  if (stateAt(states, from)) return undefined;
  return states.find((s) => s.from > from && s.from <= to)?.from;
}

/** Whether the state follows the one before it directly (a change, not a return). */
export function follows(states: readonly State<unknown>[], i: number): boolean {
  return i > 0 && states[i - 1]!.to === states[i]!.from;
}

/**
 * The run of states the state at `index` belongs to: those around it, each
 * directly following the one before (changes, not a removal and a return).
 */
export function runOf<V>(
  states: readonly State<V>[],
  index: number,
): { first: State<V>; last: State<V> } {
  let first = index;
  let last = index;
  while (first > 0 && follows(states, first)) first--;
  while (last + 1 < states.length && follows(states, last + 1)) last++;
  return { first: states[first]!, last: states[last]! };
}

/**
 * The versions (indexes) at which the object changed within (`from`, `to`]:
 * states directly following another, with something said about it.
 */
export function changesWithin(states: readonly State<unknown>[], from: number, to: number) {
  const out: number[] = [];
  states.forEach((s, i) => {
    if (s.from > from && s.from <= to && follows(states, i) && s.changes.length > 0)
      out.push(s.from);
  });
  return out;
}

/** The version (index) where the object last went within (`from`, `to`], if it is gone at `to`. */
export function goneWithin(states: readonly State<unknown>[], from: number, to: number) {
  if (stateAt(states, to)) return undefined;
  const last = lastWithin(states, from, to);
  return last?.to ?? undefined;
}

/**
 * How much happened at each version: objects that appeared, went or changed
 * there (tables and their parts, types). Index 0 counts the first version's
 * objects.
 */
export function activity(history: History): number[] {
  const out = new Array<number>(history.versions.length).fill(0);
  const count = (states: readonly State<unknown>[]) => {
    states.forEach((s, i) => {
      if (!follows(states, i) || s.changes.length > 0) out[s.from] = (out[s.from] ?? 0) + 1;
      const next = states[i + 1];
      if (s.to !== null && (!next || next.from !== s.to)) out[s.to] = (out[s.to] ?? 0) + 1;
    });
  };
  for (const t of history.tables) {
    count(t.states);
    for (const p of [...t.columns, ...t.constraints, ...t.indexes, ...t.policies]) count(p.states);
  }
  for (const t of history.types) count(t.states);
  return out;
}
