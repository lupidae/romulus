// Where versions sit on the timeline's lanes: positions, the bars a lane
// draws (one per version, or one per few when they would overlap), the
// overview's date ticks. Pure: the components measure, this places.

import type { Range } from "./scale";

/** Room at both ends of a lane, so the end handles are whole. */
export const PAD = 14;

/**
 * Narrower than this per version (px), the track splits in two: the whole
 * history as an overview, the range in detail below it (docs/MVP.md "The
 * time machine", long histories).
 */
export const LONG_SPACING = 4;

/** Pixels between two neighbouring versions on a lane `width` wide showing `domain`. */
export function spacing(domain: Range, width: number): number {
  const count = domain.to - domain.from;
  return count <= 0 ? width : (width - 2 * PAD) / count;
}

/** Where version `i` sits on a lane `width` wide showing `domain`. */
export function xOf(domain: Range, width: number): (i: number) => number {
  const count = domain.to - domain.from;
  if (count <= 0) return () => width / 2;
  const step = (width - 2 * PAD) / count;
  return (i) => PAD + step * (i - domain.from);
}

/** The version nearest to `px` (from the lane's left edge), within `domain`. */
export function indexAt(domain: Range, width: number, px: number): number {
  const count = domain.to - domain.from;
  const t = (px - PAD) / Math.max(1, width - 2 * PAD);
  return Math.max(domain.from, Math.min(domain.to, domain.from + Math.round(t * count)));
}

/** Whether `n` versions are too many for one lane `width` wide. */
export function isLong(n: number, width: number): boolean {
  return width > 0 && n > 2 && spacing({ from: 0, to: n - 1 }, width) < LONG_SPACING;
}

/** A bar of a lane: versions `first` to `last`, drawn as `top` (the one that changed most). */
export interface Column {
  first: number;
  last: number;
  top: number;
}

/**
 * The bars of a lane: one per version while they are `min` px apart or
 * more, else one per run of neighbours (each drawn as the version of the run
 * that changed most), so a thousand versions are a few hundred bars, never
 * overlapping.
 */
export function columns(
  domain: Range,
  width: number,
  activity: readonly number[],
  min = 3,
): Column[] {
  const gap = spacing(domain, width);
  const step = gap >= min ? 1 : Math.ceil(min / Math.max(gap, 0.01));
  const out: Column[] = [];
  for (let first = domain.from; first <= domain.to; first += step) {
    const last = Math.min(domain.to, first + step - 1);
    let top = first;
    for (let i = first + 1; i <= last; i++) if ((activity[i] ?? 0) > (activity[top] ?? 0)) top = i;
    out.push({ first, last, top });
  }
  return out;
}

/**
 * About how wide a label of the lanes is (px): their text is 12px, ~6.6px a
 * character set in the UI font, 7.3px in the mono; the date ticks' mark adds 5.
 */
export function labelWidth(label: string, mono = false): number {
  return Math.ceil(label.length * (mono ? 7.3 : 6.6));
}

const tickWidth = (label: string) => labelWidth(label) + 5;

export interface Tick {
  /** The first version (index) of its period. */
  at: number;
  label: string;
}

type Unit = "day" | "week" | "month" | "quarter" | "year";
const UNITS: Unit[] = ["day", "week", "month", "quarter", "year"];

const monthDay = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });
const monthDayYear = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
});
const month = new Intl.DateTimeFormat("en-US", { month: "short" });
const monthYear = new Intl.DateTimeFormat("en-US", { month: "short", year: "numeric" });

/** The period a date falls in, as a key: two dates share one when equal. */
function periodOf(date: Date, unit: Unit): string {
  const y = date.getFullYear();
  if (unit === "year") return `${y}`;
  if (unit === "month") return `${y}-${date.getMonth()}`;
  if (unit === "quarter") return `${y}-q${Math.floor(date.getMonth() / 3)}`;
  if (unit === "day") return `${y}-${date.getMonth()}-${date.getDate()}`;
  // Weeks start on Monday.
  const monday = new Date(y, date.getMonth(), date.getDate() - ((date.getDay() + 6) % 7));
  return `${monday.getFullYear()}-${monday.getMonth()}-${monday.getDate()}`;
}

/**
 * Date ticks for a lane: the first version of each day, week, month, quarter
 * or year (the finest unit with room, `gap` px a tick), labelled "Oct 6",
 * "Oct", "2027"; the year said on the first tick and where it changes. Versions are
 * placed by number, not by date: a tick is where its period's first version
 * is. A tick closer to the one before than `gap` × 0.75, or than that
 * one's label is wide, is left out (the lane's first, which starts no period
 * but its own, gives way to the next).
 */
export function dateTicks(
  versions: readonly { created_at: string }[],
  domain: Range,
  width: number,
  gap = 56,
): Tick[] {
  if (domain.to <= domain.from || width <= 0) return [];
  const dates: Date[] = [];
  for (let i = domain.from; i <= domain.to; i++) dates.push(new Date(versions[i]!.created_at));
  const room = Math.max(1, Math.floor((width - 2 * PAD) / gap));
  const starts = (unit: Unit) => {
    const out: number[] = [];
    let before = "";
    dates.forEach((d, k) => {
      const key = periodOf(d, unit);
      if (key !== before) out.push(domain.from + k);
      before = key;
    });
    return out;
  };
  let unit: Unit = "year";
  let at: number[] = [];
  for (const u of UNITS) {
    at = starts(u);
    unit = u;
    if (at.length <= room) break;
  }
  const x = xOf(domain, width);
  const labelOf = (i: number, newYear: boolean) => {
    const date = dates[i - domain.from]!;
    return unit === "year"
      ? `${date.getFullYear()}`
      : unit === "month" || unit === "quarter"
        ? (newYear ? monthYear : month).format(date)
        : (newYear ? monthDayYear : monthDay).format(date);
  };
  const yearOf = (i: number) => dates[i - domain.from]!.getFullYear();
  const ticks: Tick[] = [];
  for (const i of at) {
    const last = ticks[ticks.length - 1];
    const label = labelOf(i, !last || yearOf(i) !== yearOf(last.at));
    if (!last || x(i) - x(last.at) >= Math.max(gap * 0.75, tickWidth(last.label) + 8)) {
      ticks.push({ at: i, label });
    } else if (ticks.length === 1 && last.at === domain.from) {
      // The domain's start is a period's first version only by chance: a
      // real start too close to it takes its place (and says the year).
      ticks[0] = { at: i, label: labelOf(i, true) };
    }
  }
  return ticks;
}
