// The change list: romulus-diff's output as one flat, ordered list of
// objects (a table, an enum…), each with its status and its lines, grouped
// the way the summary counts them.

import type { components } from "@/api/schema.gen";
import type { ChangeStatus } from "@/features/schema/diff";
import { qualifiedName } from "@/features/schema/model";

export type SchemaDiff = components["schemas"]["SchemaDiff"];
type Category = components["schemas"]["Category"];
type EntityKind = components["schemas"]["EntityKind"];

/** A list section: what the summary counts, in its order. */
export interface ChangeSection {
  category: Category;
  /** "Tables", "Enums"… */
  title: string;
  items: ChangeItem[];
}

export interface ChangeItem {
  /** Stable and unique in a diff: `table:public.users`, `enum:shop.status`. */
  id: string;
  category: Category;
  /** What it is, in a word or two: "table", "partitioned table", "enum". */
  kind: string;
  /** `null` for extensions, which are database-wide. */
  schema: string | null;
  name: string;
  status: ChangeStatus;
  /** One line per change, as romulus-diff words them (`code` in backticks). */
  lines: string[];
  /** The graph node it is, for tables and views: its qualified name. */
  table?: string;
}

const TITLES: Record<Category, string> = {
  table: "Tables",
  view: "Views",
  materialized_view: "Materialized views",
  foreign_table: "Foreign tables",
  partition: "Partitions",
  enum: "Enums",
  domain: "Domains",
  composite: "Composite types",
  extension: "Extensions",
};

const ORDER: Category[] = [
  "table",
  "view",
  "materialized_view",
  "foreign_table",
  "partition",
  "enum",
  "domain",
  "composite",
  "extension",
];

const ENTITY: Record<EntityKind, { category: Category; kind: string }> = {
  table: { category: "table", kind: "table" },
  partitioned_table: { category: "table", kind: "partitioned table" },
  view: { category: "view", kind: "view" },
  materialized_view: { category: "materialized_view", kind: "materialized view" },
  foreign_table: { category: "foreign_table", kind: "foreign table" },
};

const changed = (status: string): status is ChangeStatus =>
  status === "added" || status === "removed" || status === "changed";

/**
 * Every changed object of `diff`, by section (in the summary's order), then
 * as romulus-diff sorts them (schema, then name). Sections without a change
 * are left out.
 */
export function changeSections(diff: SchemaDiff): ChangeSection[] {
  const items: ChangeItem[] = [];
  for (const e of diff.entities) {
    if (!changed(e.status)) continue;
    const { category, kind } = ENTITY[e.kind];
    const table = qualifiedName(e);
    items.push({
      id: `${category}:${table}`,
      category,
      kind,
      schema: e.schema,
      name: e.name,
      status: e.status,
      lines: e.lines,
      table,
    });
  }
  // Orphan partitions share one set of lines: each gets those naming it.
  const orphans = diff.orphan_partitions;
  for (const p of orphans.partitions) {
    if (!changed(p.status)) continue;
    const name = `${p.schema}.${p.name}`;
    const mine = orphans.lines.filter((l) => l.includes(`\`${name}\``));
    items.push({
      id: `partition:${name}`,
      category: "partition",
      kind: "partition",
      schema: p.schema,
      name: p.name,
      status: p.status,
      lines: mine.length ? mine : orphans.lines,
    });
  }
  const typed = (
    category: Category,
    kind: string,
    list: { schema: string; name: string; status: string; lines: string[] }[],
  ) => {
    for (const t of list) {
      if (!changed(t.status)) continue;
      items.push({
        id: `${category}:${t.schema}.${t.name}`,
        category,
        kind,
        schema: t.schema,
        name: t.name,
        status: t.status,
        lines: t.lines,
      });
    }
  };
  typed("enum", "enum", diff.enums);
  typed("domain", "domain", diff.domains);
  typed("composite", "composite type", diff.composites);
  for (const x of diff.extensions) {
    if (!changed(x.status)) continue;
    items.push({
      id: `extension:${x.name}`,
      category: "extension",
      kind: "extension",
      schema: null,
      name: x.name,
      status: x.status,
      lines: x.lines,
    });
  }

  return ORDER.map((category) => ({
    category,
    title: TITLES[category],
    items: items.filter((i) => i.category === category),
  })).filter((s) => s.items.length > 0);
}

export type ChangeFilter = "all" | ChangeStatus;

export function filterSections(sections: ChangeSection[], filter: ChangeFilter): ChangeSection[] {
  if (filter === "all") return sections;
  return sections
    .map((s) => ({ ...s, items: s.items.filter((i) => i.status === filter) }))
    .filter((s) => s.items.length > 0);
}

export type StatusCounts = Record<ChangeStatus, number>;

export function statusCounts(sections: ChangeSection[]): StatusCounts {
  const counts: StatusCounts = { added: 0, removed: 0, changed: 0 };
  for (const s of sections) for (const i of s.items) counts[i.status]++;
  return counts;
}

/**
 * A line worth showing under its object: an added or removed object's lone
 * line ("table `x` added") says what its status badge already says.
 */
export function detailLines(item: ChangeItem): string[] {
  if (item.status !== "changed" && item.lines.length === 1) return [];
  return item.lines;
}

/** The object's name as the list shows it: qualified when the diff spans schemas. */
export function displayName(item: ChangeItem, multiSchema: boolean): string {
  return multiSchema && item.schema ? `${item.schema}.${item.name}` : item.name;
}

/** Whether objects of the diff live in more than one schema. */
export function spansSchemas(sections: ChangeSection[]): boolean {
  const schemas = new Set<string>();
  for (const s of sections) for (const i of s.items) if (i.schema) schemas.add(i.schema);
  return schemas.size > 1;
}

/** A romulus-diff line in parts: plain text, and `code` between backticks. */
export function lineParts(line: string): { text: string; code: boolean }[] {
  const parts: { text: string; code: boolean }[] = [];
  const re = /`([^`]*)`/g;
  let at = 0;
  for (let m = re.exec(line); m; m = re.exec(line)) {
    if (m.index > at) parts.push({ text: line.slice(at, m.index), code: false });
    parts.push({ text: m[1]!, code: true });
    at = m.index + m[0].length;
  }
  if (at < line.length) parts.push({ text: line.slice(at), code: false });
  return parts;
}
