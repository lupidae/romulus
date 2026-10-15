// Where each statement of a DDL text starts and ends, and which relation it is
// about: what links the text pane to the graph (jump to a table's CREATE, and
// select the table whose statement holds the cursor).
//
// A scanner, not a parser: it splits on top-level semicolons (skipping
// strings, quoted identifiers, dollar quotes and comments) and reads the head
// of each statement. That is exact for remus-sql's output and for ordinary
// hand-written DDL. The draft editor parses with libpg_query, which reports
// the same ranges; it can pass those instead (`SqlStatement` is the contract).

import type { RelationRef } from "../model";

export type StatementKind =
  | "create" // CREATE TABLE / VIEW / MATERIALIZED VIEW / FOREIGN TABLE
  | "alter" // ALTER TABLE …, foreign keys included
  | "comment" // COMMENT ON TABLE / COLUMN / CONSTRAINT …
  | "index" // CREATE INDEX … ON
  | "policy" // CREATE POLICY … ON
  | "type" // CREATE TYPE / DOMAIN
  | "other";

export interface SqlStatement {
  /** `ALTER … RENAME TO`: the name it gives (internal: targets are resolved through it). */
  renamedTo?: RelationRef;
  /** Offset of the first character (comments before it excluded). */
  from: number;
  /** Offset just after the terminating semicolon, or the end of the text. */
  to: number;
  kind: StatementKind;
  /** The relation (or, for `type`, the type) the statement defines or changes. */
  target?: RelationRef;
  /** `CREATE TABLE … PARTITION OF parent`: the parent. */
  partitionOf?: RelationRef;
}

/** Unqualified names resolve here, as with the default `search_path`. */
const DEFAULT_SCHEMA = "public";

interface Token {
  /** Identifier text, folded like Postgres: unquoted to lower case. */
  text: string;
  quoted: boolean;
  /** Punctuation: `.`, `(`, `,` … */
  punct?: string;
}

/** End of the dollar-quote tag starting at `i` (`$tag$`), or -1. */
function dollarTag(sql: string, i: number): string | null {
  const match = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(sql.slice(i, i + 64));
  return match ? match[0] : null;
}

/**
 * Statement ranges of `sql`, in text order. A table renamed further down
 * (`ALTER TABLE … RENAME TO`, as graph edits append) is known by its last
 * name: its CREATE, the rename and everything in between point at it.
 */
export function findStatements(sql: string): SqlStatement[] {
  return followRenames(scan(sql));
}

function followRenames(statements: SqlStatement[]): SqlStatement[] {
  if (!statements.some((s) => s.renamedTo)) return statements;
  const key = (r: RelationRef) => `${r.schema}\u0000${r.name}`;
  // Walking back: what each name is called at the end of the text.
  const final = new Map<string, RelationRef>();
  const follow = (r: RelationRef | undefined) => (r ? (final.get(key(r)) ?? r) : r);
  const out = [...statements];
  for (let i = out.length - 1; i >= 0; i--) {
    const s = out[i]!;
    if (s.renamedTo && s.target) final.set(key(s.target), follow(s.renamedTo)!);
    const { renamedTo: _, ...rest } = s;
    out[i] = {
      ...rest,
      target: follow(s.target),
      ...(s.partitionOf ? { partitionOf: follow(s.partitionOf) } : {}),
    };
  }
  return out;
}

