// Where a table is defined in the read-only renderings (JSON, DBML,
// Mermaid), so selecting it in the graph scrolls those tabs too. Each knows
// the layout remus's emitter writes; anything else simply is not found.

import type { RelationRef } from "../model";
import type { Range } from "./codemirror";

const escape = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** From an opening brace to its match, skipping strings. */
function closeBrace(text: string, open: number): number {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    const ch = text[i];
    if (ch === '"') {
      for (i++; i < text.length && text[i] !== '"'; i++) if (text[i] === "\\") i++;
    } else if (ch === "{") depth++;
    else if (ch === "}" && --depth === 0) return i + 1;
  }
  return text.length;
}

/**
 * The entity's object in remus JSON: `"schema"`, `"name"` then `"kind"` at its
 * top, which a view's `depends_on` references (schema and name only) lack.
 */
export function locateInJson(text: string, ref: RelationRef): Range | undefined {
  const s = escape(JSON.stringify(ref.schema));
  const n = escape(JSON.stringify(ref.name));
  const pattern = new RegExp(
    `"schema":\\s*${s},\\s*"name":\\s*${n},\\s*"kind"|"name":\\s*${n},\\s*"schema":\\s*${s},\\s*"kind"`,
  );
  const match = pattern.exec(text);
  if (!match) return undefined;
  const open = text.lastIndexOf("{", match.index);
  if (open < 0) return undefined;
  return { from: open, to: closeBrace(text, open) };
}

/** A block that ends on a line holding only `}` at its own indentation. */
function block(text: string, start: number): Range {
  const lineStart = text.lastIndexOf("\n", start) + 1;
  const indent = /^\s*/.exec(text.slice(lineStart))![0];
  const end = new RegExp(`\\n${escape(indent)}\\}[ \\t]*(\\n|$)`).exec(text.slice(start));
  return { from: lineStart, to: end ? start + end.index + end[0].length : text.length };
}

/** remus-dbml: `Table "schema"."name" {` (quotes and schema optional). */
export function locateInDbml(text: string, ref: RelationRef): Range | undefined {
  const name = `"?${escape(ref.name)}"?`;
  const pattern = new RegExp(
    `^Table\\s+(?:"?${escape(ref.schema)}"?\\.)?${name}(?:\\s+as\\s+\\S+)?\\s*(?:\\[[^\\]]*\\])?\\s*\\{`,
    "m",
  );
  const match = pattern.exec(text);
  return match ? block(text, match.index) : undefined;
}

/** remus-mermaid's node name: word characters only, `schema_name` when qualified. */
export function mermaidNode(ref: RelationRef, qualified: boolean): string {
  const raw = qualified ? `${ref.schema}_${ref.name}` : ref.name;
  let token = raw.replace(/[^A-Za-z0-9_()[\]]/g, "_");
  if (!/^[A-Za-z_]/.test(token)) token = `_${token}`;
  return token;
}

export function locateInMermaid(
  text: string,
  ref: RelationRef,
  qualified: boolean,
): Range | undefined {
  const pattern = new RegExp(`^[ \\t]*${escape(mermaidNode(ref, qualified))}[ \\t]*\\{`, "m");
  const match = pattern.exec(text);
  if (match) return block(text, match.index);
  // A table without columns is a bare line.
  const bare = new RegExp(`^[ \\t]*${escape(mermaidNode(ref, qualified))}[ \\t]*$`, "m").exec(text);
  return bare ? { from: bare.index, to: bare.index + bare[0].length } : undefined;
}
