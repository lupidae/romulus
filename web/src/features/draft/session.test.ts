import { describe, expect, it } from "vitest";

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
  unsynced: false,
  edits: [],
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
