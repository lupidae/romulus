import type { EditorView } from "@codemirror/view";
import { useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { CheckCircleIcon, GitBranchIcon, TableIcon, TrashIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { toast } from "@/components/ui/toaster";
import { meQuery } from "@/features/auth/queries";
import { orgKeys, orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";
import type { Schema } from "@/features/schema/model";
import { SchemaViewer } from "@/features/schema/viewer/schema-viewer";
import { SchemaViewerSkeleton } from "@/features/schema/viewer/viewer-skeleton";

import { draftExtensions, jumpTo, nextConflict, showDiagnostics } from "../collab";
import { findConflicts } from "../conflicts";
import { useUpdateDraft } from "../mutations";
import { type Diagnostic, type Draft, draftKeys, draftQuery } from "../queries";
import { names, resetApprovals } from "../review";
import { type DraftSession, mayEdit, type SessionState } from "../session";
import { useGraphEdits } from "../use-graph-edits";
import {
  useDraftSession,
  usePeers,
  useSessionState,
  useUndoState,
  useYMap,
  useYText,
} from "../use-session";
import { EditorBar } from "./editor-bar";
import { FirstTable } from "./first-table";
import { LiveGraph } from "./live-graph";
import { PublishDialog } from "./publish-dialog";

const emptySchema = (database: string): Schema => ({
  remus_version: "0.2",
  database,
  server_version: "",
  entities: [],
  enums: [],
  domains: [],
  composites: [],
  partitions: [],
  extensions: [],
});

const noLayoutChange = () => {};

const WAITING: SessionState["compile"] = {
  state: "waiting",
  stale: true,
  diagnostics: [],
  model: null,
};

/**
 * A draft, live: SQL and graph side by side, everyone's cursors, compile on
 * type, groups, undo, publish. The screen the product is about.
 */
export function DraftEditor({ org, db, draftId }: { org: string; db: string; draftId: string }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { data: me } = useSuspenseQuery(meQuery);
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const { data: draft } = useSuspenseQuery(draftQuery(org, db, draftId));
  const [gone, setGone] = React.useState(false);

  const session = useDraftSession(org, draftId, {
    // The socket would not open: ask the API why (deleted? published? signed out?).
    onConnectionFailed: () => {
      queryClient.fetchQuery({ ...draftQuery(org, db, draftId), staleTime: 0 }).catch((error) => {
        if (error instanceof ApiError && error.status === 404) setGone(true);
      });
    },
  });
  const state = useSessionState(session, (s) => s, null);

  // What the live session tells about the draft goes into its query.
  const liveName = state?.name;
  React.useEffect(() => {
    if (liveName)
      queryClient.setQueryData<Draft>(draftKeys.one(org, db, draftId), (d) =>
        d ? { ...d, name: liveName } : d,
      );
  }, [liveName, queryClient, org, db, draftId]);
  const versionCreated = state?.versionCreated;
  React.useEffect(() => {
    if (versionCreated)
      void queryClient.invalidateQueries({ queryKey: draftKeys.one(org, db, draftId) });
  }, [versionCreated, queryClient, org, db, draftId]);
  // An admin removed or restored a version: maybe this draft's base.
  const versionsChanged = state?.versionsChanged ?? null;
  React.useEffect(() => {
    if (!versionsChanged) return;
    // The base as it was when it happened (before the refetch moves it).
    const base = queryClient.getQueryData<Draft>(draftKeys.one(org, db, draftId))?.base_version;
    void queryClient.invalidateQueries({ queryKey: draftKeys.one(org, db, draftId) });
    void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, draftId) });
    const { kind, number, by } = versionsChanged;
    if (kind === "removed" && number === base)
      toast.warning(`${by} removed v${number}`, {
        description: "This draft started from it: update the draft before publishing.",
      });
    else toast.info(`${by} ${kind} v${number}`);
  }, [versionsChanged, queryClient, org, db, draftId]);
  // An edit reset approvals: everyone hears it (the editor first of all).
  const review = state?.review ?? null;
  const previousReview = React.useRef(review);
  React.useEffect(() => {
    const before = previousReview.current;
    previousReview.current = review;
    if (!review) return;
    const reset = resetApprovals(before, review);
    if (reset.length > 0)
      toast.info(reset.length === 1 ? "An approval was reset" : "Approvals were reset", {
        description: `${names(reset)} approved an earlier version: the SQL changed since.`,
      });
  }, [review]);
  // Someone updated the draft to a newer version: its base moved.
  const updated = state?.updated ?? null;
  React.useEffect(() => {
    if (!updated) return;
    void queryClient.invalidateQueries({ queryKey: draftKeys.one(org, db, draftId) });
    void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, draftId) });
    // Whoever clicked hears it from their own request.
    if (updated.by !== me.name)
      toast.info(`${updated.by} updated the draft to v${updated.version}`, {
        description: conflictsSentence(updated.conflicts),
      });
  }, [updated, queryClient, org, db, draftId, me.name]);
  // The server sees another role than this page thought: a role changed.
  // Catch up.
  const serverRole = state?.role ?? null;
  const isWriter = canWrite(orgData.role);
  React.useEffect(() => {
    if (serverRole && canWrite(serverRole) !== isWriter)
      void queryClient.invalidateQueries({ queryKey: orgKeys.all });
  }, [serverRole, isWriter, queryClient]);
  // The session ended (signed out elsewhere, expired): sign in and come back.
  const signedOut = state?.signedOut ?? false;
  React.useEffect(() => {
    if (!signedOut) return;
    const { pathname, search } = window.location;
    queryClient.removeQueries({ queryKey: meQuery.queryKey });
    void navigate({
      to: "/signin",
      search: { next: `${pathname}${search}`, error: "session_expired" },
    });
  }, [signedOut, queryClient, navigate]);

  if (gone || state?.deleted) {
    return (
      <Ended org={org} db={db} draft={draft}>
        <EmptyState
          icon={<TrashIcon />}
          title="This draft was deleted"
          description={
            state?.statusBy
              ? `${state.statusBy} deleted it. Published versions are not affected.`
              : "It is no longer available. Published versions are not affected."
          }
          action={<BackButton org={org} db={db} name={draft.database.name} />}
        />
      </Ended>
    );
  }
  if (draft.status === "published" && state?.status !== "published") {
    return (
      <Ended org={org} db={db} draft={draft}>
        <EmptyState
          icon={<CheckCircleIcon />}
          title={`Published as v${draft.published_version}`}
          description={`“${draft.name}” became v${draft.published_version} of ${draft.database.name}. Its SQL and layout live on there.`}
          action={<BackButton org={org} db={db} name={draft.database.name} primary />}
        />
      </Ended>
    );
  }

  return (
    <Workspace
      key={session?.key ?? 0}
      org={org}
      db={db}
      draft={draft}
      meId={me.id}
      isWriter={isWriter}
      session={session}
      state={state}
      onPublished={(number) => {
        toast.success(`Published v${number}`, {
          description: `${draft.database.name} is at v${number}. The draft is closed.`,
        });
        void navigate({ to: "/$org/$db", params: { org, db } });
      }}
      onDeleted={() => {
        toast.success("Draft deleted");
        void navigate({ to: "/$org/$db", params: { org, db } });
      }}
    />
  );
}

