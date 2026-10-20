import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { MigrationTest, TestReport as Report } from "../queries";
import { TestReport } from "./test-report";

const statement = (index: number, sql: string, status: "passed" | "failed" | "not_run") => ({
  index,
  sql,
  status,
  hazard: null,
  duration_ms: 4,
  locks: status === "passed" ? [{ relation: "public.orders", mode: "AccessExclusiveLock" }] : [],
  locks_documented: false,
  rewrote: [],
  rows_changed: null,
  tag: null,
  error:
    status === "failed"
      ? {
          sqlstate: "23502",
          message: 'column "nickname" of relation "customers" contains null values',
        }
      : null,
  offending: status === "failed" ? { count: 12, query: "SELECT 1", sample: ["(4)"] } : null,
});

function report(over: Partial<Report>): Report {
  return {
    verdict: "passed",
    headline: "Up and down ran on 2,000 rows. Data is lost in 1 place, as flagged.",
    data: {
      kind: "generated",
      size: "small",
      profile: "realistic",
      seed: "7f3a91",
      generator: "1",
    },
    sample: 25,
    tables: 2,
    rows: 2000,
    phases: [
      { phase: "seed", status: "passed", duration_ms: 900, summary: "Rows generated.", checks: [] },
      {
        phase: "before",
        status: "passed",
        duration_ms: 40,
        summary: "2,000 rows in 2 tables; 50 sampled by primary key.",
        checks: [],
      },
      { phase: "up", status: "passed", duration_ms: 12, summary: "1 statement ran.", checks: [] },
      {
        phase: "after_up",
        status: "warning",
        duration_ms: 30,
        summary: "As expected, with flagged data loss.",
        checks: [],
      },
      { phase: "down", status: "passed", duration_ms: 9, summary: "1 statement ran.", checks: [] },
      {
        phase: "after_down",
        status: "warning",
        duration_ms: 31,
        summary: "Back as it was, except flagged data loss.",
        checks: [],
      },
    ],
    up: {
      passed: true,
      statements: [statement(0, "ALTER TABLE public.orders DROP COLUMN legacy;", "passed")],
      failed_at: null,
      duration_ms: 12,
    },
    down_status: "ran",
    down: {
      passed: true,
      statements: [statement(0, "ALTER TABLE public.orders ADD COLUMN legacy text;", "passed")],
      failed_at: null,
      duration_ms: 9,
    },
    data_loss: [
      {
        table: "public.orders",
        column: "legacy",
        kind: "column_dropped",
        rows: 18,
        sampled: 25,
        count_before: null,
        count_after: null,
        examples: [{ key: "(42)", before: "abc", after: null }],
        cause: { direction: "up", index: 0, hazard: "deletes_data", flag: "flagged" },
        expected: true,
        status: "warning",
        message: "public.orders.legacy: its values are gone.",
      },
    ],
    locks_from: "pg_locks",
    duration_ms: 1200,
    nothing_checked: false,
    empty_tables: [],
    not_compared: ["grants and owners"],
    ...over,
  };
}

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
    data: { size: "small", profile: "realistic", seed: "7f3a91", playground: null },
    sha256: null,
    position: null,
    progress: null,
    summary: { data_loss: 1 },
    report: report({}),
    failure: null,
    up_sql: "",
    down_sql: "",
    created_by: { id: "u1", name: "Alice" },
    created_at: new Date().toISOString(),
    started_at: null,
    finished_at: null,
    ...over,
  } as MigrationTest;
}

