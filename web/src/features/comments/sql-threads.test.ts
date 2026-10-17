import { EditorState } from "@codemirror/state";
import { EditorView, runScopeHandlers } from "@codemirror/view";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createThreadFeed, pasteWithoutComments, type SqlThread, sqlThreads } from "./sql-threads";

const SQL =
  "CREATE TABLE users (\n    id bigint,\n    email text\n);\n\nCREATE INDEX ON users (email);\n";

const views: EditorView[] = [];
afterEach(() => views.splice(0).forEach((v) => v.destroy()));

function editor(doc: string, readOnly = false) {
  const feed = createThreadFeed();
  const view = new EditorView({
    state: EditorState.create({
      doc,
      extensions: [sqlThreads(feed), EditorState.readOnly.of(readOnly)],
    }),
    parent: document.body,
  });
  views.push(view);
  return { feed, view };
}

const thread = (over: Partial<SqlThread>): SqlThread => ({
  id: "t1",
  anchor: { kind: "table", table: "public.users" },
  author: "Alice",
  presence: 3,
  body: "Why text?",
  replies: 0,
  unread: false,
  ...over,
});

const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));

function enter(view: EditorView) {
  return runScopeHandlers(view, new KeyboardEvent("keydown", { key: "Enter" }), "editor");
}

describe("threads in the SQL pane", () => {
  it("are lines above their statement, never text", async () => {
    const { feed, view } = editor(SQL);
    const open = vi.fn();
    feed.handle({ open });
    feed.set([
      thread({}),
      thread({
        id: "t2",
        anchor: { kind: "column", table: "public.users", column: "email" },
        author: "Bob",
        body: "citext?",
        replies: 2,
        unread: true,
      }),
    ]);
    await frame();
    const lines = [...view.dom.querySelectorAll<HTMLButtonElement>(".cm-thread")];
    expect(lines.map((l) => l.textContent)).toEqual([
      "--- Alice: Why text?",
      "--- Bob: citext? 2 replies",
    ]);
    expect(lines[1]!.getAttribute("aria-label")).toContain("unread");
    expect(view.state.doc.toString()).toBe(SQL);
    lines[1]!.click();
    expect(open).toHaveBeenCalledWith("t2");
  });

  it("turn a typed `---` line into a thread on Enter, on the statement below", () => {
    const doc = `--- Is this index enough?\n${SQL}`;
    const { feed, view } = editor(doc);
    const commentLine = vi.fn(() => true);
    feed.handle({ open: () => {}, commentLine });
    view.dispatch({ selection: { anchor: "--- Is this index enough?".length } });
    expect(enter(view)).toBe(true);
    expect(commentLine).toHaveBeenCalledWith(
      { from: 0, to: "--- Is this index enough?\n".length },
      "Is this index enough?",
      { kind: "table", table: "public.users" },
    );
  });

  it("turn what is before the caret into a thread when Enter splits a `---` line", () => {
    const doc = `--- Why text?CREATE TABLE users (\n    id bigint,\n    email text\n);\n`;
    const { feed, view } = editor(doc);
    const commentLine = vi.fn(() => true);
    feed.handle({ open: () => {}, commentLine });
    view.dispatch({ selection: { anchor: "--- Why text?".length } });
    expect(enter(view)).toBe(true);
    expect(commentLine).toHaveBeenCalledWith({ from: 0, to: "--- Why text?".length }, "Why text?", {
      kind: "table",
      table: "public.users",
    });
  });

  it("keep the indentation of a column line a `---` was typed into", () => {
    const doc = "CREATE TABLE users (\n    --- unique? email text\n);\n";
    const { feed, view } = editor(doc);
    const commentLine = vi.fn(() => true);
    feed.handle({ open: () => {}, commentLine });
    const at = doc.indexOf("unique?") + "unique?".length;
    view.dispatch({ selection: { anchor: at } });
    expect(enter(view)).toBe(true);
    const from = doc.indexOf("---");
    expect(commentLine).toHaveBeenCalledWith({ from, to: at + 1 }, "unique?", {
      kind: "column",
      table: "public.users",
      column: "email",
    });
  });

  it("say why a `---` line with nothing to pin it to stays", () => {
    const { feed, view } = editor("--- first things first");
    const commentLine = vi.fn(() => true);
    const notice = vi.fn();
    feed.handle({ open: () => {}, commentLine, notice });
    view.dispatch({ selection: { anchor: view.state.doc.length } });
    expect(enter(view)).toBe(false);
    expect(commentLine).not.toHaveBeenCalled();
    expect(notice).toHaveBeenCalledWith(expect.stringContaining("statement below"));
  });

  it("turn pasted `---` lines into threads on what follows them", () => {
    const { feed, view } = editor(SQL);
    const pasteComments = vi.fn(() => true);
    feed.handle({ open: () => {}, pasteComments });
    const at = SQL.indexOf("CREATE INDEX");
    view.dispatch({ selection: { anchor: at } });
    const pasted = "--- Is this enough?\nCREATE TABLE orders ();\n--- And this?\n";
    const event = new Event("paste", { bubbles: true, cancelable: true });
    Object.defineProperty(event, "clipboardData", {
      value: { getData: (type: string) => (type === "text/plain" ? pasted : "") },
    });
    view.contentDOM.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(true);
    expect(pasteComments).toHaveBeenCalledWith({ from: at, to: at }, "CREATE TABLE orders ();\n", [
      { body: "Is this enough?", anchor: { kind: "table", table: "public.orders" } },
      {
        body: "And this?",
        anchor: { kind: "table", table: "public.users", statement: "CREATE INDEX ON users" },
      },
    ]);
  });

  it("leave other lines, the middle of a line and read-only panes to Enter", () => {
    const commentLine = vi.fn(() => true);
    const plain = editor("-- a plain comment\nCREATE TABLE t ();\n");
    plain.feed.handle({ open: () => {}, commentLine });
    plain.view.dispatch({ selection: { anchor: "-- a plain comment".length } });
    expect(enter(plain.view)).toBe(false);

    const middle = editor("--- half\nCREATE TABLE t ();\n");
    middle.feed.handle({ open: () => {}, commentLine });
    middle.view.dispatch({ selection: { anchor: 4 } });
    expect(enter(middle.view)).toBe(false);

    const readOnly = editor("--- note\nCREATE TABLE t ();\n", true);
    readOnly.feed.handle({ open: () => {}, commentLine });
    readOnly.view.dispatch({ selection: { anchor: "--- note".length } });
    expect(enter(readOnly.view)).toBe(false);
    expect(commentLine).not.toHaveBeenCalled();
  });
});

