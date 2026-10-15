import { appendTo } from "@/features/schema/edits/append";
import { applyEdit } from "@/features/schema/edits/apply";
import { describe as label, type GraphEdit, sqlFor } from "@/features/schema/edits/edits";
import blogJson from "@/features/schema/fixtures/blog.json";
import { parseSchema, type Schema } from "@/features/schema/model";

import {
  type CompileFacts,
  displayModel,
  type EditEntry,
  judge,
  marksOf,
  touchedEntities,
} from "./graph-edits";
import type { Diagnostic } from "./queries";

const blog = parseSchema(blogJson);
const users = { schema: "public", name: "users", kind: "r" as const };
const SQL = "CREATE TABLE users ();\n";

let ids = 0;
function made(edit: GraphEdit, model: Schema = blog): EditEntry {
  return {
    id: ++ids,
    edit,
    sql: sqlFor(edit).join("\n"),
    touched: touchedEntities(model, applyEdit(model, edit)),
  };
}
function withEdits(text: string, ...entries: EditEntry[]) {
  for (const e of entries) {
    const a = appendTo(text, [e.sql]);
    text = text.slice(0, a.at) + a.insert;
  }
  return text;
}
const compile = (patch: Partial<CompileFacts>): CompileFacts => ({
  state: "ok",
  stale: true,
  diagnostics: [],
  model: blog,
  ...patch,
});
const error = (text: string, needle: string, message: string): Diagnostic => {
  const at = text.indexOf(needle);
  return {
    code: "sql",
    message,
    severity: "error",
    span: {
      start: at,
      end: at + needle.length,
      utf16_start: at,
      utf16_end: at + needle.length,
      from: { line: 1, column: 1, offset: at },
      to: { line: 1, column: 1, offset: at },
    },
  } as unknown as Diagnostic;
};
const columnsOf = (model: Schema | null, table: string) =>
  model?.entities?.find((e) => e.name === table)?.columns?.map((c) => c.name);

describe("judging graph edits", () => {
  const rename = made({ kind: "renameColumn", table: users, column: "name", to: "full_name" });

  test("pending until the model shows it, then nothing to do", () => {
    const text = withEdits(SQL, rename);
    const states = judge([rename], text, compile({}));
    expect(states.get(rename.id)).toEqual({ status: "pending" });
    const shown = displayModel(blog, [rename], states);
    expect(columnsOf(shown, "users")).toContain("full_name");
    expect(
      marksOf([rename], states, label).byElement.get("public.users\u0000full_name"),
    ).toMatchObject({
      status: "pending",
    });

    // A stale compile that already has it (someone typed after): settled.
    const compiled = applyEdit(blog, rename.edit);
    expect(judge([rename], text, compile({ model: compiled })).get(rename.id)).toEqual({
      status: "idle",
    });
    // The compile of this very text: settled.
    expect(judge([rename], text, compile({ stale: false })).get(rename.id)).toEqual({
      status: "idle",
    });
  });

  test("failed when this text's compile errs inside its statements", () => {
    const text = withEdits(SQL, rename);
    const diagnostics = [error(text, "full_name", 'column "full_name" already exists')];
    const states = judge([rename], text, compile({ state: "failed", stale: false, diagnostics }));
    expect(states.get(rename.id)).toEqual({
      status: "failed",
      error: 'column "full_name" already exists',
    });
    // Still shown as edited (nothing jumps), the error on it.
    expect(columnsOf(displayModel(blog, [rename], states), "users")).toContain("full_name");
    expect(
      marksOf([rename], states, label).byElement.get("public.users\u0000full_name"),
    ).toMatchObject({
      status: "failed",
      label: "Rename users.name to full_name",
    });
  });

  test("an error elsewhere leaves it pending", () => {
    const text = withEdits(`${SQL}CREATE TABLE broken (;\n`, rename);
    const diagnostics = [error(text, "(;", "syntax error")];
    const states = judge([rename], text, compile({ state: "failed", stale: false, diagnostics }));
    expect(states.get(rename.id)).toEqual({ status: "pending" });
  });

  test("undone: shown as before until the model forgets it, and back on redo", () => {
    const compiled = applyEdit(blog, rename.edit);
    // Undone: the text no longer holds it, the model still does.
    const states = judge([rename], SQL, compile({ model: compiled }));
    expect(states.get(rename.id)).toEqual({ status: "reverting" });
    const shown = displayModel(compiled, [rename], states);
    expect(columnsOf(shown, "users")).toContain("name");
    expect(columnsOf(shown, "users")).not.toContain("full_name");
    expect(marksOf([rename], states, label).byElement.size).toBe(0);
    // The model caught up.
    expect(judge([rename], SQL, compile({})).get(rename.id)).toEqual({ status: "idle" });
    // Redone: the statements are back.
    expect(judge([rename], withEdits(SQL, rename), compile({})).get(rename.id)).toEqual({
      status: "pending",
    });
  });

  test("several edits apply in order, the later on the earlier", () => {
    const add = made({ kind: "addTable", table: { schema: "public", name: "invoices" } });
    const afterAdd = applyEdit(blog, add.edit);
    const column = made(
      {
        kind: "renameColumn",
        table: { schema: "public", name: "invoices", kind: "r" },
        column: "id",
        to: "invoice_id",
      },
      afterAdd,
    );
    const text = withEdits(SQL, add, column);
    const states = judge([add, column], text, compile({}));
    expect(columnsOf(displayModel(blog, [add, column], states), "invoices")).toEqual([
      "invoice_id",
    ]);
  });

  test("a dropped table comes back whole when the drop is undone", () => {
    const drop = made({ kind: "dropTable", table: users, cascade: true });
    const compiled = applyEdit(blog, drop.edit);
    expect(compiled.entities?.find((e) => e.name === "users")).toBeUndefined();
    const states = judge([drop], SQL, compile({ model: compiled }));
    expect(states.get(drop.id)).toEqual({ status: "reverting" });
    const shown = displayModel(compiled, [drop], states)!;
    expect(shown.entities?.find((e) => e.name === "users")?.columns).toHaveLength(4);
    // The view and the foreign keys it took came back too.
    expect(shown.entities?.find((e) => e.name === "published_posts")).toBeDefined();
    expect(
      shown.entities?.find((e) => e.name === "posts")?.constraints?.map((c) => c.name),
    ).toContain("posts_author_id_fkey");
  });
});
