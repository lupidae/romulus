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

import { changesText, MODE_NAME, modeCaption, versionName } from "../describe";
import type { Frame, Mode } from "../frame";
import type { TimelineVersion } from "../history";
import { ERA_BG, eraOf, type Range, type Scale } from "../scale";
import { columns, dateTicks, indexAt, isLong, labelWidth, spacing, xOf } from "../track";

/**
 * What was there at the range's start: uncolored, outlined. Its bar on the
 * track and its key in the legend look alike, the timeline being the legend.
 */
const START_KEY = "rounded-xs bg-card ring-1 ring-faint-foreground ring-inset";

export interface TimelineProps {
  versions: TimelineVersion[];
  /** How much happened at each version (the bars above the track). */
  activity: number[];
  range: Range;
  at: number;
  /** The range's age scale (stable while the range is: the bars are drawn again only then). */
  scale: Scale;
  onRange: (range: Range, at: number) => void;
  onAt: (at: number) => void;
}

const clamp = (n: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, n));

/** A version for a slider's value: what it is, then how much it changed (the bars' height). */
function describeVersion(v: TimelineVersion, changes: number | undefined): string {
  return [
    versionName(v),
    v.message ?? "no message",
    originLabel[v.origin].toLowerCase(),
    v.author?.name,
    formatDate(v.created_at),
    changes !== undefined && changesText(changes),
  ]
    .filter(Boolean)
    .join(", ");
}

const THUMB_LABEL = { from: "Range start", to: "Range end", at: "Cursor" } as const;
type Target = keyof typeof THUMB_LABEL;

/** An element's width, followed as it resizes (0 until measured). */
function useWidth(ref: React.RefObject<HTMLElement | null>) {
  const [width, setWidth] = React.useState(0);
  React.useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.clientWidth);
    const observer = new ResizeObserver(() => setWidth(el.clientWidth));
    observer.observe(el);
    return () => observer.disconnect();
  }, [ref]);
  return width;
}

/**
 * Moving a range handle or the cursor to a version, kept in order (start
 * before end, the cursor between them), and the slider keys doing it.
 */
