// What the graph reads from a romulus-diff `SchemaDiff`: its status map.
//
// The structural subset of `romulus_diff::StatusMap` (crates/romulus-diff/src/
// model.rs). When the API exposes the diff, the generated `SchemaDiff["status"]`
// type is assignable to `StatusMap` here, so pass it through as is.

import type { RelationRef, Schema } from "./model";

/** `unchanged` exists only in the status map; change lists leave it out. */
export type DiffStatus = "added" | "removed" | "changed" | "unchanged";

/**
 * One graph node: the entity's status and that of every part it has on either
 * side. Columns, constraints, indexes and policies by name; partitions (every
 * level under the entity) by schema, then name.
 */
export interface EntityStatus {
  status: DiffStatus;
  columns: Record<string, DiffStatus>;
  constraints: Record<string, DiffStatus>;
  indexes: Record<string, DiffStatus>;
  policies: Record<string, DiffStatus>;
  partitions: Record<string, Record<string, DiffStatus>>;
}

/** Nested by schema, then name, both unquoted: `entities["shop"]["orders"]`. */
export interface StatusMap {
  entities: Record<string, Record<string, EntityStatus>>;
  enums?: Record<string, Record<string, DiffStatus>>;
  domains?: Record<string, Record<string, DiffStatus>>;
  composites?: Record<string, Record<string, DiffStatus>>;
}

/**
 * Diff mode: the graph draws the union of both models (removed tables and
 * columns exist only in `base`) and styles each part by its status.
 */
export interface SchemaDiffInput {
  /** The old model; the viewer's `schema` is the new one. */
  base: Schema;
  status: StatusMap;
}

export function entityStatus(status: StatusMap, ref: RelationRef): EntityStatus | undefined {
  return status.entities[ref.schema]?.[ref.name];
}

/** Statuses worth drawing: `unchanged` is drawn as no status at all. */
export type ChangeStatus = Exclude<DiffStatus, "unchanged">;

export function changeOf(status: DiffStatus | undefined): ChangeStatus | undefined {
  return status === undefined || status === "unchanged" ? undefined : status;
}

export const STATUS_LABEL: Record<ChangeStatus, string> = {
  added: "Added",
  removed: "Removed",
  changed: "Changed",
};

/** The one-character mark of a changed row: `+`, `−` or `~`. */
export function statusGlyph(status: ChangeStatus): string {
  return status === "added" ? "+" : status === "removed" ? "−" : "~";
}

const STATUSES = new Set<string>(["added", "removed", "changed", "unchanged"]);

/**
 * Types a romulus-diff status map read as plain JSON (a fixture, or an API
 * response not yet in the generated types), checking every status on the way.
 */
export function parseStatusMap(value: unknown): StatusMap {
  const fail = (path: string): never => {
    throw new Error(`${path} is not a romulus-diff status map`);
  };
  const isObject = (v: unknown): v is Record<string, unknown> =>
    typeof v === "object" && v !== null && !Array.isArray(v);
  const statuses = (v: unknown, path: string) => {
    if (!isObject(v)) return fail(path);
    for (const [k, s] of Object.entries(v))
      if (typeof s !== "string" || !STATUSES.has(s)) fail(`${path}.${k}`);
  };
  const nested = (v: unknown, path: string, leaf: (v: unknown, path: string) => void) => {
    if (v === undefined) return;
    if (!isObject(v)) return fail(path);
    for (const [schema, byName] of Object.entries(v)) {
      if (!isObject(byName)) return fail(`${path}.${schema}`);
      for (const [name, item] of Object.entries(byName)) leaf(item, `${path}.${schema}.${name}`);
    }
  };
  if (!isObject(value)) return fail("status");
  nested(value.entities, "status.entities", (e, path) => {
    if (!isObject(e) || typeof e.status !== "string" || !STATUSES.has(e.status)) return fail(path);
    for (const key of ["columns", "constraints", "indexes", "policies"])
      statuses(e[key], `${path}.${key}`);
    nested(e.partitions, `${path}.partitions`, (s, p) => {
      if (typeof s !== "string" || !STATUSES.has(s)) fail(p);
    });
  });
  for (const key of ["enums", "domains", "composites"]) {
    nested(value[key], `status.${key}`, (s, p) => {
      if (typeof s !== "string" || !STATUSES.has(s)) fail(p);
    });
  }
  return value as unknown as StatusMap;
}
