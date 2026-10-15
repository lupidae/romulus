// From remus models to what the graph draws: one node per entity (partitions
// collapsed under the entity at the top of their tree), one edge per foreign
// key, and in diff mode the union of both sides with each part's status.
// Pure and synchronous, so it is tested without a DOM.

import {
  type ChangeStatus,
  changeOf,
  type DiffStatus,
  type EntityStatus,
  entityStatus,
  type SchemaDiffInput,
} from "../diff";
import {
  type Column,
  type Constraint,
  type Entity,
  type EntityKindCode,
  foreignKeys,
  isMultiSchema,
  qualifiedName,
  type Schema,
} from "../model";
import { type Cardinality, cardinality } from "./cardinality";
import { shortType } from "./geometry";

export type TableKind = "table" | "partitioned" | "view" | "materialized" | "foreign";

const KINDS: Record<EntityKindCode, TableKind> = {
  r: "table",
  p: "partitioned",
  v: "view",
  m: "materialized",
  f: "foreign",
};

export interface UserType {
  kind: "enum" | "domain" | "composite";
  schema: string;
  name: string;
}

export interface ColumnModel {
  name: string;
  /** As `format_type` prints it. */
  type: string;
  /** Shorter, for the node: `timestamptz`, `varchar(80)`, a type of the table's own schema unqualified. */
  display: string;
  /** Set when the type is an enum, domain or composite of the model: drawn as a link. */
  userType?: UserType;
  pk: boolean;
  fk: boolean;
  /** A single-column UNIQUE constraint. */
  unique: boolean;
  notNull: boolean;
  identity: "always" | "by default" | null;
  generated: "stored" | "virtual" | null;
  default: string | null;
  comment: string | null;
  status?: ChangeStatus;
  /** Diff mode, changed columns: the column as it was. */
  before?: Column;
}

export interface PartitionModel {
  key: string;
  schema: string;
  name: string;
  /** Qualified name of its immediate parent (the entity, or a sub-partitioned partition). */
  parent: string;
  bound: string | null;
  /** 0 under the entity, 1 under a partition of it… */
  depth: number;
  status?: ChangeStatus;
}

export interface TableModel {
  key: string;
  schema: string;
  name: string;
  kind: TableKind;
  comment: string | null;
  columns: ColumnModel[];
  partitionKey: string | null;
  partitions: PartitionModel[];
  rls: boolean;
  status?: ChangeStatus;
  /** The entity drawn: the new one, or the old one when removed. */
  entity: Entity;
}

export interface EdgeModel {
  id: string;
  kind: "fk" | "reads";
  /** The referencing table (or the view that reads). */
  source: string;
  /** The referenced table (or the relation read). */
  target: string;
  /** First column of the FK on each side: where the edge attaches. */
  sourceColumn?: string;
  targetColumn?: string;
  name: string;
  columns: string[];
  refColumns: string[];
  cardinality?: Cardinality;
  onDelete: Constraint["on_delete"];
  onUpdate: Constraint["on_update"];
  status?: ChangeStatus;
}

export interface TypeModel {
  kind: UserType["kind"];
  schema: string;
  name: string;
  /** Enum labels, a domain's `base NOT NULL DEFAULT … CHECK …`, or composite attributes. */
  lines: string[];
  comment: string | null;
  status?: ChangeStatus;
}

export interface DiffCounts {
  added: number;
  removed: number;
  changed: number;
}

export interface GraphModel {
  tables: TableModel[];
  byKey: Map<string, TableModel>;
  edges: EdgeModel[];
  /** Qualified names of partitions → the entity they are drawn under. */
  partitionOwner: Map<string, string>;
  types: Map<string, TypeModel>;
  multiSchema: boolean;
  /** Diff mode only: tables by status. */
  counts?: DiffCounts;
}

const TYPE_KINDS = { e: "enum", d: "domain", c: "composite" } as const;

function compareRefs(a: { schema: string; name: string }, b: { schema: string; name: string }) {
  return a.schema < b.schema
    ? -1
    : a.schema > b.schema
      ? 1
      : a.name < b.name
        ? -1
        : a.name > b.name
          ? 1
          : 0;
}

function columnModel(
  entity: Entity,
  column: Column,
  types: Map<string, TypeModel>,
  status?: DiffStatus,
  before?: Column,
): ColumnModel {
  const constraints = entity.constraints ?? [];
  const typeKind = TYPE_KINDS[column.type_kind as keyof typeof TYPE_KINDS];
  const typeKey = qualifiedName({ schema: column.type_schema, name: column.type_name });
  const prefix = `${column.type_schema}.`;
  const local = column.type_schema === entity.schema || column.type_schema === "public";
  const display = shortType(
    typeKind && local && column.type.startsWith(prefix)
      ? column.type.slice(prefix.length)
      : column.type,
  );
  return {
    name: column.name,
    type: column.type,
    display,
    userType:
      typeKind && types.has(typeKey)
        ? { kind: typeKind, schema: column.type_schema, name: column.type_name }
        : undefined,
    pk: constraints.some((c) => c.kind === "p" && c.columns?.includes(column.name)),
    fk: constraints.some((c) => c.kind === "f" && c.columns?.includes(column.name)),
    unique: constraints.some(
      (c) => c.kind === "u" && c.columns?.length === 1 && c.columns[0] === column.name,
    ),
    notNull: column.not_null === true,
    identity: column.identity === "a" ? "always" : column.identity === "d" ? "by default" : null,
    generated: column.generated === "s" ? "stored" : column.generated === "v" ? "virtual" : null,
    default: column.default,
    comment: column.comment,
    status: changeOf(status),
    before: status === "changed" ? before : undefined,
  };
}

