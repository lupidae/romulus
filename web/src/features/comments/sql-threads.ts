// Comment threads in the SQL pane (docs/MVP.md "Review round 1", item 6):
// each open thread is a read-only `--- alice: …` line above the statement
// (or column) it is pinned to, drawn by CodeMirror (block widgets from a
// StateField): never text in the document, so the SQL stays pure. In an
// editable pane, a line typed as `--- words` becomes a thread on Enter (at
// its end, or splitting it after the words), and pasted `---` lines become
// threads: the lines leave the text in the same transaction (one undo
// step). The server refuses a source that keeps one (`comment_line`).

import { type EditorState, Prec, type Range, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  keymap,
  ViewPlugin,
  WidgetType,
} from "@codemirror/view";

import { findStatements } from "@/features/schema/text/statements";

import { type Anchor, anchorBelow, commentLine, placeInSql } from "./model";

/** A thread as the SQL pane shows it. */
export interface SqlThread {
  id: string;
  anchor: Anchor;
  author: string;
  /** Their collaborator color, 1–8. */
  presence: number;
  /** The first comment. */
  body: string;
  replies: number;
  unread: boolean;
}

/**
 * Where the pane reads its threads from, and what clicking one or typing a
 * `---` line does. Made once per pane; its content changes underneath.
 */
export interface ThreadFeed {
  get(): readonly SqlThread[];
  subscribe(listener: () => void): () => void;
  /** Opens a thread (in the comments panel). */
  open(id: string): void;
  /**
   * A `---` line typed: make it a thread. `range` is the line with its line
   * break, `anchor` what it is pinned to. Absent: the pane only shows.
   * Returns false when it could not (the line stays, as SQL).
   */
  commentLine?: (range: { from: number; to: number }, body: string, anchor: Anchor) => boolean;
  /**
   * Text pasted with `---` lines: `insert` (the text without them) replaces
   * `range`, each comment a thread. Absent or false: pasted as it is.
   */
  pasteComments?: (
    range: { from: number; to: number },
    insert: string,
    comments: { body: string; anchor: Anchor }[],
  ) => boolean;
  /** Says why a `---` line could not become a thread. */
  notice?: (message: string) => void;
}

interface FeedHandlers {
  open: (id: string) => void;
  commentLine?: ThreadFeed["commentLine"];
  pasteComments?: ThreadFeed["pasteComments"];
  notice?: ThreadFeed["notice"];
}

/**
 * A feed whose threads React sets (`set`), and what opening one or typing a
 * `---` line does (`handle`, the latest handlers: the editor keeps the feed).
 */
