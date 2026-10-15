import blogJson from "../fixtures/blog.json";
import { parseSchema, type Schema } from "../model";
import { applyEdit, layoutChange, markOf, reflects } from "./apply";
import type { GraphEdit, Target } from "./edits";
import {
  checkForeignKey,
  columnDependents,
  columnNameTaken,
  foreignKeyName,
  freshTableName,
  indexColumns,
  makeObjectName,
  mentions,
  referenceableColumns,
  relationNameTaken,
  tableDependents,
} from "./inspect";
import { dropColumn, dropTable, foreignKey, setType } from "./plan";
import { canonicalType, customType, modelTypes, needsUsing } from "./types";

const blog = parseSchema(blogJson);
const t = (name: string, kind: Target["kind"] = "r"): Target => ({ schema: "public", name, kind });
const entity = (schema: Schema, name: string) => schema.entities!.find((e) => e.name === name);
const columns = (schema: Schema, name: string) => entity(schema, name)?.columns?.map((c) => c.name);
const reason = (check: ReturnType<typeof checkForeignKey>) => (check.ok ? "" : check.reason);

describe("what depends on what", () => {
  test("a table: the foreign keys pointing at it and the views reading it", () => {
    const users = tableDependents(blog, t("users"));
    expect(users.cascade).toBe(true);
    expect(users.all.map((d) => `${d.kind} ${d.table.name}.${d.name}`).sort()).toEqual([
      "foreign key comments.comments_author_id_fkey",
      "foreign key posts.posts_author_id_fkey",
      "foreign key profiles.profiles_user_id_fkey",
      "view published_posts.published_posts",
    ]);
    expect(tableDependents(blog, t("tags")).all.map((d) => d.name)).toEqual([
      "post_tags_tag_id_fkey",
    ]);
    // Nothing points at a join table: a plain DROP.
    expect(tableDependents(blog, t("post_tags"))).toEqual({ all: [], cascade: false });
  });

  test("a column: what references it needs CASCADE, its own constraints go anyway", () => {
    const id = columnDependents(blog, t("users"), "id");
    expect(id.cascade).toBe(true);
    expect(
      id.all
        .filter((d) => d.kind === "foreign key")
        .map((d) => d.name)
        .sort(),
    ).toEqual(["comments_author_id_fkey", "posts_author_id_fkey", "profiles_user_id_fkey"]);
    expect(id.all.find((d) => d.name === "users_pkey")?.kind).toBe("constraint");

    const bio = columnDependents(blog, t("profiles"), "bio");
    expect(bio).toEqual({ all: [], cascade: false });

    // The CHECK on posts names status: it goes with it, no CASCADE needed…
    const status = columnDependents(blog, t("posts"), "status");
    expect(status.all.map((d) => d.name)).toContain("posts_published_have_a_date");
    // …but the view selecting posts.title does need it.
    const title = columnDependents(blog, t("posts"), "title");
    expect(title.cascade).toBe(true);
    expect(title.all.map((d) => d.name)).toContain("published_posts");
  });

  test("mentions finds a column as a whole identifier", () => {
    expect(mentions("SELECT p.title FROM posts p", "title")).toBe(true);
    expect(mentions("(owner_id = auth.uid())", "owner_id")).toBe(true);
    expect(mentions("SELECT subtitle FROM posts", "title")).toBe(false);
    expect(mentions("SELECT title_case FROM posts", "title")).toBe(false);
    expect(mentions('SELECT "Title" FROM posts', "Title")).toBe(true);
    expect(mentions("SELECT title FROM posts", "Title")).toBe(false);
    expect(mentions(null, "x")).toBe(false);
  });

  test("index definitions give their columns, not expressions", () => {
    expect(indexColumns("CREATE UNIQUE INDEX a ON public.t USING btree (email)")).toEqual([
      "email",
    ]);
    expect(indexColumns('CREATE INDEX a ON public.t USING btree ("Org", id)')).toEqual([
      "Org",
      "id",
    ]);
    expect(indexColumns("CREATE UNIQUE INDEX a ON public.t USING btree (lower(email))")).toBeNull();
    expect(
      indexColumns("CREATE UNIQUE INDEX a ON public.t USING btree (email) WHERE (x)"),
    ).toBeNull();
  });
});

