// An object's history in words: one event per version where it appeared,
// changed (how, as romulus-diff structures it), was renamed, went, or came
// back.

import type { Counts, Frame } from "./frame";
import type { History, ObjectChange, State, TimelineVersion } from "./history";

export type EventKind = "added" | "removed" | "changed" | "renamed" | "restored";

export interface HistoryEvent {
  /** The version (index) it happened in. */
  at: number;
  kind: EventKind;
  /** What changed, worded: `type bigint → numeric`. Empty for an addition or removal. */
  details: string[];
}

const LABELS: Record<string, string> = {
  type: "type",
  type_name: "type",
  type_kind: "kind of type",
  not_null: "not null",
  default: "default",
  expression: "expression",
  identity: "identity",
  generated: "generated",
  comment: "comment",
  position: "position",
  kind: "kind",
  rls: "row level security",
  rls_forced: "forced row level security",
  unlogged: "unlogged",
  partition_key: "partition key",
  view_definition: "definition",
  depends_on: "reads",
  definition: "definition",
  command: "command",
  permissive: "permissive",
  roles: "roles",
  using: "using",
  check: "check",
  parent: "parent",
  bound: "bound",
  base_type: "base type",
  constraints: "checks",
  schema: "schema",
  version: "version",
};

const WORDS: Record<string, string> = {
  table: "table",
  partitioned_table: "partitioned table",
  view: "view",
  materialized_view: "materialized view",
  foreign_table: "foreign table",
  primary_key: "primary key",
  foreign_key: "foreign key",
  unique: "unique",
  exclusion: "exclusion",
  by_default: "by default",
};

/** A value as a change shows it: short, `none` for nothing. */
function show(value: unknown, max = 48): string {
  if (value === null || value === undefined) return "none";
  if (typeof value === "boolean") return value ? "yes" : "no";
  if (typeof value === "string") {
    const text = WORDS[value] ?? value.replace(/\s+/g, " ").trim();
    return text.length > max ? `${text.slice(0, max - 1)}…` : text;
  }
  if (typeof value === "number") return String(value);
  if (Array.isArray(value)) {
    if (value.length === 0) return "none";
    const names = (value as unknown[]).map((v) =>
      typeof v === "object" && v !== null && "name" in v ? String(v.name) : String(v),
    );
    return show(names.join(", "), max);
  }
  return show(JSON.stringify(value), max);
}

/** One change, worded. */
export function describeChange(change: ObjectChange): string {
  const { field, before, after } = change as { field: string; before: unknown; after: unknown };
  switch (field) {
    case "name":
      return `renamed ${show(before)} → ${show(after)}`;
    case "schema":
      return `moved to schema ${show(after)}`;
    case "not_null":
      return after ? "made not null" : "made nullable";
    case "rls":
      return after ? "row level security enabled" : "row level security disabled";
    case "rls_forced":
      return after ? "row level security forced" : "row level security no longer forced";
    case "unlogged":
      return after ? "made unlogged" : "made logged";
    case "permissive":
      return after ? "made permissive" : "made restrictive";
    case "position":
      return `moved to position ${Number(after) + 1}`;
    case "comment":
      return before === null
        ? `comment added`
        : after === null
          ? "comment removed"
          : "comment changed";
    case "view_definition":
      return "definition changed";
    case "labels": {
      const was = new Set(before as string[]);
      const now = new Set(after as string[]);
      const added = (after as string[]).filter((l) => !was.has(l));
      const removed = (before as string[]).filter((l) => !now.has(l));
      const parts = [
        added.length > 0 && `labels ${added.map((l) => `+${l}`).join(", ")}`,
        removed.length > 0 &&
          `${added.length ? "" : "labels "}${removed.map((l) => `−${l}`).join(", ")}`,
      ].filter(Boolean);
      return parts.length ? parts.join(", ") : "labels reordered";
    }
    case "attributes":
      return `attributes ${show(before)} → ${show(after)}`;
    default:
      return `${LABELS[field] ?? field.replace(/_/g, " ")} ${show(before)} → ${show(after)}`;
  }
}

