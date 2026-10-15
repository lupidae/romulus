import * as React from "react";

import { ArrowRightIcon, ArrowSeparateVerticalIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip } from "@/components/ui/tooltip";
import { usePreference } from "@/features/schema/viewer/use-preference";
import type { Highlights, Range } from "@/features/schema/text/codemirror";
import { findStatements, type SqlStatement, statementAt } from "@/features/schema/text/statements";
import { cn } from "@/lib/utils";

import { type Locatable, rangesOf } from "../locate";
import type { DiffLayout, Side, SqlDiffEditor } from "../sql-diff/merge";

/** Below this width the two sides would be too narrow to read: one column instead. */
const SPLIT_MIN_WIDTH = 760;

export interface SqlTarget {
  item: Locatable;
  /** Scroll to it (false when the reader's own cursor picked it). */
  scroll: boolean;
}

interface SqlDiffProps {
  before: string;
  after: string;
  /** Each side's name: "v4", "v5". */
  labels: { before: string; after: string };
  /** The object shown: highlighted while set, scrolled to whenever a new target is passed. */
  target: SqlTarget | null;
  /** The reader's cursor entered a statement. */
  onCursor?: (statement: SqlStatement, side: Side) => void;
  /** Hidden panes skip work; a reveal waits until the pane is shown. */
  active?: boolean;
  /** More buttons at the end of the bar (hide the pane, full screen…). */
  actions?: React.ReactNode;
  /** Where the reader's choices (layout, folding) are remembered. */
  storageKey?: string;
  className?: string;
}

type LayoutChoice = "auto" | DiffLayout;
const isChoice = (v: unknown): v is LayoutChoice =>
  v === "auto" || v === "split" || v === "unified";
const isBool = (v: unknown): v is boolean => typeof v === "boolean";

const NO_HIGHLIGHTS: Highlights = { related: [] };
const highlightsOf = (ranges: Range[]): Highlights =>
  ranges.length ? { primary: ranges[0], related: ranges.slice(1) } : NO_HIGHLIGHTS;

/**
 * The whole schema's SQL before and after, aligned line for line: side by
 * side when there is room, one column with the deletions inline otherwise.
 * Long unchanged stretches fold; picking an object scrolls to its first change.
 */
