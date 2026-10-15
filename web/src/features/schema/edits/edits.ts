// Schema edits made from the graph, and the SQL each one appends to a
// draft (docs/MVP.md "Schema edits from the graph append SQL"). An edit
// carries every decision already made (CASCADE or not, USING or not, the
// foreign key's name), so turning it into SQL needs nothing else: `sqlFor`
// is pure, and every statement kind is snapshot-tested.

import type { EntityKindCode, ReferentialActionCode, RelationRef } from "../model";
import { qualified, quoteIdent, quoteLiteral } from "./quote";
import type { TypeChoice } from "./types";

/** A relation an edit is about, with its kind (views take other verbs). */
export interface Target extends RelationRef {
  kind: EntityKindCode;
}

export interface NewColumn {
  name: string;
  type: TypeChoice;
  notNull: boolean;
  /** An SQL expression, or null for none. */
  default: string | null;
}

export type GraphEdit =
  | { kind: "renameTable"; table: Target; to: string }
  | { kind: "renameColumn"; table: Target; column: string; to: string }
  | { kind: "addTable"; table: RelationRef }
  | { kind: "addColumn"; table: Target; column: NewColumn }
  /** `cascade`: other objects depend on it, and go with it. */
  | { kind: "dropTable"; table: Target; cascade: boolean }
  | { kind: "dropColumn"; table: Target; column: string; cascade: boolean }
  /** `using`: Postgres would not cast the values by itself. */
  | { kind: "setType"; table: Target; column: string; type: TypeChoice; using: boolean }
  | { kind: "setNotNull"; table: Target; column: string; notNull: boolean }
  | { kind: "setDefault"; table: Target; column: string; default: string | null }
  /** A table's comment, or a column's when `column` is set; null removes it. */
  | { kind: "setComment"; table: Target; column?: string; comment: string | null }
  | {
      kind: "addForeignKey";
      table: Target;
      column: string;
      references: RelationRef;
      refColumn: string;
      name: string;
      onDelete: ReferentialActionCode;
    };

/** The verb's object for a relation of this kind: `TABLE`, `VIEW`… */
export function relationWord(kind: EntityKindCode): string {
  switch (kind) {
    case "v":
      return "VIEW";
    case "m":
      return "MATERIALIZED VIEW";
    case "f":
      return "FOREIGN TABLE";
    case "r":
    case "p":
      return "TABLE";
  }
}

const ON_DELETE: Record<ReferentialActionCode, string | null> = {
  a: null,
  r: "RESTRICT",
  c: "CASCADE",
  n: "SET NULL",
  d: "SET DEFAULT",
};

/** The id column every table added from the graph starts with. */
export const ID_COLUMN = "id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY";

/** The statements an edit appends, each ending with `;`. */
export function sqlFor(edit: GraphEdit): string[] {
  switch (edit.kind) {
    case "renameTable":
      return [
        `ALTER ${relationWord(edit.table.kind)} ${qualified(edit.table)} RENAME TO ${quoteIdent(edit.to)};`,
      ];
    case "renameColumn":
      return [
        `ALTER ${relationWord(edit.table.kind)} ${qualified(edit.table)} RENAME COLUMN ${quoteIdent(edit.column)} TO ${quoteIdent(edit.to)};`,
      ];
    case "addTable":
      return [`CREATE TABLE ${qualified(edit.table)} (\n    ${ID_COLUMN}\n);`];
    case "addColumn": {
      const c = edit.column;
      const parts = [`${quoteIdent(c.name)} ${c.type.sql}`];
      if (c.notNull) parts.push("NOT NULL");
      if (c.default !== null) parts.push(`DEFAULT ${c.default}`);
      return [`ALTER TABLE ${qualified(edit.table)} ADD COLUMN ${parts.join(" ")};`];
    }
    case "dropTable":
      return [
        `DROP ${relationWord(edit.table.kind)} ${qualified(edit.table)}${edit.cascade ? " CASCADE" : ""};`,
      ];
    case "dropColumn":
      return [
        `ALTER TABLE ${qualified(edit.table)} DROP COLUMN ${quoteIdent(edit.column)}${edit.cascade ? " CASCADE" : ""};`,
      ];
    case "setType": {
      const column = quoteIdent(edit.column);
      const using = edit.using ? ` USING ${column}::${edit.type.sql}` : "";
      return [
        `ALTER TABLE ${qualified(edit.table)} ALTER COLUMN ${column} TYPE ${edit.type.sql}${using};`,
      ];
    }
    case "setNotNull":
      return [
        `ALTER TABLE ${qualified(edit.table)} ALTER COLUMN ${quoteIdent(edit.column)} ${edit.notNull ? "SET" : "DROP"} NOT NULL;`,
      ];
    case "setDefault":
      return [
        edit.default === null
          ? `ALTER TABLE ${qualified(edit.table)} ALTER COLUMN ${quoteIdent(edit.column)} DROP DEFAULT;`
          : `ALTER TABLE ${qualified(edit.table)} ALTER COLUMN ${quoteIdent(edit.column)} SET DEFAULT ${edit.default};`,
      ];
    case "setComment": {
      const value = edit.comment === null ? "NULL" : quoteLiteral(edit.comment);
      return [
        edit.column === undefined
          ? `COMMENT ON ${relationWord(edit.table.kind)} ${qualified(edit.table)} IS ${value};`
          : `COMMENT ON COLUMN ${qualified(edit.table)}.${quoteIdent(edit.column)} IS ${value};`,
      ];
    }
    case "addForeignKey": {
      const action = ON_DELETE[edit.onDelete];
      return [
        `ALTER TABLE ${qualified(edit.table)} ADD CONSTRAINT ${quoteIdent(edit.name)} FOREIGN KEY (${quoteIdent(edit.column)}) REFERENCES ${qualified(edit.references)} (${quoteIdent(edit.refColumn)})${action ? ` ON DELETE ${action}` : ""};`,
      ];
    }
  }
}

/** What an edit does, in a few words: undo toasts and error notes. */
export function describe(edit: GraphEdit): string {
  const t = edit.table.name;
  switch (edit.kind) {
    case "renameTable":
      return `Rename ${t} to ${edit.to}`;
    case "renameColumn":
      return `Rename ${t}.${edit.column} to ${edit.to}`;
    case "addTable":
      return `Add table ${t}`;
    case "addColumn":
      return `Add column ${t}.${edit.column.name}`;
    case "dropTable":
      return `Drop ${edit.table.kind === "v" || edit.table.kind === "m" ? "view" : "table"} ${t}`;
    case "dropColumn":
      return `Drop column ${t}.${edit.column}`;
    case "setType":
      return `Change ${t}.${edit.column} to ${edit.type.label}`;
    case "setNotNull":
      return edit.notNull
        ? `Make ${t}.${edit.column} NOT NULL`
        : `Allow nulls in ${t}.${edit.column}`;
    case "setDefault":
      return edit.default === null
        ? `Remove the default of ${t}.${edit.column}`
        : `Set the default of ${t}.${edit.column}`;
    case "setComment":
      return `${edit.comment === null ? "Remove" : "Edit"} the comment on ${edit.column === undefined ? t : `${t}.${edit.column}`}`;
    case "addForeignKey":
      return `Link ${t}.${edit.column} to ${edit.references.name}.${edit.refColumn}`;
  }
}
