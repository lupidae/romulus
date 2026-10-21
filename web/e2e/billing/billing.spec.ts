import { readFile } from "node:fs/promises";

import { expect, type Page, test } from "@playwright/test";

import { fillBusinessCheckout, signedInOrg } from "../billing-flows";

// Hosted billing, Stripe faked server-side (ROMULUS_BILLING=fake on this
// project's API, sold to businesses only): a new account accepts the Terms
// first; a new organization is on Free (1 database, 1 member); checkout goes
// through the API's fake page, which completes a paid subscription; portal
// links point at billing.stripe.com, intercepted here.

const PORTAL = "https://billing.stripe.com/p/session/test_fake";

async function newDatabase(page: Page, slug: string, name: string) {
  await page.goto(`./${slug}`);
  await page.getByRole("button", { name: "New database" }).first().click();
  const create = page.getByRole("dialog", { name: "New database" });
  await create.getByLabel("Name").fill(name);
  await create.getByRole("button", { name: "Create database" }).click();
  return create;
}

test("the database cap asks to upgrade where it refused, and the billing page shows the usage", async ({
  page,
}) => {
  const slug = await signedInOrg(page, "cap");
  await newDatabase(page, slug, "Shop");
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/shop$`));

  // A second database: Free keeps one. Refused in the dialog, with the way out.
  const create = await newDatabase(page, slug, "Ledger");
  const notice = create.getByRole("status").filter({ hasText: "Database limit reached" });
  await expect(notice).toBeVisible();
  await expect(notice).toContainText("Starter keeps 3 databases");
  await expect(page).toHaveURL(new RegExp(`/app/${slug}$`));
  await notice.getByRole("link", { name: "See plans" }).click();

  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing$`));
  await expect(page.getByRole("heading", { name: "Billing", level: 1 })).toBeVisible();
  const plan = page.getByRole("region", { name: "Plan", exact: true });
  await expect(plan.getByText("Free", { exact: true })).toBeVisible();
  const usage = page.getByRole("region", { name: "Usage" });
  const databases = usage.getByRole("listitem").filter({ hasText: "Databases" });
  await expect(databases).toContainText("1 of 1");
  await expect(databases).toContainText("At the limit");
  await expect(databases.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "100");
  await expect(usage.getByRole("listitem").filter({ hasText: "Members" })).toContainText("1 of 1");
  await expect(usage.getByRole("listitem").filter({ hasText: "History shown" })).toContainText(
    "90 days",
  );
  // The sidebar leads here too.
  await expect(page.getByRole("link", { name: "Billing" })).toHaveAttribute(
    "href",
    `/app/${slug}/settings/billing`,
  );
});

test("upgrading through checkout, then Cancel opens Stripe's cancel page", async ({ page }) => {
  const slug = await signedInOrg(page, "upgrade");
  await page.goto(`./${slug}/settings/billing`);
  const plans = page.getByRole("region", { name: "Plans" });
  await plans.getByRole("button", { name: "Upgrade to Starter" }).click();

  const checkout = page.getByRole("dialog", { name: "Upgrade to Starter" });
  await expect(checkout).toContainText("€19 a month");
  await fillBusinessCheckout(checkout);
  await checkout.getByRole("button", { name: "Continue to payment" }).click();

  // The fake checkout completes the subscription and comes back.
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing$`));
  await expect(page.getByText("Payment received")).toBeVisible();
  const plan = page.getByRole("region", { name: "Plan", exact: true });
  await expect(plan.getByText("Starter", { exact: true })).toBeVisible();
  await expect(plan.getByText("Active")).toBeVisible();
  await expect(plan).toContainText("€19 a month");
  await expect(plan).toContainText("Renews on");
  await expect(plans.getByText("Current plan")).toBeVisible();
  await expect(page.getByRole("region", { name: "Invoices" }).getByRole("row")).toHaveCount(2);

  // Cancelling is one click away, straight to Stripe's cancel page.
  const opened: string[] = [];
  await page.route(`${PORTAL}**`, async (route) => {
    opened.push(route.request().url());
    await route.fulfill({ contentType: "text/html", body: "<title>Stripe portal</title>" });
  });
  await plan.getByRole("button", { name: "Cancel subscription" }).click();
  await expect(page).toHaveURL(`${PORTAL}?flow=subscription_cancel`);
  expect(opened).toEqual([`${PORTAL}?flow=subscription_cancel`]);

  // The renewal email's link focuses the same button.
  await page.goto(`./${slug}/settings/billing?action=cancel`);
  await expect(page.getByRole("button", { name: "Cancel subscription" })).toBeFocused();
});

test("export downloads a ZIP of the database, from the history", async ({ page }) => {
  const slug = await signedInOrg(page, "export");
  const headers = { "X-Romulus-Csrf": "1", Origin: new URL(page.url()).origin };
  const created = await page.request.post(`/api/orgs/${slug}/databases`, {
    headers,
    data: { name: "Shop", slug: "shop" },
  });
  expect(created.status()).toBe(201);
  const { id } = (await created.json()) as { id: string };
  const token = await page.request.post(`/api/orgs/${slug}/databases/shop/tokens`, {
    headers,
    data: { name: "CI", scope: "push" },
  });
  const { secret } = (await token.json()) as { secret: string };
  for (const tables of [["customers"], ["customers", "orders"]]) {
    const push = await page.request.post(`/api/databases/${id}/versions`, {
      headers: { Authorization: `Bearer ${secret}`, "Content-Type": "application/json" },
      data: JSON.stringify({
        remus_version: "0.2",
        database: "shop",
        server_version: "17.6",
        entities: tables.map((name) => ({
          schema: "public",
          name,
          kind: "r",
          comment: null,
          partition_key: null,
          view_definition: null,
          columns: [
            {
              name: "id",
              type: "bigint",
              type_schema: "pg_catalog",
              type_name: "bigint",
              type_kind: "b",
              not_null: true,
              default: null,
              identity: null,
              generated: null,
              comment: null,
            },
          ],
          constraints: [],
        })),
      }),
    });
    expect(push.status()).toBe(201);
  }

  await page.goto(`./${slug}/shop/history`);
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    page.getByRole("button", { name: "Export (ZIP)" }).click(),
  ]);
  expect(download.suggestedFilename()).toMatch(/shop.*\.zip$/);
  const file = await readFile(await download.path());
  expect(file.subarray(0, 2).toString()).toBe("PK");
});