describe("a migration test's report", () => {
  it("leads with the verdict, then the data that does not come back", () => {
    render(<TestReport test={test({})} canRun={false} />);
    expect(screen.getByText("Passed, data lost as declared")).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(
      "Up and down ran on 2,000 rows.",
    );
    const lost = screen.getByRole("region", { name: /Data not back/ });
    expect(within(lost).getByText("public.orders.legacy")).toBeInTheDocument();
    expect(within(lost).getByText("18 of 25 sampled values lost")).toBeInTheDocument();
    expect(
      within(lost).getByText("Dropped by up statement 1, flagged “deletes data”"),
    ).toBeInTheDocument();
    expect(within(lost).getByRole("rowheader", { name: "(42)" })).toBeInTheDocument();
    // No run button for reviewers.
    expect(screen.queryByRole("button", { name: /Run again/ })).not.toBeInTheDocument();
  });

  it("asks a developer to acknowledge data the migration loses without saying so", async () => {
    const onAcknowledge = vi.fn(() => Promise.resolve());
    const undeclared = report({
      verdict: "needs_acknowledgement",
      headline:
        "Up and down ran on 2,000 rows. Data changed in 1 place the migration does not declare: acknowledge it, or change the migration.",
      data_loss: [
        {
          ...report({}).data_loss[0]!,
          kind: "values_changed",
          cause: { direction: "up", index: 0, hazard: null, flag: "written_undeclared" },
          expected: false,
          status: "needs_acknowledgement",
        },
      ],
    });
    render(
      <TestReport
        test={test({
          status: "needs_acknowledgement",
          verdict: "needs_acknowledgement",
          report: undeclared,
        })}
        canRun
        onAcknowledge={onAcknowledge}
      />,
    );
    expect(screen.getAllByText("Needs acknowledgement").length).toBeGreaterThan(0);
    const lost = screen.getByRole("region", { name: /Data not back/ });
    expect(within(lost).getByText(/1 to acknowledge/)).toBeInTheDocument();
    expect(
      within(lost).getByText(
        "Changed by up statement 1, written by hand without saying it loses data",
      ),
    ).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Acknowledge…" }));
    await user.click(screen.getByRole("button", { name: "Acknowledge" }));
    expect(onAcknowledge).toHaveBeenCalled();
  });

  it("says when nothing was checked on rows, and what is never compared", () => {
    render(
      <TestReport
        test={test({
          rows_checked: false,
          report: report({
            nothing_checked: true,
            empty_tables: ["public.orders"],
            data_loss: [],
          }),
        })}
        canRun={false}
      />,
    );
    expect(screen.getByText("Passed, nothing checked")).toBeInTheDocument();
    expect(screen.getByText("Nothing to check on")).toBeInTheDocument();
    expect(screen.getByText(/Empty, so not checked on rows: public.orders/)).toBeInTheDocument();
    expect(screen.getByText(/Not compared: grants and owners/)).toBeInTheDocument();
  });

  it("opens the failing step on its statement, its error and the rows that break it", async () => {
    const failed = report({
      verdict: "failed",
      headline:
        'Failed at statement 1 of 1 of the up migration: column "nickname" contains null values',
      phases: [
        { phase: "seed", status: "passed", duration_ms: 1, summary: "Rows generated.", checks: [] },
        { phase: "before", status: "passed", duration_ms: 1, summary: "2,000 rows.", checks: [] },
        {
          phase: "up",
          status: "failed",
          duration_ms: 5,
          summary: "Statement 1 of 1 failed.",
          checks: [],
        },
      ],
      up: {
        passed: false,
        statements: [
          statement(
            0,
            "ALTER TABLE public.customers ALTER COLUMN nickname SET NOT NULL;",
            "failed",
          ),
        ],
        failed_at: 0,
        duration_ms: 5,
      },
      down_status: "not_run",
      down: null,
      data_loss: [],
    });
    render(<TestReport test={test({ status: "failed", report: failed })} canRun={false} />);
    expect(screen.getByText("Failed")).toBeInTheDocument();
    expect(screen.getByText(/SET NOT NULL/)).toBeInTheDocument();
    // In the headline, and again under the statement that failed.
    expect(screen.getAllByText(/contains null values/)).toHaveLength(2);
    expect(screen.getByText(/12 rows break it/)).toBeInTheDocument();
    expect(screen.getAllByText("Not run: an earlier step failed.").length).toBe(3);
    // The failing step is open; it closes on demand.
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Hide the details of Up migration" }));
    expect(screen.queryByText(/SET NOT NULL/)).not.toBeInTheDocument();
  });

  it("says where a running test is, and offers Cancel to developers", async () => {
    const onCancel = vi.fn();
    render(
      <TestReport
        test={test({
          status: "running",
          report: null,
          summary: null,
          progress: {
            stage: "testing",
            waiting: null,
            build: null,
            phase: "up",
            direction: "up",
            statement: 2,
            statements: 7,
            done: [
              { phase: "seed", status: "passed" },
              { phase: "before", status: "passed" },
            ],
          },
        })}
        canRun
        onCancel={onCancel}
      />,
    );
    expect(screen.getByRole("heading", { level: 2 })).toHaveTextContent(
      "Running the up migration: statement 3 of 7",
    );
    await userEvent.setup().click(screen.getByRole("button", { name: "Cancel test" }));
    expect(onCancel).toHaveBeenCalled();
  });
});