describe("foreign keys", () => {
  test("need a unique target of a comparable type", () => {
    expect(checkForeignKey(blog, t("comments"), "body", t("users"), "name")).toMatchObject({
      ok: false,
      fix: "unique",
    });
    expect(reason(checkForeignKey(blog, t("comments"), "body", t("users"), "id"))).toContain(
      "cannot compare",
    );
    expect(checkForeignKey(blog, t("users"), "id", t("users"), "id")).toMatchObject({ ok: false });
    expect(reason(checkForeignKey(blog, t("comments"), "author_id", t("users"), "id"))).toContain(
      "comments_author_id_fkey",
    );
    // integer → bigint compare; text → a unique text column is fine.
    expect(checkForeignKey(blog, t("post_tags"), "tag_id", t("posts"), "id")).toEqual({ ok: true });
    expect(checkForeignKey(blog, t("comments"), "body", t("tags"), "name")).toEqual({ ok: true });
    expect(
      reason(checkForeignKey(blog, t("comments"), "author_id", t("published_posts", "v"), "id")),
    ).toContain("views");
  });

  test("a unique index counts as unique", () => {
    const withIndex = applyEdit(blog, { kind: "addTable", table: t("x") });
    entity(withIndex, "users")!.indexes = [
      {
        name: "users_name_idx",
        unique: true,
        partial: false,
        definition: "CREATE UNIQUE INDEX users_name_idx ON public.users USING btree (name)",
      },
    ];
    expect(checkForeignKey(withIndex, t("tags"), "name", t("users"), "name")).toEqual({ ok: true });
    expect(referenceableColumns(entity(withIndex, "users")!).map((c) => c.name)).toEqual([
      "id",
      "email",
      "name",
    ]);
  });

  test("are named as Postgres names them", () => {
    expect(foreignKeyName(blog, t("posts"), "editor_id")).toBe("posts_editor_id_fkey");
    // Taken: the next free number, like ChooseConstraintName.
    expect(foreignKeyName(blog, t("posts"), "author_id")).toBe("posts_author_id_fkey1");
    expect(makeObjectName("a".repeat(40), "b".repeat(40), "fkey")).toBe(
      `${"a".repeat(29)}_${"b".repeat(28)}_fkey`,
    );
    expect(makeObjectName("short", "c".repeat(80), "fkey").length).toBe(63);
    // Shortened on a character boundary, in bytes.
    const name = makeObjectName("é".repeat(40), "x", "fkey");
    expect(new TextEncoder().encode(name).length).toBeLessThanOrEqual(63);
    expect(name.endsWith("é_x_fkey")).toBe(true);
  });

  test("plan into an edit with that name", () => {
    const plan = foreignKey(blog, t("posts"), "author_id", t("profiles"), "user_id", "c");
    expect(plan).toMatchObject({
      ok: true,
      edit: { name: "posts_author_id_fkey1", onDelete: "c" },
    });
  });
});

describe("types", () => {
  test("typed by hand: type names only", () => {
    expect(customType("varchar(80)")?.sql).toBe("varchar(80)");
    expect(customType("  timestamp(3)   with time zone ")?.sql).toBe("timestamp(3) with time zone");
    expect(customType("numeric(10, 2)")?.display).toBe("numeric(10,2)");
    expect(customType("public.mood[]")?.isArray).toBe(true);
    expect(customType('"My Type"')?.sql).toBe('"My Type"');
    expect(customType("double precision")?.display).toBe("double precision");
    expect(customType("interval day to second")?.sql).toBe("interval day to second");
    expect(customType("national character varying(10)")?.sql).toBe(
      "national character varying(10)",
    );
    expect(customType("bit varying(8)[]")?.isArray).toBe(true);
    for (const bad of [
      // Words after a type are not part of it.
      "integer PRIMARY KEY",
      "text NOT NULL",
      "varchar(80) default 'x'",
      "double precision references users",
      "timestamp with time zone not null",
      "text; DROP TABLE users",
      "text -- x",
      "'x'",
      "text)",
      "",
      "a(b)",
      "x /* */",
    ])
      expect(customType(bad), bad).toBeNull();
  });

  test("spelled as format_type spells them", () => {
    expect(canonicalType("int8")).toBe("bigint");
    expect(canonicalType("varchar(20)")).toBe("character varying(20)");
    expect(canonicalType("timestamptz")).toBe("timestamp with time zone");
    expect(canonicalType("timestamp(3)")).toBe("timestamp(3) without time zone");
    expect(canonicalType("int[]")).toBe("integer[]");
    expect(canonicalType("Numeric( 10 , 2 )")).toBe("numeric(10,2)");
  });

  test("USING only where Postgres would not cast by itself", () => {
    expect(needsUsing("integer", "bigint")).toBe(false);
    expect(needsUsing("bigint", "numeric")).toBe(false);
    expect(needsUsing("integer", "text")).toBe(false);
    expect(needsUsing("text", "integer")).toBe(true);
    expect(needsUsing("text", "uuid")).toBe(true);
    expect(needsUsing("timestamp with time zone", "date")).toBe(false);
    expect(needsUsing("text", "jsonb")).toBe(true);
  });

  test("a type change plans USING, and nothing when the type is the same", () => {
    expect(setType(blog, t("users"), "name", customType("varchar(80)")!)).toMatchObject({
      using: false,
    });
    expect(setType(blog, t("tags"), "name", customType("integer")!)).toMatchObject({ using: true });
    expect(setType(blog, t("users"), "id", customType("int8")!)).toBeNull();
    const [mood] = modelTypes(blog);
    expect(mood?.sql).toBe("post_status");
    expect(setType(blog, t("users"), "name", mood!)).toMatchObject({ using: true });
    expect(setType(blog, t("posts"), "status", mood!)).toBeNull();
  });
});