function scan(sql: string): SqlStatement[] {
  const statements: SqlStatement[] = [];
  let start = -1;
  let i = 0;
  const n = sql.length;
  const close = (end: number) => {
    if (start >= 0) statements.push(classify(sql, start, end));
    start = -1;
  };

  while (i < n) {
    const ch = sql[i]!;
    const next = sql[i + 1];
    if (ch === "-" && next === "-") {
      const eol = sql.indexOf("\n", i);
      i = eol < 0 ? n : eol + 1;
      continue;
    }
    if (ch === "/" && next === "*") {
      let depth = 1;
      i += 2;
      while (i < n && depth > 0) {
        if (sql[i] === "/" && sql[i + 1] === "*") {
          depth++;
          i += 2;
        } else if (sql[i] === "*" && sql[i + 1] === "/") {
          depth--;
          i += 2;
        } else i++;
      }
      continue;
    }
    if (/\s/.test(ch)) {
      i++;
      continue;
    }
    if (start < 0) start = i;
    if (ch === ";") {
      close(i + 1);
      i++;
      continue;
    }
    if (ch === "'") {
      // E'…' strings take backslash escapes; '' is a quote in both kinds.
      const escapes = i > 0 && /[eE]/.test(sql[i - 1]!) && !/\w/.test(sql[i - 2] ?? "");
      i++;
      while (i < n) {
        if (escapes && sql[i] === "\\") i += 2;
        else if (sql[i] === "'" && sql[i + 1] === "'") i += 2;
        else if (sql[i] === "'") break;
        else i++;
      }
      i++;
      continue;
    }
    if (ch === '"') {
      i++;
      while (i < n) {
        if (sql[i] === '"' && sql[i + 1] === '"') i += 2;
        else if (sql[i] === '"') break;
        else i++;
      }
      i++;
      continue;
    }
    if (ch === "$") {
      const tag = dollarTag(sql, i);
      if (tag && !/\w/.test(sql[i - 1] ?? "")) {
        const end = sql.indexOf(tag, i + tag.length);
        i = end < 0 ? n : end + tag.length;
        continue;
      }
    }
    i++;
  }
  close(n);
  return statements;
}

/** The first `limit` tokens of a statement: enough to read its head. */
function tokens(sql: string, from: number, to: number, limit = 24): Token[] {
  const out: Token[] = [];
  let i = from;
  while (i < to && out.length < limit) {
    const ch = sql[i]!;
    if (/\s/.test(ch)) {
      i++;
    } else if (ch === "-" && sql[i + 1] === "-") {
      const eol = sql.indexOf("\n", i);
      i = eol < 0 ? to : eol + 1;
    } else if (ch === "/" && sql[i + 1] === "*") {
      const end = sql.indexOf("*/", i + 2);
      i = end < 0 ? to : end + 2;
    } else if (ch === '"') {
      let text = "";
      i++;
      while (i < to) {
        if (sql[i] === '"' && sql[i + 1] === '"') {
          text += '"';
          i += 2;
        } else if (sql[i] === '"') break;
        else text += sql[i++];
      }
      i++;
      out.push({ text, quoted: true });
    } else if (/[A-Za-z_\u0080-￿]/.test(ch)) {
      const match = /^[A-Za-z_\u0080-￿][A-Za-z0-9_$\u0080-￿]*/.exec(sql.slice(i, i + 128))!;
      out.push({ text: match[0].toLowerCase(), quoted: false });
      i += match[0].length;
    } else {
      out.push({ text: ch, quoted: false, punct: ch });
      i++;
    }
  }
  return out;
}

/** A dotted name starting at `at`: its parts, and the index after it. */
function dotted(ts: Token[], at: number): { parts: string[]; next: number } {
  const parts: string[] = [];
  let i = at;
  while (i < ts.length && !ts[i]!.punct) {
    parts.push(ts[i]!.text);
    if (ts[i + 1]?.punct === ".") i += 2;
    else {
      i++;
      break;
    }
  }
  return { parts, next: i };
}

function relation(parts: string[]): RelationRef | undefined {
  if (parts.length === 1) return { schema: DEFAULT_SCHEMA, name: parts[0]! };
  if (parts.length >= 2)
    return { schema: parts[parts.length - 2]!, name: parts[parts.length - 1]! };
  return undefined;
}

/** Skips the listed unquoted keywords, in order, wherever they appear. */
function skip(ts: Token[], at: number, ...words: string[]): number {
  let i = at;
  for (const w of words) if (!ts[i]?.quoted && ts[i]?.text === w) i++;
  return i;
}

