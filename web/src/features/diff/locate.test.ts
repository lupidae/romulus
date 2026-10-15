import blogV2 from "@/features/schema/fixtures/blog-v2.sql?raw";
import showcase from "@/features/schema/fixtures/showcase.sql?raw";
import { findStatements } from "@/features/schema/text/statements";

import { type Locatable, rangesOf } from "./locate";

const heads = (sql: string, target: Locatable) =>
  rangesOf(sql, findStatements(sql), target).map((r) =>
    sql.slice(r.from, r.to).split("\n")[0]!.slice(0, 80),
  );

describe("finding an object in the SQL", () => {
  it("finds a table's CREATE, its foreign keys, indexes and comments", () => {
    const found = heads(blogV2, { category: "table", schema: "public", name: "posts" });
    expect(found[0]).toBe("CREATE TABLE public.posts (");
    expect(found.some((h) => h.startsWith("ALTER TABLE public.posts ADD CONSTRAINT"))).toBe(true);
    expect(found.some((h) => h.startsWith("CREATE INDEX posts_author_idx"))).toBe(true);
    expect(found.some((h) => h.startsWith("COMMENT ON COLUMN public.posts.slug"))).toBe(true);
    // Not the foreign keys of other tables that point at it.
    expect(found.some((h) => h.includes("public.comments"))).toBe(false);
  });

  it("finds a type's CREATE and its comment", () => {
    expect(heads(blogV2, { category: "enum", schema: "public", name: "reaction_kind" })).toEqual([
      expect.stringMatching(/^CREATE TYPE public\.reaction_kind AS ENUM/),
      expect.stringMatching(/^COMMENT ON TYPE public\.reaction_kind IS/),
    ]);
    // A table is not a type.
    expect(heads(blogV2, { category: "enum", schema: "public", name: "posts" })).toEqual([]);
    expect(heads(showcase, { category: "enum", schema: "shop", name: "order_status" })).toEqual([
      expect.stringMatching(/^CREATE TYPE shop\.order_status/),
      "COMMENT ON TYPE shop.order_status IS 'Lifecycle of an order';",
    ]);
  });

  it("finds an extension", () => {
    expect(heads(showcase, { category: "extension", schema: null, name: "pgcrypto" })).toEqual([
      "CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA public;",
    ]);
    expect(heads(showcase, { category: "extension", schema: null, name: "pgcrypt" })).toEqual([]);
  });

  it("finds nothing for an object the text does not have", () => {
    expect(heads(blogV2, { category: "table", schema: "public", name: "profiles" })).toEqual([]);
  });
});
