import { describe, expect, it } from "vitest";
import * as Y from "yjs";

import blogJson from "@/features/schema/fixtures/blog.json";
import { MARKER } from "@/features/schema/edits/append";
import { parseSchema } from "@/features/schema/model";

import { DraftSession, eventPatch, mayEdit, type SessionState } from "./session";

const initial: SessionState = {
  connection: "connected",
  synced: true,
  role: null,
  status: "open",
  statusBy: null,
  publishedVersion: null,
  name: null,
  compile: { state: "ok", stale: false, diagnostics: [], model: null },
  versionCreated: null,
  deleted: false,
  signedOut: false,
  tooLarge: false,
  saveFailed: false,
  updated: null,
  unsynced: false,
  edits: [],
  historyCleared: null,
};

const apply = (state: SessionState, ...events: Parameters<typeof eventPatch>[1][]) =>
  events.reduce((s, e) => ({ ...s, ...eventPatch(s, e) }), state);

describe("a draft session's state", () => {
  it("lets someone who joined during a publish edit once it failed", () => {
    const joined = apply(initial, {
      type: "welcome",
      role: "developer",
      read_only: true,
      status: "publishing",
    });
    expect(mayEdit(joined)).toBe(false);
    const reopened = apply(joined, { type: "status", status: "open", by: null, version: null });
    expect(mayEdit(reopened)).toBe(true);
  });

  it("never lets a reviewer edit, open or not", () => {
    const reviewer = apply(initial, {
      type: "welcome",
      role: "reviewer",
      read_only: true,
      status: "open",
    });
    expect(mayEdit(reviewer)).toBe(false);
  });

  it("closes for good when published, and keeps who did it", () => {
    const published = apply(
      initial,
      { type: "welcome", role: "admin", read_only: false, status: "open" },
      { type: "status", status: "published", by: "Alice", version: 4 },
    );
    expect(mayEdit(published)).toBe(false);
    expect(published).toMatchObject({ statusBy: "Alice", publishedVersion: 4 });
  });

  it("is read-only while someone updates it, and says who did", () => {
    const updating = apply(
      initial,
      { type: "welcome", role: "developer", read_only: false, status: "open" },
      { type: "status", status: "updating", by: "Bob", version: null },
    );
    expect(mayEdit(updating)).toBe(false);
    const done = apply(
      updating,
      { type: "updated", version: 3, by: "Bob", conflicts: 2 },
      { type: "status", status: "open", by: null, version: null },
    );
    expect(mayEdit(done)).toBe(true);
    expect(done.updated).toEqual({ version: 3, by: "Bob", conflicts: 2 });
  });

  it("says when the server could not save", () => {
    expect(apply(initial, { type: "save_failed" }).saveFailed).toBe(true);
  });
});

describe("schema edits from the graph", () => {
  const blog = parseSchema(blogJson);
  const users = { schema: "public", name: "users", kind: "r" as const };
  const local = () =>
    new DraftSession("acme", "draft", {
      local: {
        sql: "CREATE TABLE users (id bigint);\n",
        layout: { "public.users": { x: 10, y: 20 } },
        groups: { g: { name: "People", color: "blue", members: ["public.users", "public.x"] } },
        role: "developer",
        compile: () => ({ model: null, diagnostics: [] }),
      },
    });

  it("append their SQL and move the layout in one step that one undo takes back", () => {
    const session = local();
    session.editSchema({ kind: "renameTable", table: users, to: "members" }, blog);
    expect(session.sql.toJSON()).toBe(
      `CREATE TABLE users (id bigint);\n\n${MARKER}\nALTER TABLE public.users RENAME TO members;\n`,
    );
    expect(session.layout.toJSON()).toEqual({ "public.members": { x: 10, y: 20 } });
    expect(session.groups.get("g")?.members).toEqual(["public.members", "public.x"]);
    expect(session.getState().edits).toHaveLength(1);

    session.undo.undo();
    expect(session.sql.toJSON()).toBe("CREATE TABLE users (id bigint);\n");
    expect(session.layout.toJSON()).toEqual({ "public.users": { x: 10, y: 20 } });
    expect(session.groups.get("g")?.members).toEqual(["public.users", "public.x"]);
    session.undo.redo();
    expect(session.sql.toJSON()).toContain("RENAME TO members");
    session.destroy();
  });

  it("each edit is its own step, and a new table is placed with it", () => {
    const session = local();
    session.editSchema({ kind: "addTable", table: { schema: "public", name: "invoices" } }, blog, {
      x: 100.4,
      y: 200.6,
    });
    session.editSchema({ kind: "setComment", table: users, comment: "People" }, blog);
    expect(session.layout.get("public.invoices")).toEqual({ x: 100, y: 201 });
    session.undo.undo();
    expect(session.sql.toJSON()).toContain("CREATE TABLE public.invoices");
    expect(session.sql.toJSON()).not.toContain("COMMENT ON");
    session.undo.undo();
    expect(session.sql.toJSON()).not.toContain("invoices");
    expect(session.layout.has("public.invoices")).toBe(false);
    session.destroy();
  });

  it("an edit that is not the last one is taken back on its own", () => {
    const session = local();
    const rename = session.editSchema({ kind: "renameTable", table: users, to: "members" }, blog);
    session.editSchema({ kind: "setComment", table: users, comment: "x" }, blog);
    session.undoSchemaEdit(rename.id);
    expect(session.sql.toJSON()).not.toContain("RENAME");
    expect(session.sql.toJSON()).toContain("COMMENT ON TABLE public.users IS 'x';");
    expect(session.layout.toJSON()).toEqual({ "public.users": { x: 10, y: 20 } });
    // As a step of its own: undo brings it back.
    session.undo.undo();
    expect(session.sql.toJSON()).toContain("RENAME TO members");
    session.destroy();
  });
});

