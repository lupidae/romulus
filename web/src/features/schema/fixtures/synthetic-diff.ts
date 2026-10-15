// A 300-table diff for the diff view's performance spec: the large fixture as
// v1, a seeded "v2" of it (about a tenth of its tables changed, a few removed,
// a dozen added), and a stand-in for romulus-diff's output between the two,
// in its JSON shape (change lines, summary, the status of every part). The
// real diff is computed in Rust by the API; this one only needs the same
// shape and volume. Not product code.

import type { components } from "@/api/schema.gen";

import type { Column, Constraint, Entity, Schema } from "../model";
import { largeFixture, renderFixtureSql } from "./synthetic";

type SchemaDiff = components["schemas"]["SchemaDiff"];
type EntityDiff = SchemaDiff["entities"][number];
type ColumnDiff = EntityDiff["columns"][number];
type ConstraintDiff = EntityDiff["constraints"][number];
type IndexDiff = EntityDiff["indexes"][number];
type EntityStatus = SchemaDiff["status"]["entities"][string][string];
type Status = EntityDiff["status"];

export interface DiffFixture {
  before: Schema;
  after: Schema;
  diff: SchemaDiff;
  sql: { before: string; after: string };
}

/** mulberry32, as in synthetic.ts. */
function random(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const key = (e: { schema: string; name: string }) => `${e.schema}.${e.name}`;

const column = (name: string, type: string, typeName: string, notNull: boolean): Column => ({
  name,
  type,
  type_schema: "pg_catalog",
  type_name: typeName,
  type_kind: "b",
  not_null: notNull,
  default: null,
  identity: null,
  generated: null,
  comment: null,
});

/**
 * v2 of the large fixture: the same seed always makes the same changes, to
 * `share` of the tables (and a few removed, a dozen added, unless every
 * table changes).
 */
function nextVersion(before: Schema, seed: number, share: number): Schema {
  const rand = random(seed);
  const after = structuredClone(before);
  const entities = after.entities ?? [];
  const referenced = new Set(
    entities.flatMap((e) =>
      (e.constraints ?? [])
        .filter((c) => c.kind === "f" && c.ref_schema && c.ref_table)
        .map((c) => `${c.ref_schema}.${c.ref_table}`),
    ),
  );
  const kept: Entity[] = [];
  for (const e of entities) {
    const isTable = e.kind === "r";
    const roll = rand();
    // Removed: tables nothing points at, so no foreign key loses its target.
    if (share < 1 && isTable && roll < 0.03 && !referenced.has(key(e))) continue;
    if (isTable && roll >= 1 - share) {
      const columns = e.columns ?? [];
      const change = Math.floor(rand() * 4);
      if (change === 0) columns.push(column("notes", "text", "text", false));
      else if (change === 1) {
        const extra = columns.findIndex(
          (c) => c.name !== "id" && !c.name.endsWith("_id") && c.name !== "created_at",
        );
        if (extra >= 0) columns.splice(extra, 1);
        else columns.push(column("legacy_ref", "uuid", "uuid", false));
      } else if (change === 2) {
        const c = columns.find((c) => c.name !== "id" && !c.not_null);
        if (c) c.not_null = true;
        else columns.push(column("flag", "boolean", "bool", true));
      } else {
        const c = columns.find((c) => c.type === "text");
        if (c) {
          c.type = "character varying(255)";
          c.type_name = "varchar";
        } else columns.push(column("label", "character varying(80)", "varchar", false));
      }
      if (rand() < 0.3) {
        const extra = column("score", "integer", "int4", false);
        columns.push(extra);
        e.indexes = [
          ...(e.indexes ?? []),
          {
            name: `${e.name}_score_idx`,
            definition: `CREATE INDEX ${e.name}_score_idx ON ${e.schema}.${e.name} USING btree (score)`,
          },
        ];
      }
    }
    kept.push(e);
  }
  // A dozen new tables, each referencing a table of its schema.
  const schemas = [...new Set(kept.map((e) => e.schema))];
  for (let i = 0; i < (share < 1 ? 12 : 0); i++) {
    const schema = schemas[i % schemas.length]!;
    const target = kept.find((e) => e.schema === schema && e.kind === "r")!;
    const name = `feature_${String(i + 1).padStart(2, "0")}_${target.name}`;
    const fk: Constraint = {
      name: `${name}_${target.name}_id_fkey`,
      kind: "f",
      columns: [`${target.name}_id`],
      ref_schema: schema,
      ref_table: target.name,
      ref_columns: ["id"],
      on_update: "a",
      on_delete: "c",
      definition: `FOREIGN KEY (${target.name}_id) REFERENCES ${schema}.${target.name}(id) ON DELETE CASCADE`,
      comment: null,
    };
    kept.push({
      schema,
      name,
      kind: "r",
      comment: null,
      partition_key: null,
      view_definition: null,
      columns: [
        { ...column("id", "bigint", "int8", true), identity: "a" },
        column(`${target.name}_id`, "bigint", "int8", true),
        column("payload", "jsonb", "jsonb", false),
        column("created_at", "timestamp with time zone", "timestamptz", true),
      ],
      constraints: [
        {
          name: `${name}_pkey`,
          kind: "p",
          columns: ["id"],
          ref_schema: null,
          ref_table: null,
          on_update: null,
          on_delete: null,
          definition: "PRIMARY KEY (id)",
          comment: null,
        },
        fk,
      ],
      indexes: [],
    });
  }
  after.entities = kept;
  return after;
}

const definition = (c: Column) =>
  `${c.type}${c.default ? ` DEFAULT ${c.default}` : ""}${c.not_null ? " NOT NULL" : ""}`;

function byName<T extends { name: string }>(list: T[] | undefined): Map<string, T> {
  return new Map((list ?? []).map((x) => [x.name, x]));
}

/** Every name of either side, sorted; with each side's item. */
function pairs<T extends { name: string }>(a: T[] | undefined, b: T[] | undefined) {
  const before = byName(a);
  const after = byName(b);
  return [...new Set([...before.keys(), ...after.keys()])]
    .sort()
    .map((name) => ({ name, before: before.get(name), after: after.get(name) }));
}

const statusOf = <T>(before: T | undefined, after: T | undefined, same: boolean): Status =>
  !before ? "added" : !after ? "removed" : same ? "unchanged" : "changed";

function entityDiff(
  before: Entity | undefined,
  after: Entity | undefined,
): { diff: EntityDiff | null; status: EntityStatus } {
  const e = (after ?? before)!;
  const label = `${e.schema}.${e.name}`;
  const whole = !before || !after;
  const lines: string[] = [];
  const columns: ColumnDiff[] = [];
  const columnStatus: Record<string, Status> = {};
  pairs(before?.columns, after?.columns).forEach(({ name, before: b, after: a }, i) => {
    const same = !!a && !!b && definition(a) === definition(b);
    const status = statusOf(b, a, same);
    columnStatus[name] = whole ? (before ? "removed" : "added") : status;
    if (whole || status === "unchanged") {
      if (whole)
        columns.push({
          name,
          status: before ? "removed" : "added",
          position: { before: b ? i : null, after: a ? i : null },
          changes: [],
          definition: { before: b ? definition(b) : null, after: a ? definition(a) : null },
        });
      return;
    }
    const changes: ColumnDiff["changes"] = [];
    if (a && b && a.type !== b.type) changes.push({ field: "type", before: b.type, after: a.type });
    if (a && b && !!a.not_null !== !!b.not_null)
      changes.push({ field: "not_null", before: !!b.not_null, after: !!a.not_null });
    columns.push({
      name,
      status,
      position: { before: b ? i : null, after: a ? i : null },
      changes,
      definition: { before: b ? definition(b) : null, after: a ? definition(a) : null },
    });
    if (status === "added") lines.push(`column \`${label}.${name}\` added: ${definition(a!)}`);
    else if (status === "removed") lines.push(`column \`${label}.${name}\` removed`);
    else
      for (const c of changes)
        lines.push(
          c.field === "type"
            ? `\`${label}.${name}\`: type ${String(c.before)} → ${String(c.after)}`
            : `\`${label}.${name}\`: now ${c.after ? "NOT NULL" : "nullable"}`,
        );
  });
  const constraints: ConstraintDiff[] = [];
  const constraintStatus: Record<string, Status> = {};
  for (const { name, before: b, after: a } of pairs(before?.constraints, after?.constraints)) {
    const status = whole
      ? before
        ? "removed"
        : "added"
      : statusOf(b, a, a?.definition === b?.definition);
    constraintStatus[name] = status;
    if (status === "unchanged") continue;
    constraints.push({
      name,
      kind:
        (a ?? b)!.kind === "f" ? "foreign_key" : (a ?? b)!.kind === "p" ? "primary_key" : "unique",
      status,
      changes: [],
      definition: { before: b?.definition ?? null, after: a?.definition ?? null },
    });
  }
  const indexes: IndexDiff[] = [];
  const indexStatus: Record<string, Status> = {};
  for (const { name, before: b, after: a } of pairs(before?.indexes, after?.indexes)) {
    const status = whole
      ? before
        ? "removed"
        : "added"
      : statusOf(b, a, a?.definition === b?.definition);
    indexStatus[name] = status;
    if (status === "unchanged") continue;
    indexes.push({
      name,
      status,
      changes: [],
      definition: { before: b?.definition ?? null, after: a?.definition ?? null },
    });
    if (!whole && status === "added")
      lines.push(`index \`${name}\` on \`${label}\` added: ${a!.definition}`);
  }
  const changed = !whole && (lines.length > 0 || constraints.length > 0);
  const status: Status = whole ? (before ? "removed" : "added") : changed ? "changed" : "unchanged";
  if (whole) lines.splice(0, lines.length, `table \`${label}\` ${status}`);
  const kinds: Record<string, EntityDiff["kind"]> = {
    r: "table",
    p: "partitioned_table",
    v: "view",
    m: "materialized_view",
    f: "foreign_table",
  };
  return {
    diff:
      status === "unchanged"
        ? null
        : {
            schema: e.schema,
            name: e.name,
            kind: kinds[e.kind] ?? "table",
            status,
            changes: [],
            columns,
            constraints,
            indexes,
            policies: [],
            partitions: [],
            lines,
            sql: { before: null, after: null },
          },
    status: {
      status,
      columns: columnStatus,
      constraints: constraintStatus,
      indexes: indexStatus,
      policies: {},
      partitions: {},
    },
  };
}

/** romulus-diff's output for two models, entities only (the large fixture has nothing else). */
export function diffOf(before: Schema, after: Schema): SchemaDiff {
  const old = new Map((before.entities ?? []).map((e) => [key(e), e]));
  const now = new Map((after.entities ?? []).map((e) => [key(e), e]));
  const names = [...new Set([...old.keys(), ...now.keys()])].sort();
  const entities: EntityDiff[] = [];
  const status: SchemaDiff["status"]["entities"] = {};
  const counts = { added: 0, removed: 0, changed: 0 };
  for (const name of names) {
    const { diff, status: s } = entityDiff(old.get(name), now.get(name));
    const e = (now.get(name) ?? old.get(name))!;
    (status[e.schema] ??= {})[e.name] = s;
    if (diff) {
      entities.push(diff);
      counts[diff.status as keyof typeof counts]++;
    }
  }
  const parts = (["added", "changed", "removed"] as const).filter((k) => counts[k] > 0);
  const text = parts.length
    ? parts
        .map((k, i) =>
          i === 0
            ? `${counts[k]} ${counts[k] === 1 ? "table" : "tables"} ${k}`
            : `${counts[k]} ${k}`,
        )
        .join(", ")
    : "no changes";
  return {
    summary: {
      counts: parts.length ? [{ category: "table", ...counts }] : [],
      text,
    },
    entities,
    enums: [],
    domains: [],
    composites: [],
    extensions: [],
    orphan_partitions: { partitions: [], lines: [], sql: { before: null, after: null } },
    status: {
      entities: status,
      orphan_partitions: {},
      enums: {},
      domains: {},
      composites: {},
      extensions: {},
    },
  };
}

/**
 * The large fixture (`count` tables) and its next version, compared: about a
 * tenth of the tables changed, or (`every`) all of them, the change list's
 * stress case.
 */
export function largeDiffFixture(count = 300, { every = false } = {}): DiffFixture {
  const before = largeFixture(count);
  const after = nextVersion(before, 11, every ? 1 : 0.1);
  return {
    before,
    after,
    diff: diffOf(before, after),
    sql: { before: renderFixtureSql(before), after: renderFixtureSql(after) },
  };
}
