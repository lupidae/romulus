import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

const V1 = `CREATE TABLE customers (
  id bigint PRIMARY KEY,
  email text NOT NULL
);
CREATE TABLE orders (
  id bigint PRIMARY KEY,
  customer_id bigint REFERENCES customers (id),
  note text
);
`;
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

const changes = (page: Page) => page.getByRole("region", { name: "Tables" }).getByRole("button");
const change = (page: Page, name: string) =>
  page.getByRole("button", { name: new RegExp(`^${name},`) });
const node = (page: Page, table: string) =>
  page.locator(`.react-flow__node[data-id="public.${table}"]`);

/**
 * Some line of the selected object's statements is on screen in the SQL diff
 * (its scroller is the merge view side by side, the editor's own otherwise).
 */
async function expectStatementInView(page: Page) {
  await expect
    .poll(() =>
      page.evaluate(() =>
        Array.from(document.querySelectorAll<HTMLElement>(".cm-statement")).some((el) => {
          const scroller = el.closest(".cm-mergeView") ?? el.closest(".cm-scroller");
          if (!scroller || !el.offsetParent) return false;
          const r = el.getBoundingClientRect();
          const s = scroller.getBoundingClientRect();
          return r.bottom > s.top && r.top < s.bottom;
        }),
      ),
    )
    .toBe(true);
}

async function upload(page: Page, sql: string, next: number) {
  // The header's (an empty database shows a second one in its getting started).
  await page.getByRole("button", { name: "Upload SQL" }).first().click();
  const dialog = page.getByRole("dialog", { name: "Upload SQL" });
  await dialog.getByRole("textbox", { name: "SQL to upload" }).click();
  await page.keyboard.insertText(sql);
  // "Compiles", or "Compiles, but it is the same schema as v2." for a re-upload.
  await expect(dialog.getByText(/^Compiles/)).toBeVisible();
  await dialog.getByRole("button", { name: new RegExp(`Create v${next}`) }).click();
  await expect(dialog).toBeHidden();
}

async function shot(page: Page, name: string) {
  await test.info().attach(name, { body: await page.screenshot(), contentType: "image/png" });
}

