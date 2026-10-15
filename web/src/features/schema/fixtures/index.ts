// Sample schemas for the dev viewer (/app/dev/viewer), its end-to-end tests
// and the unit tests. Never imported by product code.
//
// blog and showcase are remus's own examples (lupidae/remus, examples/*/out),
// rendered here with remus-sql/-dbml/-mermaid 0.1.1; blog-v2 is blog with a
// release's worth of changes, and blog-diff.json is romulus-diff's output for
// blog → blog-v2. The others are built in code (synthetic.ts). showcase.json
// also carries remus 0.2's `partition_key` on its sub-partitioned partition.

import { parseStatusMap, type SchemaDiffInput } from "../diff";
import type { Groups, Layout } from "../layout";
import { parseSchema, type Schema } from "../model";
import type { SchemaTexts } from "../text/formats";

export interface FixtureData {
  schema: Schema;
  diff?: SchemaDiffInput;
  texts: SchemaTexts;
  layout?: Layout;
  groups?: Groups;
}

export const FIXTURE_IDS = ["small", "medium", "large", "multi-schema", "diff", "empty"] as const;
export type FixtureId = (typeof FIXTURE_IDS)[number];

export interface Fixture {
  id: FixtureId;
  label: string;
  description: string;
  load: () => Promise<FixtureData>;
}

const json = (schema: Schema) => `${JSON.stringify(schema, null, 2)}\n`;

/** Where the blog's tables sat in v1: the diff shows new tables placed next to them. */
const BLOG_V1_LAYOUT: Layout = {
  "public.users": { x: 0, y: 120 },
  "public.profiles": { x: 0, y: 380 },
  "public.posts": { x: 340, y: 0 },
  "public.tags": { x: 340, y: 340 },
  "public.comments": { x: 700, y: 0 },
  "public.post_tags": { x: 700, y: 250 },
  "public.published_posts": { x: 700, y: 390 },
};

export const FIXTURES: Fixture[] = [
  {
    id: "small",
    label: "Blog",
    description: "7 tables: an enum, a self-reference, a junction table and a view",
    load: async () => {
      const [model, sql, dbml, mermaid] = await Promise.all([
        import("./blog.json"),
        import("./blog.sql?raw"),
        import("./blog.dbml?raw"),
        import("./blog.mmd?raw"),
      ]);
      const schema = parseSchema(model.default);
      return {
        schema,
        texts: {
          sql: sql.default,
          json: json(schema),
          dbml: dbml.default,
          mermaid: mermaid.default,
        },
      };
    },
  },
  {
    id: "medium",
    label: "Commerce",
    description:
      "46 tables in six groups, partitions, views, a 30-column table, row level security",
    load: async () => {
      const { mediumFixture, renderFixtureSql } = await import("./synthetic");
      const { schema, groups } = mediumFixture();
      return { schema, groups, texts: { sql: renderFixtureSql(schema), json: json(schema) } };
    },
  },
  {
    id: "large",
    label: "Warehouse",
    description: "300 tables over six schemas: the performance fixture",
    load: async () => {
      const { largeFixture, renderFixtureSql } = await import("./synthetic");
      const schema = largeFixture(300);
      return { schema, texts: { sql: renderFixtureSql(schema), json: json(schema) } };
    },
  },
  {
    id: "multi-schema",
    label: "Showcase",
    description: "Two schemas, domains, a composite type, sub-partitions, views over views",
    load: async () => {
      const [model, sql, dbml, mermaid] = await Promise.all([
        import("./showcase.json"),
        import("./showcase.sql?raw"),
        import("./showcase.dbml?raw"),
        import("./showcase.mmd?raw"),
      ]);
      const schema = parseSchema(model.default);
      return {
        schema,
        texts: {
          sql: sql.default,
          json: json(schema),
          dbml: dbml.default,
          mermaid: mermaid.default,
        },
      };
    },
  },
  {
    id: "diff",
    label: "Blog, v1 → v2",
    description: "2 tables added, 3 changed, 1 removed: the graph of a diff",
    load: async () => {
      const [base, model, diff, sql, dbml, mermaid] = await Promise.all([
        import("./blog.json"),
        import("./blog-v2.json"),
        import("./blog-diff.json"),
        import("./blog-v2.sql?raw"),
        import("./blog-v2.dbml?raw"),
        import("./blog-v2.mmd?raw"),
      ]);
      const schema = parseSchema(model.default);
      return {
        schema,
        diff: { base: parseSchema(base.default), status: parseStatusMap(diff.default.status) },
        texts: {
          sql: sql.default,
          json: json(schema),
          dbml: dbml.default,
          mermaid: mermaid.default,
        },
        layout: BLOG_V1_LAYOUT,
      };
    },
  },
  {
    id: "empty",
    label: "Empty",
    description: "A database whose schema has no tables yet",
    load: () => {
      const schema: Schema = {
        remus_version: "0.1.1",
        database: "fresh",
        server_version: "17.6",
        entities: [],
      };
      return Promise.resolve({ schema, texts: { sql: "-- No tables yet.\n", json: json(schema) } });
    },
  },
];

export function fixture(id: FixtureId): Fixture {
  return FIXTURES.find((f) => f.id === id)!;
}
