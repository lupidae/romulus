import * as React from "react";

import { Button } from "@/components/ui/button";
import { Kbd } from "@/components/ui/kbd";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip } from "@/components/ui/tooltip";
import { type ChangeStatus, STATUS_LABEL, statusGlyph } from "@/features/schema/diff";
import { cn } from "@/lib/utils";

import {
  type ChangeFilter,
  type ChangeItem,
  type ChangeSection,
  detailLines,
  displayName,
  filterSections,
  lineParts,
  statusCounts,
} from "../changes";

/** Lines shown under an object until it is selected. */
const PREVIEW_LINES = 4;

const GLYPH: Record<ChangeStatus, string> = {
  added: "bg-success/12 text-success",
  removed: "bg-destructive/12 text-destructive",
  changed: "bg-warning/14 text-warning",
};

/** `+`, `−` or `~` in a tinted square; the word goes to assistive tech. */
export function StatusGlyph({ status, className }: { status: ChangeStatus; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        "flex size-4 shrink-0 items-center justify-center rounded-xs font-mono text-xs leading-none font-semibold",
        GLYPH[status],
        className,
      )}
    >
      {statusGlyph(status)}
    </span>
  );
}

/** A romulus-diff line: words, with names and SQL in code. */
export function ChangeLine({ line, className }: { line: string; className?: string }) {
  return (
    <span className={cn("block text-sm wrap-anywhere text-muted-foreground", className)}>
      {lineParts(line).map((part, i) =>
        part.code ? (
          <code key={i} className="font-mono text-xs text-foreground">
            {part.text}
          </code>
        ) : (
          <React.Fragment key={i}>{part.text}</React.Fragment>
        ),
      )}
    </span>
  );
}

interface ChangeListProps {
  sections: ChangeSection[];
  /** romulus-diff's summary sentence. */
  summary: string;
  filter: ChangeFilter;
  onFilterChange: (filter: ChangeFilter) => void;
  /** The selected object's id. */
  selected: string | null;
  onSelect: (item: ChangeItem) => void;
  /** Names carry their schema. */
  multiSchema: boolean;
  /** Keyboard hints at the bottom (pages with the shortcuts). */
  hints?: boolean;
  className?: string;
}

const FILTERS: { value: ChangeFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "added", label: "Added" },
  { value: "changed", label: "Changed" },
  { value: "removed", label: "Removed" },
];

/**
 * What changed, object by object, grouped as the summary counts them: a
 * filter by status, then each object with its lines. Picking one shows it in
 * the graph and the SQL.
 */
