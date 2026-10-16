import { Link } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import {
  ArrowLeftIcon,
  EditPencilIcon,
  MoreHorizIcon,
  RedoIcon,
  SparksIcon,
  TrashIcon,
  UndoIcon,
  WifiOffIcon,
} from "@/components/icons";
import { AvatarStack } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuSeparator,
  MenuShortcut,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { modKey } from "@/lib/platform";
import { cn } from "@/lib/utils";

import type { Role } from "@/features/orgs/queries";

import { useDeleteDraft, useRenameDraft, useTidyDraft } from "../mutations";
import type { Diagnostic, Draft } from "../queries";
import { type Review, reviewing } from "../review";
import type { CompileView, Connection, Peer } from "../session";
import { CompileStatus } from "./compile-status";
import { ReviewControl } from "./review-control";

interface EditorBarProps {
  org: string;
  db: string;
  draft: Draft;
  /** May edit (and the draft is open). */
  canEdit: boolean;
  /** May publish, rename, delete: a developer or admin. */
  isWriter: boolean;
  compile: CompileView;
  connection: Connection;
  synced: boolean;
  /** Edits made here while disconnected, not on the server yet. */
  unsynced: boolean;
  peers: Peer[];
  meId: string;
  canUndo: boolean;
  canRedo: boolean;
  onUndo: () => void;
  onRedo: () => void;
  /** The SQL now (Tidy says what it would lose). */
  sql: string;
  /** Tidy is being asked for (true), or it failed or changed nothing (false). */
  onRewrite?: (expected: boolean) => void;
  onJump: (diagnostic: Diagnostic) => void;
  onPublish: () => void;
  /** Conflict blocks left in the SQL: publishing waits for none. */
  conflicts: number;
  onNextConflict: () => void;
  onUpdate: () => void;
  updating: boolean;
  onDeleted: () => void;
  /** Approvals, when the organization asks for some (the server's `review`). */
  review: Review | null;
  /** This connection's role, as the server sees it. */
  role: Role | null;
}

/**
 * The editor's one bar: where it is (back to the database, the draft's
 * name), how it is (compiling, connection), who is here, and what to do
 * next (undo, publish).
 */
