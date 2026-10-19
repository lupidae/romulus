import * as React from "react";

import {
  CombineIcon,
  KeyframeIcon,
  PauseIcon,
  PlayIcon,
  SkipNextIcon,
  SkipPrevIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip } from "@/components/ui/tooltip";
import { Byline, OriginBadge } from "@/features/databases/components/version-meta";
import { originLabel } from "@/features/databases/queries";
import { formatDate } from "@/lib/time";
import { cn } from "@/lib/utils";

import { versionName } from "../describe";
import type { Mode } from "../frame";
import type { TimelineVersion } from "../history";
import { type Range, type Scale, STEP_BG, stepOf } from "../scale";

/** Room at both ends of the track, so the end handles are whole. */
const PAD = 14;

export interface TimelineProps {
  versions: TimelineVersion[];
  /** How much happened at each version (the bars above the track). */
  activity: number[];
  range: Range;
  at: number;
  scale: Scale;
  onRange: (range: Range, at: number) => void;
  onAt: (at: number) => void;
}

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

function describeVersion(v: TimelineVersion): string {
  return [
    versionName(v),
    v.message ?? "no message",
    originLabel[v.origin].toLowerCase(),
    v.author?.name,
    formatDate(v.created_at),
  ]
    .filter(Boolean)
    .join(", ");
}

/**
 * The versions along a track: how much each changed (bars, tinted by the age
 * scale inside the range), the range between two handles and the cursor.
 * Click or drag anywhere to move the cursor, drag a handle to change the
 * range; each is a slider for the keyboard too.
 */