describe("names", () => {
  test("a relation's name is taken by tables, partitions and types alike", () => {
    expect(relationNameTaken(blog, "public", "users")).toMatch(/already exists/);
    expect(relationNameTaken(blog, "public", "post_status")).toMatch(/type/);
    expect(relationNameTaken(blog, "other", "users")).toBeNull();
    // Indexes and sequences share the namespace with tables.
    expect(relationNameTaken(blog, "public", "users_pkey")).toBe(
      "public has an index named users_pkey.",
    );
    expect(relationNameTaken(blog, "public", "posts_author_idx")).toMatch(/index/);
    expect(relationNameTaken(blog, "public", "users_id_seq")).toBe(
      "public has a sequence named users_id_seq.",
    );
    const serial = applyEdit(blog, {
      kind: "addColumn",
      table: t("tags"),
      column: {
        name: "n",
        type: customType("integer")!,
        notNull: true,
        default: "nextval('public.tag_numbers'::regclass)",
      },
    });
    expect(relationNameTaken(serial, "public", "tag_numbers")).toMatch(/sequence/);
    expect(freshTableName(blog, "public")).toBe("new_table");
    const once = applyEdit(blog, { kind: "addTable", table: t("new_table") });
    expect(freshTableName(once, "public")).toBe("new_table_2");
  });
});

describe("column names", () => {
  test("system columns and existing names are refused", () => {
    const users = blog.entities!.find((e) => e.name === "users")!;
    expect(columnNameTaken(users, "xmin")).toBe(
      "xmin is a system column: Postgres keeps the name.",
    );
    expect(columnNameTaken(users, "ctid")).toMatch(/system column/);
    expect(columnNameTaken(users, "email")).toBe("users already has a column email.");
    expect(columnNameTaken(users, "oid")).toBeNull();
    expect(columnNameTaken(users, "nickname")).toBeNull();
  });
});

