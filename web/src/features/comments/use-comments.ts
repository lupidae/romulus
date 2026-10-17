import type { EditorView } from "@codemirror/view";
import * as React from "react";

import { presenceIndex } from "@/components/ui/avatar";
import { toast } from "@/components/ui/toaster";
import type { SchemaGraphProps } from "@/features/schema/graph/schema-graph";
import type { Schema } from "@/features/schema/model";
import { useMediaQuery } from "@/features/schema/viewer/use-preference";
import { useShortcut } from "@/lib/use-shortcut";

import type { Discussion } from "./discussion";
import { anchorExists, isUnread, placeInSql, type ThreadView } from "./model";
import { createThreadFeed, type SqlThread, type ThreadFeed } from "./sql-threads";
import type { CommentsUi } from "./use-comments-ui";

interface Options {
  discussion: Discussion;
  ui: CommentsUi;
  /** The model the graph shows (null: none yet, nothing is outdated). */
  schema: Schema | null;
  /** The SQL the pane shows (statement anchors are looked for in it). */
  sql: string | null;
  /** The table selected in the graph. */
  selected: string | null;
  onSelect: (table: string | null) => void;
  /** The SQL editor, to bring a thread's statement into view. */
  sqlView?: EditorView | null;
  /** A `---` line typed in the source (editors only): see `ThreadFeed.commentLine`. */
  commentLine?: ThreadFeed["commentLine"];
  /** `---` lines pasted in the source (editors only): see `ThreadFeed.pasteComments`. */
  pasteComments?: ThreadFeed["pasteComments"];
}

/**
 * What the graph, the SQL pane and the panel need of a discussion: which
 * threads are outdated, unread or pinned, the SQL pane's feed, where to
 * take the view to show a thread, and the C shortcut.
 */
export function useComments({
  discussion,
  ui,
  schema,
  sql,
  selected,
  onSelect,
  sqlView,
  commentLine,
  pasteComments,
}: Options) {
  const { threads, me, readAt } = discussion;
  const outdated = React.useCallback(
    (t: ThreadView) => !anchorExists(t.anchor, schema, sql),
    [schema, sql],
  );
  const unread = React.useCallback(
    (t: ThreadView) => isUnread(t, me.id, readAt(t.id)),
    [me.id, readAt],
  );
  const pinned = React.useMemo(
    () => (threads ?? []).filter((t) => !t.resolved && !outdated(t)),
    [threads, outdated],
  );
  const unreadCount = pinned.filter(unread).length;

  // The SQL pane's feed: made once (the editor keeps it), the latest handlers in it.
  const [feed] = React.useState(createThreadFeed);
  const { openThread } = ui;
  React.useEffect(
    () =>
      feed.handle({
        open: openThread,
        commentLine,
        pasteComments,
        notice: (message) => toast.info("This line stays in the SQL", { description: message }),
      }),
    [feed, openThread, commentLine, pasteComments],
  );
  const shown = React.useMemo<SqlThread[]>(
    () =>
      pinned.map((t) => {
        const first = t.comments[0]!;
        return {
          id: t.id,
          anchor: t.anchor,
          author: first.authorName,
          presence: presenceIndex(first.authorId ?? first.authorName),
          body: first.body,
          replies: t.comments.length - 1,
          unread: unread(t),
        };
      }),
    [pinned, unread],
  );
  React.useEffect(() => feed.set(shown), [feed, shown]);

  const [focus, setFocus] = React.useState<SchemaGraphProps["focus"]>(null);
  const show = React.useCallback(
    (t: ThreadView) => {
      const a = t.anchor;
      if (a.kind === "table" || a.kind === "column") {
        if (selected !== a.table) onSelect(a.table);
        setFocus({ key: a.table });
      } else if (a.kind === "point") {
        setFocus({ point: { x: a.x, y: a.y } });
      }
      // Its statement, in the SQL (when the pane shows this text).
      if (sqlView && a.kind !== "point") {
        const text = sqlView.state.doc.toString();
        const at = placeInSql(text, a);
        if (at !== null)
          void import("@codemirror/view").then(({ EditorView }) =>
            sqlView.dispatch({ effects: EditorView.scrollIntoView(at, { y: "center" }) }),
          );
      }
    },
    [selected, onSelect, sqlView],
  );

  /**
   * "New comment": on the selected table, else comment mode (pick something;
   * on a phone the panel makes way for the graph, and comes back with the pick).
   */
  const wide = useMediaQuery("(min-width: 48rem)");
  const startNew = React.useCallback(() => {
    if (selected) ui.compose({ kind: "table", table: selected });
    else {
      ui.setOpen(wide);
      ui.setCommentMode(true);
    }
  }, [selected, ui, wide]);

  /** Comment mode on or off; on, the panel opens beside the viewer to see what comes (not over it, on a phone). */
  const setCommentMode = React.useCallback(
    (on: boolean) => {
      if (on && wide) ui.setOpen(true);
      if (on && !wide) ui.setOpen(false);
      ui.setCommentMode(on);
    },
    [ui, wide],
  );

  // C: comment mode.
  useShortcut("c", () => setCommentMode(!ui.commentMode), discussion.canComment, {
    inGraph: true,
  });

  return { outdated, unread, pinned, unreadCount, feed, focus, show, startNew, setCommentMode };
}
