import type { Extension } from "@codemirror/state";
import { useQuery } from "@tanstack/react-query";
import * as React from "react";

import {
  DatabaseScriptIcon,
  InfoCircleIcon,
  PauseIcon,
  PlayIcon,
  RefreshDoubleIcon,
  TableIcon,
  TerminalIcon,
  WarningTriangleIcon,
  WifiOffIcon,
  XmarkIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { KbdGroup } from "@/components/ui/kbd";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip } from "@/components/ui/tooltip";
import type { Editor } from "@/features/schema/text/codemirror";
import { CodeView } from "@/features/schema/text/code-view";
import { modKey } from "@/lib/platform";
import { timeAgo } from "@/lib/time";
import { useMediaQuery } from "@/lib/use-preference";
import { cn } from "@/lib/utils";

import {
  canRun,
  type ConsoleSession,
  type ConsoleState,
  type PlaygroundRun,
  type Run,
  type StatementResult,
} from "../console";
import type * as EditorModule from "../editor-extensions";
import {
  formatCount,
  type MigrationReport,
  type Playground,
  runsQuery,
  timeLeft,
} from "../queries";
import { statementAt } from "../statements";
import { useConsoleState } from "../use-console";
import { duration, verdict } from "../format";
import { type LiveRun, MigrationReportView } from "./migration-report";
import { ResultsGrid } from "./results-grid";

export interface MigrationView {
  live: LiveRun | null;
  report: MigrationReport | null;
  error: string | null;
  /** Who ran it, when (a run from the history). */
  by?: string;
}

interface WorkspaceProps {
  org: string;
  db: string;
  playground: Playground;
  session: ConsoleSession | null;
  migration: MigrationView | null;
  onShowMigration: (view: MigrationView) => void;
  /** Results, Plan, Messages or Migration: the parent switches to Migration. */
  tab: string;
  onTab: (tab: string) => void;
}

/**
 * The console: the tables and the shared history beside, the SQL above, the
 * results below. Read-only for everyone: the database refuses writes, and
 * says so.
 */
