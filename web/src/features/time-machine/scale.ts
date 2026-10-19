// The age scale of a range of versions: what tints an element by when it
// appeared. One hue, light to dark (dark to light in the dark theme), six
// steps at most (`--era-1` to `--era-6`): a range of more versions groups
// them into runs, oldest first.

/** A range of versions, as indexes into the timeline (`from` ≤ `to`). */
export interface Range {
  from: number;
  to: number;
}

export const SCALE_STEPS = 6;

/**
 * The ramp steps a scale of `n` buckets uses: few buckets take the darker,
 * more distinct end and stay apart.
 */
const STEPS: Record<number, number[]> = {
  1: [5],
  2: [3, 5],
  3: [1, 3, 5],
  4: [2, 3, 4, 5],
  5: [1, 2, 3, 4, 5],
  6: [0, 1, 2, 3, 4, 5],
};

export interface Bucket {
  /** First and last version (indexes) it covers. */
  first: number;
  last: number;
  /** Its step on the ramp, 0 (oldest) to 5. */
  step: number;
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
  const k = Math.min(n, SCALE_STEPS);
  const steps = STEPS[k]!;
  const buckets: Bucket[] = [];
  for (let i = range.from + 1; i <= range.to; i++) {
    const b = Math.floor(((i - range.from - 1) * k) / n);
    const last = buckets[buckets.length - 1];
    if (last && last.step === steps[b]) last.last = i;
    else buckets.push({ first: i, last: i, step: steps[b]! });
  }
  return { range, buckets };
}

/** The ramp step of a version (index), `undefined` outside (`from`, `to`]. */
export function stepOf(scale: Scale, index: number): number | undefined {
  if (index <= scale.range.from || index > scale.range.to) return undefined;
  return scale.buckets.find((b) => index >= b.first && index <= b.last)?.step;
}

/** Tailwind classes per ramp step: literal, so the build keeps them. */
export const STEP_BG = ["bg-era-1", "bg-era-2", "bg-era-3", "bg-era-4", "bg-era-5", "bg-era-6"];
