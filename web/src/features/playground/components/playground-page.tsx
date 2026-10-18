import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import {
  ArrowLeftIcon,
  CheckIcon,
  CircleIcon,
  DatabaseScriptIcon,
  FlaskIcon,
  MoreHorizIcon,
  RefreshDoubleIcon,
  TrashIcon,
  XmarkCircleIcon,
} from "@/components/icons";
import { AvatarStack } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { EmptyState } from "@/components/ui/empty-state";
import { Progress } from "@/components/ui/progress";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { meQuery } from "@/features/auth/queries";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";
import { timeAgo } from "@/lib/time";
import { cn } from "@/lib/utils";

import type { Announcement } from "../console";
import { runMigration, useDeletePlayground, useResetPlayground } from "../mutations";
import {
  type BuildProgress,
  type Playground,
  type PlaygroundStage,
  playgroundKeys,
  playgroundQuery,
} from "../queries";
import { useConsoleSession, useConsoleState } from "../use-console";
import { CreatePlaygroundDialog } from "./create-playground-dialog";
import { verdict } from "../format";
import { RunMigrationDialog } from "./run-migration-dialog";
import { type MigrationView, Workspace } from "./workspace";

/**
 * A playground, full screen: its bar (where it is, who is here, Reset and
 * Run migration for developers), then by status: the build's progress, the
 * console, or why there is nothing (failed, expired).
 */
export function PlaygroundPage({ org, db, id }: { org: string; db: string; id: string }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const { data: me } = useSuspenseQuery(meQuery);
  const { data: playground } = useSuspenseQuery(playgroundQuery(org, db, id));
  const writer = canWrite(orgData.role);
  const ready = playground.status === "ready";
  const session = useConsoleSession(org, db, id, ready);
  // A run started here (live, then its report); otherwise the run whose
  // rows the playground holds, whoever ran it.
  const [local, setLocal] = React.useState<{ view: MigrationView; at: number } | null>(null);
  const [tab, setTab] = React.useState("results");
  const held = playground.migrated_by;
  const heldAt = held ? new Date(held.started_at).getTime() : 0;
  const migration: MigrationView | null =
    local && (local.view.live || !held || local.at >= heldAt)
      ? local.view
      : held?.report
        ? { live: null, report: held.report, error: null, by: held.by.name }
        : (local?.view ?? null);
  const setMigration = (view: MigrationView) => setLocal({ view, at: Date.now() });

  // Someone else reset it or ran the migration: say so, and catch up.
  const notice = useConsoleState(session, (s) => s.notice, null);
  React.useEffect(() => {
    if (!notice) return;
    void queryClient.invalidateQueries({ queryKey: playgroundKeys.one(org, db, id) });
    void queryClient.invalidateQueries({ queryKey: playgroundKeys.runs(org, db, id) });
    // By id: two people may share a name.
    if (notice.by.id === me.id) return;
    const said: Partial<Record<Announcement, string>> = {
      reset: `${notice.by.name} reset the rows`,
      migrating: `${notice.by.name} is running the migration`,
      migrated: `${notice.by.name} ran the migration`,
    };
    const message = said[notice.event];
    if (message) toast.info(message, { description: description(notice.event) });
  }, [notice, queryClient, org, db, id, me.id]);

  const startMigration = async (sha256: string) => {
    // The host puts the generated rows back first.
    setMigration({
      live: { resetting: true, running: null, total: null, done: [] },
      report: null,
      error: null,
    });
    setTab("migration");
    try {
      const report = await runMigration(org, db, id, sha256, (event) => {
        setLocal((current) => {
          const view = current?.view ?? { live: null, report: null, error: null };
          const live = view.live ?? { resetting: false, running: null, total: null, done: [] };
          switch (event.type) {
            case "resetting":
              return { at: Date.now(), view: { ...view, live: { ...live, resetting: true } } };
            case "running":
              return {
                at: Date.now(),
                view: {
                  ...view,
                  live: { ...live, resetting: false, running: event.index, total: event.total },
                },
              };
            case "statement":
              return {
                at: Date.now(),
                view: {
                  ...view,
                  live: { ...live, running: null, done: [...live.done, event.result] },
                },
              };
            default:
              return current;
          }
        });
      });
      setMigration({ live: null, report, error: null, by: me.name });
      toast[report.passed ? "success" : "warning"](
        report.passed ? "The migration passed" : verdict(report),
        { description: "The playground holds the migrated rows until someone resets them." },
      );
    } catch (error) {
      setMigration({
        live: null,
        report: null,
        error:
          error instanceof ApiError
            ? error.message
            : "The run could not finish. Check your connection.",
      });
    } finally {
      void queryClient.invalidateQueries({ queryKey: playgroundKeys.one(org, db, id) });
      void queryClient.invalidateQueries({ queryKey: playgroundKeys.runs(org, db, id) });
    }
  };

  return (
    <div className="flex h-dvh flex-col bg-background">
      <PlaygroundBar
        org={org}
        db={db}
        playground={playground}
        writer={writer}
        session={session}
        meId={me.id}
        migrating={migration?.live != null}
        onRunMigration={(sha256) => void startMigration(sha256)}
        onDeleted={() => void navigate({ to: "/$org/$db", params: { org, db } })}
      />
      {playground.status === "building" ? (
        <BuildingView playground={playground} />
      ) : playground.status === "failed" ? (
        <Ended
          org={org}
          db={db}
          playground={playground}
          writer={writer}
          title="The build failed"
          description={playground.failure ?? "The playground could not be built."}
        />
      ) : playground.status === "expired" ? (
        <Ended
          org={org}
          db={db}
          playground={playground}
          writer={writer}
          title="This playground expired"
          description="Playgrounds are deleted after 7 days unused, or when their draft is published or deleted. The same seed gives the same rows again."
        />
      ) : (
        <>
          {playground.migrated_by && (
            <MigratedBar playground={playground} writer={writer} org={org} db={db} />
          )}
          <Workspace
            org={org}
            db={db}
            playground={playground}
            session={session}
            migration={migration}
            onShowMigration={(view) => {
              setMigration(view);
              setTab("migration");
            }}
            tab={tab}
            onTab={setTab}
          />
        </>
      )}
    </div>
  );
}