export function Workspace({
  org,
  db,
  playground,
  session,
  migration,
  onShowMigration,
  tab,
  onTab: setTab,
}: WorkspaceProps) {
  const wide = useMediaQuery("(min-width: 48rem)");
  const [sideOpen, setSideOpen] = React.useState(false);
  const [editor, setEditor] = React.useState<Editor | null>(null);
  const [cm, setCm] = React.useState<typeof EditorModule | null>(null);
  const [extensions, setExtensions] = React.useState<Extension[] | null>(null);
  const text = React.useRef("");
  const connection = useConsoleState(session, (s) => s.connection, "connecting");
  const runs = useConsoleState(session, (s) => s.runs, NO_RUNS);
  const running = useConsoleState(session, (s) => s.running, null);
  const [shownRun, setShownRun] = React.useState<string | null>(null);
  const run = runs.find((r) => r.id === shownRun) ?? runs[runs.length - 1] ?? null;

  // What the editor's keys call: the latest closures, the editor made once.
  const actions = React.useRef({ run: () => {}, explain: () => {}, cancel: () => {} });
  React.useEffect(() => {
    let cancelled = false;
    void import("../editor-extensions").then((module) => {
      if (cancelled) return;
      setCm(module);
      setExtensions(
        module.consoleEditorExtensions({
          onChange: (doc) => {
            text.current = doc;
          },
          onRun: () => actions.current.run(),
          onExplain: () => actions.current.explain(),
          onCancel: () => actions.current.cancel(),
        }),
      );
    });
    return () => {
      cancelled = true;
    };
  }, []);

  /** The selection, or everything (Run), or the statement under the cursor (Explain). */
  const target = (explain: boolean): { sql: string; offset: number } | null => {
    const all = editor?.view.state.doc.toString() ?? text.current;
    const selection = editor?.view.state.selection.main;
    if (selection && !selection.empty) {
      return { sql: all.slice(selection.from, selection.to), offset: selection.from };
    }
    if (!explain) return all.trim() ? { sql: all, offset: 0 } : null;
    const piece = statementAt(all, selection?.head ?? 0);
    return piece ? { sql: piece.text, offset: piece.from } : null;
  };
  const start = (explain: boolean) => {
    if (!session) return;
    const chosen = target(explain);
    if (!chosen) return;
    const id = session.run(chosen.sql, explain, chosen.offset);
    if (id) {
      setShownRun(null);
      setTab(explain ? "plan" : "results");
      if (editor && cm) cm.showMarks(editor.view, []);
    }
  };
  React.useEffect(() => {
    actions.current = {
      run: () => start(false),
      explain: () => start(true),
      cancel: () => session?.cancel(),
    };
  });

  // The run's error, underlined in the editor.
  const failed = run?.statements.find((s) => s.error)?.error;
  React.useEffect(() => {
    if (!editor || !cm || !run || !failed?.span) return;
    const offset = run.offset;
    cm.showMarks(editor.view, [
      {
        from: offset + failed.span.utf16_start,
        to: offset + failed.span.utf16_end,
        message: failed.message,
      },
    ]);
  }, [editor, cm, run, failed]);

  const load = (sql: string, runIt: boolean) => {
    if (editor) {
      editor.setDoc(sql);
      text.current = sql;
      editor.view.focus();
    }
    if (runIt) window.setTimeout(() => start(false), 0);
  };
  // A stopped playground starts for a run: Run stays available.
  const ready = canRun(connection);

  const side = (
    <SidePanel
      org={org}
      db={db}
      playground={playground}
      session={session}
      onTable={(schema, table) => {
        load(`SELECT * FROM ${quoted(schema)}.${quoted(table)} LIMIT 100;`, ready);
        setSideOpen(false);
      }}
      onRunAgain={(sql) => {
        load(sql, false);
        setSideOpen(false);
      }}
      onMigration={(entry) => {
        if (entry.report)
          onShowMigration({ live: null, report: entry.report, error: null, by: entry.by.name });
        setTab("migration");
        setSideOpen(false);
      }}
    />
  );

  return (
    <div className="flex min-h-0 flex-1">
      {wide ? (
        <aside className="flex w-64 shrink-0 flex-col border-r border-border bg-card lg:w-72">
          {side}
        </aside>
      ) : (
        <Sheet open={sideOpen} onOpenChange={setSideOpen}>
          <SheetContent className="w-80 bg-card p-0">
            <SheetTitle className="sr-only">Tables and history</SheetTitle>
            {side}
          </SheetContent>
        </Sheet>
      )}
      <main className="flex min-w-0 flex-1 flex-col">
        <ConnectionBanner session={session} />
        <div className="flex h-10 shrink-0 items-center gap-1.5 border-b border-border bg-card px-2">
          {!wide && (
            <Button variant="ghost" size="sm" onClick={() => setSideOpen(true)}>
              <TableIcon />
              Tables
            </Button>
          )}
          <Tooltip content="Run" shortcut={[modKey, "↵"]}>
            <Button
              variant="primary"
              size="sm"
              onClick={() => start(false)}
              disabled={!ready || running !== null}
              aria-label="Run"
            >
              <PlayIcon />
              Run
            </Button>
          </Tooltip>
          <Tooltip content="Explain the statement under the cursor" shortcut={["⇧", modKey, "↵"]}>
            <Button
              variant="secondary"
              size="sm"
              onClick={() => start(true)}
              disabled={!ready || running !== null}
            >
              Explain
            </Button>
          </Tooltip>
          {running && (
            <Button variant="ghost" size="sm" onClick={() => session?.cancel()}>
              <XmarkIcon />
              Cancel
            </Button>
          )}
          <div className="ml-auto flex items-center gap-2">
            <TxnBadge session={session} />
          </div>
        </div>
        <div className="h-[38%] min-h-32 shrink-0 border-b border-border">
          {extensions ? (
            <CodeView
              doc=""
              language="sql"
              label="SQL to run"
              readOnly={false}
              active
              extensions={extensions}
              onEditor={setEditor}
              className="h-full"
            />
          ) : (
            <div className="grid gap-2 p-4">
              <Skeleton className="h-4 w-2/3" />
              <Skeleton className="h-4 w-1/2" />
            </div>
          )}
        </div>
        <ResultsPane
          tab={tab}
          onTab={setTab}
          run={run}
          runs={runs}
          onRun={setShownRun}
          connection={connection}
          migration={migration}
          onQuery={(sql) => {
            load(sql, ready);
            setTab("results");
          }}
          onJump={(from, to) => {
            if (editor && cm) cm.jumpTo(editor.view, from, to);
          }}
          offset={run?.offset ?? 0}
        />
        <StatusLine playground={playground} />
      </main>
    </div>
  );
}

