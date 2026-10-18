import { describe, expect, it } from "vitest";

import {
  begin,
  canRun,
  type ConsoleEvent,
  type ConsoleNotice,
  type ConsoleState,
  reduce,
} from "./console";

const empty: ConsoleState = {
  connection: "connecting",
  pausedBecause: null,
  unavailable: null,
  closed: null,
  txn: "idle",
  runs: [],
  running: null,
  present: [],
  history: [],
  notice: null,
};

const span = { start: 0, end: 8, utf16_start: 0, utf16_end: 8 };

function play(state: ConsoleState, ...messages: (ConsoleEvent | ConsoleNotice)[]) {
  return messages.reduce(reduce, state);
}

describe("the console's state", () => {
  it("folds a run's events into its statements", () => {
    let state = play(empty, { type: "ready", txn: "idle" });
    state = begin(state, "r1", "SELECT 1; SELECT 2", false, 4);
    state = play(
      state,
      { type: "statement", id: "r1", index: 0, span },
      {
        type: "columns",
        id: "r1",
        index: 0,
        columns: [{ name: "a", type: "integer", numeric: true }],
      },
      { type: "rows", id: "r1", index: 0, rows: [["1"], [null]] },
      { type: "done", id: "r1", index: 0, tag: "SELECT 2", rows: 2, truncated: false, ms: 3 },
      { type: "txn", txn: "idle" },
      { type: "finished", id: "r1", status: "ok", statements: 1, rows: 2, ms: 5 },
    );
    const run = state.runs[0];
    expect(state.running).toBeNull();
    expect(run?.offset).toBe(4);
    expect(run?.status).toBe("ok");
    expect(run?.statements[0]?.rows).toEqual([["1"], [null]]);
    expect(run?.statements[0]?.tag).toBe("SELECT 2");
  });

  it("keeps a syntax error that no statement started", () => {
    let state = begin(play(empty, { type: "ready", txn: "idle" }), "r1", "SELEC 1", false);
    state = play(
      state,
      {
        type: "error",
        id: "r1",
        index: 0,
        error: { sqlstate: "42601", message: 'syntax error at or near "SELEC"', span },
      },
      { type: "finished", id: "r1", status: "error", statements: 0, ms: 1, error: "syntax error" },
    );
    expect(state.runs[0]?.statements[0]?.error?.sqlstate).toBe("42601");
    expect(state.runs[0]?.error).toBeNull();
  });

  it("says why a run was refused before any statement", () => {
    let state = begin(play(empty, { type: "ready", txn: "idle" }), "r1", "x", true);
    state = play(state, {
      type: "finished",
      id: "r1",
      status: "error",
      statements: 0,
      ms: 0,
      error: "Explain shows one statement's plan",
    });
    expect(state.runs[0]?.error).toBe("Explain shows one statement's plan");
  });

  it("pauses with the reason and resumes ready", () => {
    let state = play(empty, { type: "ready", txn: "in_transaction" });
    state = play(state, { type: "paused", reason: "Bob is resetting the rows" });
    expect(state.connection).toBe("paused");
    expect(state.pausedBecause).toBe("Bob is resetting the rows");
    expect(state.txn).toBe("idle");
    state = play(state, { type: "ready", txn: "idle" });
    expect(state.connection).toBe("ready");
    expect(state.pausedBecause).toBeNull();
  });

  it("tells a stop (a run starts it) from a failed start (try again)", () => {
    let state = play(empty, { type: "ready", txn: "idle" });
    state = play(state, {
      type: "stopped",
      reason: "it was stopped after 15 minutes without a query",
    });
    expect(state.connection).toBe("stopped");
    expect(state.pausedBecause).toContain("without a query");
    expect(canRun(state.connection)).toBe(true);
    state = play(
      state,
      { type: "starting" },
      { type: "unavailable", code: "org_full", reason: "2 playgrounds are in use" },
    );
    expect(state.connection).toBe("unavailable");
    expect(state.unavailable).toEqual({ code: "org_full", reason: "2 playgrounds are in use" });
    expect(canRun(state.connection)).toBe(true);
    state = play(state, { type: "starting" }, { type: "ready", txn: "idle" });
    expect(state.connection).toBe("ready");
    expect(state.unavailable).toBeNull();
    expect(canRun("starting")).toBe(false);
    expect(canRun("reconnecting")).toBe(false);
  });

  it("says deleted at once while a deletion runs, and resumes if it fails", () => {
    let state = play(empty, { type: "ready", txn: "idle" }, { type: "deleting" });
    expect(state.connection).toBe("closed");
    expect(state.closed).toBe("deleted");
    expect(canRun(state.connection)).toBe(false);
    state = play(state, { type: "ready", txn: "idle" });
    expect(state.connection).toBe("ready");
    expect(state.closed).toBeNull();
  });

  it("puts the newest history first, once", () => {
    const run = {
      id: "h1",
      kind: "query" as const,
      by: { id: "u1", name: "Alice" },
      sql: "SELECT 1",
      started_at: "2026-10-17T10:00:00Z",
      duration_ms: 3,
      status: "ok" as const,
      rows: 1,
    };
    const state = play(
      empty,
      { type: "history", run },
      { type: "history", run: { ...run, id: "h2" } },
      { type: "history", run },
    );
    expect(state.history.map((r) => r.id)).toEqual(["h1", "h2"]);
  });

  it("numbers announcements so each one is new", () => {
    const bob = { id: "u2", name: "Bob" };
    const state = play(
      empty,
      { type: "announcement", event: "reset", by: bob },
      { type: "announcement", event: "reset", by: bob },
    );
    expect(state.notice).toMatchObject({ id: 2, event: "reset", by: bob });
  });
});