function description(event: Announcement): string | undefined {
  switch (event) {
    case "reset":
      return "It holds the generated rows again.";
    case "migrating":
      return "The console pauses until it is done.";
    case "migrated":
      return "It holds the migrated rows: the Migration tab has the report.";
    default:
      return undefined;
  }
}

function sourceLabel(playground: Playground): string {
  return playground.draft
    ? `“${playground.draft.name}” · v${playground.version}`
    : `v${playground.version}`;
}

function PlaygroundBar({
  org,
  db,
  playground,
  writer,
  session,
  meId,
  migrating,
  onRunMigration,
  onDeleted,
}: {
  org: string;
  db: string;
  playground: Playground;
  writer: boolean;
  session: ReturnType<typeof useConsoleSession>;
  /** Who is looking: the others are shown. */
  meId: string;
  migrating: boolean;
  onRunMigration: (sha256: string) => void;
  onDeleted: () => void;
}) {
  const [resetting, setResetting] = React.useState(false);
  const [deleting, setDeleting] = React.useState(false);
  const [running, setRunning] = React.useState(false);
  const reset = useResetPlayground(org, db, playground.id);
  const remove = useDeletePlayground(org, db, playground.id);
  const livePeople = useConsoleState(session, (s) => s.present, null);
  const connection = useConsoleState(session, (s) => s.connection, "connecting");
  const people = (livePeople ?? playground.present).filter((p) => p.id !== meId);
  const ready = playground.status === "ready";
  const draft = playground.draft;
  // What the host is doing, for everyone here (its status, or the console
  // paused by someone's reset or migration): Reset, Run migration and
  // Delete wait for it.
  const busy =
    playground.live?.busy ??
    (connection === "paused" ? "busy" : null) ??
    (migrating ? "migrating" : null);
  const busyText = busy ? `The playground is ${busy}: wait until it is done.` : null;
  const canMigrate = writer && ready && draft && draft.status === "open" && !draft.moved;
  const migrateBlocked = !draft
    ? null
    : draft.status !== "open"
      ? "The draft was published or deleted."
      : draft.moved
        ? "The draft moved to another version since this playground was made: create a new one."
        : busyText;

  return (
    <header className="flex h-12 shrink-0 items-center gap-1.5 border-b border-border bg-card px-2 md:gap-2 md:px-3">
      <Tooltip content={draft ? `Back to “${draft.name}”` : "Back to the database"}>
        <Button variant="ghost" size="icon-sm" asChild>
          {draft && draft.status === "open" ? (
            <Link
              to="/$org/$db/drafts/$draft"
              params={{ org, db, draft: draft.id }}
              aria-label={`Back to “${draft.name}”`}
            >
              <ArrowLeftIcon />
            </Link>
          ) : (
            <Link to="/$org/$db" params={{ org, db }} aria-label="Back to the database">
              <ArrowLeftIcon />
            </Link>
          )}
        </Button>
      </Tooltip>
      <FlaskIcon aria-hidden className="size-4 shrink-0 text-muted-foreground max-sm:hidden" />
      <div className="flex min-w-0 items-baseline gap-2">
        <h1 className="truncate text-base font-medium text-foreground">Playground</h1>
        <span className="truncate text-sm text-muted-foreground">{sourceLabel(playground)}</span>
      </div>
      <StatusBadge playground={playground} />
      <span className="hidden text-sm text-muted-foreground lg:inline">
        {playground.size === "small" ? "Small" : "Medium"} ·{" "}
        {playground.profile === "edge_cases" ? "Edge cases" : "Realistic"} · seed{" "}
        <span className="font-mono">{playground.seed}</span>
      </span>

      <div className="ml-auto flex shrink-0 items-center gap-1 md:gap-1.5">
        {people.length > 0 && (
          <AvatarStack
            people={people}
            max={3}
            className={cn("mr-1", people.length > 2 && "max-sm:hidden")}
          />
        )}
        {writer && ready && (
          <Tooltip content={busyText ?? "Put the generated rows back, for everyone here"}>
            <Button
              variant="ghost"
              onClick={() => setResetting(true)}
              disabled={busy !== null}
              className="max-md:hidden"
            >
              <RefreshDoubleIcon />
              Reset rows
            </Button>
          </Tooltip>
        )}
        {writer && ready && draft && (
          <Tooltip content={migrateBlocked ?? "Try the draft's migration on these rows"}>
            <span className="max-sm:hidden">
              <Button
                variant="secondary"
                onClick={() => setRunning(true)}
                disabled={!canMigrate || busy !== null}
                loading={migrating}
              >
                <DatabaseScriptIcon />
                Run migration
              </Button>
            </span>
          </Tooltip>
        )}
        {writer && (
          <Menu>
            <Tooltip content="More">
              <MenuTrigger asChild>
                <Button variant="ghost" size="icon-sm" aria-label="More actions">
                  <MoreHorizIcon />
                </Button>
              </MenuTrigger>
            </Tooltip>
            <MenuContent align="end">
              {ready && draft && (
                <MenuItem
                  className="sm:hidden"
                  disabled={!canMigrate || busy !== null}
                  onSelect={() => setRunning(true)}
                >
                  <DatabaseScriptIcon />
                  Run migration…
                </MenuItem>
              )}
              {ready && (
                <MenuItem
                  className="md:hidden"
                  disabled={busy !== null}
                  onSelect={() => setResetting(true)}
                >
                  <RefreshDoubleIcon />
                  Reset rows…
                </MenuItem>
              )}
              {ready && <MenuSeparator className="md:hidden" />}
              {/* Never waits: deleting cancels a build or a migration run. */}
              <MenuItem variant="destructive" onSelect={() => setDeleting(true)}>
                <TrashIcon />
                Delete playground…
              </MenuItem>
            </MenuContent>
          </Menu>
        )}
      </div>

      <ConfirmDialog
        open={resetting}
        onOpenChange={setResetting}
        title="Reset the rows?"
        description="The generated rows come back for everyone in this playground, in about a second. Open transactions end, and what a migration run changed is gone."
        confirmLabel="Reset rows"
        onConfirm={async () => {
          try {
            await reset.mutateAsync();
            toast.success("Rows reset", {
              description: "The playground holds the generated rows again.",
            });
          } catch (error) {
            toast.error("Could not reset the rows", {
              description: error instanceof ApiError ? error.message : "Check your connection.",
            });
            throw error;
          }
        }}
      />
      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title="Delete this playground?"
        description="Its database and its history are deleted for everyone. The same seed gives the same rows in a new one."
        confirmLabel="Delete playground"
        destructive
        onConfirm={async () => {
          try {
            await remove.mutateAsync();
            toast.success("Playground deleted");
            onDeleted();
          } catch (error) {
            toast.error("Could not delete the playground", {
              description: error instanceof ApiError ? error.message : "Check your connection.",
            });
            throw error;
          }
        }}
      />
      {draft && (
        <RunMigrationDialog
          org={org}
          db={db}
          playground={playground}
          open={running}
          onOpenChange={setRunning}
          onRun={onRunMigration}
        />
      )}
    </header>
  );
}

