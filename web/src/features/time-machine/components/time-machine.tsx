import * as React from "react";

import {
  ClockRotateRightIcon,
  FilterIcon,
  NavArrowDownIcon,
  NavArrowUpIcon,
  SearchIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import { useAutoLayout } from "@/features/schema/graph/use-auto-layout";
import { SchemaGraph } from "@/features/schema/graph/schema-graph";
import { groupColor } from "@/features/schema/layout";
import { cn } from "@/lib/utils";

import { Frames } from "../frame";
import { activity as activityOf, type History } from "../history";
import { keepPlaced, placedOnly, readPlaced } from "../layout-store";
import { carryOver, type TimeMachineState } from "../state";
import { useFrame, usePlayback } from "../use-frame";
import { HistoryPanel, type VersionLink } from "./history-panel";
import { summaryText } from "../describe";
import { CursorReadout, Legend, ModeToggle, Track, Transport } from "./timeline-bar";

export interface TimeMachineProps {
  history: History;
  initial: TimeMachineState;
  /** Every change of state, to keep the address in step (debounce it). */
  onStateChange?: (state: TimeMachineState, history: History) => void;
  /** A version as a link to what it changed (its diff). */
  VersionLink: VersionLink;
  /**
   * Where the positions computed for tables no version placed are kept in
   * this browser (the database's id): the first layout of a long history is
   * computed once. None: computed on every visit.
   */
  layoutKey?: string;
  /** More in the toolbar: compare the range, removed versions… */
  actions?: (state: TimeMachineState) => React.ReactNode;
  className?: string;
}

/** Whether a key press belongs to whatever has focus (a field, a menu, a dialog). */
function typing(target: EventTarget | null) {
  return (
    target instanceof HTMLElement &&
    (target.isContentEditable ||
      ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName) ||
      target.closest("[role=dialog], [role=menu], [role=listbox]") !== null)
  );
}

/**
 * The time machine: one graph over a range of versions, the timeline under
 * it. The graph superposes everything the range had (colored by when it
 * appeared, what is not at the cursor faded back), or shows exactly the
 * version at the cursor; the timeline sets the range, moves and plays the
 * cursor. Tables keep their place across versions: positions are computed
 * once, for every table the history ever had.
 *
 * The header speaks of the range (what it changed), the timeline's bar of
 * the cursor (its version, what the graph shows). Keys go through the
 * timeline as it reads: range start, range end, cursor, play, the mode, the
 * filters.
 */
