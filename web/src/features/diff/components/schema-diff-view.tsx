import * as React from "react";

import { CodeIcon, ListIcon, NetworkRightIcon } from "@/components/icons";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import type { SchemaDiffInput } from "@/features/schema/diff";
import { graphOf } from "@/features/schema/graph/graph-of";
import { SchemaGraph } from "@/features/schema/graph/schema-graph";
import type { Groups, Layout } from "@/features/schema/layout";
import { qualifiedName, type Schema } from "@/features/schema/model";
import type { SqlStatement } from "@/features/schema/text/statements";
import { type HiddenPane, LayoutMenu } from "@/features/schema/viewer/layout-menu";
import { MAX_RATIO, MIN_RATIO, Split } from "@/features/schema/viewer/split";
import { usePreference } from "@/features/schema/viewer/use-preference";
import { useShortcut } from "@/lib/use-shortcut";
import { cn } from "@/lib/utils";

import {
  type ChangeFilter,
  type ChangeItem,
  changeSections,
  filterSections,
  type SchemaDiff,
  spansSchemas,
} from "../changes";
import type { Locatable } from "../locate";
import { ChangeList } from "./change-list";
import { SqlDiff, type SqlTarget } from "./sql-diff";

export interface SchemaDiffViewProps {
  /** The old model and the new one: the graph draws their union. */
  before: Schema;
  after: Schema;
  /** romulus-diff's diff from `before` to `after`. */
  diff: SchemaDiff;
  /** Both models as SQL, rendered alike (same order, same header). */
  sql: { before: string; after: string };
  /** Positions and groups for the union's tables; the rest are placed automatically. */
  layout?: Layout;
  groups?: Groups;
  /** What each side is called: "v4" and "v5", or "v12" and "Draft". */
  labels: { before: string; after: string };
  /** The change selected first (a change id, see `ChangeItem.id`). */
  defaultSelected?: string | null;
  /** A change, or a table of the graph (`table:<schema>.<name>`), was selected. */
  onSelectedChange?: (id: string | null) => void;
  filter?: ChangeFilter;
  onFilterChange?: (filter: ChangeFilter) => void;
  /** `j` and `k` move through the changes; off in a dialog that has its own keys. */
  shortcuts?: boolean;
  /** Where view preferences (split, panes, SQL layout) are remembered. */
  storageKey?: string;
  className?: string;
}

/** Who picked the selection: the graph and the SQL do not move under the reader's own pick. */
type Source = "list" | "keys" | "graph" | "sql" | "initial";

interface Selection {
  item: ChangeItem | null;
  /** The graph's table: the item's, or one picked in the graph or the SQL that did not change. */
  table: string | null;
  source: Source;
}

type Pane = "changes" | "graph" | "sql";
const isRatio = (v: unknown): v is number =>
  typeof v === "number" && v >= MIN_RATIO && v <= MAX_RATIO;
const isHidden = (v: unknown): v is HiddenPane => v === "none" || v === "graph" || v === "text";

const RELATION_ID = /^(table|view|materialized_view|foreign_table):(.*)$/;

/** From this width the three panes sit side by side; below it they are tabs. */
const WIDE = 960;
/** From this width the change list gets more room. */
const ROOMY = 1200;