export function Track({ versions, activity, range, at, scale, onRange, onAt }: TimelineProps) {
  const ref = React.useRef<HTMLDivElement>(null);
  const [width, setWidth] = React.useState(0);
  const [hover, setHover] = React.useState<number | null>(null);
  const [drag, setDrag] = React.useState<"from" | "to" | "at" | null>(null);
  const n = versions.length;

  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.clientWidth);
    const observer = new ResizeObserver(() => setWidth(el.clientWidth));
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  const x = React.useCallback(
    (i: number) => (n <= 1 ? width / 2 : PAD + ((width - 2 * PAD) * i) / (n - 1)),
    [n, width],
  );
  const indexAt = (clientX: number) => {
    const box = ref.current!.getBoundingClientRect();
    const t = (clientX - box.left - PAD) / Math.max(1, box.width - 2 * PAD);
    return clamp(Math.round(t * (n - 1)), 0, n - 1);
  };
  const spacing = n > 1 ? (width - 2 * PAD) / (n - 1) : width;
  // Scaled to the versions after the first, which adds everything and would
  // flatten the rest (it is drawn at full height).
  const max = Math.max(1, ...activity.slice(1));

  const move = (target: "from" | "to" | "at", i: number) => {
    if (target === "at") onAt(clamp(i, range.from, range.to));
    else if (target === "from") {
      const from = clamp(i, 0, range.to - 1);
      onRange({ from, to: range.to }, clamp(at, from, range.to));
    } else {
      const to = clamp(i, range.from + 1, n - 1);
      onRange({ from: range.from, to }, clamp(at, range.from, to));
    }
  };

  const onPointerDown = (event: React.PointerEvent) => {
    if (event.button !== 0) return;
    const box = ref.current!.getBoundingClientRect();
    const px = event.clientX - box.left;
    const y = event.clientY - box.top;
    // A finger gets a 44px target around each handle; a pointer, 24px.
    const reach = event.pointerType === "mouse" ? 12 : 22;
    const inLane = y >= 60 - reach && y <= 60 + reach;
    // A handle, when pressed on (on the track's lane); the cursor anywhere else.
    const target =
      inLane && Math.abs(px - x(range.to)) <= reach
        ? "to"
        : inLane && Math.abs(px - x(range.from)) <= reach
          ? "from"
          : "at";
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    setDrag(target);
    move(target, indexAt(event.clientX));
    // The slider being dragged takes focus: the keys go on where the pointer left off.
    ref.current
      ?.querySelector<HTMLElement>(`[data-thumb="${target}"]`)
      ?.focus({ preventScroll: true, focusVisible: false });
  };
  const onPointerMove = (event: React.PointerEvent) => {
    const i = indexAt(event.clientX);
    if (drag) move(drag, i);
    else if (event.pointerType !== "touch") setHover(i);
  };
  const end = () => setDrag(null);

  const keys = (target: "from" | "to" | "at", value: number) => (event: React.KeyboardEvent) => {
    const page = Math.max(1, Math.round(n / 10));
    const next: Record<string, number> = {
      ArrowLeft: value - 1,
      ArrowDown: value - 1,
      ArrowRight: value + 1,
      ArrowUp: value + 1,
      PageDown: value - page,
      PageUp: value + page,
      Home: target === "at" ? range.from : 0,
      End: target === "at" ? range.to : n - 1,
    };
    if (!(event.key in next)) return;
    event.preventDefault();
    move(target, next[event.key]!);
  };

  // Labels: the ends, the cursor and every few versions in between, never touching.
  const labelEvery = Math.max(1, Math.ceil(44 / Math.max(1, spacing)));
  const labels: number[] = [];
  for (let i = 0; i < n; i++) {
    const wanted = i === 0 || i === n - 1 || i % labelEvery === 0;
    if (!wanted) continue;
    const last = labels[labels.length - 1];
    if (last !== undefined && x(i) - x(last) < 36) {
      // The last one wins over a regular one.
      if (i === n - 1) labels[labels.length - 1] = i;
      continue;
    }
    labels.push(i);
  }
  const shown = hover !== null && !drag ? versions[hover] : undefined;
  const barWidth = clamp(spacing * 0.56, 2, 10);
  const cursor = versions[at];

  return (
    <div
      ref={ref}
      className={cn(
        "relative h-[5.5rem] touch-none select-none",
        drag ? "cursor-grabbing" : "cursor-pointer",
      )}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={end}
      onPointerCancel={end}
      onPointerLeave={() => setHover(null)}
    >
      {width > 0 && (
        <>
          {/* How much each version changed: tinted by when, within the range. */}
          <div aria-hidden className="absolute inset-x-0 top-[1.375rem] h-7">
            {versions.map((v, i) => {
              const step = stepOf(scale, i);
              const inRange = i >= range.from && i <= range.to;
              const height = 3 + Math.round(25 * Math.sqrt(Math.min(1, (activity[i] ?? 0) / max)));
              return (
                <span
                  key={v.id}
                  className={cn(
                    "absolute bottom-0 rounded-t-xs transition-opacity duration-(--duration-fast)",
                    step !== undefined
                      ? STEP_BG[step]
                      : i === range.from
                        ? "bg-muted-foreground"
                        : "bg-border-strong",
                    !inRange && "opacity-60",
                    inRange && i > at && "opacity-35",
                    v.removed_at && "border border-dashed border-faint-foreground bg-transparent",
                  )}
                  style={{ left: x(i) - barWidth / 2, width: barWidth, height }}
                />
              );
            })}
          </div>

          {/* The track, the range on it, and a dot per version when there is room. */}
          <div aria-hidden className="absolute inset-x-0 top-[3.125rem] h-5">
            <span
              className="absolute top-1/2 h-1 -translate-y-1/2 rounded-full bg-border"
              style={{ left: PAD, right: PAD }}
            />
            <span
              className="absolute top-1/2 h-1.5 -translate-y-1/2 rounded-full bg-primary/25"
              style={{ left: x(range.from), width: x(range.to) - x(range.from) }}
            />
            {spacing >= 7 &&
              versions.map((v, i) => (
                <span
                  key={v.id}
                  className={cn(
                    "absolute top-1/2 size-1 -translate-1/2 rounded-full",
                    i >= range.from && i <= range.to ? "bg-primary/60" : "bg-faint-foreground/50",
                  )}
                  style={{ left: x(i) }}
                />
              ))}
          </div>

          {/* The cursor: a playhead, its version on top, its line through the bars. */}
          <span
            aria-hidden
            className="pointer-events-none absolute top-[1.125rem] h-[2.625rem] w-0.5 -translate-x-1/2 rounded-full bg-foreground"
            style={{ left: x(at) }}
          />

          <Thumb
            target="from"
            label="Range start"
            left={x(range.from)}
            min={0}
            max={range.to - 1}
            value={range.from}
            text={describeVersion(versions[range.from]!)}
            onKeyDown={keys("from", range.from)}
          />
          <Thumb
            target="to"
            label="Range end"
            left={x(range.to)}
            min={range.from + 1}
            max={n - 1}
            value={range.to}
            text={describeVersion(versions[range.to]!)}
            onKeyDown={keys("to", range.to)}
          />
          <Thumb
            target="at"
            label="Cursor"
            left={clamp(x(at), 18, width - 18)}
            min={range.from}
            max={range.to}
            value={at}
            text={cursor ? describeVersion(cursor) : ""}
            onKeyDown={keys("at", at)}
          >
            v{cursor?.number}
          </Thumb>

          <div aria-hidden className="absolute inset-x-0 top-[4.5rem] h-4">
            {labels.map((i) => (
              <span
                key={versions[i]!.id}
                className="absolute -translate-x-1/2 font-mono text-xs whitespace-nowrap text-faint-foreground tabular"
                style={{ left: clamp(x(i), 12, width - 12) }}
              >
                v{versions[i]!.number}
              </span>
            ))}
          </div>

          {shown && hover !== null && (
            <div
              aria-hidden
              className="pointer-events-none absolute bottom-full z-(--z-tooltip) mb-2 grid w-max max-w-72 gap-0.5 rounded-md bg-foreground px-2 py-1.5 text-xs text-background shadow-popover"
              style={{
                left: clamp(x(hover), 120, Math.max(120, width - 120)),
                transform: "translateX(-50%)",
              }}
            >
              <span className="flex items-center gap-1.5 font-medium">
                <span className="font-mono">{versionName(shown)}</span>
                <span className="truncate">{shown.message ?? "No message"}</span>
              </span>
              <span className="text-background/70">
                {originLabel[shown.origin]}
                {shown.author ? ` by ${shown.author.name}` : ""} · {formatDate(shown.created_at)}
                {activity[hover] ? ` · ${activity[hover]} changes` : ""}
              </span>
              {shown.removal_reason && (
                <span className="text-background/70">Removed: {shown.removal_reason}</span>
              )}
            </div>
          )}
        </>
      )}
    </div>
  );
}

