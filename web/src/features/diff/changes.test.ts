import blogDiff from "@/features/schema/fixtures/blog-diff.json";

import {
  changeSections,
  detailLines,
  displayName,
  filterSections,
  lineParts,
  type SchemaDiff,
  spansSchemas,
  statusCounts,
} from "./changes";
import { parseRange, rangeParam } from "./queries";

const diff = blogDiff as unknown as SchemaDiff;

const empty = (over: Partial<SchemaDiff> = {}): SchemaDiff => ({
  summary: { counts: [], text: "no changes" },
  entities: [],
  enums: [],
  domains: [],
  composites: [],
  extensions: [],
  orphan_partitions: { partitions: [], lines: [], sql: { before: null, after: null } },
  status: {
    entities: {},
    orphan_partitions: {},
    enums: {},
    domains: {},
    composites: {},
    extensions: {},
  },
  ...over,
});

describe("the change list", () => {
  it("groups objects the way the summary counts them, sorted as romulus-diff sorts them", () => {
    const sections = changeSections(diff);
    expect(sections.map((s) => s.title)).toEqual(["Tables", "Enums"]);
    expect(sections[0]!.items.map((i) => [i.name, i.status])).toEqual([
      ["comments", "changed"],
      ["post_reactions", "added"],
      ["posts", "changed"],
      ["profiles", "removed"],
      ["subscriptions", "added"],
      ["users", "changed"],
    ]);
    const posts = sections[0]!.items[2]!;
    expect(posts).toMatchObject({ id: "table:public.posts", table: "public.posts", kind: "table" });
    expect(sections[1]!.items[0]).toMatchObject({
      id: "enum:public.reaction_kind",
      category: "enum",
    });
    expect(sections[1]!.items[0]!.table).toBeUndefined();
  });

  it("counts and filters by status", () => {
    const sections = changeSections(diff);
    expect(statusCounts(sections)).toEqual({ added: 3, removed: 1, changed: 3 });
    expect(filterSections(sections, "all")).toBe(sections);
    const removed = filterSections(sections, "removed");
    expect(removed.map((s) => s.items.map((i) => i.name))).toEqual([["profiles"]]);
    // A section with nothing left goes away.
    expect(filterSections(sections, "added").map((s) => s.title)).toEqual(["Tables", "Enums"]);
    expect(filterSections(sections, "changed").map((s) => s.title)).toEqual(["Tables"]);
  });

  it("skips the line that only repeats an added or removed object's status", () => {
    const [tables] = changeSections(diff);
    const added = tables!.items.find((i) => i.name === "post_reactions")!;
    expect(added.lines).toEqual(["table `post_reactions` added"]);
    expect(detailLines(added)).toEqual([]);
    const changed = tables!.items.find((i) => i.name === "posts")!;
    expect(detailLines(changed)).toHaveLength(4);
  });

  it("lists views, extensions and orphan partitions in their own sections", () => {
    const sections = changeSections(
      empty({
        entities: [
          {
            schema: "shop",
            name: "orders_view",
            kind: "materialized_view",
            status: "added",
            changes: [],
            columns: [],
            constraints: [],
            indexes: [],
            policies: [],
            partitions: [],
            lines: ["materialized view `shop.orders_view` added"],
            sql: { before: null, after: null },
          },
        ],
        extensions: [
          {
            name: "pgcrypto",
            status: "changed",
            changes: [{ field: "version", before: "1.3", after: "1.4" }],
            lines: ["`pgcrypto`: version 1.3 → 1.4"],
            sql: { before: null, after: null },
          },
        ],
        orphan_partitions: {
          partitions: [
            {
              schema: "audit",
              name: "events_2028",
              parent: { schema: "audit", name: "events_2027" },
              status: "added",
              changes: [],
              definition: { before: null, after: "DEFAULT" },
            },
          ],
          lines: [
            "partition `audit.events_2028` of `audit.events_2027` added: DEFAULT",
            "partition `audit.events_2029` of `audit.events_2027` removed: DEFAULT",
          ],
          sql: { before: null, after: null },
        },
      }),
    );
    expect(sections.map((s) => [s.title, s.items.map((i) => i.id)])).toEqual([
      ["Materialized views", ["materialized_view:shop.orders_view"]],
      ["Partitions", ["partition:audit.events_2028"]],
      ["Extensions", ["extension:pgcrypto"]],
    ]);
    expect(sections[1]!.items[0]!.lines).toEqual([
      "partition `audit.events_2028` of `audit.events_2027` added: DEFAULT",
    ]);
    expect(sections[2]!.items[0]!.schema).toBeNull();
    expect(spansSchemas(sections)).toBe(true);
    expect(displayName(sections[0]!.items[0]!, true)).toBe("shop.orders_view");
    expect(displayName(sections[0]!.items[0]!, false)).toBe("orders_view");
  });

  it("has nothing to list for identical schemas", () => {
    expect(changeSections(empty())).toEqual([]);
  });

  it("splits a line into words and code", () => {
    expect(lineParts("`users.email`: type text → citext")).toEqual([
      { text: "users.email", code: true },
      { text: ": type text → citext", code: false },
    ]);
    expect(lineParts("foreign key `a` on `b`: x")).toEqual([
      { text: "foreign key ", code: false },
      { text: "a", code: true },
      { text: " on ", code: false },
      { text: "b", code: true },
      { text: ": x", code: false },
    ]);
    expect(lineParts("no code")).toEqual([{ text: "no code", code: false }]);
  });
});

describe("a comparison's address", () => {
  it("is from...to, both version numbers", () => {
    expect(rangeParam(4, 5)).toBe("4...5");
    expect(parseRange("4...5")).toEqual({ from: 4, to: 5 });
    expect(parseRange("12...3")).toEqual({ from: 12, to: 3 });
    for (const bad of ["4..5", "0...1", "v4...v5", "4...", "-1...2", "1.5...2", "01...2"]) {
      expect(parseRange(bad)).toBeNull();
    }
  });
});
