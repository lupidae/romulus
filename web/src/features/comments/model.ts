// Comment threads (docs/MVP.md "Review round 1", item 6): what they are,
// what they are pinned to, and where they show. Plain functions over plain
// data, shared by a draft (threads live in its Y.Doc, checked and stamped by
// the server: crates/romulus-api/src/collab/comments.rs) and a version
// (threads over REST, those of its draft's review frozen).
//
// The doc holds two maps of plain JSON values, so two people replying at
// once never overwrite each other:
//   threads:  Y.Map<thread id, DocThread>
//   comments: Y.Map<comment id, DocComment>

import { qualifiedName, type RelationRef, type Schema } from "@/features/schema/model";
import { findStatements, type SqlStatement } from "@/features/schema/text/statements";

/** What a thread is pinned to (the server's `comments::Anchor`). */
export type Anchor =
  | { kind: "table"; table: string; statement?: string }
  | { kind: "column"; table: string; column: string; statement?: string }
  | { kind: "point"; x: number; y: number }
  | { kind: "statement"; statement: string };

/** A thread as the doc holds it (its comments are apart). */
export interface DocThread {
  anchor: Anchor;
  /** Who started it (the server stamps it). */
  by: string;
  at: string;
  resolved: { by: string; name: string; at: string } | null;
}

export interface DocComment {
  thread: string;
  author: { id: string; name: string };
  body: string;
  /** Member ids @mentioned. */
  mentions: string[];
  at: string;
  edited: string | null;
}

/** One comment, as the UI shows it (a draft's or a version's). */
export interface CommentView {
  id: string;
  /** Null once their account is gone. */
  authorId: string | null;
  authorName: string;
  body: string;
  mentions: string[];
  at: string;
  edited: string | null;
}

/** One thread, as the UI shows it. */
export interface ThreadView {
  id: string;
  anchor: Anchor;
  by: string | null;
  at: string;
  resolved: { by: string | null; name: string; at: string } | null;
  /** Oldest first. */
  comments: CommentView[];
  /** From the draft's review, kept with its version: read-only. */
  frozen?: boolean;
}

/** Up to 4000 characters, like the server. */
export const MAX_BODY = 4000;
export const MAX_MENTIONS = 20;
/** A statement's head, as an anchor keeps it. */
const MAX_HEAD = 200;

// ---- Reading the doc ---------------------------------------------------------

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isAnchor(value: unknown): value is Anchor {
  if (!isRecord(value)) return false;
  switch (value.kind) {
    case "table":
      return typeof value.table === "string";
    case "column":
      return typeof value.table === "string" && typeof value.column === "string";
    case "point":
      return Number.isFinite(value.x) && Number.isFinite(value.y);
    case "statement":
      return typeof value.statement === "string";
    default:
      return false;
  }
}

function docThread(value: unknown): DocThread | null {
  if (!isRecord(value) || !isAnchor(value.anchor)) return null;
  if (typeof value.by !== "string" || typeof value.at !== "string") return null;
  const r = value.resolved;
  const resolved =
    isRecord(r) &&
    typeof r.by === "string" &&
    typeof r.name === "string" &&
    typeof r.at === "string"
      ? { by: r.by, name: r.name, at: r.at }
      : null;
  return { anchor: value.anchor, by: value.by, at: value.at, resolved };
}

function docComment(value: unknown): DocComment | null {
  if (!isRecord(value) || typeof value.thread !== "string" || typeof value.body !== "string")
    return null;
  const a = value.author;
  if (!isRecord(a) || typeof a.id !== "string" || typeof a.name !== "string") return null;
  if (typeof value.at !== "string") return null;
  return {
    thread: value.thread,
    author: { id: a.id, name: a.name },
    body: value.body,
    mentions: Array.isArray(value.mentions)
      ? value.mentions.filter((m): m is string => typeof m === "string")
      : [],
    at: value.at,
    edited: typeof value.edited === "string" ? value.edited : null,
  };
}

const byTime = <T extends { at: string; id: string }>(a: T, b: T) =>
  a.at < b.at ? -1 : a.at > b.at ? 1 : a.id < b.id ? -1 : a.id > b.id ? 1 : 0;

