import { readFileSync } from "node:fs";

import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

const V1 = `CREATE TABLE customers (
  id bigint PRIMARY KEY,
  email text NOT NULL,
  legacy_code text
);
CREATE TABLE orders (
  id bigint PRIMARY KEY,
  customer_id bigint REFERENCES customers (id),
  note text
);
`;
// customers.legacy_code dropped, orders.customer_id required: destructive.
const V2 = `CREATE TABLE customers (
  id bigint PRIMARY KEY,
  email text NOT NULL,
  name text
);
CREATE TABLE orders (
  id bigint PRIMARY KEY,
  customer_id bigint NOT NULL REFERENCES customers (id),
  note text
);
CREATE TABLE invoices (
  id bigint PRIMARY KEY,
  order_id bigint REFERENCES orders (id)
);
`;

/** The migration's SQL as the editor shows it (line breaks are lost; enough to find statements). */
const sql = (page: Page) => page.locator(".cm-content");

async function shop(page: Page) {
  await devSignIn(page, `migration-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Migrations"));
  await page.goto(`./${slug}`);
  await page.getByRole("button", { name: "New database" }).click();
  const create = page.getByRole("dialog", { name: "New database" });
  await create.getByLabel("Name").fill("Shop");
  await create.getByRole("button", { name: "Create database" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/shop$`));
  const headers = { "X-Romulus-Csrf": "1", Origin: new URL(page.url()).origin };
  for (const [source, message] of [
    [V1, "Customers and orders"],
    [V2, "Add invoices"],
  ]) {
    const upload = await page.request.post(`/api/orgs/${slug}/databases/shop/versions`, {
      headers,
      data: { sql: source, message },
    });
    expect(upload.status()).toBe(201);
  }
  return slug;
}

test("a migration between two versions: reviewed, laid out for a tool, copied and downloaded", async ({
  page,
  context,
}) => {
  test.slow();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const slug = await shop(page);

  await page.goto(`./${slug}/shop/diff/1...2`);
  await page.getByRole("radio", { name: "Migration" }).click();
  await expect(page).toHaveURL(/view=migration/);
  await expect(page.getByText("5 statements", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: /^2 destructive/ })).toBeVisible();
  await expect(sql(page)).toContainText(
    "ALTER TABLE public.orders ALTER COLUMN customer_id SET NOT NULL;",
  );
  await expect(sql(page)).toContainText("BEGIN;");
  // Destructive statements are tinted, and the button goes through them.
  await expect(page.locator(".cm-statement-danger").first()).toBeAttached();
  await page.getByRole("button", { name: /^2 destructive/ }).click();
  await expect(page.getByRole("button", { name: /^2 destructive/ })).toContainText("1/2");
  await test.info().attach("migration, plain SQL", {
    body: await page.screenshot(),
    contentType: "image/png",
  });

  // Copy puts the file on the clipboard.
  await page.getByRole("button", { name: /^Copy / }).click();
  await expect(page.getByRole("button", { name: "Copied" })).toBeVisible();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain("-- Migration of");
  expect(copied).toContain("CREATE TABLE public.invoices");
  // Below what the editor renders: the removals come last.
  expect(copied).toMatch(/Removals\. DESTRUCTIVE[\s\S]*DROP COLUMN legacy_code;\n\nCOMMIT;/);

  // Download: the file, named like the tool names it.
  const plain = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download", exact: true }).click();
  const file = await plain;
  expect(file.suggestedFilename()).toMatch(/^\d{14}_add_invoices\.sql$/);
  const content = readFileSync(await file.path(), "utf8");
  expect(content).toContain("DESTRUCTIVE");
  expect(content).toContain("COMMIT;");

  // golang-migrate: an up and a down file, downloaded together.
  await page.getByRole("combobox", { name: "Tool" }).click();
  await page.getByRole("option", { name: /^golang-migrate/ }).click();
  await expect(page.getByRole("tab", { name: /\.up\.sql$/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.getByRole("tab", { name: /\.down\.sql/ }).click();
  await expect(sql(page)).toContainText("DROP TABLE public.invoices;");
  const zipped = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download .zip" }).click();
  const archive = await zipped;
  expect(archive.suggestedFilename()).toMatch(/^\d{14}_add_invoices\.zip$/);
  const bytes = readFileSync(await archive.path());
  expect(bytes.subarray(0, 4)).toEqual(Buffer.from([0x50, 0x4b, 0x03, 0x04]));
  expect(bytes.toString("latin1")).toContain("_add_invoices.down.sql");

  // The choice is remembered, and the address keeps the view.
  await page.reload();
  await expect(page.getByRole("combobox", { name: "Tool" })).toHaveText("golang-migrate");
  await page.getByRole("radio", { name: "Changes" }).click();
  await expect(page).not.toHaveURL(/view=migration/);
  await expect(page.getByText("1 table added, 2 changed")).toBeVisible();
  expect(errors).toEqual([]);
});

test("a draft's publish dialog shows the migration it would call for", async ({ page }) => {
  test.slow();
  const slug = await shop(page);
  await page.goto(`./${slug}/shop`);
  await page.getByRole("button", { name: "New draft" }).first().click();
  await page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");

  await sql(page).click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type("\nCREATE TABLE coupons (code text PRIMARY KEY);\n", { delay: 10 });
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");

  await page.getByRole("button", { name: "Publish v3" }).click();
  const dialog = page.getByRole("dialog", { name: "Publish v3" });
  await dialog.getByRole("tab", { name: "Migration" }).click();
  await expect(dialog.locator(".cm-content")).toContainText("CREATE TABLE public.coupons (");
  await expect(dialog.getByText("1 statement", { exact: true })).toBeVisible();
  // Publishing stays the one primary action.
  await expect(dialog.getByRole("button", { name: "Download" })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Publish v3" })).toBeEnabled();
});
