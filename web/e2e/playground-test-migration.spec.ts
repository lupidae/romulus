import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// Test migration: from a draft's Migration tab, one click runs the
// migration up and down on a playground of its own and opens its report.
// A migration that keeps the rows passes; one that sets NOT NULL on a
// column holding NULLs fails, and the report names the statement and
// Postgres's error. Needs the playground runner and service (`just play`,
// or `just dev`); the name has "playground" in it so Playwright's config
// starts them.

const V1 = `CREATE TABLE customers (
  id bigint PRIMARY KEY,
  email text NOT NULL,
  nickname text
);
`;

async function databaseAtV1(page: Page): Promise<string> {
  // Both tests sign in at once: an address of their own each.
  const who = unique("mtest").replace(" ", "-");
  await devSignIn(page, `${who}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Tests"));
  const headers = { "X-Romulus-Csrf": "1", Origin: new URL(page.url()).origin };
  expect(
    (
      await page.request.post(`/api/orgs/${slug}/databases`, {
        headers,
        data: { name: "Shop", slug: "shop" },
      })
    ).status(),
  ).toBe(201);
  expect(
    (
      await page.request.post(`/api/orgs/${slug}/databases/shop/versions`, {
        headers,
        data: { sql: V1, message: "Customers" },
      })
    ).status(),
  ).toBe(201);
  return slug;
}

/** A new draft whose SQL ends with `change`, compiled. */
async function draftWith(page: Page, slug: string, change: (text: string) => string) {
  await page.goto(`./${slug}/shop`);
  await page.getByRole("button", { name: "New draft" }).first().click();
  await page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");
  const sql = change(V1);
  await page.locator(".cm-content").first().click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Delete");
  await page.keyboard.insertText(sql);
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");
}

async function testOnData(page: Page) {
  await page.getByRole("tab", { name: "Migration" }).click();
  await page.getByRole("button", { name: "Test on data" }).click();
  const panel = page.getByRole("dialog", { name: "Migration test" });
  await expect(panel).toBeVisible();
  return panel;
}

test("a draft's migration tested on data passes, up and down", async ({ page }) => {
  test.slow();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const slug = await databaseAtV1(page);
  await draftWith(page, slug, (sql) => `${sql}CREATE TABLE invoices (id bigint PRIMARY KEY);\n`);
  const panel = await testOnData(page);
  // Waiting, building, running: then the verdict.
  await expect(panel.getByRole("heading", { name: /^Up and down ran on/ })).toBeVisible({
    timeout: 180_000,
  });
  await expect(panel.getByText("Passed", { exact: true }).first()).toBeVisible();
  // Each step with its status (read out after its name).
  for (const step of ["Up migration", "Checks after up", "Down migration", "Checks after down"]) {
    await expect(panel.getByText(`${step}: Passed`)).toBeVisible();
  }
  // Each statement's locks, from its step.
  await panel.getByRole("button", { name: "Show the details of Up migration" }).click();
  await expect(panel.getByText(/CREATE TABLE public\.invoices/)).toBeVisible();
  // The report has a page of its own.
  await panel.getByRole("link", { name: "Open as a page" }).click();
  await page.waitForURL(/\/tests\/[0-9a-f-]+$/);
  await expect(page.getByRole("heading", { name: /^Up and down ran on/ })).toBeVisible();
  await expect(page.getByRole("button", { name: "Copy a link to this report" })).toBeVisible();
  expect(errors).toEqual([]);
});

test("a migration that fails on the rows shows the statement and Postgres's error", async ({
  page,
}) => {
  test.slow();
  const slug = await databaseAtV1(page);
  await draftWith(page, slug, (sql) => sql.replace("nickname text", "nickname text NOT NULL"));
  const panel = await testOnData(page);
  await expect(
    panel.getByRole("heading", { name: /^Failed at statement 1 of 1 of the up migration/ }),
  ).toBeVisible({ timeout: 180_000 });
  // The failing step is open: the statement, the error, the rows.
  await expect(panel.getByText(/SET NOT NULL/)).toBeVisible();
  await expect(panel.getByText(/23502/)).toBeVisible();
  // In the headline, and under the statement.
  await expect(panel.getByText(/contains null values/)).toHaveCount(2);
  await expect(panel.getByText(/rows? breaks? it/)).toBeVisible();
  // The down migration did not run.
  await expect(panel.getByText("Down migration: Not run")).toBeVisible();
  await expect(panel.getByText("Not run: an earlier step failed.").first()).toBeVisible();
});
