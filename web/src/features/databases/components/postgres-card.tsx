import { useQuery } from "@tanstack/react-query";
import * as React from "react";

import { ApiError } from "@/api/client";
import { XmarkCircleIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Field } from "@/components/ui/field";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toaster";

import { useChangeMajor, useCheckMajor } from "../mutations";
import { type MajorCheck, majorLabel, type PgMajor, postgresMajorsQuery } from "../postgres";
import type { Database, Diagnostic } from "../queries";
import { MajorSelect } from "./postgres";

/**
 * The database's Postgres major, and moving it to another one: picking one
 * compiles the latest version there at once (the dry run), and the switch
 * is offered only when it compiles. `initial` preselects one (the history's
 * "Switch to Postgres 16").
 */
export function PostgresCard({
  org,
  database,
  initial,
}: {
  org: string;
  database: Database;
  initial?: PgMajor;
}) {
  const current = database.pg_major;
  const [picked, setPicked] = React.useState<PgMajor>(initial ?? current);
  const check = useCheckMajor(org, database.slug);
  const change = useChangeMajor(org, database.slug);
  const majors = useQuery(postgresMajorsQuery());
  const server = majors.data?.find((m) => m.major === current)?.server_version?.split(" ")[0];
  const latest = database.latest?.number;

  // Each pick is checked once; picking back the current one clears it.
  const { mutate: runCheck, reset: resetCheck } = check;
  React.useEffect(() => {
    if (picked === current) resetCheck();
    else runCheck(picked);
  }, [picked, current, runCheck, resetCheck]);

  const result = check.data?.pg_major === picked ? check.data : undefined;

  // Asked for by a link (the history's "Switch to…"): brought into view,
  // within the page's own scroll area only (scrollIntoView would also move
  // the app's frame).
  const card = React.useRef<HTMLElement>(null);
  React.useEffect(() => {
    if (initial === undefined) return;
    const element = card.current;
    let scroller = element?.parentElement;
    while (scroller && !["auto", "scroll"].includes(getComputedStyle(scroller).overflowY)) {
      scroller = scroller.parentElement;
    }
    if (!element || !scroller) return;
    const top =
      element.getBoundingClientRect().top -
      scroller.getBoundingClientRect().top +
      scroller.scrollTop;
    scroller.scrollTo({ top: Math.max(0, top - 24) });
  }, [initial]);
  const switchTo = () =>
    change.mutate(picked, {
      onSuccess: (saved) =>
        toast.success(`${saved.name} is on ${majorLabel(saved.pg_major)}`, {
          description: "Uploads, drafts and generated migrations use it from now on.",
        }),
      onError: (error) => {
        if (error instanceof ApiError && error.code === "latest_changed") {
          toast.info("A new version arrived meanwhile", {
            description: `Checked again on ${majorLabel(picked)}.`,
          });
          runCheck(picked);
        } else if (error instanceof ApiError && error.code === "incompatible_major") {
          runCheck(picked);
        } else {
          toast.error("Could not switch", {
            description: error instanceof ApiError ? error.message : "Check your connection.",
          });
        }
      },
    });

  return (
    <Card ref={card} aria-labelledby="postgres-title">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle id="postgres-title">Postgres version</CardTitle>
          <p className="text-sm text-muted-foreground">
            Match your server: SQL compiles on it, and migrations are written for it.
          </p>
        </div>
      </CardHeader>
      <CardContent className="grid gap-4">
        <Field
          label="Version"
          description={
            picked === current
              ? `Compiles on ${majorLabel(current)}${server ? ` (${server} on this server)` : ""}.`
              : `Now on ${majorLabel(current)}.`
          }
        >
          <MajorSelect value={picked} current={current} onValueChange={setPicked} />
        </Field>
        {picked !== current && (
          <div aria-live="polite" className="grid gap-3">
            {check.isPending && (
              <p className="flex items-center gap-2 text-sm text-muted-foreground">
                <Spinner />
                {latest
                  ? `Compiling v${latest} on ${majorLabel(picked)}…`
                  : `Checking ${majorLabel(picked)}…`}
              </p>
            )}
            {check.isError && (
              <Alert
                variant="destructive"
                title={`Could not check ${majorLabel(picked)}`}
                action={
                  <Button size="sm" onClick={() => runCheck(picked)}>
                    Retry
                  </Button>
                }
              >
                {check.error instanceof ApiError
                  ? check.error.message
                  : "Check your connection, then try again."}
              </Alert>
            )}
            {result && <CheckResult result={result} from={current} />}
          </div>
        )}
      </CardContent>
      {picked !== current && (
        <div className="flex flex-col-reverse gap-2 border-t border-border px-4 py-3 sm:flex-row sm:justify-end">
          <Button onClick={() => setPicked(current)}>Keep {majorLabel(current)}</Button>
          <Button
            variant="primary"
            disabled={!result?.compiles}
            loading={change.isPending}
            onClick={switchTo}
          >
            Switch to {majorLabel(picked)}
          </Button>
        </div>
      )}
    </Card>
  );
}

