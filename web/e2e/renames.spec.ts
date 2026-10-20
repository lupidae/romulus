import { expect, type Locator, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// Renames in the migration (task MIG2): a column renamed in the SQL editor
// is proposed as a rename, on; the migration renames it. Turned off, it is
// a drop and an add, flagged destructive. The choice is the draft's, and
// the version keeps it: its migration from the version before is the one
// seen at publish.

const V1 = "CREATE TABLE users (id bigint PRIMARY KEY, email text NOT NULL UNIQUE, note text);\n";
const RENAMED =
  "CREATE TABLE users (id bigint PRIMARY KEY, email_address text NOT NULL UNIQUE, note text);";

async function call(page: Page, method: string, path: string, data?: unknown) {
  const origin = new URL(page.url()).origin;
  const response = await page.request.fetch(path, {
    method,
    data,
    headers: { "X-Romulus-Csrf": "1", Origin: origin },
  });
  expect(response.ok(), `${method} ${path}: ${response.status()}`).toBe(true);
}

const panel = (page: Page, name: string) => page.getByRole("tabpanel", { name });

/**
 * A migration file's whole text: CodeMirror renders only the lines in view
 * (its content element carries the view, `cmTile.view`).
 */
const fileOf = (scope: Locator) =>
  scope
    .locator(".cm-content")
    .first()
    .evaluate(
      (element) =>
        (
          element as unknown as {
            cmTile?: { view?: { state: { doc: { toString(): string } } } };
          }
        ).cmTile?.view?.state.doc.toString() ?? "",
    );

test("a column renamed in the SQL is a rename, unless turned off; the version keeps the choice", async ({
  page,
}) => {
  test.slow();
  await devSignIn(page, `renamer-${Date.now().toString(36)}@romulus.test`);
  const slug = await createOrg(page, unique("Renames"));
  await call(page, "POST", `/api/orgs/${slug}/databases`, { name: "Shop", slug: "shop" });
  await call(page, "POST", `/api/orgs/${slug}/databases/shop/versions`, { sql: V1 });

  await page.goto(`./${slug}/shop`);
  await page.getByRole("button", { name: "New draft" }).first().click();
  await page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");

  // Rename the column in the text, as one edits a CREATE TABLE.
  await panel(page, "SQL").locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.type(RENAMED);
  await expect(page.locator('.react-flow__node [data-column="email_address"]')).toBeVisible();
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");

  // The migration proposes it, on: RENAME COLUMN, its key renamed along.
  await page.getByRole("tab", { name: "Migration" }).click();
  const migration = panel(page, "Migration");
  const rename = migration.getByRole("switch", { name: "Renamed email to email_address?" });
  await expect(rename).toBeChecked();
  await expect(migration.getByRole("region", { name: "Renames" })).toContainText(
    "users_email_key → users_email_address_key",
  );
  await expect
    .poll(() => fileOf(migration))
    .toContain("ALTER TABLE public.users RENAME COLUMN email TO email_address;");
  expect(await fileOf(migration)).not.toContain("DROP COLUMN");
  await expect(migration.getByRole("button", { name: /destructive/ })).toHaveCount(0);

  // Turned off: dropped and added, the data lost, said three ways.
  await rename.click();
  await expect(rename).not.toBeChecked();
  await expect
    .poll(() => fileOf(migration))
    .toContain("ALTER TABLE public.users DROP COLUMN email;");
  expect(await fileOf(migration)).toContain("ADD COLUMN email_address text NOT NULL");
  await expect(migration.getByRole("button", { name: /destructive/ }).first()).toBeVisible();
  await expect(migration).toContainText(
    "Off: email is dropped and email_address added, losing the column's data.",
  );
  await expect(migration).toContainText("looks like a rename");

  // On again: the rename is back.
  await rename.click();
  await expect(rename).toBeChecked();
  await expect.poll(() => fileOf(migration)).not.toContain("DROP COLUMN email;");

  // Publishing: the change list compares by name, and says the migration
  // renames instead.
  await page.getByRole("button", { name: "Publish v2" }).click();
  const dialog = page.getByRole("dialog", { name: "Publish v2" });
  await expect(dialog).toContainText(
    "A rename below reads as removed and added; the migration keeps its data.",
  );
  await dialog.getByRole("button", { name: "See the migration" }).click();
  // Switched off there, for everyone; Publish waits for the migration to follow.
  const inDialog = dialog.getByRole("switch", { name: "Renamed email to email_address?" });
  await expect(inDialog).toBeChecked();
  await inDialog.click();
  await expect(inDialog).not.toBeChecked();
  await expect.poll(() => fileOf(dialog)).toContain("ALTER TABLE public.users DROP COLUMN email;");
  await dialog.getByRole("textbox", { name: "Message" }).fill("Email address");
  const confirm = dialog.getByRole("button", { name: "Publish v2" });
  await expect(confirm).toBeEnabled();
  await confirm.click();
  await expect(page.getByText("Published v2")).toBeVisible();

  await page.goto(`./${slug}/shop/diff/1...2?view=migration`);
  const kept = page.getByRole("switch", { name: "Renamed email to email_address?" });
  await expect(kept).not.toBeChecked();
  await expect.poll(() => fileOf(page.locator("body"))).toContain("DROP COLUMN email;");
  // Someone reading it may try it on: the migration between the versions
  // renames then.
  await kept.click();
  await expect
    .poll(() => fileOf(page.locator("body")))
    .toContain("ALTER TABLE public.users RENAME COLUMN email TO email_address;");
});
