import * as React from "react";

import {
  CheckCircleIcon,
  CircleIcon,
  MinusCircleIcon,
  WarningTriangleIcon,
  XmarkCircleIcon,
} from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import { HAZARD, duration, lockName, verdict } from "../format";
import { formatCount, type MigrationReport, type MigrationStatementResult } from "../queries";

export interface LiveRun {
  /** The generated rows are being put back, before the first statement. */
  resetting: boolean;
  /** Statement running now (0-based); `null` between statements. */
  running: number | null;
  total: number | null;
  /** Results so far. */
  done: MigrationStatementResult[];
}

interface MigrationReportViewProps {
  /** A finished run. */
  report?: MigrationReport | null;
  /** A run in progress. */
  live?: LiveRun | null;
  /** Puts a query in the console (the rows that break a statement). */
  onQuery?: (sql: string) => void;
}

/**
 * A migration run, statement by statement: what passed (how long, which
 * locks, what it rewrote, rows changed), what failed (Postgres's error and
 * the rows that broke it), what did not run.
 */
export function MigrationReportView({ report, live, onQuery }: MigrationReportViewProps) {
  const statements = report?.statements ?? live?.done ?? [];
  return (
    <div className="grid gap-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        {report ? (
          <>
            <Badge variant={report.passed ? "success" : "destructive"}>
              {report.passed ? <CheckCircleIcon aria-hidden /> : <XmarkCircleIcon aria-hidden />}
              {report.passed ? "Passed" : "Failed"}
            </Badge>
            <p className="text-base font-medium text-foreground">{verdict(report)}</p>
          </>
        ) : (
          <p className="flex items-center gap-2 text-base text-foreground">
            <Spinner className="size-4" label="Running" />
            {progress(live)}
          </p>
        )}
      </div>
      <StatementList statements={statements} running={live?.running} onQuery={onQuery} />
      {report && !report.passed && (
        <p className="text-sm text-muted-foreground">{afterFailure(report)}</p>
      )}
    </div>
  );
}

/**
 * The statements of a run that ran (or failed), each with what it did; the
 * one running now, if any. A migration test shows its up and down runs
 * with it.
 */
export function StatementList({
  statements,
  report,
  running,
  onQuery,
}: {
  statements?: MigrationStatementResult[];
  report?: MigrationReport;
  running?: number | null;
  onQuery?: (sql: string) => void;
}) {
  const list = statements ?? report?.statements ?? [];
  return (
    <ol className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-card">
      {list
        .filter((s) => s.status !== "not_run")
        .map((statement) => (
          <StatementRow key={statement.index} statement={statement} onQuery={onQuery} />
        ))}
      {running != null && (
        <li className="flex items-center gap-2.5 px-3 py-2.5 text-sm text-muted-foreground">
          <Spinner className="size-4" label="Running" />
          Statement {running + 1}
        </li>
      )}
    </ol>
  );
}

/** Where a run in progress is. */
function progress(live: LiveRun | null | undefined): string {
  if (!live || live.resetting) return "Putting the generated rows back first…";
  const total = live.total ?? "…";
  if (live.running != null) return `Running statement ${live.running + 1} of ${total}`;
  return `Ran ${live.done.length} of ${total} statements…`;
}

/** What a failed run left: which statements did not run, what the rows hold. */
function afterFailure(report: MigrationReport): string {
  const at = report.failed_at ?? 0;
  const notRun = report.statements.filter((s) => s.status === "not_run");
  const first = notRun[0];
  const last = notRun[notRun.length - 1];
  const skipped =
    !first || !last
      ? "It was the last statement."
      : first === last
        ? `Statement ${first.index + 1} did not run.`
        : `Statements ${first.index + 1}–${last.index + 1} did not run.`;
  const held =
    at === 0
      ? "Nothing ran before it: the playground holds the generated rows."
      : at === 1
        ? "The playground holds what statement 1 did."
        : `The playground holds what statements 1–${at} did.`;
  return `${skipped} ${held}`;
}

const STATUS: Record<MigrationStatementResult["status"], string> = {
  passed: "Passed",
  failed: "Failed",
  not_run: "Not run",
  skipped: "Skipped",
};

