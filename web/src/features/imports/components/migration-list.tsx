import { Link } from "@tanstack/react-router";
import * as React from "react";

import { CheckCircleIcon, CircleIcon, NavArrowDownIcon, XmarkCircleIcon } from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import type { ImportError, PlannedMigration, SkippedStatement } from "../api";
import { skipReasonHint, skipReasonLabel } from "../labels";

/** Where a migration is in a running (or finished) import. */
export type RowProgress =
  | { state: "waiting" }
  | { state: "running" }
  | { state: "replayed"; changes: string }
  /** Ran without error before the import stopped: nothing was stored. */
  | { state: "passed" }
  | { state: "failed" };

/** Lines of the file around an error, to show it where it is. */
export interface Excerpt {
  /** The first line's number. */
  from: number;
  lines: string[];
  /** The line the error is on. */
  at: number;
}

interface MigrationListProps {
  org: string;
  db: string;
  migrations: PlannedMigration[];
  /** The number the first new migration gets. */
  firstNumber: number | null;
  /** Per migration index, while importing and after. */
  progress?: Map<number, RowProgress>;
  /** Numbers given by a finished import, per migration index: links. */
  created?: Map<number, number>;
  /** The migration that stopped the import, with its errors. */
  failure?: { index: number; errors: ImportError[]; excerpt: Excerpt | null };
}

/**
 * The migrations in the order they replay: the version each becomes, its
 * file, whether it is new, what it skips (unfolded on demand), and while
 * importing, how far it got.
 */
export function MigrationList({
  org,
  db,
  migrations,
  firstNumber,
  progress,
  created,
  failure,
}: MigrationListProps) {
  // Numbers the new ones will get, in order.
  const numbers = React.useMemo(() => {
    const out = new Map<number, number>();
    let next = firstNumber ?? 1;
    migrations.forEach((m, i) => {
      if (m.status === "new") out.set(i, next++);
      else if (m.version != null) out.set(i, m.version);
    });
    return out;
  }, [migrations, firstNumber]);

  // "New" says something only next to migrations that are not; once
  // imported, the version's link says it.
  const badges = !created && migrations.some((m) => m.status !== "new");

  // A long run of migrations imported already folds into one row: what is
  // new is what this import is about.
  const imported = migrations.findIndex((m) => m.status !== "imported" && m.status !== "removed");
  const run = imported === -1 ? migrations.length : imported;
  const [unfolded, setUnfolded] = React.useState(false);
  const folded = run >= FOLD_FROM && !unfolded;

  return (
    <ol aria-label="Migrations, in order" className="divide-y divide-border">
      {folded && (
        <li className="flex min-h-12 items-center gap-3 px-4 py-2">
          <span className="min-w-0 flex-1 text-sm text-muted-foreground">
            <span className="font-mono font-semibold text-faint-foreground">
              v{numbers.get(0)} to v{numbers.get(run - 1)}
            </span>{" "}
            {run === migrations.length ? "are" : `(${run} migrations) are`} imported already
          </span>
          <Button variant="ghost" size="sm" onClick={() => setUnfolded(true)}>
            Show them
          </Button>
        </li>
      )}
      {migrations.map((m, i) =>
        folded && i < run ? null : (
          <Row
            key={`${m.path}:${m.id}`}
            org={org}
            db={db}
            migration={m}
            badge={badges || m.status !== "new"}
            number={created?.get(i) ?? numbers.get(i)}
            linked={created?.has(i) ?? false}
            progress={progress?.get(i)}
            failure={failure?.index === i ? failure : undefined}
          />
        ),
      )}
    </ol>
  );
}

/** Imported migrations in a row before folding them. */
const FOLD_FROM = 4;

function Row({
  org,
  db,
  migration,
  badge,
  number,
  linked,
  progress,
  failure,
}: {
  org: string;
  db: string;
  migration: PlannedMigration;
  badge: boolean;
  number: number | undefined;
  linked: boolean;
  progress: RowProgress | undefined;
  failure: { errors: ImportError[]; excerpt: Excerpt | null } | undefined;
}) {
  const [open, setOpen] = React.useState(false);
  const skipped = migration.skipped;
  const errors = failure?.errors ?? migration.errors;
  const detailsId = React.useId();
  const shown = migration.status !== "imported" && migration.status !== "removed";
  const dim = migration.status === "imported" || migration.status === "removed";
  const toggle = shown && skipped.length > 0 && (
    <Button
      variant="ghost"
      size="sm"
      aria-expanded={open}
      aria-controls={detailsId}
      onClick={() => setOpen((o) => !o)}
      className="text-muted-foreground"
    >
      {skipped.length} skipped
      <NavArrowDownIcon
        aria-hidden
        className={cn("transition-transform motion-reduce:transition-none", open && "rotate-180")}
      />
    </Button>
  );

  return (
    <li
      className={cn("px-4 py-2.5", errors.length > 0 && "bg-destructive/4")}
      aria-current={progress?.state === "running" ? "step" : undefined}
    >
      <div className="flex min-h-8 items-center gap-3">
        {progress && <ProgressIcon progress={progress} />}
        <span
          className={cn(
            "w-11 shrink-0 font-mono text-sm font-semibold tabular-nums",
            dim ? "text-faint-foreground" : "text-foreground",
          )}
        >
          {number != null &&
            (linked ? (
              <Link
                to="/$org/$db/v/$number"
                params={{ org, db, number }}
                className="rounded-xs text-primary underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              >
                v{number}
              </Link>
            ) : (
              `v${number}`
            ))}
        </span>
        <span className="grid min-w-0 flex-1 gap-0.5">
          <span
            className={cn(
              "truncate font-mono text-sm",
              dim ? "text-muted-foreground" : "text-foreground",
            )}
            title={migration.name}
          >
            {migration.name}
          </span>
          <span className="flex min-w-0 items-center gap-2 text-xs text-muted-foreground">
            {migration.path !== migration.name && (
              <span className="truncate font-mono" title={migration.path}>
                {migration.path}
                {migration.line > 1 && `:${migration.line}`}
              </span>
            )}
            {progress?.state === "replayed" && (
              <span className="shrink-0 text-foreground/80">{sentence(progress.changes)}</span>
            )}
          </span>
          {/* Narrow screens: under the name, which keeps the width. */}
          {toggle && <span className="-ml-2.5 sm:hidden">{toggle}</span>}
        </span>
        {toggle && <span className="shrink-0 max-sm:hidden">{toggle}</span>}
        {badge && <StatusBadge migration={migration} />}
      </div>
      {errors.length > 0 && <ErrorDetails errors={errors} excerpt={failure?.excerpt ?? null} />}
      {open && shown && skipped.length > 0 && <SkippedList id={detailsId} statements={skipped} />}
    </li>
  );
}

