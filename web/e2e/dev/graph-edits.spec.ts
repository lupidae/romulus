import { expect, type Page, test } from "@playwright/test";

// Editing the schema from the graph, on the dev editor (/app/dev/editor): a
// draft held in the tab, whose "compile" replays the graph edits found in
// the text onto the blog fixture. The real thing, with the collab server and
// two people, is e2e/drafts.spec.ts.

const node = (page: Page, table: string) =>
  page.locator(`.react-flow__node[data-id="public.${table}"]`);
const row = (page: Page, table: string, column: string) =>
  node(page, table).locator(`[data-column="${column}"]`);
const graph = (page: Page) => page.getByRole("application", { name: "Schema graph" });
/** The SQL, without line wrapping marks. */
const sqlText = (page: Page) =>
  page.evaluate(() => document.querySelector(".cm-content")?.textContent ?? "");

async function open(page: Page, query = "") {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`./dev/editor${query}`);
  await expect(node(page, "users")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");
  return errors;
}

test("a column renamed in place appends its SQL; one undo takes it back", async ({ page }) => {
  const errors = await open(page);
  await row(page, "users", "email").getByText("email", { exact: true }).dblclick();
  const field = page.getByRole("textbox", { name: "Rename email" });
  await expect(field).toBeFocused();
  await field.fill("mail");
  await field.press("Enter");

  await expect(row(page, "users", "mail")).toBeVisible();
  await expect.poll(() => sqlText(page)).toContain("-- edits from the graph");
  await expect
    .poll(() => sqlText(page))
    .toContain("ALTER TABLE public.users RENAME COLUMN email TO mail;");
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");

  await graph(page).focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(row(page, "users", "email")).toBeVisible();
  await expect.poll(() => sqlText(page)).not.toContain("RENAME COLUMN");
  expect(errors).toEqual([]);
});

test("names that cannot be used say why and are not written", async ({ page }) => {
  await open(page);
  await row(page, "users", "name").getByText("name", { exact: true }).dblclick();
  const field = page.getByRole("textbox", { name: "Rename name" });
  await field.fill("email");
  await field.press("Enter");
  await expect(page.getByRole("alert")).toHaveText("users already has a column email.");
  await field.press("Escape");
  await expect(row(page, "users", "name")).toBeVisible();
  expect(await sqlText(page)).not.toContain("-- edits from the graph");
});

test("columns are added, retyped, defaulted and commented from small popovers", async ({
  page,
}) => {
  await open(page);
  // Add: the + of the header.
  await node(page, "users").locator("[data-table-header]").hover();
  await page.getByRole("button", { name: "Add a column to users" }).click();
  const add = page.getByRole("dialog", { name: "Add a column" });
  await add.getByRole("textbox", { name: "Name" }).fill("nickname");
  await add.getByRole("combobox", { name: "Type" }).fill("varch");
  await page.getByRole("option", { name: /varchar\(255\)/ }).click();
  await add.getByRole("checkbox", { name: "Not null" }).click();
  await add.getByRole("textbox", { name: "Default" }).fill("''");
  await add.getByRole("button", { name: "Add column" }).click();
  await expect(row(page, "users", "nickname")).toContainText("varchar(255)");
  await expect
    .poll(() => sqlText(page))
    .toContain(
      "ALTER TABLE public.users ADD COLUMN nickname varchar(255) NOT NULL DEFAULT '';",
    );

  // Retype: the column's menu.
  await row(page, "tags", "name").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Change type…" }).click();
  await page.getByRole("combobox", { name: "New type" }).fill("integer");
  await page.getByRole("combobox", { name: "New type" }).press("Enter");
  await expect(page.getByText("Values are converted with USING name::integer.")).toBeVisible();
  await page.getByRole("button", { name: "Change type" }).click();
  await expect
    .poll(() => sqlText(page))
    .toContain("ALTER TABLE public.tags ALTER COLUMN name TYPE integer USING name::integer;");

  // Comment: the table's menu, saved with ⌘↵.
  await node(page, "tags").locator("[data-table-header]").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Add comment" }).click();
  await page.getByRole("textbox", { name: "Comment" }).fill("Labels for posts, Bob's idea");
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(node(page, "tags")).toContainText("Labels for posts, Bob's idea");
  await expect
    .poll(() => sqlText(page))
    .toContain("COMMENT ON TABLE public.tags IS 'Labels for posts, Bob''s idea';");
});

test("dropping a table says what goes with it, and cascades", async ({ page }) => {
  await open(page);
  await node(page, "users").locator("[data-table-header]").click();
  await page.keyboard.press("Delete");
  const dialog = page.getByRole("dialog", { name: "Drop" });
  await expect(dialog).toContainText("Drop table users?");
  await expect(dialog).toContainText("posts_author_id_fkey");
  await expect(dialog).toContainText("published_posts");
  await dialog.getByRole("button", { name: "Drop table" }).click();
  await expect(node(page, "users")).toHaveCount(0);
  await expect(node(page, "published_posts")).toHaveCount(0);
  await expect.poll(() => sqlText(page)).toContain("DROP TABLE public.users CASCADE;");
  // Undone, it comes back where it was.
  await graph(page).focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(node(page, "users")).toBeVisible();
});

