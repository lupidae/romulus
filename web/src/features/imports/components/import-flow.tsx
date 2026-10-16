import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { GitCompareIcon, NetworkRightIcon, RefreshDoubleIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Label } from "@/components/ui/label";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Progress } from "@/components/ui/progress";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { toast } from "@/components/ui/toaster";
import { databaseKeys } from "@/features/databases/queries";

import {
  failureOf,
  type ImportOptions,
  type ImportPreview,
  type ImportReport,
  previewImport,
  runImport,
} from "../api";
import { type Picked, tooMuch } from "../files";
import { formatLabel, formats, ignoreReasonLabel } from "../labels";
import { DropZone } from "./drop-zone";
import { type Excerpt, MigrationList, type RowProgress } from "./migration-list";
import { WaysIn } from "./ways-in";

type Phase =
  | { step: "choose"; problem?: string }
  | { step: "planning"; picked: Picked }
  | { step: "review"; picked: Picked; preview: ImportPreview }
  | {
      step: "importing";
      picked: Picked;
      preview: ImportPreview;
      /** Waiting for other imports to finish (one replays at a time). */
      queued: boolean;
      checked: boolean;
      replayed: Map<number, string>;
    }
  | { step: "done"; picked: Picked; preview: ImportPreview; report: ImportReport }
  | {
      step: "failed";
      picked: Picked;
      preview: ImportPreview;
      error: ApiError;
      replayed: Map<number, string>;
      excerpt: Excerpt | null;
    };

/**
 * Migrations in, versions out: pick the files, review what was found, then
 * import with progress per migration. Nothing is stored unless every
 * migration replays.
 */
export function ImportFlow({
  org,
  db,
  databaseId,
}: {
  org: string;
  db: string;
  databaseId: string;
}) {
  const [phase, setPhase] = React.useState<Phase>({ step: "choose" });
  const [options, setOptions] = React.useState<ImportOptions>({});
  const queryClient = useQueryClient();
  const heading = React.useRef<HTMLHeadingElement>(null);

  const plan = useMutation({
    mutationFn: ({ picked, opts }: { picked: Picked; opts: ImportOptions }) =>
      previewImport(org, db, picked.files, opts),
  });
  const run = useMutation({
    mutationFn: ({
      picked,
      opts,
      onEvent,
    }: {
      picked: Picked;
      opts: ImportOptions;
      onEvent: Parameters<typeof runImport>[4];
    }) => runImport(org, db, picked.files, opts, onEvent),
  });

  // A new step is a new screen: its heading takes the focus.
  React.useEffect(() => {
    if (phase.step !== "planning" && phase.step !== "importing") heading.current?.focus();
  }, [phase.step]);

  const planFiles = (picked: Picked, opts: ImportOptions) => {
    const problem = tooMuch(picked);
    if (problem) {
      setPhase({ step: "choose", problem });
      return;
    }
    setPhase((current) => (current.step === "review" ? current : { step: "planning", picked }));
    plan.mutate(
      { picked, opts },
      {
        onSuccess: (preview) => setPhase({ step: "review", picked, preview }),
        onError: (error) =>
          setPhase({
            step: "choose",
            problem:
              error instanceof ApiError
                ? error.message
                : "Could not read the files. Check your connection and try again.",
          }),
      },
    );
  };

  const changeFormat = (format: ImportOptions["format"]) => {
    if (phase.step !== "review") return;
    const next = { ...options, format };
    setOptions(next);
    planFiles(phase.picked, next);
  };

  const start = (picked: Picked, preview: ImportPreview) => {
    const replayed = new Map<number, string>();
    setPhase({ step: "importing", picked, preview, queued: false, checked: false, replayed });
    run.mutate(
      {
        picked,
        opts: options,
        onEvent: (event) => {
          if (event.event === "queued") {
            setPhase((p) => (p.step === "importing" ? { ...p, queued: true } : p));
          } else if (event.event === "checked") {
            setPhase((p) => (p.step === "importing" ? { ...p, queued: false, checked: true } : p));
          } else if (event.event === "replayed") {
            setPhase((p) =>
              p.step === "importing"
                ? {
                    ...p,
                    queued: false,
                    checked: true,
                    replayed: new Map(p.replayed).set(event.index, event.changes),
                  }
                : p,
            );
          }
        },
      },
      {
        onSuccess: (report) => {
          void queryClient.invalidateQueries({ queryKey: databaseKeys.one(org, db) });
          void queryClient.invalidateQueries({ queryKey: databaseKeys.versions(org, db) });
          void queryClient.invalidateQueries({ queryKey: databaseKeys.list(org), exact: true });
          setPhase({ step: "done", picked, preview, report });
          toast.success(
            report.created === 1 ? "1 version created" : `${report.created} versions created`,
            { description: `Imported from ${formatLabel(report.format)} migrations.` },
          );
        },
        onError: (error) => {
          const apiError =
            error instanceof ApiError
              ? error
              : new ApiError(0, {
                  code: "network",
                  message:
                    "The connection was lost. The import may still finish: check the history.",
                });
          setPhase((p) => ({
            step: "failed",
            picked,
            preview,
            error: apiError,
            replayed: p.step === "importing" ? p.replayed : new Map<number, string>(),
            excerpt: null,
          }));
          const failure = failureOf(apiError);
          if (failure) {
            void excerptOf(picked, failure.path, failure.errors[0]?.line ?? null).then((excerpt) =>
              setPhase((p) => (p.step === "failed" ? { ...p, excerpt } : p)),
            );
          }
        },
      },
    );
  };

  const reset = () => {
    setOptions({});
    setPhase({ step: "choose" });
  };

  return (
    <div className="grid w-full max-w-4xl gap-6 px-4 py-6 md:px-8">
      <div className="grid gap-1">
        <h2
          ref={heading}
          tabIndex={-1}
          className="text-lg font-semibold text-foreground outline-none"
        >
          Import migrations
        </h2>
        <p className="max-w-prose text-base text-muted-foreground">
          Each migration becomes a version. Nothing is stored unless all of them replay.
        </p>
      </div>

      {phase.step === "choose" || phase.step === "planning" ? (
        <>
          {phase.step === "choose" && phase.problem && (
            <Alert variant="destructive" title="These files cannot be imported">
              {phase.problem}
            </Alert>
          )}
          <DropZone
            busy={phase.step === "planning"}
            onPicked={(picked) => planFiles(picked, options)}
          />
          <WaysIn databaseId={databaseId} />
        </>
      ) : (
        <Review
          org={org}
          db={db}
          phase={phase}
          options={options}
          replanning={plan.isPending}
          onFormat={changeFormat}
          onAppend={(append) => setOptions((o) => ({ ...o, append }))}
          onImport={() => {
            if (phase.step === "review" || phase.step === "failed")
              start(phase.picked, phase.preview);
          }}
          onRestart={reset}
        />
      )}
    </div>
  );
}