export function SqlDiff({
  before,
  after,
  labels,
  target,
  onCursor,
  active = true,
  actions,
  storageKey = "romulus:sql-diff",
  className,
}: SqlDiffProps) {
  const host = React.useRef<HTMLDivElement>(null);
  const frame = React.useRef<HTMLDivElement>(null);
  const [choice, setChoice] = usePreference<LayoutChoice>(`${storageKey}:layout`, "auto", isChoice);
  const [collapse, setCollapse] = usePreference(`${storageKey}:fold`, true, isBool);
  const [width, setWidth] = React.useState(0);
  const [editor, setEditor] = React.useState<SqlDiffEditor | null>(null);

  // The pane's width decides "auto"; a hidden pane (width 0) keeps the last one.
  React.useLayoutEffect(() => {
    const el = frame.current;
    if (!el) return;
    const measure = () => {
      if (el.clientWidth > 0) setWidth(el.clientWidth);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const layout: DiffLayout =
    choice === "auto" ? (width >= SPLIT_MIN_WIDTH ? "split" : "unified") : choice;

  const statements = React.useMemo(
    () => ({ a: findStatements(before), b: findStatements(after) }),
    [before, after],
  );
  const latest = React.useRef({ onCursor, statements });
  React.useEffect(() => {
    latest.current = { onCursor, statements };
  });

  const ready = width > 0;
  React.useEffect(() => {
    if (!ready || !host.current) return;
    let created: SqlDiffEditor | null = null;
    let cancelled = false;
    void import("../sql-diff/merge").then(({ createSqlDiff }) => {
      if (cancelled || !host.current) return;
      const started = performance.now();
      created = createSqlDiff(host.current, {
        before,
        after,
        layout,
        collapse,
        labels: { a: `SQL of ${labels.before}`, b: `SQL of ${labels.after}` },
        onCursor: (side, offset) => {
          const statement = statementAt(latest.current.statements[side], offset);
          if (statement) latest.current.onCursor?.(statement, side);
        },
      });
      // Read by the perf spec, like the graph's marks.
      performance.measure(`sql diff: ${layout} view built`, { start: started });
      setEditor(created);
    });
    return () => {
      cancelled = true;
      created?.destroy();
      setEditor(null);
    };
  }, [ready, before, after, layout, collapse, labels.before, labels.after]);

  const ranges = React.useMemo(
    () =>
      target
        ? {
            a: rangesOf(before, statements.a, target.item),
            b: rangesOf(after, statements.b, target.item),
          }
        : null,
    [target, before, after, statements],
  );

  React.useEffect(() => {
    editor?.setHighlights(
      ranges
        ? { a: highlightsOf(ranges.a), b: highlightsOf(ranges.b) }
        : { a: NO_HIGHLIGHTS, b: NO_HIGHLIGHTS },
    );
  }, [editor, ranges]);

  // Once per target, when the pane can show it.
  const revealed = React.useRef<SqlTarget | null>(null);
  React.useEffect(() => {
    if (!editor) revealed.current = null;
  }, [editor]);
  React.useEffect(() => {
    if (!editor || !active || !ranges || !target?.scroll || revealed.current === target) return;
    revealed.current = target;
    editor.reveal(ranges);
  }, [editor, active, ranges, target]);

  return (
    <section
      aria-label="SQL diff"
      className={cn("flex h-full min-h-0 flex-col bg-card", className)}
    >
      <div className="flex h-11 shrink-0 items-center gap-1 border-b border-border pr-1.5 pl-3">
        <h2 className="flex min-w-0 items-center gap-1.5 text-sm font-medium text-foreground">
          SQL
          <span className="flex min-w-0 items-center gap-1 font-mono text-xs text-muted-foreground">
            <span className="truncate">{labels.before}</span>
            <ArrowRightIcon aria-hidden className="size-3 shrink-0" />
            <span className="sr-only">to</span>
            <span className="truncate">{labels.after}</span>
          </span>
        </h2>
        <div className="ml-auto flex items-center gap-1">
          <Tooltip
            content={
              choice === "auto"
                ? `Auto: ${layout === "split" ? "side by side" : "unified"} at this width`
                : "Diff layout"
            }
          >
            <ToggleGroup
              type="single"
              value={choice}
              onValueChange={(v) => v && setChoice(v as LayoutChoice)}
              aria-label="Diff layout"
              className="h-7.5"
            >
              <ToggleGroupItem value="auto" className="h-6.5 px-2">
                Auto
              </ToggleGroupItem>
              <ToggleGroupItem value="split" className="h-6.5 px-2" aria-label="Side by side">
                Split
              </ToggleGroupItem>
              <ToggleGroupItem value="unified" className="h-6.5 px-2">
                Unified
              </ToggleGroupItem>
            </ToggleGroup>
          </Tooltip>
          <Tooltip content={collapse ? "Show unchanged lines" : "Fold unchanged lines"}>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Fold unchanged lines"
              aria-pressed={collapse}
              onClick={() => setCollapse(!collapse)}
            >
              <ArrowSeparateVerticalIcon />
            </Button>
          </Tooltip>
          {actions}
        </div>
      </div>
      {layout === "split" && (
        <div
          aria-hidden
          className="grid h-7 shrink-0 grid-cols-2 border-b border-border bg-muted/50 text-xs text-muted-foreground"
        >
          <span className="flex items-center gap-1.5 truncate px-3">
            <span className="font-mono font-medium text-foreground">{labels.before}</span>
            before
          </span>
          <span className="flex items-center gap-1.5 truncate border-l border-border px-3">
            <span className="font-mono font-medium text-foreground">{labels.after}</span>
            after
          </span>
        </div>
      )}
      <div ref={frame} className="relative min-h-0 flex-1">
        <div ref={host} className="h-full" />
        {!editor && (
          <div
            aria-hidden
            className="absolute inset-0 grid content-start gap-2.5 bg-card px-4 py-3"
          >
            {[64, 88, 72, 40, 80, 56, 92, 48, 70, 36].map((w, i) => (
              <Skeleton key={i} className="h-3" style={{ width: `${w}%` }} />
            ))}
          </div>
        )}
      </div>
    </section>
  );
}
