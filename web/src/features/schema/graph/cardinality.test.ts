import type { Column, Constraint, Entity } from "../model";
import { cardinality, describeCardinality } from "./cardinality";

const column = (name: string, notNull: boolean): Column => ({
  name,
  type: "bigint",
  type_schema: "pg_catalog",
  type_name: "int8",
  type_kind: "b",
  not_null: notNull,
  default: null,
  identity: null,
  generated: null,
  comment: null,
});

const fk = (columns: string[]): Constraint => ({
  name: "child_fk",
  kind: "f",
  columns,
  ref_schema: "public",
  ref_table: "parent",
  ref_columns: columns.map(() => "id"),
  on_update: "a",
  on_delete: "a",
  definition: "",
  comment: null,
});

function child(columns: Column[], constraints: Constraint[]): Entity {
  return {
    schema: "public",
    name: "child",
    kind: "r",
    comment: null,
    partition_key: null,
    view_definition: null,
    columns,
    constraints,
  };
}

describe("cardinality (remus's rules)", () => {
  test("a not-null, non-unique reference is exactly one parent, zero or many children", () => {
    const key = fk(["parent_id"]);
    expect(cardinality(child([column("parent_id", true)], [key]), key)).toEqual({
      parent: "||",
      child: "o{",
    });
  });

  test("a nullable reference makes the parent end zero or one", () => {
    const key = fk(["parent_id"]);
    expect(cardinality(child([column("parent_id", false)], [key]), key)).toEqual({
      parent: "|o",
      child: "o{",
    });
  });

  test("unique reference columns make the child end zero or one", () => {
    const key = fk(["parent_id"]);
    const unique: Constraint = { ...key, name: "child_parent_id_key", kind: "u" };
    expect(cardinality(child([column("parent_id", true)], [key, unique]), key)).toEqual({
      parent: "||",
      child: "o|",
    });
  });

  test("a primary key made of the reference is unique too (a 1:1 profile)", () => {
    const key = fk(["user_id"]);
    const pk: Constraint = { ...key, name: "child_pkey", kind: "p" };
    expect(cardinality(child([column("user_id", true)], [key, pk]), key).child).toBe("o|");
  });

  test("a composite reference is mandatory only when every column is not null", () => {
    const key = fk(["a", "b"]);
    const entity = child([column("a", true), column("b", false)], [key]);
    expect(cardinality(entity, key).parent).toBe("|o");
  });

  test("uniqueness of a composite reference needs the exact set, in any order", () => {
    const key = fk(["a", "b"]);
    const wider: Constraint = { ...key, name: "u_abc", kind: "u", columns: ["a", "b", "c"] };
    const same: Constraint = { ...key, name: "u_ba", kind: "u", columns: ["b", "a"] };
    const columns = [column("a", true), column("b", true), column("c", true)];
    expect(cardinality(child(columns, [key, wider]), key).child).toBe("o{");
    expect(cardinality(child(columns, [key, same]), key).child).toBe("o|");
  });

  test("the child end is never one or many", () => {
    const key = fk(["parent_id"]);
    const marks = [true, false].map(
      (n) => cardinality(child([column("parent_id", n)], [key]), key).child,
    );
    expect(marks.every((m) => m === "o|" || m === "o{")).toBe(true);
  });

  test("is described in words", () => {
    expect(describeCardinality({ parent: "||", child: "o{" })).toBe(
      "each row references exactly one, each referenced row has zero or many",
    );
  });
});