const NO_RUNS: Run[] = [];

function quoted(name: string): string {
  return /^[a-z_][a-z0-9_]*$/.test(name) ? name : `"${name.replaceAll('"', '""')}"`;
}

/** Why the console cannot run right now, and what happens next. */
function ConnectionBanner({ session }: { session: ConsoleSession | null }) {
  const connection = useConsoleState(session, (s) => s.connection, "connecting");
  const reason = useConsoleState(session, (s) => s.pausedBecause, null);
  const unavailable = useConsoleState(session, (s) => s.unavailable, null);
  const closed = useConsoleState(session, (s) => s.closed, null);
  if (connection === "ready" || connection === "connecting") return null;
  if (connection === "starting")
    return (
      <Banner icon={<Spinner className="size-4" label="Starting" />}>
        Starting the playground: it stops when unused and starts again in a few seconds.
      </Banner>
    );
  if (connection === "paused")
    return (
      <Banner icon={<Spinner className="size-4" label="Paused" />}>
        {sentence(reason ?? "The playground is busy.")} The console comes back by itself.
      </Banner>
    );
  if (connection === "stopped")
    return (
      <Banner icon={<PauseIcon className="size-4 text-muted-foreground" />}>
        <span>
          {sentence(reason ?? "The playground was stopped.")} Run a query to start it again, or
        </span>
        <Button size="sm" variant="secondary" className="ml-2" onClick={() => session?.start()}>
          Start it now
        </Button>
      </Banner>
    );
  if (connection === "unavailable")
    return (
      <Banner icon={<WarningTriangleIcon className="size-4 text-warning" />}>
        <span>{sentence(unavailable?.reason ?? "The playground could not start.")}</span>
        <Button size="sm" variant="secondary" className="ml-2" onClick={() => session?.start()}>
          Try again
        </Button>
      </Banner>
    );
  if (connection === "reconnecting")
    return (
      <Banner icon={<WifiOffIcon className="size-4 text-warning" />}>
        Connection lost. Reconnecting…
      </Banner>
    );
  const closedText: Record<NonNullable<ConsoleState["closed"]>, string> = {
    deleted: "This playground was deleted.",
    membership: "Your access to this organization changed. Reload the page.",
    signed_out: "You were signed out. Sign in again to use the console.",
    too_many: "You have this playground open in too many tabs: close one, then reload.",
    unavailable: "The playground host is not answering.",
  };
  return (
    <Banner icon={<WifiOffIcon className="size-4 text-destructive" />}>
      <span>{closed ? closedText[closed] : "The console closed."}</span>
      {closed === "unavailable" && (
        <Button size="sm" variant="secondary" className="ml-2" onClick={() => session?.retry()}>
          Try again
        </Button>
      )}
    </Banner>
  );
}

function sentence(text: string): string {
  const trimmed = text.trim();
  const capital = trimmed.charAt(0).toUpperCase() + trimmed.slice(1);
  return /[.!?]$/.test(capital) ? capital : `${capital}.`;
}

function Banner({ icon, children }: { icon: React.ReactNode; children: React.ReactNode }) {
  return (
    <div
      role="status"
      className="flex shrink-0 items-center gap-2 border-b border-border bg-muted px-3 py-2 text-sm text-foreground"
    >
      {icon}
      <div className="flex flex-wrap items-center">{children}</div>
    </div>
  );
}