/** What the dry run found, in words: the switch's consequence or its blocker. */
function CheckResult({ result, from }: { result: MajorCheck; from: PgMajor }) {
  const to = majorLabel(result.pg_major);
  const version = result.against === null ? null : `v${result.against}`;
  if (result.compiles) {
    const changes = result.changes;
    return (
      <Alert
        variant="success"
        title={version ? `${version} compiles on ${to}` : "No version to check yet"}
      >
        {version
          ? changes && !changes.unchanged
            ? `Its schema reads differently there: ${changes.summary}. `
            : "Its schema reads the same there. "
          : "The database can move freely. "}
        Drafts will compile on {to}; past versions stay as they are.
      </Alert>
    );
  }
  const errors = result.diagnostics.filter((d) => d.severity === "error");
  return (
    <div className="grid gap-2">
      <Alert
        variant="destructive"
        title={`${version ?? "The latest version"} does not compile on ${to}`}
      >
        {errors.length === 1 ? "Fix it" : `Fix its ${errors.length} errors`} in a draft, or stay on{" "}
        {majorLabel(from)}.
      </Alert>
      <ul
        aria-label={`Errors on ${to}`}
        className="divide-y divide-border overflow-hidden rounded-lg border border-border"
      >
        {errors.map((d, i) => (
          <DiagnosticRow key={i} diagnostic={d} sql={result.sql ?? ""} />
        ))}
      </ul>
    </div>
  );
}

/** One error: where, what Postgres said, and the line it is about. */
function DiagnosticRow({ diagnostic, sql }: { diagnostic: Diagnostic; sql: string }) {
  const line = diagnostic.span ? excerpt(sql, diagnostic.span.from) : undefined;
  return (
    <li className="flex items-start gap-2.5 px-3 py-2.5">
      <XmarkCircleIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-destructive" />
      <div className="grid min-w-0 gap-1">
        <p className="text-sm text-foreground">
          {diagnostic.span && (
            <span className="mr-2 font-mono text-xs text-muted-foreground tabular-nums">
              Line {diagnostic.span.from.line}
            </span>
          )}
          {diagnostic.message}
        </p>
        {line && (
          <code className="block truncate rounded-sm bg-muted px-1.5 py-0.5 font-mono text-xs text-muted-foreground">
            {line}
          </code>
        )}
        {(diagnostic.hint ?? diagnostic.detail) && (
          <p className="text-sm text-muted-foreground">{diagnostic.hint ?? diagnostic.detail}</p>
        )}
      </div>
    </li>
  );
}

/** The line an error is on, from a little before its column (long lines start at the point). */
function excerpt(sql: string, at: { line: number; column: number }): string | undefined {
  const line = sql.split("\n")[at.line - 1];
  if (line === undefined) return undefined;
  const from = Math.max(0, at.column - 1 - 24);
  const shown = line.slice(from).trimStart();
  return from > 0 && line.slice(0, from).trim() ? `…${shown}` : shown;
}