/** Every event of an object, oldest first. */
export function eventsOf(states: readonly State<unknown>[]): HistoryEvent[] {
  const out: HistoryEvent[] = [];
  states.forEach((s, i) => {
    const before = states[i - 1];
    if (!before) out.push({ at: s.from, kind: "added", details: [] });
    else if (before.to !== s.from) out.push({ at: s.from, kind: "restored", details: [] });
    else if (s.changes.length > 0) {
      const renamed = s.changes.some((c) => c.field === "name");
      out.push({
        at: s.from,
        kind: renamed ? "renamed" : "changed",
        details: s.changes.map(describeChange),
      });
    }
    const next = states[i + 1];
    if (s.to !== null && (!next || next.from !== s.to))
      out.push({ at: s.to, kind: "removed", details: [] });
  });
  return out;
}

const VERB: Record<EventKind, string> = {
  added: "added",
  removed: "removed",
  changed: "changed",
  renamed: "renamed",
  restored: "added back",
};

/** Who made a version, in a word or two. */
export function authorOf(version: TimelineVersion): string | null {
  return version.author?.name ?? null;
}

/**
 * An event in a line: `v4 · added by Ada`, `v7 · type bigint → numeric`.
 * The hover card's lines (`TimeMark.history`).
 */
export function eventLine(event: HistoryEvent, versions: readonly TimelineVersion[]): string {
  const v = versions[event.at];
  const head = v ? `v${v.number}` : "?";
  const author = v ? authorOf(v) : null;
  const what = event.details.length > 0 ? event.details.join("; ") : VERB[event.kind];
  return `${head} · ${what}${author && event.details.length === 0 ? ` by ${author}` : ""}`;
}

/** An object's events as lines, oldest first, at most `max` (the latest kept). */
export function historyLines(
  states: readonly State<unknown>[],
  history: Pick<History, "versions">,
  max = 8,
): string[] {
  const events = eventsOf(states);
  const lines = events.map((e) => eventLine(e, history.versions));
  if (lines.length <= max) return lines;
  return [`${lines.length - max + 1} earlier events`, ...lines.slice(-(max - 1))];
}

export const EVENT_VERB = VERB;

/** `v7`, or `v7 (removed)` for an admin's removed version. */
export function versionName(v: TimelineVersion | undefined): string {
  if (!v) return "";
  return `v${v.number}${v.removed_at ? " (removed)" : ""}`;
}

/** "3 tables added, 1 removed · 5 columns changed": what the range did, start to end. */
export function summaryText(counts: Frame["counts"]): string {
  const part = (c: Counts, one: string, many: string) => {
    const entries = (
      [
        ["added", c.added],
        ["removed", c.removed],
        ["changed", c.changed],
      ] as const
    ).filter(([, k]) => k > 0);
    return entries
      .map(([word, k], i) => (i === 0 ? `${k} ${k === 1 ? one : many} ${word}` : `${k} ${word}`))
      .join(", ");
  };
  const parts = [part(counts.tables, "table", "tables"), part(counts.columns, "column", "columns")];
  return parts.filter(Boolean).join(" · ") || "No change";
}

/**
 * What the graph shows, in a line: "All 8 versions superposed, v4
 * highlighted", "The schema at v4". Superposed with nothing faded (the
 * cursor's version has all the range had), it says so: the other mode then
 * draws the same graph.
 */
export function modeCaption(
  mode: Frame["request"]["mode"],
  range: Frame["request"]["range"],
  at: number,
  versions: readonly TimelineVersion[],
  away?: Frame["away"],
): string {
  const name = (i: number) => versionName(versions[i]);
  if (mode === "cursor") return `The schema at ${name(at)}`;
  const n = range.to - range.from + 1;
  const what =
    n === 2
      ? `${name(range.from)} and ${name(range.to)}`
      : range.from === 0 && range.to === versions.length - 1
        ? `All ${n} versions`
        : `${n} versions (${name(range.from)} → ${name(range.to)})`;
  if (away && away.gone + away.later === 0)
    return `${what} superposed: all of it is in ${name(at)}`;
  return `${what} superposed, ${name(at)} highlighted`;
}