function StatusBadge({ playground }: { playground: Playground }) {
  switch (playground.status) {
    case "building":
      return (
        <Badge variant="info" className="shrink-0">
          <Spinner className="size-3" label="Building" />
          Building
        </Badge>
      );
    case "failed":
      return (
        <Badge variant="destructive" className="shrink-0">
          Failed
        </Badge>
      );
    case "expired":
      return (
        <Badge variant="outline" className="shrink-0">
          Expired
        </Badge>
      );
    default:
      if (playground.live && !playground.live.running)
        return (
          <Tooltip content="Stopped after 15 minutes unused: it starts again in a few seconds when someone opens it">
            <Badge variant="outline" className="shrink-0">
              Stopped
            </Badge>
          </Tooltip>
        );
      return (
        <Badge variant="success" className="shrink-0 max-sm:hidden">
          Ready
        </Badge>
      );
  }
}

/** It holds the rows a migration run left. */
function MigratedBar({
  org,
  db,
  playground,
  writer,
}: {
  org: string;
  db: string;
  playground: Playground;
  writer: boolean;
}) {
  const run = playground.migrated_by;
  const reset = useResetPlayground(org, db, playground.id);
  if (!run) return null;
  return (
    <div
      role="status"
      className="flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-b border-border bg-info/6 px-3 py-1.5 text-sm text-foreground"
    >
      <DatabaseScriptIcon aria-hidden className="size-4 shrink-0 text-info" />
      <span>
        Holding the rows {run.by.name}&apos;s migration run left
        {run.report && (
          <span className="text-muted-foreground"> ({verdict(run.report).toLowerCase()})</span>
        )}
        <span className="text-muted-foreground"> · {timeAgo(run.started_at)}</span>
      </span>
      {writer && (
        <Button
          size="sm"
          variant="ghost"
          className="ml-auto"
          loading={reset.isPending}
          onClick={() =>
            reset.mutate(undefined, {
              onSuccess: () =>
                toast.success("Rows reset", {
                  description: "The playground holds the generated rows again.",
                }),
              onError: (error) =>
                toast.error("Could not reset the rows", {
                  description: error instanceof ApiError ? error.message : "Check your connection.",
                }),
            })
          }
        >
          <RefreshDoubleIcon />
          Reset rows
        </Button>
      )}
    </div>
  );
}

