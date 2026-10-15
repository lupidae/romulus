// Identifiers and literals in SQL the graph writes, quoted exactly as
// Postgres's own `quote_ident` and `quote_literal` would (src/backend/utils/
// adt/ruleutils.c): a name is left bare only when it reads back as itself.

import type { RelationRef } from "../model";
import { QUOTED_KEYWORDS } from "./keywords";

/** Longest identifier Postgres keeps (NAMEDATALEN - 1), in bytes. */
export const MAX_IDENTIFIER_BYTES = 63;

const SAFE = /^[a-z_][a-z0-9_]*$/;

/**
 * `quote_ident`: bare when it is lowercase letters, digits and underscores,
 * not starting with a digit, and not a keyword Postgres reserves in any
 * way; otherwise in double quotes, with inner double quotes doubled.
 */
export function quoteIdent(name: string): string {
  if (SAFE.test(name) && !QUOTED_KEYWORDS.has(name)) return name;
  return `"${name.replaceAll('"', '""')}"`;
}

/** `schema.name`, each part quoted as it needs. */
export function qualified(ref: RelationRef): string {
  return `${quoteIdent(ref.schema)}.${quoteIdent(ref.name)}`;
}

/**
 * A string constant: in single quotes, inner ones doubled. Backslashes stay
 * as they are (`standard_conforming_strings` is on, Postgres's default).
 */
export function quoteLiteral(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}

/** UTF-8 length: what NAMEDATALEN counts. */
export function byteLength(value: string): number {
  return new TextEncoder().encode(value).length;
}

/**
 * Why a name cannot be used for a table or column, or null when it can.
 * Postgres takes any characters (quoted); it truncates past 63 bytes, which
 * would silently make the name another one.
 */
export function nameProblem(name: string): string | null {
  if (name.length === 0) return "Give it a name.";
  if (name.includes("\u0000")) return "Names cannot hold a NUL character.";
  if (byteLength(name) > MAX_IDENTIFIER_BYTES)
    return `Postgres keeps ${MAX_IDENTIFIER_BYTES} bytes of a name: make it shorter.`;
  return null;
}
