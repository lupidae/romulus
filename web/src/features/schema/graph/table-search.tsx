import { useId, useMemo, useRef, useState } from "react";

import { SearchIcon } from "@/components/icons";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { cn } from "@/lib/utils";

import type { GraphModel } from "./build";
import { searchTables } from "./search";
import { StatusBadge } from "./status";
import { KIND_ICON } from "./kind";

interface TableSearchProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  graph: GraphModel;
  /** A table (and, for a column hit, which column) was chosen. */
  onPick: (key: string, column?: string) => void;
}

function Highlight({ text, match }: { text: string; match?: [number, number] }) {
  if (!match) return <>{text}</>;
  return (
    <>
      {text.slice(0, match[0])}
      <mark className="bg-transparent font-semibold text-foreground">
        {text.slice(match[0], match[1])}
      </mark>
      {text.slice(match[1])}
    </>
  );
}

/**
 * Find a table by name (or a column, as `table.column`) and focus it: a
 * combobox in a dialog, arrow keys to move, Enter to go, Esc to come back.
 */
export function TableSearch({ open, onOpenChange, graph, onPick }: TableSearchProps) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const listId = useId();
  const listRef = useRef<HTMLUListElement>(null);
  const hits = useMemo(() => searchTables(graph.tables, query), [graph, query]);
  const current = Math.min(active, hits.length - 1);

  const pick = (index: number) => {
    const hit = hits[index];
    if (!hit) return;
    onOpenChange(false);
    onPick(hit.table.key, hit.column);
  };

  const move = (index: number) => {
    const next = Math.max(0, Math.min(hits.length - 1, index));
    setActive(next);
    // jsdom has no scrollIntoView.
    listRef.current?.children[next]?.scrollIntoView?.({ block: "nearest" });
  };

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "ArrowDown") move(current + 1);
    else if (event.key === "ArrowUp") move(current - 1);
    else if (event.key === "Home" && event.ctrlKey) move(0);
    else if (event.key === "End" && event.ctrlKey) move(hits.length - 1);
    else if (event.key === "Enter") pick(current);
    else return;
    event.preventDefault();
  };

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (!next) {
          setQuery("");
          setActive(0);
        }
      }}
    >
      <DialogContent hideClose className="max-w-lg gap-0 overflow-hidden p-0">
        <DialogTitle className="sr-only">Find a table</DialogTitle>
        <DialogDescription className="sr-only">
          Type a table or column name. Arrow keys move through the results, Enter shows the table.
        </DialogDescription>
        <div className="flex h-12 items-center gap-2.5 border-b border-border px-4">
          <SearchIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
          {/* A bare field: the dialog is the frame, the caret the focus. */}
          <Input
            role="combobox"
            aria-label="Table or column name"
            aria-controls={listId}
            aria-expanded={hits.length > 0}
            aria-autocomplete="list"
            aria-activedescendant={hits[current] ? `${listId}-${current}` : undefined}
            autoComplete="off"
            spellCheck={false}
            placeholder="Find a table or column…"
            value={query}
            onChange={(event) => {
              setQuery(event.target.value);
              setActive(0);
            }}
            onKeyDown={onKeyDown}
            className="h-full flex-1 rounded-none border-0 bg-transparent px-0 shadow-none hover:border-0 focus-visible:ring-0"
          />
          <Kbd>Esc</Kbd>
        </div>
        {hits.length > 0 ? (
          <ul
            ref={listRef}
            id={listId}
            role="listbox"
            aria-label="Tables"
            className="max-h-[min(60vh,24rem)] overflow-y-auto p-1.5"
          >
            {hits.map((hit, i) => {
              const Icon = KIND_ICON[hit.table.kind];
              const label = hit.column ?? hit.table.name;
              return (
                <li
                  key={`${hit.table.key}:${hit.column ?? ""}`}
                  id={`${listId}-${i}`}
                  role="option"
                  aria-selected={i === current}
                  onPointerMove={() => setActive(i)}
                  // Keys stay with the field (aria-activedescendant); a press
                  // must not take its focus away.
                  onMouseDown={(event) => {
                    event.preventDefault();
                    pick(i);
                  }}
                  className={cn(
                    "flex h-9 cursor-pointer items-center gap-2.5 rounded-md px-2.5 text-sm",
                    i === current ? "bg-highlight text-foreground" : "text-muted-foreground",
                  )}
                >
                  <Icon aria-hidden className="size-4 shrink-0" />
                  <span className="min-w-0 truncate font-mono text-foreground">
                    {graph.multiSchema && !hit.column && (
                      <span className="text-faint-foreground">{hit.table.schema}.</span>
                    )}
                    {hit.column && <span className="text-faint-foreground">{hit.table.name}.</span>}
                    <Highlight text={label} match={hit.match} />
                  </span>
                  <span className="ml-auto flex shrink-0 items-center gap-2 text-xs text-faint-foreground">
                    {hit.table.status && <StatusBadge status={hit.table.status} />}
                    {hit.column ? "column" : `${hit.table.columns.length} columns`}
                  </span>
                </li>
              );
            })}
          </ul>
        ) : (
          <p className="px-4 py-10 text-center text-base text-muted-foreground">
            No table or column matches “{query.trim()}”.
          </p>
        )}
        <p aria-live="polite" className="sr-only">
          {query ? `${hits.length} ${hits.length === 1 ? "result" : "results"}` : ""}
        </p>
        <div className="flex h-9 items-center gap-3 border-t border-border bg-muted/60 px-4 text-xs text-muted-foreground">
          <span className="flex items-center gap-1">
            <Kbd>↑</Kbd>
            <Kbd>↓</Kbd> to move
          </span>
          <span className="flex items-center gap-1">
            <Kbd>↵</Kbd> to show
          </span>
          <span className="ml-auto">
            {graph.tables.length} {graph.tables.length === 1 ? "table" : "tables"}
          </span>
        </div>
      </DialogContent>
    </Dialog>
  );
}
