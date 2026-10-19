import { beforeEach, describe, expect, it } from "vitest";

import { clear, load, save, seed } from "./store";

const KEY = "romulus:hero:1";

const kept = (value: unknown) => localStorage.setItem(KEY, JSON.stringify(value));

describe("the hero's kept state", () => {
  beforeEach(() => clear());

  it("reads back what it saved", () => {
    const state = seed();
    save(state);
    expect(load()).toEqual(state);
  });

  it("is nothing when nothing was kept", () => {
    expect(load()).toBeNull();
  });

  it("forgets text that is not JSON", () => {
    localStorage.setItem(KEY, "{not json");
    expect(load()).toBeNull();
    expect(localStorage.getItem(KEY)).toBeNull();
  });

  const broken: [string, (state: ReturnType<typeof seed>) => unknown][] = [
    ["a value that is not an object", () => 42],
    ["a position that is not numbers", (s) => ({ ...s, layout: { "public.t": { x: "1", y: 2 } } })],
    ["a group without members", (s) => ({ ...s, groups: { g: { name: "G", color: "blue" } } })],
    [
      "a thread with an unknown anchor",
      (s) => ({
        ...s,
        threads: {
          t: { anchor: { kind: "galaxy" }, by: "x", at: s.versions[0]!.savedAt, resolved: null },
        },
        comments: {},
      }),
    ],
    [
      "a comment of a thread that is not there",
      (s) => ({
        ...s,
        comments: {
          c: {
            thread: "gone",
            author: { id: "x", name: "X" },
            body: "hi",
            mentions: [],
            at: s.versions[0]!.savedAt,
            edited: null,
          },
        },
      }),
    ],
    [
      "a comment whose time is not a time",
      (s) => {
        const [id, comment] = Object.entries(s.comments)[0]!;
        return { ...s, comments: { ...s.comments, [id]: { ...comment, at: "yesterday" } } };
      },
    ],
    ["no versions", (s) => ({ ...s, versions: [] })],
    ["versions out of order", (s) => ({ ...s, versions: [{ ...s.versions[0]!, number: 2 }] })],
    [
      "a version whose layout is not positions",
      (s) => ({ ...s, versions: [{ ...s.versions[0]!, layout: { "public.t": null } }] }),
    ],
  ];
  it.each(broken)("forgets %s, and starts over", (_, make) => {
    kept(make(seed()));
    expect(load()).toBeNull();
    expect(localStorage.getItem(KEY)).toBeNull();
  });
});
