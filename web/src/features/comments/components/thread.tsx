import * as React from "react";

import {
  CheckCircleIcon,
  CursorPointerIcon,
  EditPencilIcon,
  LockIcon,
  MoreHorizIcon,
  NavArrowUpIcon,
  TableIcon,
  TableRowsIcon,
  TextSquareIcon,
  TrashIcon,
  UndoIcon,
} from "@/components/icons";
import { Avatar } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { Menu, MenuContent, MenuItem, MenuTrigger } from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { formatDateTime, timeAgo } from "@/lib/time";
import { cn } from "@/lib/utils";

import type { Discussion } from "../discussion";
import {
  type Anchor,
  anchorLabel,
  bodyParts,
  type CommentView,
  type ThreadView,
  typingLine,
} from "../model";
import { Composer } from "./composer";

export function AnchorIcon({ anchor, className }: { anchor: Anchor; className?: string }) {
  const Icon =
    anchor.kind === "table"
      ? TableIcon
      : anchor.kind === "column"
        ? TableRowsIcon
        : anchor.kind === "point"
          ? CursorPointerIcon
          : TextSquareIcon;
  return <Icon aria-hidden className={cn("size-3.5 shrink-0", className)} />;
}

/** "users.email" in the code font, with what kind of thing it is. */
export function AnchorChip({ anchor, outdated }: { anchor: Anchor; outdated?: boolean }) {
  return (
    <span
      className={cn(
        "inline-flex min-w-0 items-center gap-1 text-xs text-muted-foreground",
        outdated && "text-faint-foreground line-through decoration-faint-foreground/60",
      )}
    >
      <AnchorIcon anchor={anchor} />
      <span className={cn("shrink-0", anchor.kind !== "point" && "font-mono")}>
        {anchorLabel(anchor)}
      </span>
      {/* Written above another statement than its table's CREATE: which. */}
      {anchor.kind !== "point" && anchor.kind !== "statement" && anchor.statement && (
        <span className="min-w-0 truncate font-mono text-faint-foreground" title={anchor.statement}>
          · {anchor.statement}
        </span>
      )}
    </span>
  );
}

function When({ at, edited }: { at: string; edited?: string | null }) {
  return (
    <span className="shrink-0 text-xs text-faint-foreground">
      <time dateTime={at} title={formatDateTime(at)}>
        {timeAgo(at)}
      </time>
      {edited && (
        <span title={`Edited ${formatDateTime(edited)}`}>
          {" · "}
          edited
        </span>
      )}
    </span>
  );
}

/** A comment's text, its mentions drawn as such (yours stronger). */
function Body({
  comment,
  discussion,
  className,
}: {
  comment: CommentView;
  discussion: Discussion;
  className?: string;
}) {
  const mentioned = discussion.people.filter((p) => comment.mentions.includes(p.id));
  return (
    <p className={cn("text-sm break-words whitespace-pre-wrap text-foreground", className)}>
      {bodyParts(comment.body, mentioned).map((part, i) =>
        "mention" in part ? (
          <span
            key={i}
            className={cn(
              "rounded-sm px-0.5 font-medium text-primary",
              part.mention.id === discussion.me.id ? "bg-primary/15" : "bg-primary/8",
            )}
          >
            {part.text}
          </span>
        ) : (
          <React.Fragment key={i}>{part.text}</React.Fragment>
        ),
      )}
    </p>
  );
}

interface ThreadItemProps {
  thread: ThreadView;
  discussion: Discussion;
  expanded: boolean;
  unread: boolean;
  outdated: boolean;
  onToggle: () => void;
  /** Brings what it is pinned to into view. */
  onShow?: () => void;
}

/**
 * One thread in the panel: its first comment and how it stands, folded;
 * every comment, the reply field and its actions, open.
 */
