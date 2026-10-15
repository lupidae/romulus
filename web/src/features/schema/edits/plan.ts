// Edits as the graph asks for them, with the decisions the model makes for
// it: CASCADE when something depends on what goes, USING when Postgres would
// not cast by itself, the foreign key's name.

import type { Entity, ReferentialActionCode, RelationRef, Schema } from "../model";
import type { GraphEdit, Target } from "./edits";
import {
  checkForeignKey,
  columnDependents,
  type Dependents,
  findColumn,
  findEntity,
  foreignKeyName,
  type ForeignKeyCheck,
  tableDependents,
} from "./inspect";
import { baseType, canonicalType, needsUsing, type TypeChoice } from "./types";

export function targetOf(entity: Pick<Entity, "schema" | "name" | "kind">): Target {
  return { schema: entity.schema, name: entity.name, kind: entity.kind };
}

export function dropTable(
  schema: Schema,
  table: Target,
): { edit: GraphEdit; dependents: Dependents } {
  const dependents = tableDependents(schema, table);
  return { edit: { kind: "dropTable", table, cascade: dependents.cascade }, dependents };
}

export function dropColumn(
  schema: Schema,
  table: Target,
  column: string,
): { edit: GraphEdit; dependents: Dependents } {
  const dependents = columnDependents(schema, table, column);
  return { edit: { kind: "dropColumn", table, column, cascade: dependents.cascade }, dependents };
}

/** A type change, or null when the column already has that type. */
export function setType(
  schema: Schema,
  table: Target,
  column: string,
  type: TypeChoice,
): GraphEdit | null {
  const current = findColumn(findEntity(schema, table), column);
  if (!current) return null;
  const same =
    type.typeKind === "b"
      ? current.type_kind === "b" && canonicalType(current.type) === canonicalType(type.display)
      : current.type_schema === type.typeSchema && current.type_name === type.typeName;
  if (same) return null;
  const from = baseType(current, schema);
  const to =
    type.typeKind === "b"
      ? baseType({ type: type.display, type_kind: "b", type_schema: "", type_name: "" }, schema)
      : baseType(
          {
            type: type.display,
            type_kind: type.typeKind,
            type_schema: type.typeSchema,
            type_name: type.typeName,
          },
          schema,
        );
  return { kind: "setType", table, column, type, using: needsCast(from, to, type) };
}

function needsCast(from: string, to: string, type: TypeChoice): boolean {
  // Into an enum or composite from anything else: only an explicit cast reads text as one.
  if (type.typeKind === "e" || type.typeKind === "c") return from !== to;
  return needsUsing(from, to);
}

export type ForeignKeyPlan =
  { ok: true; edit: GraphEdit } | Extract<ForeignKeyCheck, { ok: false }>;

export function foreignKey(
  schema: Schema,
  from: Target,
  column: string,
  to: RelationRef,
  refColumn: string,
  onDelete: ReferentialActionCode,
): ForeignKeyPlan {
  const check = checkForeignKey(schema, from, column, to, refColumn);
  if (!check.ok) return check;
  return {
    ok: true,
    edit: {
      kind: "addForeignKey",
      table: from,
      column,
      references: { schema: to.schema, name: to.name },
      refColumn,
      name: foreignKeyName(schema, from, column),
      onDelete,
    },
  };
}