function movesOf(
  n: number,
  { range, at, onRange, onAt }: Pick<TimelineProps, "range" | "at" | "onRange" | "onAt">,
) {
  const move = (target: Target, i: number) => {
    if (target === "at") onAt(clamp(i, range.from, range.to));
    else if (target === "from") {
      const from = clamp(i, 0, range.to - 1);
      onRange({ from, to: range.to }, clamp(at, from, range.to));
    } else {
      const to = clamp(i, range.from + 1, n - 1);
      onRange({ from: range.from, to }, clamp(at, range.from, to));
    }
  };
  const keys = (target: Target, value: number) => (event: React.KeyboardEvent) => {
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
  return { move, keys };
}

/**
 * How much each version of `domain` changed: a bar each, as high as that,
 * tinted by when within the range; one bar per run of neighbours when they
 * would overlap (the run's busiest version standing for it). Drawn again
 * only when the range or the width changes, never at a cursor step: what
 * comes after the cursor fades under `AfterCursor`.
 */
const Bars = React.memo(function Bars({
  versions,
  activity,
  max,
  scale,
  range,
  domain,
  width,
  height,
}: {
  versions: TimelineVersion[];
  activity: number[];
  /** The activity drawn at full height. */
  max: number;
  scale: Scale;
  range: Range;
  domain: Range;
  width: number;
  height: number;
}) {
  const x = xOf(domain, width);
  const gap = spacing(domain, width);
  return columns(domain, width, activity).map((c) => {
    const i = c.top;
    const v = versions[i]!;
    const slot = eraOf(scale, i);
    const inRange = i >= range.from && i <= range.to;
    const tall = 3 + Math.round((height - 3) * Math.sqrt(Math.min(1, (activity[i] ?? 0) / max)));
    const wide =
      c.first === c.last
        ? clamp(gap * 0.56, 2, 10)
        : Math.max(1.5, (c.last - c.first + 1) * gap - 1);
    return (
      <span
        key={v.id}
        data-bar=""
        data-range-start={i === range.from ? "" : undefined}
        className={cn(
          "absolute bottom-0 rounded-t-xs",
          slot !== undefined
            ? ERA_BG[slot]
            : i === range.from && !v.removed_at
              ? cn(START_KEY, "rounded-b-none")
              : "bg-border-strong",
          !inRange && "opacity-60",
          v.removed_at && "border border-dashed border-faint-foreground bg-transparent",
        )}
        style={{ left: (x(c.first) + x(c.last)) / 2 - wide / 2, width: wide, height: tall }}
      />
    );
  });
});

/**
 * The bars of the range after the cursor, stepped back: a veil of the
 * card's color at 65%, which is the bars at 35% opacity, moved alone when
 * the cursor moves.
 */
function AfterCursor({
  domain,
  width,
  range,
  at,
}: {
  domain: Range;
  width: number;
  range: Range;
  at: number;
}) {
  if (at >= range.to) return null;
  const x = xOf(domain, width);
  const half = spacing(domain, width) / 2;
  const left = Math.max(0, x(at) + half);
  const right = Math.min(width, x(range.to) + Math.max(half, 5));
  if (right <= left) return null;
  return <span className="absolute inset-y-0 bg-card/65" style={{ left, width: right - left }} />;
}

/** What a version is, over the track: named after a handle when one is meant. */
function VersionCard({
  version,
  changes,
  title,
  left,
  width,
  kind,
}: {
  version: TimelineVersion;
  changes: number | undefined;
  title?: string;
  left: number;
  width: number;
  kind: string;
}) {
  return (
    <div
      aria-hidden
      data-track-card={kind}
      className="pointer-events-none absolute bottom-full z-(--z-tooltip) mb-2 grid w-max max-w-72 gap-0.5 rounded-md bg-foreground px-2 py-1.5 text-xs text-background shadow-popover"
      style={{ left: clamp(left, 120, Math.max(120, width - 120)), transform: "translateX(-50%)" }}
    >
      {title && <span className="text-background/70">{title}</span>}
      <span className="flex items-center gap-1.5 font-medium">
        <span className="font-mono">{versionName(version)}</span>
        <span className="truncate">{version.message ?? "No message"}</span>
      </span>
      <span className="text-background/70">
        {originLabel[version.origin]}
        {version.author ? ` by ${version.author.name}` : ""} · {formatDate(version.created_at)}
        {changes !== undefined ? ` · ${changesText(changes)}` : ""}
      </span>
      {version.removal_reason && (
        <span className="text-background/70">Removed: {version.removal_reason}</span>
      )}
    </div>
  );
}

/**
 * The versions along a track: how much each changed (bars, colored by the
 * age scale inside the range), the range between two handles and the cursor.
 * Click or drag anywhere to move the cursor, drag a handle to change the
 * range; each is a slider for the keyboard too. Hovering says what a version
 * is (a handle, which one it is and that it drags); dragging, what the handle
 * dragged is on.
 *
 * A history too long for one lane (under 4px a version) gets two: the whole
 * history above, the range a window on it (its handles there), and the range
 * alone below, at full width, where the cursor moves.
 */
export function Track(props: TimelineProps) {
  const ref = React.useRef<HTMLDivElement>(null);
  const width = useWidth(ref);
  const n = props.versions.length;
  const long = isLong(n, width);
  const all = React.useMemo(() => ({ from: 0, to: n - 1 }), [n]);
  // Scaled to the versions after the first, which adds everything and would
  // flatten the rest (it is drawn at full height).
  const max = React.useMemo(() => Math.max(1, ...props.activity.slice(1)), [props.activity]);
  return (
    <div ref={ref} data-track-lanes={long ? "two" : "one"} className="min-h-[5.5rem]">
      {width > 0 &&
        (long ? (
          <>
            <Overview {...props} width={width} max={max} all={all} />
            <Lane {...props} width={width} max={max} domain={props.range} handles={false} />
          </>
        ) : (
          <Lane {...props} width={width} max={max} domain={all} handles />
        ))}
    </div>
  );
}

/** One lane of the track: the versions of `domain`, the cursor, and the range's handles when `handles`. */
function Lane({
  width,
  max,
  domain,
  handles,
  ...props
}: TimelineProps & { width: number; max: number; domain: Range; handles: boolean }) {
  const { versions, activity, range, at, scale, onRange, onAt } = props;
  const ref = React.useRef<HTMLDivElement>(null);
  const [hover, setHover] = React.useState<number | null>(null);
  // The range handle under the pointer: the card names it.
  const [over, setOver] = React.useState<"from" | "to" | null>(null);
  const [drag, setDrag] = React.useState<Target | null>(null);
  const x = xOf(domain, width);
  const gap = spacing(domain, width);
  const { move, keys } = movesOf(versions.length, { range, at, onRange, onAt });
  const indexOf = (clientX: number) =>
    indexAt(domain, width, clientX - ref.current!.getBoundingClientRect().left);

  /** The range handle within reach of the pointer, on the track's lane. */
  const handleAt = (event: React.PointerEvent): "from" | "to" | null => {
    if (!handles) return null;
    const box = ref.current!.getBoundingClientRect();
    const px = event.clientX - box.left;
    const y = event.clientY - box.top;
    // A finger gets a 44px target around each handle; a pointer, 24px.
    const reach = event.pointerType === "mouse" ? 12 : 22;
    if (y < 60 - reach || y > 60 + reach) return null;
    if (Math.abs(px - x(range.to)) <= reach) return "to";
    if (Math.abs(px - x(range.from)) <= reach) return "from";
    return null;
  };

  const onPointerDown = (event: React.PointerEvent) => {
    if (event.button !== 0) return;
    // A handle, when pressed on; the cursor anywhere else.
    const target = handleAt(event) ?? "at";
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    setDrag(target);
    move(target, indexOf(event.clientX));
    // The slider being dragged takes focus: the keys go on where the pointer left off.
    ref.current
      ?.querySelector<HTMLElement>(`[data-thumb="${target}"]`)
      ?.focus({ preventScroll: true, focusVisible: false });
  };
  const onPointerMove = (event: React.PointerEvent) => {
    const i = indexOf(event.clientX);
    if (drag) move(drag, i);
    // Kept while dragging too: once dropped, the card is about where the pointer is.
    if (event.pointerType !== "touch") {
      setHover(i);
      if (!drag) setOver(handleAt(event));
    }
  };
  const end = (event: React.PointerEvent) => {
    setDrag(null);
    const touch = event.pointerType === "touch";
    setHover(touch ? null : indexOf(event.clientX));
    setOver(touch ? null : handleAt(event));
  };

  // Labels: the ends, the cursor and every few versions in between, never touching.
  const labelEvery = Math.max(1, Math.ceil(44 / Math.max(1, gap)));
  const labels: number[] = [];
  for (let i = domain.from; i <= domain.to; i++) {
    const wanted = i === domain.from || i === domain.to || (i - domain.from) % labelEvery === 0;
    if (!wanted) continue;
    const last = labels[labels.length - 1];
    if (last !== undefined && x(i) - x(last) < 36) {
      // The last one wins over a regular one.
      if (i === domain.to) labels[labels.length - 1] = i;
      continue;
    }
    labels.push(i);
  }
  // Dragging: the version the dragged handle is on, said to be its own; over
  // a range handle, the version it is on, the card saying which handle it is.
  const value = { from: range.from, to: range.to, at };
  const named = drag ?? over;
  const card = named !== null ? value[named] : hover;
  const shown = card !== null ? versions[card] : undefined;
  const cursor = versions[at];

  return (
    <div
      ref={ref}
      data-track=""
      className={cn(
        "relative h-[5.5rem] touch-none select-none",
        drag ? "cursor-grabbing" : "cursor-pointer",
      )}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={end}
      onPointerCancel={end}
      onPointerLeave={() => {
        setHover(null);
        setOver(null);
      }}
    >
      {/* How much each version changed: tinted by when, within the range. */}
      <div aria-hidden className="absolute inset-x-0 top-[1.375rem] h-7">
        <Bars
          versions={versions}
          activity={activity}
          max={max}
          scale={scale}
          range={range}
          domain={domain}
          width={width}
          height={28}
        />
        <AfterCursor domain={domain} width={width} range={range} at={at} />
      </div>

      {/* The track, the range on it, and a dot per version when there is room. */}
      <div aria-hidden className="absolute inset-x-0 top-[3.125rem] h-5">
        <span
          className="absolute top-1/2 h-1 -translate-y-1/2 rounded-full bg-border"
          style={{ left: x(domain.from), right: width - x(domain.to) }}
        />
        <span
          className="absolute top-1/2 h-1.5 -translate-y-1/2 rounded-full bg-primary/25"
          style={{ left: x(range.from), width: x(range.to) - x(range.from) }}
        />
        {gap >= 7 &&
          versions.slice(domain.from, domain.to + 1).map((v, k) => {
            const i = domain.from + k;
            return (
              <span
                key={v.id}
                className={cn(
                  "absolute top-1/2 size-1 -translate-1/2 rounded-full",
                  i >= range.from && i <= range.to ? "bg-primary/60" : "bg-faint-foreground/50",
                )}
                style={{ left: x(i) }}
              />
            );
          })}
      </div>

      {/* The cursor: a playhead, its version on top, its line through the bars. */}
      <span
        aria-hidden
        className="pointer-events-none absolute top-[1.125rem] h-[2.625rem] w-0.5 -translate-x-1/2 rounded-full bg-foreground"
        style={{ left: x(at) }}
      />

      {handles && (
        <>
          <Thumb
            target="from"
            left={x(range.from)}
            min={0}
            max={range.to - 1}
            value={range.from}
            text={describeVersion(versions[range.from]!, activity[range.from])}
            onKeyDown={keys("from", range.from)}
          />
          <Thumb
            target="to"
            left={x(range.to)}
            min={range.from + 1}
            max={versions.length - 1}
            value={range.to}
            text={describeVersion(versions[range.to]!, activity[range.to])}
            onKeyDown={keys("to", range.to)}
          />
        </>
      )}
      <Thumb
        target="at"
        left={clamp(x(at), 18, width - 18)}
        min={range.from}
        max={range.to}
        value={at}
        text={cursor ? describeVersion(cursor, activity[at]) : ""}
        onKeyDown={keys("at", at)}
      >
        v{cursor?.number}
      </Thumb>

      <div aria-hidden data-lane-labels="" className="absolute inset-x-0 top-[4.5rem] h-4">
        {labels.map((i) => (
          <span
            key={versions[i]!.id}
            className="absolute -translate-x-1/2 font-mono text-xs whitespace-nowrap text-faint-foreground tabular"
            // Whole, at both ends: v1000 is wider than its place's margin.
            style={{
              left: clamp(
                x(i),
                labelWidth(`v${versions[i]!.number}`, true) / 2,
                width - labelWidth(`v${versions[i]!.number}`, true) / 2,
              ),
            }}
          >
            v{versions[i]!.number}
          </span>
        ))}
      </div>

      {shown && card !== null && (
        <VersionCard
          version={shown}
          changes={activity[card]}
          kind={drag ?? "hover"}
          title={
            named ? `${THUMB_LABEL[named]}${drag ? "" : " · drag to change the range"}` : undefined
          }
          left={x(card)}
          width={width}
        />
      )}
    </div>
  );
}

/**
 * A long history, whole: its bars, the range as a window with a handle at
 * each end (drag one to change the range, the window to move it), the
 * cursor's place, and dates as ticks (days to years, whichever have room).
 */
function Overview({
  width,
  max,
  all,
  ...props
}: TimelineProps & { width: number; max: number; all: Range }) {
  const { versions, activity, range, at, scale, onRange, onAt } = props;
  const n = versions.length;
  const ref = React.useRef<HTMLDivElement>(null);
  const [hover, setHover] = React.useState<number | null>(null);
  const [over, setOver] = React.useState<"from" | "to" | "window" | null>(null);
  const [drag, setDrag] = React.useState<"from" | "to" | "window" | null>(null);
  // A window being moved: where it was taken (from its start), and what it was then.
  const grab = React.useRef({ offset: 0, from: 0, to: 0, at: 0 });
  const x = xOf(all, width);
  const ticks = React.useMemo(() => dateTicks(versions, all, width), [versions, all, width]);
  const { move, keys } = movesOf(versions.length, { range, at, onRange, onAt });
  const indexOf = (clientX: number) =>
    indexAt(all, width, clientX - ref.current!.getBoundingClientRect().left);
  // The handles sit outside the window, so that two close ends never cover each other.
  const handleX = { from: x(range.from) - 5, to: x(range.to) + 5 };

  /** What the pointer is on: a handle (the nearer), the window, or neither. */
  const partAt = (event: React.PointerEvent): "from" | "to" | "window" | null => {
    const box = ref.current!.getBoundingClientRect();
    const px = event.clientX - box.left;
    const y = event.clientY - box.top;
    // A finger gets a 44px target around each handle; a pointer, the handle and a little more.
    const mouse = event.pointerType === "mouse";
    const reach = mouse ? 8 : 22;
    if (y <= (mouse ? 24 : 34)) {
      const fromIs = Math.abs(px - handleX.from);
      const toIs = Math.abs(px - handleX.to);
      if (Math.min(fromIs, toIs) <= reach) return toIs <= fromIs ? "to" : "from";
    }
    if (px >= x(range.from) && px <= x(range.to)) return "window";
    return null;
  };

  const slide = (i: number) => {
    const g = grab.current;
    const length = g.to - g.from;
    const from = clamp(i - g.offset, 0, n - 1 - length);
    onRange({ from, to: from + length }, clamp(g.at + from - g.from, from, from + length));
  };

  const onPointerDown = (event: React.PointerEvent) => {
    if (event.button !== 0) return;
    const part = partAt(event);
    const target = part === "from" || part === "to" ? part : "window";
    const i = indexOf(event.clientX);
    event.preventDefault();
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    setDrag(target);
    if (target === "window") {
      // Taken where pressed; pressed outside it, the window centers there.
      const inside = i >= range.from && i <= range.to;
      grab.current = {
        offset: inside ? i - range.from : Math.floor((range.to - range.from) / 2),
        from: range.from,
        to: range.to,
        at,
      };
      slide(i);
    } else {
      move(target, i);
      ref.current
        ?.querySelector<HTMLElement>(`[data-thumb="${target}"]`)
        ?.focus({ preventScroll: true, focusVisible: false });
    }
  };
  const onPointerMove = (event: React.PointerEvent) => {
    const i = indexOf(event.clientX);
    if (drag === "window") slide(i);
    else if (drag) move(drag, i);
    if (event.pointerType !== "touch") {
      setHover(i);
      if (!drag) setOver(partAt(event));
    }
  };
  const end = (event: React.PointerEvent) => {
    setDrag(null);
    const touch = event.pointerType === "touch";
    setHover(touch ? null : indexOf(event.clientX));
    setOver(touch ? null : partAt(event));
  };

  const named = drag ?? over;
  const card = named === "from" || named === "to" ? range[named] : hover;
  const shown = card !== null ? versions[card] : undefined;
  const title =
    named === "window"
      ? drag
        ? `Range ${versionName(versions[range.from])} → ${versionName(versions[range.to])}`
        : "Range · drag to move it"
      : named
        ? `${THUMB_LABEL[named]}${drag ? "" : " · drag to change the range"}`
        : undefined;

  return (
    <div
      ref={ref}
      data-track-overview=""
      className={cn(
        "relative mb-1 h-11 touch-none select-none",
        drag ? "cursor-grabbing" : over === "window" ? "cursor-grab" : "cursor-pointer",
      )}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={end}
      onPointerCancel={end}
      onPointerLeave={() => {
        setHover(null);
        setOver(null);
      }}
    >
      {/* The range: a window on the whole history. */}
      <span
        aria-hidden
        data-range-window=""
        className="absolute top-0 h-6 rounded-xs bg-primary/10 ring-1 ring-primary/60 ring-inset"
        style={{ left: x(range.from), width: Math.max(2, x(range.to) - x(range.from)) }}
      />
      <div aria-hidden className="absolute inset-x-0 top-1 h-4">
        <Bars
          versions={versions}
          activity={activity}
          max={max}
          scale={scale}
          range={range}
          domain={all}
          width={width}
          height={16}
        />
      </div>
      <span
        aria-hidden
        className="pointer-events-none absolute top-0 h-6 w-0.5 -translate-x-1/2 rounded-full bg-foreground"
        style={{ left: x(at) }}
      />
      <Thumb
        target="from"
        className="top-3"
        left={handleX.from}
        min={0}
        max={range.to - 1}
        value={range.from}
        text={describeVersion(versions[range.from]!, activity[range.from])}
        onKeyDown={keys("from", range.from)}
      />
      <Thumb
        target="to"
        className="top-3"
        left={handleX.to}
        min={range.from + 1}
        max={n - 1}
        value={range.to}
        text={describeVersion(versions[range.to]!, activity[range.to])}
        onKeyDown={keys("to", range.to)}
      />

      {/* Dates: where each period's first version is. */}
      <div aria-hidden className="absolute inset-x-0 top-7 h-4">
        {ticks.map((t) => (
          <span
            key={t.at}
            className="absolute flex items-center gap-1 text-xs whitespace-nowrap text-faint-foreground tabular"
            style={{ left: clamp(x(t.at), 0, width - labelWidth(t.label) - 5) }}
          >
            <span className="h-3 w-px bg-border-strong" />
            {t.label}
          </span>
        ))}
      </div>

      {shown && card !== null && (
        <VersionCard
          version={shown}
          changes={activity[card]}
          kind={drag ?? "hover"}
          title={title}
          left={x(card)}
          width={width}
        />
      )}
    </div>
  );
}

function Thumb({
  target,
  left,
  min,
  max,
  value,
  text,
  onKeyDown,
  className,
  children,
}: {
  target: Target;
  left: number;
  min: number;
  max: number;
  value: number;
  text: string;
  onKeyDown: (event: React.KeyboardEvent) => void;
  className?: string;
  children?: React.ReactNode;
}) {
  const cursor = target === "at";
  return (
    <div
      role="slider"
      tabIndex={0}
      data-thumb={target}
      aria-label={THUMB_LABEL[target]}
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
        className,
      )}
      style={{ left }}
    >
      {children}
    </div>
  );
}

