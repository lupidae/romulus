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
  mode: "schema",
  schemaSql: null,
  modeChanged: null,
  staleEdit: null,
  compile: { state: "ok", stale: false, diagnostics: [], model: null, sha256: null },
  versionCreated: null,
  versionsChanged: null,
  review: null,
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

describe("groups", () => {
  const local = () =>
    new DraftSession("acme", "draft", {
      local: {
        sql: "",
        layout: {
          "public.users": { x: 0, y: 0 },
          "public.sessions": { x: 300, y: 0 },
          "public.orders": { x: 0, y: 400 },
        },
        groups: {
          g: { name: "People", color: "blue", members: ["public.users", "public.sessions"] },
        },
        role: "developer",
        compile: () => ({ model: null, diagnostics: [] }),
      },
    });

  it("move with all their tables in one transaction that one undo takes back", () => {
    const session = local();
    // What a collaborator's doc receives: the whole move in one update.
    const peer = new Y.Doc();
    Y.applyUpdate(peer, Y.encodeStateAsUpdate(session.doc));
    const updates: Uint8Array[] = [];
    session.doc.on("update", (update: Uint8Array) => updates.push(update));

    session.placeTables({
      "public.users": { x: 50.4, y: 80 },
      "public.sessions": { x: 350, y: 80 },
    });
    expect(updates).toHaveLength(1);
    Y.applyUpdate(peer, updates[0]!);
    expect(peer.getMap("layout").toJSON()).toMatchObject({
      "public.users": { x: 50, y: 80 },
      "public.sessions": { x: 350, y: 80 },
    });

    session.undo.undo();
    expect(session.layout.get("public.users")).toEqual({ x: 0, y: 0 });
    expect(session.layout.get("public.sessions")).toEqual({ x: 300, y: 0 });
    session.destroy();
  });

  it("take in a table dropped on them, with its move, as one step", () => {
    const session = local();
    session.placeTables(
      { "public.orders": { x: 100, y: 20 } },
      { tables: ["public.orders"], group: "g" },
    );
    expect(session.groups.get("g")?.members).toContain("public.orders");
    session.undo.undo();
    expect(session.groups.get("g")?.members).not.toContain("public.orders");
    expect(session.layout.get("public.orders")).toEqual({ x: 0, y: 400 });
    session.destroy();
  });

  it("are made, renamed, recolored and deleted one undo step at a time", () => {
    const session = local();
    const id = session.createGroup(["public.orders", "public.users"]);
    expect(session.groups.get(id)).toEqual({
      name: "Group 2",
      color: "teal",
      members: ["public.orders", "public.users"],
    });
    // A table belongs to one group.
    expect(session.groups.get("g")?.members).toEqual(["public.sessions"]);
    session.updateGroup(id, { name: "Shop" });
    session.updateGroup(id, { color: "amber" });
    session.deleteGroup(id);
    expect(session.groups.has(id)).toBe(false);

    session.undo.undo();
    expect(session.groups.get(id)).toMatchObject({ name: "Shop", color: "amber" });
    session.undo.undo();
    expect(session.groups.get(id)?.color).toBe("teal");
    session.undo.undo();
    expect(session.groups.get(id)?.name).toBe("Group 2");
    session.undo.undo();
    expect(session.groups.has(id)).toBe(false);
    expect(session.groups.get("g")?.members).toEqual(["public.users", "public.sessions"]);
    session.destroy();
  });

  it("go when their last table leaves, and come back with an undo", () => {
    const session = local();
    session.assignGroup(["public.users", "public.sessions"], null);
    expect(session.groups.has("g")).toBe(false);
    session.undo.undo();
    expect(session.groups.get("g")?.members).toEqual(["public.users", "public.sessions"]);
    session.destroy();
  });
});

describe("migration mode", () => {
  const blog = parseSchema(blogJson);
  const users = { schema: "public", name: "users", kind: "r" as const };

  it("appends graph edits to the migration, the SQL left alone", () => {
    const session = new DraftSession("acme", "draft", {
      local: {
        sql: "CREATE TABLE users (id bigint);\n",
        migration: "-- acme/shop: migration from v1, written by hand.\n",
        role: "developer",
        compile: () => ({ model: null, diagnostics: [] }),
      },
    });
    expect(session.getState().mode).toBe("migration");
    expect(session.source()).toBe(session.migration);
    session.editSchema({ kind: "renameTable", table: users, to: "members" }, blog);
    expect(session.migration.toJSON()).toBe(
      `-- acme/shop: migration from v1, written by hand.\n\n${MARKER}\nALTER TABLE public.users RENAME TO members;\n`,
    );
    expect(session.sql.toJSON()).toBe("CREATE TABLE users (id bigint);\n");
    // One undo takes it back, in the migration too.
    session.undo.undo();
    expect(session.migration.toJSON()).toBe("-- acme/shop: migration from v1, written by hand.\n");
    session.destroy();
  });

  it("follows the server's switch: the other text is the source, the history gone", () => {
    const session = new DraftSession("acme", "draft", {
      local: {
        sql: "CREATE TABLE users (id bigint);\n",
        role: "developer",
        compile: () => ({ model: null, diagnostics: [] }),
      },
    });
    session.editSchema({ kind: "setComment", table: users, comment: "People" }, blog);
    expect(session.undo.undoStack).toHaveLength(1);
    // The room switches: the migration seeded and the mode noted, together.
    const server = new Y.Doc();
    Y.applyUpdate(server, Y.encodeStateAsUpdate(session.doc));
    server.transact(() => {
      server.getText("migration").insert(0, "ALTER TABLE public.users ADD x int;\n");
      server.getMap("meta").set("mode", "migration");
    });
    Y.applyUpdate(session.doc, Y.encodeStateAsUpdate(server, Y.encodeStateVector(session.doc)));
    expect(session.getState().mode).toBe("migration");
    expect(session.source().toJSON()).toBe("ALTER TABLE public.users ADD x int;\n");
    expect(session.undo.undoStack).toHaveLength(0);
    expect(session.getState().edits).toEqual([]);
    // Nobody's "Undo history was cleared": the switch says it.
    expect(session.getState().historyCleared).toBeNull();
    session.destroy();
  });

  it("knows who switched modes", () => {
    const patched = apply(initial, { type: "mode", mode: "migration", by: "Alice" });
    expect(patched.modeChanged).toEqual({ mode: "migration", by: "Alice" });
  });

  it("hears when an edit reached the text that is not the source", () => {
    const patched = apply(initial, { type: "stale_edit", mode: "migration" });
    expect(patched.staleEdit).toMatchObject({ mode: "migration" });
  });
});
