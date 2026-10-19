// The homepage's live hero: romulus's own draft editor, graph, diff,
// migration and comments, on a draft held in this tab. The SQL is checked by
// Postgres's parser in the browser (./engine), versions and the discussion
// stay in this browser (./store). The app's server is never called.

import type { EditorView } from "@codemirror/view";
import { queryOptions } from "@tanstack/react-query";
import * as React from "react";

import {
  CheckCircleSolidIcon,
  CircleIcon,
  CodeIcon,
  GitBranchIcon,
  GitCompareIcon,
  PageIcon,
  RefreshDoubleIcon,
  TableIcon,
  WarningTriangleIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip } from "@/components/ui/tooltip";
import { CommentModeButton, CommentPins } from "@/features/comments/components/comment-pins";
import { CommentsAside, CommentsToggle } from "@/features/comments/components/comments-layer";
import { CommentsPanel } from "@/features/comments/components/comments-panel";
import { useDraftDiscussion } from "@/features/comments/discussion";
import { useComments } from "@/features/comments/use-comments";
import { useCommentsUi } from "@/features/comments/use-comments-ui";
import { SchemaDiffView } from "@/features/diff/components/schema-diff-view";
import { draftExtensions, jumpTo, showDiagnostics } from "@/features/draft/collab";
import { CompileStatus } from "@/features/draft/components/compile-status";
import { FirstTable } from "@/features/draft/components/first-table";
import type { Diagnostic } from "@/features/draft/queries";
import { DraftSession, LOCAL, type SessionState } from "@/features/draft/session";
import { useGraphEdits } from "@/features/draft/use-graph-edits";
import { useSessionState, useYMap, useYText } from "@/features/draft/use-session";
import { MigrationView } from "@/features/migration/components/migration-view";
import type {
  MigrationChoice,
  MigrationQuery,
  SchemaMigration,
} from "@/features/migration/queries";
import type { Schema } from "@/features/schema/model";
import { SchemaViewer } from "@/features/schema/viewer/schema-viewer";
import { useMediaQuery } from "@/features/schema/viewer/use-preference";
import { cn } from "@/lib/utils";

import { revealInPage } from "./contain-scroll";
import { type Compared, DATABASE, type Engine } from "./engine";
import { ME, PEOPLE } from "./seed";
import * as store from "./store";
import { toast, Toaster } from "./toaster";

type View = "draft" | "changes" | "migration";
type EditorAction = "column" | "comment";
type MobileTab = "sql" | "graph" | "changes" | "migration";

interface Version extends store.SavedVersion {
  model: Schema;
}

const empty = (): Schema => ({
  remus_version: "0.1",
  database: DATABASE,
  server_version: "17.7",
  entities: [],
  enums: [],
  domains: [],
  composites: [],
  partitions: [],
  extensions: [],
});

const WAITING: SessionState["compile"] = {
  state: "waiting",
  stale: true,
  diagnostics: [],
  model: null,
  sha256: null,
};

/** The hero: a draft that starts from what this browser kept, or the seed. */
export function HeroApp({ engine }: { engine: Engine }) {
  // A reset starts a new workspace: a new session, the seed again.
  const [generation, setGeneration] = React.useState(0);
  const reset = React.useCallback(() => {
    store.clear();
    setGeneration((g) => g + 1);
    toast("Back to v1", {
      description: "The demo starts over: your edits, versions and comments are gone.",
    });
  }, []);
  return <Workspace key={generation} engine={engine} onReset={reset} />;
}

