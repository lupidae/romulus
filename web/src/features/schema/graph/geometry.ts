// Table node sizes, computed rather than measured: the layout needs them
// before anything renders, the minimap needs explicit sizes on controlled
// nodes, and fixed row heights put each column's edge anchor at a known y.
// The node component reads the same constants.

import type { TableModel } from "./build";

export const NODE = {
  header: 40,
  comment: 20,
  row: 26,
  /** Above the first row and below the last. */
  padding: 4,
  footer: 30,
  partitionRow: 24,
  minWidth: 220,
  maxWidth: 380,
  /** Tables with more columns show the first `collapsedColumns` until expanded. */
  maxColumns: 24,
  collapsedColumns: 20,
} as const;

/** Room around a group's members for its tinted area, and above them for its label. */
export const GROUP_PADDING = { top: 60, side: 24, bottom: 24 } as const;

/** Below this zoom a node shows only its name: rows are unreadable, and 300 nodes stay cheap. */
export const OVERVIEW_ZOOM = 0.42;

/** JetBrains Mono advances 0.6em: 7.2px at 12px, 7.8px at 13px. */
const CHAR = 7.2;
const HEADER_CHAR = 7.8;

const ABBREVIATIONS: [RegExp, string][] = [
  [/^timestamp\((\d+)\) with time zone/, "timestamptz($1)"],
  [/^timestamp with time zone/, "timestamptz"],
  [/^timestamp\((\d+)\) without time zone/, "timestamp($1)"],
  [/^timestamp without time zone/, "timestamp"],
  [/^time with time zone/, "timetz"],
  [/^time without time zone/, "time"],
  [/^character varying/, "varchar"],
  [/^character\b/, "char"],
  [/^double precision/, "float8"],
  [/^bit varying/, "varbit"],
];

/** `format_type` spelled the way people write it, for the dense node rows. */
export function shortType(type: string): string {
  for (const [pattern, short] of ABBREVIATIONS)
    if (pattern.test(type)) return type.replace(pattern, short);
  return type;
}

export interface NodeState {
  /** All columns shown, not just the first `collapsedColumns`. */
  allColumns?: boolean;
  /** The partition list unfolded. */
  partitions?: boolean;
}

export function visibleColumnCount(table: TableModel, state: NodeState = {}): number {
  const n = table.columns.length;
  return n > NODE.maxColumns && !state.allColumns ? NODE.collapsedColumns : n;
}

export function hasFooter(table: TableModel): boolean {
  return footerRows(table) > 0;
}

/** "N more columns" and "N partitions", each one row of the footer. */
export function footerRows(table: TableModel): number {
  return (table.partitions.length > 0 ? 1 : 0) + (table.columns.length > NODE.maxColumns ? 1 : 0);
}

/** Views have no NOT NULL: their columns are all nullable, so the mark says nothing. */
export function showsNullable(table: TableModel): boolean {
  return table.kind !== "view" && table.kind !== "materialized";
}

const KIND_BADGE: Record<TableModel["kind"], number> = {
  table: 0,
  partitioned: 11,
  view: 4,
  materialized: 12,
  foreign: 7,
};

export function tableSize(
  table: TableModel,
  multiSchema: boolean,
  state: NodeState = {},
): { width: number; height: number } {
  const name = (multiSchema ? table.schema.length + 1 : 0) + table.name.length;
  const chip = multiSchema ? 16 : 0;
  const kind = KIND_BADGE[table.kind] ? KIND_BADGE[table.kind] * 6.6 + 20 : 0;
  const status = table.status ? 72 : 0;
  let width = 12 + chip + 16 + 8 + name * HEADER_CHAR + kind + status + 12 + 8;
  const nullable = showsNullable(table);
  const diffGutter = table.columns.some((c) => c.status) ? 4 : 0;
  for (const c of table.columns) {
    const marks = (c.unique ? 1 : 0) + (c.identity ? 1 : 0) + (c.generated ? 1 : 0);
    const type = c.display.length + (nullable && !c.notNull ? 1 : 0);
    const row =
      12 + 14 + 6 + c.name.length * CHAR + (marks ? 6 + marks * 16 : 0) + 18 + type * CHAR + 12;
    width = Math.max(width, row + diffGutter + 8);
  }
  width = Math.round(Math.min(NODE.maxWidth, Math.max(NODE.minWidth, width)));

  const rows = visibleColumnCount(table, state);
  let height =
    NODE.header + (table.comment ? NODE.comment : 0) + NODE.padding * 2 + rows * NODE.row;
  height += footerRows(table) * NODE.footer;
  if (state.partitions) height += table.partitions.length * NODE.partitionRow + NODE.padding;
  return { width, height };
}

/** Where a column's edge attaches, from the top of the node. */
export function columnAnchorY(
  table: TableModel,
  column: string | undefined,
  state: NodeState = {},
): number {
  const top = NODE.header + (table.comment ? NODE.comment : 0) + NODE.padding;
  const index = column ? table.columns.findIndex((c) => c.name === column) : -1;
  const visible = visibleColumnCount(table, state);
  if (index < 0) return NODE.header / 2;
  // A column folded away attaches to the "more columns" footer.
  if (index >= visible) return top + visible * NODE.row + NODE.padding + NODE.footer / 2;
  return top + index * NODE.row + NODE.row / 2;
}
