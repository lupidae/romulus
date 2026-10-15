// An edit's effect on the model, before the compile confirms it: the graph
// shows edits at once (optimistic) and settles when the compiled model has
// them. Best effort by design: the compile is what the graph ends up
// showing; this only has to look right in the meantime.

import {
  type Column,
  type Constraint,
  type Entity,
  qualifiedName,
  type RelationRef,
  type Schema,
} from "../model";
import type { GraphEdit } from "./edits";
import { columnDependents, findColumn, findEntity, tableDependents } from "./inspect";
import { quoteIdent, qualified } from "./quote";
import { canonicalType } from "./types";

const same = (a: RelationRef, b: RelationRef) => a.schema === b.schema && a.name === b.name;

function mapEntities(schema: Schema, f: (e: Entity) => Entity | null): Schema {
  const entities: Entity[] = [];
  for (const e of schema.entities ?? []) {
    const next = f(e);
    if (next) entities.push(next);
  }
  return { ...schema, entities };
}

function mapColumn(
  schema: Schema,
  table: RelationRef,
  column: string,
  f: (c: Column) => Column,
): Schema {
  return mapEntities(schema, (e) =>
    same(e, table)
      ? { ...e, columns: (e.columns ?? []).map((c) => (c.name === column ? f(c) : c)) }
      : e,
  );
}

const ACTIONS = { a: null, r: "RESTRICT", c: "CASCADE", n: "SET NULL", d: "SET DEFAULT" } as const;