/**
 * The doc's two maps as threads with their comments, oldest first. Entries
 * that do not read are left out, and so is a thread with no comment left
 * (its author deleted them all: the server removes it).
 */
export function threadsOf(
  threads: Record<string, unknown>,
  comments: Record<string, unknown>,
): ThreadView[] {
  const byThread = new Map<string, CommentView[]>();
  for (const [id, value] of Object.entries(comments)) {
    const c = docComment(value);
    if (!c) continue;
    const list = byThread.get(c.thread) ?? [];
    list.push({
      id,
      authorId: c.author.id,
      authorName: c.author.name,
      body: c.body,
      mentions: c.mentions,
      at: c.at,
      edited: c.edited,
    });
    byThread.set(c.thread, list);
  }
  const out: ThreadView[] = [];
  for (const [id, value] of Object.entries(threads)) {
    const t = docThread(value);
    const list = byThread.get(id);
    if (!t || !list?.length) continue;
    out.push({
      id,
      anchor: t.anchor,
      by: t.by,
      at: t.at,
      resolved: t.resolved,
      comments: list.sort(byTime),
    });
  }
  return out.sort(byTime);
}

// ---- Anchors -------------------------------------------------------------------

/** "users.email", "users", "Canvas", or the statement's head; the schema shown when not `public`. */
export function anchorLabel(anchor: Anchor): string {
  const table = (key: string) => (key.startsWith("public.") ? key.slice(7) : key);
  switch (anchor.kind) {
    case "table":
      return table(anchor.table);
    case "column":
      return `${table(anchor.table)}.${anchor.column}`;
    case "point":
      return "Canvas";
    case "statement":
      return anchor.statement;
  }
}

/** The relation an anchor is about, if any. */
export function anchorTable(anchor: Anchor): string | null {
  return anchor.kind === "table" || anchor.kind === "column" ? anchor.table : null;
}

/**
 * Whether the anchor still points at something: its table (and column) in
 * the model shown, its statement in the text. Points always do. A thread
 * whose anchor does not is outdated (shown apart, never lost).
 */
export function anchorExists(anchor: Anchor, schema: Schema | null, sql: string | null): boolean {
  switch (anchor.kind) {
    case "point":
      return true;
    case "statement":
      return sql === null || findStatementHead(sql, anchor.statement) !== null;
    case "table":
    case "column": {
      if (!schema) return true;
      const entity = (schema.entities ?? []).find((e) => qualifiedName(e) === anchor.table);
      if (!entity) return false;
      return (
        anchor.kind === "table" || (entity.columns ?? []).some((c) => c.name === anchor.column)
      );
    }
  }
}

/** The anchor once a table is renamed (`from`, `to`: qualified names). */
export function renameTable(anchor: Anchor, from: string, to: string): Anchor {
  if ((anchor.kind === "table" || anchor.kind === "column") && anchor.table === from)
    return { ...anchor, table: to };
  return anchor;
}

/** The anchor once a column of `table` is renamed. */
export function renameColumn(anchor: Anchor, table: string, from: string, to: string): Anchor {
  if (anchor.kind === "column" && anchor.table === table && anchor.column === from)
    return { ...anchor, column: to };
  return anchor;
}

// ---- Statements ----------------------------------------------------------------

/** A statement's head, as anchors keep it: its first words, whitespace collapsed. */
export function statementHead(sql: string, statement: Pick<SqlStatement, "from" | "to">): string {
  let text = sql.slice(statement.from, statement.to).replace(/;\s*$/, "");
  const paren = text.indexOf("(");
  if (paren > 0) text = text.slice(0, paren);
  text = text.replace(/\s+/g, " ").trim();
  return text.length > MAX_HEAD ? `${text.slice(0, MAX_HEAD - 1)}…` : text;
}

/** The first statement whose head is `head`. */
function findStatementHead(sql: string, head: string): SqlStatement | null {
  return findStatements(sql).find((s) => statementHead(sql, s) === head) ?? null;
}

const sameRef = (a: RelationRef | undefined, key: string) => !!a && `${a.schema}.${a.name}` === key;

