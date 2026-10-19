import { describe, expect, it } from "vitest";

import { buildGraph } from "@/features/schema/graph/build";

import { describeChange, eventLine, eventsOf } from "./describe";
import { column, model, table, timelineFrom } from "./fixtures";
import { type FrameRequest, Frames } from "./frame";
import {
  activity,
  bornWithin,
  changesWithin,
  goneWithin,
  type History,
  type ObjectChange,
  parseHistory,
  stateAt,
} from "./history";
import { scaleOf, stepOf } from "./scale";

// v1: customers, orders (orders → customers).
// v2: customers gains `email`; `legacy` appears.
// v3: `amount` on orders; `legacy` dropped.
// v4: `amount` retyped bigint → numeric; `invoices` appears (→ orders).
const versions = [
  model([
    table("customers", [column("name", "text")]),
    table("orders", [column("customers_id")], ["customers"]),
  ]),
  model([
    table("customers", [column("name", "text"), column("email", "text")]),
    table("orders", [column("customers_id")], ["customers"]),
    table("legacy", []),
  ]),
  model([
    table("customers", [column("name", "text"), column("email", "text")]),
    table("orders", [column("customers_id"), column("amount")], ["customers"]),
  ]),
  model([
    table("customers", [column("name", "text"), column("email", "text")]),
    table("orders", [column("customers_id"), column("amount", "numeric")], ["customers"]),
    table("invoices", [column("orders_id")], ["orders"]),
  ]),
];

function history(): History {
  return parseHistory(
    timelineFrom(versions.map((m, i) => ({ model: m, author: i === 0 ? "Ada" : "Grace" }))),
  );
}

const request = (over: Partial<FrameRequest> = {}): FrameRequest => ({
  range: { from: 0, to: 3 },
  at: 3,
  mode: "superposed",
  ...over,
});

describe("history", () => {
  it("knows each object's states between versions", () => {
    const h = history();
    const legacy = h.tables.find((t) => t.id === "public.legacy")!;
    expect(legacy.states.map((s) => [s.from, s.to])).toEqual([[1, 2]]);
    expect(stateAt(legacy.states, 1)).toBeDefined();
    expect(stateAt(legacy.states, 2)).toBeUndefined();
    expect(bornWithin(legacy.states, 0, 3)).toBe(1);
    expect(goneWithin(legacy.states, 0, 3)).toBe(2);
    expect(goneWithin(legacy.states, 0, 1)).toBeUndefined();

    const amount = h.tables
      .find((t) => t.id === "public.orders")!
      .columns.find((c) => c.id === "amount")!;
    expect(changesWithin(amount.states, 0, 3)).toEqual([3]);
    expect(changesWithin(amount.states, 0, 2)).toEqual([]);
    // Something happened at every version.
    expect(activity(h).every((n) => n > 0)).toBe(true);
  });

  it("refuses a state outside the timeline", () => {
    const timeline = timelineFrom([{ model: versions[0]! }]);
    timeline.history.tables[0]!.states[0]!.from = 4;
    expect(() => parseHistory(timeline)).toThrow(/out of the timeline/);
  });
});

describe("the age scale", () => {
  it("gives each version of a short range its own step, the newest the darkest", () => {
    const scale = scaleOf({ from: 2, to: 5 });
    expect(scale.buckets.map((b) => [b.first, b.last, b.step])).toEqual([
      [3, 3, 1],
      [4, 4, 3],
      [5, 5, 5],
    ]);
    expect(stepOf(scale, 2)).toBeUndefined();
    expect(stepOf(scale, 6)).toBeUndefined();
  });

  it("groups a long range into six runs, oldest first", () => {
    const scale = scaleOf({ from: 0, to: 60 });
    expect(scale.buckets).toHaveLength(6);
    expect(scale.buckets[0]).toEqual({ first: 1, last: 10, step: 0 });
    expect(scale.buckets[5]).toEqual({ first: 51, last: 60, step: 5 });
    expect(scaleOf({ from: 4, to: 4 }).buckets).toEqual([]);
  });
});