describe("applyEdit and reflects", () => {
  const roundTrip = (edit: GraphEdit, check: (after: Schema) => void) => {
    expect(reflects(blog, edit)).not.toBe(true);
    const after = applyEdit(blog, edit);
    check(after);
    expect(reflects(after, edit)).not.toBe(false);
  };

  test("renaming a table follows it into references and views", () => {
    roundTrip({ kind: "renameTable", table: t("users"), to: "members" }, (after) => {
      expect(entity(after, "users")).toBeUndefined();
      expect(entity(after, "members")).toBeDefined();
      const fk = entity(after, "posts")!.constraints!.find(
        (c) => c.name === "posts_author_id_fkey",
      );
      expect(fk?.ref_table).toBe("members");
      expect(entity(after, "published_posts")!.depends_on).toContainEqual({
        schema: "public",
        name: "members",
      });
    });
  });

  test("renaming a column follows it into constraints and references", () => {
    roundTrip({ kind: "renameColumn", table: t("users"), column: "id", to: "user_id" }, (after) => {
      expect(columns(after, "users")).toContain("user_id");
      expect(entity(after, "users")!.constraints!.find((c) => c.kind === "p")?.columns).toEqual([
        "user_id",
      ]);
      const fk = entity(after, "posts")!.constraints!.find(
        (c) => c.name === "posts_author_id_fkey",
      );
      expect(fk?.ref_columns).toEqual(["user_id"]);
    });
  });

  test("adding what is there already changes nothing", () => {
    const once = applyEdit(blog, { kind: "addTable", table: t("invoices") });
    expect(applyEdit(once, { kind: "addTable", table: t("invoices") })).toBe(once);
    const column = { name: "email", type: customType("text")!, notNull: false, default: null };
    expect(applyEdit(blog, { kind: "addColumn", table: t("users"), column })).toBe(blog);
  });

  test("adding a table gives it an identity primary key", () => {
    roundTrip({ kind: "addTable", table: t("invoices") }, (after) => {
      const added = entity(after, "invoices")!;
      expect(added.columns).toMatchObject([{ name: "id", identity: "a", not_null: true }]);
      expect(added.constraints).toMatchObject([{ kind: "p", columns: ["id"] }]);
    });
  });

  test("adding a column", () => {
    const column = { name: "bio", type: customType("varchar(200)")!, notNull: true, default: "''" };
    roundTrip({ kind: "addColumn", table: t("users"), column }, (after) => {
      expect(entity(after, "users")!.columns!.at(-1)).toMatchObject({
        name: "bio",
        type: "character varying(200)",
        not_null: true,
        default: "''",
      });
    });
  });

  test("dropping a table with CASCADE takes its references and views", () => {
    const { edit } = dropTable(blog, t("users"));
    roundTrip(edit, (after) => {
      expect(entity(after, "users")).toBeUndefined();
      expect(entity(after, "published_posts")).toBeUndefined();
      expect(entity(after, "posts")!.constraints!.map((c) => c.name)).not.toContain(
        "posts_author_id_fkey",
      );
    });
  });

  test("dropping a column takes what involves it", () => {
    const { edit } = dropColumn(blog, t("posts"), "status");
    roundTrip(edit, (after) => {
      expect(columns(after, "posts")).not.toContain("status");
      expect(entity(after, "posts")!.constraints!.map((c) => c.name)).not.toContain(
        "posts_published_have_a_date",
      );
    });
  });

  test("column settings", () => {
    roundTrip({ kind: "setNotNull", table: t("users"), column: "name", notNull: false }, (after) =>
      expect(entity(after, "users")!.columns!.find((c) => c.name === "name")?.not_null).toBe(false),
    );
    roundTrip(
      { kind: "setDefault", table: t("users"), column: "name", default: "'anon'" },
      (after) =>
        expect(entity(after, "users")!.columns!.find((c) => c.name === "name")?.default).toBe(
          "'anon'",
        ),
    );
    roundTrip({ kind: "setComment", table: t("users"), comment: "People." }, (after) =>
      expect(entity(after, "users")!.comment).toBe("People."),
    );
    roundTrip(
      { kind: "setComment", table: t("users"), column: "email", comment: "Lowercase." },
      (after) => expect(entity(after, "users")!.columns![1]!.comment).toBe("Lowercase."),
    );
    roundTrip(
      {
        kind: "setType",
        table: t("users"),
        column: "name",
        type: customType("varchar(9)")!,
        using: false,
      },
      (after) =>
        expect(entity(after, "users")!.columns!.find((c) => c.name === "name")?.type).toBe(
          "character varying(9)",
        ),
    );
  });

  test("a foreign key draws an edge from the new constraint", () => {
    const plan = foreignKey(blog, t("comments"), "body", t("tags"), "name", "n");
    if (!plan.ok) throw new Error(plan.reason);
    roundTrip(plan.edit, (after) => {
      const fk = entity(after, "comments")!.constraints!.at(-1)!;
      expect(fk).toMatchObject({ kind: "f", name: "comments_body_fkey", on_delete: "n" });
      expect(fk.definition).toBe(
        "FOREIGN KEY (body) REFERENCES public.tags(name) ON DELETE SET NULL",
      );
    });
  });

  test("an expression's default cannot be recognised in the compiled model", () => {
    const edit: GraphEdit = {
      kind: "setDefault",
      table: t("users"),
      column: "name",
      default: "'x'",
    };
    const compiled = applyEdit(blog, { ...edit, default: "'x'::text" });
    expect(reflects(compiled, edit)).toBeNull();
  });
});

describe("marks and layout", () => {
  test("marks sit on the result of the edit, or on what a drop drops", () => {
    const rename: GraphEdit = { kind: "renameColumn", table: t("users"), column: "id", to: "uid" };
    expect(markOf(rename)).toEqual({ table: "public.users", column: "uid" });
    const table: GraphEdit = { kind: "renameTable", table: t("users"), to: "people" };
    expect(markOf(table)).toEqual({ table: "public.people" });
    expect(markOf({ kind: "dropColumn", table: t("users"), column: "id", cascade: true })).toEqual({
      table: "public.users",
      column: "id",
    });
  });

  test("renames move the layout, new tables are placed, dropped ones stay put", () => {
    expect(layoutChange({ kind: "renameTable", table: t("users"), to: "people" })).toEqual({
      rename: ["public.users", "public.people"],
    });
    expect(layoutChange({ kind: "addTable", table: t("x") }, { x: 1, y: 2 })).toEqual({
      place: { key: "public.x", x: 1, y: 2 },
    });
    expect(layoutChange({ kind: "dropTable", table: t("x"), cascade: false })).toEqual({});
    expect(layoutChange({ kind: "renameColumn", table: t("x"), column: "a", to: "b" })).toEqual({});
  });
});