export function createThreadFeed(): ThreadFeed & {
  set(threads: readonly SqlThread[]): void;
  handle(handlers: FeedHandlers): void;
} {
  let threads: readonly SqlThread[] = [];
  let handlers: FeedHandlers = { open: () => {} };
  const listeners = new Set<() => void>();
  return {
    handle(next) {
      handlers = next;
    },
    get: () => threads,
    set(next) {
      threads = next;
      listeners.forEach((l) => l());
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    open: (id) => handlers.open(id),
    commentLine: (...args) => handlers.commentLine?.(...args) ?? false,
    pasteComments: (...args) => handlers.pasteComments?.(...args) ?? false,
    notice: (message) => handlers.notice?.(message),
  };
}

/** Nothing below or above a `---` line to pin its thread to. */
const NOTHING_TO_PIN =
  "A --- comment is pinned to the statement below it: write one first, then post the comment.";

/**
 * Pasting `text` over `from`–`to` of `doc`, without its `---` lines: each
 * becomes a comment found `at` (an offset in the text once pasted). Lines
 * are `---` lines as they will stand: the first only if nothing but
 * indentation is before it (which then goes with it), the last without
 * what follows the paste. Null: no `---` line in it.
 */
export function pasteWithoutComments(doc: string, from: number, to: number, text: string) {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const lineStart = from === 0 ? 0 : doc.lastIndexOf("\n", from - 1) + 1;
  const indented = doc.slice(lineStart, from).trim() === "";
  let start = from;
  let insert = "";
  const comments: { body: string; at: number }[] = [];
  lines.forEach((line, i) => {
    const last = i === lines.length - 1;
    const body = i === 0 && !indented ? null : commentLine(line);
    if (!body) {
      insert += last ? line : `${line}\n`;
      return;
    }
    if (i === 0 && !last) start = lineStart;
    comments.push({ body, at: start + insert.length });
  });
  return comments.length === 0 ? null : { from: start, to, insert, comments };
}

const setThreads = StateEffect.define<readonly SqlThread[]>();

interface Shown {
  threads: readonly SqlThread[];
  decorations: DecorationSet;
}

const MAX_BODY = 140;

class ThreadLines extends WidgetType {
  readonly threads: readonly SqlThread[];
  readonly indent: number;
  readonly feed: ThreadFeed;

  constructor(threads: readonly SqlThread[], indent: number, feed: ThreadFeed) {
    super();
    this.threads = threads;
    this.indent = indent;
    this.feed = feed;
  }

  override eq(other: ThreadLines) {
    return (
      other.indent === this.indent &&
      other.threads.length === this.threads.length &&
      other.threads.every((t, i) => {
        const mine = this.threads[i]!;
        return (
          t.id === mine.id &&
          t.body === mine.body &&
          t.author === mine.author &&
          t.replies === mine.replies &&
          t.unread === mine.unread
        );
      })
    );
  }

  override toDOM() {
    const box = document.createElement("div");
    box.className = "cm-threads";
    box.setAttribute("role", "group");
    box.setAttribute("aria-label", this.threads.length === 1 ? "Comment" : "Comments");
    box.style.marginLeft = `${this.indent}ch`;
    for (const t of this.threads) {
      const line = document.createElement("button");
      line.type = "button";
      line.className = "cm-thread";
      line.dataset.thread = t.id;
      const replies =
        t.replies === 0 ? "" : t.replies === 1 ? ", 1 reply" : `, ${t.replies} replies`;
      line.setAttribute(
        "aria-label",
        `Comment by ${t.author}${t.unread ? ", unread" : ""}: ${t.body}${replies}. Open the thread`,
      );
      const dashes = document.createElement("span");
      dashes.className = "cm-thread-dashes";
      dashes.textContent = "---";
      if (t.unread) dashes.classList.add("cm-thread-unread");
      const author = document.createElement("span");
      author.className = `cm-thread-author cm-presence-${t.presence}`;
      author.textContent = `${t.author}:`;
      const body = document.createElement("span");
      body.className = "cm-thread-body";
      const oneLine = t.body.replace(/\s+/g, " ");
      body.textContent = oneLine.length > MAX_BODY ? `${oneLine.slice(0, MAX_BODY - 1)}…` : oneLine;
      line.append(dashes, " ", author, " ", body);
      if (t.replies > 0) {
        const more = document.createElement("span");
        more.className = "cm-thread-replies";
        more.textContent = t.replies === 1 ? "1 reply" : `${t.replies} replies`;
        line.append(" ", more);
      }
      line.addEventListener("click", () => this.feed.open(t.id));
      box.append(line);
    }
    return box;
  }

  override ignoreEvent() {
    return true;
  }
}

/** The widgets for these threads in this text: one block above each line that has some. */
function decorate(state: EditorState, threads: readonly SqlThread[], feed: ThreadFeed) {
  if (threads.length === 0) return Decoration.none;
  const text = state.doc.toString();
  const statements = findStatements(text);
  const byLine = new Map<number, SqlThread[]>();
  for (const thread of threads) {
    const at = placeInSql(text, thread.anchor, statements);
    if (at === null) continue;
    byLine.set(at, [...(byLine.get(at) ?? []), thread]);
  }
  const ranges: Range<Decoration>[] = [];
  for (const [at, list] of [...byLine].sort((a, b) => a[0] - b[0])) {
    const line = state.doc.lineAt(Math.min(at, state.doc.length));
    const indent = /^\s*/.exec(line.text)![0].replace(/\t/g, "    ").length;
    ranges.push(
      Decoration.widget({
        widget: new ThreadLines(list, indent, feed),
        block: true,
        side: -1,
      }).range(line.from),
    );
  }
  return Decoration.set(ranges, true);
}

const theme = EditorView.baseTheme({
  ".cm-threads": {
    display: "flex",
    flexDirection: "column",
    // One line each, cut with an ellipsis: never wider than the pane (a
    // long comment would otherwise widen the whole editor).
    contain: "inline-size",
    fontFamily: "var(--font-mono)",
    fontSize: "var(--text-sm)",
    lineHeight: "1.25rem",
    padding: "0.125rem 1rem 0.125rem 0.75rem",
  },
  ".cm-thread": {
    display: "block",
    maxWidth: "100%",
    margin: "0",
    padding: "0 0.25rem",
    marginLeft: "-0.25rem",
    border: "none",
    borderRadius: "0.25rem",
    background: "transparent",
    color: "var(--color-muted-foreground)",
    font: "inherit",
    textAlign: "left",
    cursor: "pointer",
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  ".cm-thread:hover": {
    backgroundColor: "color-mix(in oklab, var(--color-primary) 8%, transparent)",
    color: "var(--color-foreground)",
  },
  ".cm-thread:focus-visible": {
    outline: "none",
    boxShadow: "0 0 0 2px var(--color-ring)",
  },
  ".cm-thread-dashes": { color: "var(--color-faint-foreground)" },
  ".cm-thread-unread": { color: "var(--color-primary)", fontWeight: "700" },
  ".cm-thread-author": { fontWeight: "600" },
  ".cm-thread-replies": { color: "var(--color-faint-foreground)" },
  ".cm-presence-1": { color: "var(--color-presence-1)" },
  ".cm-presence-2": { color: "var(--color-presence-2)" },
  ".cm-presence-3": { color: "var(--color-presence-3)" },
  ".cm-presence-4": { color: "var(--color-presence-4)" },
  ".cm-presence-5": { color: "var(--color-presence-5)" },
  ".cm-presence-6": { color: "var(--color-presence-6)" },
  ".cm-presence-7": { color: "var(--color-presence-7)" },
  ".cm-presence-8": { color: "var(--color-presence-8)" },
});

/**
 * Threads shown above their statements, from `feed`; with `feed.commentLine`,
 * `---` lines typed become threads (Enter at the end of one).
 */
export function sqlThreads(feed: ThreadFeed) {
  const field = StateField.define<Shown>({
    create: (state) => ({ threads: feed.get(), decorations: decorate(state, feed.get(), feed) }),
    update(shown, tr) {
      let threads = shown.threads;
      for (const effect of tr.effects) if (effect.is(setThreads)) threads = effect.value;
      if (threads === shown.threads && !tr.docChanged) return shown;
      return { threads, decorations: decorate(tr.state, threads, feed) };
    },
    provide: (f) => EditorView.decorations.from(f, (shown) => shown.decorations),
  });

  // The feed changes outside the editor: hand it in, a frame later at most.
  const listen = ViewPlugin.define((view) => {
    let frame = 0;
    const unsubscribe = feed.subscribe(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (view.dom.isConnected) view.dispatch({ effects: setThreads.of(feed.get()) });
      });
    });
    return {
      destroy() {
        cancelAnimationFrame(frame);
        unsubscribe();
      },
    };
  });

  const enter = Prec.highest(
    keymap.of([
      {
        key: "Enter",
        run(view) {
          if (view.state.readOnly || !feed.commentLine) return false;
          const { main } = view.state.selection;
          if (!main.empty || view.state.selection.ranges.length > 1) return false;
          const doc = view.state.doc;
          const line = doc.lineAt(main.head);
          // What is before the caret is the comment; the rest of the line stays.
          const before = doc.sliceString(line.from, main.head);
          const body = commentLine(before);
          if (!body) return false;
          const after = doc.sliceString(main.head, line.to);
          const range =
            after.trim() !== ""
              ? {
                  from: line.from + (/^\s*/.exec(before)?.[0].length ?? 0),
                  to: main.head + (/^\s*/.exec(after)?.[0].length ?? 0),
                }
              : // The whole line, with its line break (the one before it, at the end).
                line.to < doc.length
                ? { from: line.from, to: line.to + 1 }
                : { from: Math.max(0, line.from - 1), to: line.to };
          const text = doc.toString();
          const rest = text.slice(0, range.from) + text.slice(range.to);
          const anchor = anchorBelow(rest, range.from);
          if (!anchor) {
            feed.notice?.(NOTHING_TO_PIN);
            return false;
          }
          return feed.commentLine(range, body, anchor);
        },
      },
    ]),
  );

  const paste = EditorView.domEventHandlers({
    paste(event, view) {
      if (view.state.readOnly || !feed.pasteComments) return false;
      const text = event.clipboardData?.getData("text/plain");
      if (!text || view.state.selection.ranges.length > 1) return false;
      const { from, to } = view.state.selection.main;
      const doc = view.state.doc.toString();
      const pasted = pasteWithoutComments(doc, from, to, text);
      if (!pasted) return false;
      const result = doc.slice(0, pasted.from) + pasted.insert + doc.slice(pasted.to);
      const comments = pasted.comments.map(({ body, at }) => ({
        body,
        anchor: anchorBelow(result, at),
      }));
      if (comments.some((c) => !c.anchor)) {
        feed.notice?.(NOTHING_TO_PIN);
        return false;
      }
      const done = feed.pasteComments(
        { from: pasted.from, to: pasted.to },
        pasted.insert,
        comments as { body: string; anchor: Anchor }[],
      );
      if (!done) return false;
      event.preventDefault();
      const caret = Math.min(pasted.from + pasted.insert.length, view.state.doc.length);
      view.dispatch({ selection: { anchor: caret }, scrollIntoView: true, userEvent: "select" });
      return true;
    },
  });

  return [field, listen, enter, paste, theme];
}
