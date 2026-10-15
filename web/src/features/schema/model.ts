// The remus schema model (`remus_core::Schema`), as its JSON spells it.
//
// Hand-written against remus-core 0.1.1 (crates/remus-core/src/model/mod.rs in
// lupidae/remus): the API stores and serves this JSON verbatim (versions.model,
// draft compiles), and remus-core has no JSON Schema to generate it from. Field
// names and the single-letter catalog codes are remus's on purpose, so a
// payload deserialises with no translation layer. Collections that remus
// defaults with `#[serde(default)]` are optional here for the same reason.

/** `pg_class.relkind`: table, partitioned table, view, materialized view, foreign table. */
export type EntityKindCode = "r" | "p" | "v" | "m" | "f";
/** `pg_constraint.contype`: primary key, unique, foreign key, check, exclusion. */
export type ConstraintKindCode = "p" | "u" | "f" | "c" | "x";
/** `pg_type.typtype` of a column's type: base, enum, domain, composite, range, multirange, pseudo. */
export type TypeKindCode = "b" | "e" | "d" | "c" | "r" | "m" | "p";
/** `pg_attribute.attidentity`: always, by default. */
export type IdentityCode = "a" | "d";
/** `pg_attribute.attgenerated`: stored, virtual (PostgreSQL 18+). */
export type GeneratedCode = "s" | "v";
/** `confupdtype` / `confdeltype`: no action, restrict, cascade, set null, set default. */
export type ReferentialActionCode = "a" | "r" | "c" | "n" | "d";
/** `pg_policy.polcmd`: all, select, insert, update, delete. */
export type PolicyCommandCode = "*" | "r" | "a" | "w" | "d";

export interface RelationRef {
  schema: string;
  name: string;
}

export interface Schema {
  remus_version: string;
  generated_at?: string | null;
  database: string;
  server_version: string;
  entities?: Entity[];
  enums?: EnumType[];
  domains?: DomainType[];
  composites?: CompositeType[];
  /** Partitions are not entities: one box per partitioned table, not forty. */
  partitions?: Partition[];
  extensions?: Extension[];
}

export interface Entity {
  schema: string;
  name: string;
  kind: EntityKindCode;
  comment: string | null;
  rls?: boolean;
  rls_forced?: boolean;
  unlogged?: boolean;
  partition_key: string | null;
  view_definition: string | null;
  /** Relations a view reads from. Empty for tables. */
  depends_on?: RelationRef[];
  columns?: Column[];
  constraints?: Constraint[];
  indexes?: Index[];
  policies?: Policy[];
}

export interface Column {
  name: string;
  /** As `format_type` prints it: `character varying(255)`, `order_status`, `text[]`. */
  type: string;
  type_schema: string;
  type_name: string;
  type_kind: TypeKindCode;
  is_array?: boolean;
  not_null?: boolean;
  /** The default, or the generation expression of a generated column. */
  default: string | null;
  identity: IdentityCode | null;
  generated: GeneratedCode | null;
  comment: string | null;
}

export interface Constraint {
  name: string;
  kind: ConstraintKindCode;
  /** Constrained columns in constraint order. Empty for CHECK constraints. */
  columns?: string[];
  ref_schema: string | null;
  ref_table: string | null;
  /** Referenced columns, positionally paired with `columns`. */
  ref_columns?: string[];
  on_update: ReferentialActionCode | null;
  on_delete: ReferentialActionCode | null;
  deferrable?: boolean;
  /** `pg_get_constraintdef`. */
  definition: string;
  comment: string | null;
}

export interface Index {
  name: string;
  unique?: boolean;
  partial?: boolean;
  /** `pg_get_indexdef`: a complete `CREATE INDEX` statement. */
  definition: string;
}

export interface Policy {
  name: string;
  command: PolicyCommandCode;
  permissive?: boolean;
  /** Empty means PUBLIC. */
  roles?: string[];
  using: string | null;
  check: string | null;
}

export interface EnumType {
  schema: string;
  name: string;
  labels?: string[];
  comment: string | null;
}

export interface DomainType {
  schema: string;
  name: string;
  base_type: string;
  not_null?: boolean;
  default: string | null;
  /** `pg_get_constraintdef` of each CHECK. */
  constraints?: string[];
  comment: string | null;
}

export interface CompositeType {
  schema: string;
  name: string;
  attributes?: { name: string; type: string }[];
  comment: string | null;
}

export interface Partition {
  parent_schema: string;
  parent: string;
  child_schema: string;
  child: string;
  /** `FOR VALUES ...` or `DEFAULT`. */
  bound: string | null;
  /** remus-core 0.2+: the partition's own `PARTITION BY` when it is partitioned in turn. */
  partition_key?: string | null;
}

export interface Extension {
  name: string;
  schema: string;
  version: string;
}