function Thumb({
  target,
  label,
  left,
  min,
  max,
  value,
  text,
  onKeyDown,
  children,
}: {
  target: "from" | "to" | "at";
  label: string;
  left: number;
  min: number;
  max: number;
  value: number;
  text: string;
  onKeyDown: (event: React.KeyboardEvent) => void;
  children?: React.ReactNode;
}) {
  const cursor = target === "at";
  return (
    <div
      role="slider"
      tabIndex={0}
      data-thumb={target}
      aria-label={label}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={value}
      aria-valuetext={text}
      aria-orientation="horizontal"
      onKeyDown={onKeyDown}
      className={cn(
        "absolute -translate-x-1/2 outline-none",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-card",
        cursor
          ? "top-0 z-2 flex h-[1.125rem] cursor-grab items-center rounded-sm bg-foreground px-1.5 font-mono text-xs font-semibold text-background tabular shadow-xs"
          : "top-[3.75rem] z-1 h-5 w-2.5 -translate-y-1/2 cursor-ew-resize rounded-xs border-2 border-primary bg-card shadow-xs",
      )}
      style={{ left }}
    >
      {children}
    </div>
  );
}

/** Play, pause and step: the cursor through the range. */
export function Transport({
  playing,
  onPlayingChange,
  canBack,
  canForward,
  onStep,
}: {
  playing: boolean;
  onPlayingChange: (playing: boolean) => void;
  canBack: boolean;
  canForward: boolean;
  onStep: (by: -1 | 1) => void;
}) {
  return (
    <div className="flex shrink-0 items-center gap-0.5">
      <Tooltip content={playing ? "Pause" : "Play the range"} shortcut={["K"]} side="top">
        <Button
          variant="primary"
          size="icon"
          aria-label={playing ? "Pause" : "Play the range"}
          aria-pressed={playing}
          onClick={() => onPlayingChange(!playing)}
        >
          {playing ? <PauseIcon /> : <PlayIcon />}
        </Button>
      </Tooltip>
      <Tooltip content="Previous version" shortcut={["["]} side="top">
        <Button
          variant="ghost"
          size="icon"
          aria-label="Previous version"
          disabled={!canBack}
          onClick={() => onStep(-1)}
        >
          <SkipPrevIcon />
        </Button>
      </Tooltip>
      <Tooltip content="Next version" shortcut={["]"]} side="top">
        <Button
          variant="ghost"
          size="icon"
          aria-label="Next version"
          disabled={!canForward}
          onClick={() => onStep(1)}
        >
          <SkipNextIcon />
        </Button>
      </Tooltip>
    </div>
  );
}