describe("an object's history in words", () => {
  it("words changes as romulus-diff structures them", () => {
    const change = (c: object) => describeChange(c as ObjectChange);
    expect(change({ field: "type", before: "bigint", after: "numeric" })).toBe(
      "type bigint → numeric",
    );
    expect(change({ field: "not_null", before: false, after: true })).toBe("made not null");
    expect(change({ field: "name", before: "email", after: "email_address" })).toBe(
      "renamed email → email_address",
    );
    expect(change({ field: "default", before: null, after: "now()" })).toBe("default none → now()");
    expect(change({ field: "labels", before: ["a"], after: ["a", "b"] })).toBe("labels +b");
    expect(change({ field: "kind", before: "table", after: "materialized_view" })).toBe(
      "kind table → materialized view",
    );
  });

  it("lists additions, changes, removals and returns, with who and when", () => {
    const h = history();
    const amount = h.tables
      .find((t) => t.id === "public.orders")!
      .columns.find((c) => c.id === "amount")!;
    const events = eventsOf(amount.states);
    expect(events.map((e) => [e.at, e.kind])).toEqual([
      [2, "added"],
      [3, "changed"],
    ]);
    expect(events.map((e) => eventLine(e, h.versions))).toEqual([
      "v3 · added by Grace",
      "v4 · type bigint → numeric",
    ]);
    const legacy = h.tables.find((t) => t.id === "public.legacy")!;
    expect(eventsOf(legacy.states).map((e) => e.kind)).toEqual(["added", "removed"]);
  });

  it("shows a rename the diff reports as a rename", () => {
    const h = parseHistory(
      timelineFrom([
        { model: model([table("users", [column("email", "text")])]) },
        {
          model: model([table("customers", [column("email_address", "text")])]),
          renames: {
            tables: { "public.users": "public.customers" },
            columns: { "public.customers.email": "email_address" },
          },
        },
      ]),
    );
    expect(h.tables.map((t) => t.id)).toEqual(["public.customers"]);
    const customers = h.tables[0]!;
    expect(eventsOf(customers.states).map((e) => eventLine(e, h.versions))).toEqual([
      "v1 · added",
      "v2 · renamed users → customers",
    ]);
    const email = customers.columns.find((c) => c.id === "email_address")!;
    expect(eventsOf(email.states).map((e) => e.kind)).toEqual(["added", "renamed"]);

    // At v1 the graph keeps its keys (the last names) and shows the names of then.
    const frames = new Frames(h);
    const then = frames.frame({ range: { from: 0, to: 1 }, at: 0, mode: "cursor" });
    const node = then.graph.byKey.get("public.customers")!;
    expect(node.time?.label).toBe("users");
    expect(node.columns.find((c) => c.name === "email_address")?.time?.label).toBe("email");
  });
});

