// Column types the graph offers and writes: the common built-ins, the
// model's own enums and domains, and anything else typed by hand (checked to
// be a type name, so it cannot end the statement or start another).

import type { Column, Schema, TypeKindCode } from "../model";
import { quoteIdent } from "./quote";

/** A type as an edit writes it, and as the model would describe it. */
export interface TypeChoice {
  /** The SQL written after the column name: `text`, `varchar(80)`, `public.mood`. */
  sql: string;
  /** How it reads in the picker. */
  label: string;
  /** For the optimistic model: `format_type`'s spelling, best effort. */
  display: string;
  typeSchema: string;
  typeName: string;
  typeKind: TypeKindCode;
  isArray: boolean;
  /** Where it comes from, for grouping in the picker. */
  source: "common" | "enum" | "domain" | "custom";
  hint?: string;
}

const builtin = (sql: string, display: string, typeName: string, hint?: string): TypeChoice => ({
  sql,
  label: sql,
  display,
  typeSchema: "pg_catalog",
  typeName,
  typeKind: "b",
  isArray: false,
  source: "common",
  hint,
});

/** The built-ins most columns use, most common first. */
export const COMMON_TYPES: TypeChoice[] = [
  builtin("text", "text", "text", "Any length"),
  builtin("bigint", "bigint", "int8", "64-bit integer"),
  builtin("integer", "integer", "int4", "32-bit integer"),
  builtin("boolean", "boolean", "bool"),
  builtin("timestamptz", "timestamp with time zone", "timestamptz", "Date and time, with zone"),
  builtin("uuid", "uuid", "uuid"),
  builtin("jsonb", "jsonb", "jsonb", "Binary JSON"),
  builtin("numeric(12,2)", "numeric(12,2)", "numeric", "Exact decimal"),
  builtin("varchar(255)", "character varying(255)", "varchar", "Text up to 255 characters"),
  builtin("date", "date", "date"),
  builtin("smallint", "smallint", "int2", "16-bit integer"),
  builtin("timestamp", "timestamp without time zone", "timestamp", "Date and time, no zone"),
  builtin("time", "time without time zone", "time"),
  builtin("interval", "interval", "interval"),
  builtin("double precision", "double precision", "float8", "Floating point"),
  builtin("real", "real", "float4"),
  builtin("bytea", "bytea", "bytea", "Binary data"),
  builtin("inet", "inet", "inet", "IP address"),
  builtin("json", "json", "json"),
  {
    ...builtin("text[]", "text[]", "_text", "Array of text"),
    isArray: true,
  },
];

/** The model's enums and domains, as types to pick. */
export function modelTypes(schema: Schema): TypeChoice[] {
  const out: TypeChoice[] = [];
  const name = (s: string, n: string) =>
    s === "public" ? quoteIdent(n) : `${quoteIdent(s)}.${quoteIdent(n)}`;
  for (const e of schema.enums ?? []) {
    out.push({
      sql: name(e.schema, e.name),
      label: name(e.schema, e.name),
      display: name(e.schema, e.name),
      typeSchema: e.schema,
      typeName: e.name,
      typeKind: "e",
      isArray: false,
      source: "enum",
      hint: (e.labels ?? []).slice(0, 4).join(", ") + ((e.labels ?? []).length > 4 ? ", …" : ""),
    });
  }
  for (const d of schema.domains ?? []) {
    out.push({
      sql: name(d.schema, d.name),
      label: name(d.schema, d.name),
      display: name(d.schema, d.name),
      typeSchema: d.schema,
      typeName: d.name,
      typeKind: "d",
      isArray: false,
      source: "domain",
      hint: d.base_type,
    });
  }
  return out.sort((a, b) => a.label.localeCompare(b.label));
}

// A type name as Postgres's grammar takes it: words (`double precision`,
// `character varying`), a schema-qualified or quoted name, a modifier list of
// numbers, `with[out] time zone`, array brackets. Nothing that could end the
// statement, open a string or a comment.
const WORD = String.raw`(?:[A-Za-z_][A-Za-z0-9_$]*|"(?:[^"]|"")+")`;
const NAME = String.raw`${WORD}(?:\s*\.\s*${WORD})?(?:\s+[A-Za-z_][A-Za-z0-9_]*)*`;
const MODIFIERS = String.raw`(?:\s*\(\s*\d+\s*(?:,\s*-?\d+\s*)?\))?`;
const ZONE = String.raw`(?:\s+with(?:out)?\s+time\s+zone)?`;
const ARRAY = String.raw`(?:\s*\[\s*\d*\s*\])*`;
const TYPE = new RegExp(`^${NAME}${MODIFIERS}${ZONE}${ARRAY}$`, "i");