function sentence(text: string) {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

function ProgressIcon({ progress }: { progress: RowProgress }) {
  switch (progress.state) {
    case "waiting":
      return <CircleIcon aria-label="Waiting" className="size-4 shrink-0 text-faint-foreground" />;
    case "running":
      return <Spinner className="size-4 shrink-0" label="Replaying" />;
    case "replayed":
      return <CheckCircleIcon aria-label="Replayed" className="size-4 shrink-0 text-success" />;
    case "passed":
      return (
        <CheckCircleIcon
          aria-label="Replays, not imported"
          className="size-4 shrink-0 text-faint-foreground"
        />
      );
    case "failed":
      return <XmarkCircleIcon aria-label="Failed" className="size-4 shrink-0 text-destructive" />;
  }
}

function StatusBadge({ migration }: { migration: PlannedMigration }) {
  switch (migration.status) {
    case "new":
      return <Badge variant="primary">New</Badge>;
    case "imported":
      return <Badge variant="outline">Imported</Badge>;
    case "changed":
      return <Badge variant="destructive">Changed</Badge>;
    case "out_of_order":
      return <Badge variant="destructive">Out of order</Badge>;
    case "removed":
      return (
        <Badge variant="neutral" title="Its version was removed in romulus: this import skips it.">
          Removed in romulus
        </Badge>
      );
  }
}

function SkippedList({ id, statements }: { id: string; statements: SkippedStatement[] }) {
  return (
    <ul
      id={id}
      aria-label="Not replayed"
      className="mt-2 ml-14 grid gap-1 rounded-md border border-border bg-muted/40 px-3 py-2 max-sm:ml-0"
    >
      {statements.map((s, i) => (
        <li key={i} className="flex min-w-0 items-center gap-2 text-sm">
          <span className="w-12 shrink-0 font-mono text-xs text-faint-foreground tabular-nums">
            L{s.line}
          </span>
          <span
            className="min-w-0 flex-1 truncate font-mono text-xs text-foreground"
            title={s.text}
          >
            {s.text}
          </span>
          <Badge variant="neutral" title={skipReasonHint[s.reason]} className="shrink-0">
            {skipReasonLabel[s.reason]}
          </Badge>
        </li>
      ))}
    </ul>
  );
}

function ErrorDetails({ errors, excerpt }: { errors: ImportError[]; excerpt: Excerpt | null }) {
  return (
    <div className="mt-2 ml-14 grid gap-2 max-sm:ml-0">
      {errors.map((e, i) => (
        <div key={i} className="grid gap-0.5 text-sm">
          <p className="text-foreground">
            {e.line != null && (
              <span className="mr-2 font-mono text-xs text-muted-foreground tabular-nums">
                Line {e.line}
                {e.column != null && `:${e.column}`}
              </span>
            )}
            {e.message}
          </p>
          {(e.hint ?? e.detail) && <p className="text-muted-foreground">{e.hint ?? e.detail}</p>}
        </div>
      ))}
      {excerpt && <CodeExcerpt excerpt={excerpt} column={errors[0]?.column ?? null} />}
    </div>
  );
}

/** A few lines of the file, the error's line marked. */
function CodeExcerpt({ excerpt, column }: { excerpt: Excerpt; column: number | null }) {
  return (
    <pre
      aria-label="Where it failed"
      className="overflow-x-auto rounded-md border border-border bg-card py-1.5 font-mono text-xs leading-5"
    >
      {excerpt.lines.map((line, i) => {
        const n = excerpt.from + i;
        const at = n === excerpt.at;
        return (
          <React.Fragment key={n}>
            <span className={cn("block px-3", at && "bg-destructive/10")}>
              <span className="inline-block w-8 pr-3 text-right text-faint-foreground select-none">
                {n}
              </span>
              <span className={at ? "text-foreground" : "text-muted-foreground"}>
                {line || " "}
              </span>
            </span>
            {at && column != null && (
              <span aria-hidden className="block px-3 text-destructive">
                <span className="inline-block w-8 pr-3" />
                {" ".repeat(Math.max(column - 1, 0))}^
              </span>
            )}
          </React.Fragment>
        );
      })}
    </pre>
  );
}