const STAGES: { stage: PlaygroundStage; label: string }[] = [
  { stage: "starting", label: "Starting its database" },
  { stage: "schema", label: "Creating the schema" },
  { stage: "rows", label: "Generating rows" },
  { stage: "analyze", label: "Gathering statistics" },
  { stage: "snapshot", label: "Keeping a copy for Reset" },
];

function BuildingView({ playground }: { playground: Playground }) {
  const progress: BuildProgress = playground.progress ?? {
    stage: "starting",
    tables_done: 0,
    tables_total: playground.tables.length,
    table: null,
  };
  const current = STAGES.findIndex((s) => s.stage === progress.stage);
  const [now, setNow] = React.useState(() => Date.now());
  React.useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  const elapsed = Math.max(0, Math.round((now - new Date(playground.created_at).getTime()) / 1000));
  return (
    <div className="flex flex-1 items-start justify-center overflow-y-auto px-4 py-10 md:items-center">
      <Card className="w-full max-w-md p-5">
        <div className="grid gap-1">
          <h2 className="text-lg font-semibold text-foreground">Building the playground</h2>
          <p className="text-base text-muted-foreground">
            v{playground.version}, {playground.size === "small" ? "small" : "medium"},{" "}
            {playground.profile === "edge_cases" ? "edge cases" : "realistic"} values. It keeps
            building if you leave.
          </p>
        </div>
        <ol className="mt-5 grid gap-3" aria-label="Steps">
          {STAGES.map((step, i) => {
            const state = i < current ? "done" : i === current ? "current" : "todo";
            return (
              <li
                key={step.stage}
                className="grid gap-1.5"
                aria-current={state === "current" ? "step" : undefined}
              >
                <div className="flex items-center gap-2.5">
                  {state === "done" ? (
                    <CheckIcon aria-hidden className="size-4 text-success" />
                  ) : state === "current" ? (
                    <Spinner className="size-4" label="In progress" />
                  ) : (
                    <CircleIcon aria-hidden className="size-4 text-faint-foreground" />
                  )}
                  <span
                    className={cn(
                      "text-base",
                      state === "todo" ? "text-muted-foreground" : "text-foreground",
                    )}
                  >
                    <span className="sr-only">
                      {state === "done" ? "Done: " : state === "current" ? "Now: " : "Next: "}
                    </span>
                    {step.label}
                  </span>
                </div>
                {state === "current" && step.stage === "rows" && progress.tables_total > 0 && (
                  <div className="ml-6.5 grid gap-1">
                    <Progress
                      value={progress.tables_done}
                      max={progress.tables_total}
                      aria-label="Tables with their rows"
                    />
                    <span className="text-sm text-muted-foreground">
                      {progress.tables_done} of {progress.tables_total} tables
                      {progress.table && <> · {progress.table}</>}
                    </span>
                  </div>
                )}
              </li>
            );
          })}
        </ol>
        <p className="mt-5 text-sm text-muted-foreground tabular-nums">{elapsed} s</p>
      </Card>
    </div>
  );
}