export interface WorkspaceProps {
  org: string;
  db: string;
  draft: Draft;
  meId: string;
  isWriter: boolean;
  session: DraftSession | null;
  state: SessionState | null;
  onPublished: (number: number) => void;
  onDeleted: () => void;
}

export function Workspace({
  org,
  db,
  draft,
  meId,
  isWriter,
  session,
  state,
  onPublished,
  onDeleted,
}: WorkspaceProps) {
  const peers = usePeers(session);
  const layout = useYMap(session?.layout);
  const groups = useYMap(session?.groups);
  const sql = useYText(session?.sql);
  const { canUndo, canRedo } = useUndoState(session);
  const [view, setView] = React.useState<EditorView | null>(null);
  const [selected, setSelected] = React.useState<string | null>(null);
  const [picked, setPicked] = React.useState<string[]>([]);
  const [publishing, setPublishing] = React.useState(false);
  const conflicts = React.useMemo(() => findConflicts(sql), [sql]);
  const update = useUpdateDraft(org, db, draft.id);

  const status = state?.status ?? "open";
  const canEdit =
    isWriter &&
    draft.status === "open" &&
    !(state?.tooLarge ?? false) &&
    (state ? state.role === null || mayEdit(state) : true) &&
    status === "open";
  const compile = state?.compile ?? WAITING;
  // The compiled model, with this tab's schema edits in flight (and an
  // editor to make them, for those who may).
  const { schema: shown, editor } = useGraphEdits({
    session,
    compile,
    edits: state?.edits,
    sql,
    view,
    canEdit,
  });
  const schema = shown ?? emptySchema(db);

  const sqlExtensions = React.useMemo(
    () => (session ? draftExtensions(session, setView) : undefined),
    [session],
  );

  React.useEffect(() => {
    if (view) showDiagnostics(view, compile.diagnostics);
  }, [view, compile.diagnostics]);

  // Undo and redo wherever the focus is, as one history: CodeMirror handles
  // them itself when it has the focus.
  React.useEffect(() => {
    if (!session || !canEdit) return;
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key !== "z" && key !== "y") return;
      const target = event.target as HTMLElement | null;
      if (target?.closest(".cm-editor, input, textarea, [contenteditable=true], [role=dialog]"))
        return;
      event.preventDefault();
      if (key === "y" || event.shiftKey) session.undo.redo();
      else session.undo.undo();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [session, canEdit]);

  // Someone rewrote the SQL (Tidy): this tab's undo history went with it.
  // Whoever asked was told by Tidy itself.
  const historyCleared = state?.historyCleared;
  React.useEffect(() => {
    if (!historyCleared || historyCleared.mine) return;
    toast(
      historyCleared.reason === "tidy"
        ? "Undo history was cleared by Tidy"
        : historyCleared.reason === "update"
          ? "Undo history was cleared by Update draft"
          : "Undo history was cleared",
      { description: "The SQL was rewritten for everyone: earlier edits can no longer be undone." },
    );
  }, [historyCleared]);

  // Leaving with edits only this tab has would lose them.
  const unsynced = state?.unsynced ?? false;
  React.useEffect(() => {
    if (!unsynced) return;
    const onLeave = (event: BeforeUnloadEvent) => event.preventDefault();
    window.addEventListener("beforeunload", onLeave);
    return () => window.removeEventListener("beforeunload", onLeave);
  }, [unsynced]);

  const select = (key: string | null) => {
    setSelected(key);
    session?.setSelection(key);
  };
  const jump = (diagnostic: Diagnostic) => {
    if (view) jumpTo(view, diagnostic);
  };
  const toNextConflict = () => {
    if (view) nextConflict(view);
  };
  const runUpdate = () => {
    // This tab asked for the rewrite: its undo history goes without a word.
    session?.expectRewrite();
    update.mutate(undefined, {
      onSuccess: (result) => {
        if (result.conflicts > 0) {
          toast.warning(`Updated to v${result.base_version}`, {
            description: conflictsSentence(result.conflicts),
          });
          requestAnimationFrame(toNextConflict);
        } else {
          toast.success(`Updated to v${result.base_version}`, {
            description: result.changed
              ? "Everything merged. Publishing is open again."
              : "It already had everything.",
          });
        }
      },
      onError: (error) => {
        const diagnostics =
          error instanceof ApiError && error.code === "draft_has_errors"
            ? ((error.details as { diagnostics?: Diagnostic[] } | undefined)?.diagnostics ?? [])
            : [];
        session?.expectRewrite(false);
        toast.error("Could not update the draft", {
          description: error instanceof ApiError ? error.message : "Check your connection.",
        });
        const first = diagnostics[0];
        if (first) requestAnimationFrame(() => jump(first));
      },
    });
  };

  return (
    <div className="flex h-dvh flex-col bg-background">
      <EditorBar
        org={org}
        db={db}
        draft={draft}
        canEdit={canEdit}
        isWriter={isWriter}
        compile={compile}
        // A closed draft disconnects on purpose: nothing to report.
        connection={status === "open" ? (state?.connection ?? "connecting") : "connected"}
        synced={state?.synced ?? false}
        unsynced={unsynced}
        peers={peers}
        meId={meId}
        canUndo={canUndo}
        canRedo={canRedo}
        onUndo={() => session?.undo.undo()}
        onRedo={() => session?.undo.redo()}
        sql={sql}
        onRewrite={(expected) => session?.expectRewrite(expected)}
        onJump={jump}
        onPublish={() => setPublishing(true)}
        conflicts={conflicts.length}
        onNextConflict={toNextConflict}
        onUpdate={runUpdate}
        updating={update.isPending}
        onDeleted={onDeleted}
        review={state?.review ?? null}
        role={state?.role ?? null}
      />
      <Notice
        org={org}
        db={db}
        draft={draft}
        state={state}
        isWriter={isWriter}
        conflicts={conflicts.length}
        conflictsWith={conflicts[0]?.label ?? null}
        onNextConflict={toNextConflict}
      />
      <main className="min-h-0 flex-1">
        {session && state?.synced ? (
          <SchemaViewer
            schema={schema}
            texts={{
              sql,
              json: compile.model ? JSON.stringify(compile.model, null, 2) : undefined,
            }}
            layout={layout}
            groups={groups}
            // Reviewers write nothing, but place new tables exactly as editors
            // do (the persisted first layout's shape).
            onLayoutChange={
              canEdit ? (positions) => session.setPositions(positions) : noLayoutChange
            }
            editable={canEdit}
            readOnly={!canEdit}
            sqlExtensions={sqlExtensions}
            selected={selected}
            onSelectedChange={select}
            picked={canEdit ? picked : undefined}
            onPickedChange={canEdit ? setPicked : undefined}
            editor={editor}
            storageKey="romulus:draft-editor"
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
                <EmptyState
                  icon={<TableIcon />}
                  title="No tables yet"
                  description="Tables appear here as soon as someone writes them in the SQL."
                />
              )
            }
            graphChildren={<LiveGraph session={session} peers={peers} />}
          />
        ) : (
          <SchemaViewerSkeleton />
        )}
      </main>
      {isWriter && (
        <PublishDialog
          org={org}
          db={db}
          draftId={draft.id}
          databaseName={draft.database.name}
          layout={layout}
          groups={groups}
          next={draft.next_version}
          open={publishing}
          onOpenChange={setPublishing}
          onJump={(d) => requestAnimationFrame(() => jump(d))}
          onPublished={(number) => {
            setPublishing(false);
            onPublished(number);
          }}
        />
      )}
    </div>
  );
}

