import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// An exclusion constraint on a partitioned table: Postgres 17 and later only.
const BOOKINGS = `CREATE TABLE rooms (
  id bigint PRIMARY KEY,
  name text NOT NULL
);
CREATE EXTENSION btree_gist;
CREATE TABLE bookings (
  room int NOT NULL,
  during tstzrange NOT NULL,
  EXCLUDE USING gist (room WITH =, during WITH &&)
) PARTITION BY LIST (room);
`;
const ROOMS = BOOKINGS.slice(0, BOOKINGS.indexOf("CREATE EXTENSION"));
const NOT_ON_15 = "exclusion constraints are not supported on partitioned tables";

/** Waits for the API's PostgreSQL 15 compiler (`just sandbox 15`; CI starts it). */
async function postgres15(page: Page): Promise<boolean> {
  for (let i = 0; i < 30; i++) {
    const health = (await (await page.request.get("/api/health")).json()) as {
      sandboxes?: Record<string, string>;
    };
    if (health.sandboxes?.["15"] === "ok") return true;
    await page.waitForTimeout(1000);
  }
  return false;
}

async function upload(page: Page, sql: string) {
  await page.keyboard.press("u");
  const dialog = page.getByRole("dialog", { name: "Upload SQL" });
  const editor = dialog.getByRole("textbox", { name: "SQL to upload" });
  await editor.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.insertText(sql);
  return dialog;
}

test("a database on Postgres 15 refuses what 17 has, until it moves to 17", async ({ page }) => {
  test.slow();
  await devSignIn(page, `pg-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const on15 = await postgres15(page);
  test.skip(!on15 && !process.env.CI, "PostgreSQL 15's sandbox is not running: just sandbox 15");
  expect(on15, "the PostgreSQL 15 sandbox answers").toBe(true);
  const slug = await createOrg(page, unique("Majors"));

  // Created on 15: said under the name.
  await page.goto(`./${slug}`);
  await page.getByRole("button", { name: "New database" }).click();
  const create = page.getByRole("dialog", { name: "New database" });
  await create.getByLabel("Name").fill("Rooms");
  await create.getByRole("combobox", { name: "Postgres version" }).click();
  await page.getByRole("option", { name: /^Postgres 15/ }).click();
  await create.getByRole("button", { name: "Create database" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/rooms$`));
  // The badge sits in the database's title.
  const title = page.getByRole("heading", { level: 1 });
  await expect(title).toContainText("Postgres 15");

  // v1 compiles on 15.
  let dialog = await upload(page, ROOMS);
  await expect(dialog.getByText("Compiles", { exact: true })).toBeVisible();
  await expect(dialog.getByText(/compiled on Postgres 15/)).toBeVisible();
  await dialog.getByRole("button", { name: /Create v1/ }).click();
  await expect(dialog).toBeHidden();

  // What only 17 has: Postgres 15's own error, on its line.
  dialog = await upload(page, BOOKINGS);
  const problem = dialog.getByRole("list", { name: "Problems" }).getByRole("button");
  await expect(problem).toContainText(NOT_ON_15);
  await expect(problem).toContainText("Line 9:");
  await dialog.getByRole("button", { name: "Cancel" }).click();

  // Moving to 17: the dry run compiles v1 there first.
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  const card = page.getByRole("region", { name: "Postgres version" });
  await card.getByRole("combobox", { name: "Version" }).click();
  await page.getByRole("option", { name: /^Postgres 17/ }).click();
  await expect(card.getByText("v1 compiles on Postgres 17")).toBeVisible();
  await card.getByRole("button", { name: "Switch to Postgres 17" }).click();
  await expect(page.getByText("Rooms is on Postgres 17")).toBeVisible();
  await expect(title).toContainText("Postgres 17");

  // The same SQL compiles now, as v2.
  dialog = await upload(page, BOOKINGS);
  await expect(dialog.getByText("Compiles", { exact: true })).toBeVisible();
  await dialog.getByRole("button", { name: /Create v2/ }).click();
  await expect(dialog).toBeHidden();
  await expect(page.locator(".react-flow__node", { hasText: "bookings" })).toBeVisible();

  // Back to 15 is refused: v2 does not compile there, and says why.
  await page.goto(`./${slug}/rooms/settings?major=15`);
  await expect(card.getByText("v2 does not compile on Postgres 15")).toBeVisible();
  await expect(card.getByRole("list", { name: "Errors on Postgres 15" })).toContainText(NOT_ON_15);
  await expect(card.getByRole("button", { name: "Switch to Postgres 15" })).toBeDisabled();

  // Each version keeps the Postgres it was compiled on.
  await page.goto(`./${slug}/rooms/v/1`);
  await expect(page.getByText("Postgres 15", { exact: true }).last()).toBeVisible();
  // The activity says who moved it.
  await page.goto(`./${slug}/rooms/settings`);
  await expect(page.locator("#activity")).toContainText(
    "moved the database from Postgres 15 to 17",
  );
});