test("two versions compared: from the history, a change picked, identical versions", async ({
  page,
}) => {
  test.slow();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await devSignIn(page, `diff-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Diffs"));

  // v1 uploaded as SQL.
  await page.goto(`./${slug}`);
  await page.getByRole("button", { name: "New database" }).click();
  const create = page.getByRole("dialog", { name: "New database" });
  await create.getByLabel("Name").fill("Shop");
  await create.getByRole("button", { name: "Create database" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/shop$`));
  await upload(page, V1, 1);
  await expect(node(page, "orders")).toBeVisible();

  // v2 pushed by CI, as SQL, with a table added and two changed.
  const headers = { "X-Romulus-Csrf": "1", Origin: new URL(page.url()).origin };
  const database = (await (await page.request.get(`/api/orgs/${slug}/databases/shop`)).json()) as {
    id: string;
  };
  const token = await page.request.post(`/api/orgs/${slug}/databases/shop/tokens`, {
    headers,
    data: { name: "CI", scope: "push" },
  });
  expect(token.status()).toBe(201);
  const { secret } = (await token.json()) as { secret: string };
  const push = await page.request.post(`/api/databases/${database.id}/versions?message=Invoices`, {
    headers: { Authorization: `Bearer ${secret}`, "Content-Type": "application/sql" },
    data: V2,
  });
  expect(push.status()).toBe(201);

  // From the history: compare v2 with the version before it.
  await page.goto(`./${slug}/shop/history`);
  await page.getByRole("link", { name: "Compare v1 → v2" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/shop/diff/1\\.\\.\\.2$`));
  await expect(page).toHaveTitle("v1 → v2 – romulus");
  await expect(page.getByRole("link", { name: /History/ })).toHaveAttribute("aria-current", "page");
  await expect(page.getByText("1 table added, 2 changed")).toBeVisible();
  await expect(changes(page)).toHaveText([/^~customers/, /^\+invoices/, /^~orders/]);
  await expect(change(page, "orders")).toContainText("orders.customer_id");
  await expect(node(page, "invoices")).toBeVisible();

  // A change picked in the list: selected in the graph, the SQL scrolled to it.
  await change(page, "orders").click();
  await expect(change(page, "orders")).toHaveAttribute("aria-pressed", "true");
  await expect(node(page, "orders").locator("[data-selected]")).toBeVisible();
  await expectStatementInView(page);
  await expect(page).toHaveURL(/change=table%3Apublic\.orders/);
  await shot(page, "diff, a change picked");

  // j and k go through the changes, the graph following.
  await page.keyboard.press("k");
  await expect(change(page, "invoices")).toHaveAttribute("aria-pressed", "true");
  await expect(node(page, "invoices").locator("[data-selected]")).toBeVisible();
  // A table clicked in the graph selects its change (the view fitted first).
  await page.getByRole("application", { name: "Schema graph" }).focus();
  await page.keyboard.press("0");
  await expect(node(page, "customers")).toBeInViewport();
  await page.waitForTimeout(400);
  await node(page, "customers").click();
  await expect(change(page, "customers")).toHaveAttribute("aria-pressed", "true");

  // The filter keeps what it shows in the address.
  await page.getByRole("radio", { name: "Added: 1" }).click();
  await expect(changes(page)).toHaveText([/^\+invoices/]);
  await expect(page).toHaveURL(/show=added/);
  await page.getByRole("radio", { name: "All: 3" }).click();

  // The same SQL again is a version with nothing new: the page says so.
  await page.goto(`./${slug}/shop`);
  await upload(page, V2, 3);
  await page.goto(`./${slug}/shop/diff/2...3`);
  await expect(
    page.getByRole("heading", { name: "No schema changes between v2 and v3" }),
  ).toBeVisible();
  await shot(page, "identical versions");
  // [ steps back to the previous pair, which has changes.
  await page.getByRole("heading", { name: "No schema changes between v2 and v3" }).click();
  await page.keyboard.press("[");
  await expect(page).toHaveURL(/\/diff\/1\.\.\.2/);
  await expect(changes(page)).toHaveCount(3);

  // From a version's page: compare it with the one before.
  await page.goto(`./${slug}/shop/v/2`);
  await page.getByRole("button", { name: "Compare" }).click();
  await shot(page, "version page, compare menu");
  await page.getByRole("menuitem", { name: /The version before/ }).click();
  await expect(page).toHaveURL(/\/diff\/1\.\.\.2/);

  // Any two, ticked in the history; then swapped.
  await page.goto(`./${slug}/shop/history`);
  await page.getByRole("checkbox", { name: "Tick v3 to compare" }).check();
  await page.getByRole("checkbox", { name: "Tick v1 to compare" }).check();
  await shot(page, "history, two ticked");
  await page.getByRole("link", { name: "Compare v1 → v3" }).click();
  await expect(page).toHaveURL(/\/diff\/1\.\.\.3/);
  await expect(page.getByText("1 table added, 2 changed")).toBeVisible();
  await page.getByRole("button", { name: "Swap sides" }).click();
  await expect(page).toHaveURL(/\/diff\/3\.\.\.1/);
  await expect(page.getByText("2 tables changed, 1 removed")).toBeVisible();
  await expect(change(page, "invoices")).toContainText("invoices");

  // For review: dark, and a phone.
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto(`./${slug}/shop/diff/1...2?change=table:public.orders`);
  await expect(node(page, "orders").locator("[data-selected]")).toBeVisible();
  await shot(page, "diff, dark");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload();
  await expect(change(page, "orders")).toBeVisible();
  await shot(page, "phone, changes");
  await change(page, "orders").click();
  await expect(node(page, "orders")).toBeVisible();
  await shot(page, "phone, graph");
  await page.getByRole("tab", { name: "SQL" }).click();
  await shot(page, "phone, SQL");
  expect(errors).toEqual([]);
});