/** `ref`'s width, live; the window's until it is measured (before the first paint). */
function useWidth(ref: React.RefObject<HTMLElement | null>): number {
  const [width, setWidth] = React.useState(() =>
    typeof window === "undefined" ? WIDE : window.innerWidth,
  );
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const measure = () => {
      if (el.clientWidth > 0) setWidth(el.clientWidth);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

/**
 * Two schemas compared, three ways at once: the change list, the graph of
 * their union with what was added, removed or changed highlighted, and the
 * SQL side by side. Picking a change in the list selects and zooms to it in
 * the graph and scrolls the SQL to it; picking a table in the graph or a
 * statement in the SQL selects its change. Below `lg` the three are tabs.
 *
 * Self-contained (no router, no queries): the diff page renders it, and so
 * can a draft's publish dialog.
 */
export function SchemaDiffView({
  before,
  after,
  diff,
  sql,
  layout,
  groups,
  labels,
  defaultSelected = null,
  onSelectedChange,
  filter: controlledFilter,
  onFilterChange,
  shortcuts = true,
  storageKey = "romulus:diff-view",
  className,
}: SchemaDiffViewProps) {
  const sections = React.useMemo(() => changeSections(diff), [diff]);
  const items = React.useMemo(() => sections.flatMap((s) => s.items), [sections]);
  const byTable = React.useMemo(
    () => new Map(items.filter((i) => i.table).map((i) => [i.table!, i])),
    [items],
  );
  const graphDiff = React.useMemo<SchemaDiffInput>(
    () => ({ base: before, status: diff.status }),
    [before, diff.status],
  );
  const graph = graphOf(after, graphDiff);
  const multiSchema = graph.multiSchema || spansSchemas(sections);

  const [ownFilter, setOwnFilter] = React.useState<ChangeFilter>("all");
  const filter = controlledFilter ?? ownFilter;
  const setFilter = (next: ChangeFilter) => {
    setOwnFilter(next);
    onFilterChange?.(next);
  };
  const visible = React.useMemo(
    () => filterSections(sections, filter).flatMap((s) => s.items),
    [sections, filter],
  );

  const [selection, setSelection] = React.useState<Selection>(() => {
    const item = items.find((i) => i.id === defaultSelected) ?? null;
    if (item) return { item, table: item.table ?? null, source: "initial" };
    const table = RELATION_ID.exec(defaultSelected ?? "")?.[2];
    return {
      item: null,
      table: table && graph.byKey.has(table) ? table : null,
      source: "initial",
    };
  });
  const [focus, setFocus] = React.useState<{ key: string } | null>(() =>
    selection.table ? { key: selection.table } : null,
  );
  const [target, setTarget] = React.useState<SqlTarget | null>(() =>
    selection.item
      ? { item: selection.item, scroll: true }
      : selection.table
        ? { item: tableTarget(selection.table, graph.byKey), scroll: true }
        : null,
  );

  // Its own width, not the window's, decides the layout: the page gives it
  // the screen, a dialog much less.
  const root = React.useRef<HTMLDivElement>(null);
  const width = useWidth(root);
  const wide = width >= WIDE;
  const uid = React.useId();
  // On a phone the panes are the tabs' panels.
  const panel = (name: Pane) =>
    wide
      ? undefined
      : ({
          id: `${uid}-${name}`,
          role: "tabpanel",
          "aria-labelledby": `${uid}-tab-${name}`,
        } as const);
  const tab = (name: Pane) => ({ id: `${uid}-tab-${name}`, "aria-controls": `${uid}-${name}` });
  const [pane, setPane] = React.useState<Pane>("changes");
  const [hidden, setHidden] = usePreference<HiddenPane>(`${storageKey}:hidden`, "none", isHidden);
  const showGraph = wide ? hidden !== "graph" : pane === "graph";
  const showSql = wide ? hidden !== "text" : pane === "sql";

  // A pick made while the graph is hidden (another tab on a phone, "Text
  // only") zooms it when it is shown, not while it has no size.
  const [zoomOnShow, setZoomOnShow] = React.useState<string | null>(null);
  const [graphWasShown, setGraphWasShown] = React.useState(showGraph);
  if (graphWasShown !== showGraph) {
    setGraphWasShown(showGraph);
    if (showGraph && zoomOnShow) {
      setFocus({ key: zoomOnShow });
      setZoomOnShow(null);
    }
  }

  const select = (next: Selection) => {
    setSelection(next);
    const { item, table, source } = next;
    if (table && (source === "list" || source === "keys")) {
      if (showGraph) setFocus({ key: table });
      else setZoomOnShow(table);
    }
    setTarget(
      item
        ? { item, scroll: source !== "sql" }
        : table
          ? { item: tableTarget(table, graph.byKey), scroll: source !== "sql" }
          : null,
    );
    onSelectedChange?.(item?.id ?? (table ? `table:${table}` : null));
  };

  const pickItem = (item: ChangeItem, source: Source) => {
    select({ item, table: item.table ?? null, source });
    // On a phone, a pick from the list goes on to where it is drawn.
    if (!wide && source === "list") setPane(item.table ? "graph" : "sql");
  };

  const pickTable = (key: string | null, source: "graph" | "sql") => {
    if (!key) {
      if (source === "graph") select({ item: null, table: null, source });
      return;
    }
    select({ item: byTable.get(key) ?? null, table: key, source });
  };

  const onCursor = (statement: SqlStatement) => {
    const t = statement.target;
    if (!t) return;
    if (statement.kind === "type") {
      const item = items.find(
        (i) =>
          (i.category === "enum" || i.category === "domain" || i.category === "composite") &&
          i.schema === t.schema &&
          i.name === t.name,
      );
      if (item && item.id !== selection.item?.id) select({ item, table: null, source: "sql" });
      return;
    }
    const key = qualifiedName(t);
    const owner = graph.byKey.has(key) ? key : graph.partitionOwner.get(key);
    if (owner && owner !== selection.table) pickTable(owner, "sql");
  };

  const step = (by: 1 | -1) => {
    if (!visible.length) return;
    const at = selection.item ? visible.findIndex((i) => i.id === selection.item!.id) : -1;
    const next =
      at < 0 ? visible[by === 1 ? 0 : visible.length - 1]! : (visible[at + by] ?? visible[at]!);
    if (next.id !== selection.item?.id) pickItem(next, "keys");
  };
  useShortcut("j", () => step(1), shortcuts, { inGraph: true });
  useShortcut("k", () => step(-1), shortcuts, { inGraph: true });

  // ---- Panes ------------------------------------------------------------------
  const [ratio, setRatio] = usePreference(`${storageKey}:split`, 0.55, isRatio);
  const [fullScreen, setFullScreen] = React.useState(false);
  React.useEffect(() => {
    if (!fullScreen) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      if (event.target instanceof Element && event.target.closest(".cm-panel, [role=dialog]"))
        return;
      setFullScreen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [fullScreen]);

  const layoutMenu = wide && (
    <LayoutMenu
      hidden={hidden}
      onHiddenChange={setHidden}
      fullScreen={fullScreen}
      onFullScreenChange={setFullScreen}
    />
  );
  // React Flow cannot start in a hidden pane (no size, no first fit): on a
  // phone the graph is created the first time its tab is shown, then kept.
  const [graphWanted, setGraphWanted] = React.useState(showGraph);
  if (showGraph && !graphWanted) setGraphWanted(true);

  const list = (
    <ChangeList
      sections={sections}
      summary={diff.summary.text}
      filter={filter}
      onFilterChange={setFilter}
      selected={selection.item?.id ?? null}
      onSelect={(item) => pickItem(item, "list")}
      multiSchema={multiSchema}
      hints={shortcuts}
    />
  );

  const graphPane = graphWanted && (
    <SchemaGraph
      schema={after}
      diff={graphDiff}
      layout={layout}
      groups={groups}
      selected={selection.table}
      onSelectedChange={(key) => pickTable(key, "graph")}
      focus={focus}
      legend={false}
      controls={wide && hidden === "text" && layoutMenu}
    />
  );

  const sqlPane = (
    <SqlDiff
      before={sql.before}
      after={sql.after}
      labels={labels}
      target={target}
      onCursor={onCursor}
      active={showSql}
      actions={layoutMenu}
      storageKey={`${storageKey}:sql`}
    />
  );

  return (
    <div
      ref={root}
      className={cn(
        "flex h-full min-h-0 w-full flex-col overflow-hidden bg-card",
        fullScreen && "fixed inset-0 z-(--z-overlay) h-dvh",
        className,
      )}
    >
      {!wide && (
        <div className="flex h-11 shrink-0 items-center border-b border-border px-2">
          <Tabs value={pane} onValueChange={(v) => setPane(v as Pane)} className="flex-1">
            <TabsList aria-label="Pane" className="grid w-full grid-cols-3">
              <TabsTrigger value="changes" {...tab("changes")}>
                <ListIcon />
                Changes
                <span className="text-xs text-muted-foreground tabular-nums">{items.length}</span>
              </TabsTrigger>
              <TabsTrigger value="graph" {...tab("graph")}>
                <NetworkRightIcon />
                Graph
              </TabsTrigger>
              <TabsTrigger value="sql" {...tab("sql")}>
                <CodeIcon />
                SQL
              </TabsTrigger>
            </TabsList>
          </Tabs>
        </div>
      )}
      <div className="flex min-h-0 flex-1">
        {/* Every pane stays mounted when hidden: the graph keeps its view, the SQL its scroll. */}
        <div
          {...panel("changes")}
          hidden={!wide && pane !== "changes"}
          className={cn(
            "min-h-0 w-full shrink-0",
            wide && "border-r border-border",
            wide && (width >= ROOMY ? "w-80" : "w-72"),
          )}
        >
          {list}
        </div>
        <Split
          ratio={ratio}
          show={showGraph && showSql ? "both" : showGraph ? "start" : showSql ? "end" : "both"}
          onRatioChange={setRatio}
          label="Resize the graph and the SQL"
          start={graphPane}
          end={sqlPane}
          startProps={panel("graph")}
          endProps={panel("sql")}
          className={cn("min-w-0 flex-1", !wide && pane === "changes" && "hidden")}
        />
      </div>
    </div>
  );
}

/** A table of the graph, as something to find in the SQL. */
function tableTarget(
  key: string,
  tables: ReadonlyMap<string, { schema: string; name: string }>,
): Locatable {
  const t = tables.get(key);
  return { category: "table", schema: t?.schema ?? null, name: t?.name ?? key };
}
