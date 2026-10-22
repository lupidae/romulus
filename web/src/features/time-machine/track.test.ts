import { describe, expect, it } from "vitest";

import { MODE_NAME, modeCaption } from "./describe";
import type { TimelineVersion } from "./history";
import { columns, dateTicks, indexAt, isLong, labelWidth, PAD, xOf } from "./track";

/** Versions a day apart from Sep 1, 2026 (noon UTC: the same day in any zone near it). */
const daily = (n: number, every = 1) =>
  Array.from({ length: n }, (_, i) => ({
    created_at: new Date(Date.UTC(2026, 8, 1, 12) + i * every * 86_400_000).toISOString(),
  }));

describe("the track's lanes", () => {
  it("places a domain's versions evenly between the pads, and finds them back", () => {
    const x = xOf({ from: 10, to: 20 }, 228);
    expect(x(10)).toBe(PAD);
    expect(x(20)).toBe(228 - PAD);
    expect(x(15)).toBe(114);
    expect(indexAt({ from: 10, to: 20 }, 228, 114)).toBe(15);
    // Outside the lane: its ends.
    expect(indexAt({ from: 10, to: 20 }, 228, -50)).toBe(10);
    expect(indexAt({ from: 10, to: 20 }, 228, 900)).toBe(20);
  });

  it("splits in two past ~4px a version: 1000 versions at 1440px, 120 on a phone", () => {
    expect(isLong(8, 1400)).toBe(false);
    expect(isLong(120, 1400)).toBe(false);
    expect(isLong(1000, 1400)).toBe(true);
    expect(isLong(120, 366)).toBe(true);
    expect(isLong(80, 366)).toBe(false);
    // Not measured yet: nothing decided.
    expect(isLong(1000, 0)).toBe(false);
  });

  it("draws a bar per version while there is room, else one per run, the busiest standing for it", () => {
    const activity = [50, 1, 9, 2, 3, 7, 1, 1];
    expect(columns({ from: 0, to: 7 }, 400, activity)).toHaveLength(8);
    // 8 versions over 12px: runs of 2 (1.7px apart, 3px wanted).
    const runs = columns({ from: 0, to: 7 }, 12 + 2 * PAD, activity);
    expect(runs).toEqual([
      { first: 0, last: 1, top: 0 },
      { first: 2, last: 3, top: 2 },
      { first: 4, last: 5, top: 5 },
      { first: 6, last: 7, top: 6 },
    ]);
    // A thousand versions on 1400px: a few hundred bars, every version in one.
    const many = columns({ from: 0, to: 999 }, 1400, new Array<number>(1000).fill(1));
    expect(many.length).toBeLessThan(500);
    expect(many[0]!.first).toBe(0);
    expect(many[many.length - 1]!.last).toBe(999);
  });
});

describe("the overview's date ticks", () => {
  it("picks the finest unit with room: days, weeks, months, quarters, years", () => {
    const week = daily(8);
    expect(dateTicks(week, { from: 0, to: 7 }, 1000).map((t) => t.label)).toEqual([
      "Sep 1, 2026",
      "Sep 2",
      "Sep 3",
      "Sep 4",
      "Sep 5",
      "Sep 6",
      "Sep 7",
      "Sep 8",
    ]);
    // 300 days on 1400px: months, the year where it changes.
    const months = dateTicks(daily(300), { from: 0, to: 299 }, 1400).map((t) => t.label);
    expect(months.slice(0, 6)).toEqual(["Sep 2026", "Oct", "Nov", "Dec", "Jan 2027", "Feb"]);
    // 1000 days (Sep 2026 to May 2029) on 1400px: quarters.
    const quarters = dateTicks(daily(1000), { from: 0, to: 999 }, 1400).map((t) => t.label);
    // (October, 30 days after the start, too close to it: the start gives way.)
    expect(quarters.slice(0, 4)).toEqual(["Oct 2026", "Jan 2027", "Apr", "Jul"]);
    // Years on a phone (2026's four months too short for a label of their own).
    expect(dateTicks(daily(1000), { from: 0, to: 999 }, 366).map((t) => t.label)).toEqual([
      "2027",
      "2028",
      "2029",
    ]);
  });

  it("leaves room for each label: a tick never starts before the one before ends", () => {
    for (const width of [366, 1000, 1400, 1700]) {
      const x = xOf({ from: 0, to: 999 }, width);
      const ticks = dateTicks(daily(1000), { from: 0, to: 999 }, width);
      for (let k = 1; k < ticks.length; k++)
        expect(
          x(ticks[k]!.at) - x(ticks[k - 1]!.at),
          `${width}px: ${ticks[k - 1]!.label} then ${ticks[k]!.label}`,
        ).toBeGreaterThanOrEqual(labelWidth(ticks[k - 1]!.label) + 5);
    }
  });

  it("puts a tick where its period's first version is, and none too close to another", () => {
    // Two versions a day for 30 days: a tick every week, on the week's first version.
    const busy = daily(60, 0.5);
    const ticks = dateTicks(busy, { from: 0, to: 59 }, 600);
    expect(ticks.length).toBeGreaterThan(2);
    const x = xOf({ from: 0, to: 59 }, 600);
    for (let k = 1; k < ticks.length; k++)
      expect(x(ticks[k]!.at) - x(ticks[k - 1]!.at)).toBeGreaterThanOrEqual(42);
    for (const t of ticks.slice(1))
      expect(new Date(busy[t.at]!.created_at).getDay(), t.label).toBe(1); // Monday
  });
});

describe("the modes in words", () => {
  const versions = Array.from({ length: 8 }, (_, i) => ({
    number: i + 1,
    removed_at: null,
  })) as unknown as TimelineVersion[];

  it("names them in common words, and says what each shows", () => {
    expect(MODE_NAME).toEqual({ superposed: "All versions", cursor: "One version" });
    expect(modeCaption("superposed", { from: 0, to: 7 }, 3, versions)).toBe(
      "All 8 versions at once, v4 highlighted",
    );
    expect(modeCaption("superposed", { from: 2, to: 7 }, 7, versions)).toBe(
      "6 versions (v3 → v8) at once, v8 highlighted",
    );
    expect(modeCaption("superposed", { from: 2, to: 3 }, 3, versions, { gone: 0, later: 0 })).toBe(
      "v3 and v4 at once: all of it is in v4",
    );
    expect(modeCaption("cursor", { from: 0, to: 7 }, 3, versions)).toBe("The schema at v4");
  });
});
