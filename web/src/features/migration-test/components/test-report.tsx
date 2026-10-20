import * as React from "react";

import {
  CheckCircleIcon,
  CircleIcon,
  ClockIcon,
  LinkIcon,
  MinusCircleIcon,
  NavArrowDownIcon,
  NavArrowRightIcon,
  PlayIcon,
  WarningCircleIcon,
  WarningTriangleIcon,
  XmarkCircleIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { duration } from "@/features/playground/format";
import { StatementList } from "@/features/playground/components/migration-report";
import { cn } from "@/lib/utils";
import { formatDateTime, timeAgo } from "@/lib/time";

import {
  PHASES,
  STATUS_WORD,
  causeLine,
  directionsLabel,
  lossLine,
  phaseLabel,
  progressSentence,
  rows,
  rowsLabel,
  subject,
  verdictWord,
} from "../format";
import {
  type CheckReport,
  type CheckStatus,
  type DataLoss,
  type MigrationTest,
  type PhaseReport,
  type TableCheck,
  type TestNote,
  isLive,
} from "../queries";

interface TestReportProps {
  test: MigrationTest;
  /** Developers and admins: Cancel, Run again. */
  canRun: boolean;
  onRunAgain?: () => void;
  /** Runs it again with a generated down migration (one written without). */
  onGenerateDown?: () => void;
  runningAgain?: boolean;
  onCancel?: () => void;
  cancelling?: boolean;
  /**
   * Developers and admins: acknowledges the data loss the migration does
   * not declare (`needs_acknowledgement`). Rejects to keep the dialog open.
   */
  onAcknowledge?: () => Promise<unknown>;
  /** The test's own page, for Copy link. */
  href?: string;
}

/** A status's icon, colored; the word is always next to it or read out. */
function StatusIcon({ status, className }: { status: CheckStatus; className?: string }) {
  const Icon =
    status === "passed"
      ? CheckCircleIcon
      : status === "warning"
        ? WarningTriangleIcon
        : status === "needs_acknowledgement"
          ? WarningCircleIcon
          : status === "failed"
            ? XmarkCircleIcon
            : MinusCircleIcon;
  return (
    <Icon
      aria-hidden
      className={cn(
        "size-4 shrink-0",
        status === "passed" && "text-success",
        (status === "warning" || status === "needs_acknowledgement") && "text-warning",
        status === "failed" && "text-destructive",
        status === "skipped" && "text-faint-foreground",
        className,
      )}
    />
  );
}

/**
 * The verdict as a badge: passed (up and down, up only, on empty tables,
 * a loss acknowledged), needs acknowledgement, failed, could not run,
 * stopped, waiting, running.
 */
export function TestStatusBadge({
  test,
}: {
  test: Pick<MigrationTest, "status" | "summary" | "verdict" | "acknowledged" | "rows_checked">;
}) {
  switch (test.status) {
    case "passed": {
      const lost = test.summary?.data_loss ?? 0;
      const word = verdictWord(test) ?? "Passed";
      if (word !== "Passed")
        return (
          <Badge variant="warning">
            <WarningTriangleIcon aria-hidden />
            {word}
          </Badge>
        );
      return (
        <Badge variant={lost > 0 ? "warning" : "success"}>
          {lost > 0 ? <WarningTriangleIcon aria-hidden /> : <CheckCircleIcon aria-hidden />}
          {lost > 0 ? "Passed, data lost as declared" : "Passed"}
        </Badge>
      );
    }
    case "needs_acknowledgement":
      return (
        <Badge variant="warning">
          <WarningCircleIcon aria-hidden />
          Needs acknowledgement
        </Badge>
      );
    case "failed":
      return (
        <Badge variant="destructive">
          <XmarkCircleIcon aria-hidden />
          Failed
        </Badge>
      );
    case "error":
      return (
        <Badge variant="destructive">
          <XmarkCircleIcon aria-hidden />
          Could not run
        </Badge>
      );
    case "cancelled":
      return (
        <Badge variant="neutral">
          <MinusCircleIcon aria-hidden />
          Stopped
        </Badge>
      );
    case "queued":
      return (
        <Badge variant="neutral">
          <ClockIcon aria-hidden />
          Waiting
        </Badge>
      );
    case "running":
      return (
        <Badge variant="info">
          <Spinner className="size-3" label="Running" />
          Running
        </Badge>
      );
  }
}

/**
 * A migration test: the verdict first, then what did not come back (if
 * anything), then each phase with its checks and statements. While it
 * runs, where it is and Cancel.
 */
export function TestReport({
  test,
  canRun,
  onRunAgain,
  onGenerateDown,
  runningAgain,
  onCancel,
  cancelling,
  onAcknowledge,
  href,
}: TestReportProps) {
  const [acknowledging, setAcknowledging] = React.useState(false);
  const report = test.report;
  const live = isLive(test);
  const headline = live
    ? progressSentence(test)
    : (report?.headline ??
      (test.status === "cancelled"
        ? (test.failure ?? "Stopped before it finished.")
        : "The test could not run."));
  return (
    <article className="grid gap-5" aria-busy={live}>
      <header className="grid gap-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <TestStatusBadge test={test} />
          <div className="flex items-center gap-1.5">
            {href && (
              <Tooltip content="Copy a link to this report">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Copy a link to this report"
                  onClick={() => {
                    void navigator.clipboard
                      .writeText(new URL(href, window.location.origin).toString())
                      .then(() => toast.success("Link copied"));
                  }}
                >
                  <LinkIcon />
                </Button>
              </Tooltip>
            )}
            {canRun && live && onCancel && (
              <Button size="sm" onClick={onCancel} loading={cancelling}>
                Cancel test
              </Button>
            )}
            {canRun && !live && onRunAgain && (
              <Button size="sm" onClick={onRunAgain} loading={runningAgain}>
                <PlayIcon aria-hidden />
                Run again
              </Button>
            )}
          </div>
        </div>
        <h2 className="text-lg font-semibold text-pretty text-foreground" aria-live="polite">
          {headline}
        </h2>
        <p className="flex flex-wrap gap-x-1.5 text-sm text-muted-foreground">
          <span className="font-medium text-foreground">{subject(test)}</span>
          <span aria-hidden>·</span>
          <span>{directionsLabel(test)}</span>
          <span aria-hidden>·</span>
          <span>{rowsLabel(test)}</span>
          <span aria-hidden>·</span>
          <span title={formatDateTime(test.created_at)}>
            {test.created_by?.name ?? "Someone"}, {timeAgo(test.created_at)}
          </span>
          {report && (
            <>
              <span aria-hidden>·</span>
              <span className="tabular-nums">{duration(report.duration_ms)}</span>
            </>
          )}
        </p>
        {test.acknowledged && (
          <p className="text-sm text-muted-foreground">
            Its data loss was acknowledged by{" "}
            <span className="text-foreground">{test.acknowledged.by?.name ?? "someone"}</span>,{" "}
            <span title={formatDateTime(test.acknowledged.at)}>
              {timeAgo(test.acknowledged.at)}
            </span>
            : it counts as passed.
          </p>
        )}
      </header>

      {live && <LiveSteps test={test} />}

      {test.status === "error" && test.failure && (
        <Alert variant="destructive" title="The test could not run">
          {test.failure}
        </Alert>
      )}

      {test.status === "needs_acknowledgement" && (
        <Alert
          variant="warning"
          title="The migration changes data without saying so"
          action={
            canRun &&
            onAcknowledge && (
              <Button size="sm" onClick={() => setAcknowledging(true)}>
                Acknowledge…
              </Button>
            )
          }
        >
          A statement written by hand loses data it does not declare, or does something the checks
          cannot read. Look at what changed below: if it is what you meant,{" "}
          {canRun ? "acknowledge it" : "a developer acknowledges it"}; otherwise change the
          migration. Until then the test does not count as passed.
        </Alert>
      )}
      {onAcknowledge && (
        <ConfirmDialog
          open={acknowledging}
          onOpenChange={setAcknowledging}
          title="Acknowledge this data loss?"
          description="The test then counts as passed for publishing this migration. The audit log keeps who acknowledged it, and the report keeps what was lost."
          confirmLabel="Acknowledge"
          onConfirm={onAcknowledge}
        />
      )}

      {report && report.nothing_checked && test.status !== "failed" && (
        <Alert variant="warning" title="Nothing to check on">
          Every table the migration changes was empty, so no rows were checked. A publish gate does
          not count this test: run it on rows (Small or Medium, or a playground's copy).
        </Alert>
      )}

      {report && report.data_loss.length > 0 && <DataLossList losses={report.data_loss} />}

      {report && (
        <section aria-labelledby={`${test.id}-steps`} className="grid gap-2">
          <h3 id={`${test.id}-steps`} className="text-sm font-medium text-muted-foreground">
            Steps
          </h3>
          <ol className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-card">
            {PHASES.map(({ phase }) => {
              const done = report.phases.find((p) => p.phase === phase);
              return (
                <PhaseRow
                  key={phase}
                  phase={done}
                  name={phaseLabel(phase)}
                  statements={
                    phase === "up"
                      ? report.up
                      : phase === "down"
                        ? (report.down ?? undefined)
                        : undefined
                  }
                  notRun={
                    report.down_status !== "not_provided"
                      ? undefined
                      : phase === "down"
                        ? "Written without a down migration: up only."
                        : phase === "after_down"
                          ? "Nothing to check without a down migration."
                          : undefined
                  }
                  action={
                    phase === "down" &&
                    report.down_status === "not_provided" &&
                    canRun &&
                    onGenerateDown && (
                      <Button size="sm" onClick={onGenerateDown} loading={runningAgain}>
                        Test with a generated down
                      </Button>
                    )
                  }
                />
              );
            })}
          </ol>
        </section>
      )}

      {report && (
        <p className="text-sm text-muted-foreground">
          {report.data.kind === "existing"
            ? `A copy of ${report.data.label}: generated rows, not production data (romulus never connects to your database).`
            : "Generated rows, not production data."}{" "}
          Timings without load; lock modes exact.
          {report.empty_tables.length > 0 &&
            ` Empty, so not checked on rows: ${report.empty_tables.join(", ")}.`}
          {report.not_compared.length > 0 && ` Not compared: ${report.not_compared.join("; ")}.`}
        </p>
      )}
    </article>
  );
}

/** The phases while the test runs: done, running, to come. */
function LiveSteps({ test }: { test: MigrationTest }) {
  const progress = test.progress;
  const done = progress?.done ?? [];
  return (
    <ol className="grid gap-1.5 rounded-lg border border-border bg-card px-3 py-2.5">
      <li className="flex items-center gap-2.5 text-sm">
        {test.status === "queued" || progress?.stage === "queued" ? (
          <Spinner className="size-4" label="Waiting" />
        ) : (
          <CheckCircleIcon aria-hidden className="size-4 text-success" />
        )}
        <span className="text-foreground">A playground of its own</span>
        {progress?.stage === "building" && <span className="text-muted-foreground">building…</span>}
      </li>
      {PHASES.map(({ phase, label }) => {
        const finished = done.find((d) => d.phase === phase);
        const current = progress?.stage === "testing" && progress.phase === phase && !finished;
        return (
          <li key={phase} className="flex items-center gap-2.5 text-sm">
            {finished ? (
              <StatusIcon status={finished.status} />
            ) : current ? (
              <Spinner className="size-4" label="Running" />
            ) : (
              <CircleIcon aria-hidden className="size-4 text-faint-foreground" />
            )}
            <span className={finished || current ? "text-foreground" : "text-muted-foreground"}>
              {label}
            </span>
            {finished && <span className="sr-only">{STATUS_WORD[finished.status]}</span>}
            {current && progress?.statement != null && progress.statements != null && (
              <span className="text-muted-foreground tabular-nums">
                statement {progress.statement + 1} of {progress.statements}
              </span>
            )}
          </li>
        );
      })}
    </ol>
  );
}

/** One phase: its status and summary, opened on its checks and statements. */
function PhaseRow({
  phase,
  name,
  statements,
  notRun,
  action,
}: {
  phase?: PhaseReport;
  name: string;
  statements?: React.ComponentProps<typeof StatementList>["report"];
  notRun?: string;
  /** What to do about it (a down migration to generate). */
  action?: React.ReactNode;
}) {
  const status: CheckStatus = phase?.status ?? "skipped";
  const hasMore = (phase?.checks.length ?? 0) > 0 || (statements?.statements.length ?? 0) > 0;
  const [open, setOpen] = React.useState(
    status === "failed" || status === "warning" || status === "needs_acknowledgement",
  );
  const id = React.useId();
  return (
    <li className={cn(status === "failed" && "bg-destructive/4")}>
      <div className="flex items-start gap-2.5 px-3 py-2.5">
        <StatusIcon status={status} className="mt-0.5" />
        <div className="grid min-w-0 flex-1 gap-0.5">
          <p className="text-base font-medium text-foreground">
            {name}
            <span className="sr-only">: {STATUS_WORD[status]}</span>
          </p>
          <p className="text-sm text-muted-foreground">
            {phase?.summary ?? notRun ?? "Not run: an earlier step failed."}
          </p>
        </div>
        {action}
        {phase && phase.duration_ms > 0 && (
          <span className="mt-0.5 shrink-0 text-sm text-muted-foreground tabular-nums">
            {duration(phase.duration_ms)}
          </span>
        )}
        {hasMore && (
          <Button
            variant="ghost"
            size="icon-sm"
            aria-expanded={open}
            aria-controls={id}
            aria-label={open ? `Hide the details of ${name}` : `Show the details of ${name}`}
            onClick={() => setOpen((o) => !o)}
          >
            {open ? <NavArrowDownIcon /> : <NavArrowRightIcon />}
          </Button>
        )}
      </div>
      {hasMore && open && (
        <div id={id} className="grid gap-3 px-3 pb-3 sm:pl-[2.125rem]">
          {phase?.checks.map((check, i) => (
            <Check key={i} check={check} />
          ))}
          {statements && statements.statements.length > 0 && <StatementList report={statements} />}
        </div>
      )}
    </li>
  );
}

function Check({ check }: { check: CheckReport }) {
  return (
    <div className="grid gap-1.5">
      <p className="flex items-start gap-2 text-sm text-foreground">
        <StatusIcon status={check.status} className="mt-0.5 size-3.5" />
        <span>{check.summary}</span>
      </p>
      {check.details.length > 0 && (
        <ul className="ml-5.5 grid gap-0.5 rounded-md border border-border bg-muted/40 px-2.5 py-2 font-mono text-xs text-muted-foreground">
          {check.details.map((line, i) => (
            <li key={i} className="break-words">
              {line}
            </li>
          ))}
        </ul>
      )}
      {check.tables.length > 0 && (
        <ul className="ml-5.5 grid gap-1.5">
          {check.tables.map((table) => (
            <TableLine key={table.table} table={table} counts={check.kind === "row_counts"} />
          ))}
        </ul>
      )}
    </div>
  );
}

function TableLine({ table, counts }: { table: TableCheck; counts: boolean }) {
  const changed = table.before != null && table.after != null && table.before !== table.after;
  return (
    <li className="grid gap-1">
      <div className="flex items-baseline justify-between gap-3 text-sm">
        <span className="min-w-0 truncate">
          <span className="font-mono text-foreground">{table.table}</span>
          {table.from && <span className="text-muted-foreground"> (was {table.from})</span>}
        </span>
        {counts && (
          <span
            className={cn(
              "shrink-0 tabular-nums",
              changed ? "text-foreground" : "text-muted-foreground",
            )}
          >
            {table.before != null ? rows(table.before) : "–"}
            {changed && ` → ${table.after != null ? rows(table.after) : "–"}`}
          </span>
        )}
      </div>
      {table.notes.map((note, i) => (
        <NoteLine key={i} note={note} />
      ))}
    </li>
  );
}

function NoteLine({ note }: { note: TestNote }) {
  return (
    <div className="grid gap-1 pl-3">
      <p className="flex items-start gap-1.5 text-sm text-muted-foreground">
        <StatusIcon status={note.status} className="mt-0.5 size-3.5" />
        <span>
          {note.column && <span className="font-mono text-foreground">{note.column}</span>}
          {note.column && ": "}
          {note.message}
        </span>
      </p>
      {note.examples.length > 0 && <Examples examples={note.examples} />}
    </div>
  );
}

function Examples({ examples }: { examples: DataLoss["examples"] }) {
  return (
    <table className="ml-5 w-fit max-w-full text-xs">
      <caption className="sr-only">Examples: the row's key, the value before and after</caption>
      <tbody className="font-mono">
        {examples.map((example, i) => (
          <tr key={i} className="align-baseline">
            <th scope="row" className="pr-3 text-left font-normal text-faint-foreground">
              {example.key}
            </th>
            <td className="max-w-[16rem] truncate pr-1.5 text-muted-foreground">
              {example.before ?? "NULL"}
            </td>
            <td aria-hidden className="pr-1.5 text-faint-foreground">
              →
            </td>
            <td className="max-w-[16rem] truncate text-foreground">
              <span className="sr-only">became </span>
              {example.after ?? "NULL"}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** What does not come back: each finding, the statement that explains it. */
function DataLossList({ losses }: { losses: DataLoss[] }) {
  const unexpected = losses.filter((l) => l.status === "failed").length;
  const undeclared = losses.filter((l) => l.status === "needs_acknowledgement").length;
  return (
    <section aria-labelledby="data-not-back" className="grid gap-2">
      <h3 id="data-not-back" className="text-sm font-medium text-muted-foreground">
        Data not back ({losses.length})
        {unexpected > 0 && (
          <span className="text-destructive">, {unexpected} not flagged by the migration</span>
        )}
        {undeclared > 0 && <span className="text-warning">, {undeclared} to acknowledge</span>}
      </h3>
      <ul className="grid gap-2">
        {losses.map((loss, i) => (
          <li
            key={i}
            className={cn(
              "grid gap-1.5 rounded-lg border px-3 py-2.5",
              loss.status === "failed"
                ? "border-destructive/25 bg-destructive/6"
                : "border-warning/30 bg-warning/6",
            )}
          >
            <p className="flex items-start gap-2 text-base">
              <StatusIcon status={loss.status} className="mt-0.5" />
              <span className="sr-only">{STATUS_WORD[loss.status]}: </span>
              <span className="grid min-w-0 gap-0.5">
                <span className="font-mono font-medium break-all text-foreground">
                  {loss.table}
                  {loss.column && `.${loss.column}`}
                </span>
                <span className="text-foreground">{lossLine(loss)}</span>
                <span className="text-sm text-muted-foreground" title={loss.message}>
                  {causeLine(loss)}
                </span>
              </span>
            </p>
            {loss.examples.length > 0 && (
              <div className="pl-1">
                <Examples examples={loss.examples} />
              </div>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}
