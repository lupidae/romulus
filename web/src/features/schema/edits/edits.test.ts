import { appendTo, MARKER } from "./append";
import { describe as describeEdit, type GraphEdit, sqlFor, type Target } from "./edits";
import { QUOTED_KEYWORDS } from "./keywords";
import { nameProblem, qualified, quoteIdent, quoteLiteral } from "./quote";
import { COMMON_TYPES, customType, type TypeChoice } from "./types";

const users: Target = { schema: "public", name: "users", kind: "r" };
const type = (sql: string): TypeChoice => customType(sql)!;

describe("quoteIdent", () => {
  test("leaves plain lowercase names bare", () => {
    expect(quoteIdent("users")).toBe("users");
    expect(quoteIdent("_private2")).toBe("_private2");
    // Unreserved keywords read back as themselves.
    expect(quoteIdent("name")).toBe("name");
    expect(quoteIdent("comment")).toBe("comment");
    expect(quoteIdent("action")).toBe("action");
  });

  test("quotes mixed case, digits first, spaces, dollars, dots and non-ASCII", () => {
    expect(quoteIdent("Users")).toBe('"Users"');
    expect(quoteIdent("userId")).toBe('"userId"');
    expect(quoteIdent("2fa")).toBe('"2fa"');
    expect(quoteIdent("first name")).toBe('"first name"');
    expect(quoteIdent("price$")).toBe('"price$"');
    expect(quoteIdent("a.b")).toBe('"a.b"');
    expect(quoteIdent("café")).toBe('"café"');
    expect(quoteIdent("")).toBe('""');
  });

  test("doubles inner double quotes", () => {
    expect(quoteIdent('say "hi"')).toBe('"say ""hi"""');
    expect(quoteIdent('"')).toBe('""""');
  });

  test("quotes reserved, column-name and type/function-name keywords", () => {
    expect(quoteIdent("user")).toBe('"user"');
    expect(quoteIdent("order")).toBe('"order"');
    expect(quoteIdent("select")).toBe('"select"');
    // Column-name keywords.
    expect(quoteIdent("int")).toBe('"int"');
    expect(quoteIdent("timestamp")).toBe('"timestamp"');
    expect(quoteIdent("values")).toBe('"values"');
    // Type/function-name keywords.
    expect(quoteIdent("left")).toBe('"left"');
    expect(quoteIdent("collation")).toBe('"collation"');
  });

  test("knows PostgreSQL 17's keyword list", () => {
    expect(QUOTED_KEYWORDS.size).toBe(164);
    expect(QUOTED_KEYWORDS.has("json_table")).toBe(true);
    expect(QUOTED_KEYWORDS.has("merge_action")).toBe(true);
    expect(QUOTED_KEYWORDS.has("text")).toBe(false);
  });

  test("qualifies with the schema, each part quoted on its own", () => {
    expect(qualified({ schema: "public", name: "users" })).toBe("public.users");
    expect(qualified({ schema: "Billing", name: "order" })).toBe('"Billing"."order"');
    expect(qualified({ schema: "a.b", name: "c" })).toBe('"a.b".c');
  });
});

describe("quoteLiteral", () => {
  test("doubles single quotes and keeps everything else", () => {
    expect(quoteLiteral("plain")).toBe("'plain'");
    expect(quoteLiteral("it's")).toBe("'it''s'");
    expect(quoteLiteral("C:\\path")).toBe("'C:\\path'");
    expect(quoteLiteral("two\nlines")).toBe("'two\nlines'");
  });
});

describe("nameProblem", () => {
  test("refuses empty names and names Postgres would truncate", () => {
    expect(nameProblem("")).toMatch(/name/);
    expect(nameProblem("x".repeat(63))).toBeNull();
    expect(nameProblem("x".repeat(64))).toMatch(/63 bytes/);
    // Bytes, not characters: 32 two-byte letters are 64 bytes.
    expect(nameProblem("é".repeat(32))).toMatch(/63 bytes/);
    expect(nameProblem("a\u0000b")).toMatch(/NUL/);
  });
});

