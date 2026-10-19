// The age scale of a range of versions: what colors an element by when it
// appeared. Six distinct hues at most (`--era-1` to `--era-6`, categorical
// and color-blind safe, ≥ 3:1 on a table in both themes): a range of more
// versions groups them into runs, oldest first, and the legend says which
// versions each color covers.

/** A range of versions, as indexes into the timeline (`from` ≤ `to`). */
export interface Range {
  from: number;
  to: number;
}

export const SCALE_SLOTS = 6;

export interface Bucket {
  /** First and last version (indexes) it covers. */
  first: number;
  last: number;
  /** Its color, 0 (`--era-1`, the oldest) to 5. */
  slot: number;
}

export interface Scale {
  range: Range;
  /** Oldest first; empty for a range of one version. */
  buckets: Bucket[];
}

/** The scale of a range: each version after its first one in a bucket. */
export function scaleOf(range: Range): Scale {
  const n = range.to - range.from;
  if (n <= 0) return { range, buckets: [] };
  const k = Math.min(n, SCALE_SLOTS);
  const buckets: Bucket[] = [];
  for (let i = range.from + 1; i <= range.to; i++) {
    const slot = Math.floor(((i - range.from - 1) * k) / n);
    const last = buckets[buckets.length - 1];
    if (last && last.slot === slot) last.last = i;
    else buckets.push({ first: i, last: i, slot });
  }
  return { range, buckets };
}

/** The color slot of a version (index), `undefined` outside (`from`, `to`]. */
export function eraOf(scale: Scale, index: number | undefined): number | undefined {
  if (index === undefined || index <= scale.range.from || index > scale.range.to) return undefined;
  return scale.buckets.find((b) => index >= b.first && index <= b.last)?.slot;
}

/** Tailwind classes per color slot: literal, so the build keeps them. */
export const ERA_BG = ["bg-era-1", "bg-era-2", "bg-era-3", "bg-era-4", "bg-era-5", "bg-era-6"];