/** The session's transaction: idle, open, or failed (ROLLBACK). */
function TxnBadge({ session }: { session: ConsoleSession | null }) {
  const txn = useConsoleState(session, (s) => s.txn, "idle");
  const connection = useConsoleState(session, (s) => s.connection, "connecting");
  if (connection !== "ready") return null;
  if (txn === "in_transaction")
    return (
      <Tooltip content="A transaction is open in this tab: end it with COMMIT or ROLLBACK. Closing the tab rolls it back.">
        <Badge variant="info">In a transaction</Badge>
      </Tooltip>
    );
  if (txn === "failed")
    return (
      <Tooltip content="An error ended the transaction: run ROLLBACK to go on.">
        <Badge variant="destructive">Transaction failed</Badge>
      </Tooltip>
    );
  return (
    <Tooltip content="This console reads: the database refuses writes.">
      <Badge variant="outline">Read-only</Badge>
    </Tooltip>
  );
}

interface ResultsPaneProps {
  tab: string;
  onTab: (tab: string) => void;
  run: Run | null;
  runs: Run[];
  onRun: (id: string) => void;
  connection: ConsoleState["connection"];
  migration: MigrationView | null;
  onQuery: (sql: string) => void;
  onJump: (from: number, to: number) => void;
  /** Where the run's text starts in the editor. */
  offset: number;
}

function ResultsPane({
  tab,
  onTab,
  run,
  connection,
  migration,
  onQuery,
  onJump,
  offset,
}: ResultsPaneProps) {
  const [pick, setPick] = React.useState<{ run: string; index: number } | null>(null);
  const picked = pick && pick.run === run?.id ? pick.index : null;
  const statements = run?.statements ?? [];
  // The last statement with rows, else the last one.
  const shown =
    statements.find((s) => s.index === picked) ??
    [...statements].reverse().find((s) => s.columns.length > 0 || s.error) ??
    statements[statements.length - 1] ??
    null;
  const messages = statements.reduce((n, s) => n + s.notices.length, 0);

  return (
    <Tabs
      value={tab === "migration" && !migration ? "results" : tab}
      onValueChange={onTab}
      className="flex min-h-0 flex-1 flex-col"
    >
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-2">
        <TabsList>
          <TabsTrigger value="results">Results</TabsTrigger>
          <TabsTrigger value="plan">Plan</TabsTrigger>
          <TabsTrigger value="messages">
            Messages
            {messages > 0 && (
              <span className="ml-1 text-xs text-muted-foreground tabular-nums">{messages}</span>
            )}
          </TabsTrigger>
          {migration && <TabsTrigger value="migration">Migration</TabsTrigger>}
        </TabsList>
        {tab !== "migration" && <RunMeta run={run} statement={shown} />}
      </div>
      {statements.length > 1 && tab === "results" && (
        <div className="flex shrink-0 gap-1 overflow-x-auto border-b border-border px-2 py-1.5">
          {statements.map((s) => (
            <Button
              key={s.index}
              size="sm"
              variant={s === shown ? "secondary" : "ghost"}
              onClick={() => run && setPick({ run: run.id, index: s.index })}
              className={cn("font-mono", s.error && "text-destructive")}
            >
              {s.index + 1}. {s.tag ?? (s.error ? "error" : "…")}
            </Button>
          ))}
        </div>
      )}
      <TabsContent value="results" className="mt-0 flex min-h-0 flex-1 flex-col">
        {!run ? (
          <FirstRun connection={connection} />
        ) : (
          <StatementView run={run} statement={shown} onJump={onJump} offset={offset} />
        )}
      </TabsContent>
      <TabsContent value="plan" className="mt-0 min-h-0 flex-1 overflow-auto">
        <PlanView run={run} />
      </TabsContent>
      <TabsContent value="messages" className="mt-0 min-h-0 flex-1 overflow-auto p-3">
        <MessagesView run={run} />
      </TabsContent>
      {migration && (
        <TabsContent value="migration" className="mt-0 min-h-0 flex-1 overflow-auto p-3">
          {migration.error ? (
            <Alert variant="destructive" title="The migration did not run">
              {migration.error}
            </Alert>
          ) : (
            <div className="grid gap-2">
              {migration.by && migration.report && (
                <p className="text-sm text-muted-foreground">Run by {migration.by}</p>
              )}
              <MigrationReportView
                report={migration.report}
                live={migration.live}
                onQuery={onQuery}
              />
            </div>
          )}
        </TabsContent>
      )}
    </Tabs>
  );
}