describe("a frame", () => {
  it("superposes the range: removed tables ghosted, added ones tinted by when", () => {
    const frames = new Frames(history());
    const frame = frames.frame(request());
    const keys = frame.graph.tables.map((t) => t.key);
    expect(keys).toEqual(["public.customers", "public.invoices", "public.legacy", "public.orders"]);
    const legacy = frame.graph.byKey.get("public.legacy")!;
    expect(legacy.time).toMatchObject({ ghost: true, note: "−v3" });
    const invoices = frame.graph.byKey.get("public.invoices")!;
    // Three steps after v1: v4 is the newest, the darkest step.
    expect(invoices.time).toMatchObject({ era: 5, note: "+v4" });
    expect(invoices.time?.ghost).toBeUndefined();
    const customers = frame.graph.byKey.get("public.customers")!;
    expect(customers.time?.era).toBeUndefined();
    expect(customers.columns.map((c) => [c.name, c.time?.era])).toEqual([
      ["id", undefined],
      ["name", undefined],
      ["email", 1],
    ]);
    const amount = frame.graph.byKey
      .get("public.orders")!
      .columns.find((c) => c.name === "amount")!;
    expect(amount.type).toBe("numeric");
    expect(amount.time).toMatchObject({ era: 3, changed: true });
    expect(amount.time?.history).toEqual(["v3 · added by Grace", "v4 · type bigint → numeric"]);
    // Edges of what is new are new too.
    const fk = frame.graph.edges.find((e) => e.source === "public.invoices")!;
    expect(fk.time?.era).toBe(5);
    expect(frame.counts.tables).toEqual({ added: 1, removed: 0, changed: 0 });
    expect(frame.counts.columns).toEqual({ added: 2, removed: 0, changed: 0 });
  });

  it("at the cursor, draws exactly that version, as its own graph would", () => {
    const h = history();
    const frames = new Frames(h);
    for (let at = 0; at < versions.length; at++) {
      frames.frame(request({ at, mode: "cursor" }));
      // Once what was leaving has gone.
      const frame = frames.settle()!;
      const own = buildGraph({ schema: versions[at]! });
      expect(frame.graph.tables.map((t) => t.key)).toEqual(own.tables.map((t) => t.key));
      for (const t of own.tables) {
        const drawn = frame.graph.byKey.get(t.key)!;
        expect(drawn.columns.map((c) => [c.name, c.type])).toEqual(
          t.columns.map((c) => [c.name, c.type]),
        );
      }
      expect(frame.graph.edges.map((e) => e.id).sort()).toEqual(own.edges.map((e) => e.id).sort());
      expect(frame.graph.tables.every((t) => !t.time?.ghost)).toBe(true);
    }
  });

  it("replays up to the cursor: what comes later is not there yet, what went is a ghost", () => {
    const frames = new Frames(history());
    const at1 = frames.frame(request({ at: 1 }));
    expect(at1.graph.byKey.has("public.invoices")).toBe(false);
    expect(at1.graph.byKey.get("public.legacy")?.time?.ghost).toBeUndefined();
    const amount = at1.graph.byKey.get("public.orders")!.columns.map((c) => c.name);
    expect(amount).toEqual(["id", "customers_id"]);
  });

  it("keeps a removed column where it was, struck", () => {
    const h = parseHistory(
      timelineFrom([
        { model: model([table("t", [column("a"), column("b"), column("c")])]) },
        { model: model([table("t", [column("a"), column("c"), column("d")])]) },
      ]),
    );
    const frame = new Frames(h).frame({ range: { from: 0, to: 1 }, at: 1, mode: "superposed" });
    const t = frame.graph.byKey.get("public.t")!;
    expect(t.columns.map((c) => [c.name, !!c.time?.ghost])).toEqual([
      ["id", false],
      ["a", false],
      ["b", true],
      ["c", false],
      ["d", false],
    ]);
  });

  it("hands unchanged tables and edges again, the same objects", () => {
    const frames = new Frames(history());
    const first = frames.frame(request({ at: 2 }));
    frames.settle();
    const second = frames.frame(request({ at: 3 }));
    // customers did not change between v3 and v4: the same model.
    expect(second.graph.byKey.get("public.customers")).toBe(
      first.graph.byKey.get("public.customers"),
    );
    // orders did (amount retyped).
    expect(second.graph.byKey.get("public.orders")).not.toBe(
      first.graph.byKey.get("public.orders"),
    );
  });

  it("animates what comes and goes, until it settles", () => {
    const frames = new Frames(history());
    frames.frame(request({ at: 1, mode: "cursor" }));
    frames.settle();
    const next = frames.frame(request({ at: 2, mode: "cursor" }));
    expect(next.graph.byKey.get("public.legacy")?.time?.motion).toBe("leave");
    expect(
      next.graph.byKey.get("public.orders")!.columns.find((c) => c.name === "amount")?.time?.motion,
    ).toBe("enter");
    expect(next.leaving).toBe(1);
    const settled = frames.settle()!;
    expect(settled.graph.byKey.has("public.legacy")).toBe(false);
    expect(settled.graph.tables.every((t) => !t.time?.motion)).toBe(true);
  });

  it("fades back what a filter leaves out", () => {
    const frames = new Frames(history());
    const changed = frames.frame(request({ filters: { changedOnly: true } }));
    expect(changed.graph.byKey.get("public.invoices")?.time?.muted).toBeUndefined();
    const customers = changed.graph.byKey.get("public.customers")!;
    expect(customers.time?.muted).toBeUndefined();
    expect(customers.columns.find((c) => c.name === "name")?.time?.muted).toBe(true);
    expect(customers.columns.find((c) => c.name === "email")?.time?.muted).toBeUndefined();

    const found = new Frames(history()).frame(request({ filters: { query: "INVOI" } }));
    expect(found.graph.byKey.get("public.invoices")?.time?.muted).toBeUndefined();
    expect(found.graph.byKey.get("public.orders")?.time?.muted).toBe(true);
    const group = new Frames(history()).frame(
      request({ filters: { tables: new Set(["public.orders"]) } }),
    );
    expect(group.graph.byKey.get("public.customers")?.time?.muted).toBe(true);
    expect(group.graph.byKey.get("public.orders")?.time?.muted).toBeUndefined();
  });
});

describe("a history arriving while the view is open", () => {
  it("keeps the versions selected, by number, and follows the newest", async () => {
    const { carryOver } = await import("./state");
    const before = history();
    const pushed = parseHistory(
      timelineFrom([
        ...versions.map((m) => ({ model: m })),
        { model: versions[3]!, message: "Again" },
      ]),
    );
    const state = {
      range: { from: 1, to: 3 },
      at: 2,
      mode: "superposed" as const,
      changedOnly: false,
      group: null,
      query: "",
      table: "public.legacy",
    };
    // v5 pushed: the range ending at the newest follows it; the cursor stays at v3.
    expect(carryOver(state, before, pushed)).toMatchObject({ range: { from: 1, to: 4 }, at: 2 });
    // v3 removed: the cursor falls back to v2, the range end stays the newest.
    const removed = parseHistory(
      timelineFrom(versions.filter((_, i) => i !== 2).map((m) => ({ model: m }))),
    );
    removed.versions.forEach((v, i) => (v.number = [1, 2, 4][i]!));
    expect(carryOver(state, before, removed)).toMatchObject({ range: { from: 1, to: 2 }, at: 1 });
  });
});