/**
 * The timeline on a phone, with the sheet closed: a strip of the track's
 * bars in their colors and the playhead, nothing else. Tap or drag it to
 * scrub the cursor (within the range); it is the cursor's slider for the
 * keyboard too. Long histories show the range alone, as the track's detail
 * lane does. While the sheet is open over it, `inert`.
 */
export function TimelineStrip({
  inert,
  className,
  ...props
}: TimelineProps & { inert?: boolean; className?: string }) {
  const { versions, activity, range, at, scale, onRange, onAt } = props;
  const ref = React.useRef<HTMLDivElement>(null);
  const width = useWidth(ref);
  const n = versions.length;
  const all = React.useMemo(() => ({ from: 0, to: n - 1 }), [n]);
  const domain = isLong(n, width) ? range : all;
  const max = React.useMemo(() => Math.max(1, ...activity.slice(1)), [activity]);
  const [drag, setDrag] = React.useState(false);
  const { keys } = movesOf(versions.length, { range, at, onRange, onAt });
  const scrub = (clientX: number) =>
    onAt(
      clamp(
        indexAt(domain, width, clientX - ref.current!.getBoundingClientRect().left),
        range.from,
        range.to,
      ),
    );
  const cursor = versions[at];
  return (
    <div
      ref={ref}
      role="slider"
      tabIndex={0}
      data-timeline-strip=""
      // Under the open sheet: out of reach and out of the accessibility tree
      // (the sheet's own cursor is the one slider then).
      inert={inert}
      aria-hidden={inert || undefined}
      aria-label={THUMB_LABEL.at}
      aria-valuemin={range.from}
      aria-valuemax={range.to}
      aria-valuenow={at}
      aria-valuetext={cursor ? describeVersion(cursor, activity[at]) : ""}
      aria-orientation="horizontal"
      onKeyDown={keys("at", at)}
      onPointerDown={(event) => {
        if (event.button !== 0) return;
        event.preventDefault();
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
        setDrag(true);
        scrub(event.clientX);
      }}
      onPointerMove={(event) => drag && scrub(event.clientX)}
      onPointerUp={() => setDrag(false)}
      onPointerCancel={() => setDrag(false)}
      className={cn(
        "relative h-7 cursor-pointer touch-none rounded-sm outline-none select-none",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
        className,
      )}
    >
      {width > 0 && (
        <>
          <div aria-hidden className="absolute inset-x-0 top-1 h-5">
            <Bars
              versions={versions}
              activity={activity}
              max={max}
              scale={scale}
              range={range}
              domain={domain}
              width={width}
              height={20}
            />
            <AfterCursor domain={domain} width={width} range={range} at={at} />
          </div>
          <span
            aria-hidden
            className="pointer-events-none absolute top-0 h-7 w-0.5 -translate-x-1/2 rounded-full bg-foreground"
            style={{ left: xOf(domain, width)(at) }}
          />
        </>
      )}
    </div>
  );
}

