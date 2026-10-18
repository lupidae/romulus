import * as React from "react";

import { cn } from "@/lib/utils";

import type { ConsoleColumn } from "../console";

interface ResultsGridProps {
  columns: ConsoleColumn[];
  rows: (string | null)[][];
  className?: string;
}

/** Values longer than this are shown cut (the full value is in the cell's title). */
const SHOWN = 300;

/**
 * A statement's rows as Postgres prints them: column names with their types,
 * NULL as a word, numbers aligned right, the header and row numbers pinned
 * while the grid scrolls both ways.
 */
export function ResultsGrid({ columns, rows, className }: ResultsGridProps) {
  return (
    <div
      className={cn("relative min-h-0 overflow-auto overscroll-contain", className)}
      role="region"
      aria-label={`Results, ${rows.length} ${rows.length === 1 ? "row" : "rows"}`}
      // A scrollable region is focusable, so the keyboard can scroll it.
      // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
      tabIndex={0}
    >
      <table className="border-separate border-spacing-0 font-mono text-sm">
        <thead>
          <tr>
            <th
              scope="col"
              className="sticky top-0 left-0 z-20 border-r border-b border-border bg-muted px-2 text-right text-xs font-normal text-faint-foreground"
            >
              <span className="sr-only">Row</span>
            </th>
            {columns.map((column, i) => (
              <th
                key={i}
                scope="col"
                className={cn(
                  "sticky top-0 z-10 border-r border-b border-border bg-muted px-3 py-1.5 text-left font-sans whitespace-nowrap",
                  column.numeric && "text-right",
                )}
              >
                <span className="block text-sm font-medium text-foreground">
                  {column.name || "?column?"}
                </span>
                <span className="block text-xs font-normal text-faint-foreground">
                  {column.type}
                </span>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, r) => (
            <Row key={r} index={r} row={row} columns={columns} />
          ))}
        </tbody>
      </table>
    </div>
  );
}

const Row = React.memo(function Row({
  index,
  row,
  columns,
}: {
  index: number;
  row: (string | null)[];
  columns: ConsoleColumn[];
}) {
  return (
    <tr className="hover:bg-accent/60">
      <td className="sticky left-0 border-r border-b border-border bg-card px-2 text-right text-xs text-faint-foreground tabular-nums">
        {index + 1}
      </td>
      {row.map((value, c) => (
        <td
          key={c}
          title={value !== null && value.length > 40 ? value.slice(0, 2000) : undefined}
          className={cn(
            "max-w-[28rem] truncate border-r border-b border-border px-3 py-1 whitespace-pre",
            columns[c]?.numeric && "text-right tabular-nums",
            value === null && "text-faint-foreground",
          )}
        >
          {value === null ? "NULL" : value.length > SHOWN ? `${value.slice(0, SHOWN)}…` : value}
        </td>
      ))}
    </tr>
  );
});