/** A few lines around `line` of the file `path`, read from what was picked. */
async function excerptOf(
  picked: Picked,
  path: string,
  line: number | null,
): Promise<Excerpt | null> {
  if (line == null) return null;
  const file = picked.files.find((f) => f.path === path || f.path.endsWith(`/${path}`));
  if (!file || /\.zip$/i.test(file.path)) return null;
  const lines = (await file.file.text()).split(/\r?\n/);
  const from = Math.max(line - 3, 1);
  return { from, at: line, lines: lines.slice(from - 1, Math.min(line + 2, lines.length)) };
}

type Reviewed = Exclude<Phase, { step: "choose" } | { step: "planning" }>;

function Review({
  org,
  db,
  phase,
  options,
  replanning,
  onFormat,
  onAppend,
  onImport,
  onRestart,
}: {
  org: string;
  db: string;
  phase: Reviewed;
  options: ImportOptions;
  replanning: boolean;
  onFormat: (format: ImportOptions["format"]) => void;
  onAppend: (append: boolean) => void;
  onImport: () => void;
  onRestart: () => void;
}) {
  const { preview, picked } = phase;
  const migrations = preview.migrations;
  const newIndexes = React.useMemo(
    () => migrations.flatMap((m, i) => (m.status === "new" ? [i] : [])),
    [migrations],
  );
  const first = preview.first_number ?? null;
  const last = first != null ? first + preview.new - 1 : null;

  // Row states while importing and after.
  const progress = React.useMemo(() => {
    if (phase.step === "review") return undefined;
    const rows = new Map<number, RowProgress>();
    const replayed = phase.step === "done" ? null : phase.replayed;
    const failedAt = phase.step === "failed" ? failureOf(phase.error)?.index : undefined;
    let running = phase.step === "importing" && phase.checked;
    for (const i of newIndexes) {
      if (phase.step === "done") {
        const report = phase.report.migrations[i];
        rows.set(i, { state: "replayed", changes: report?.changes ?? "" });
      } else if (replayed?.has(i)) {
        rows.set(i, { state: "replayed", changes: replayed.get(i) ?? "" });
      } else if (failedAt === i) {
        rows.set(i, { state: "failed" });
      } else if (failedAt !== undefined && i < failedAt) {
        rows.set(i, { state: "passed" });
      } else if (running) {
        rows.set(i, { state: "running" });
        running = false;
      } else {
        rows.set(i, { state: "waiting" });
      }
    }
    return rows;
  }, [phase, newIndexes]);

  const created = React.useMemo(() => {
    if (phase.step !== "done") return undefined;
    const out = new Map<number, number>();
    phase.report.migrations.forEach((m, i) => {
      if (m.status === "new" && m.version != null) out.set(i, m.version);
    });
    return out;
  }, [phase]);

  const failure = phase.step === "failed" ? failureOf(phase.error) : null;
  const blocked =
    preview.problem != null || preview.new === 0 || (preview.needs_append && !options.append);
  const busy = phase.step === "importing";

  return (
    <Card className="overflow-hidden">
      <SummaryBar phase={phase} options={options} replanning={replanning} onFormat={onFormat} />

      <div className="grid gap-3 empty:hidden [&:not(:empty)]:border-b [&:not(:empty)]:border-border [&:not(:empty)]:p-4">
        {phase.step === "review" && preview.problem && (
          <Alert variant="destructive" title={problemTitle(preview.problem.code)}>
            {preview.problem.message}
          </Alert>
        )}
        {phase.step === "review" && !preview.problem && preview.new === 0 && (
          <Alert
            variant="info"
            title="Every migration here is imported already"
            action={
              <Button size="sm" asChild>
                <Link to="/$org/$db/history" params={{ org, db }}>
                  View history
                </Link>
              </Button>
            }
          >
            Add new migration files to the folder to import them.
          </Alert>
        )}
        {phase.step === "review" && preview.needs_append && !preview.problem && (
          <Alert variant="warning" title="This database already has versions">
            <div className="mt-1 flex items-start gap-2">
              <Checkbox
                id="import-append"
                checked={options.append === true}
                onCheckedChange={(on) => onAppend(on === true)}
                className="mt-0.5"
              />
              <Label htmlFor="import-append" className="text-sm font-normal text-foreground">
                Add the {preview.new === 1 ? "migration" : "migrations"} after them, as{" "}
                {first === last ? `v${first}` : `v${first} to v${last}`}
              </Label>
            </div>
          </Alert>
        )}
        {phase.step === "failed" && (
          <Alert
            variant="destructive"
            title={failure ? `${failure.name} does not replay` : "The import stopped"}
          >
            {failure
              ? "Nothing was imported. Fix the migration below, then import again."
              : phase.error.message}
          </Alert>
        )}
        {phase.step === "done" && created && created.size > 0 && (
          <DoneBanner org={org} db={db} created={[...created.values()]} />
        )}
      </div>

      <div className={replanning ? "opacity-60 transition-opacity" : undefined}>
        <MigrationList
          org={org}
          db={db}
          migrations={migrations}
          firstNumber={first ?? null}
          progress={progress}
          created={created}
          failure={
            failure
              ? {
                  index: failure.index,
                  errors: failure.errors,
                  excerpt: phase.step === "failed" ? phase.excerpt : null,
                }
              : undefined
          }
        />
      </div>

      {phase.step !== "done" && (
        <div className="flex flex-col-reverse gap-2 border-t border-border px-4 py-3 sm:flex-row sm:items-center">
          <p className="min-w-0 flex-1 text-sm text-muted-foreground">
            {busy
              ? "You can leave this page: the import finishes on the server."
              : picked.left > 0
                ? `${picked.left} other ${picked.left === 1 ? "file was" : "files were"} not sent (not SQL).`
                : null}
          </p>
          <Button onClick={onRestart} disabled={busy}>
            Choose other files
          </Button>
          <Button
            variant="primary"
            onClick={onImport}
            disabled={phase.step === "review" ? blocked || replanning : false}
            loading={busy}
          >
            {phase.step === "failed" ? (
              <>
                <RefreshDoubleIcon />
                Try again
              </>
            ) : preview.new === 0 ? (
              "Nothing to import"
            ) : preview.new === 1 ? (
              "Import 1 migration"
            ) : (
              `Import ${preview.new} migrations`
            )}
          </Button>
        </div>
      )}
      {phase.step === "done" && (
        <div className="flex justify-end border-t border-border px-4 py-3">
          <Button onClick={onRestart}>Import more</Button>
        </div>
      )}
    </Card>
  );
}

