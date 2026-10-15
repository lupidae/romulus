import { expect, test } from "@playwright/test";

import { createOrg, devSignIn, inviteLink, unique } from "./helpers";

const BROKEN = `CREATE TABLE users (
  id bigint PRIMARY KEY,
  email text NOT NULL UNIQUE
);
CREATE TABLE orders (
  id bigint PRIMARY KEY,
  user_id bigint NOT NULL REFERENCES customers (id)
);
`;
const FIXED = BROKEN.replace("customers", "users");

test("a database, from its first upload to a push from CI, read-only for reviewers", async ({
  browser,
  request,
}) => {
  test.slow();
  const admin = await (await browser.newContext()).newPage();
  await devSignIn(admin, `dba-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const orgName = unique("Schemas");
  const slug = await createOrg(admin, orgName);
  const reviewerEmail = `reader-${Date.now().toString(36)}@romulus.test`;
  const link = await inviteLink(admin, reviewerEmail, "Reviewer");

  // A database, empty: the ways in.
  await admin.goto(`./${slug}`);
  await expect(admin.getByRole("heading", { name: "No databases yet" })).toBeVisible();
  await admin.getByRole("button", { name: "New database" }).click();
  const create = admin.getByRole("dialog", { name: "New database" });
  await create.getByLabel("Name").fill("Billing");
  await create.getByRole("button", { name: "Create database" }).click();
  await expect(admin).toHaveURL(new RegExp(`/app/${slug}/billing$`));
  await expect(admin.getByRole("heading", { name: "No versions yet" })).toBeVisible();
  await expect(admin.getByLabel("Push command", { exact: true })).toContainText("/api/databases/");

  // Upload SQL that does not compile: the error points at its line.
  await admin.keyboard.press("u");
  const upload = admin.getByRole("dialog", { name: "Upload SQL" });
  await upload.getByRole("textbox", { name: "SQL to upload" }).click();
  await admin.keyboard.insertText(BROKEN);
  const problem = upload.getByRole("list", { name: "Problems" }).getByRole("button");
  await expect(problem).toContainText('relation "customers" does not exist');
  await expect(problem).toContainText("Line 7:");
  await upload.getByRole("button", { name: /Create v1/ }).click();
  await expect(upload.getByRole("status")).toContainText("1 error");

  // Fixed, it compiles and becomes v1; the graph shows its tables.
  await upload.getByRole("textbox", { name: "SQL to upload" }).click();
  await admin.keyboard.press("ControlOrMeta+a");
  await admin.keyboard.insertText(FIXED);
  await expect(upload.getByText("Compiles", { exact: true })).toBeVisible();
  await upload.getByLabel("Message").fill("First cut");
  await upload.getByRole("button", { name: /Create v1/ }).click();
  await expect(upload).toBeHidden();
  await expect(admin.locator(".react-flow__node", { hasText: "orders" })).toBeVisible();
  await expect(admin.getByText("Latest")).toBeVisible();

  // An API token, and v2 pushed with it the way CI would.
  await admin.getByRole("link", { name: "Settings", exact: true }).click();
  await admin.getByRole("button", { name: "New token" }).click();
  const tokenDialog = admin.getByRole("dialog", { name: "New API token" });
  await tokenDialog.getByLabel("Name").fill("CI");
  await tokenDialog.getByRole("button", { name: "Create token" }).click();
  const secret = await admin.getByRole("textbox", { name: "API token" }).inputValue();
  expect(secret).toMatch(/^rom_/);
  await admin.getByRole("button", { name: "Done" }).click();
  // A second token starts from an empty form: the first secret is gone.
  await admin.getByRole("button", { name: "New token" }).click();
  const second = admin.getByRole("dialog", { name: "New API token" });
  await expect(second.getByLabel("Name")).toHaveValue("");
  await expect(admin.getByRole("textbox", { name: "API token" })).toHaveCount(0);
  await second.getByLabel("Name").fill("remus on prod");
  await second.getByRole("button", { name: "Create token" }).click();
  const secondSecret = await admin.getByRole("textbox", { name: "API token" }).inputValue();
  expect(secondSecret).not.toBe(secret);
  await admin.getByRole("button", { name: "Done" }).click();
  await expect(admin.getByRole("list", { name: "API tokens" }).getByRole("listitem")).toHaveCount(
    2,
  );
  const databaseId = await admin.getByRole("textbox", { name: "Database ID" }).inputValue();
  const push = await request.post(`/api/databases/${databaseId}/versions?message=Add%20invoices`, {
    headers: { Authorization: `Bearer ${secret}`, "Content-Type": "application/sql" },
    data: `${FIXED}CREATE TABLE invoices (id bigint PRIMARY KEY, order_id bigint REFERENCES orders (id));\n`,
  });
  expect(push.status()).toBe(201);
  // The same schema again changes nothing.
  const again = await request.post(`/api/databases/${databaseId}/versions`, {
    headers: { Authorization: `Bearer ${secret}`, "Content-Type": "application/sql" },
    data: `${FIXED}CREATE TABLE invoices (id bigint PRIMARY KEY, order_id bigint REFERENCES orders (id));\n`,
  });
  expect(await again.json()).toMatchObject({ status: "unchanged" });

  // The page does not know of v2 yet; the version picker does, and v2 opens.
  await admin.getByRole("link", { name: "Schema", exact: true }).click();
  await admin.getByRole("button", { name: /choose another version/ }).click();
  await admin.getByRole("menuitem", { name: /^v2/ }).click();
  await expect(admin).toHaveURL(new RegExp(`/app/${slug}/billing/v/2$`));
  await expect(admin.getByText("Latest")).toBeVisible();
  await expect(admin.locator(".react-flow__node", { hasText: "invoices" })).toBeVisible();

  // The history has both; v1 opens read-only, marked as older.
  await admin.getByRole("link", { name: /History/ }).click();
  const versions = admin.getByRole("list", { name: "Versions" }).getByRole("link");
  await expect(versions).toHaveCount(2);
  await expect(versions.first()).toContainText("Add invoices");
  await expect(versions.first()).toContainText("CI");
  await versions.last().click();
  await expect(admin).toHaveURL(new RegExp(`/app/${slug}/billing/v/1$`));
  await expect(admin.getByText("Older version")).toBeVisible();
  await expect(admin.locator(".react-flow__node", { hasText: "orders" })).toBeVisible();
  await expect(admin.locator(".react-flow__node", { hasText: "invoices" })).toHaveCount(0);

  // A reviewer sees everything, and nothing to change it with.
  const reviewer = await (await browser.newContext()).newPage();
  await devSignIn(reviewer, reviewerEmail, new URL(link).pathname);
  await reviewer.getByRole("button", { name: `Join ${orgName}` }).click();
  await expect(reviewer).toHaveURL(new RegExp(`/app/${slug}$`));
  await reviewer.goto(`./${slug}/billing`);
  await expect(reviewer.getByText("Read-only")).toBeVisible();
  await expect(reviewer.locator(".react-flow__node", { hasText: "invoices" })).toBeVisible();
  await expect(reviewer.getByRole("button", { name: "Upload SQL" })).toHaveCount(0);
  const tabs = reviewer.getByRole("navigation", { name: "Billing" });
  await expect(tabs.getByRole("link")).toHaveText([/Schema/, /History/]);
  await reviewer.keyboard.press("u");
  await expect(reviewer.getByRole("dialog")).toHaveCount(0);
});

test("an unknown database or version is a page that leads back", async ({ page }) => {
  await devSignIn(page, `lost-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Lost"));
  await page.goto(`./${slug}/nope`);
  await expect(page.getByRole("heading", { name: "No such database" })).toBeVisible();
  await page.getByRole("link", { name: "All databases" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}$`));
});
