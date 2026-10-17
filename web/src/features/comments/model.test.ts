import { describe, expect, it } from "vitest";

import { matchPeople, mentionQuery } from "@/components/ui/mention-textarea";
import type { Schema } from "@/features/schema/model";

import {
  anchorBelow,
  anchorExists,
  anchorLabel,
  bodyParts,
  commentLine,
  isUnread,
  mentionsIn,
  placeInSql,
  renameColumn,
  renameTable,
  threadsOf,
  type ThreadView,
} from "./model";

const SQL = `CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL,
    "Display Name" text
);

CREATE INDEX users_email_idx ON users (email);
COMMENT ON COLUMN users.email IS 'Where we write';
CREATE TYPE mood AS ENUM ('ok', 'sad');
`;

const schema: Schema = {
  remus_version: "0.2",
  database: "shop",
  server_version: "17",
  entities: [
    {
      schema: "public",
      name: "users",
      kind: "r",
      comment: null,
      partition_key: null,
      view_definition: null,
      columns: [
        {
          name: "email",
          type: "text",
          type_schema: "pg_catalog",
          type_name: "text",
          type_kind: "b",
          default: null,
          identity: null,
          generated: null,
          comment: null,
        },
      ],
    },
  ],
};

describe("`---` lines", () => {
  it("are three dashes then words; separators and four dashes are SQL", () => {
    expect(commentLine("--- why text?")).toBe("why text?");
    expect(commentLine("   ---why")).toBe("why");
    expect(commentLine("---")).toBeNull();
    expect(commentLine("---------")).toBeNull();
    expect(commentLine("---- note")).toBeNull();
    expect(commentLine("-- a plain comment")).toBeNull();
    expect(commentLine("SELECT 1; --- trailing")).toBeNull();
  });

  it("attach to the statement below them", () => {
    const at = (line: string) => SQL.indexOf(line);
    expect(anchorBelow(SQL, 0)).toEqual({ kind: "table", table: "public.users" });
    expect(anchorBelow(SQL, at("CREATE INDEX"))).toEqual({
      kind: "table",
      table: "public.users",
      statement: "CREATE INDEX users_email_idx ON users",
    });
    expect(anchorBelow(SQL, at("COMMENT ON"))).toEqual({
      kind: "column",
      table: "public.users",
      column: "email",
      statement: "COMMENT ON COLUMN users.email IS 'Where we write'",
    });
    expect(anchorBelow(SQL, at("CREATE TYPE"))).toEqual({
      kind: "statement",
      statement: "CREATE TYPE mood AS ENUM",
    });
  });

  it("inside a CREATE TABLE attach to the column below", () => {
    expect(anchorBelow(SQL, SQL.indexOf("    email"))).toEqual({
      kind: "column",
      table: "public.users",
      column: "email",
    });
    expect(anchorBelow(SQL, SQL.indexOf('    "Display'))).toEqual({
      kind: "column",
      table: "public.users",
      column: "Display Name",
    });
  });

  it("attach to the closest statement at the end, to nothing in an empty text", () => {
    expect(anchorBelow(SQL, SQL.length)).toEqual({
      kind: "statement",
      statement: "CREATE TYPE mood AS ENUM",
    });
    expect(anchorBelow("", 0)).toBeNull();
  });
});

describe("placing threads in the SQL", () => {
  const line = (text: string) => SQL.lastIndexOf("\n", SQL.indexOf(text) - 1) + 1;

  it("shows a table's above its CREATE, a column's above its line", () => {
    expect(placeInSql(SQL, { kind: "table", table: "public.users" })).toBe(0);
    expect(placeInSql(SQL, { kind: "column", table: "public.users", column: "email" })).toBe(
      line("    email"),
    );
    expect(placeInSql(SQL, { kind: "column", table: "public.users", column: "Display Name" })).toBe(
      line('    "Display'),
    );
  });

  it("keeps a thread above the statement it was written above", () => {
    const anchor = anchorBelow(SQL, SQL.indexOf("CREATE INDEX"))!;
    expect(placeInSql(SQL, anchor)).toBe(SQL.indexOf("CREATE INDEX"));
  });

  it("falls back to the table when that statement changed, to nothing for a point", () => {
    expect(
      placeInSql(SQL, { kind: "table", table: "public.users", statement: "CREATE INDEX gone" }),
    ).toBe(0);
    expect(placeInSql(SQL, { kind: "point", x: 1, y: 2 })).toBeNull();
    expect(placeInSql(SQL, { kind: "statement", statement: "CREATE EXTENSION x" })).toBeNull();
    expect(placeInSql(SQL, { kind: "table", table: "public.orders" })).toBeNull();
  });

  it("finds a table a migration only alters", () => {
    const migration =
      "ALTER TABLE users ADD COLUMN phone text;\nALTER TABLE users DROP COLUMN email;\n";
    expect(placeInSql(migration, { kind: "table", table: "public.users" })).toBe(0);
    expect(placeInSql(migration, { kind: "column", table: "public.users", column: "email" })).toBe(
      migration.indexOf("ALTER TABLE users DROP"),
    );
  });

  it("follows a table renamed by a graph edit", () => {
    const renamed = `${SQL}ALTER TABLE users RENAME TO members;\n`;
    expect(placeInSql(renamed, { kind: "table", table: "public.members" })).toBe(0);
  });
});