function problemTitle(code: string): string {
  switch (code) {
    case "migration_changed":
      return "A migration changed since it was imported";
    case "migration_out_of_order":
      return "A new migration comes before imported ones";
    case "too_many_migrations":
      return "Too many migrations for one import";
    case "import_failed":
      return "A migration does not parse";
    default:
      return "These migrations cannot be imported";
  }
}

/** The bar above the list: what was found, and how far the import is. */
function SummaryBar({
  phase,
  options,
  replanning,
  onFormat,
}: {
  phase: Reviewed;
  options: ImportOptions;
  replanning: boolean;
  onFormat: (format: ImportOptions["format"]) => void;
}) {
  const { preview, picked } = phase;
  const count = preview.migrations.length;
  if (phase.step === "importing") {
    const done = phase.replayed.size;
    const total = preview.new;
    const label = phase.queued
      ? "Waiting for other imports to finish…"
      : !phase.checked
        ? `Checking ${total === 1 ? "the migration" : `all ${total} migrations`} in the sandbox…`
        : done < total
          ? `Reading version ${done + 1} of ${total}…`
          : "Saving the versions…";
    return (
      <div className="grid gap-2 border-b border-border px-4 py-3">
        <p className="text-sm font-medium text-foreground" aria-live="polite">
          {label}
        </p>
        <Progress
          aria-label="Import progress"
          value={phase.checked ? done + 1 : 0}
          max={total + 1}
        />
      </div>
    );
  }
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 border-b border-border px-4 py-3">
      <p className="min-w-0 flex-1 text-sm text-muted-foreground">
        <span className="font-medium text-foreground">
          {count === 1 ? "1 migration" : `${count} migrations`}
        </span>{" "}
        in <span className="font-mono text-foreground">{picked.label}</span>
        {preview.new > 0 && preview.new < count && `, ${preview.new} new`}
      </p>
      {preview.ignored.length > 0 && <Ignored preview={preview} />}
      {phase.step !== "review" ? (
        <p className="text-sm text-muted-foreground">
          Read as <span className="font-medium text-foreground">{formatLabel(preview.format)}</span>
        </p>
      ) : (
        <div className="flex items-center gap-2">
          <Label htmlFor="import-format" className="text-sm text-muted-foreground">
            Read as
          </Label>
          <Select
            value={preview.format}
            disabled={replanning}
            onValueChange={(value) => onFormat(value as ImportOptions["format"])}
          >
            <SelectTrigger id="import-format" className="w-40">
              <SelectValue />
            </SelectTrigger>
            <SelectContent align="end">
              {formats.map((f) => (
                <SelectItem
                  key={f.value}
                  value={f.value}
                  description={f.value === preview.detected ? `Detected · ${f.files}` : f.files}
                >
                  {f.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {options.format && options.format !== preview.detected && (
            <Button variant="ghost" size="sm" onClick={() => onFormat(undefined)}>
              Use {formatLabel(preview.detected)}
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

/** "3 files left out", and which and why on demand. */
function Ignored({ preview }: { preview: ImportPreview }) {
  const n = preview.ignored.length;
  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button variant="ghost" size="sm" className="text-muted-foreground">
          {n === 1 ? "1 file left out" : `${n} files left out`}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-80 p-0">
        <ul aria-label="Files left out" className="max-h-72 divide-y divide-border overflow-y-auto">
          {preview.ignored.map((f) => (
            <li key={f.path} className="flex items-center gap-3 px-3 py-2 text-sm">
              <span
                className="min-w-0 flex-1 truncate font-mono text-xs text-foreground"
                title={f.path}
              >
                {f.path}
              </span>
              <span className="shrink-0 text-xs text-muted-foreground">
                {ignoreReasonLabel[f.reason]}
              </span>
            </li>
          ))}
        </ul>
      </PopoverContent>
    </Popover>
  );
}

function DoneBanner({ org, db, created }: { org: string; db: string; created: number[] }) {
  const first = Math.min(...created);
  const last = Math.max(...created);
  return (
    <Alert
      variant="success"
      title={
        created.length === 1
          ? `v${first} created`
          : `${created.length} versions created: v${first} to v${last}`
      }
    >
      <div className="mt-2 flex flex-wrap gap-2">
        <Button size="sm" variant="primary" asChild>
          <Link to="/$org/$db" params={{ org, db }}>
            <NetworkRightIcon />
            Open v{last}
          </Link>
        </Button>
        {first > 1 && (
          <Button size="sm" asChild>
            <Link
              to="/$org/$db/diff/$range"
              params={{ org, db, range: { from: first - 1, to: last } }}
            >
              <GitCompareIcon />
              Compare v{first - 1} → v{last}
            </Link>
          </Button>
        )}
        {created.length > 1 && (
          <Button size="sm" asChild>
            <Link to="/$org/$db/diff/$range" params={{ org, db, range: { from: first, to: last } }}>
              <GitCompareIcon />
              Compare v{first} → v{last}
            </Link>
          </Button>
        )}
        <Button size="sm" variant="ghost" asChild>
          <Link to="/$org/$db/history" params={{ org, db }}>
            View history
          </Link>
        </Button>
      </div>
    </Alert>
  );
}