/** "212 rows · 38 ms", or what it is doing. */
function RunMeta({ run, statement }: { run: Run | null; statement: StatementResult | null }) {
  if (!run) return null;
  if (run.status === null) {
    return (
      <span className="ml-auto flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner className="size-3.5" label="Running" />
        {run.queued
          ? "Waiting for a free slot…"
          : run.statements.length > 1
            ? `Statement ${run.statements.length}…`
            : "Running…"}
      </span>
    );
  }
  if (!statement || statement.rowCount === null) return null;
  // An explained statement's rows are its plan's lines: say how long it took.
  if (run.explain)
    return statement.ms === null ? null : (
      <span className="ml-auto truncate text-sm text-muted-foreground tabular-nums">
        {duration(statement.ms)}
      </span>
    );
  return (
    <span className="ml-auto truncate text-sm text-muted-foreground tabular-nums">
      {formatCount(statement.rowCount)}
      {statement.truncated ? "+" : ""} {statement.rowCount === 1 ? "row" : "rows"}
      {statement.ms !== null && ` · ${duration(statement.ms)}`}
    </span>
  );
}

function FirstRun({ connection }: { connection: ConsoleState["connection"] }) {
  if (connection === "connecting")
    return (
      <div className="grid gap-2 p-4">
        <Skeleton className="h-6 w-full" />
        <Skeleton className="h-6 w-full" />
        <Skeleton className="h-6 w-3/4" />
      </div>
    );
  return (
    <EmptyState
      icon={<TerminalIcon />}
      title="Query the playground"
      description={
        <>
          Pick a table to see its rows, or write SQL above.{" "}
          <span className="whitespace-nowrap">
            <KbdGroup keys={[modKey, "↵"]} /> runs it,
          </span>{" "}
          <span className="whitespace-nowrap">
            <KbdGroup keys={["⇧", modKey, "↵"]} /> shows its plan.
          </span>
        </>
      }
    />
  );
}