export function ThreadItem({
  thread,
  discussion,
  expanded,
  unread,
  outdated,
  onToggle,
  onShow,
}: ThreadItemProps) {
  const first = thread.comments[0]!;
  const replies = thread.comments.length - 1;
  const typing = typingLine(discussion.typing(thread.id));
  const writable = discussion.canComment && !thread.frozen;
  const item = React.useRef<HTMLLIElement>(null);
  React.useEffect(() => {
    if (expanded) item.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [expanded]);
  const summary = thread.resolved
    ? `Resolved by ${thread.resolved.name}`
    : replies === 0
      ? null
      : replies === 1
        ? "1 reply"
        : `${replies} replies`;

  return (
    <li
      ref={item}
      data-thread={thread.id}
      className={cn(
        "relative border-b border-border",
        expanded ? "bg-accent/40" : "hover:bg-accent/40",
      )}
    >
      {unread && !expanded && (
        <span aria-hidden className="absolute top-4 left-1.5 size-1.5 rounded-full bg-primary" />
      )}
      <Button
        variant="ghost"
        aria-expanded={expanded}
        onClick={onToggle}
        className="grid h-auto w-full justify-stretch gap-1.5 rounded-none px-4 py-3 text-left font-normal whitespace-normal hover:bg-transparent focus-visible:ring-offset-0 focus-visible:ring-inset sm:h-auto"
      >
        <span className="flex min-w-0 items-center gap-2">
          <AnchorChip anchor={thread.anchor} outdated={outdated} />
          {thread.frozen && (
            <LockIcon
              aria-label="From the draft's review"
              className="size-3 text-faint-foreground"
            />
          )}
          {!expanded && (
            <span className="ml-auto">
              <When at={first.at} />
            </span>
          )}
          {expanded && (
            <NavArrowUpIcon aria-hidden className="ml-auto size-3.5 text-faint-foreground" />
          )}
        </span>
        {!expanded && (
          <>
            <span className="flex min-w-0 items-center gap-2">
              <Avatar name={first.authorName} id={first.authorId ?? first.authorName} size="sm" />
              <span className="truncate text-sm font-medium text-foreground">
                {first.authorName}
              </span>
              {unread && <span className="sr-only">, unread</span>}
            </span>
            <Body comment={first} discussion={discussion} className="line-clamp-3" />
            {(summary || typing) && (
              <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
                {thread.resolved && (
                  <CheckCircleIcon aria-hidden className="size-3.5 text-success" />
                )}
                {typing ?? summary}
              </span>
            )}
          </>
        )}
      </Button>
      {expanded && (
        <div className="grid gap-3 px-4 pb-4">
          <div className="-mt-1 flex flex-wrap items-center gap-1">
            {onShow && (
              <Button variant="ghost" size="sm" onClick={onShow} disabled={outdated}>
                <AnchorIcon anchor={thread.anchor} />
                {outdated ? "No longer in the schema" : "Show"}
              </Button>
            )}
            {thread.frozen ? (
              <Badge variant="outline" className="ml-auto">
                <LockIcon aria-hidden />
                From the draft's review
              </Badge>
            ) : (
              writable && (
                <Button
                  variant="ghost"
                  size="sm"
                  className="ml-auto"
                  onClick={() =>
                    void discussion
                      .resolve(thread.id, thread.resolved === null)
                      .then(() => {
                        if (thread.resolved === null) {
                          toast.success("Thread resolved", {
                            description: "It moved to Resolved. Reopen it there if it comes back.",
                          });
                        }
                      })
                      .catch(failed("Could not change the thread"))
                  }
                >
                  {thread.resolved ? <UndoIcon /> : <CheckCircleIcon />}
                  {thread.resolved ? "Reopen" : "Resolve"}
                </Button>
              )
            )}
            {!thread.frozen && writable && discussion.canModerate && discussion.removeThread && (
              <DeleteThread
                thread={thread}
                remove={discussion.removeThread}
                me={discussion.me.id}
              />
            )}
          </div>
          {thread.resolved && (
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <CheckCircleIcon aria-hidden className="size-3.5 text-success" />
              Resolved by {thread.resolved.name}, {timeAgo(thread.resolved.at)}
            </p>
          )}
          <ol className="grid gap-3" aria-label="Comments">
            {thread.comments.map((comment, i) => (
              <CommentItem
                key={comment.id}
                thread={thread}
                comment={comment}
                first={i === 0}
                discussion={discussion}
                writable={writable}
              />
            ))}
          </ol>
          {typing && (
            <p aria-live="polite" className="text-xs text-muted-foreground">
              {typing}
            </p>
          )}
          {writable && (
            <Composer
              label={`Reply to ${first.authorName}`}
              placeholder="Reply…"
              submitLabel="Reply"
              people={discussion.people}
              onTyping={(on) => discussion.setTyping(on ? thread.id : null)}
              onSubmit={(body, mentions) =>
                discussion
                  .reply(thread.id, body, mentions)
                  .catch(failed("Could not send the reply"))
              }
            />
          )}
        </div>
      )}
    </li>
  );
}

/** A developer's or an admin's: the thread and every comment in it. */
function DeleteThread({
  thread,
  remove,
  me,
}: {
  thread: ThreadView;
  remove: (thread: string) => Promise<void>;
  me: string;
}) {
  const [open, setOpen] = React.useState(false);
  const count = thread.comments.length;
  const others = thread.comments.some((c) => c.authorId !== me);
  return (
    <>
      <Tooltip content="Delete thread">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Delete thread"
          onClick={() => setOpen(true)}
        >
          <TrashIcon />
        </Button>
      </Tooltip>
      <ConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title="Delete this thread?"
        description={`${count === 1 ? "Its comment goes" : `Its ${count} comments go`} for everyone.${
          others ? " Deleting others' comments is recorded in the database's activity." : ""
        }`}
        confirmLabel="Delete thread"
        destructive
        onConfirm={async () => {
          try {
            await remove(thread.id);
          } catch (error) {
            failed("Could not delete the thread")(error);
          }
        }}
      />
    </>
  );
}

function failed(title: string) {
  return (error: unknown) => {
    toast.error(title, {
      description: error instanceof Error ? error.message : "Check your connection.",
    });
    throw error;
  };
}

function CommentItem({
  thread,
  comment,
  first,
  discussion,
  writable,
}: {
  thread: ThreadView;
  comment: CommentView;
  first: boolean;
  discussion: Discussion;
  writable: boolean;
}) {
  const [editing, setEditing] = React.useState(false);
  const [deleting, setDeleting] = React.useState(false);
  const mine = comment.authorId === discussion.me.id && writable;
  // Developers and admins delete anyone's (the server records it).
  const moderated = !mine && writable && discussion.canModerate;
  // The thread goes with its last comment (the server never takes others' replies).
  const alone = thread.comments.length === 1;
  const recorded = "Deleting someone else's comment is recorded in the database's activity.";
  return (
    <li className="grid gap-1">
      <div className="flex min-w-0 items-center gap-2">
        <Avatar name={comment.authorName} id={comment.authorId ?? comment.authorName} size="sm" />
        <span className="truncate text-sm font-medium text-foreground">{comment.authorName}</span>
        <When at={comment.at} edited={comment.edited} />
        {(mine || moderated) && !editing && (
          <Menu>
            <Tooltip content={mine ? "Edit or delete" : "Delete"}>
              <MenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="-my-1 ml-auto"
                  aria-label={
                    mine
                      ? first
                        ? "More on your comment"
                        : "More on your reply"
                      : `More on ${comment.authorName}'s ${first ? "comment" : "reply"}`
                  }
                >
                  <MoreHorizIcon />
                </Button>
              </MenuTrigger>
            </Tooltip>
            <MenuContent align="end">
              {mine && (
                <MenuItem onSelect={() => setEditing(true)}>
                  <EditPencilIcon />
                  Edit
                </MenuItem>
              )}
              <MenuItem variant="destructive" onSelect={() => setDeleting(true)}>
                <TrashIcon />
                Delete…
              </MenuItem>
            </MenuContent>
          </Menu>
        )}
      </div>
      {editing ? (
        <Composer
          label="Edit your comment"
          placeholder="Your comment"
          submitLabel="Save"
          initial={comment.body}
          focusOnMount
          people={discussion.people}
          onCancel={() => setEditing(false)}
          onSubmit={(body, mentions) =>
            discussion
              .edit(thread.id, comment.id, body, mentions)
              .then(() => setEditing(false))
              .catch(failed("Could not save the comment"))
          }
        />
      ) : (
        <Body comment={comment} discussion={discussion} className="pl-7" />
      )}
      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title={
          alone
            ? "Delete this thread?"
            : mine
              ? "Delete your comment?"
              : `Delete ${comment.authorName}'s comment?`
        }
        description={
          !mine
            ? alone
              ? `${comment.authorName}'s comment is the whole thread: it goes for everyone. ${recorded}`
              : `It goes for everyone in the discussion. ${recorded}`
            : alone
              ? "Your comment is the whole thread: it goes for everyone."
              : first
                ? "The replies stay, without your comment above them."
                : "It goes for everyone in the discussion."
        }
        confirmLabel={alone ? "Delete thread" : "Delete comment"}
        destructive
        onConfirm={async () => {
          try {
            await discussion.remove(thread.id, comment.id);
          } catch (error) {
            failed("Could not delete the comment")(error);
          }
        }}
      />
    </li>
  );
}