function StatementRow({
  statement,
  onQuery,
}: {
  statement: MigrationStatementResult;
  onQuery?: (sql: string) => void;
}) {
  const [expanded, setExpanded] = React.useState(statement.status === "failed");
  const firstLine =
    statement.sql.split("\n").find((line) => line.trim() && !line.trim().startsWith("--")) ??
    statement.sql;
  const multiline = statement.sql.trim().includes("\n");
  const hazard = statement.hazard ? HAZARD[statement.hazard] : undefined;
  const Icon =
    statement.status === "passed"
      ? CheckCircleIcon
      : statement.status === "failed"
        ? XmarkCircleIcon
        : statement.status === "skipped"
          ? MinusCircleIcon
          : CircleIcon;
  return (
    <li
      className={cn("grid gap-2 px-3 py-2.5", statement.status === "failed" && "bg-destructive/4")}
    >
      <div className="flex items-start gap-2.5">
        <span className="sr-only">{STATUS[statement.status]}: </span>
        <Icon
          aria-hidden
          className={cn(
            "mt-0.5 size-4 shrink-0",
            statement.status === "passed" && "text-success",
            statement.status === "failed" && "text-destructive",
            statement.status === "skipped" && "text-faint-foreground",
          )}
        />
        <span className="w-6 shrink-0 text-right text-sm text-faint-foreground tabular-nums">
          {statement.index + 1}
        </span>
        <div className="grid min-w-0 flex-1 gap-1">
          {expanded || !multiline ? (
            <pre className="overflow-x-auto font-mono text-sm whitespace-pre-wrap text-foreground">
              {statement.sql.trim()}
            </pre>
          ) : (
            <code className="truncate font-mono text-sm text-foreground">{firstLine.trim()}</code>
          )}
          <Facts statement={statement} />
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {hazard && (
            <Badge variant={hazard.variant} className="max-sm:hidden">
              {hazard.label}
            </Badge>
          )}
          {statement.status !== "skipped" && (
            <span className="text-sm text-muted-foreground tabular-nums">
              {duration(statement.duration_ms)}
            </span>
          )}
          {multiline && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setExpanded((e) => !e)}
              aria-expanded={expanded}
            >
              {expanded ? "Less" : "More"}
            </Button>
          )}
        </div>
      </div>
      {statement.error && (
        <div className="ml-[3.25rem] grid gap-1.5 rounded-md border border-destructive/25 bg-card px-3 py-2">
          <p className="text-sm text-destructive">
            <span className="font-mono text-xs">{statement.error.sqlstate}</span>{" "}
            {statement.error.message}
          </p>
          {statement.error.detail && (
            <p className="text-sm text-muted-foreground">{statement.error.detail}</p>
          )}
          {statement.error.hint && (
            <p className="text-sm text-muted-foreground">Hint: {statement.error.hint}</p>
          )}
          {statement.offending && (
            <div className="grid gap-1.5 border-t border-border pt-2">
              <p className="text-sm text-foreground">
                {formatCount(statement.offending.count)}{" "}
                {statement.offending.count === 1 ? "row breaks" : "rows break"} it
                {statement.offending.sample.length > 0 && ", for example:"}
              </p>
              {statement.offending.sample.length > 0 && (
                <ul className="grid gap-0.5 font-mono text-xs text-muted-foreground">
                  {statement.offending.sample.map((row, i) => (
                    <li key={i} className="truncate">
                      {row}
                    </li>
                  ))}
                </ul>
              )}
              {onQuery && (
                <div>
                  <Button size="sm" onClick={() => onQuery(statement.offending?.query ?? "")}>
                    Query them
                  </Button>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </li>
  );
}

/** Locks, rewrites, rows changed: what the statement did to the table. */
function Facts({ statement }: { statement: MigrationStatementResult }) {
  if (statement.status === "skipped")
    return (
      <p className="text-sm text-muted-foreground">
        Skipped: each statement runs in its own transaction here.
      </p>
    );
  if (statement.status !== "passed") return null;
  const facts: React.ReactNode[] = [];
  const locks = statement.locks ?? [];
  const rewrote = statement.rewrote ?? [];
  for (const lock of locks) {
    facts.push(
      <span key={`lock-${lock.relation}`}>
        <span className="font-medium text-foreground">{lockName(lock.mode)}</span> on{" "}
        {lock.relation}
        {statement.locks_documented && " (documented, not measured)"}
      </span>,
    );
  }
  for (const table of rewrote) {
    facts.push(
      <span key={`rewrote-${table}`} className="inline-flex items-center gap-1 text-warning">
        <WarningTriangleIcon aria-hidden className="size-3.5" />
        rewrote {table}
      </span>,
    );
  }
  if (rewrote.length === 0 && locks.length > 0) {
    facts.push(<span key="no-rewrite">no rewrite</span>);
  }
  if (statement.rows_changed) {
    facts.push(
      <span key="rows">
        {formatCount(statement.rows_changed)} row{statement.rows_changed === 1 ? "" : "s"} changed
      </span>,
    );
  }
  if (facts.length === 0) return null;
  return (
    <p className="flex flex-wrap gap-x-1.5 text-sm text-muted-foreground">
      {facts.map((fact, i) => (
        <React.Fragment key={i}>
          {i > 0 && <span aria-hidden>·</span>}
          {fact}
        </React.Fragment>
      ))}
    </p>
  );
}
