// What the graph needs to know about the model before it writes an edit:
// what depends on a table or column (dropping asks, and cascades when it
// must), whether a foreign key can be drawn, the name Postgres would give
// it, and names already taken.

import {
  type Column,
  type Constraint,
  type Entity,
  foreignKeys,
  isUniqueSet,
  isView,
  qualifiedName,
  type RelationRef,
  type Schema,
} from "../model";
import { byteLength, MAX_IDENTIFIER_BYTES } from "./quote";
import { baseType, comparable } from "./types";

export function findEntity(schema: Schema, ref: RelationRef): Entity | undefined {
  return (schema.entities ?? []).find((e) => e.schema === ref.schema && e.name === ref.name);
}

export function findColumn(entity: Entity | undefined, name: string): Column | undefined {
  return entity?.columns?.find((c) => c.name === name);
}

const sameRef = (a: RelationRef, b: RelationRef) => a.schema === b.schema && a.name === b.name;

/** One thing that goes when something is dropped. */
export interface Dependent {
  kind: "foreign key" | "view" | "materialized view" | "policy" | "index" | "constraint";
  /** The relation it belongs to (a view is its own). */
  table: RelationRef;
  name: string;
}

export interface Dependents {
  /** Everything that goes with it. */
  all: Dependent[];
  /** Some of them need CASCADE (Postgres refuses the plain DROP). */
  cascade: boolean;
}

/** Views (and materialized views) that read a relation, directly or through another view. */
function readers(schema: Schema, table: RelationRef, seen = new Set<string>()): Entity[] {
  const out: Entity[] = [];
  for (const e of schema.entities ?? []) {
    const key = qualifiedName(e);
    if (seen.has(key) || !isView(e.kind)) continue;
    if ((e.depends_on ?? []).some((d) => sameRef(d, table))) {
      seen.add(key);
      out.push(e, ...readers(schema, e, seen));
    }
  }
  return out;
}

const viewDependent = (e: Entity): Dependent => ({
  kind: e.kind === "m" ? "materialized view" : "view",
  table: { schema: e.schema, name: e.name },
  name: e.name,
});

/** What dropping a table (or view) takes with it. */
export function tableDependents(schema: Schema, table: RelationRef): Dependents {
  const all: Dependent[] = [];
  for (const e of schema.entities ?? []) {
    if (sameRef(e, table)) continue;
    for (const fk of foreignKeys(e)) {
      if (fk.ref_schema === table.schema && fk.ref_table === table.name)
        all.push({ kind: "foreign key", table: { schema: e.schema, name: e.name }, name: fk.name });
    }
  }
  all.push(...readers(schema, table).map(viewDependent));
  const cascade = all.length > 0;
  const own = findEntity(schema, table);
  for (const p of own?.policies ?? []) all.push({ kind: "policy", table, name: p.name });
  return { all, cascade };
}

/**
 * Whether an SQL expression (a view's definition, a policy's) mentions a
 * column: its name as a whole identifier, bare or quoted.
 */
export function mentions(expression: string | null | undefined, column: string): boolean {
  if (!expression) return false;
  const escaped = column.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const quoted = `"${column.replaceAll('"', '""')}"`;
  if (expression.includes(quoted)) return true;
  if (/^[a-z_][a-z0-9_$]*$/.test(column))
    return new RegExp(
      `(^|[^A-Za-z0-9_$".])(?:[A-Za-z0-9_$"]+\\.)?${escaped}(?![A-Za-z0-9_$"])`,
    ).test(expression);
  return false;
}