/** The model as it will be once the edit compiles. */
export function applyEdit(schema: Schema, edit: GraphEdit): Schema {
  switch (edit.kind) {
    case "renameTable": {
      const to = { schema: edit.table.schema, name: edit.to };
      return {
        ...mapEntities(schema, (e) => ({
          ...(same(e, edit.table) ? { ...e, name: edit.to } : e),
          constraints: (e.constraints ?? []).map((c) =>
            c.ref_schema === edit.table.schema && c.ref_table === edit.table.name
              ? { ...c, ref_table: edit.to }
              : c,
          ),
          depends_on: (e.depends_on ?? []).map((d) => (same(d, edit.table) ? to : d)),
        })),
        partitions: (schema.partitions ?? []).map((p) =>
          p.parent_schema === edit.table.schema && p.parent === edit.table.name
            ? { ...p, parent: edit.to }
            : p,
        ),
      };
    }
    case "renameColumn": {
      const rename = (list: string[] | undefined) =>
        (list ?? []).map((c) => (c === edit.column ? edit.to : c));
      return mapEntities(schema, (e) => {
        const own = same(e, edit.table);
        return {
          ...e,
          columns: own
            ? (e.columns ?? []).map((c) => (c.name === edit.column ? { ...c, name: edit.to } : c))
            : e.columns,
          constraints: (e.constraints ?? []).map((c) => {
            let next: Constraint = own ? { ...c, columns: rename(c.columns) } : c;
            if (c.ref_schema === edit.table.schema && c.ref_table === edit.table.name)
              next = { ...next, ref_columns: rename(c.ref_columns) };
            return next;
          }),
        };
      });
    }
    case "addTable": {
      const entity: Entity = {
        schema: edit.table.schema,
        name: edit.table.name,
        kind: "r",
        comment: null,
        partition_key: null,
        view_definition: null,
        columns: [
          {
            name: "id",
            type: "bigint",
            type_schema: "pg_catalog",
            type_name: "int8",
            type_kind: "b",
            not_null: true,
            default: null,
            identity: "a",
            generated: null,
            comment: null,
          },
        ],
        constraints: [
          {
            name: `${edit.table.name}_pkey`,
            kind: "p",
            columns: ["id"],
            ref_schema: null,
            ref_table: null,
            on_update: null,
            on_delete: null,
            definition: "PRIMARY KEY (id)",
            comment: null,
          },
        ],
      };
      return { ...schema, entities: [...(schema.entities ?? []), entity] };
    }
    case "addColumn": {
      const c = edit.column;
      const column: Column = {
        name: c.name,
        type: c.type.display,
        type_schema: c.type.typeSchema,
        type_name: c.type.typeName,
        type_kind: c.type.typeKind,
        is_array: c.type.isArray,
        not_null: c.notNull,
        default: c.default,
        identity: null,
        generated: null,
        comment: null,
      };
      return mapEntities(schema, (e) =>
        same(e, edit.table) ? { ...e, columns: [...(e.columns ?? []), column] } : e,
      );
    }
    case "dropTable": {
      const gone = new Set([qualifiedName(edit.table)]);
      if (edit.cascade)
        for (const d of tableDependents(schema, edit.table).all)
          if (d.kind === "view" || d.kind === "materialized view") gone.add(qualifiedName(d.table));
      return {
        ...mapEntities(schema, (e) =>
          gone.has(qualifiedName(e))
            ? null
            : {
                ...e,
                constraints: (e.constraints ?? []).filter(
                  (c) => !(c.kind === "f" && gone.has(`${c.ref_schema}.${c.ref_table}`)),
                ),
              },
        ),
        partitions: (schema.partitions ?? []).filter(
          (p) => !gone.has(`${p.parent_schema}.${p.parent}`),
        ),
      };
    }
    case "dropColumn": {
      const dependents = columnDependents(schema, edit.table, edit.column).all;
      const views = new Set(
        dependents
          .filter((d) => d.kind === "view" || d.kind === "materialized view")
          .map((d) => qualifiedName(d.table)),
      );
      const foreign = new Set(
        dependents
          .filter((d) => d.kind === "foreign key")
          .map((d) => `${qualifiedName(d.table)}\u0000${d.name}`),
      );
      const policies = new Set(dependents.filter((d) => d.kind === "policy").map((d) => d.name));
      const indexes = new Set(dependents.filter((d) => d.kind === "index").map((d) => d.name));
      return mapEntities(schema, (e) => {
        if (views.has(qualifiedName(e))) return null;
        const own = same(e, edit.table);
        return {
          ...e,
          columns: own ? (e.columns ?? []).filter((c) => c.name !== edit.column) : e.columns,
          constraints: (e.constraints ?? []).filter(
            (c) =>
              !foreign.has(`${qualifiedName(e)}\u0000${c.name}`) &&
              !(own && (c.columns ?? []).includes(edit.column)),
          ),
          indexes: own ? (e.indexes ?? []).filter((i) => !indexes.has(i.name)) : e.indexes,
          policies: own ? (e.policies ?? []).filter((p) => !policies.has(p.name)) : e.policies,
        };
      });
    }
    case "setType":
      return mapColumn(schema, edit.table, edit.column, (c) => ({
        ...c,
        type: edit.type.display,
        type_schema: edit.type.typeSchema,
        type_name: edit.type.typeName,
        type_kind: edit.type.typeKind,
        is_array: edit.type.isArray,
      }));
    case "setNotNull":
      return mapColumn(schema, edit.table, edit.column, (c) => ({ ...c, not_null: edit.notNull }));
    case "setDefault":
      return mapColumn(schema, edit.table, edit.column, (c) => ({ ...c, default: edit.default }));
    case "setComment":
      if (edit.column === undefined)
        return mapEntities(schema, (e) =>
          same(e, edit.table) ? { ...e, comment: edit.comment } : e,
        );
      return mapColumn(schema, edit.table, edit.column, (c) => ({ ...c, comment: edit.comment }));
    case "addForeignKey": {
      const action = ACTIONS[edit.onDelete];
      const constraint: Constraint = {
        name: edit.name,
        kind: "f",
        columns: [edit.column],
        ref_schema: edit.references.schema,
        ref_table: edit.references.name,
        ref_columns: [edit.refColumn],
        on_update: "a",
        on_delete: edit.onDelete,
        deferrable: false,
        definition: `FOREIGN KEY (${quoteIdent(edit.column)}) REFERENCES ${qualified(edit.references)}(${quoteIdent(edit.refColumn)})${action ? ` ON DELETE ${action}` : ""}`,
        comment: null,
      };
      return mapEntities(schema, (e) =>
        same(e, edit.table) ? { ...e, constraints: [...(e.constraints ?? []), constraint] } : e,
      );
    }
  }
}

