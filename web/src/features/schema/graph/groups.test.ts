import { describe, expect, it } from "vitest";

import type { Groups } from "../layout";
import { GROUP_PADDING } from "./geometry";
import {
  applyGroupsChange,
  drawnMembers,
  dropMembership,
  frameOf,
  groupAt,
  groupOfTable,
  movedMembers,
  nextGroupName,
  regroup,
} from "./groups";

const box = (x: number, y: number, width = 200, height = 100) => ({ x, y, width, height });

const groups: Groups = {
  b: { name: "Billing", color: "blue", members: ["public.invoices", "public.payments"] },
  a: { name: "Auth", color: "teal", members: ["public.users", "public.sessions"] },
};

describe("a group's frame", () => {
  it("wraps its tables, with room for the label above them", () => {
    expect(frameOf([box(0, 0), box(300, 200, 100, 50)])).toEqual({
      x: -GROUP_PADDING.side,
      y: -GROUP_PADDING.top,
      width: 400 + GROUP_PADDING.side * 2,
      height: 250 + GROUP_PADDING.top + GROUP_PADDING.bottom,
    });
  });

  it("is nowhere while none of its tables is placed", () => {
    expect(frameOf([])).toBeNull();
  });

  it("follows its tables when they move", () => {
    const before = frameOf([box(0, 0), box(300, 0)])!;
    const moved = movedMembers({ a: { x: 0, y: 0 }, b: { x: 300, y: 0 } }, ["a", "b"], 40.4, -10);
    const after = frameOf(Object.values(moved).map((p) => box(p.x, p.y)))!;
    expect(after).toEqual({ ...before, x: before.x + 40, y: before.y - 10 });
  });
});

describe("membership", () => {
  it("puts a table listed twice in the first group by id", () => {
    const twice: Groups = { ...groups, c: { name: "C", color: "red", members: ["public.users"] } };
    expect(groupOfTable(twice).get("public.users")).toBe("a");
    expect(drawnMembers(twice).get("c")).toEqual([]);
  });

  it("moves a table between groups, out of every other one", () => {
    const change = regroup(groups, ["public.users"], "b");
    expect(change.set.b!.members).toEqual(["public.invoices", "public.payments", "public.users"]);
    expect(change.set.a!.members).toEqual(["public.sessions"]);
    expect(change.deleted).toEqual([]);
  });

  it("takes tables out of their group with null", () => {
    const after = applyGroupsChange(groups, regroup(groups, ["public.invoices"], null));
    expect(after.b!.members).toEqual(["public.payments"]);
    expect(after.a).toEqual(groups.a);
  });

  it("deletes a group its last table left", () => {
    const change = regroup(groups, ["public.users", "public.sessions"], "b");
    expect(change.deleted).toEqual(["a"]);
    expect(applyGroupsChange(groups, change).a).toBeUndefined();
  });

  it("writes nothing when the tables are already where they go", () => {
    expect(regroup(groups, ["public.invoices"], "b")).toEqual({ set: {}, deleted: [] });
  });

  it("names a new group after the others", () => {
    expect(nextGroupName({})).toBe("Group 1");
    expect(nextGroupName(groups)).toBe("Group 3");
    expect(nextGroupName({ ...groups, x: { name: "Group 3", color: "red", members: [] } })).toBe(
      "Group 4",
    );
  });
});

describe("a table dropped on the graph", () => {
  const frames = new Map([
    ["b", box(0, 0, 600, 400)],
    ["a", box(1000, 0, 400, 400)],
    // A small group drawn over a corner of Billing.
    ["c", box(500, 300, 300, 300)],
  ]);

  it("finds the smallest group under the pointer", () => {
    expect(groupAt({ x: 100, y: 100 }, frames)).toBe("b");
    expect(groupAt({ x: 550, y: 350 }, frames)).toBe("c");
    expect(groupAt({ x: 900, y: 100 }, frames)).toBeNull();
  });

  it("joins the group it is dropped in", () => {
    expect(dropMembership(null, { x: 1100, y: 100 }, frames)).toEqual({ group: "a" });
    expect(dropMembership("b", { x: 1100, y: 100 }, frames)).toEqual({ group: "a" });
  });

  it("stays in its group when dropped inside it", () => {
    expect(dropMembership("b", { x: 100, y: 100 }, frames)).toBeNull();
  });

  it("leaves its group when dropped outside what the others span", () => {
    expect(dropMembership("b", { x: 900, y: 100 }, frames)).toEqual({ group: null });
  });

  it("carries a group whose only table it is, unless dropped in another one", () => {
    const alone = new Map([["b", box(0, 0, 600, 400)]]);
    expect(dropMembership("solo", { x: 900, y: 900 }, alone)).toBeNull();
    expect(dropMembership("solo", { x: 100, y: 100 }, alone)).toEqual({ group: "b" });
  });

  it("does nothing outside every group when it had none", () => {
    expect(dropMembership(null, { x: 900, y: 100 }, frames)).toBeNull();
  });
});
