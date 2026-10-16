import type { Extension } from "@codemirror/state";
import { Fragment, type ReactNode, useMemo, useRef, useState } from "react";

import {
  CheckIcon,
  CodeIcon,
  CopyIcon,
  InfoCircleIcon,
  LockIcon,
  SearchIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { modKey } from "@/lib/platform";
import { cn } from "@/lib/utils";

import type { RelationRef } from "../model";
import { CodeView } from "./code-view";
import type { Editor, Highlights, Range } from "./codemirror";
import { type SchemaTexts, TEXT_FORMATS, type TextFormat } from "./formats";
import { locateInDbml, locateInJson, locateInMermaid } from "./locate";
import { findStatements, type SqlStatement, statementAt } from "./statements";

/** A tab of the pane's own beside the formats (a draft's migration). */
export interface TextTab {
  id: string;
  label: string;
  /** Before the formats, or right after the SQL. */
  position: "start" | "after-sql";
  /** Shown read-only, with this reason in the tab's tooltip. */
  locked?: string;
  /** The tab's content; `active` while it is the one shown (it stays mounted). */
  render: (active: boolean) => ReactNode;
  /** In the tab bar while it is shown, instead of search and copy. */
  actions?: ReactNode;
}

export interface SchemaTextProps {
  texts: SchemaTexts;
  /** Statement ranges of `texts.sql` (libpg_query's, in a draft). Default: scanned from the text. */
  statements?: SqlStatement[];
  /**
   * The table a statement belongs to: partitions to the table they are drawn
   * under, unknown relations to nothing. Default: the statement's own target.
   */
  resolve?: (target: RelationRef) => RelationRef | undefined;
  /** The selected table: its statements are highlighted and scrolled to. */
  selected?: RelationRef | null;
  /** The cursor entered a statement of another table. */
  onSelectedChange?: (table: RelationRef | null) => void;
  /** The tab shown: a format, or one of `tabs`. */
  format?: string;
  onFormatChange?: (format: string) => void;
  /** Tabs of the pane's own (a draft's migration). */
  tabs?: TextTab[];
  /**
   * The SQL tab's label (`SQL`), why it is read-only when it is shown so,
   * and a line above it saying what it is.
   */
  sqlTab?: { label?: string; locked?: string; notice?: ReactNode };
  /** A new value remounts the SQL editor (new `sqlExtensions`, or none any more). */
  sqlKey?: string;
  /** Read-only by default; a draft passes false with its Yjs binding in `sqlExtensions`. */
  readOnly?: boolean;
  /**
   * Extra CodeMirror extensions for the SQL editor, read once when it is
   * created (memoize them). With a binding here (`yCollab`), the binding owns
   * the text: `texts.sql` only seeds the editor and must match the Y.Text.
   */
  sqlExtensions?: Extension[];
  /** Mermaid node names carry the schema when the model spans several. */
  multiSchema?: boolean;
  /** More buttons at the end of the tab bar (hide the pane, full screen…). */
  actions?: ReactNode;
  className?: string;
}

const NO_TABS: TextTab[] = [];

const same = (a: RelationRef | null | undefined, b: RelationRef | null | undefined) =>
  !!a && !!b && a.schema === b.schema && a.name === b.name;

/**
 * The schema as text: SQL (canonical, Postgres-highlighted) and its JSON,
 * DBML and Mermaid renderings, read-only, with line numbers, search, copy, and
 * selection kept in step with the graph through statement ranges.
 */
export function SchemaText({
  texts,
  statements: given,
  resolve = (target) => target,
  selected = null,
  onSelectedChange,
  format: controlled,
  onFormatChange,
  tabs = NO_TABS,
  sqlTab,
  sqlKey,
  readOnly = true,
  sqlExtensions,
  multiSchema = false,
  actions,
  className,
}: SchemaTextProps) {
  const [own, setOwn] = useState<string>("sql");
  const format = controlled ?? own;
  const setFormat = (next: string) => {
    setOwn(next);
    onFormatChange?.(next);
  };
  const statements = useMemo(() => given ?? findStatements(texts.sql), [given, texts.sql]);
  const editors = useRef(new Map<TextFormat, Editor | null>());
  const [copied, setCopied] = useState(false);
  // The table this pane's own cursor selected: no scrolling to it, the
  // reader is already there. Forgotten once the selection moves elsewhere.
  const [cursorPick, setCursorPick] = useState<RelationRef | null>(null);
  if (cursorPick && !same(cursorPick, selected)) setCursorPick(null);

  // Statements of the selected table: its CREATE first, the rest related.
  const sqlHighlights = useMemo<Highlights>(() => {
    if (!selected) return { related: [] };
    const mine = statements.filter(
      (s) => s.kind !== "type" && s.target && same(resolve(s.target), selected),
    );
    const primary = mine.find((s) => s.kind === "create" && same(s.target, selected)) ?? mine[0];
    return { primary, related: mine.filter((s) => s !== primary) };
    // `resolve` is usually an inline function; the statements and selection decide.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [statements, selected]);

  // Scroll targets: new objects when the selection changes, so each tab jumps
  // once, and not when the selection came from this pane's own cursor. The
  // SQL's only when the selected table changes: not on every edit of the
  // text (a draft's: someone typing, a graph edit), which would pull the
  // reader back to the table's CREATE each time.
  const selectedKey =
    selected && !same(selected, cursorPick) ? `${selected.schema}.${selected.name}` : null;
  const sqlReveal = useMemo(
    () => (selectedKey && sqlHighlights.primary ? { ...sqlHighlights.primary } : null),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- the selection decides, see above
    [selectedKey],
  );
  const reveals = useMemo(() => {
    const out: Partial<Record<TextFormat, Range | null>> = {};
    if (!selected || same(selected, cursorPick)) return out;
    out.sql = sqlReveal;
    if (texts.json) out.json = locateInJson(texts.json, selected) ?? null;
    if (texts.dbml) out.dbml = locateInDbml(texts.dbml, selected) ?? null;
    if (texts.mermaid) out.mermaid = locateInMermaid(texts.mermaid, selected, multiSchema) ?? null;
    return out;
  }, [selected, cursorPick, sqlReveal, texts, multiSchema]);

  const otherHighlights = useMemo(() => {
    const out: Partial<Record<TextFormat, Highlights>> = {};
    for (const f of ["json", "dbml", "mermaid"] as const) {
      const r = reveals[f];
      out[f] = { primary: r ?? undefined, related: [] };
    }
    return out;
  }, [reveals]);

  const onCursor = (offset: number) => {
    const statement = statementAt(statements, offset);
    if (!statement?.target || statement.kind === "type") return;
    const table = resolve(statement.target);
    if (!table || same(table, selected)) return;
    setCursorPick(table);
    onSelectedChange?.(table);
  };

  const extra = tabs.find((t) => t.id === format);
  const current = TEXT_FORMATS.find((f) => f.id === format) ?? TEXT_FORMATS[0]!;
  const text = extra ? undefined : texts[current.id];
  const missing = format === "sql" && selected && !sqlHighlights.primary;
  const sqlLabel = sqlTab?.label ?? "SQL";
  const labelOf = (f: (typeof TEXT_FORMATS)[number]) => (f.id === "sql" ? sqlLabel : f.label);
  const trigger = (id: string, label: string, locked: string | undefined) => {
    const node = (
      <TabsTrigger key={id} value={id} className="h-6.5 px-2">
        {label}
        {locked && <LockIcon aria-hidden className="size-3 text-faint-foreground" />}
      </TabsTrigger>
    );
    return locked ? (
      <Tooltip key={id} content={locked}>
        {node}
      </Tooltip>
    ) : (
      node
    );
  };

  const copy = async () => {
    if (text === undefined) return;
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
      toast.success(`${current.label} copied`);
    } catch {
      toast.error("Could not copy", {
        description: "The browser blocked access to the clipboard.",
      });
    }
  };

  return (
    <Tabs
      value={format}
      onValueChange={setFormat}
      className={cn("flex h-full min-h-0 flex-col bg-card", className)}
    >
      <div className="flex h-11 shrink-0 items-center gap-1 border-b border-border pr-1.5 pl-2">
        {/* Narrow panes scroll the tabs; the actions stay in reach. */}
        <div className="min-w-0 [scrollbar-width:none] overflow-x-auto">
          <TabsList aria-label="Format" className="h-7.5">
            {tabs
              .filter((t) => t.position === "start")
              .map((t) => trigger(t.id, t.label, t.locked))}
            {TEXT_FORMATS.map((f) => (
              <Fragment key={f.id}>
                {trigger(f.id, labelOf(f), f.id === "sql" ? sqlTab?.locked : undefined)}
                {f.id === "sql" &&
                  tabs
                    .filter((t) => t.position === "after-sql")
                    .map((t) => trigger(t.id, t.label, t.locked))}
              </Fragment>
            ))}
          </TabsList>
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-0.5">
          {extra ? (
            extra.actions
          ) : (
            <>
              <Tooltip content="Search" shortcut={[modKey, "F"]}>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={`Search the ${current.label}`}
                  disabled={text === undefined}
                  onClick={() => editors.current.get(current.id)?.openSearch()}
                >
                  <SearchIcon />
                </Button>
              </Tooltip>
              <Tooltip content={`Copy ${current.label}`}>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={`Copy the ${current.label}`}
                  disabled={text === undefined}
                  onClick={() => void copy()}
                >
                  {copied ? <CheckIcon className="text-success" /> : <CopyIcon />}
                </Button>
              </Tooltip>
            </>
          )}
          {actions}
        </div>
      </div>
      {format === "sql" && sqlTab?.notice}
      {missing && (
        <p className="flex h-8 shrink-0 items-center gap-2 border-b border-border bg-muted/60 px-3 text-sm text-muted-foreground">
          <InfoCircleIcon aria-hidden className="size-3.5 shrink-0" />
          <span className="truncate">
            No statement in this SQL defines{" "}
            <span className="font-mono text-foreground">{selected.name}</span>.
          </span>
        </p>
      )}
      {tabs.map((t) => (
        <TabsContent
          key={t.id}
          value={t.id}
          forceMount
          tabIndex={-1}
          className="mt-0 min-h-0 flex-1 data-[state=inactive]:hidden"
        >
          {t.render(format === t.id)}
        </TabsContent>
      ))}
      {TEXT_FORMATS.map((f) => (
        <TabsContent
          key={f.id}
          value={f.id}
          forceMount
          tabIndex={-1}
          className="mt-0 min-h-0 flex-1 data-[state=inactive]:hidden"
        >
          {texts[f.id] === undefined ? (
            <EmptyState
              icon={<CodeIcon />}
              title={`No ${f.label} for this schema`}
              description="It is rendered from the schema model, which this view did not receive."
            />
          ) : (
            <CodeView
              key={f.id === "sql" ? sqlKey : undefined}
              doc={texts[f.id]!}
              language={f.id}
              label={`Schema as ${labelOf(f)}`}
              readOnly={f.id === "sql" ? readOnly : true}
              extensions={f.id === "sql" ? sqlExtensions : undefined}
              highlights={f.id === "sql" ? sqlHighlights : otherHighlights[f.id]}
              reveal={reveals[f.id]}
              active={format === f.id}
              onCursor={f.id === "sql" ? onCursor : undefined}
              onEditor={(e) => editors.current.set(f.id, e)}
            />
          )}
        </TabsContent>
      ))}
    </Tabs>
  );
}