export function TimeMachine({
  history,
  initial,
  onStateChange,
  VersionLink,
  layoutKey,
  actions,
  className,
}: TimeMachineProps) {
  const [state, setState] = React.useState(initial);
  // A version pushed, removed or restored while the view is open: the same
  // versions stay selected (by number), and a range or cursor at the newest
  // version follows it.
  const [seen, setSeen] = React.useState(history);
  if (seen !== history) {
    setSeen(history);
    setState((s) => carryOver(s, seen, history));
  }
  const [playing, setPlaying] = React.useState(false);
  const [expanded, setExpanded] = React.useState(false);
  // A slider of the track has focus: it says its own value (the version, as
  // the readout would), so the readout keeps quiet instead of saying it twice.
  const [onSlider, setOnSlider] = React.useState(false);
  const sheet = React.useRef<HTMLDivElement>(null);
  const [query, setQuery] = React.useState(initial.query);
  const update = React.useCallback((patch: Partial<TimeMachineState>) => {
    setState((s) => ({ ...s, ...patch }));
  }, []);
  React.useEffect(() => {
    onStateChange?.(state, history);
  }, [state, history, onStateChange]);
  // Typing filters a beat later: every key press would redraw the graph.
  React.useEffect(() => {
    const timer = window.setTimeout(() => update({ query }), 180);
    return () => window.clearTimeout(timer);
  }, [query, update]);

  const { versions } = history;
  const n = versions.length;
  const activity = React.useMemo(() => activityOf(history), [history]);
  const groupMembers = React.useMemo(
    () => (state.group ? new Set(history.groups[state.group]?.members ?? []) : null),
    [state.group, history.groups],
  );
  const frame = useFrame(history, {
    range: state.range,
    at: state.at,
    mode: state.mode,
    filters: { changedOnly: state.changedOnly, tables: groupMembers, query: state.query },
  });

  // Every table the history ever had, placed once: nothing moves from one
  // version to the next, nor when the range changes.
  const union = React.useMemo(
    () =>
      new Frames(history).frame({
        range: { from: 0, to: n - 1 },
        at: n - 1,
        mode: "superposed",
      }).graph,
    [history, n],
  );
  // Positions saved by versions first, then the ones computed on an
  // earlier visit; only the tables neither has are laid out (once, kept).
  const [placedBefore] = React.useState(() => readPlaced(layoutKey));
  const known = React.useMemo(
    () => ({ ...placedBefore, ...history.layout }),
    [placedBefore, history.layout],
  );
  const { positions, pending } = useAutoLayout(union, known, history.groups, (placed) =>
    keepPlaced(layoutKey, placed, union),
  );
  // Tables still waiting for their place (a version just pushed) are left
  // out until placed; the rest stays on screen.
  const drawn = React.useMemo(() => placedOnly(frame.graph, positions), [frame.graph, positions]);
  const arranging = pending > 0 && drawn.tables.length === 0 && frame.graph.tables.length > 0;

  // While playing, the version under the cursor is not read out at every
  // step; pausing says where it stopped.
  const [announcement, setAnnouncement] = React.useState("");
  usePlayback({
    at: state.at,
    from: state.range.from,
    to: state.range.to,
    playing,
    onStep: (at) => update({ at }),
    onEnd: () => {
      setPlaying(false);
      setAnnouncement(`Played to v${versions[state.at]?.number ?? ""}.`);
    },
  });
  const play = (on: boolean) => {
    // Played from the end, the range plays again from its start.
    if (on && state.at >= state.range.to) update({ at: state.range.from });
    if (!on && playing) {
      const v = versions[state.at];
      setAnnouncement(`Paused at v${v?.number ?? ""}${v?.message ? `: ${v.message}` : ""}.`);
    }
    if (on) setAnnouncement("");
    setPlaying(on);
  };
  const step = (by: -1 | 1) => {
    setPlaying(false);
    update({ at: Math.max(state.range.from, Math.min(state.range.to, state.at + by)) });
  };

  // [ and ] step (held, they keep stepping), K plays, M switches the mode.
  const keys = React.useRef({ step, play, playing });
  React.useEffect(() => {
    keys.current = { step, play, playing };
  });
  React.useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
      if (typing(event.target)) return;
      const { step, play, playing } = keys.current;
      if (event.key === "[") step(-1);
      else if (event.key === "]") step(1);
      else if ((event.key === "k" || event.key === "K") && !event.repeat) play(!playing);
      else if ((event.key === "m" || event.key === "M") && !event.repeat)
        setState((s) => ({ ...s, mode: s.mode === "cursor" ? "superposed" : "cursor" }));
      else return;
      event.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const track = state.table ? history.tables.find((t) => t.id === state.table) : undefined;
  const select = (table: string | null) => {
    update({ table });
    if (table) setExpanded(true);
  };
  const cursor = versions[state.at]!;
  const summary = summaryText(frame.counts);
  const groups = Object.entries(history.groups);
  const filtering = state.changedOnly || state.group !== null || state.query.trim() !== "";

  // In the bar (wide screens), "Only changes" explains itself in a tooltip;
  // in the popover, whose opening focuses it, a tooltip would open at once
  // over the popover: a line under the filters says what they do instead.
  const filters = (where: "bar" | "popover") => {
    const changesOnly = (
      <Button
        variant={state.changedOnly ? "secondary" : "ghost"}
        size="sm"
        aria-pressed={state.changedOnly}
        onClick={() => update({ changedOnly: !state.changedOnly })}
        className={cn(state.changedOnly && "border-primary/40 bg-primary/8 text-foreground")}
      >
        <FilterIcon />
        Only changes
      </Button>
    );
    return (
      <>
        {where === "bar" ? (
          <Tooltip content="Fade back what did not change in the range" side="top">
            {changesOnly}
          </Tooltip>
        ) : (
          changesOnly
        )}
        {groups.length > 0 && (
          <Select
            value={state.group ?? "all"}
            onValueChange={(v) => update({ group: v === "all" ? null : v })}
          >
            <SelectTrigger aria-label="Group" className="h-7 w-auto min-w-36 text-sm sm:h-7">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">All tables</SelectItem>
              {groups.map(([id, g]) => (
                <SelectItem key={id} value={id}>
                  <span className="flex items-center gap-2">
                    <span
                      aria-hidden
                      className={cn("size-2 rounded-xs", GROUP_DOT[groupColor(g.color)])}
                    />
                    {g.name}
                  </span>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )}
        <Input
          type="search"
          aria-label="Filter tables by name"
          placeholder="Filter tables"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          prefix={<SearchIcon aria-hidden className="mr-1.5 size-3.5" />}
          className="h-7 w-full sm:h-7 xl:w-44"
        />
        {where === "popover" && (
          <p className="text-xs text-muted-foreground">
            What the filters leave out fades back; no table moves.
          </p>
        )}
      </>
    );
  };

  const versionsCount = state.range.to - state.range.from + 1;
  const range = `v${versions[state.range.from]?.number} → v${versions[state.range.to]?.number}`;
  const modeToggle = (className?: string) => (
    <ModeToggle
      mode={state.mode}
      onMode={(mode) => update({ mode })}
      range={state.range}
      at={state.at}
      versions={versions}
      away={frame.away}
      className={className}
    />
  );
  return (
    <div className={cn("flex h-full min-h-0 flex-col", className)}>
      {/* The range: what it is and what it changed. */}
      <div className="flex min-h-11 shrink-0 items-center gap-2 border-b border-border px-3 py-1.5 md:px-4">
        <ClockRotateRightIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
        <h2 className="shrink-0 text-sm font-semibold text-foreground">Time machine</h2>
        <p
          className="min-w-0 truncate text-sm text-muted-foreground"
          title={`${range}: ${summary}`}
        >
          <span className="font-mono text-foreground tabular">{range}</span>
          <span className="hidden sm:inline"> · {versionsCount} versions</span>
          <span className="hidden lg:inline" data-range-summary="">
            {" "}
            · {summary}
          </span>
        </p>
        <div className="ml-auto flex shrink-0 items-center gap-1.5">{actions?.(state)}</div>
      </div>

      <div className="relative flex min-h-0 flex-1">
        <div className="relative min-w-0 flex-1">
          {arranging ? (
            // Shown only if the layout takes a moment (a first visit; the
            // engine is preloaded with the history): a quick one flashes nothing.
            <div className="flex h-full items-center justify-center gap-2.5 text-base text-muted-foreground motion-safe:animate-in motion-safe:delay-400 motion-safe:fade-in-0 motion-safe:fill-mode-both">
              <Spinner label={`Arranging ${union.tables.length} tables`} />
              <span aria-hidden>Arranging {union.tables.length} tables…</span>
            </div>
          ) : (
            <SchemaGraph
              schema={frame.schema}
              graph={drawn}
              layout={positions}
              groups={history.groups}
              selected={state.table}
              onSelectedChange={select}
              legend={false}
              empty={
                <EmptyState
                  icon={<ClockRotateRightIcon />}
                  title={
                    state.mode === "cursor"
                      ? `No tables in v${cursor.number}`
                      : `No tables in ${range}`
                  }
                  description={
                    state.mode === "cursor"
                      ? "This version has no table or view. Move the cursor, or superpose the range."
                      : "No version of this range has a table or view. Widen the range."
                  }
                />
              }
            />
          )}
        </div>
        {track && (
          <aside className="hidden w-80 shrink-0 border-l border-border bg-card md:flex">
            <HistoryPanel
              history={history}
              track={track}
              range={state.range}
              scale={frame.scale}
              VersionLink={VersionLink}
              onClose={() => update({ table: null })}
              className="w-full"
            />
          </aside>
        )}
      </div>

      {/* The timeline: the track over the bar, docked under the graph; on a
          phone, the bar alone, the track in a sheet rising from it. */}
      <section
        aria-label="Timeline"
        className="relative z-1 shrink-0 border-t border-border bg-card shadow-raised md:shadow-none"
        onFocus={(event) => setOnSlider(event.target.getAttribute("role") === "slider")}
        onBlur={() => setOnSlider(false)}
      >
        <div
          ref={sheet}
          id="time-machine-sheet"
          className={cn(
            // A phone: a sheet rising from the bar over the graph, which keeps
            // its size. Wider: docked under the graph, always open.
            "absolute inset-x-0 bottom-full max-h-[min(58vh,32rem)] overflow-y-auto rounded-t-xl border-t border-border bg-card px-3 pt-4 pb-3 shadow-dialog",
            "duration-(--duration-base) motion-safe:animate-in motion-safe:fade-in-0 motion-safe:slide-in-from-bottom-2",
            "md:static md:block md:max-h-none md:animate-none md:overflow-visible md:rounded-none md:border-0 md:px-4 md:pt-2 md:pb-1.5 md:shadow-none",
            expanded ? "block" : "hidden",
          )}
        >
          <span
            aria-hidden
            className="absolute top-1.5 left-1/2 h-1 w-9 -translate-x-1/2 rounded-full bg-border-strong md:hidden"
          />
          <Track
            versions={versions}
            activity={activity}
            range={state.range}
            at={state.at}
            scale={frame.scale}
            onRange={(range, at) => update({ range, at })}
            onAt={(at) => {
              setPlaying(false);
              update({ at });
            }}
          />
          <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 pt-1">
            <Legend scale={frame.scale} versions={versions} mode={state.mode} at={state.at} />
            <p className="text-xs text-muted-foreground lg:hidden">
              <span className="font-mono tabular">{range}</span>: {summary}
            </p>
          </div>
          {modeToggle("mt-3 md:hidden")}
          {track && (
            <div className="-mx-3 mt-3 border-t border-border md:hidden">
              <HistoryPanel
                history={history}
                track={track}
                range={state.range}
                scale={frame.scale}
                VersionLink={VersionLink}
                onClose={() => update({ table: null })}
              />
            </div>
          )}
        </div>
        {/* Above the sheet, whose shadow falls on the graph, not on it. */}
        <div className="relative z-2 flex items-center gap-2 bg-card px-3 py-2 md:gap-3 md:border-t md:border-border md:px-4">
          <Transport
            mode={state.mode}
            playing={playing}
            onPlayingChange={play}
            canBack={state.at > state.range.from}
            canForward={state.at < state.range.to}
            onStep={step}
          />
          <CursorReadout version={cursor} live={!playing && !onSlider} />
          <p className="sr-only" aria-live="polite">
            {announcement}
          </p>
          {modeToggle("hidden shrink-0 items-end md:flex")}
          <div className="hidden shrink-0 items-center gap-1.5 xl:flex">{filters("bar")}</div>
          <Popover>
            <PopoverTrigger asChild>
              <Button
                variant={filtering ? "secondary" : "ghost"}
                size="icon"
                aria-label={filtering ? "Filters (on)" : "Filters"}
                className="xl:hidden"
              >
                <FilterIcon />
              </Button>
            </PopoverTrigger>
            <PopoverContent align="end" side="top" className="grid w-64 gap-2">
              {filters("popover")}
            </PopoverContent>
          </Popover>
          <Button
            variant="ghost"
            size="icon"
            className="md:hidden"
            aria-expanded={expanded}
            aria-controls="time-machine-sheet"
            aria-label={expanded ? "Hide the timeline" : "Show the timeline"}
            onClick={(event) => {
              setExpanded(!expanded);
              // Opened from the keyboard: the sheet's controls come before
              // this button, so the keys go on in it, on the cursor (drawn
              // once the track has measured itself, a frame or two later).
              if (!expanded && event.detail === 0) {
                let frames = 0;
                const focus = () => {
                  const thumb = sheet.current?.querySelector<HTMLElement>('[data-thumb="at"]');
                  if (thumb) thumb.focus();
                  else if (++frames < 10) requestAnimationFrame(focus);
                };
                requestAnimationFrame(focus);
              }
            }}
          >
            {expanded ? <NavArrowDownIcon /> : <NavArrowUpIcon />}
          </Button>
        </div>
      </section>
    </div>
  );
}

const GROUP_DOT: Record<string, string> = {
  stone: "bg-group-stone",
  red: "bg-group-red",
  amber: "bg-group-amber",
  green: "bg-group-green",
  teal: "bg-group-teal",
  blue: "bg-group-blue",
  violet: "bg-group-violet",
  pink: "bg-group-pink",
};
