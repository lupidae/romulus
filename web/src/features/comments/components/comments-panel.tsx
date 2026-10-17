import * as React from "react";

import {
  ChatLinesIcon,
  ChatPlusInIcon,
  NavArrowDownIcon,
  NavArrowRightIcon,
  XmarkIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";
import { Tooltip } from "@/components/ui/tooltip";
import { useMediaQuery } from "@/features/schema/viewer/use-preference";
import { cn } from "@/lib/utils";

import type { Discussion } from "../discussion";
import { type Anchor, anchorLabel, involves, isUnread, type ThreadView } from "../model";
import type { CommentsUi, Filter } from "../use-comments-ui";
import { Composer } from "./composer";
import { AnchorIcon, ThreadItem } from "./thread";

interface CommentsPanelProps {
  discussion: Discussion;
  ui: CommentsUi;
  /** "Comments" in a draft, "Discussion" on a version. */
  title: string;
  /** Its anchor is gone from the schema (or the text). */
  outdated: (thread: ThreadView) => boolean;
  /** Brings a thread's anchor into view (selects its table…). */
  onShow: (thread: ThreadView) => void;
  /** "New comment": on the selected table, or comment mode. */
  onNew: () => void;
  /** One line under the title (a version: what is read-only). */
  note?: React.ReactNode;
  className?: string;
}

const FILTERS: { value: Filter; label: string }[] = [
  { value: "open", label: "Open" },
  { value: "resolved", label: "Resolved" },
  { value: "mine", label: "Mine" },
];

/**
 * The threads of a draft or a version, newest first (a thread does not
 * move while people reply in it; unread ones are marked): a filter
 * (open, resolved, mine), a thread opened in place, a new one being written
 * at the top, and those whose anchor is gone under "Outdated".
 */
export function CommentsPanel({
  discussion,
  ui,
  title,
  outdated,
  onShow,
  onNew,
  note,
  className,
}: CommentsPanelProps) {
  const { threads, me } = discussion;
  const [showOutdated, setShowOutdated] = React.useState(false);
  const wide = useMediaQuery("(min-width: 48rem)");
  const headingId = React.useId();

  const unread = (t: ThreadView) => isUnread(t, me.id, discussion.readAt(t.id));
  const all = threads ?? [];
  const counts = {
    open: all.filter((t) => !t.resolved).length,
    resolved: all.filter((t) => t.resolved).length,
    mine: all.filter((t) => involves(t, me.id)).length,
  };
  const shown = all
    .filter(
      (t) =>
        // The thread being read stays, whatever happens to it meanwhile.
        t.id === ui.active ||
        (ui.filter === "open"
          ? !t.resolved
          : ui.filter === "resolved"
            ? !!t.resolved
            : involves(t, me.id)),
    )
    .sort((a, b) => (a.at < b.at ? 1 : a.at > b.at ? -1 : 0));
  const current = shown.filter((t) => !outdated(t));
  const gone = shown.filter((t) => outdated(t));

  // Opening a thread reads it.
  const active = ui.active ? all.find((t) => t.id === ui.active) : undefined;
  const { markRead } = discussion;
  React.useEffect(() => {
    if (active) markRead(active);
  }, [active, markRead]);
  // A thread opened elsewhere (a pin, the SQL) under another filter, or
  // outdated: shown, once, when it opens.
  const [seen, setSeen] = React.useState<string | null>(null);
  if (active && seen !== active.id) {
    setSeen(active.id);
    const fits =
      ui.filter === "open" ? !active.resolved : ui.filter === "resolved" ? !!active.resolved : true;
    if (!fits) ui.setFilter(active.resolved ? "resolved" : "open");
    if (outdated(active)) setShowOutdated(true);
  }

  const item = (t: ThreadView) => (
    <ThreadItem
      key={t.id}
      thread={t}
      discussion={discussion}
      expanded={ui.active === t.id}
      unread={unread(t)}
      outdated={outdated(t)}
      onToggle={() => {
        if (ui.active === t.id) ui.setActive(null);
        else {
          ui.openThread(t.id);
          // Beside the viewer, what it is about comes into view; over it
          // (a phone), only when asked ("Show").
          if (!outdated(t) && wide) onShow(t);
        }
      }}
      onShow={() => {
        onShow(t);
        if (!wide) ui.setOpen(false);
      }}
    />
  );

  return (
    <section
      aria-labelledby={headingId}
      className={cn("flex h-full min-h-0 flex-col bg-card", className)}
    >
      <header className="flex h-11 shrink-0 items-center gap-1 border-b border-border pr-1.5 pl-4">
        <h2 id={headingId} className="text-sm font-semibold text-foreground">
          {title}
        </h2>
        {threads && counts.open > 0 && (
          <span className="text-sm text-muted-foreground tabular-nums">{counts.open}</span>
        )}
        <div className="ml-auto flex items-center gap-0.5">
          {discussion.canComment && (
            <Tooltip content="New comment" shortcut={["C"]}>
              <Button variant="ghost" size="icon-sm" aria-label="New comment" onClick={onNew}>
                <ChatPlusInIcon />
              </Button>
            </Tooltip>
          )}
          <Tooltip content={`Close ${title.toLowerCase()}`}>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={`Close ${title.toLowerCase()}`}
              onClick={ui.close}
            >
              <XmarkIcon />
            </Button>
          </Tooltip>
        </div>
      </header>
      {note && (
        <p className="shrink-0 border-b border-border bg-muted/60 px-4 py-2 text-xs text-muted-foreground">
          {note}
        </p>
      )}
      <div className="shrink-0 border-b border-border px-3 py-2">
        <ToggleGroup
          type="single"
          value={ui.filter}
          onValueChange={(value) => value && ui.setFilter(value as Filter)}
          aria-label="Show threads"
          className="grid w-full grid-cols-3"
        >
          {FILTERS.map((f) => (
            <ToggleGroupItem key={f.value} value={f.value}>
              {f.label}
              {threads && f.value !== "mine" && counts[f.value] > 0 && (
                <span className="text-xs text-muted-foreground tabular-nums">
                  {counts[f.value]}
                </span>
              )}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
        {ui.composing && <NewThread anchor={ui.composing} discussion={discussion} ui={ui} />}
        {discussion.error ? (
          <div className="p-4">
            <Alert
              variant="destructive"
              title="Could not load the comments"
              action={
                <Button size="sm" onClick={discussion.retry}>
                  Retry
                </Button>
              }
            >
              {discussion.error.message}
            </Alert>
          </div>
        ) : threads === null ? (
          <ThreadsSkeleton />
        ) : shown.length === 0 && !ui.composing ? (
          <Empty
            filter={ui.filter}
            any={all.length > 0}
            canComment={discussion.canComment}
            onNew={onNew}
          />
        ) : (
          <>
            {current.length > 0 && (
              <ul aria-label={`${FILTERS.find((f) => f.value === ui.filter)!.label} threads`}>
                {current.map(item)}
              </ul>
            )}
            {gone.length > 0 && (
              <div>
                <Button
                  variant="ghost"
                  aria-expanded={showOutdated}
                  onClick={() => setShowOutdated(!showOutdated)}
                  className="h-auto w-full justify-start gap-1.5 rounded-none border-b border-border bg-muted/40 px-4 py-2 text-xs focus-visible:ring-offset-0 focus-visible:ring-inset sm:h-auto [&_svg]:size-3.5"
                >
                  {showOutdated ? (
                    <NavArrowDownIcon aria-hidden />
                  ) : (
                    <NavArrowRightIcon aria-hidden />
                  )}
                  Outdated
                  <span className="tabular-nums">{gone.length}</span>
                  <span className="ml-auto font-normal text-faint-foreground">
                    Their table or column is gone
                  </span>
                </Button>
                {showOutdated && <ul aria-label="Outdated threads">{gone.map(item)}</ul>}
              </div>
            )}
          </>
        )}
      </div>
    </section>
  );
}

function NewThread({
  anchor,
  discussion,
  ui,
}: {
  anchor: Anchor;
  discussion: Discussion;
  ui: CommentsUi;
}) {
  const label = anchorLabel(anchor);
  return (
    <div className="grid gap-2 border-b border-border bg-accent/40 px-4 py-3">
      <p className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
        <AnchorIcon anchor={anchor} />
        New comment on
        <span className={cn("truncate text-foreground", anchor.kind !== "point" && "font-mono")}>
          {anchor.kind === "point" ? "the canvas" : label}
        </span>
      </p>
      <Composer
        key={JSON.stringify(anchor)}
        label={`Comment on ${anchor.kind === "point" ? "the canvas" : label}`}
        placeholder="Add a comment…"
        submitLabel="Comment"
        focusOnMount
        people={discussion.people}
        onTyping={(on) => discussion.setTyping(on ? "new" : null)}
        onCancel={() => ui.setComposing(null)}
        onSubmit={async (body, mentions) => {
          try {
            const id = await discussion.start(anchor, body, mentions);
            ui.setComposing(null);
            if (id) ui.openThread(id);
          } catch (error) {
            toast.error("Could not add the comment", {
              description: error instanceof Error ? error.message : "Check your connection.",
            });
            throw error;
          }
        }}
      />
    </div>
  );
}

function Empty({
  filter,
  any,
  canComment,
  onNew,
}: {
  filter: Filter;
  any: boolean;
  canComment: boolean;
  onNew: () => void;
}) {
  if (any && filter === "open")
    return (
      <EmptyState
        icon={<ChatLinesIcon />}
        title="Every thread is resolved"
        description="Resolved threads stay under Resolved."
        className="py-10"
      />
    );
  if (any && filter === "resolved")
    return (
      <EmptyState
        icon={<ChatLinesIcon />}
        title="Nothing resolved yet"
        description="Threads people resolve move here."
        className="py-10"
      />
    );
  if (any && filter === "mine")
    return (
      <EmptyState
        icon={<ChatLinesIcon />}
        title="Nothing involving you"
        description="Threads you start, reply to or are @mentioned in show here."
        className="py-10"
      />
    );
  return (
    <EmptyState
      icon={<ChatLinesIcon />}
      title="No comments yet"
      description={
        canComment ? (
          <>
            Press C, then click a table, a column or the canvas. In the SQL, a line starting with{" "}
            <code className="font-mono text-foreground">---</code> becomes a comment on the
            statement below it.
          </>
        ) : (
          "Comments left here show up in this list."
        )
      }
      action={
        canComment && (
          <Button onClick={onNew}>
            <ChatPlusInIcon />
            Add a comment
          </Button>
        )
      }
      className="py-10"
    />
  );
}

function ThreadsSkeleton() {
  return (
    <div aria-hidden className="grid">
      {[0, 1, 2].map((i) => (
        <div key={i} className="grid gap-2 border-b border-border px-4 py-3">
          <Skeleton className="h-3 w-24" />
          <div className="flex items-center gap-2">
            <Skeleton className="size-5 rounded-full" />
            <Skeleton className="h-3 w-20" />
          </div>
          <Skeleton className="h-3 w-full" />
          <Skeleton className="h-3 w-3/5" />
        </div>
      ))}
    </div>
  );
}