/**
 * Play, pause and step: the cursor through the range. All versions, the
 * highlight walks through them; one version, the schema evolves.
 */
export function Transport({
  mode,
  playing,
  onPlayingChange,
  canBack,
  canForward,
  onStep,
}: {
  mode: Mode;
  playing: boolean;
  onPlayingChange: (playing: boolean) => void;
  canBack: boolean;
  canForward: boolean;
  onStep: (by: -1 | 1) => void;
}) {
  const play =
    mode === "cursor"
      ? "Play the range: the schema evolves"
      : "Play the range: the highlight walks through it";
  return (
    <div className="flex shrink-0 items-center gap-0.5">
      <Tooltip content={playing ? "Pause" : play} shortcut={["K"]} side="top">
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

/**
 * The version at the cursor: its number, message, origin and who made it
 * when. On a phone, where the modes are in the sheet, a second line names
 * the mode the graph is in.
 */
export function CursorReadout({
  version,
  mode,
  compact = false,
  live = true,
}: {
  version: TimelineVersion;
  mode?: Mode;
  compact?: boolean;
  /** Read out when it changes (not while playing: a version every half second). */
  live?: boolean;
}) {
  return (
    <div
      data-cursor-readout=""
      className="flex min-w-0 flex-1 flex-col justify-center"
      aria-live={live ? "polite" : "off"}
    >
      <div className="flex min-w-0 items-center gap-2.5">
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
      {mode && (
        <span data-mode-name="" className="truncate text-xs text-muted-foreground md:hidden">
          {MODE_NAME[mode]}
        </span>
      )}
    </div>
  );
}

/**
 * All versions or one version, with a line saying what the graph shows:
 * "All 8 versions at once, v4 highlighted", "The schema at v4".
 */
export function ModeToggle({
  mode,
  onMode,
  range,
  at,
  versions,
  away,
  className,
}: {
  mode: Mode;
  onMode: (mode: Mode) => void;
  range: Range;
  at: number;
  versions: TimelineVersion[];
  /** What the frame draws faded (superposed). */
  away: Frame["away"];
  className?: string;
}) {
  const caption = React.useId();
  // No tooltips on the items: a tooltip's trigger state would overwrite
  // theirs (`data-state`), and the chosen mode would not show. The chosen
  // one is inverted: on the timeline's card, the kit's raised item would
  // barely differ from its track.
  // On a phone (the sheet), items as tall as the kit's buttons there (36px)
  // for a thumb: the kit's toggle is sized for a pointer.
  const item =
    "max-md:h-9 max-md:px-3 data-[state=on]:bg-foreground data-[state=on]:text-background data-[state=on]:shadow-raised data-[state=on]:hover:text-background";
  return (
    <div className={cn("flex min-w-0 flex-col items-start gap-1", className)}>
      <ToggleGroup
        type="single"
        value={mode}
        onValueChange={(next) => next && onMode(next as Mode)}
        aria-label="What the graph shows"
        aria-describedby={caption}
        aria-keyshortcuts="M"
        className="bg-background ring-1 ring-border-strong ring-inset max-md:h-10"
      >
        <ToggleGroupItem
          value="superposed"
          className={item}
          aria-description="Every version of the range at once, what is not at the cursor faded"
        >
          <CombineIcon />
          {MODE_NAME.superposed}
        </ToggleGroupItem>
        <ToggleGroupItem
          value="cursor"
          className={item}
          aria-description="Exactly the schema at the cursor"
        >
          <KeyframeIcon />
          {MODE_NAME.cursor}
        </ToggleGroupItem>
      </ToggleGroup>
      <p id={caption} data-mode-caption="" className="truncate text-xs text-muted-foreground">
        {modeCaption(mode, range, at, versions, away)}
      </p>
    </div>
  );
}

/**
 * What the colors say: when things appeared (the scale's runs, oldest
 * first, each color the versions it covers), what was there at the start,
 * what went, what changed; all versions, what is not at the cursor.
 */
export function Legend({
  scale,
  versions,
  mode,
  at,
}: {
  scale: Scale;
  versions: TimelineVersion[];
  mode: Mode;
  at: number;
}) {
  const start = versions[scale.range.from];
  const name = (i: number) => `v${versions[i]?.number ?? "?"}`;
  const cursor = name(at);
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-4 gap-y-1 text-xs text-muted-foreground">
      <span className="flex items-center gap-1.5" data-legend-key="start">
        {/* The range start's bar on the track is drawn the same: uncolored. */}
        <span aria-hidden className={cn("size-2.5", START_KEY)} />
        In {start ? `v${start.number}` : "the first version"}
      </span>
      {scale.buckets.length > 0 && (
        <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <span>Appeared in</span>
          {scale.buckets.map((b) => (
            <span key={b.first} data-era-key={b.slot + 1} className="flex items-center gap-1">
              {/* After the cursor: the swatch fades, as their bars do; the
                  words step back to muted ink, never below 4.5:1. */}
              <span
                aria-hidden
                className={cn("size-2.5 rounded-xs", ERA_BG[b.slot], b.first > at && "opacity-50")}
              />
              <span
                className={cn(
                  "font-mono tabular",
                  b.first > at ? "text-muted-foreground" : "text-foreground",
                )}
              >
                {b.first === b.last
                  ? name(b.first)
                  : `${name(b.first)}–${versions[b.last]?.number}`}
              </span>
            </span>
          ))}
        </span>
      )}
      {/* All versions, the two faded states: keys of the encoding, shown
          whether or not the frame has one (playing never reflows them). The
          outlines stay visible (≥ 3:1 for the solid ones): a key that cannot
          be seen keys nothing. */}
      {mode === "superposed" && (
        <span className="flex items-center gap-1.5">
          <span
            aria-hidden
            className="h-2.5 w-4 rounded-xs border border-dashed border-faint-foreground bg-card/60"
          />
          <span className="line-through">Gone by {cursor}</span>
        </span>
      )}
      {mode === "superposed" && (
        <span className="flex items-center gap-1.5">
          <span
            aria-hidden
            className="h-2.5 w-4 rounded-xs bg-card/60 ring-1 ring-faint-foreground/45 ring-inset"
          />
          Not yet in {cursor}
        </span>
      )}
      <span className="flex items-center gap-1.5">
        <span aria-hidden className="font-mono font-semibold text-warning">
          ~
        </span>
        Changed
      </span>
    </div>
  );
}