/** A type typed by hand, or null when it does not read as one. */
export function customType(text: string): TypeChoice | null {
  const sql = text.trim().replace(/\s+/g, " ");
  if (!sql || sql.length > 200 || !TYPE.test(sql)) return null;
  const known = COMMON_TYPES.find((t) => t.sql === sql.toLowerCase());
  if (known) return known;
  const isArray = /\]$/.test(sql);
  const base = sql.replace(/(\s*\[\s*\d*\s*\])+$/, "");
  return {
    sql,
    label: sql,
    display: canonicalType(sql),
    typeSchema: "pg_catalog",
    typeName: canonicalType(base).replace(/\(.*$/, ""),
    typeKind: "b",
    isArray,
    source: "custom",
  };
}

// How `format_type` spells the common aliases, so an optimistic column reads
// as the compiled one will, and a type change can be recognised once it lands.
const ALIASES: [RegExp, string][] = [
  [/^int8$/, "bigint"],
  [/^int4$|^int$/, "integer"],
  [/^int2$/, "smallint"],
  [/^serial8$|^bigserial$/, "bigint"],
  [/^serial4$|^serial$/, "integer"],
  [/^serial2$|^smallserial$/, "smallint"],
  [/^bool$/, "boolean"],
  [/^float8$|^float$/, "double precision"],
  [/^float4$/, "real"],
  [/^decimal\b/, "numeric"],
  [/^varchar\b/, "character varying"],
  [/^char\b|^bpchar\b/, "character"],
  [/^timestamptz(\(\d+\))?$/, "timestamp$1 with time zone"],
  [/^timestamp(\(\d+\))?$/, "timestamp$1 without time zone"],
  [/^timetz(\(\d+\))?$/, "time$1 with time zone"],
  [/^time(\(\d+\))?$/, "time$1 without time zone"],
  [/^varbit\b/, "bit varying"],
];

/** A built-in type as `format_type` prints it (best effort, lowercase). */
export function canonicalType(sql: string): string {
  let text = sql.trim().replace(/\s+/g, " ").toLowerCase();
  const brackets = /(\s*\[\s*\d*\s*\])+$/.exec(text);
  const suffix = brackets ? "[]" : "";
  if (brackets) text = text.slice(0, brackets.index);
  text = text
    .replace(/\s*\(\s*/g, "(")
    .replace(/\s*,\s*/g, ",")
    .replace(/\s*\)/g, ")");
  for (const [pattern, replacement] of ALIASES) {
    if (pattern.test(text)) {
      text = text.replace(pattern, replacement);
      break;
    }
  }
  return text + suffix;
}

// Types that compare with each other: Postgres finds an equality operator
// between them (or casts one to the other implicitly), so one can reference
// the other, and changing between them needs no USING.
const FAMILIES: Record<string, string> = {
  smallint: "integer",
  integer: "integer",
  bigint: "integer",
  numeric: "numeric",
  real: "float",
  "double precision": "float",
  text: "string",
  "character varying": "string",
  character: "string",
  name: "string",
  "timestamp with time zone": "datetime",
  "timestamp without time zone": "datetime",
  date: "datetime",
  "time with time zone": "time",
  "time without time zone": "time",
  json: "json",
  jsonb: "jsonb",
};

/** A column's type without modifiers, its domain resolved to the base type. */
export function baseType(
  column: Pick<Column, "type" | "type_kind" | "type_schema" | "type_name">,
  schema: Schema,
): string {
  if (column.type_kind === "d") {
    const domain = (schema.domains ?? []).find(
      (d) => d.schema === column.type_schema && d.name === column.type_name,
    );
    if (domain)
      return baseType(
        { type: domain.base_type, type_kind: "b", type_schema: "", type_name: "" },
        schema,
      );
  }
  if (column.type_kind === "e" || column.type_kind === "c")
    return `${column.type_schema}.${column.type_name}`;
  return canonicalType(column.type).replace(/\(.*?\)/, "");
}

/** The family a base type compares within: itself when it has none. */
export function typeFamily(base: string): string {
  if (base.endsWith("[]")) return `array:${typeFamily(base.slice(0, -2))}`;
  return FAMILIES[base] ?? base;
}

/**
 * Whether a column referencing `from` can reference `to`: the same family,
 * or an integer referencing a numeric (cast implicitly).
 */
export function comparable(from: string, to: string): boolean {
  const a = typeFamily(from);
  const b = typeFamily(to);
  return a === b || (a === "integer" && b === "numeric");
}

/**
 * Whether changing a column from one type to another needs `USING col::type`:
 * Postgres casts within a family by itself (an assignment cast), not across.
 */
export function needsUsing(from: string, to: string): boolean {
  const a = typeFamily(from);
  const b = typeFamily(to);
  if (a === b) return false;
  // Numbers widen and narrow among themselves by assignment.
  const numbers = new Set(["integer", "numeric", "float"]);
  if (numbers.has(a) && numbers.has(b)) return false;
  // Anything becomes text by assignment (its output function).
  if (b === "string") return false;
  return true;
}

/** The type a picker's text stands for: the model's enum or domain, a known or typed name; or null. */
export function typeFromText(schema: Schema, value: string): TypeChoice | null {
  const text = value.trim();
  if (!text) return null;
  return modelTypes(schema).find((c) => c.sql === text) ?? customType(text);
}
