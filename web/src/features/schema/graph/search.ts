// Ranking for the table search: names people type, matched the way they
// type them (prefix, word start, anywhere, then letters in order).

import type { TableModel } from "./build";

export interface SearchHit {
  table: TableModel;
  /** Set when the hit is a column of the table rather than the table itself. */
  column?: string;
  /** [start, end) of the match in the label, for highlighting. */
  match?: [number, number];
  score: number;
}

function rank(label: string, query: string): { score: number; match?: [number, number] } | null {
  const text = label.toLowerCase();
  if (text === query) return { score: 0, match: [0, label.length] };
  if (text.startsWith(query)) return { score: 1, match: [0, query.length] };
  const word = text.search(new RegExp(`[._]${query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}`));
  if (word >= 0) return { score: 2, match: [word + 1, word + 1 + query.length] };
  const at = text.indexOf(query);
  if (at >= 0) return { score: 3, match: [at, at + query.length] };
  // Letters in order: "ordit" finds order_items.
  let i = 0;
  for (const ch of text) if (ch === query[i]) i++;
  return i === query.length ? { score: 5 } : null;
}

/**
 * Tables first (by how well the name matches, then name), then columns
 * (`users.email`) when the query names a column. Empty query: every table.
 */
export function searchTables(tables: readonly TableModel[], raw: string, limit = 50): SearchHit[] {
  const query = raw.trim().toLowerCase();
  if (!query) return tables.slice(0, limit).map((table) => ({ table, score: 0 }));
  const hits: SearchHit[] = [];
  for (const table of tables) {
    const byName = rank(table.name, query);
    const byKey = query.includes(".") ? rank(table.key, query) : null;
    const best = byName ?? (byKey ? { score: byKey.score + 0.5 } : null);
    if (best) hits.push({ table, score: best.score, match: byName?.match });
  }
  // Equally good matches: the shorter name is the closer one (orders before order_items).
  hits.sort(
    (a, b) =>
      a.score - b.score ||
      a.table.name.length - b.table.name.length ||
      a.table.key.localeCompare(b.table.key),
  );
  if (hits.length < limit) {
    const columns: SearchHit[] = [];
    const [tablePart, columnPart] = query.includes(".")
      ? query.split(/\.(?=[^.]*$)/)
      : [null, query];
    for (const table of tables) {
      if (tablePart && !table.name.toLowerCase().includes(tablePart)) continue;
      for (const c of table.columns) {
        const r = columnPart ? rank(c.name, columnPart) : null;
        if (r && r.score <= 3)
          columns.push({ table, column: c.name, match: r.match, score: 10 + r.score });
      }
    }
    columns.sort(
      (a, b) =>
        a.score - b.score ||
        `${a.table.key}.${a.column}`.localeCompare(`${b.table.key}.${b.column}`),
    );
    hits.push(...columns.slice(0, limit - hits.length));
  }
  return hits.slice(0, limit);
}