function StatementView({
  run,
  statement,
  onJump,
  offset,
}: {
  run: Run;
  statement: StatementResult | null;
  onJump: (from: number, to: number) => void;
  offset: number;
}) {
  if (run.error)
    return (
      <div className="p-3">
        <Alert variant="warning" title="Nothing ran">
          {run.error}
        </Alert>
      </div>
    );
  if (!statement) {
    return run.status === null ? (
      <div className="grid gap-2 p-4" aria-busy>
        <Skeleton className="h-6 w-full" />
        <Skeleton className="h-6 w-5/6" />
      </div>
    ) : (
      <p className="p-4 text-sm text-muted-foreground">The text holds no statement to run.</p>
    );
  }
  if (statement.error) {
    const span = statement.error.span;
    return (
      <div className="p-3">
        <Alert
          variant="destructive"
          title={statement.error.message}
          action={
            span ? (
              <Button
                size="sm"
                variant="secondary"
                onClick={() => onJump(offset + span.utf16_start, offset + span.utf16_end)}
              >
                Show in the SQL
              </Button>
            ) : undefined
          }
        >
          <div className="grid gap-1">
            {statement.error.detail && <p>{statement.error.detail}</p>}
            {statement.error.hint && <p>Hint: {statement.error.hint}</p>}
            {(statement.error.sqlstate === "42501" || statement.error.sqlstate === "25006") && (
              <p>This console only reads. Developers try changes with Run migration.</p>
            )}
            <p className="font-mono text-xs text-muted-foreground">
              SQLSTATE {statement.error.sqlstate}
            </p>
          </div>
        </Alert>
      </div>
    );
  }
  if (statement.columns.length === 0) {
    return (
      <p className="p-4 text-sm text-muted-foreground">
        {statement.tag ? (
          <>
            <span className="font-mono text-foreground">{statement.tag}</span>
            {statement.ms !== null && ` · ${duration(statement.ms)}`}
          </>
        ) : (
          "Running…"
        )}
      </p>
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {(statement.truncated || statement.cellsCut) && (
        <p className="flex shrink-0 items-center gap-1.5 border-b border-border bg-muted px-3 py-1.5 text-sm text-muted-foreground">
          <InfoCircleIcon className="size-3.5 shrink-0" aria-hidden />
          {statement.truncated
            ? `First ${formatCount(statement.rows.length)} rows shown; the statement was stopped. Narrow it with WHERE or LIMIT.`
            : "Some values are longer than 64 KiB and are cut."}
        </p>
      )}
      {statement.rows.length === 0 && statement.rowCount !== null ? (
        <>
          <ResultsGrid columns={statement.columns} rows={[]} className="shrink-0" />
          <p className="p-4 text-sm text-muted-foreground">No rows.</p>
        </>
      ) : (
        <ResultsGrid columns={statement.columns} rows={statement.rows} className="flex-1" />
      )}
    </div>
  );
}

function PlanView({ run }: { run: Run | null }) {
  const plan = run?.statements.find((s) => s.plan)?.plan;
  if (!run?.explain || (!plan && run.status !== null)) {
    return (
      <EmptyState
        title="No plan yet"
        description={
          <>
            Explain runs the statement under the cursor with EXPLAIN ANALYZE:{" "}
            <KbdGroup keys={["⇧", modKey, "↵"]} />. The rows are generated, so plans are indicative.
          </>
        }
      />
    );
  }
  if (!plan)
    return (
      <div className="flex items-center gap-2 p-4 text-sm text-muted-foreground">
        <Spinner className="size-4" label="Explaining" /> Explaining…
      </div>
    );
  return <pre className="p-3 font-mono text-sm whitespace-pre text-foreground">{plan}</pre>;
}

function MessagesView({ run }: { run: Run | null }) {
  if (!run) return <p className="text-sm text-muted-foreground">Nothing ran yet.</p>;
  return (
    <ol className="grid gap-1.5 font-mono text-sm">
      {run.statements.map((s) => (
        <React.Fragment key={s.index}>
          {s.notices.map((notice, i) => (
            <li key={`${s.index}-n${i}`} className="text-muted-foreground">
              <span className="text-foreground">{notice.severity}</span>: {notice.message}
            </li>
          ))}
          <li className={s.error ? "text-destructive" : "text-foreground"}>
            {s.index + 1}.{" "}
            {s.error ? `ERROR ${s.error.sqlstate}: ${s.error.message}` : (s.tag ?? "…")}
            {s.ms !== null && <span className="text-muted-foreground"> · {duration(s.ms)}</span>}
          </li>
        </React.Fragment>
      ))}
    </ol>
  );
}

/** What the page tells people the playground cannot do, and its lifetime. */
function StatusLine({ playground }: { playground: Playground }) {
  return (
    <footer className="flex h-8 shrink-0 items-center gap-3 overflow-hidden border-t border-border bg-card px-3 text-xs text-muted-foreground">
      <Tooltip content="The rows are generated, so plans are indicative. Functions the schema calls return NULL here, as in compiles.">
        <span className="flex items-center gap-1 truncate">
          <InfoCircleIcon className="size-3.5 shrink-0" aria-hidden />
          Generated rows<span className="max-sm:hidden"> · functions return NULL</span>
        </span>
      </Tooltip>
      <span className="ml-auto shrink-0 whitespace-nowrap">
        Deleted in {timeLeft(playground.expires_at)} unless used
      </span>
    </footer>
  );
}

interface SidePanelProps {
  org: string;
  db: string;
  playground: Playground;
  session: ConsoleSession | null;
  onTable: (schema: string, table: string) => void;
  onRunAgain: (sql: string) => void;
  onMigration: (run: PlaygroundRun) => void;
}

function SidePanel({
  org,
  db,
  playground,
  session,
  onTable,
  onRunAgain,
  onMigration,
}: SidePanelProps) {
  const schemas = React.useMemo(() => {
    const bySchema = new Map<string, Playground["tables"]>();
    for (const table of playground.tables) {
      bySchema.set(table.schema, [...(bySchema.get(table.schema) ?? []), table]);
    }
    return [...bySchema.entries()];
  }, [playground.tables]);
  const single = schemas.length === 1;
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <section aria-labelledby="playground-tables" className="flex max-h-[50%] min-h-0 flex-col">
        <h2
          id="playground-tables"
          className="flex h-10 shrink-0 items-center justify-between px-3 text-sm font-medium text-foreground"
        >
          Tables
          <span className="text-xs font-normal text-muted-foreground">rows</span>
        </h2>
        <div className="min-h-0 overflow-y-auto px-1.5 pb-2">
          {schemas.length === 0 ? (
            <p className="px-1.5 text-sm text-muted-foreground">This schema has no tables.</p>
          ) : (
            schemas.map(([schema, tables]) => (
              <div key={schema}>
                {!single && (
                  <p className="px-1.5 pt-2 pb-1 text-xs text-faint-foreground">{schema}</p>
                )}
                <ul>
                  {tables.map((table) => (
                    <li key={table.table}>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="w-full justify-between font-normal text-foreground"
                        onClick={() => onTable(table.schema, table.table)}
                        title={`SELECT * FROM ${table.schema}.${table.table}`}
                      >
                        <span className="truncate font-mono">{table.table}</span>
                        <span className="text-muted-foreground tabular-nums">
                          {formatCount(table.rows)}
                        </span>
                      </Button>
                    </li>
                  ))}
                </ul>
              </div>
            ))
          )}
          <GeneratorReport playground={playground} />
        </div>
      </section>
      <History
        org={org}
        db={db}
        playground={playground}
        session={session}
        onRunAgain={onRunAgain}
        onMigration={onMigration}
      />
    </div>
  );
}

