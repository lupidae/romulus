import type { Extension } from "@codemirror/state";
import { type ReactNode, useEffect, useId, useMemo, useState } from "react";

import { CodeIcon, CollapseIcon, ExpandIcon, NetworkRightIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

import type { SchemaDiffInput } from "../diff";
import type { GraphEditor } from "../graph/editing/types";
import { graphOf } from "../graph/graph-of";
import { SchemaGraph, type SchemaGraphProps } from "../graph/schema-graph";
import type { Groups, Layout } from "../layout";
import { qualifiedName, type RelationRef, type Schema } from "../model";
import type { SchemaTexts } from "../text/formats";
import { SchemaText, type TextTab } from "../text/schema-text";
import type { SqlStatement } from "../text/statements";
import { type HiddenPane, LayoutMenu } from "./layout-menu";
import { DEFAULT_RATIO, MAX_RATIO, MIN_RATIO, Split } from "./split";
import { useMediaQuery, usePreference } from "./use-preference";

export interface SchemaViewerProps {
  /** The model drawn; in diff mode, the new side. */
  schema: Schema;
  /** Diff mode: the graph draws both sides, styled by status. */
  diff?: SchemaDiffInput;
  /** SQL and the pre-rendered JSON, DBML and Mermaid of `schema`. */
  texts: SchemaTexts;
  /** Statement ranges of `texts.sql`; scanned from it when absent. */
  statements?: SqlStatement[];
  layout?: Layout;
  groups?: Groups;
  /** Positions decided by the graph (new tables placed, tables moved): write them to the layout map. */
  onLayoutChange?: (positions: Layout) => void;
  /** Tables can be dragged. */
  editable?: boolean;
  /** SQL can be edited (with `sqlExtensions` binding it to a draft). */
  readOnly?: boolean;
  sqlExtensions?: Extension[];
  /** Drawn over the SQL without owning it (comment threads): see `SchemaText`. */
  sqlOverlay?: Extension[];
  /** Qualified name of the selected table; leave undefined to let the viewer own it. */
  selected?: string | null;
  onSelectedChange?: (key: string | null) => void;
  /** The text pane's tab: a format, or one of `textTabs`. */
  format?: string;
  onFormatChange?: (format: string) => void;
  /** The text pane's own tabs (a draft's migration). */
  textTabs?: TextTab[];
  /** The SQL tab's label, and why it is read-only (see `SchemaText`). */
  sqlTab?: { label?: string; locked?: string; notice?: ReactNode };
  /** A new value remounts the SQL editor. */
  sqlKey?: string;
  /** Where view preferences (split, hidden pane) are remembered, per browser. */
  storageKey?: string;
  /** Rendered inside the graph's flow (remote cursors…). */
  graphChildren?: ReactNode;
  /** More buttons in the graph's toolbar (a draft's groups). */
  graphControls?: ReactNode;
  /** Shown instead of the graph when the schema has no tables. */
  graphEmpty?: ReactNode;
  /** Tables picked for an action on several: see `SchemaGraph`. */
  picked?: readonly string[];
  onPickedChange?: (keys: string[]) => void;
  /** Schema editing from the graph (a draft's editors): see `SchemaGraph`. */
  editor?: GraphEditor;
  /** Where the graph goes whenever a new object is passed: see `SchemaGraph`'s `focus`. */
  graphFocus?: SchemaGraphProps["focus"];
  /** More buttons in the text pane's tab bar, before the layout menu. */
  textActions?: ReactNode;
  className?: string;
}

type Hidden = HiddenPane;
const isRatio = (v: unknown): v is number =>
  typeof v === "number" && v >= MIN_RATIO && v <= MAX_RATIO;
const isHidden = (v: unknown): v is Hidden => v === "none" || v === "graph" || v === "text";

/**
 * A schema as a graph and as text, side by side and in step: selecting a
 * table in one shows it in the other. The base of the version page, the diff
 * page and the draft editor. Below `md` the panes become tabs.
 */
export function SchemaViewer({
  schema,
  diff,
  texts,
  statements,
  layout,
  groups,
  onLayoutChange,
  editable,
  readOnly,
  sqlExtensions,
  sqlOverlay,
  selected: controlled,
  onSelectedChange,
  format,
  onFormatChange,
  textTabs,
  sqlTab,
  sqlKey,
  storageKey = "romulus:schema-viewer",
  graphChildren,
  graphControls,
  graphEmpty,
  picked,
  onPickedChange,
  editor,
  graphFocus,
  textActions,
  className,
}: SchemaViewerProps) {
  const graph = useMemo(() => graphOf(schema, diff), [schema, diff]);
  const [own, setOwn] = useState<string | null>(null);
  const selected = controlled !== undefined ? controlled : own;
  const select = (key: string | null) => {
    setOwn(key);
    onSelectedChange?.(key);
  };

  const [ratio, setRatio] = usePreference(`${storageKey}:split`, DEFAULT_RATIO, isRatio);
  const [hidden, setHidden] = usePreference<Hidden>(`${storageKey}:hidden`, "none", isHidden);
  const [mobilePane, setMobilePane] = useState<"graph" | "text">("graph");
  const [fullScreen, setFullScreen] = useState(false);
  const wide = useMediaQuery("(min-width: 48rem)");
  const uid = useId();

  // Esc leaves full screen, unless something on top (a dialog, the editor's
  // search, a selection in the graph) used it first.
  useEffect(() => {
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

  const selectedRef = useMemo<RelationRef | null>(() => {
    const t = selected ? graph.byKey.get(selected) : undefined;
    return t ? { schema: t.schema, name: t.name } : null;
  }, [graph, selected]);

  const resolve = (target: RelationRef): RelationRef | undefined => {
    const key = qualifiedName(target);
    const owner = graph.byKey.get(key) ?? graph.byKey.get(graph.partitionOwner.get(key) ?? "");
    return owner ? { schema: owner.schema, name: owner.name } : undefined;
  };

  const fullScreenButton = (
    <Tooltip
      content={fullScreen ? "Exit full screen" : "Full screen"}
      shortcut={fullScreen ? ["Esc"] : undefined}
    >
      <Button
        variant="ghost"
        size="icon-sm"
        aria-label={fullScreen ? "Exit full screen" : "Full screen"}
        aria-pressed={fullScreen}
        onClick={() => setFullScreen(!fullScreen)}
      >
        {fullScreen ? <CollapseIcon /> : <ExpandIcon />}
      </Button>
    </Tooltip>
  );

  const layoutMenu = (
    <LayoutMenu
      hidden={hidden}
      onHiddenChange={setHidden}
      fullScreen={fullScreen}
      onFullScreenChange={setFullScreen}
    />
  );

  const showGraph = !wide ? mobilePane === "graph" : hidden !== "graph";
  const showText = !wide ? mobilePane === "text" : hidden !== "text";

  const graphPane = (
    <SchemaGraph
      schema={schema}
      diff={diff}
      layout={layout}
      groups={groups}
      onLayoutChange={onLayoutChange}
      editable={editable}
      selected={selected}
      onSelectedChange={select}
      picked={picked}
      onPickedChange={onPickedChange}
      editor={editor}
      focus={graphFocus}
      empty={graphEmpty}
      controls={
        <>
          {graphControls}
          {wide && hidden === "text" && layoutMenu}
        </>
      }
    >
      {graphChildren}
    </SchemaGraph>
  );

  const textPane = (
    <SchemaText
      texts={texts}
      statements={statements}
      resolve={resolve}
      selected={selectedRef}
      onSelectedChange={(ref) => select(ref ? qualifiedName(ref) : null)}
      format={format}
      onFormatChange={onFormatChange}
      tabs={textTabs}
      sqlTab={sqlTab}
      sqlKey={sqlKey}
      readOnly={readOnly}
      sqlExtensions={sqlExtensions}
      sqlOverlay={sqlOverlay}
      multiSchema={graph.multiSchema}
      actions={
        <>
          {textActions}
          {wide && layoutMenu}
        </>
      }
    />
  );

  return (
    <div
      className={cn(
        "flex h-full min-h-0 w-full flex-col overflow-hidden bg-card",
        fullScreen && "fixed inset-0 z-(--z-overlay) h-dvh",
        className,
      )}
    >
      {!wide && (
        <div className="flex h-11 shrink-0 items-center gap-2 border-b border-border px-2">
          <Tabs
            value={mobilePane}
            onValueChange={(v) => setMobilePane(v as "graph" | "text")}
            className="flex-1"
          >
            <TabsList aria-label="Pane" className="grid w-full grid-cols-2">
              {/* The panes are the split's, kept mounted: the tabs point at them. */}
              <TabsTrigger value="graph" id={`${uid}-tab-graph`} aria-controls={`${uid}-graph`}>
                <NetworkRightIcon />
                Graph
              </TabsTrigger>
              <TabsTrigger value="text" id={`${uid}-tab-text`} aria-controls={`${uid}-text`}>
                <CodeIcon />
                SQL
              </TabsTrigger>
            </TabsList>
          </Tabs>
          {fullScreenButton}
        </div>
      )}
      <Split
        ratio={ratio}
        show={showGraph && showText ? "both" : showGraph ? "start" : "end"}
        onRatioChange={setRatio}
        label="Resize the graph and the text"
        start={graphPane}
        end={textPane}
        startProps={
          wide
            ? undefined
            : { id: `${uid}-graph`, role: "tabpanel", "aria-labelledby": `${uid}-tab-graph` }
        }
        endProps={
          wide
            ? undefined
            : { id: `${uid}-text`, role: "tabpanel", "aria-labelledby": `${uid}-tab-text` }
        }
        className="flex-1"
      />
    </div>
  );
}