/** The version at the cursor: its number, message, origin and who made it when. */
export function CursorReadout({
  version,
  compact = false,
  live = true,
}: {
  version: TimelineVersion;
  compact?: boolean;
  /** Read out when it changes (not while playing: a version every half second). */
  live?: boolean;
}) {
  return (
    <div className="flex min-w-0 flex-1 items-center gap-2.5" aria-live={live ? "polite" : "off"}>
      <span className="shrink-0 font-mono text-base font-semibold text-foreground tabular">
        v{version.number}
      </span>
      <span
        className="min-w-0 truncate text-sm text-foreground"
        title={version.message ?? undefined}
      >
        {version.message ?? <span className="text-muted-foreground">No message</span>}
      </span>
      {version.removed_at && (
        <span className="shrink-0 rounded-sm px-1 text-xs text-muted-foreground ring-1 ring-border-strong ring-inset">
          Removed
        </span>
      )}
      {!compact && (
        <span className="hidden shrink-0 items-center gap-2.5 lg:flex">
          <OriginBadge origin={version.origin} />
          <Byline author={version.author} at={version.created_at} className="text-sm" />
        </span>
      )}
    </div>
  );
}

export function ModeToggle({ mode, onMode }: { mode: Mode; onMode: (mode: Mode) => void }) {
  // No tooltips on the items: a tooltip's trigger state would overwrite
  // theirs (`data-state`), and the chosen mode would not show.
  return (
    <ToggleGroup
      type="single"
      value={mode}
      onValueChange={(next) => next && onMode(next as Mode)}
      aria-label="What the graph shows"
      aria-keyshortcuts="M"
      // On the timeline's card: the track sinks to the canvas so the chosen
      // mode (the card's color) stands out in both themes.
      className="bg-background ring-1 ring-border ring-inset"
    >
      <ToggleGroupItem
        value="superposed"
        aria-description="Everything in the range up to the cursor, what is gone as ghosts"
      >
        <CombineIcon />
        Superposed
      </ToggleGroupItem>
      <ToggleGroupItem value="cursor" aria-description="Exactly the schema at the cursor">
        <KeyframeIcon />
        At cursor
      </ToggleGroupItem>
    </ToggleGroup>
  );
}

/**
 * What the colors say: when things appeared (the scale's runs, oldest
 * first), what was there at the start, what went, what changed.
 */
export function Legend({ scale, versions }: { scale: Scale; versions: TimelineVersion[] }) {
  const start = versions[scale.range.from];
  const name = (i: number) => `v${versions[i]?.number ?? "?"}`;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
      <span className="flex items-center gap-1.5">
        <span
          aria-hidden
          className="size-2.5 rounded-xs bg-card ring-1 ring-border-strong ring-inset"
        />
        In {start ? `v${start.number}` : "the first version"}
      </span>
      {scale.buckets.length > 0 && (
        <span className="flex min-w-0 items-center gap-1.5">
          <span>Appeared</span>
          <span className="flex items-center gap-1">
            {scale.buckets.map((b) => (
              <span key={b.first} className="flex items-center gap-1">
                <span aria-hidden className={cn("size-2.5 rounded-xs", STEP_BG[b.step])} />
                <span className="font-mono tabular">
                  {b.first === b.last
                    ? name(b.first)
                    : `${name(b.first)}–${versions[b.last]?.number}`}
                </span>
              </span>
            ))}
          </span>
        </span>
      )}
      <span className="flex items-center gap-1.5">
        <span
          aria-hidden
          className="h-2.5 w-4 rounded-xs border border-dashed border-faint-foreground"
        />
        <span className="line-through">Removed</span>
      </span>
      <span className="flex items-center gap-1.5">
        <span aria-hidden className="font-mono font-semibold text-warning">
          ~
        </span>
        Changed
      </span>
    </div>
  );
}
