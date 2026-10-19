import { afterEach, describe, expect, it } from "vitest";

import { timeWords } from "@/features/schema/graph/time-words";

import { column, model, table, timelineFrom } from "./fixtures";
import { Frames } from "./frame";
import { parseHistory } from "./history";
import { keepPlaced, placedOnly, readPlaced } from "./layout-store";

const graph = () =>
  new Frames(
    parseHistory(
      timelineFrom([
        { model: model([table("users", []), table("orders", [column("users_id")], ["users"])]) },
        {
          model: model([
            table("users", [column("email", "text")]),
            table("orders", [column("users_id")], ["users"]),
          ]),
        },
      ]),
    ),
  ).frame({ range: { from: 0, to: 1 }, at: 1, mode: "superposed" }).graph;

afterEach(() => localStorage.clear());

describe("positions kept for a database", () => {
  it("are read back, rounded, for the tables the history has", () => {
    keepPlaced(
      "db-1",
      { "public.users": { x: 10.4, y: 20.6 }, "public.gone": { x: 1, y: 1 } },
      graph(),
    );
    expect(readPlaced("db-1")).toEqual({ "public.users": { x: 10, y: 21 } });
    // More later: merged with what was kept.
    keepPlaced("db-1", { "public.orders": { x: 300, y: 0 } }, graph());
    expect(readPlaced("db-1")).toEqual({
      "public.users": { x: 10, y: 21 },
      "public.orders": { x: 300, y: 0 },
    });
    expect(readPlaced("db-2")).toEqual({});
    expect(readPlaced(undefined)).toEqual({});
  });

  it("anything unreadable is nothing", () => {
    localStorage.setItem("romulus:time-machine:layout:db-1", "{not json");
    expect(readPlaced("db-1")).toEqual({});
    localStorage.setItem(
      "romulus:time-machine:layout:db-1",
      JSON.stringify({ "public.users": { x: "left" }, "public.orders": { x: 1, y: 2 } }),
    );
    expect(readPlaced("db-1")).toEqual({ "public.orders": { x: 1, y: 2 } });
  });
});

describe("a graph while new tables wait for their place", () => {
  it("draws the placed tables and their edges, the same objects", () => {
    const g = graph();
    expect(placedOnly(g, { "public.users": { x: 0, y: 0 }, "public.orders": { x: 1, y: 1 } })).toBe(
      g,
    );
    const some = placedOnly(g, { "public.users": { x: 0, y: 0 } });
    expect(some.tables.map((t) => t.key)).toEqual(["public.users"]);
    expect(some.tables[0]).toBe(g.byKey.get("public.users"));
    expect(some.edges).toEqual([]);
  });
});

describe("what the tint says, in words", () => {
  it("is on new rows and edges too", () => {
    const g = graph();
    const email = g.byKey.get("public.users")!.columns.find((c) => c.name === "email")!;
    expect(email.time?.note).toBe("+v2");
    expect(timeWords(email.time)).toBe("added in v2");
    expect(timeWords({ note: "−v7", changed: true })).toBe("removed in v7, changed");
    expect(timeWords(undefined)).toBe("");
  });
});
