// Where an object of the diff is in each side's SQL: the statements that
// define it (its CREATE, foreign keys, indexes, policies, comments; a type's
// CREATE and comment; an extension's CREATE EXTENSION).

import type { RelationRef } from "@/features/schema/model";
import type { Range } from "@/features/schema/text/codemirror";
import type { SqlStatement } from "@/features/schema/text/statements";

import type { ChangeItem } from "./changes";

/** What to find: a diff object, or a table picked in the graph. */
export type Locatable = Pick<ChangeItem, "category" | "schema" | "name">;

const same = (a: RelationRef | undefined, schema: string | null, name: string) =>
  !!a && a.schema === schema && a.name === name;

const RELATIONS = new Set(["table", "view", "materialized_view", "foreign_table", "partition"]);
const TYPES = new Set(["enum", "domain", "composite"]);

function quoted(name: string): string {
  return /^[a-z_][a-z0-9_$]*$/.test(name) ? name : `"${name.replaceAll('"', '""')}"`;
}

/** The ranges of `sql` (scanned into `statements`) that are about `target`, in text order. */
export function rangesOf(
  sql: string,
  statements: readonly SqlStatement[],
  target: Locatable,
): Range[] {
  const { schema, name } = target;
  const head = (s: SqlStatement) => sql.slice(s.from, s.from + 40).toUpperCase();
  if (RELATIONS.has(target.category)) {
    return statements.filter(
      (s) =>
        (same(s.target, schema, name) || same(s.partitionOf, schema, name)) &&
        s.kind !== "type" &&
        s.kind !== "other" &&
        !/^COMMENT ON (TYPE|DOMAIN)\b/.test(head(s)),
    );
  }
  if (TYPES.has(target.category)) {
    return statements.filter(
      (s) =>
        same(s.target, schema, name) &&
        (s.kind === "type" || /^COMMENT ON (TYPE|DOMAIN)\b/.test(head(s))),
    );
  }
  // An extension: `CREATE EXTENSION IF NOT EXISTS name …`.
  const create = new RegExp(
    `^CREATE EXTENSION (IF NOT EXISTS )?${escape(quoted(name))}(\\s|;)`,
    "i",
  );
  return statements.filter((s) => s.kind === "other" && create.test(sql.slice(s.from, s.to)));
}

function escape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