describe("anchors", () => {
  it("move with renames, and only theirs", () => {
    const column = { kind: "column", table: "public.users", column: "email" } as const;
    expect(renameTable(column, "public.users", "public.members")).toEqual({
      ...column,
      table: "public.members",
    });
    expect(renameTable(column, "public.orders", "public.x")).toBe(column);
    expect(renameColumn(column, "public.users", "email", "address")).toEqual({
      ...column,
      column: "address",
    });
    expect(renameColumn(column, "public.orders", "email", "address")).toBe(column);
  });

  it("dropped, leave their threads outdated", () => {
    expect(anchorExists({ kind: "table", table: "public.users" }, schema, SQL)).toBe(true);
    expect(anchorExists({ kind: "table", table: "public.orders" }, schema, SQL)).toBe(false);
    expect(
      anchorExists({ kind: "column", table: "public.users", column: "phone" }, schema, SQL),
    ).toBe(false);
    expect(anchorExists({ kind: "point", x: 0, y: 0 }, schema, SQL)).toBe(true);
    expect(
      anchorExists({ kind: "statement", statement: "CREATE TYPE mood AS ENUM" }, schema, SQL),
    ).toBe(true);
    expect(anchorExists({ kind: "statement", statement: "CREATE TYPE gone" }, schema, SQL)).toBe(
      false,
    );
  });

  it("read as people write them", () => {
    expect(anchorLabel({ kind: "column", table: "public.users", column: "email" })).toBe(
      "users.email",
    );
    expect(anchorLabel({ kind: "table", table: "billing.invoices" })).toBe("billing.invoices");
    expect(anchorLabel({ kind: "point", x: 0, y: 0 })).toBe("Canvas");
  });
});

describe("threads from the doc", () => {
  it("pairs comments with their thread, oldest first, and skips what does not read", () => {
    const threads = threadsOf(
      {
        t1: {
          anchor: { kind: "table", table: "public.users" },
          by: "a",
          at: "2026-10-16T10:00:00Z",
        },
        t2: { anchor: { kind: "nope" }, by: "a", at: "2026-10-16T10:00:00Z" },
        empty: { anchor: { kind: "point", x: 1, y: 1 }, by: "a", at: "2026-10-16T09:00:00Z" },
      },
      {
        c2: {
          thread: "t1",
          author: { id: "b", name: "Bob" },
          body: "second",
          at: "2026-10-16T10:05:00Z",
        },
        c1: {
          thread: "t1",
          author: { id: "a", name: "Alice" },
          body: "first",
          mentions: ["b"],
          at: "2026-10-16T10:00:00Z",
        },
        bad: { thread: "t1", body: 3 },
      },
    );
    expect(threads.map((t) => t.id)).toEqual(["t1"]);
    expect(threads[0]!.comments.map((c) => c.body)).toEqual(["first", "second"]);
    expect(threads[0]!.comments[0]!.mentions).toEqual(["b"]);
    expect(threads[0]!.resolved).toBeNull();
  });

  it("are unread when someone else wrote after the reader read", () => {
    const thread: ThreadView = {
      id: "t",
      anchor: { kind: "point", x: 0, y: 0 },
      by: "a",
      at: "2026-10-16T10:00:00Z",
      resolved: null,
      comments: [
        {
          id: "c",
          authorId: "b",
          authorName: "Bob",
          body: "hi",
          mentions: [],
          at: "2026-10-16T10:05:00Z",
          edited: null,
        },
      ],
    };
    expect(isUnread(thread, "a", null)).toBe(true);
    expect(isUnread(thread, "a", "2026-10-16T10:06:00Z")).toBe(false);
    expect(isUnread(thread, "b", null)).toBe(false);
  });
});

describe("mentions", () => {
  const members = [
    { id: "1", name: "Ann" },
    { id: "2", name: "Ann Lee" },
    { id: "3", name: "Bob" },
  ];

  it("are found as they are typed, names starting with it first", () => {
    expect(mentionQuery("hey @An", 7)).toEqual({ from: 4, query: "An" });
    expect(mentionQuery("@", 1)).toEqual({ from: 0, query: "" });
    expect(mentionQuery("mail me@x", 9)).toBeNull();
    expect(matchPeople(members, "lee").map((m) => m.id)).toEqual(["2"]);
    expect(matchPeople(members, "b").map((m) => m.id)).toEqual(["3"]);
  });

  it("are the members a body names, longest name first", () => {
    expect(mentionsIn("thanks @Ann Lee and @bob", members)).toEqual(["2", "3"]);
    expect(mentionsIn("@Ann, look", members)).toEqual(["1"]);
    expect(mentionsIn("nobody@Ann", members)).toEqual([]);
  });

  it("are drawn apart from the text", () => {
    expect(bodyParts("hi @Bob!", [members[2]!])).toEqual([
      { text: "hi " },
      { mention: members[2], text: "@Bob" },
      { text: "!" },
    ]);
  });
});