/** What the generator reported (what it could not satisfy), as it wrote it. */
function GeneratorReport({ playground }: { playground: Playground }) {
  const report = playground.report as {
    lines?: unknown;
    notes?: unknown;
    rows_planned?: unknown;
    rows_inserted?: unknown;
  } | null;
  if (!report) return null;
  const strings = (value: unknown) =>
    Array.isArray(value) ? value.filter((n): n is string => typeof n === "string") : null;
  // romulus-fakedata's sentences, else a plain list of notes.
  const notes = strings(report.lines) ?? strings(report.notes);
  const planned = typeof report.rows_planned === "number" ? report.rows_planned : null;
  const inserted = typeof report.rows_inserted === "number" ? report.rows_inserted : null;
  return (
    <details className="mx-1.5 mt-2 rounded-md border border-border px-2.5 py-2 text-sm">
      <summary className="cursor-pointer text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring">
        How the rows were made
      </summary>
      <div className="mt-2 grid gap-1.5 text-muted-foreground">
        <p>
          {playground.size === "small" ? "Small" : "Medium"},{" "}
          {playground.profile === "edge_cases" ? "edge cases" : "realistic"}, seed{" "}
          <span className="font-mono text-foreground">{playground.seed}</span>
          {playground.generator && /^[1-9]\d*$/.test(playground.generator) && (
            <> · generator v{playground.generator}</>
          )}
        </p>
        {planned !== null && inserted !== null && (
          <p>
            {formatCount(inserted)} of {formatCount(planned)} rows planned went in.
          </p>
        )}
        {notes ? (
          notes.length > 0 ? (
            <ul className="list-disc pl-4">
              {notes.map((note, i) => (
                <li key={i}>{note}</li>
              ))}
            </ul>
          ) : (
            <p>Every row was generated as planned.</p>
          )
        ) : (
          <pre className="max-h-48 overflow-auto font-mono text-xs whitespace-pre-wrap">
            {JSON.stringify(report, null, 2)}
          </pre>
        )}
      </div>
    </details>
  );
}

const KIND_ICON = {
  query: TerminalIcon,
  explain: TerminalIcon,
  migration: DatabaseScriptIcon,
  reset: RefreshDoubleIcon,
} as const;