function word(ts: Token[], i: number): string | undefined {
  const t = ts[i];
  return t && !t.quoted && !t.punct ? t.text : undefined;
}

function classify(sql: string, from: number, to: number): SqlStatement {
  const ts = tokens(sql, from, to);
  const base = { from, to };
  const w = (i: number) => word(ts, i);

  if (w(0) === "create") {
    let i = skip(ts, 1, "or", "replace");
    i = skip(ts, i, "global", "local");
    i = skip(ts, i, "temp", "temporary", "unlogged");
    i = skip(ts, i, "recursive");
    const kind = w(i);
    if (kind === "table" || kind === "view" || kind === "foreign" || kind === "materialized") {
      i = skip(ts, i + 1, "table", "view");
      i = skip(ts, i, "if", "not", "exists");
      const name = dotted(ts, i);
      const target = relation(name.parts);
      let partitionOf: RelationRef | undefined;
      if (w(name.next) === "partition" && w(name.next + 1) === "of") {
        partitionOf = relation(dotted(ts, name.next + 2).parts);
      }
      return { ...base, kind: "create", target, ...(partitionOf ? { partitionOf } : {}) };
    }
    if (kind === "unique" || kind === "index") {
      i = skip(ts, i, "unique", "index", "concurrently");
      i = skip(ts, i, "if", "not", "exists");
      if (w(i) !== "on") i = dotted(ts, i).next;
      if (w(i) === "on") {
        const at = skip(ts, i + 1, "only");
        return { ...base, kind: "index", target: relation(dotted(ts, at).parts) };
      }
    }
    if (kind === "policy") {
      const on = dotted(ts, i + 1).next;
      if (w(on) === "on")
        return { ...base, kind: "policy", target: relation(dotted(ts, on + 1).parts) };
    }
    if (kind === "type" || kind === "domain") {
      return { ...base, kind: "type", target: relation(dotted(ts, i + 1).parts) };
    }
  }

  if (w(0) === "alter") {
    let i = skip(ts, 1, "materialized", "foreign");
    if (w(i) === "table" || w(i) === "view") {
      i = skip(ts, i + 1, "if", "exists");
      i = skip(ts, i, "only");
      const name = dotted(ts, i);
      const target = relation(name.parts);
      const to = ts[name.next + 2];
      if (target && w(name.next) === "rename" && w(name.next + 1) === "to" && to && !to.punct)
        return {
          ...base,
          kind: "alter",
          target,
          renamedTo: { schema: target.schema, name: to.text },
        };
      return { ...base, kind: "alter", target };
    }
  }

  if (w(0) === "comment" && w(1) === "on") {
    let i = 2;
    const what = w(i);
    if (what === "column") {
      const parts = dotted(ts, i + 1).parts;
      return { ...base, kind: "comment", target: relation(parts.slice(0, -1)) };
    }
    if (what === "constraint" || what === "policy" || what === "trigger") {
      const on = dotted(ts, i + 1).next;
      if (w(on) === "on") {
        const at = skip(ts, on + 1, "domain");
        return { ...base, kind: "comment", target: relation(dotted(ts, at).parts) };
      }
    }
    if (what === "type" || what === "domain") {
      return { ...base, kind: "comment", target: relation(dotted(ts, i + 1).parts) };
    }
    i = skip(ts, i, "materialized", "foreign");
    if (w(i) === "table" || w(i) === "view") {
      return { ...base, kind: "comment", target: relation(dotted(ts, i + 1).parts) };
    }
  }

  return { ...base, kind: "other" };
}

/** The statement whose range holds `offset` (ranges are sorted and disjoint). */
export function statementAt(
  statements: readonly SqlStatement[],
  offset: number,
): SqlStatement | undefined {
  let lo = 0;
  let hi = statements.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const s = statements[mid]!;
    if (offset < s.from) hi = mid - 1;
    else if (offset > s.to) lo = mid + 1;
    else return s;
  }
  return undefined;
}