/**
 * Columns of both sides in table order: the new columns as they are, each
 * removed one right after the old column it followed.
 */
function unionColumns(
  current: Entity,
  base: Entity | undefined,
  status: EntityStatus | undefined,
  types: Map<string, TypeModel>,
): ColumnModel[] {
  const oldColumns = base?.columns ?? [];
  const columns = (current.columns ?? []).map((c) =>
    columnModel(
      current,
      c,
      types,
      status?.columns[c.name],
      oldColumns.find((o) => o.name === c.name),
    ),
  );
  if (!base || base === current) return columns;
  const present = new Set(columns.map((c) => c.name));
  oldColumns.forEach((old, i) => {
    if (present.has(old.name)) return;
    const removed = columnModel(base, old, types, status?.columns[old.name] ?? "removed");
    let at = 0;
    for (let j = i - 1; j >= 0; j--) {
      const index = columns.findIndex((c) => c.name === oldColumns[j]!.name);
      if (index >= 0) {
        at = index + 1;
        break;
      }
    }
    columns.splice(at, 0, removed);
    present.add(old.name);
  });
  return columns;
}

function typeModels(schemas: Schema[], status: SchemaDiffInput["status"] | undefined) {
  const types = new Map<string, TypeModel>();
  // The first schema (the new side) wins; the base only adds what was removed.
  for (const schema of schemas) {
    for (const e of schema.enums ?? []) {
      const key = qualifiedName(e);
      if (types.has(key)) continue;
      types.set(key, {
        kind: "enum",
        schema: e.schema,
        name: e.name,
        lines: e.labels ?? [],
        comment: e.comment,
        status: changeOf(status?.enums?.[e.schema]?.[e.name]),
      });
    }
    for (const d of schema.domains ?? []) {
      const key = qualifiedName(d);
      if (types.has(key)) continue;
      const head = [
        d.base_type,
        d.not_null ? "NOT NULL" : "",
        d.default ? `DEFAULT ${d.default}` : "",
      ];
      types.set(key, {
        kind: "domain",
        schema: d.schema,
        name: d.name,
        lines: [head.filter(Boolean).join(" "), ...(d.constraints ?? [])],
        comment: d.comment,
        status: changeOf(status?.domains?.[d.schema]?.[d.name]),
      });
    }
    for (const c of schema.composites ?? []) {
      const key = qualifiedName(c);
      if (types.has(key)) continue;
      types.set(key, {
        kind: "composite",
        schema: c.schema,
        name: c.name,
        lines: (c.attributes ?? []).map((a) => `${a.name} ${a.type}`),
        comment: c.comment,
        status: changeOf(status?.composites?.[c.schema]?.[c.name]),
      });
    }
  }
  return types;
}

/**
 * Partitions of every level, each under the entity its chain of parents leads
 * to (remus lists no partition among the entities). Orphans whose chain
 * reaches no entity are left out: they have no box to sit in.
 */
function partitionTrees(
  schemas: Schema[],
  entities: Map<string, Entity>,
  status: SchemaDiffInput["status"] | undefined,
) {
  const parentOf = new Map<
    string,
    { parent: string; bound: string | null; schema: string; name: string }
  >();
  for (const schema of schemas) {
    for (const p of schema.partitions ?? []) {
      const key = qualifiedName({ schema: p.child_schema, name: p.child });
      if (parentOf.has(key)) continue;
      parentOf.set(key, {
        parent: qualifiedName({ schema: p.parent_schema, name: p.parent }),
        bound: p.bound,
        schema: p.child_schema,
        name: p.child,
      });
    }
  }
  const owner = new Map<string, string>();
  const byOwner = new Map<string, PartitionModel[]>();
  for (const [key, info] of parentOf) {
    let depth = 0;
    let parent = info.parent;
    const seen = new Set([key]);
    while (!entities.has(parent)) {
      const next = parentOf.get(parent);
      if (!next || seen.has(parent)) break;
      seen.add(parent);
      parent = next.parent;
      depth++;
    }
    const root = entities.get(parent);
    if (!root) continue;
    owner.set(key, parent);
    const s = status?.entities[root.schema]?.[root.name]?.partitions[info.schema]?.[info.name];
    const list = byOwner.get(parent) ?? [];
    list.push({
      key,
      schema: info.schema,
      name: info.name,
      parent: info.parent,
      bound: info.bound,
      depth,
      status: changeOf(s),
    });
    byOwner.set(parent, list);
  }
  // Parents first, then their partitions, each level by name: a readable tree.
  for (const [ownerKey, list] of byOwner) {
    const children = new Map<string, PartitionModel[]>();
    for (const p of list) children.set(p.parent, [...(children.get(p.parent) ?? []), p]);
    const ordered: PartitionModel[] = [];
    const visit = (parent: string) => {
      for (const p of (children.get(parent) ?? []).sort(compareRefs)) {
        ordered.push(p);
        visit(p.key);
      }
    };
    visit(ownerKey);
    byOwner.set(ownerKey, ordered);
  }
  return { owner, byOwner };
}

