import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as Y from "yjs";

import blogJson from "@/features/schema/fixtures/blog.json";
import { MARKER } from "@/features/schema/edits/append";
import { parseSchema } from "@/features/schema/model";

import {
  CommentRefused,
  DraftSession,
  eventPatch,
  MESSAGE_COMMENT,
  MESSAGE_EVENT,
  mayEdit,
  type SessionState,
} from "./session";

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
  majorChanged: null,
  staleEdit: null,
  compile: { state: "ok", stale: false, diagnostics: [], model: null, sha256: null },
  versionCreated: null,
  versionsChanged: null,
  review: null,
  deleted: false,
  signedOut: false,
  tooLarge: false,
  tooManyTabs: false,
  outOfStep: false,
  syncStuck: false,
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

  it("keeps a rename decision only while it differs from romulus's own, never in the undo history", () => {
    const session = local();
    const key = "column/public/users/email/email_address";
    session.setRenames({ [key]: false }, { [key]: true });
    expect(session.renames.toJSON()).toEqual({ [key]: false });
    expect(session.undo.canUndo()).toBe(false);
    // Back to what romulus does by itself: no decision left.
    session.setRenames({ [key]: true }, { [key]: true });
    expect(session.renames.toJSON()).toEqual({});
    // Without defaults (a decision against nothing known), kept as given.
    session.setRenames({ [key]: true });
    expect(session.renames.toJSON()).toEqual({ [key]: true });
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

describe("comments", () => {
  const blog = parseSchema(blogJson);
  const users = { schema: "public", name: "users", kind: "r" as const };
  const me = { id: "u-alice", name: "Alice" };
  const SQL = "CREATE TABLE users (\n    id bigint,\n    email text\n);\n";
  const local = () =>
    new DraftSession("acme", "draft", {
      local: { sql: SQL, role: "developer", compile: () => ({ model: null, diagnostics: [] }) },
    });
  const threads = (session: DraftSession) =>
    Object.values(session.threads.toJSON()) as { anchor: Record<string, unknown> }[];

  it("turns a --- line into a thread in one step that one undo takes back", () => {
    const session = local();
    session.sql.insert(0, "--- why text?\n");
    session.undo.stopCapturing();
    const id = session.commentFromLine(
      { from: 0, to: "--- why text?\n".length },
      "why text?",
      { kind: "table", table: "public.users" },
      me,
    );
    expect(session.sql.toJSON()).toBe(SQL);
    expect(session.threads.get(id)).toMatchObject({ by: "u-alice", resolved: null });
    expect(Object.values(session.comments.toJSON())).toEqual([
      expect.objectContaining({ thread: id, body: "why text?", author: me }),
    ]);

    session.undo.undo();
    expect(session.sql.toJSON()).toBe(`--- why text?\n${SQL}`);
    expect(session.threads.size).toBe(0);
    expect(session.comments.size).toBe(0);
    session.destroy();
  });

  it("pastes text without its --- lines, their threads in the same undo step", () => {
    const session = local();
    session.pasteWithComments(
      { from: SQL.length, to: SQL.length },
      "CREATE TABLE orders ();\n",
      [{ body: "orders?", anchor: { kind: "table", table: "public.orders" } }],
      me,
    );
    expect(session.sql.toJSON()).toBe(`${SQL}CREATE TABLE orders ();\n`);
    expect(Object.values(session.comments.toJSON())).toEqual([
      expect.objectContaining({ body: "orders?", author: me }),
    ]);
    session.undo.undo();
    expect(session.sql.toJSON()).toBe(SQL);
    expect(session.threads.size).toBe(0);
    session.destroy();
  });

  it("never undoes a comment sent from the panel", async () => {
    const session = local();
    await session.startThread({ kind: "point", x: 0, y: 0 }, "billing goes here", [], me);
    session.undo.undo();
    expect(session.threads.size).toBe(1);
    session.destroy();
  });

  it("moves threads with a table or a column renamed from the graph, and back on undo", async () => {
    const session = local();
    await session.startThread(
      { kind: "column", table: "public.users", column: "email" },
      "unique?",
      [],
      me,
    );
    session.editSchema({ kind: "renameColumn", table: users, column: "email", to: "mail" }, blog);
    expect(threads(session)[0]!.anchor).toEqual({
      kind: "column",
      table: "public.users",
      column: "mail",
    });
    session.editSchema({ kind: "renameTable", table: users, to: "members" }, blog);
    expect(threads(session)[0]!.anchor).toEqual({
      kind: "column",
      table: "public.members",
      column: "mail",
    });
    session.undo.undo();
    session.undo.undo();
    expect(threads(session)[0]!.anchor).toEqual({
      kind: "column",
      table: "public.users",
      column: "email",
    });
    session.destroy();
  });

  it("deletes a whole thread here too, without a server", async () => {
    const session = local();
    const id = await session.startThread({ kind: "table", table: "public.users" }, "hm", [], me);
    await session.reply(id, "more", [], me);
    await session.deleteThread(id, me);
    expect(session.threads.size).toBe(0);
    expect(session.comments.size).toBe(0);
    session.destroy();
  });

  it("deletes a thread with its last comment, its author's own", async () => {
    const session = local();
    const id = await session.startThread({ kind: "table", table: "public.users" }, "hm", [], me);
    const comment = [...session.comments.keys()][0]!;
    await session.deleteComment(comment, me);
    expect(session.threads.has(id)).toBe(false);
    session.destroy();
  });
});

describe("comments with a server", () => {
  /** A socket that keeps what is sent and lets the test answer. */
  class FakeSocket {
    static last: FakeSocket | null = null;
    readonly OPEN = 1;
    readyState = 0;
    binaryType = "";
    sent: Uint8Array[] = [];
    onopen: (() => void) | null = null;
    onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
    onclose: ((event: { code: number; reason: string }) => void) | null = null;
    onerror: (() => void) | null = null;
    constructor() {
      FakeSocket.last = this;
    }
    send(data: Uint8Array) {
      this.sent.push(data);
    }
    close() {}
    open() {
      this.readyState = 1;
      this.onopen?.();
    }
    event(event: object) {
      const encoder = encoding.createEncoder();
      encoding.writeVarUint(encoder, MESSAGE_EVENT);
      encoding.writeVarUint8Array(encoder, new TextEncoder().encode(JSON.stringify(event)));
      this.onmessage?.({ data: encoding.toUint8Array(encoder).buffer });
    }
    /** The comment operations sent so far. */
    operations() {
      return this.sent.flatMap((bytes) => {
        const decoder = decoding.createDecoder(bytes);
        if (decoding.readVarUint(decoder) !== MESSAGE_COMMENT) return [];
        const body = decoding.readVarUint8Array(decoder);
        return [JSON.parse(new TextDecoder().decode(body)) as { id: string; op: unknown }];
      });
    }
  }

  afterEach(() => vi.unstubAllGlobals());

  function connected() {
    vi.stubGlobal("WebSocket", FakeSocket);
    const session = new DraftSession("acme", "draft", { origin: "ws://romulus.test" });
    const socket = FakeSocket.last!;
    socket.open();
    return { session, socket };
  }
  const me = { id: "u-bob", name: "Bob" };

  it("asks the server to write a reply, and never writes the doc itself", async () => {
    const { session, socket } = connected();
    const reply = session.reply("t1", "Yes", ["u-alice"], me);
    const [sent] = socket.operations();
    expect(sent!.op).toMatchObject({
      type: "reply",
      thread: "t1",
      body: "Yes",
      mentions: ["u-alice"],
    });
    expect(session.comments.size).toBe(0);
    socket.event({ type: "comment_result", id: sent!.id, ok: true });
    await expect(reply).resolves.toBeUndefined();
    session.destroy();
  });

  it("says why the server refused an operation", async () => {
    const { session, socket } = connected();
    const removal = session.deleteComment("c1", me);
    const [sent] = socket.operations();
    socket.event({
      type: "comment_result",
      id: sent!.id,
      ok: false,
      code: "not_yours",
      message: "Only its author can delete a comment.",
    });
    await expect(removal).rejects.toEqual(
      new CommentRefused("not_yours", "Only its author can delete a comment."),
    );
    session.destroy();
  });

  it("refuses at once while offline, and lets waiting ones go when the socket drops", async () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const session = new DraftSession("acme", "draft", { origin: "ws://romulus.test" });
    await expect(session.setResolved("t1", true, me)).rejects.toMatchObject({ code: "offline" });
    const socket = FakeSocket.last!;
    socket.open();
    const resolving = session.setResolved("t1", true, me);
    socket.onclose?.({ code: 1006, reason: "" });
    await expect(resolving).rejects.toMatchObject({ code: "disconnected" });
    session.destroy();
  });

  it("asks the server to delete a whole thread", async () => {
    const { session, socket } = connected();
    const removal = session.deleteThread("t1", me);
    const [sent] = socket.operations();
    expect(sent!.op).toEqual({ type: "delete_thread", thread: "t1" });
    socket.event({ type: "comment_result", id: sent!.id, ok: true });
    await expect(removal).resolves.toBeUndefined();
    session.destroy();
  });

  it("stops reconnecting after a few refusals to sync, says so, and tries again on demand", async () => {
    const { session, socket } = connected();
    let current = socket;
    for (let i = 0; i < 3; i++) {
      current.onclose?.({ code: 4003, reason: "sync again" });
      if (i === 2) break;
      const closed = current;
      await vi.waitFor(() => expect(FakeSocket.last).not.toBe(closed), { timeout: 3000 });
      current = FakeSocket.last!;
      current.open();
      expect(session.getState().syncStuck).toBe(false);
    }
    expect(session.getState().syncStuck).toBe(true);
    expect(session.getState().connection).toBe("stopped");
    const last = FakeSocket.last;
    await new Promise((resolve) => setTimeout(resolve, 1000));
    expect(FakeSocket.last).toBe(last);
    session.retrySync();
    await vi.waitFor(() => expect(FakeSocket.last).not.toBe(last), { timeout: 3000 });
    expect(session.getState().syncStuck).toBe(false);
    session.destroy();
  });

  it("knows when the server let this tab go, out of step", () => {
    const { session, socket } = connected();
    socket.onclose?.({ code: 4409, reason: "this tab wrote as someone else: reload" });
    expect(session.getState().outOfStep).toBe(true);
    session.destroy();
  });

  it("knows the draft is open in too many tabs", () => {
    const { session, socket } = connected();
    socket.onclose?.({ code: 4429, reason: "too many tabs" });
    expect(session.getState().tooManyTabs).toBe(true);
    session.destroy();
  });
});