export function EditorBar(props: EditorBarProps) {
  const { org, db, draft, isWriter, compile, connection, synced, peers, meId } = props;
  const [renaming, setRenaming] = React.useState(false);
  const [deleting, setDeleting] = React.useState(false);
  const [tidying, setTidying] = React.useState(false);
  const remove = useDeleteDraft(org, db, draft.id);
  const tidy = useTidyDraft(org, db, draft.id);
  const hasErrors = compile.state === "failed";
  const open = draft.status === "open";

  return (
    <header className="flex h-12 shrink-0 items-center gap-1 border-b border-border bg-card px-2 md:gap-2 md:px-3">
      <Tooltip content={`Back to ${draft.database.name}`}>
        <Button variant="ghost" size="icon-sm" asChild>
          <Link to="/$org/$db" params={{ org, db }} aria-label={`Back to ${draft.database.name}`}>
            <ArrowLeftIcon />
          </Link>
        </Button>
      </Tooltip>
      <nav aria-label="Breadcrumb" className="flex min-w-0 items-center gap-1 text-sm">
        <Link
          to="/$org/$db"
          params={{ org, db }}
          className="hidden truncate rounded-md px-1.5 py-1 text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring sm:block"
        >
          {draft.database.name}
        </Link>
        <span aria-hidden className="hidden text-faint-foreground sm:block">
          /
        </span>
        <DraftName
          org={org}
          db={db}
          draft={draft}
          editable={isWriter && open}
          editing={renaming}
          onEditingChange={setRenaming}
        />
      </nav>

      <div className="flex min-w-0 shrink items-center">
        {synced && <CompileStatus compile={compile} onJump={props.onJump} />}
        <ConnectionStatus connection={connection} synced={synced} unsynced={props.unsynced} />
      </div>

      <div className="ml-auto flex shrink-0 items-center gap-1 md:gap-1.5">
        <Presence peers={peers} meId={meId} />
        {isWriter && open && (
          <div className="hidden items-center sm:flex">
            <Tooltip content="Undo" shortcut={[modKey, "Z"]}>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Undo"
                disabled={!props.canUndo || !props.canEdit}
                onClick={props.onUndo}
              >
                <UndoIcon />
              </Button>
            </Tooltip>
            <Tooltip content="Redo" shortcut={["⇧", modKey, "Z"]}>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Redo"
                disabled={!props.canRedo || !props.canEdit}
                onClick={props.onRedo}
              >
                <RedoIcon />
              </Button>
            </Tooltip>
          </div>
        )}
        {isWriter && open && (
          <Menu>
            <Tooltip content="More">
              <MenuTrigger asChild>
                <Button variant="ghost" size="icon-sm" aria-label="More actions">
                  <MoreHorizIcon />
                </Button>
              </MenuTrigger>
            </Tooltip>
            <MenuContent align="end">
              <MenuItem onSelect={() => setTidying(true)} disabled={!props.canEdit || hasErrors}>
                <SparksIcon />
                Tidy SQL…
                {hasErrors && <MenuShortcut>Fix errors first</MenuShortcut>}
              </MenuItem>
              <MenuItem onSelect={() => setRenaming(true)}>
                <EditPencilIcon />
                Rename draft
              </MenuItem>
              <MenuSeparator />
              <MenuItem variant="destructive" onSelect={() => setDeleting(true)}>
                <TrashIcon />
                Delete draft…
              </MenuItem>
            </MenuContent>
          </Menu>
        )}
        {reviewing(props.review) && open && (
          <ReviewControl
            org={org}
            db={db}
            draftId={draft.id}
            review={props.review}
            meId={meId}
            role={props.role}
            open={props.canEdit || props.role === "reviewer"}
            sql={props.sql}
          />
        )}
        {isWriter && open && (
          <PublishButton
            draft={draft}
            conflicts={props.conflicts}
            updating={props.updating}
            onPublish={props.onPublish}
            onUpdate={props.onUpdate}
            onNextConflict={props.onNextConflict}
          />
        )}
      </div>

      <ConfirmDialog
        open={tidying}
        onOpenChange={setTidying}
        title="Tidy the SQL?"
        description={<TidyDescription sql={props.sql} />}
        confirmLabel="Tidy SQL"
        onConfirm={async () => {
          try {
            props.onRewrite?.(true);
            const result = await tidy.mutateAsync();
            if (!result.changed) props.onRewrite?.(false);
            toast.success(result.changed ? "SQL tidied" : "Already tidy", {
              description: result.changed
                ? "Everyone has the canonical SQL, the schema the same. Earlier edits can no longer be undone."
                : "The SQL is in canonical order already.",
            });
          } catch (error) {
            props.onRewrite?.(false);
            toast.error(tidyFailure(error), {
              description: error instanceof ApiError ? error.message : "Check your connection.",
            });
            throw error;
          }
        }}
      />
      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title={`Delete “${draft.name}”?`}
        description="Its SQL is deleted for everyone in it. Published versions stay."
        confirmLabel="Delete draft"
        destructive
        onConfirm={async () => {
          try {
            await remove.mutateAsync();
            props.onDeleted();
          } catch (error) {
            toast.error("Could not delete the draft", {
              description: error instanceof ApiError ? error.message : "Check your connection.",
            });
            throw error;
          }
        }}
      />
    </header>
  );
}

/** What Tidy keeps and what it loses, for the confirm. */
function TidyDescription({ sql }: { sql: string }) {
  const comments = sql.split("\n").filter((line) => /^\s*--/.test(line)).length;
  return (
    <>
      Rewrites the SQL in canonical order for everyone here; edits made before it can no longer be
      undone. The schema stays the same, table and column comments included;{" "}
      {comments === 0
        ? "-- comments would be lost (there are none)."
        : `the ${comments} ${comments === 1 ? "line" : "lines"} of -- comments ${comments === 1 ? "is" : "are"} lost.`}
    </>
  );
}

function tidyFailure(error: unknown): string {
  if (!(error instanceof ApiError)) return "Could not tidy the SQL";
  switch (error.code) {
    case "compile_failed":
      return "Fix the SQL's errors first";
    case "tidy_unsafe":
      return "Tidy would change the schema";
    case "draft_changed":
      return "The SQL kept changing";
    default:
      return "Could not tidy the SQL";
  }
}

/**
 * What comes next: bring in a newer version first, then resolve what
 * conflicts, then publish.
 */