describe("sqlFor", () => {
  const cases: [string, GraphEdit][] = [
    ["rename a table", { kind: "renameTable", table: users, to: "members" }],
    [
      "rename a view",
      { kind: "renameTable", table: { schema: "public", name: "v", kind: "v" }, to: "w" },
    ],
    [
      "rename a materialized view",
      { kind: "renameTable", table: { schema: "public", name: "m", kind: "m" }, to: "Totals" },
    ],
    ["rename a column", { kind: "renameColumn", table: users, column: "email", to: "e-mail" }],
    ["add a table", { kind: "addTable", table: { schema: "Sales", name: "order" } }],
    [
      "add a column",
      {
        kind: "addColumn",
        table: users,
        column: { name: "nickname", type: COMMON_TYPES[0]!, notNull: false, default: null },
      },
    ],
    [
      "add a NOT NULL column with a default",
      {
        kind: "addColumn",
        table: { schema: "public", name: "User Accounts", kind: "r" },
        column: { name: "createdAt", type: type("timestamptz"), notNull: true, default: "now()" },
      },
    ],
    [
      "add a column of an enum",
      {
        kind: "addColumn",
        table: users,
        column: {
          name: "status",
          type: { ...type("x"), sql: '"Billing".post_status', typeKind: "e" },
          notNull: true,
          default: "'draft'",
        },
      },
    ],
    ["drop a table", { kind: "dropTable", table: users, cascade: false }],
    ["drop a table others depend on", { kind: "dropTable", table: users, cascade: true }],
    [
      "drop a view",
      { kind: "dropTable", table: { schema: "public", name: "v", kind: "v" }, cascade: false },
    ],
    ["drop a column", { kind: "dropColumn", table: users, column: "email", cascade: false }],
    [
      "drop a column with dependents",
      { kind: "dropColumn", table: users, column: "user", cascade: true },
    ],
    [
      "change a type",
      { kind: "setType", table: users, column: "name", type: type("varchar(120)"), using: false },
    ],
    [
      "change a type with a cast",
      { kind: "setType", table: users, column: "Age", type: type("integer"), using: true },
    ],
    ["require a value", { kind: "setNotNull", table: users, column: "email", notNull: true }],
    ["allow nulls", { kind: "setNotNull", table: users, column: "email", notNull: false }],
    ["set a default", { kind: "setDefault", table: users, column: "role", default: "'member'" }],
    ["drop a default", { kind: "setDefault", table: users, column: "role", default: null }],
    ["comment a table", { kind: "setComment", table: users, comment: "People who sign in." }],
    [
      "comment a view",
      { kind: "setComment", table: { schema: "public", name: "v", kind: "v" }, comment: "x" },
    ],
    [
      "comment a materialized view with a quote",
      { kind: "setComment", table: { schema: "public", name: "m", kind: "m" }, comment: "Bob's" },
    ],
    [
      "comment a column",
      { kind: "setComment", table: users, column: "email", comment: "Unique, lowercase." },
    ],
    ["remove a comment", { kind: "setComment", table: users, column: "email", comment: null }],
    [
      "add a foreign key",
      {
        kind: "addForeignKey",
        table: { schema: "public", name: "orders", kind: "r" },
        column: "user_id",
        references: { schema: "public", name: "users" },
        refColumn: "id",
        name: "orders_user_id_fkey",
        onDelete: "a",
      },
    ],
    [
      "add a foreign key that cascades",
      {
        kind: "addForeignKey",
        table: { schema: "shop", name: "Order", kind: "r" },
        column: "user",
        references: { schema: "auth", name: "users" },
        refColumn: "id",
        name: "Order_user_fkey",
        onDelete: "c",
      },
    ],
    [
      "add a foreign key that sets null",
      {
        kind: "addForeignKey",
        table: users,
        column: "manager_id",
        references: { schema: "public", name: "users" },
        refColumn: "id",
        name: "users_manager_id_fkey",
        onDelete: "n",
      },
    ],
  ];

  test.each(cases)("%s", (_, edit) => {
    const statements = sqlFor(edit);
    expect(statements.every((s) => s.endsWith(";"))).toBe(true);
    expect(statements.join("\n")).toMatchSnapshot();
    expect(describeEdit(edit)).toMatchSnapshot();
  });
});

describe("appendTo", () => {
  const apply = (text: string, statements: string[]) => {
    const a = appendTo(text, statements);
    const result = text.slice(0, a.at) + a.insert + text.slice(a.at);
    return { result, statements: result.slice(a.from, a.to) };
  };

  test("writes the marker once, after a blank line, then the statements", () => {
    const first = apply("CREATE TABLE a (id int);\n", ["ALTER TABLE public.a RENAME TO b;"]);
    expect(first.result).toBe(
      `CREATE TABLE a (id int);\n\n${MARKER}\nALTER TABLE public.a RENAME TO b;\n`,
    );
    expect(first.statements).toBe("ALTER TABLE public.a RENAME TO b;");
    const second = apply(first.result, ["COMMENT ON TABLE public.b IS 'x';"]);
    expect(second.result).toBe(`${first.result}COMMENT ON TABLE public.b IS 'x';\n`);
    expect(second.statements).toBe("COMMENT ON TABLE public.b IS 'x';");
  });

  test("ends an unterminated last line first", () => {
    expect(apply("CREATE TABLE a (id int);", ["X;"]).result).toBe(
      `CREATE TABLE a (id int);\n\n${MARKER}\nX;\n`,
    );
    expect(apply(`A;\n\n${MARKER}\nX;`, ["Y;"]).result).toBe(`A;\n\n${MARKER}\nX;\nY;\n`);
  });

  test("needs no blank line when there is one, nor any text", () => {
    expect(apply("A;\n\n", ["X;"]).result).toBe(`A;\n\n${MARKER}\nX;\n`);
    expect(apply("", ["X;"]).result).toBe(`${MARKER}\nX;\n`);
  });

  test("appends at the very end, below whatever was typed under the marker", () => {
    const text = `A;\n\n${MARKER}\nX;\n\n-- typed later\nCREATE TABLE z ();\n`;
    expect(apply(text, ["Y;", "Z;"]).result).toBe(`${text}Y;\nZ;\n`);
  });

  test("finds the marker only as a whole line", () => {
    const text = `-- not ${MARKER}\nA;\n`;
    expect(apply(text, ["X;"]).result).toBe(`${text}\n${MARKER}\nX;\n`);
  });
});