export function ChangeList({
  sections,
  summary,
  filter,
  onFilterChange,
  selected,
  onSelect,
  multiSchema,
  hints = false,
  className,
}: ChangeListProps) {
  const counts = statusCounts(sections);
  const total = counts.added + counts.removed + counts.changed;
  const shown = filterSections(sections, filter);
  const list = React.useRef<HTMLDivElement>(null);
  const headingId = React.useId();

  // The selection, wherever it came from (the graph, j/k), stays in sight;
  // and focus with it, when focus was in the list.
  React.useEffect(() => {
    if (!selected) return;
    const el = list.current?.querySelector<HTMLElement>(`[data-change="${CSS.escape(selected)}"]`);
    if (!el) return;
    el.scrollIntoView({ block: "nearest" });
    if (list.current?.contains(document.activeElement) && document.activeElement !== el)
      el.focus({ preventScroll: true });
  }, [selected, filter]);

  // Up and down move between the objects, as in any list.
  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const buttons = [...(list.current?.querySelectorAll<HTMLButtonElement>("[data-change]") ?? [])];
    const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = buttons[at + (event.key === "ArrowDown" ? 1 : -1)];
    if (!next) return;
    event.preventDefault();
    next.focus();
  };

  return (
    <section
      aria-labelledby={headingId}
      className={cn("flex h-full min-h-0 flex-col bg-card", className)}
    >
      <header className="grid shrink-0 gap-2.5 border-b border-border px-3 pt-3 pb-3">
        <div className="grid gap-0.5">
          <h2 id={headingId} className="flex items-baseline gap-2 text-base font-semibold">
            Changes
            <span className="text-sm font-normal text-muted-foreground tabular-nums">{total}</span>
          </h2>
          <p className="text-sm text-muted-foreground first-letter:uppercase">{summary}</p>
        </div>
        <ToggleGroup
          type="single"
          value={filter}
          onValueChange={(v) => v && onFilterChange(v as ChangeFilter)}
          aria-label="Show"
          className="grid w-full grid-cols-4"
        >
          {FILTERS.map((f) => {
            const count = f.value === "all" ? total : counts[f.value];
            const item = (
              <ToggleGroupItem
                key={f.value}
                value={f.value}
                disabled={count === 0 && f.value !== filter}
                aria-label={`${f.label}: ${count}`}
                className="px-1.5"
              >
                {f.value === "all" ? (
                  <span>All</span>
                ) : (
                  <StatusGlyph status={f.value} className="bg-transparent" />
                )}
                <span className="tabular-nums">{count}</span>
              </ToggleGroupItem>
            );
            return f.value === "all" ? (
              item
            ) : (
              <Tooltip key={f.value} content={`${f.label} only`}>
                {item}
              </Tooltip>
            );
          })}
        </ToggleGroup>
      </header>
      {/* The list's own keys (up and down): a group of buttons, not a widget. */}
      {/* eslint-disable-next-line jsx-a11y/no-static-element-interactions */}
      <div
        ref={list}
        onKeyDown={onKeyDown}
        className="min-h-0 flex-1 [scrollbar-gutter:stable] overflow-y-auto overscroll-contain pb-2"
      >
        {shown.length === 0 ? (
          <div className="grid justify-items-start gap-2 px-3 py-4 text-sm text-muted-foreground">
            <p>Nothing {filter === "all" ? "changed" : filter}.</p>
            {filter !== "all" && (
              <Button variant="link" size="sm" onClick={() => onFilterChange("all")}>
                Show every change
              </Button>
            )}
          </div>
        ) : (
          shown.map((section) => (
            <section key={section.category} aria-label={section.title}>
              <h3 className="sticky top-0 z-1 flex items-baseline gap-1.5 bg-card/95 px-3 pt-3 pb-1 text-xs font-medium text-muted-foreground backdrop-blur-sm">
                {section.title}
                <span className="text-faint-foreground tabular-nums">{section.items.length}</span>
              </h3>
              <ul className="grid px-1.5">
                {section.items.map((item) => (
                  <li key={item.id}>
                    <ChangeRow
                      item={item}
                      selected={item.id === selected}
                      multiSchema={multiSchema}
                      onSelect={() => onSelect(item)}
                    />
                  </li>
                ))}
              </ul>
            </section>
          ))
        )}
      </div>
      {hints && (
        <footer className="hidden shrink-0 items-center gap-x-4 gap-y-1 border-t border-border px-3 py-2 text-xs whitespace-nowrap text-muted-foreground pointer-fine:flex">
          <span className="flex items-center gap-1">
            <Kbd>J</Kbd>
            <Kbd>K</Kbd>
            <span className="ml-0.5">changes</span>
          </span>
          <span className="flex items-center gap-1">
            <Kbd>[</Kbd>
            <Kbd>]</Kbd>
            <span className="ml-0.5">versions</span>
          </span>
        </footer>
      )}
    </section>
  );
}

function ChangeRow({
  item,
  selected,
  multiSchema,
  onSelect,
}: {
  item: ChangeItem;
  selected: boolean;
  multiSchema: boolean;
  onSelect: () => void;
}) {
  const lines = detailLines(item);
  const visible = selected ? lines : lines.slice(0, PREVIEW_LINES);
  const hidden = lines.length - visible.length;
  // Under Tables, the one kind worth naming: each other section has one kind.
  const kind = item.kind === "partitioned table" ? item.kind : null;
  const name = displayName(item, multiSchema);
  const linesId = React.useId();
  return (
    <Button
      variant="ghost"
      data-change={item.id}
      aria-pressed={selected}
      // Named by the object and its status; its lines describe it.
      aria-label={`${name}, ${STATUS_LABEL[item.status].toLowerCase()}${kind ? `, ${kind}` : ""}`}
      aria-describedby={lines.length ? linesId : undefined}
      onClick={onSelect}
      className={cn(
        "grid h-auto w-full grid-cols-[1rem_minmax(0,1fr)] items-start justify-stretch gap-x-2 gap-y-0.5 px-1.5 py-1.5 text-left font-normal whitespace-normal sm:h-auto",
        // A finger needs 44px: rows grow on touch screens.
        "pointer-coarse:min-h-11 pointer-coarse:py-3",
        "hover:bg-accent/60 focus-visible:ring-offset-0 focus-visible:ring-inset",
        selected && "bg-accent hover:bg-accent",
      )}
    >
      <StatusGlyph status={item.status} className="mt-0.5" />
      <span className="flex min-w-0 items-baseline gap-2">
        <span
          className={cn(
            "truncate font-mono text-sm font-medium text-foreground",
            item.status === "removed" && "text-muted-foreground line-through decoration-1",
          )}
        >
          {name}
        </span>
        {kind && <span className="shrink-0 text-xs text-faint-foreground">{kind}</span>}
      </span>
      {lines.length > 0 && (
        <span id={linesId} className="col-start-2 grid gap-0.5">
          {visible.map((line, i) => (
            <ChangeLine key={i} line={line} />
          ))}
          {hidden > 0 && (
            <span className="text-xs text-faint-foreground">
              {hidden} more {hidden === 1 ? "change" : "changes"}
            </span>
          )}
        </span>
      )}
    </Button>
  );
}
