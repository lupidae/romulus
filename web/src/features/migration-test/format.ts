// How a migration test reads: its phases, its progress, what it is of.

import { formatCount } from "@/features/playground/queries";

import type { CheckStatus, DataLoss, MigrationTest, Phase, TestProgress } from "./queries";

/** The phases in order, and what each is called on the page. */
export const PHASES: { phase: Phase; label: string }[] = [
  { phase: "seed", label: "Rows" },
  { phase: "before", label: "Before the migration" },
  { phase: "up", label: "Up migration" },
  { phase: "after_up", label: "Checks after up" },
  { phase: "down", label: "Down migration" },
  { phase: "after_down", label: "Checks after down" },
];

export function phaseLabel(phase: Phase): string {
  return PHASES.find((p) => p.phase === phase)?.label ?? phase;
}

/** A check's or a phase's status, as a word. */
export const STATUS_WORD: Record<CheckStatus, string> = {
  passed: "Passed",
  warning: "Passed, with data to look at",
  needs_acknowledgement: "Needs acknowledgement",
  failed: "Failed",
  skipped: "Not run",
};

/** `v7 → v8`, `v7 → draft "Billing"`, `v8's own migration (from v7)`. */
export function subject(test: MigrationTest): string {
  switch (test.source) {
    case "draft":
      return `v${test.from} → draft “${test.draft?.name ?? "deleted"}”`;
    case "versions":
      return `v${test.from} → v${test.to ?? "?"}`;
    case "version":
      return `v${test.to ?? "?"} as written, from v${test.from}`;
  }
}

/** `Small rows, seed 7f3a91`, `Medium rows, edge cases, seed 01ab`, `A copy of Alice's playground`. */
export function rowsLabel(test: MigrationTest): string {
  if (test.data.playground) return `A copy of ${test.data.playground.label}`;
  const size = test.data.size === "medium" ? "Medium" : "Small";
  const profile = test.data.profile === "edge_cases" ? ", edge cases" : "";
  return `${size} rows${profile}, seed ${test.data.seed}`;
}

/** `Up and down`, `Up only`, `Up, then a generated down`. */
export function directionsLabel(test: MigrationTest): string {
  if (test.down === "none") return "Up only";
  if (test.down === "generated" && test.source !== "draft" && test.source !== "versions")
    return "Up, then a generated down";
  return "Up and down";
}

function ordinal(n: number): string {
  const tens = n % 100;
  if (tens >= 11 && tens <= 13) return `${n}th`;
  return `${n}${({ 1: "st", 2: "nd", 3: "rd" } as Record<number, string>)[n % 10] ?? "th"}`;
}

/** Where a test that is not over is, in one sentence. */
export function progressSentence(test: MigrationTest): string {
  const progress: TestProgress | null | undefined = test.progress;
  if (test.status === "queued") {
    if (progress?.waiting) return progress.waiting;
    if (test.position) return `${ordinal(test.position)} in line. Tests run two at a time.`;
    return "Waiting for its turn…";
  }
  if (!progress) return "Running…";
  switch (progress.stage) {
    case "queued":
      return progress.waiting ?? "Waiting for room on the playground host…";
    case "building": {
      const build = progress.build;
      if (build?.table && build.tables_total > 0)
        return `Generating rows: ${build.table} (${build.tables_done} of ${build.tables_total} tables)`;
      return "Building its playground…";
    }
    case "testing": {
      if (progress.statement != null && progress.statements && progress.phase) {
        const which = progress.direction === "down" ? "down" : "up";
        return `Running the ${which} migration: statement ${progress.statement + 1} of ${progress.statements}`;
      }
      return progress.phase ? `${phaseLabel(progress.phase)}…` : "Testing…";
    }
  }
}

/** `2,000 rows`, `1 row`. */
export function rows(n: number): string {
  return `${formatCount(n)} row${n === 1 ? "" : "s"}`;
}

/** `18 of 25 sampled values lost`, `2,000 of 2,000 rows gone`. */
export function lossLine(loss: DataLoss): string {
  if (loss.kind === "rows_deleted") {
    if (loss.count_before != null && loss.count_after != null)
      return `${formatCount(loss.count_before - loss.count_after)} of ${rows(loss.count_before)} gone`;
    return `${loss.rows} of ${loss.sampled} sampled rows gone`;
  }
  return `${loss.rows} of ${loss.sampled} sampled values lost`;
}

const FLAG_WORDS: Record<NonNullable<DataLoss["cause"]>["flag"], string> = {
  flagged: "flagged “deletes data”",
  written: "written by hand to do it",
  written_undeclared: "written by hand without saying it loses data",
  opaque: "its effects cannot be read (DO, CALL, MERGE)",
  moved: "the rows moved with a partition, not lost",
  not_flagged: "not flagged as deleting data",
};

/** `Dropped by up statement 7, flagged “deletes data”`. */
export function causeLine(loss: DataLoss): string {
  const cause = loss.cause;
  if (!cause) return "No statement explains it";
  const verb =
    loss.kind === "column_dropped"
      ? "Dropped by"
      : loss.kind === "rows_deleted"
        ? "Deleted by"
        : "Changed by";
  return `${verb} ${cause.direction} statement ${cause.index + 1}, ${FLAG_WORDS[cause.flag]}`;
}

/**
 * A finished test's verdict, in words: what its badge says. Acknowledged
 * and up-only passes say so; a pass on empty tables checked nothing.
 */
export function verdictWord(
  test: Pick<MigrationTest, "status" | "verdict" | "acknowledged" | "rows_checked">,
): string | null {
  switch (test.status) {
    case "needs_acknowledgement":
      return "Needs acknowledgement";
    case "passed":
      if (test.acknowledged)
        return test.verdict === "passed_up_only"
          ? "Passed up only, loss acknowledged"
          : "Passed, loss acknowledged";
      if (!test.rows_checked) return "Passed, nothing checked";
      if (test.verdict === "passed_up_only") return "Passed up only";
      return "Passed";
    default:
      return null;
  }
}