/**
 * The key of a relation in layouts, groups, selections and statement ranges:
 * remus's `Entity::qualified_name`, `schema.name`, unquoted. (A schema or
 * name holding a dot could collide with another; romulus-diff's status map
 * nests instead, and is read with `schema` and `name` separately.)
 */
export function qualifiedName(ref: RelationRef): string {
  return `${ref.schema}.${ref.name}`;
}

/** Diagram labels need the schema only when names could collide (remus's rule). */
export function isMultiSchema(schema: Schema): boolean {
  const entities = schema.entities ?? [];
  const first = entities[0]?.schema;
  return entities.some((e) => e.schema !== first);
}

export function isView(kind: EntityKindCode): boolean {
  return kind === "v" || kind === "m";
}

export function primaryKey(entity: Entity): Constraint | undefined {
  return entity.constraints?.find((c) => c.kind === "p");
}

export function foreignKeys(entity: Entity): Constraint[] {
  return (entity.constraints ?? []).filter((c) => c.kind === "f");
}

/**
 * True when `columns` are exactly covered by a PK or UNIQUE constraint: at most
 * one row holds a given combination (remus's `Entity::is_unique_set`).
 */
export function isUniqueSet(entity: Entity, columns: readonly string[]): boolean {
  return (entity.constraints ?? []).some(
    (c) =>
      (c.kind === "p" || c.kind === "u") &&
      (c.columns ?? []).length === columns.length &&
      columns.every((x) => (c.columns ?? []).includes(x)),
  );
}

const ACTIONS: Record<ReferentialActionCode, string> = {
  a: "no action",
  r: "restrict",
  c: "cascade",
  n: "set null",
  d: "set default",
};

export function referentialAction(code: ReferentialActionCode | null): string | null {
  return code && code !== "a" ? ACTIONS[code] : null;
}

export class SchemaParseError extends Error {
  override name = "SchemaParseError";
}

const ENTITY_KINDS = new Set<string>(["r", "p", "v", "m", "f"]);
const CONSTRAINT_KINDS = new Set<string>(["p", "u", "f", "c", "x"]);
const TYPE_KINDS = new Set<string>(["b", "e", "d", "c", "r", "m", "p"]);

type Json = Record<string, unknown>;
const isObject = (v: unknown): v is Json =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/**
 * Checks that JSON from the API (a version's `model`, typed `unknown` there)
 * or a file is a remus model the viewer can draw, and types it. Checks the
 * fields the viewer relies on; unknown extra fields are left alone, so a
 * newer remus adding fields still parses.
 */
export function parseSchema(value: unknown): Schema {
  const fail = (path: string, expected: string): never => {
    throw new SchemaParseError(`${path} should be ${expected}`);
  };
  const str = (o: Json, key: string, path: string) => {
    if (typeof o[key] !== "string") fail(`${path}.${key}`, "a string");
  };
  const list = (o: Json, key: string, path: string, each: (item: Json, path: string) => void) => {
    const v = o[key];
    if (v === undefined || v === null) return;
    if (!Array.isArray(v)) return fail(`${path}.${key}`, "a list");
    v.forEach((item: unknown, i) => {
      const at = `${path}.${key}[${i}]`;
      if (!isObject(item)) return fail(at, "an object");
      each(item, at);
    });
  };
  const code = (o: Json, key: string, path: string, codes: Set<string>) => {
    if (typeof o[key] !== "string" || !codes.has(o[key]))
      fail(`${path}.${key}`, `one of ${[...codes].join(", ")}`);
  };

  if (!isObject(value)) return fail("model", "an object");
  for (const key of ["remus_version", "database", "server_version"]) str(value, key, "model");
  list(value, "entities", "model", (e, at) => {
    str(e, "schema", at);
    str(e, "name", at);
    code(e, "kind", at, ENTITY_KINDS);
    list(e, "columns", at, (c, cat) => {
      str(c, "name", cat);
      str(c, "type", cat);
      str(c, "type_schema", cat);
      str(c, "type_name", cat);
      code(c, "type_kind", cat, TYPE_KINDS);
    });
    list(e, "constraints", at, (c, cat) => {
      str(c, "name", cat);
      str(c, "definition", cat);
      code(c, "kind", cat, CONSTRAINT_KINDS);
    });
    list(e, "indexes", at, (i, iat) => str(i, "name", iat));
    list(e, "policies", at, (p, pat) => str(p, "name", pat));
    list(e, "depends_on", at, (d, dat) => {
      str(d, "schema", dat);
      str(d, "name", dat);
    });
  });
  for (const key of ["enums", "domains", "composites"]) {
    list(value, key, "model", (t, at) => {
      str(t, "schema", at);
      str(t, "name", at);
    });
  }
  list(value, "partitions", "model", (p, at) => {
    for (const key of ["parent_schema", "parent", "child_schema", "child"]) str(p, key, at);
  });
  list(value, "extensions", "model", (x, at) => str(x, "name", at));
  return value as unknown as Schema;
}