function PublishButton({
  draft,
  conflicts,
  updating,
  onPublish,
  onUpdate,
  onNextConflict,
}: {
  draft: Draft;
  conflicts: number;
  updating: boolean;
  onPublish: () => void;
  onUpdate: () => void;
  onNextConflict: () => void;
}) {
  const next = draft.next_version;
  if (draft.stale) {
    // Someone published, or an admin removed the version it started from.
    const why = draft.base_removed
      ? `v${draft.base_version} was removed`
      : `v${draft.latest_version} is out`;
    return (
      <Button
        variant="primary"
        loading={updating}
        onClick={onUpdate}
        aria-label={`Update draft (${why})`}
      >
        <span className="sm:hidden">Update</span>
        <span className="max-sm:hidden">
          Update draft <span className="font-normal opacity-80">({why})</span>
        </span>
      </Button>
    );
  }
  if (conflicts > 0) {
    const label = `Resolve ${conflicts} conflict${conflicts === 1 ? "" : "s"}`;
    return (
      <Tooltip content="Publishing waits until no conflict is left. Shows the next one.">
        <Button onClick={onNextConflict} aria-label={label}>
          <span className="sm:hidden">{conflicts} left</span>
          <span className="max-sm:hidden">{label}</span>
        </Button>
      </Tooltip>
    );
  }
  return (
    <Button variant="primary" onClick={onPublish} aria-label={`Publish v${next}`}>
      <span className="sm:hidden">Publish</span>
      <span className="max-sm:hidden">Publish v{next}</span>
    </Button>
  );
}

/** The name, renamed in place (Enter saves, Esc cancels). */
function DraftName({
  org,
  db,
  draft,
  editable,
  editing,
  onEditingChange,
}: {
  org: string;
  db: string;
  draft: Draft;
  editable: boolean;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
}) {
  const rename = useRenameDraft(org, db, draft.id);
  const [value, setValue] = React.useState(draft.name);
  // Starting to edit takes the current name (a rename by someone else
  // meanwhile must not reset what is being typed).
  const [wasEditing, setWasEditing] = React.useState(editing);
  if (editing !== wasEditing) {
    setWasEditing(editing);
    if (editing) setValue(draft.name);
  }
  const selectAll = React.useCallback((input: HTMLInputElement | null) => {
    if (input) requestAnimationFrame(() => input.select());
  }, []);

  const save = () => {
    onEditingChange(false);
    const name = value.trim();
    if (!name || name === draft.name) return;
    rename.mutate(name, {
      onError: (error) =>
        toast.error("Could not rename the draft", {
          description: error instanceof ApiError ? error.message : "Check your connection.",
        }),
    });
  };

  if (editing) {
    return (
      <Input
        ref={selectAll}
        aria-label="Draft name"
        value={value}
        maxLength={200}
        onChange={(event) => setValue(event.target.value)}
        onBlur={save}
        onKeyDown={(event) => {
          if (event.key === "Enter") event.currentTarget.blur();
          if (event.key === "Escape") {
            event.preventDefault();
            setValue(draft.name);
            onEditingChange(false);
          }
        }}
        className="h-7 w-[min(18rem,40vw)] px-1.5 text-sm font-semibold"
      />
    );
  }
  if (!editable) {
    return <h1 className="truncate px-1.5 text-sm font-semibold text-foreground">{draft.name}</h1>;
  }
  return (
    <h1 className="min-w-0">
      <Tooltip content="Rename">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => onEditingChange(true)}
          className="max-w-[min(18rem,40vw)] px-1.5 text-sm font-semibold text-foreground"
          aria-label={`${draft.name}, rename`}
        >
          <span className="truncate">{draft.name}</span>
        </Button>
      </Tooltip>
    </h1>
  );
}

function ConnectionStatus({
  connection,
  synced,
  unsynced,
}: {
  connection: Connection;
  synced: boolean;
  unsynced: boolean;
}) {
  if (!synced || connection === "connected" || connection === "connecting") return null;
  const label = unsynced
    ? "Edits not sent yet"
    : connection === "offline"
      ? "Offline"
      : "Reconnecting…";
  return (
    <Tooltip content="Your edits are kept here and sent as soon as you are back online.">
      <Button
        variant="ghost"
        size="sm"
        data-testid="connection-status"
        className="gap-1.5 text-warning"
        aria-live="polite"
      >
        <WifiOffIcon />
        <span className="max-sm:sr-only">{label}</span>
      </Button>
    </Tooltip>
  );
}

/** Who else is here, once per person (several tabs count once). */
function Presence({ peers, meId }: { peers: Peer[]; meId: string }) {
  const people = React.useMemo(() => {
    const byUser = new Map<string, { id: string; name: string; detail: string }>();
    for (const peer of peers) {
      if (peer.user.id === meId) continue;
      const watching = peer.user.role === "reviewer";
      const known = byUser.get(peer.user.id);
      if (!known || (known.detail === "watching" && !watching)) {
        byUser.set(peer.user.id, {
          id: peer.user.id,
          name: peer.user.name,
          detail: watching ? "watching" : "editing",
        });
      }
    }
    return [...byUser.values()].sort((a, b) => a.name.localeCompare(b.name));
  }, [peers, meId]);
  if (people.length === 0) return null;
  return (
    <AvatarStack
      people={people}
      max={3}
      className={cn("mr-1", people.length > 2 && "max-sm:hidden")}
    />
  );
}
