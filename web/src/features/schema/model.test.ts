import { parseStatusMap } from "./diff";
import blogDiff from "./fixtures/blog-diff.json";
import blog from "./fixtures/blog.json";
import showcase from "./fixtures/showcase.json";
import { largeFixture, mediumFixture, renderFixtureSql } from "./fixtures/synthetic";
import { isMultiSchema, parseSchema, qualifiedName, SchemaParseError } from "./model";
import { findStatements } from "./text/statements";

describe("parseSchema", () => {
  test("accepts remus's own output", () => {
    expect(parseSchema(blog).entities).toHaveLength(7);
    expect(isMultiSchema(parseSchema(showcase))).toBe(true);
  });

  test("accepts a model with no optional collections", () => {
    const minimal = { remus_version: "0.1", database: "db", server_version: "17" };
    expect(parseSchema(minimal).entities).toBeUndefined();
  });

  test("names the field that is wrong", () => {
    const broken = structuredClone(blog);
    (broken.entities[2]!.columns[1] as { type_kind: string }).type_kind = "z";
    expect(() => parseSchema(broken)).toThrow(SchemaParseError);
    expect(() => parseSchema(broken)).toThrow(
      "model.entities[2].columns[1].type_kind should be one of b, e, d, c, r, m, p",
    );
    expect(() => parseSchema([])).toThrow("model should be an object");
  });
});

describe("parseStatusMap", () => {
  test("accepts romulus-diff's status map", () => {
    expect(parseStatusMap(blogDiff.status).entities.public!.profiles!.status).toBe("removed");
  });

  test("refuses a status it does not know", () => {
    const broken = structuredClone(blogDiff.status);
    (broken.entities.public.users.columns as Record<string, string>).email = "renamed";
    expect(() => parseStatusMap(broken)).toThrow("status.entities.public.users.columns.email");
  });
});

describe("synthetic fixtures", () => {
  test("the large one is 300 tables over six schemas, the same every time", () => {
    const schema = largeFixture(300);
    expect(schema.entities).toHaveLength(300);
    expect(new Set(schema.entities!.map((e) => e.schema)).size).toBe(6);
    expect(largeFixture(300)).toEqual(schema);
    expect(parseSchema(schema)).toBe(schema);
  });

  test("their SQL defines every table it draws", () => {
    for (const schema of [mediumFixture().schema, largeFixture(120)]) {
      const created = new Set(
        findStatements(renderFixtureSql(schema))
          .filter((s) => s.kind === "create" && !s.partitionOf)
          .map((s) => qualifiedName(s.target!)),
      );
      for (const e of schema.entities ?? []) expect(created).toContain(qualifiedName(e));
    }
  });

  test("the medium one's groups cover every table once", () => {
    const { schema, groups } = mediumFixture();
    const members = Object.values(groups).flatMap((g) => g.members);
    expect(members.sort()).toEqual(schema.entities!.map(qualifiedName).sort());
  });
});