function History({
  org,
  db,
  playground,
  session,
  onRunAgain,
  onMigration,
}: {
  org: string;
  db: string;
  playground: Playground;
  session: ConsoleSession | null;
  onRunAgain: (sql: string) => void;
  onMigration: (run: PlaygroundRun) => void;
}) {
  const stored = useQuery(runsQuery(org, db, playground.id));
  const live = useConsoleState(session, (s) => s.history, NO_HISTORY);
  const entries = React.useMemo(() => {
    const seen = new Set<string>();
    return [...live, ...(stored.data?.runs ?? [])].filter((run) => {
      if (seen.has(run.id)) return false;
      seen.add(run.id);
      return true;
    });
  }, [live, stored.data]);
  return (
    <section
      aria-labelledby="playground-history"
      className="flex min-h-0 flex-1 flex-col border-t border-border"
    >
      <h2
        id="playground-history"
        className="flex h-10 shrink-0 items-center px-3 text-sm font-medium text-foreground"
      >
        History
        <span className="ml-1.5 text-xs font-normal text-muted-foreground">shared</span>
      </h2>
      <div className="min-h-0 flex-1 overflow-y-auto px-1.5 pb-2">
        {stored.isPending ? (
          <div className="grid gap-2 px-1.5">
            <Skeleton className="h-9 w-full" />
            <Skeleton className="h-9 w-full" />
          </div>
        ) : stored.isError && entries.length === 0 ? (
          <p className="px-1.5 text-sm text-muted-foreground">
            The history could not load.{" "}
            <Button variant="link" size="sm" onClick={() => void stored.refetch()}>
              Retry
            </Button>
          </p>
        ) : entries.length === 0 ? (
          <p className="px-1.5 text-sm text-muted-foreground">
            What anyone runs here shows up for everyone.
          </p>
        ) : (
          <ul className="grid gap-0.5">
            {entries.map((entry) => (
              <HistoryEntry
                key={entry.id}
                entry={entry}
                onRunAgain={onRunAgain}
                onMigration={onMigration}
              />
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}

const NO_HISTORY: PlaygroundRun[] = [];

function HistoryEntry({
  entry,
  onRunAgain,
  onMigration,
}: {
  entry: PlaygroundRun;
  onRunAgain: (sql: string) => void;
  onMigration: (run: PlaygroundRun) => void;
}) {
  const Icon = KIND_ICON[entry.kind];
  const failed = entry.status !== "ok";
  const title =
    entry.kind === "reset"
      ? "Reset the rows"
      : entry.kind === "migration"
        ? entry.report
          ? `Migration: ${verdict(entry.report).toLowerCase()}`
          : "Migration did not run"
        : (entry.sql ?? "").replace(/\s+/g, " ").trim();
  const detail =
    entry.kind === "query" || entry.kind === "explain"
      ? failed
        ? entry.status === "cancelled"
          ? "cancelled"
          : "error"
        : entry.kind === "explain"
          ? duration(entry.duration_ms)
          : entry.rows != null
            ? `${formatCount(entry.rows)} ${entry.rows === 1 ? "row" : "rows"}`
            : null
      : null;
  const clickable = entry.kind === "migration" ? Boolean(entry.report) : entry.kind !== "reset";
  const body = (
    <>
      <Icon
        aria-hidden
        className={cn(
          "mt-0.5 size-3.5 shrink-0",
          failed ? "text-destructive" : "text-muted-foreground",
        )}
      />
      <span className="grid min-w-0 flex-1 gap-0.5 text-left">
        <span
          className={cn(
            "truncate text-sm text-foreground",
            (entry.kind === "query" || entry.kind === "explain") && "font-mono",
          )}
        >
          {entry.kind === "explain" && (
            <span className="font-sans text-muted-foreground">Explain </span>
          )}
          {title}
        </span>
        <span className="truncate text-xs text-muted-foreground">
          {entry.by.name} · {timeAgo(entry.started_at)}
          {detail && (
            <>
              {" "}
              · <span className={failed ? "text-destructive" : undefined}>{detail}</span>
            </>
          )}
        </span>
      </span>
    </>
  );
  return (
    <li>
      {clickable ? (
        <Button
          variant="ghost"
          className="h-auto w-full items-start justify-start gap-2 px-1.5 py-1.5 font-normal"
          title={entry.error ?? entry.sql ?? undefined}
          onClick={() =>
            entry.kind === "migration" ? onMigration(entry) : onRunAgain(entry.sql ?? "")
          }
        >
          {body}
        </Button>
      ) : (
        <div className="flex items-start gap-2 px-1.5 py-1.5">{body}</div>
      )}
    </li>
  );
}
