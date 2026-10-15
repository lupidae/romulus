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

import { draftExtensions, jumpTo, showDiagnostics } from "../collab";
import { type Diagnostic, type Draft, draftKeys, draftQuery } from "../queries";
import type { DraftSession, SessionState } from "../session";
import {
  useDraftSession,
  usePeers,
  useSessionState,
  useUndoState,
  useYMap,
  useYText,
} from "../use-session";
import { EditorBar } from "./editor-bar";
import { GroupsMenu, PickBar } from "./groups";
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
  // The server treats this connection as read-only though this page thought
  // otherwise: a role changed. Catch up.
  const readOnly = state?.readOnly;
  const isWriter = canWrite(orgData.role);
  React.useEffect(() => {
    if (readOnly && isWriter) void queryClient.invalidateQueries({ queryKey: orgKeys.all });
  }, [readOnly, isWriter, queryClient]);

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

interface WorkspaceProps {
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

function Workspace({
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
  const [groupsOpen, setGroupsOpen] = React.useState(false);
  const [focusGroup, setFocusGroup] = React.useState<string | null>(null);
  const [publishing, setPublishing] = React.useState(false);

  const status = state?.status ?? "open";
  const canEdit =
    isWriter && draft.status === "open" && status === "open" && state?.readOnly !== true;
  const compile = state?.compile ?? { state: "waiting", stale: true, diagnostics: [], model: null };
  const schema = compile.model ?? emptySchema(db);

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
  const group = () => {
    if (!session || picked.length === 0) return;
    const id = session.createGroup(picked);
    setPicked([]);
    setFocusGroup(id);
    setGroupsOpen(true);
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
        peers={peers}
        meId={meId}
        canUndo={canUndo}
        canRedo={canRedo}
        onUndo={() => session?.undo.undo()}
        onRedo={() => session?.undo.redo()}
        onJump={jump}
        onPublish={() => setPublishing(true)}
        onDeleted={onDeleted}
      />
      <Notice org={org} db={db} draft={draft} state={state} isWriter={isWriter} />
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
            storageKey="romulus:draft-editor"
            graphEmpty={
              <EmptyState
                icon={<TableIcon />}
                title="No tables yet"
                description={
                  canEdit
                    ? "Write CREATE TABLE in the SQL: tables appear here as you type."
                    : "Tables appear here as soon as someone writes them in the SQL."
                }
              />
            }
            graphControls={
              canEdit && (
                <GroupsMenu
                  session={session}
                  groups={groups}
                  picked={picked}
                  onPickedChange={setPicked}
                  open={groupsOpen}
                  onOpenChange={(open) => {
                    setGroupsOpen(open);
                    if (!open) setFocusGroup(null);
                  }}
                  focus={focusGroup}
                />
              )
            }
            graphChildren={
              <>
                <LiveGraph session={session} peers={peers} />
                {canEdit && (
                  <PickBar picked={picked} onGroup={group} onClear={() => setPicked([])} />
                )}
              </>
            }
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
          next={(draft.latest_version ?? 0) + 1}
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

/** One line under the bar when something about the draft needs saying. */
function Notice({
  org,
  db,
  draft,
  state,
  isWriter,
}: {
  org: string;
  db: string;
  draft: Draft;
  state: SessionState | null;
  isWriter: boolean;
}) {
  const strip = "rounded-none border-x-0 border-t-0 px-4 py-2 md:px-5";
  if (state?.status === "published") {
    return (
      <Alert
        variant="success"
        className={strip}
        title={`${state.statusBy ?? "Someone"} published this draft as v${state.publishedVersion}`}
        action={<BackButton org={org} db={db} name={draft.database.name} />}
      >
        It is closed now; its SQL and layout live on as v{state.publishedVersion}.
      </Alert>
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
              Everyone sees your cursor; only developers and admins can edit.
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
        title={`v${draft.latest_version} was published since this draft started`}
      >
        Bringing its changes into this draft comes with the next release of romulus; until then this
        draft cannot be published.
      </Alert>
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