/** `name` as a column definition starts: quoted exactly, or unquoted any case. */
function columnLine(sql: string, s: SqlStatement, column: string): number | null {
  const body = sql.slice(s.from, s.to);
  const quoted = `"${column.replace(/"/g, '""')}"`;
  const plain = /^[a-z_][a-z0-9_$]*$/.test(column) ? column : null;
  let offset = s.from;
  // The statement's first line holds its name, not a column.
  let first = true;
  for (const line of body.split("\n")) {
    if (!first) {
      const trimmed = line.trimStart();
      const at = offset + (line.length - trimmed.length);
      const lower = trimmed.toLowerCase();
      if (
        trimmed.startsWith(`${quoted} `) ||
        (plain && (lower.startsWith(`${plain} `) || lower.startsWith(`${plain}\t`)))
      ) {
        return sql.lastIndexOf("\n", at) + 1;
      }
    }
    first = false;
    offset += line.length + 1;
  }
  return null;
}

/**
 * Where the SQL pane shows a thread: the start of the line its widget goes
 * above, or null when it is not in this text (a point of the canvas, a table
 * this text does not mention). Its statement when it has one (what a `---`
 * line was written above), else its table's `CREATE` (a column's own line
 * inside it), else the first statement about its table (a migration).
 */
export function placeInSql(
  sql: string,
  anchor: Anchor,
  statements: SqlStatement[] = findStatements(sql),
): number | null {
  const lineStart = (offset: number) => sql.lastIndexOf("\n", offset - 1) + 1;
  if (anchor.kind === "point") return null;
  if (anchor.statement !== undefined) {
    const own = statements.find((s) => statementHead(sql, s) === anchor.statement);
    if (own) return lineStart(own.from);
    if (anchor.kind === "statement") return null;
  }
  if (anchor.kind === "statement") return null;
  const create = statements.find((s) => s.kind === "create" && sameRef(s.target, anchor.table));
  if (create) {
    if (anchor.kind === "column") {
      const line = columnLine(sql, create, anchor.column);
      if (line !== null) return line;
    }
    return lineStart(create.from);
  }
  const about = statements.filter((s) => s.kind !== "type" && sameRef(s.target, anchor.table));
  if (anchor.kind === "column") {
    const mention = about.find((s) =>
      new RegExp(`\\b${anchor.column.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, "i").test(
        sql.slice(s.from, s.to),
      ),
    );
    if (mention) return lineStart(mention.from);
  }
  return about[0] ? lineStart(about[0].from) : null;
}

// ---- `---` lines ---------------------------------------------------------------

/**
 * A line typed as a comment: `--- why text?` (three dashes, then words).
 * Lines of dashes only (`-----` separators) and `----` are SQL comments.
 */
export function commentLine(line: string): string | null {
  const match = /^\s*---(?!-)\s*(.*?)\s*$/.exec(line);
  return match && match[1] ? match[1] : null;
}

const COLUMN_STATEMENT =
  /^\s*(?:alter\s+table\s+(?:if\s+exists\s+)?(?:only\s+)?\S+\s+(?:add|alter|drop|rename)\s+(?:column\s+)?(?:if\s+(?:not\s+)?exists\s+)?("(?:[^"]|"")+"|[a-z_][a-z0-9_$]*)|comment\s+on\s+column\s+(?:\S+\.)?("(?:[^"]|"")+"|[a-z_][a-z0-9_$]*)\s+is)/i;

/** The column a statement is about (`ALTER TABLE t ADD COLUMN c`, `COMMENT ON COLUMN t.c`). */
function statementColumn(text: string): string | null {
  const match = COLUMN_STATEMENT.exec(text);
  const raw = match?.[1] ?? match?.[2];
  if (!raw) return null;
  if (raw.startsWith('"')) return raw.slice(1, -1).replace(/""/g, '"');
  const lower = raw.toLowerCase();
  // `ALTER TABLE t ADD CONSTRAINT …`, `ALTER TABLE t ALTER …`: no column.
  return ["constraint", "primary", "unique", "foreign", "check", "exclude"].includes(lower)
    ? null
    : lower;
}

/**
 * What a comment written above `offset` (the text with the `---` line taken
 * out) is pinned to: the next statement, or the column definition on the
 * next line inside a `CREATE TABLE`. Null when no statement follows (nor
 * precedes: the last one is then the closest).
 */
export function anchorBelow(sql: string, offset: number): Anchor | null {
  const statements = findStatements(sql);
  const inside = statements.find((s) => s.from < offset && offset < s.to);
  if (inside?.kind === "create" && inside.target) {
    const table = qualifiedName(inside.target);
    const next =
      sql
        .slice(offset)
        .split("\n")
        .find((l) => l.trim() !== "") ?? "";
    const column = /^\s*("(?:[^"]|"")+"|[A-Za-z_][A-Za-z0-9_$]*)\s/.exec(next)?.[1];
    const reserved = /^(constraint|primary|unique|foreign|check|exclude|like)$/i;
    if (column && !reserved.test(column)) {
      const name = column.startsWith('"')
        ? column.slice(1, -1).replace(/""/g, '"')
        : column.toLowerCase();
      return { kind: "column", table, column: name };
    }
    return { kind: "table", table };
  }
  const statement =
    inside ?? statements.find((s) => s.from >= offset) ?? statements[statements.length - 1];
  if (!statement) return null;
  const head = statementHead(sql, statement);
  if (statement.kind === "create" && statement.target) {
    return { kind: "table", table: qualifiedName(statement.target) };
  }
  if (statement.target && statement.kind !== "type") {
    const table = qualifiedName(statement.target);
    const column = statementColumn(sql.slice(statement.from, statement.to));
    return column
      ? { kind: "column", table, column, statement: head }
      : { kind: "table", table, statement: head };
  }
  return head ? { kind: "statement", statement: head } : null;
}

// ---- Who, what, when -------------------------------------------------------------

/** Whether someone other than `me` wrote in it after `readAt`. */
export function isUnread(thread: ThreadView, me: string, readAt: string | null | undefined) {
  return thread.comments.some((c) => c.authorId !== me && (!readAt || c.at > readAt));
}

/** The thread involves `me`: they started it, wrote in it, or were mentioned. */
export function involves(thread: ThreadView, me: string): boolean {
  return (
    thread.by === me || thread.comments.some((c) => c.authorId === me || c.mentions.includes(me))
  );
}

/** The time of its latest comment. */
export function lastActivity(thread: ThreadView): string {
  return thread.comments.at(-1)?.at ?? thread.at;
}

/** "Bob is typing…", for the people writing in a thread now; null for nobody. */
export function typingLine(names: string[]): string | null {
  if (names.length === 0) return null;
  if (names.length === 1) return `${names[0]} is typing…`;
  if (names.length === 2) return `${names[0]} and ${names[1]} are typing…`;
  return `${names.length} people are typing…`;
}

/** A member who can be mentioned. */
export interface Mentionable {
  id: string;
  name: string;
}

/** The members a body mentions: `@Name` for each (the longest name that fits). */
export function mentionsIn(body: string, members: readonly Mentionable[]): string[] {
  const found: string[] = [];
  for (const part of bodyParts(body, members)) {
    if ("mention" in part && !found.includes(part.mention.id) && found.length < MAX_MENTIONS)
      found.push(part.mention.id);
  }
  return found;
}

/** A body cut into text and mentions (of the members it names), for display. */
export function bodyParts(
  body: string,
  mentioned: readonly Mentionable[],
): ({ text: string } | { mention: Mentionable; text: string })[] {
  if (mentioned.length === 0) return [{ text: body }];
  const names = [...mentioned]
    .sort((a, b) => b.name.length - a.name.length)
    .map((m) => m.name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  const pattern = new RegExp(`@(${names.join("|")})(?![\\w])`, "gi");
  const out: ({ text: string } | { mention: Mentionable; text: string })[] = [];
  let last = 0;
  for (const match of body.matchAll(pattern)) {
    const at = match.index;
    const before = body[at - 1];
    if (at > 0 && before !== undefined && !/[\s(]/.test(before)) continue;
    if (at > last) out.push({ text: body.slice(last, at) });
    const mention = mentioned.find((m) => m.name.toLowerCase() === match[1]!.toLowerCase())!;
    out.push({ mention, text: match[0] });
    last = at + match[0].length;
  }
  if (last < body.length) out.push({ text: body.slice(last) });
  return out;
}