test("a foreign key is drawn from a column's grip to another column", async ({ page }) => {
  await open(page);
  const from = row(page, "comments", "body");
  await from.hover();
  const grip = await from.locator("[data-link-grip]").boundingBox();
  const to = await row(page, "tags", "name").boundingBox();
  await page.mouse.move(grip!.x + grip!.width / 2, grip!.y + grip!.height / 2);
  await page.mouse.down();
  await page.mouse.move(to!.x + 40, to!.y + to!.height / 2, { steps: 10 });
  await expect(row(page, "tags", "name")).toHaveAttribute("data-link-target", "ok");
  await page.mouse.up();

  const dialog = page.getByRole("dialog", { name: "Add a foreign key" });
  await expect(dialog).toContainText("comments.body");
  await expect(dialog).toContainText("tags.name");
  await dialog.getByRole("combobox").click();
  await page.getByRole("option", { name: /Cascade/ }).click();
  await dialog.getByRole("button", { name: "Add foreign key" }).click();
  await expect
    .poll(() => sqlText(page))
    .toContain(
      "ALTER TABLE public.comments ADD CONSTRAINT comments_body_fkey FOREIGN KEY (body) REFERENCES public.tags (name) ON DELETE CASCADE;",
    );
  await expect(row(page, "comments", "body").getByLabel("Foreign key")).toBeVisible();
});

test("a foreign key Postgres could not make is explained, not written", async ({ page }) => {
  await open(page);
  await row(page, "comments", "body").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Add foreign key…" }).click();
  const dialog = page.getByRole("dialog", { name: "Add a foreign key" });
  await dialog.getByRole("combobox", { name: "References" }).fill("users.id");
  await page.getByRole("option", { name: /users\.id/ }).click();
  await expect(dialog.getByRole("alert")).toContainText("Postgres cannot compare them");
  await expect(dialog.getByRole("button", { name: "Add foreign key" })).toBeDisabled();
});

test("a failed edit says why on the element and can be undone there", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Fail the next edit" }).click();
  await row(page, "profiles", "bio").getByText("bio", { exact: true }).dblclick();
  await page.getByRole("textbox", { name: "Rename bio" }).fill("about");
  await page.keyboard.press("Enter");
  const note = page.getByRole("dialog", { name: "This edit failed" });
  await expect(note).toContainText("Rename profiles.bio to about failed");
  await expect(page.getByTestId("compile-status")).toHaveText("1 error");
  await note.getByRole("button", { name: "Undo it" }).click();
  await expect(row(page, "profiles", "bio")).toBeVisible();
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");
});

test("a new table is named in place and starts with an id", async ({ page }) => {
  await open(page);
  await graph(page).focus();
  await page.keyboard.press("n");
  const name = page.getByRole("textbox", { name: "New table name" });
  await expect(name).toHaveValue("new_table");
  await name.fill("invoices");
  await name.press("Enter");
  await expect(row(page, "invoices", "id")).toBeVisible();
  await expect
    .poll(() => sqlText(page))
    .toContain(
      "CREATE TABLE public.invoices (\n    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY\n);".replaceAll(
        "\n",
        "",
      ),
    );
  // Renamed with F2 (it is selected), the place stays.
  const before = await node(page, "invoices").boundingBox();
  await page.keyboard.press("F2");
  await page.getByRole("textbox", { name: "Rename invoices" }).fill("bills");
  await page.keyboard.press("Enter");
  await expect(node(page, "bills")).toBeVisible();
  const after = await node(page, "bills").boundingBox();
  expect(after?.x).toBeCloseTo(before!.x, 0);
  expect(after?.y).toBeCloseTo(before!.y, 0);
});

test("an empty draft asks for its first table", async ({ page }) => {
  await page.goto("./dev/editor?fixture=empty");
  await page.getByRole("textbox", { name: "Table name" }).fill("accounts");
  await page.getByRole("button", { name: "Add table" }).click();
  await expect(row(page, "accounts", "id")).toBeVisible();
});

test("a reviewer gets no edit affordance at all", async ({ page }) => {
  await open(page, "?role=reviewer");
  await node(page, "users").locator("[data-table-header]").hover();
  await expect(page.getByRole("button", { name: "Add a column to users" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add a table" })).toHaveCount(0);
  await row(page, "users", "name").click({ button: "right" });
  await expect(page.getByRole("menu")).toHaveCount(0);
  await row(page, "users", "name").getByText("name", { exact: true }).dblclick();
  await expect(page.getByRole("textbox", { name: "Rename name" })).toHaveCount(0);
  await expect(page.locator("[data-link-grip]")).toHaveCount(0);
  await graph(page).focus();
  await page.keyboard.press("n");
  await expect(page.getByTestId("new-table")).toHaveCount(0);
});