/** What dropping a column takes with it. */
export function columnDependents(schema: Schema, table: RelationRef, column: string): Dependents {
  const all: Dependent[] = [];
  let cascade = false;
  const own = findEntity(schema, table);
  // Foreign keys elsewhere that reference it: CASCADE drops them.
  for (const e of schema.entities ?? []) {
    for (const fk of foreignKeys(e)) {
      if (
        fk.ref_schema === table.schema &&
        fk.ref_table === table.name &&
        (fk.ref_columns ?? []).includes(column) &&
        !(sameRef(e, table) && (fk.columns ?? []).includes(column))
      ) {
        all.push({ kind: "foreign key", table: { schema: e.schema, name: e.name }, name: fk.name });
        cascade = true;
      }
    }
  }
  // Views reading the table that use it (by name: the model has no column
  // dependencies), and policies whose expressions use it.
  for (const view of readers(schema, table)) {
    if (mentions(view.view_definition, column)) {
      all.push(viewDependent(view));
      cascade = true;
    }
  }
  for (const p of own?.policies ?? []) {
    if (mentions(p.using, column) || mentions(p.check, column)) {
      all.push({ kind: "policy", table, name: p.name });
      cascade = true;
    }
  }
  // Its own constraints and indexes go without CASCADE.
  for (const c of own?.constraints ?? []) {
    const involved =
      (c.columns ?? []).includes(column) || (c.kind === "c" && mentions(c.definition, column));
    if (involved)
      all.push({ kind: c.kind === "f" ? "foreign key" : "constraint", table, name: c.name });
  }
  const constraintNames = new Set((own?.constraints ?? []).map((c) => c.name));
  for (const index of own?.indexes ?? []) {
    if (
      !constraintNames.has(index.name) &&
      mentions(index.definition.replace(/^.*?\bON\b/i, ""), column)
    )
      all.push({ kind: "index", table, name: index.name });
  }
  return { all, cascade };
}

/** The columns of a `CREATE INDEX … (a, b)` definition, unquoted; null for expressions. */
export function indexColumns(definition: string): string[] | null {
  const open = definition.search(/\(/);
  if (open < 0) return null;
  let depth = 0;
  let close = -1;
  for (let i = open; i < definition.length; i++) {
    if (definition[i] === "(") depth++;
    else if (definition[i] === ")" && --depth === 0) {
      close = i;
      break;
    }
  }
  if (close < 0 || /\bWHERE\b/i.test(definition.slice(close))) return null;
  const parts = definition.slice(open + 1, close).split(",");
  const out: string[] = [];
  for (const raw of parts) {
    const part = raw.trim();
    const quoted = /^"((?:[^"]|"")+)"$/.exec(part);
    if (quoted) out.push(quoted[1]!.replaceAll('""', '"'));
    else if (/^[a-z_][a-z0-9_$]*$/.test(part)) out.push(part);
    else return null;
  }
  return out;
}

/** At most one row per value: a PK, a UNIQUE constraint, or a full unique index on just it. */
export function isUniqueColumn(entity: Entity, column: string): boolean {
  if (isUniqueSet(entity, [column])) return true;
  return (entity.indexes ?? []).some((i) => {
    if (!i.unique || i.partial) return false;
    const columns = indexColumns(i.definition);
    return columns?.length === 1 && columns[0] === column;
  });
}

export type ForeignKeyCheck = { ok: true } | { ok: false; reason: string; fix?: "unique" };

/** Whether `from.column` may reference `to.refColumn`, and why not. */
export function checkForeignKey(
  schema: Schema,
  from: RelationRef,
  column: string,
  to: RelationRef,
  refColumn: string,
): ForeignKeyCheck {
  const source = findEntity(schema, from);
  const target = findEntity(schema, to);
  if (!source || !target) return { ok: false, reason: "That table is not in the schema any more." };
  if (isView(source.kind) || isView(target.kind))
    return { ok: false, reason: "Foreign keys link tables, not views." };
  if (source.kind === "f" || target.kind === "f")
    return { ok: false, reason: "Foreign tables cannot have foreign keys." };
  const a = findColumn(source, column);
  const b = findColumn(target, refColumn);
  if (!a || !b) return { ok: false, reason: "That column is not in the schema any more." };
  if (sameRef(from, to) && column === refColumn)
    return { ok: false, reason: "A column cannot reference itself." };
  const existing = foreignKeys(source).find(
    (fk) =>
      fk.columns?.length === 1 &&
      fk.columns[0] === column &&
      fk.ref_schema === to.schema &&
      fk.ref_table === to.name &&
      fk.ref_columns?.[0] === refColumn,
  );
  if (existing) return { ok: false, reason: `${existing.name} already links these columns.` };
  if (!isUniqueColumn(target, refColumn))
    return {
      ok: false,
      reason: `${to.name}.${refColumn} is not unique: a foreign key needs a primary key or unique column to point at.`,
      fix: "unique",
    };
  const fromType = baseType(a, schema);
  const toType = baseType(b, schema);
  if (!comparable(fromType, toType))
    return {
      ok: false,
      reason: `${from.name}.${column} is ${a.type} and ${to.name}.${refColumn} is ${b.type}: Postgres cannot compare them.`,
    };
  return { ok: true };
}

