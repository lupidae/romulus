import { useInfiniteQuery, useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import {
  CheckCircleIcon,
  CheckIcon,
  ClockIcon,
  EyeClosedIcon,
  MoreHorizIcon,
  NavArrowDownIcon,
  NavArrowLeftIcon,
  NavArrowRightIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip } from "@/components/ui/tooltip";
import { meQuery } from "@/features/auth/queries";
import { CommentModeButton, CommentPins } from "@/features/comments/components/comment-pins";
import { CommentsAside, CommentsToggle } from "@/features/comments/components/comments-layer";
import { CommentsPanel } from "@/features/comments/components/comments-panel";
import { useVersionDiscussion } from "@/features/comments/discussion";
import { sqlThreads } from "@/features/comments/sql-threads";
import { useComments } from "@/features/comments/use-comments";
import { useCommentsUi } from "@/features/comments/use-comments-ui";
import { CompareWith } from "@/features/diff/components/compare-with";
import { MigrationButton } from "@/features/imports/components/migration-dialog";
import { PlaygroundButton } from "@/features/playground/components/entry";
import { names } from "@/features/draft/review";
import { orgQuery } from "@/features/orgs/queries";
import { canManageMembers, canWrite } from "@/features/orgs/roles";
import { parseSchema, SchemaParseError } from "@/features/schema/model";
import { SchemaViewer } from "@/features/schema/viewer/schema-viewer";
import { SchemaViewerSkeleton } from "@/features/schema/viewer/viewer-skeleton";
import { timeAgo } from "@/lib/time";
import { useShortcut } from "@/lib/use-shortcut";

import {
  type Database,
  neighbours,
  originLabel,
  versionQuery,
  versionsQuery,
  versionTextsQuery,
  type VersionDetail,
} from "../queries";
import { RemoveVersionDialog } from "./remove-version-dialog";
import { VersionPostgresBadge } from "./postgres";
import { Byline, OriginBadge } from "./version-meta";
import { WrittenMigrationButton } from "./written-migration";

interface VersionViewProps {
  org: string;
  database: Database;
  number: number;
}

/** Tables (and partitioned ones) in a model, as remus writes it. */
function tableCount(model: unknown): number | undefined {
  const entities = (model as { entities?: { kind?: string }[] } | null)?.entities;
  return entities?.filter((e) => e.kind === "r" || e.kind === "p").length;
}

/**
 * One version, read-only: a strip saying which one it is (with the way to
 * the others), then the schema viewer, graph and text, and its discussion
 * (the review of the draft it came from, frozen; threads started since).
 */
export function VersionView({ org, database, number }: VersionViewProps) {
  const db = database.slug;
  const version = useQuery(versionQuery(org, db, database.id, number));
  const texts = useQuery(versionTextsQuery(org, db, database.id, number));
  const latest = database.latest?.number ?? number;
  const { data: me } = useSuspenseQuery(meQuery);
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const [selected, setSelected] = React.useState<string | null>(null);

  const model = React.useMemo(() => {
    if (!version.data) return undefined;
    try {
      const { layout, groups } = version.data.layout;
      return { schema: parseSchema(version.data.model), layout, groups };
    } catch (error) {
      return { error: error instanceof SchemaParseError ? error.message : String(error) };
    }
  }, [version.data]);

  const commentsUi = useCommentsUi();
  const discussion = useVersionDiscussion({
    org,
    db,
    number,
    me: { id: me.id, name: me.name },
    canModerate: canWrite(orgData.role),
  });
  const comments = useComments({
    discussion,
    ui: commentsUi,
    schema: model && "schema" in model ? (model.schema ?? null) : null,
    sql: texts.data?.sql ?? null,
    selected,
    onSelect: setSelected,
  });
  const overlay = React.useMemo(() => [sqlThreads(comments.feed)], [comments.feed]);
  const frozen = discussion.threads?.some((t) => t.frozen) ?? false;

  const failed = version.error ?? texts.error;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <VersionStrip
        org={org}
        database={database}
        number={number}
        latest={latest}
        version={version.data}
        discussion={
          <CommentsToggle
            ui={commentsUi}
            label="Discussion"
            open={comments.pinned.length}
            unread={comments.unreadCount}
          />
        }
      />
      <div className="flex min-h-0 flex-1">
        <div className="relative min-h-0 min-w-0 flex-1">
          {failed ? (
            <div className="p-4 md:p-6">
              <Alert
                variant="destructive"
                title={`Could not load v${number}`}
                action={
                  <Button
                    size="sm"
                    onClick={() => {
                      void version.refetch();
                      void texts.refetch();
                    }}
                  >
                    Retry
                  </Button>
                }
              >
                {failed.message}
              </Alert>
            </div>
          ) : model && "error" in model ? (
            <div className="p-4 md:p-6">
              <Alert variant="destructive" title={`v${number} cannot be drawn`}>
                Its model is not one this viewer understands ({model.error}).
              </Alert>
            </div>
          ) : model && texts.data ? (
            <SchemaViewer
              schema={model.schema}
              texts={texts.data}
              layout={model.layout}
              groups={model.groups}
              readOnly
              storageKey="romulus:version-viewer"
              selected={selected}
              onSelectedChange={setSelected}
              sqlOverlay={overlay}
              graphFocus={comments.focus}
              graphControls={
                <CommentModeButton on={commentsUi.commentMode} onChange={comments.setCommentMode} />
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
            />
          ) : (
            <SchemaViewerSkeleton />
          )}
        </div>
        <CommentsAside ui={commentsUi} title="Discussion">
          <CommentsPanel
            discussion={discussion}
            ui={commentsUi}
            title="Discussion"
            outdated={comments.outdated}
            onShow={comments.show}
            onNew={comments.startNew}
            note={
              frozen
                ? `Threads with a lock are from the review of the draft that became v${number}, kept as they were. New threads are about this version.`
                : undefined
            }
          />
        </CommentsAside>
      </div>
    </div>
  );
}

function VersionStrip({
  org,
  database,
  number,
  latest,
  version,
  discussion,
}: {
  org: string;
  database: Database;
  number: number;
  latest: number;
  version?: VersionDetail;
  /** The discussion panel's button. */
  discussion?: React.ReactNode;
}) {
  const navigate = useNavigate();
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const [removing, setRemoving] = React.useState(false);
  const db = database.slug;
  const go = (n: number) =>
    n === latest
      ? navigate({ to: "/$org/$db", params: { org, db } })
      : navigate({ to: "/$org/$db/v/$number", params: { org, db, number: n } });
  // Removed versions leave gaps: older and newer skip them.
  const { older, newer } = neighbours(database, number);
  useShortcut("[", () => older && void go(older), !!older);
  useShortcut("]", () => newer && void go(newer), !!newer);
  const admin = canManageMembers(orgData.role);

  return (
    <div className="flex min-h-11 shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-3 py-1.5 md:px-4">
      <div className="flex items-center gap-0.5">
        <Tooltip content="Older version" shortcut={["["]}>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Older version"
            disabled={!older}
            onClick={() => older && void go(older)}
          >
            <NavArrowLeftIcon />
          </Button>
        </Tooltip>
        <VersionPicker org={org} database={database} number={number} latest={latest} />
        <Tooltip content="Newer version" shortcut={["]"]}>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Newer version"
            disabled={!newer}
            onClick={() => newer && void go(newer)}
          >
            <NavArrowRightIcon />
          </Button>
        </Tooltip>
      </div>
      {number === latest ? (
        <Badge variant="success">
          <CheckIcon aria-hidden />
          Latest
        </Badge>
      ) : (
        <span className="flex items-center gap-2">
          <Badge variant="warning">
            <ClockIcon aria-hidden />
            Older version
          </Badge>
          <Button variant="link" size="sm" asChild>
            <Link to="/$org/$db" params={{ org, db }}>
              Go to v{latest}
            </Link>
          </Button>
        </span>
      )}
      {version ? (
        <div className="hidden min-w-0 flex-1 items-center gap-3 text-sm md:flex">
          <OriginBadge origin={version.origin} />
          <VersionPostgresBadge version={version} />
          <Byline author={version.author} at={version.created_at} />
          {version.approvals.length > 0 && (
            <span className="hidden max-w-64 min-w-0 shrink items-center gap-1 text-sm text-muted-foreground lg:inline-flex">
              <CheckCircleIcon aria-hidden className="size-3.5 shrink-0 text-success" />
              <span className="truncate">Approved by {names(version.approvals)}</span>
            </span>
          )}
          {version.message && (
            <span
              className="hidden min-w-0 truncate text-foreground lg:inline"
              title={version.message}
            >
              {version.message}
            </span>
          )}
        </div>
      ) : (
        <span className="flex-1" />
      )}
      <div className="ml-auto flex items-center gap-1">
        {discussion}
        {version && (
          <PlaygroundButton
            org={org}
            db={db}
            source={{ kind: "version", version: number, tables: tableCount(version.model) }}
            canCreate={canWrite(orgData.role)}
            variant="ghost"
            compact
          />
        )}
        {version && <MigrationButton version={version} />}
        {version && <WrittenMigrationButton org={org} database={database} version={version} />}
        <CompareWith org={org} db={db} number={number} latest={latest} previous={older} />
        {admin && version && (
          <Menu>
            <Tooltip content={`More on v${number}`}>
              <MenuTrigger asChild>
                <Button variant="ghost" size="icon-sm" aria-label={`More actions on v${number}`}>
                  <MoreHorizIcon />
                </Button>
              </MenuTrigger>
            </Tooltip>
            <MenuContent align="end">
              <MenuItem variant="destructive" onSelect={() => setRemoving(true)}>
                <EyeClosedIcon />
                Remove version…
              </MenuItem>
            </MenuContent>
          </Menu>
        )}
      </div>
      {admin && (
        <RemoveVersionDialog
          org={org}
          database={database}
          number={number}
          open={removing}
          onOpenChange={setRemoving}
          onRemoved={() => void navigate({ to: "/$org/$db", params: { org, db } })}
        />
      )}
    </div>
  );
}

/** "v12 ▾": the recent versions, one click away. */
function VersionPicker({
  org,
  database,
  number,
  latest,
}: {
  org: string;
  database: Database;
  number: number;
  latest: number;
}) {
  const [open, setOpen] = React.useState(false);
  const versions = useInfiniteQuery({ ...versionsQuery(org, database.slug), enabled: open });
  const recent = versions.data?.pages[0]?.versions.slice(0, 12) ?? [];
  const db = database.slug;
  return (
    <Menu open={open} onOpenChange={setOpen}>
      <MenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="font-mono text-sm font-semibold text-foreground"
        >
          v{number}
          <NavArrowDownIcon className="text-muted-foreground" />
          <span className="sr-only">: choose another version</span>
        </Button>
      </MenuTrigger>
      <MenuContent align="start" className="w-80">
        <MenuLabel>Recent versions</MenuLabel>
        {versions.isPending
          ? Array.from({ length: Math.min(database.versions, 5) }, (_, i) => (
              <div key={i} className="mx-1 h-8 animate-pulse rounded-md bg-muted/60" />
            ))
          : recent.map((v) => (
              <MenuItem key={v.id} asChild>
                <Link
                  to={v.number === latest ? "/$org/$db" : "/$org/$db/v/$number"}
                  params={{ org, db, number: v.number }}
                  className="flex items-center gap-2"
                >
                  <span className="w-9 shrink-0 font-mono text-sm font-medium text-foreground tabular-nums">
                    v{v.number}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
                    {v.message ??
                      `${originLabel[v.origin]} by ${v.author?.name ?? "a deleted account"}`}
                  </span>
                  <span className="shrink-0 text-xs text-faint-foreground">
                    {timeAgo(v.created_at)}
                  </span>
                  {v.number === number && <CheckIcon aria-label="Shown" className="text-primary" />}
                </Link>
              </MenuItem>
            ))}
        <MenuSeparator />
        <MenuItem asChild>
          <Link to="/$org/$db/history" params={{ org, db }}>
            <ClockIcon />
            {database.versions === 1 ? "The only version" : `All ${database.versions} versions`}
          </Link>
        </MenuItem>
      </MenuContent>
    </Menu>
  );
}