function Ended({
  org,
  db,
  playground,
  writer,
  title,
  description,
}: {
  org: string;
  db: string;
  playground: Playground;
  writer: boolean;
  title: string;
  description: string;
}) {
  const [creating, setCreating] = React.useState(false);
  const draft = playground.draft;
  const canRecreate = writer && (!draft || (draft.status === "open" && !draft.moved));
  return (
    <div className="flex flex-1 items-center justify-center overflow-y-auto">
      <EmptyState
        icon={playground.status === "failed" ? <XmarkCircleIcon /> : <FlaskIcon />}
        title={title}
        description={description}
        action={
          <>
            <Button asChild>
              <Link to="/$org/$db" params={{ org, db }}>
                Back to the database
              </Link>
            </Button>
            {canRecreate && (
              // A failed or expired one does not count: it stays, with why.
              <Button variant="primary" onClick={() => setCreating(true)}>
                Create a new one
              </Button>
            )}
          </>
        }
      />
      <CreatePlaygroundDialog
        org={org}
        db={db}
        open={creating}
        onOpenChange={setCreating}
        source={
          draft
            ? {
                kind: "draft",
                draftId: draft.id,
                draftName: draft.name,
                version: playground.version,
                tables: playground.tables.length,
              }
            : { kind: "version", version: playground.version, tables: playground.tables.length }
        }
      />
    </div>
  );
}