/**
 * Whether a compiled model already shows the edit: `true` yes, `false` not
 * yet, `null` when the model cannot tell (an expression Postgres reprints).
 */
export function reflects(schema: Schema, edit: GraphEdit): boolean | null {
  const entity = findEntity(schema, edit.table);
  switch (edit.kind) {
    case "renameTable":
      return !entity && !!findEntity(schema, { schema: edit.table.schema, name: edit.to });
    case "renameColumn":
      return !findColumn(entity, edit.column) && !!findColumn(entity, edit.to);
    case "addTable":
      return !!entity;
    case "addColumn":
      return !!findColumn(entity, edit.column.name);
    case "dropTable":
      return !entity;
    case "dropColumn":
      return !!entity && !findColumn(entity, edit.column);
    case "setType": {
      const column = findColumn(entity, edit.column);
      if (!column) return false;
      if (edit.type.typeKind !== "b")
        return (
          column.type_schema === edit.type.typeSchema && column.type_name === edit.type.typeName
        );
      return canonicalType(column.type) === canonicalType(edit.type.display) ? true : null;
    }
    case "setNotNull":
      return findColumn(entity, edit.column)?.not_null === edit.notNull;
    case "setDefault": {
      const column = findColumn(entity, edit.column);
      if (!column) return false;
      if (edit.default === null) return column.default === null;
      return column.default === null ? false : null;
    }
    case "setComment": {
      const comment =
        edit.column === undefined ? entity?.comment : findColumn(entity, edit.column)?.comment;
      return (comment ?? null) === edit.comment;
    }
    case "addForeignKey":
      return (entity?.constraints ?? []).some((c) => c.kind === "f" && c.name === edit.name);
  }
}

/** An element of the graph an edit is about: a table, or one of its columns. */
export interface EditMark {
  table: string;
  column?: string;
}

/** Where the graph shows an edit while it is pending (on the model after it). */
export function pendingMark(edit: GraphEdit): EditMark {
  const key = qualifiedName(edit.table);
  switch (edit.kind) {
    case "renameTable":
      return { table: qualifiedName({ schema: edit.table.schema, name: edit.to }) };
    case "addTable":
    case "dropTable":
      return { table: key };
    case "renameColumn":
      return { table: key, column: edit.to };
    case "addColumn":
      return { table: key, column: edit.column.name };
    case "setComment":
      return edit.column === undefined ? { table: key } : { table: key, column: edit.column };
    case "dropColumn":
    case "setType":
    case "setNotNull":
    case "setDefault":
    case "addForeignKey":
      return { table: key, column: edit.column };
  }
}

/** Where the graph shows an edit that failed (on the model before it). */
export function failedMark(edit: GraphEdit): EditMark {
  const key = qualifiedName(edit.table);
  switch (edit.kind) {
    case "renameTable":
    case "addTable":
    case "dropTable":
      return { table: key };
    case "addColumn":
      return { table: key };
    case "setComment":
      return edit.column === undefined ? { table: key } : { table: key, column: edit.column };
    case "renameColumn":
    case "dropColumn":
    case "setType":
    case "setNotNull":
    case "setDefault":
    case "addForeignKey":
      return { table: key, column: edit.column };
  }
}

/** The presentation changes that go with an edit, in the same transaction. */
export interface LayoutChange {
  /** Layout and group entries to move: `[old key, new key]`. */
  rename?: [string, string];
  /** A position for a table that has none yet. */
  place?: { key: string; x: number; y: number };
  /** A table that leaves the layout and its groups. */
  remove?: string;
}

export function layoutChange(edit: GraphEdit, position?: { x: number; y: number }): LayoutChange {
  switch (edit.kind) {
    case "renameTable":
      return {
        rename: [
          qualifiedName(edit.table),
          qualifiedName({ schema: edit.table.schema, name: edit.to }),
        ],
      };
    case "addTable":
      return position ? { place: { key: qualifiedName(edit.table), ...position } } : {};
    case "dropTable":
      return { remove: qualifiedName(edit.table) };
    default:
      return {};
  }
}