/** The first `bytes` bytes of a name, cut on a character boundary (`pg_mbcliplen`). */
function clip(name: string, bytes: number): string {
  let out = "";
  let used = 0;
  for (const ch of name) {
    const size = byteLength(ch);
    if (used + size > bytes) break;
    out += ch;
    used += size;
  }
  return out;
}

/**
 * Postgres's `makeObjectName`: `name1_name2_label`, the longer of the two
 * names shortened first so the whole fits in 63 bytes.
 */
export function makeObjectName(name1: string, name2: string | null, label: string): string {
  const overhead = (name2 === null ? 0 : 1) + byteLength(label) + 1;
  const available = MAX_IDENTIFIER_BYTES - overhead;
  let a = byteLength(name1);
  let b = name2 === null ? 0 : byteLength(name2);
  while (a + b > available) {
    if (a > b) a--;
    else b--;
  }
  const first = clip(name1, a);
  return name2 === null ? `${first}_${label}` : `${first}_${clip(name2, b)}_${label}`;
}

/** Every constraint name in a schema: Postgres picks new ones that none of them has. */
function constraintNames(schema: Schema, namespace: string): Set<string> {
  const out = new Set<string>();
  for (const e of schema.entities ?? [])
    if (e.schema === namespace) for (const c of e.constraints ?? []) out.add(c.name);
  return out;
}

/**
 * The name Postgres gives a foreign key it names itself
 * (`ChooseConstraintName`): `table_column_fkey`, then `…_fkey1`, `…_fkey2`
 * while the schema already has a constraint of that name.
 */
export function foreignKeyName(schema: Schema, table: RelationRef, column: string): string {
  const taken = constraintNames(schema, table.schema);
  let label = "fkey";
  for (let pass = 1; ; pass++) {
    const name = makeObjectName(table.name, column, label);
    if (!taken.has(name)) return name;
    label = `fkey${pass}`;
  }
}

/** Why a relation cannot be named this in its schema, or null. */
export function relationNameTaken(schema: Schema, namespace: string, name: string): string | null {
  const clash =
    (schema.entities ?? []).find((e) => e.schema === namespace && e.name === name) ??
    (schema.partitions ?? []).find((p) => p.child_schema === namespace && p.child === name);
  if (clash) return `${name} already exists in ${namespace}.`;
  const type = [
    ...(schema.enums ?? []),
    ...(schema.domains ?? []),
    ...(schema.composites ?? []),
  ].find((t) => t.schema === namespace && t.name === name);
  if (type) return `A type named ${name} already exists in ${namespace}.`;
  return null;
}

/** Why a column cannot be named this in its table, or null. */
export function columnNameTaken(entity: Entity, name: string): string | null {
  return (entity.columns ?? []).some((c) => c.name === name)
    ? `${entity.name} already has a column ${name}.`
    : null;
}

/** A name for a new table that the schema does not have yet: `new_table`, `new_table_2`… */
export function freshTableName(schema: Schema, namespace: string, base = "new_table"): string {
  for (let n = 1; ; n++) {
    const name = n === 1 ? base : `${base}_${n}`;
    if (relationNameTaken(schema, namespace, name) === null) return name;
  }
}

/** The constraint a primary-key or unique column is covered by, for labels. */
export function uniqueConstraint(entity: Entity, column: string): Constraint | undefined {
  return (entity.constraints ?? []).find(
    (c) => (c.kind === "p" || c.kind === "u") && c.columns?.length === 1 && c.columns[0] === column,
  );
}

/** Columns of `entity` a foreign key may point at: primary key first, then unique ones. */
export function referenceableColumns(entity: Entity): Column[] {
  const columns = entity.columns ?? [];
  const pk = columns.filter((c) =>
    (entity.constraints ?? []).some(
      (k) => k.kind === "p" && k.columns?.length === 1 && k.columns[0] === c.name,
    ),
  );
  const unique = columns.filter((c) => !pk.includes(c) && isUniqueColumn(entity, c.name));
  return [...pk, ...unique];
}