export interface BuildGraphInput {
  schema: Schema;
  diff?: SchemaDiffInput;
}

/** Everything the graph pane draws, from the model (and, in diff mode, the old one). */
export function buildGraph({ schema, diff }: BuildGraphInput): GraphModel {
  const status = diff?.status;
  const sides = diff ? [schema, diff.base] : [schema];
  const types = typeModels(sides, status);

  const current = new Map((schema.entities ?? []).map((e) => [qualifiedName(e), e]));
  const previous = new Map((diff?.base.entities ?? []).map((e) => [qualifiedName(e), e]));
  const entities: Entity[] = [...current.values()];
  for (const [key, e] of previous) if (!current.has(key)) entities.push(e);
  entities.sort(compareRefs);
  const { owner, byOwner } = partitionTrees(
    sides,
    new Map(entities.map((e) => [qualifiedName(e), e])),
    status,
  );

  const tables: TableModel[] = entities.map((entity) => {
    const key = qualifiedName(entity);
    const s = status ? entityStatus(status, entity) : undefined;
    const base = previous.get(key);
    const isCurrent = current.get(key) === entity;
    return {
      key,
      schema: entity.schema,
      name: entity.name,
      kind: KINDS[entity.kind],
      comment: entity.comment,
      columns: unionColumns(entity, isCurrent ? base : undefined, s, types),
      partitionKey: entity.partition_key,
      partitions: byOwner.get(key) ?? [],
      rls: entity.rls === true,
      status: diff
        ? (changeOf(s?.status) ?? (isCurrent ? (base ? undefined : "added") : "removed"))
        : undefined,
      entity,
    };
  });
  const byKey = new Map(tables.map((t) => [t.key, t]));
  const resolve = (ref: string) => (byKey.has(ref) ? ref : owner.get(ref));

  const edges: EdgeModel[] = [];
  for (const table of tables) {
    const s = status ? entityStatus(status, table) : undefined;
    const base = previous.get(table.key);
    const fks: { fk: Constraint; from: Entity }[] = foreignKeys(table.entity).map((fk) => ({
      fk,
      from: table.entity,
    }));
    if (base && base !== table.entity) {
      for (const fk of foreignKeys(base)) {
        if (!fks.some((f) => f.fk.name === fk.name)) fks.push({ fk, from: base });
      }
    }
    for (const { fk, from } of fks) {
      if (!fk.ref_schema || !fk.ref_table) continue;
      const target = resolve(qualifiedName({ schema: fk.ref_schema, name: fk.ref_table }));
      if (!target) continue;
      const removedHere = from !== table.entity;
      edges.push({
        id: `fk:${table.key}:${fk.name}`,
        kind: "fk",
        source: table.key,
        target,
        sourceColumn: fk.columns?.[0],
        targetColumn: fk.ref_columns?.[0],
        name: fk.name,
        columns: fk.columns ?? [],
        refColumns: fk.ref_columns ?? [],
        cardinality: cardinality(from, fk),
        onDelete: fk.on_delete,
        onUpdate: fk.on_update,
        status: diff
          ? table.status === "added" || table.status === "removed"
            ? table.status
            : (changeOf(s?.constraints[fk.name]) ?? (removedHere ? "removed" : undefined))
          : undefined,
      });
    }
    if (table.kind === "view" || table.kind === "materialized") {
      for (const dep of table.entity.depends_on ?? []) {
        const target = resolve(qualifiedName(dep));
        if (!target || target === table.key) continue;
        edges.push({
          id: `reads:${table.key}:${target}`,
          kind: "reads",
          source: table.key,
          target,
          name: `reads ${target}`,
          columns: [],
          refColumns: [],
          onDelete: null,
          onUpdate: null,
          status: table.status === "added" || table.status === "removed" ? table.status : undefined,
        });
      }
    }
  }

  let counts: DiffCounts | undefined;
  if (diff) {
    counts = { added: 0, removed: 0, changed: 0 };
    for (const t of tables) if (t.status) counts[t.status]++;
  }

  return {
    tables,
    byKey,
    edges,
    partitionOwner: owner,
    types,
    multiSchema: isMultiSchema({ ...schema, entities }),
    counts,
  };
}