function Workspace({ engine, onReset }: { engine: Engine; onReset: () => void }) {
  const [initial] = React.useState(
    () => store.load() ?? store.seed(!window.matchMedia("(min-width: 48rem)").matches),
  );
  const [versions, setVersions] = React.useState<Version[]>(() =>
    initial.versions.map((v) => ({ ...v, model: engine.compile(v.sql).model ?? empty() })),
  );
  // Never empty: v1 is the seed.
  const latest = versions.at(-1) as Version;

  const [session, setSession] = React.useState<DraftSession | null>(null);
  React.useEffect(() => {
    const created = new DraftSession("acme", "hero", {
      local: {
        sql: initial.sql,
        layout: initial.layout,
        groups: initial.groups,
        threads: initial.threads,
        comments: initial.comments,
        role: "developer",
        compile: (sql) => engine.compile(sql),
      },
    });
    // The session owns a doc and timers: made here, then handed to render.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSession(created);
    return () => {
      created.destroy();
      setSession(null);
    };
  }, [engine, initial]);

  const state = useSessionState(session, (s) => s, null);
  const compile = state?.compile ?? WAITING;
  const sql = useYText(session?.sql);
  const layout = useYMap(session?.layout);
  const groups = useYMap(session?.groups);

  // Kept in this browser as it changes.
  const versionsRef = React.useRef(versions);
  React.useEffect(() => {
    versionsRef.current = versions;
  }, [versions]);
  React.useEffect(() => {
    if (!session) return;
    let timer = 0;
    const persist = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        store.save({
          sql: session.sql.toJSON(),
          layout: session.layout.toJSON(),
          groups: session.groups.toJSON(),
          threads: session.threads.toJSON(),
          comments: session.comments.toJSON(),
          versions: versionsRef.current.map(({ model: _, ...v }) => v),
        });
      }, 400);
    };
    session.doc.on("update", persist);
    return () => {
      window.clearTimeout(timer);
      session.doc.off("update", persist);
    };
  }, [session]);

  const wide = useMediaQuery("(min-width: 48rem)");
  const [view, setView] = React.useState<View>("draft");
  const [pane, setPane] = React.useState<"graph" | "text">("graph");
  const [sqlView, setSqlView] = React.useState<EditorView | null>(null);
  const [selected, setSelected] = React.useState<string | null>(null);
  const [picked, setPicked] = React.useState<string[]>([]);
  const [visited, setVisited] = React.useState<{ changes: boolean }>({ changes: false });

  const { schema: shown, editor } = useGraphEdits({
    session,
    compile,
    edits: state?.edits,
    sql,
    view: sqlView,
    canEdit: true,
  });
  const schema = shown ?? empty();
  // JSON, DBML and Mermaid of what compiled, as remus emits them.
  const texts = React.useMemo(
    () => (compile.model ? engine.texts(compile.model) : undefined),
    [engine, compile.model],
  );

  const commentsUi = useCommentsUi();
  const discussion = useDraftDiscussion({
    session,
    org: "acme",
    db: DATABASE,
    draftId: "hero",
    me: ME,
    canComment: true,
    canModerate: true,
    local: true,
    people: PEOPLE,
  });
  const comments = useComments({
    discussion,
    ui: commentsUi,
    schema: shown,
    sql,
    selected,
    onSelect: (key) => select(key),
    sqlView,
    commentLine: (range, body, anchor) => {
      session?.commentFromLine(range, body, anchor, ME);
      return !!session;
    },
    pasteComments: (range, insert, threads) => {
      session?.pasteWithComments(range, insert, threads, ME);
      return !!session;
    },
  });
  const sqlExtensions = React.useMemo(
    () => (session ? draftExtensions(session, setSqlView, session.sql, comments.feed) : undefined),
    [session, comments.feed],
  );
  React.useEffect(() => {
    if (sqlView) showDiagnostics(sqlView, compile.diagnostics);
  }, [sqlView, compile.diagnostics]);

  const select = (key: string | null) => {
    setSelected(key);
    session?.setSelection(key);
  };
  const jump = (d: Diagnostic) => {
    showView("draft");
    setPane("text");
    requestAnimationFrame(() => {
      if (sqlView) jumpTo(sqlView, d);
    });
  };

  // Undo and redo outside the editor (graph edits), in the island only.
  const root = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => {
    if (!session) return;
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key !== "z" && key !== "y") return;
      const target = event.target as HTMLElement | null;
      if (!target || !root.current?.contains(target)) return;
      if (target.closest(".cm-editor, input, textarea, [contenteditable=true], [role=dialog]"))
        return;
      event.preventDefault();
      if (key === "y" || event.shiftKey) session.undo.redo();
      else session.undo.undo();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [session]);

  // ---- Versions and changes ----------------------------------------------------

  const [compareFrom, setCompareFrom] = React.useState<number | null>(null);
  const base = versions.find((v) => v.number === compareFrom) ?? latest;
  const draftModel = compile.model;
  const compared = React.useMemo(
    () => (draftModel ? engine.diff(base.model, draftModel) : null),
    [engine, base, draftModel],
  );
  const sinceLatest = React.useMemo(
    () => (draftModel ? engine.diff(latest.model, draftModel) : null),
    [engine, latest, draftModel],
  );
  const changeCount = (diff: Compared["diff"] | undefined) =>
    diff?.summary.counts.reduce((n, c) => n + c.added + c.removed + c.changed, 0) ?? 0;
  const pending = changeCount(sinceLatest?.diff);
  const ok = compile.state === "ok" && !compile.stale;
  const next = latest.number + 1;

  const saveVersion = () => {
    if (!session || !draftModel || !ok || pending === 0) return;
    const version: Version = {
      number: next,
      sql: session.sql.toJSON(),
      layout: session.layout.toJSON(),
      groups: session.groups.toJSON(),
      savedAt: new Date().toISOString(),
      by: ME.name,
      model: draftModel,
    };
    setVersions((list) => [...list, version]);
    setCompareFrom(null);
    // The doc did not change: write the new list now.
    store.save({
      sql: version.sql,
      layout: version.layout,
      groups: version.groups,
      threads: session.threads.toJSON(),
      comments: session.comments.toJSON(),
      versions: [...versions, version].map(({ model: _, ...v }) => v),
    });
    toast.success(`Saved v${next}`, {
      description: "Change the schema now: Changes and Migration compare it with this version.",
    });
  };
  const saveBlocked = !ok
    ? compile.state === "failed"
      ? "Fix the SQL first"
      : "Checking the SQL…"
    : pending === 0
      ? `Nothing changed since v${latest.number}`
      : null;

  // The union's layout: the draft's, then the older version's for what it dropped.
  const diffLayout = React.useMemo(() => ({ ...base.layout, ...layout }), [base, layout]);

  const migrationQuery = React.useCallback(
    (choice: MigrationChoice): MigrationQuery =>
      // The base's number and the draft's text (its SHA-256) are the two models.
      // eslint-disable-next-line @tanstack/query/exhaustive-deps
      queryOptions<SchemaMigration, Error, SchemaMigration, readonly unknown[]>({
        queryKey: ["hero", "migration", base.number, compile.sha256, next, choice],
        queryFn: () =>
          Promise.resolve(
            engine.migration(base.model, draftModel ?? empty(), choice, {
              from: base.number,
              to: next,
              fromLabel: `v${base.number}`,
              toLabel: "draft",
              database: `acme/${DATABASE}`,
            }),
          ),
        staleTime: Infinity,
      }),
    [engine, base, draftModel, compile.sha256, next],
  );

  // ---- The guide -----------------------------------------------------------------

  const edited = versions.length > 1 || pending > 0;
  const compared2 = versions.length > 1 && pending > 0 && visited.changes;
  const commented = (discussion.threads ?? []).some((t) =>
    t.comments.some((c) => c.authorId === ME.id),
  );

  const showView = (v: View) => {
    setView(v);
    if (v !== "draft") setVisited({ changes: true });
  };
  const mobileTab: MobileTab = view !== "draft" ? view : pane === "text" ? "sql" : "graph";
  const comparing = view !== "draft" && versions.length > 1;
  const setMobileTab = (tab: MobileTab) => {
    if (tab === "sql" || tab === "graph") {
      setPane(tab === "sql" ? "text" : "graph");
      showView("draft");
    } else showView(tab);
  };

  // Guide actions that need the SQL editor run once it is there: coming
  // from Changes or Migration, it mounts a render later.
  const [action, setAction] = React.useState<{ kind: EditorAction; n: number } | null>(null);
  const inEditor = (kind: EditorAction) => {
    showView("draft");
    setPane("text");
    setAction((last) => ({ kind, n: (last?.n ?? 0) + 1 }));
  };
  React.useEffect(() => {
    if (!action || !session || !sqlView || view !== "draft") return;
    let frame = requestAnimationFrame(() => {
      setAction(null);
      perform(action.kind, session, sqlView);
      // The editor scrolls to the caret on its next frame; then the page
      // shows that line, wherever the visitor had scrolled.
      frame = requestAnimationFrame(() => {
        const at = sqlView.domAtPos(sqlView.state.selection.main.head).node;
        const line = (at instanceof Element ? at : at.parentElement)?.closest(".cm-line");
        if (line) revealInPage(line);
      });
    });
    return () => cancelAnimationFrame(frame);
  }, [action, session, sqlView, view, pane]);

  const steps: { done: boolean; label: string; short: string }[] = [
    { done: edited, label: "Change the SQL or the graph", short: "Edit SQL" },
    {
      done: compared2,
      label:
        versions.length > 1 ? "Change it again, see the diff" : `Save v${next}, then change it`,
      short: versions.length > 1 ? "See the diff" : `Save v${next}`,
    },
    { done: commented, label: "Comment with a --- line", short: "Comment" },
  ];
  // Each step does what it says, and shows it: the window comes into view
  // (the visitor may have scrolled its top away), the editor's caret too.
  const runStep = (i: number) => {
    if (root.current) revealInPage(root.current);
    if (i === 0) inEditor("column");
    else if (i === 2) inEditor("comment");
    else if (versions.length > 1) showView("changes");
    else if (!saveBlocked) saveVersion();
    else if (pending === 0 && ok) {
      inEditor("column");
      hint.current = toast(`Change something first`, {
        description: `Add a column here, then save v${next}.`,
      });
    } else toast(saveBlocked);
  };
  // That hint is done with once there is something to save.
  const hint = React.useRef<number | null>(null);
  React.useEffect(() => {
    if (pending > 0 && hint.current !== null) {
      toast.dismiss(hint.current);
      hint.current = null;
    }
  }, [pending]);
  const done = steps.filter((step) => step.done).length;

  // ---- Render ---------------------------------------------------------------------

  const viewer = session && state?.synced && (
    <SchemaViewer
      schema={schema}
      texts={{ sql, ...texts }}
      layout={layout}
      groups={groups}
      onLayoutChange={(positions) => session.setPositions(positions)}
      editable
      readOnly={false}
      sqlExtensions={sqlExtensions}
      selected={selected}
      onSelectedChange={select}
      picked={picked}
      onPickedChange={setPicked}
      editor={editor}
      storageKey="romulus:hero-viewer"
      pane={wide ? undefined : pane}
      followNewTables
      graphEmpty={
        editor ? (
          <FirstTable
            onCreate={(name) =>
              editor.onEdit(
                { kind: "addTable", table: { schema: "public", name } },
                { position: { x: 0, y: 0 } },
              )
            }
          />
        ) : (
          <EmptyState icon={<TableIcon />} title="No tables yet" />
        )
      }
      graphChildren={
        <CommentPins
          threads={comments.pinned}
          unread={comments.unread}
          active={commentsUi.open ? commentsUi.active : null}
          composing={commentsUi.open ? commentsUi.composing : null}
          onOpen={commentsUi.openThread}
          commentMode={commentsUi.commentMode}
          onPick={commentsUi.compose}
          onExitCommentMode={() => commentsUi.setCommentMode(false)}
        />
      }
      graphControls={
        <CommentModeButton on={commentsUi.commentMode} onChange={comments.setCommentMode} />
      }
      graphFocus={comments.focus}
    />
  );

  return (
    <div
      ref={root}
      className="relative flex h-full min-h-0 flex-col overflow-hidden rounded-xl bg-card text-base text-foreground shadow-dialog"
    >
      <header className="flex h-11 shrink-0 items-center gap-2 border-b border-border pr-2 pl-3.5">
        <div className="flex min-w-0 flex-1 items-center gap-2 text-sm whitespace-nowrap text-muted-foreground">
          <Mark />
          <span className="max-sm:hidden">acme</span>
          <span aria-hidden className="text-border-strong max-sm:hidden">
            /
          </span>
          <span className="max-sm:hidden">{DATABASE}</span>
          <span aria-hidden className="text-border-strong max-sm:hidden">
            /
          </span>
          <span className="inline-flex min-w-0 items-center gap-1.5 font-medium text-foreground">
            <GitBranchIcon aria-hidden className="size-3.5 shrink-0 text-primary" />
            {/* On a phone, the picker below takes the room when it shows. */}
            <span className={cn("truncate", comparing && "max-sm:hidden")}>
              <span className="max-sm:hidden">Draft from </span>
              <span className="sm:hidden">From </span>v{latest.number}
            </span>
          </span>
          {comparing && (
            <Select value={String(base.number)} onValueChange={(v) => setCompareFrom(Number(v))}>
              <SelectTrigger
                aria-label="Compare the draft with"
                className="ml-1 h-7 w-auto shrink-0 gap-1 px-2 text-sm"
              >
                <span className="text-muted-foreground max-lg:hidden">compared with</span>
                <span className="text-muted-foreground lg:hidden">vs</span>
                <SelectValue />
              </SelectTrigger>
              <SelectContent align="start">
                {[...versions].reverse().map((v) => (
                  <SelectItem key={v.number} value={String(v.number)}>
                    v{v.number}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
        </div>
        {wide && (
          <ToggleGroup
            type="single"
            value={view}
            onValueChange={(v) => v && showView(v as View)}
            aria-label="View"
          >
            <ToggleGroupItem value="draft">
              <PageIcon aria-hidden />
              Draft
            </ToggleGroupItem>
            <ToggleGroupItem value="changes">
              <GitCompareIcon aria-hidden />
              Changes
              {pending > 0 && <Count n={pending} />}
            </ToggleGroupItem>
            <ToggleGroupItem value="migration">
              <CodeIcon aria-hidden />
              Migration
            </ToggleGroupItem>
          </ToggleGroup>
        )}
        <div className="flex flex-1 items-center justify-end gap-1">
          <CompileStatus compile={compile} major={17} onJump={jump} checker="parser" />
          <CommentsToggle
            ui={commentsUi}
            open={comments.pinned.length}
            unread={comments.unreadCount}
          />
          <Tooltip content={saveBlocked ?? `Keep this schema as v${next}`}>
            {/* Not `disabled`: it stays focusable, and its tooltip says why. */}
            <Button
              variant="primary"
              size="sm"
              aria-disabled={saveBlocked ? true : undefined}
              onClick={saveVersion}
              className={cn(
                saveBlocked && "cursor-not-allowed opacity-45",
                !saveBlocked && versions.length === 1 && edited && "hero-pulse",
              )}
            >
              Save v{next}
            </Button>
          </Tooltip>
        </div>
      </header>

      {!wide && (
        <div className="flex h-12 shrink-0 items-center border-b border-border px-2">
          {/* The kit's touch size (36px, as `md` buttons on a phone): these are the hero's tabs here. */}
          <ToggleGroup
            type="single"
            value={mobileTab}
            onValueChange={(v) => v && setMobileTab(v as MobileTab)}
            aria-label="View"
            className="grid h-10 w-full grid-cols-4"
          >
            <ToggleGroupItem value="sql" className="h-9">
              SQL
            </ToggleGroupItem>
            <ToggleGroupItem value="graph" className="h-9">
              Graph
            </ToggleGroupItem>
            <ToggleGroupItem value="changes" className="h-9">
              Changes
              {pending > 0 && <Count n={pending} />}
            </ToggleGroupItem>
            <ToggleGroupItem value="migration" className="h-9">
              Migration
            </ToggleGroupItem>
          </ToggleGroup>
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <main className="relative min-h-0 min-w-0 flex-1">
          {view === "draft" && viewer}
          {view === "changes" &&
            (!draftModel ? (
              <Unavailable
                title="The SQL has errors"
                description="The comparison appears once the SQL checks out."
                onFix={() => jump(compile.diagnostics[0] as Diagnostic)}
                fixable={compile.diagnostics.length > 0}
              />
            ) : !compared ? (
              <Unavailable
                title="The comparison failed"
                description="The browser preview could not compare these versions. Edit the SQL to try again."
              />
            ) : changeCount(compared.diff) > 0 ? (
              <SchemaDiffView
                before={base.model}
                after={draftModel}
                diff={compared.diff}
                sql={compared.sql}
                layout={diffLayout}
                groups={groups}
                labels={{ before: `v${base.number}`, after: "Draft" }}
                shortcuts={false}
                storageKey="romulus:hero-diff"
              />
            ) : (
              <NoChanges version={base.number} onEdit={() => inEditor("column")} />
            ))}
          {view === "migration" && (
            <MigrationView
              query={migrationQuery}
              storageKey="romulus:hero-migration"
              defaultFormat="dbmate"
              blocked={
                !draftModel ? (
                  "The migration appears once the SQL checks out."
                ) : changeCount(compared?.diff) === 0 ? (
                  <NoChanges version={base.number} onEdit={() => inEditor("column")} />
                ) : undefined
              }
              busy={compile.stale}
            />
          )}
        </main>
        <CommentsAside ui={commentsUi} title="Comments">
          <CommentsPanel
            discussion={discussion}
            ui={commentsUi}
            title="Comments"
            outdated={comments.outdated}
            onShow={(t) => {
              showView("draft");
              comments.show(t);
            }}
            onNew={comments.startNew}
          />
        </CommentsAside>
      </div>

      <footer className="flex h-9 shrink-0 items-center gap-1 border-t border-border bg-muted px-2 text-sm">
        <span className="mr-1 shrink-0 font-medium text-muted-foreground max-sm:sr-only">
          Try it
        </span>
        <ol
          className="flex min-w-0 flex-1 items-center gap-0.5"
          aria-label={`Try it: ${done} of 3 done`}
        >
          {steps.map((s, i) => (
            <li key={i} className={cn("min-w-0", !wide && s.done && done < 3 && "hidden")}>
              <Button
                variant="ghost"
                size="sm"
                onClick={() => runStep(i)}
                className={cn(
                  "max-w-full gap-1.5 max-sm:px-1.5",
                  s.done ? "text-muted-foreground" : "text-foreground",
                )}
              >
                {s.done ? (
                  <CheckCircleSolidIcon aria-hidden className="text-success" />
                ) : (
                  <CircleIcon aria-hidden />
                )}
                <span className="truncate">
                  <span className="sr-only">{s.done ? "Done: " : ""}</span>
                  {wide ? s.label : s.short}
                </span>
              </Button>
            </li>
          ))}
        </ol>
        <Tooltip content="Start over from v1">
          <Button variant="ghost" size="sm" onClick={onReset} className="shrink-0">
            <RefreshDoubleIcon aria-hidden />
            <span className="max-sm:sr-only">Reset</span>
          </Button>
        </Tooltip>
      </footer>
      <Toaster />
    </div>
  );
}

/**
 * What a step of the guide does in the SQL: put the caret where a column
 * goes, or start a `--- ` line above `CREATE TABLE customers` (once).
 */
function perform(kind: EditorAction, session: DraftSession, view: EditorView) {
  const text = session.sql.toJSON();
  if (kind === "column") {
    const at = text.indexOf("  created_at timestamptz");
    view.focus();
    if (at >= 0) view.dispatch({ selection: { anchor: at }, scrollIntoView: true });
    return;
  }
  const statement = text.indexOf("CREATE TABLE customers");
  const at = statement < 0 ? 0 : statement;
  let caret = at - 1;
  if (!/--- *\n$/.test(text.slice(0, at))) {
    session.doc.transact(() => session.sql.insert(at, "--- \n"), LOCAL);
    caret = at + 4;
  }
  view.focus();
  view.dispatch({ selection: { anchor: caret }, scrollIntoView: true });
}

function Count({ n }: { n: number }) {
  return (
    <span className="min-w-4 rounded-full bg-primary px-1 text-xs leading-4 font-semibold text-primary-foreground tabular">
      {n}
    </span>
  );
}

function Unavailable({
  title,
  description,
  onFix,
  fixable = false,
}: {
  title: string;
  description: string;
  onFix?: () => void;
  fixable?: boolean;
}) {
  return (
    <div className="flex h-full items-center justify-center overflow-y-auto">
      <EmptyState
        icon={<WarningTriangleIcon />}
        title={title}
        description={description}
        action={
          fixable && onFix ? <Button onClick={onFix}>Show the first error</Button> : undefined
        }
      />
    </div>
  );
}

function NoChanges({ version, onEdit }: { version: number; onEdit: () => void }) {
  return (
    <div className="flex h-full items-center justify-center overflow-y-auto">
      <EmptyState
        icon={<GitCompareIcon />}
        title={`No changes since v${version}`}
        description="Change a column in the SQL, or rename a table on the graph: the diff and the migration that applies it show here."
        action={<Button onClick={onEdit}>Edit the SQL</Button>}
      />
    </div>
  );
}

/** The romulus mark, as the homepage's sprite draws it. */
function Mark() {
  return (
    <svg viewBox="0 0 32 32" aria-hidden className="mr-0.5 size-4.5 shrink-0">
      <rect width="32" height="32" rx="8" className="fill-primary" />
      <path
        d="M11.5 23V13.75A4.75 4.75 0 0 1 16.25 9H17"
        fill="none"
        strokeWidth="2.75"
        strokeLinecap="round"
        className="stroke-primary-foreground"
      />
      <circle cx="21.5" cy="9" r="2.4" className="fill-primary-foreground" />
    </svg>
  );
}