describe("a rewrite by the server (Tidy, Update draft)", () => {
  const blog = parseSchema(blogJson);
  const users = { schema: "public", name: "users", kind: "r" as const };

  /** A session and the server's copy of its doc, kept in sync by hand. */
  function setup() {
    const session = new DraftSession("acme", "draft", {
      local: {
        sql: "CREATE TABLE users (id bigint, x bigint);\n",
        layout: { "public.users": { x: 10, y: 20 } },
        role: "developer",
        compile: () => ({ model: null, diagnostics: [] }),
      },
    });
    const server = new Y.Doc();
    const toServer = () => Y.applyUpdate(server, Y.encodeStateAsUpdate(session.doc));
    const toClient = () =>
      Y.applyUpdate(session.doc, Y.encodeStateAsUpdate(server, Y.encodeStateVector(session.doc)));
    return { session, server, toServer, toClient };
  }

  /**
   * What the room does (collab::room `rewrite`): edits the text keeping the
   * characters it can (here: inserts into the client's own statement) and
   * notes the rewrite in `meta`, in one transaction.
   */
  function rewrite(server: Y.Doc, edit: (text: Y.Text, sql: string) => void) {
    const text = server.getText("sql");
    const meta = server.getMap<{ count: number; reason: string }>("meta");
    server.transact(() => {
      edit(text, text.toJSON());
      meta.set("rewrite", { count: (meta.get("rewrite")?.count ?? 0) + 1, reason: "tidy" });
    });
  }

  it("leaves nothing to undo that predates it, so undo cannot corrupt the rewritten text", () => {
    const { session, server, toServer, toClient } = setup();
    session.editSchema(
      {
        kind: "addForeignKey",
        table: users,
        column: "x",
        references: { schema: "public", name: "users" },
        refColumn: "id",
        name: "users_x_fkey",
        onDelete: "a",
      },
      blog,
    );
    session.editSchema({ kind: "renameTable", table: users, to: "members" }, blog);
    toServer();
    // The rewrite keeps the client's characters, in another place and order.
    rewrite(server, (text, sql) => {
      const at = sql.indexOf("ALTER TABLE public.users ADD");
      text.insert(at + "ALTER TABLE".length, " ONLY");
      text.delete(0, sql.indexOf("\n") + 1);
    });
    toClient();
    const rewritten = session.sql.toJSON();
    const layout = session.layout.toJSON();

    expect(session.undo.undoStack).toHaveLength(0);
    expect(session.undo.redoStack).toHaveLength(0);
    session.undo.undo();
    session.undo.undo();
    expect(session.sql.toJSON()).toBe(rewritten);
    expect(session.layout.toJSON()).toEqual(layout);
    expect(layout).toEqual({ "public.members": { x: 10, y: 20 } });
    // Its edits are folded into the text: nothing left to judge.
    expect(session.getState().edits).toEqual([]);
    expect(session.getState().historyCleared).toMatchObject({ reason: "tidy", mine: false });
    session.destroy();
  });

  it("says nothing when there was no history to clear, and new edits undo again", () => {
    const { session, server, toServer, toClient } = setup();
    toServer();
    rewrite(server, (text) => text.insert(0, "-- tidy\n"));
    toClient();
    expect(session.getState().historyCleared).toBeNull();
    session.editSchema({ kind: "setComment", table: users, comment: "People" }, blog);
    session.undo.undo();
    expect(session.sql.toJSON()).toBe("-- tidy\nCREATE TABLE users (id bigint, x bigint);\n");
    session.destroy();
  });

  it("knows the rewrite it asked for", () => {
    const { session, server, toServer, toClient } = setup();
    session.editSchema({ kind: "setComment", table: users, comment: "People" }, blog);
    toServer();
    session.expectRewrite();
    rewrite(server, (text) => text.insert(0, "-- tidy\n"));
    toClient();
    expect(session.getState().historyCleared).toMatchObject({ mine: true });
    session.destroy();
  });
});