describe("pasting `---` lines", () => {
  it("takes them out with their line break, wherever they are", () => {
    expect(pasteWithoutComments("", 0, 0, "a\n--- one\nb\n--- two")).toEqual({
      from: 0,
      to: 0,
      insert: "a\nb\n",
      comments: [
        { body: "one", at: 2 },
        { body: "two", at: 4 },
      ],
    });
  });

  it("counts the first line only at the start of a line, and takes its indentation", () => {
    const doc = "CREATE TABLE t (\n    \n);\n";
    const at = doc.indexOf("    ") + 4;
    expect(pasteWithoutComments(doc, at, at, "--- why?\n    id int")).toEqual({
      from: at - 4,
      to: at,
      insert: "    id int",
      comments: [{ body: "why?", at: at - 4 }],
    });
    expect(pasteWithoutComments("x = 1", 5, 5, " --- not one")).toBeNull();
  });

  it("leaves what follows the paste where it was, and plain comments alone", () => {
    expect(pasteWithoutComments("CREATE TABLE t ();", 0, 0, "--- why?")).toEqual({
      from: 0,
      to: 0,
      insert: "",
      comments: [{ body: "why?", at: 0 }],
    });
    expect(pasteWithoutComments("", 0, 0, "-- plain\n---- rule\n---\n")).toBeNull();
  });
});
