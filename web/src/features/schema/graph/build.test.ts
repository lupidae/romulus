import { parseStatusMap } from "../diff";
import blogDiff from "../fixtures/blog-diff.json";
import blogV1 from "../fixtures/blog.json";
import blogV2 from "../fixtures/blog-v2.json";
import showcase from "../fixtures/showcase.json";
import { parseSchema } from "../model";
import { buildGraph } from "./build";

const blog = parseSchema(blogV1);

describe("buildGraph", () => {
  test("draws one node per entity and one edge per foreign key", () => {
    const graph = buildGraph({ schema: blog });
    expect(graph.tables.map((t) => t.key)).toEqual([
      "public.comments",
      "public.post_tags",
      "public.posts",
      "public.profiles",
      "public.published_posts",
      "public.tags",
      "public.users",
    ]);
    expect(graph.edges.filter((e) => e.kind === "fk")).toHaveLength(7);
    expect(graph.multiSchema).toBe(false);
    expect(graph.counts).toBeUndefined();
  });

  test("attaches edges to the first column of each side, self references included", () => {
    const graph = buildGraph({ schema: blog });
    const parent = graph.edges.find((e) => e.name === "comments_parent_id_fkey")!;
    expect(parent).toMatchObject({ source: "public.comments", target: "public.comments" });
    expect(parent).toMatchObject({ sourceColumn: "parent_id", targetColumn: "id" });
    expect(parent.cardinality).toEqual({ parent: "|o", child: "o{" });
  });

  test("reads a profile keyed by its user as one to zero-or-one", () => {
    const graph = buildGraph({ schema: blog });
    const profile = graph.edges.find((e) => e.name === "profiles_user_id_fkey")!;
    expect(profile.cardinality).toEqual({ parent: "||", child: "o|" });
  });

  test("links views to what they read", () => {
    const graph = buildGraph({ schema: blog });
    const reads = graph.edges
      .filter((e) => e.kind === "reads")
      .map((e) => `${e.source} -> ${e.target}`);
    expect(reads.sort()).toEqual([
      "public.published_posts -> public.posts",
      "public.published_posts -> public.users",
    ]);
  });

  test("marks keys, nullability, identity and links enum columns to their type", () => {
    const posts = buildGraph({ schema: blog }).byKey.get("public.posts")!;
    const byName = Object.fromEntries(posts.columns.map((c) => [c.name, c]));
    expect(byName.id).toMatchObject({ pk: true, identity: "always", notNull: true });
    expect(byName.author_id).toMatchObject({ fk: true, pk: false });
    expect(byName.slug).toMatchObject({ unique: true });
    expect(byName.published_at).toMatchObject({ notNull: false, display: "timestamptz" });
    expect(byName.status!.userType).toEqual({
      kind: "enum",
      schema: "public",
      name: "post_status",
    });
    expect(byName.status!.display).toBe("post_status");
  });

  test("collapses partitions, at every level, under the entity at the top of their tree", () => {
    const graph = buildGraph({ schema: parseSchema(showcase) });
    expect(graph.multiSchema).toBe(true);
    const events = graph.byKey.get("audit.events")!;
    expect(events.kind).toBe("partitioned");
    // Parents first, each level by name.
    expect(events.partitions.map((p) => [p.name, p.depth])).toEqual([
      ["events_2026", 0],
      ["events_2027", 0],
      ["events_2027_h0", 1],
      ["events_2027_h1", 1],
      ["events_default", 0],
    ]);
    expect(graph.byKey.has("audit.events_2027")).toBe(false);
    expect(graph.partitionOwner.get("audit.events_2027_h1")).toBe("audit.events");
  });

  test("carries a sub-partitioned partition's own key (remus 0.2)", () => {
    const events = buildGraph({ schema: parseSchema(showcase) }).byKey.get("audit.events")!;
    expect(events.partitions.find((p) => p.name === "events_2027")?.partitionKey).toBe("HASH (id)");
    expect(events.partitions.find((p) => p.name === "events_2026")?.partitionKey).toBeNull();
  });

  test("a view reading several partitions of one table reads it once", () => {
    const schema = parseSchema(showcase);
    const view = schema.entities!.find((e) => e.name === "open_orders")!;
    view.depends_on = [
      { schema: "audit", name: "events_2026" },
      { schema: "audit", name: "events_2027_h1" },
      { schema: "audit", name: "events" },
    ];
    const ids = buildGraph({ schema }).edges.map((e) => e.id);
    expect(ids.filter((id) => id === "reads:shop.open_orders:audit.events")).toHaveLength(1);
    expect(new Set(ids).size).toBe(ids.length);
  });

  test("says so when two relations share a qualified name, and draws the first", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const schema = parseSchema(blog);
    const users = schema.entities!.find((e) => e.name === "users")!;
    // Schema "public.x" + table "y" and schema "public" + table "x.y".
    schema.entities!.push(
      { ...users, schema: "public.x", name: "y" },
      { ...users, schema: "public", name: "x.y" },
    );
    const graph = buildGraph({ schema });
    expect(graph.byKey.get("public.x.y")?.schema).toBe("public.x");
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('share the key "public.x.y"'));
    warn.mockRestore();
  });

  describe("in diff mode", () => {
    const graph = buildGraph({
      schema: parseSchema(blogV2),
      diff: { base: blog, status: parseStatusMap(blogDiff.status) },
    });

    test("draws the union of both models", () => {
      expect(graph.byKey.get("public.profiles")?.status).toBe("removed");
      expect(graph.byKey.get("public.post_reactions")?.status).toBe("added");
      expect(graph.byKey.get("public.users")?.status).toBe("changed");
      expect(graph.byKey.get("public.tags")?.status).toBeUndefined();
      expect(graph.counts).toEqual({ added: 2, removed: 1, changed: 3 });
    });

    test("keeps a removed column where it was, next to the one replacing it", () => {
      const posts = graph.byKey.get("public.posts")!;
      const names = posts.columns.map((c) => `${c.name}${c.status ? `:${c.status}` : ""}`);
      expect(names.slice(3, 7)).toEqual([
        "slug",
        "body:removed",
        "body_md:added",
        "reading_minutes:added",
      ]);
    });

    test("remembers what a changed column was", () => {
      const author = graph.byKey
        .get("public.comments")!
        .columns.find((c) => c.name === "author_id")!;
      expect(author.status).toBe("changed");
      expect(author.before?.not_null).toBe(false);
      expect(author.notNull).toBe(true);
    });

    test("styles foreign keys by status, the removed table's included", () => {
      const status = (name: string) => graph.edges.find((e) => e.name === name)?.status;
      expect(status("profiles_user_id_fkey")).toBe("removed");
      expect(status("comments_author_id_fkey")).toBe("changed");
      expect(status("subscriptions_user_id_fkey")).toBe("added");
      expect(status("post_tags_tag_id_fkey")).toBeUndefined();
    });

    test("updates the cardinality a change implies", () => {
      const author = graph.edges.find((e) => e.name === "comments_author_id_fkey")!;
      expect(author.cardinality?.parent).toBe("||");
    });
  });
});
