import { describe, expect, it } from "vitest";

import {
  causeLine,
  directionsLabel,
  lossLine,
  progressSentence,
  rowsLabel,
  subject,
  verdictWord,
} from "./format";
import type { DataLoss, MigrationTest } from "./queries";

function test(over: Partial<MigrationTest>): MigrationTest {
  return {
    id: "t1",
    status: "passed",
    verdict: "passed",
    rows_checked: true,
    source: "versions",
    draft: null,
    from: 7,
    to: 8,
    down: "generated",
    data: { size: "small", profile: "realistic", seed: "7f3a91" },
    sha256: null,
    position: null,
    progress: null,
    summary: null,
    report: null,
    failure: null,
    up_sql: null,
    down_sql: null,
    created_by: { id: "u1", name: "Alice" },
    created_at: "2026-10-19T10:00:00Z",
    started_at: null,
    finished_at: null,
    ...over,
  };
}

describe("a migration test's words", () => {
  it("says what it tests", () => {
    expect(subject(test({}))).toBe("v7 → v8");
    expect(subject(test({ source: "draft", to: null, draft: { id: "d", name: "Billing" } }))).toBe(
      "v7 → draft “Billing”",
    );
    expect(subject(test({ source: "version" }))).toBe("v8 as written, from v7");
  });

  it("says which rows and which ways", () => {
    expect(rowsLabel(test({}))).toBe("Small rows, seed 7f3a91");
    expect(rowsLabel(test({ data: { size: "medium", profile: "edge_cases", seed: "01" } }))).toBe(
      "Medium rows, edge cases, seed 01",
    );
    expect(directionsLabel(test({ down: "none" }))).toBe("Up only");
    expect(directionsLabel(test({ source: "version", down: "generated" }))).toBe(
      "Up, then a generated down",
    );
  });

  it("says where a test that is not over is", () => {
    expect(progressSentence(test({ status: "queued", position: 2 }))).toBe(
      "2nd in line. Tests run two at a time.",
    );
    expect(
      progressSentence(
        test({
          status: "running",
          progress: {
            stage: "testing",
            waiting: null,
            build: null,
            phase: "up",
            direction: "up",
            statement: 2,
            statements: 7,
            done: [],
          },
        }),
      ),
    ).toBe("Running the up migration: statement 3 of 7");
    expect(
      progressSentence(
        test({
          status: "running",
          progress: {
            stage: "building",
            waiting: null,
            build: { stage: "rows", tables_done: 3, tables_total: 14, table: "public.orders" },
            phase: null,
            direction: null,
            statement: null,
            statements: null,
            done: [],
          },
        }),
      ),
    ).toBe("Generating rows: public.orders (3 of 14 tables)");
  });

  it("says what is lost and why in two short lines", () => {
    const loss = {
      table: "public.orders",
      column: "legacy",
      kind: "column_dropped",
      rows: 18,
      sampled: 25,
      count_before: null,
      count_after: null,
      examples: [],
      cause: { direction: "up", index: 6, hazard: "deletes_data", flag: "flagged" },
      expected: true,
      status: "warning",
      message: "",
    } as DataLoss;
    expect(lossLine(loss)).toBe("18 of 25 sampled values lost");
    expect(causeLine(loss)).toBe("Dropped by up statement 7, flagged “deletes data”");
    expect(
      causeLine({
        ...loss,
        kind: "values_changed",
        cause: { ...loss.cause!, flag: "not_flagged" },
      }),
    ).toBe("Changed by up statement 7, not flagged as deleting data");
    expect(causeLine({ ...loss, cause: { ...loss.cause!, flag: "written_undeclared" } })).toBe(
      "Dropped by up statement 7, written by hand without saying it loses data",
    );
    expect(lossLine({ ...loss, kind: "rows_deleted", count_before: 2000, count_after: 0 })).toBe(
      "2,000 of 2,000 rows gone",
    );
  });

  it("says what kind of pass a test was", () => {
    expect(verdictWord(test({}))).toBe("Passed");
    expect(verdictWord(test({ verdict: "passed_up_only" }))).toBe("Passed up only");
    expect(verdictWord(test({ rows_checked: false }))).toBe("Passed, nothing checked");
    expect(
      verdictWord(
        test({
          verdict: "needs_acknowledgement",
          acknowledged: { at: "2026-10-19T10:05:00Z", by: { id: "u2", name: "Bob" } },
        }),
      ),
    ).toBe("Passed, loss acknowledged");
    expect(
      verdictWord(
        test({
          verdict: "passed_up_only",
          acknowledged: { at: "2026-10-19T10:05:00Z", by: { id: "u2", name: "Bob" } },
        }),
      ),
    ).toBe("Passed up only, loss acknowledged");
    expect(verdictWord(test({ status: "needs_acknowledgement" }))).toBe("Needs acknowledgement");
    expect(verdictWord(test({ status: "failed" }))).toBeNull();
  });
});