function conflictsSentence(conflicts: number) {
  if (conflicts === 0) return "Nothing conflicted.";
  return `${conflicts === 1 ? "One conflict" : `${conflicts} conflicts`} to resolve in the SQL before publishing.`;
}

/** One line under the bar when something about the draft needs saying. */
function Notice({
  org,
  db,
  draft,
  state,
  isWriter,
  conflicts,
  conflictsWith,
  onNextConflict,
}: {
  org: string;
  db: string;
  draft: Draft;
  state: SessionState | null;
  isWriter: boolean;
  conflicts: number;
  conflictsWith: string | null;
  onNextConflict: () => void;
}) {
  const strip = "rounded-none border-x-0 border-t-0 px-4 py-2 md:px-5";
  if (state?.tooLarge) {
    return (
      <Alert
        variant="destructive"
        className={strip}
        title="This draft reached its size limit"
        action={
          <Button size="sm" onClick={() => window.location.reload()}>
            Reload
          </Button>
        }
      >
        Your last change was too large to keep and was not saved. Reload to continue from what
        everyone has.
      </Alert>
    );
  }
  if (state?.saveFailed) {
    return (
      <Alert variant="destructive" className={strip} title="Some edits could not be saved">
        They are only in the tabs that have this draft open. Copy your SQL somewhere safe before you
        reload.
      </Alert>
    );
  }
  if (state?.status === "published") {
    return (
      <Alert
        variant="success"
        className={strip}
        title={`${state.statusBy ?? "Someone"} published this draft as v${state.publishedVersion}`}
        action={<BackButton org={org} db={db} name={draft.database.name} />}
      >
        This draft is closed.
      </Alert>
    );
  }
  if (state?.status === "updating") {
    return (
      <Alert
        variant="info"
        className={strip}
        title={
          <>
            {state.statusBy ?? "Someone"} is updating this draft to v{draft.latest_version}.{" "}
            <span className="font-normal text-muted-foreground">
              Edits are paused for a moment.
            </span>
          </>
        }
      />
    );
  }
  if (conflicts > 0) {
    return (
      <Alert
        variant="warning"
        className={strip}
        title={
          <>
            {conflicts === 1 ? "One conflict" : `${conflicts} conflicts`}
            {conflictsWith ? ` with ${conflictsWith}` : ""}.{" "}
            <span className="font-normal text-muted-foreground">
              {isWriter
                ? "Keep yours or theirs on each block in the SQL. Publishing waits until none is left."
                : "A developer or admin resolves them before publishing."}
            </span>
          </>
        }
        action={
          <Button size="sm" onClick={onNextConflict}>
            Next conflict
          </Button>
        }
      />
    );
  }
  if (state?.status === "publishing") {
    return (
      <Alert
        variant="info"
        className={strip}
        title={
          <>
            {state.statusBy ?? "Someone"} is publishing this draft.{" "}
            <span className="font-normal text-muted-foreground">
              Edits are paused until it is done.
            </span>
          </>
        }
      />
    );
  }
  if (!isWriter) {
    return (
      <Alert
        variant="info"
        className={strip}
        title={
          <>
            You’re viewing as a reviewer.{" "}
            <span className="font-normal text-muted-foreground">
              {state?.review && state.review.required > 0
                ? "Everyone sees your cursor. Approve it when it looks right; only developers and admins can edit."
                : "Everyone sees your cursor; only developers and admins can edit."}
            </span>
          </>
        }
      />
    );
  }
  if (draft.stale && draft.base_removed) {
    return (
      <Alert
        variant="warning"
        className={strip}
        title={
          <>
            v{draft.base_version}, which this draft started from, was removed.{" "}
            <span className="font-normal text-muted-foreground">
              {draft.latest_version
                ? `Update the draft to move it onto v${draft.latest_version}, then publish.`
                : "Update the draft to keep its SQL as the database's next first version."}
            </span>
          </>
        }
      />
    );
  }
  if (draft.stale) {
    return (
      <Alert
        variant="warning"
        className={strip}
        title={
          <>
            v{draft.latest_version} was published after this draft started.{" "}
            <span className="font-normal text-muted-foreground">
              Update the draft to bring v{draft.latest_version} in, then publish.
            </span>
          </>
        }
      />
    );
  }
  return null;
}

function Ended({
  org,
  db,
  draft,
  children,
}: {
  org: string;
  db: string;
  draft: Draft;
  children: React.ReactNode;
}) {
  return (
    <div className="flex h-dvh flex-col bg-background">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b border-border bg-card px-3">
        <GitBranchIcon aria-hidden className="size-4 text-muted-foreground" />
        <Link
          to="/$org/$db"
          params={{ org, db }}
          className="truncate rounded-md px-1 text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
        >
          {draft.database.name}
        </Link>
        <span aria-hidden className="text-faint-foreground">
          /
        </span>
        <h1 className="truncate text-sm font-semibold text-foreground">{draft.name}</h1>
      </header>
      <main className="flex flex-1 items-center justify-center pb-16">{children}</main>
    </div>
  );
}

function BackButton({
  org,
  db,
  name,
  primary = false,
}: {
  org: string;
  db: string;
  name: string;
  primary?: boolean;
}) {
  return (
    <Button variant={primary ? "primary" : "secondary"} size="sm" asChild>
      <Link to="/$org/$db" params={{ org, db }}>
        Back to {name}
      </Link>
    </Button>
  );
}
