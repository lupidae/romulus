import { fileURLToPath } from "node:url";

import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

/** The sqlx fixture history (12 migrations), as a folder on disk. */
const SQLX = fileURLToPath(
  new URL("../../crates/romulus-sandbox/tests/migrations/sqlx/migrations", import.meta.url),
);

async function newDatabase(page: Page, name: string) {
  await page.getByRole("button", { name: "New database" }).click();
  const create = page.getByRole("dialog", { name: "New database" });
  await create.getByLabel("Name").fill(name);
  await create.getByRole("button", { name: "Create database" }).click();
  await expect(page.getByRole("heading", { name: "No versions yet" })).toBeVisible();
}

test("a migrations folder becomes the history, one version per migration", async ({ page }) => {
  test.slow();
  await devSignIn(page, `importer-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Imports"));
  await page.goto(`./${slug}`);
  await newDatabase(page, "Tracker");

  // From the empty database, to the import page.
  await page.getByRole("link", { name: "Import migrations" }).first().click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/tracker/import$`));
  await expect(page.getByRole("heading", { name: "Import migrations" })).toBeVisible();

  // The folder: planned, in order, with what each one skips.
  await page.locator("input[type=file][webkitdirectory]").setInputFiles(SQLX);
  await expect(page.getByText("12 migrations", { exact: true })).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Read as" })).toHaveText("sqlx");
  const list = page.getByRole("list", { name: "Migrations, in order" });
  await expect(list.getByRole("listitem")).toHaveCount(12);
  const seed = list.getByRole("listitem").filter({ hasText: "20240126100900_seed_acme.sql" });
  await seed.getByRole("button", { name: "2 skipped" }).click();
  await expect(seed.getByRole("list", { name: "Not replayed" })).toContainText(
    "INSERT INTO organizations",
  );

  // Imported: twelve versions, each with what it changed.
  await page.getByRole("button", { name: "Import 12 migrations" }).click();
  await expect(page.getByText("12 versions created: v1 to v12")).toBeVisible({ timeout: 60_000 });
  await expect(list.getByRole("listitem").first()).toContainText("2 tables added");

  // The history lists them, the migration file names as messages.
  await page.getByRole("link", { name: "View history" }).click();
  const versions = page.getByRole("list", { name: "Versions" });
  await expect(versions.getByRole("listitem")).toHaveCount(12);
  await expect(versions.getByRole("listitem").first()).toContainText(
    "20240322115300_projects_archived_at.sql",
  );

  // The diff between two imported versions: the rename.
  await page.getByRole("link", { name: "Compare v4 → v5" }).click();
  await expect(page).toHaveURL(/\/diff\/4\.\.\.5$/);
  await expect(page.getByText("1 table added, 1 changed, 1 removed")).toBeVisible();

  // Importing the same folder again creates nothing.
  await page.goto(`./${slug}/tracker/import`);
  await page.locator("input[type=file][webkitdirectory]").setInputFiles(SQLX);
  await expect(page.getByText("Every migration here is imported already")).toBeVisible();
  await expect(page.getByRole("button", { name: "Nothing to import" })).toBeDisabled();
});

test("a migration that does not replay stops the import where it fails", async ({ page }) => {
  await devSignIn(page, `failer-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Failures"));
  await page.goto(`./${slug}`);
  await newDatabase(page, "Billing");
  await page.goto(`./${slug}/billing/import`);

  const file = (name: string, text: string) => ({
    name,
    mimeType: "application/sql",
    buffer: Buffer.from(text),
  });
  await page
    .locator("input[type=file][multiple]:not([webkitdirectory])")
    .setInputFiles([
      file(
        "20240101000000_customers.sql",
        "-- migrate:up\nCREATE TABLE customers (id bigint PRIMARY KEY);\n-- migrate:down\nDROP TABLE customers;\n",
      ),
      file(
        "20240102000000_invoices.sql",
        "-- migrate:up\nCREATE TABLE invoices (\n  id bigint PRIMARY KEY,\n  plan_id bigint REFERENCES plans (id)\n);\n-- migrate:down\nDROP TABLE invoices;\n",
      ),
    ]);
  await expect(page.getByRole("combobox", { name: "Read as" })).toHaveText("dbmate");
  await page.getByRole("button", { name: "Import 2 migrations" }).click();

  await expect(page.getByText("20240102000000_invoices.sql does not replay")).toBeVisible({
    timeout: 60_000,
  });
  await expect(page.getByText('relation "plans" does not exist')).toBeVisible();
  await expect(page.getByLabel("Where it failed")).toContainText("plan_id bigint REFERENCES plans");
  // Nothing was stored.
  await expect(page.getByText("No versions yet")).toBeVisible();
});
