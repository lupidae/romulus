import { type Browser, expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// A playground from a draft: created from the editor's bar, queried in its
// console (a read runs, a write is refused by the database), the draft's
// migration run on it (its rows then hold the new table), reset. And a
// reviewer's: they open a playground and query it, with nothing to create,
// reset, migrate or delete. Each test deletes its playground (the host
// holds a few dozen at most, across runs of a shared stack).
// Needs the playground runner and service (`just play`, or `just dev`).

const V1 = `CREATE TABLE customers (
  id bigint PRIMARY KEY,
  email text NOT NULL
);
`;

/** The console's SQL editor (the playground page has one CodeMirror). */
const editor = (page: Page) => page.getByRole("textbox", { name: "SQL to run" });

async function run(page: Page, sql: string) {
  // Ready (the badge shows only then): not starting, paused or reconnecting.
  await expect(page.getByText("Read-only", { exact: true })).toBeVisible({ timeout: 60_000 });
  await editor(page).click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Delete");
  await page.keyboard.type(sql);
  await page.keyboard.press("ControlOrMeta+Enter");
}

test("a playground: created from a draft, queried, its migration run, reset", async ({ page }) => {
  test.slow();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await devSignIn(page, `playground-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Playgrounds"));
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

  // A draft that adds a table.
  await page.goto(`./${slug}/shop`);
  await page.getByRole("button", { name: "New draft" }).first().click();
  await page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");
  await page.locator(".cm-content").first().click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type("\nCREATE TABLE invoices (id bigint PRIMARY KEY);\n", { delay: 10 });
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");

  // Its playground, from the bar.
  await page.getByRole("button", { name: "Playground" }).click();
  const create = page.getByRole("dialog", { name: "New playground" });
  await create.getByRole("radio", { name: /Edge cases/ }).click();
  await create.getByRole("button", { name: "Create playground" }).click();
  await page.waitForURL(/\/playgrounds\/[0-9a-f-]+$/);
  await expect(page.getByRole("heading", { name: "Playground", exact: true })).toBeVisible();
  // Built (the console shows), within the build's few seconds.
  await expect(editor(page)).toBeVisible({ timeout: 120_000 });
  await expect(page.getByRole("button", { name: /^customers/ })).toBeVisible();

  // A read.
  await run(page, "SELECT 40 + 2 AS answer, NULL::text AS nothing");
  const grid = page.getByRole("region", { name: "Results, 1 row" });
  await expect(grid.getByRole("columnheader", { name: /answer/ })).toBeVisible();
  await expect(grid.getByRole("cell", { name: "42" })).toBeVisible();
  await expect(grid.getByRole("cell", { name: "NULL" })).toBeVisible();
  // A write: Postgres refuses it, whatever the console is asked.
  await run(page, "INSERT INTO customers VALUES (1, 'a@example.com')");
  // The session is read-only first (25006), and privileges hold under it (42501).
  await expect(
    page.getByText(
      /cannot execute INSERT in a read-only transaction|permission denied for table customers/,
    ),
  ).toBeVisible();
  await expect(page.getByText(/This console only reads/)).toBeVisible();
  // The shared history has both.
  const history = page.getByRole("region", { name: /History/ });
  await expect(
    history.getByText("INSERT INTO customers VALUES (1, 'a@example.com')"),
  ).toBeVisible();

  // The draft's migration, on the playground's rows.
  await page.getByRole("button", { name: "Run migration" }).click();
  const confirm = page.getByRole("dialog", { name: "Run the migration on these rows?" });
  await expect(confirm.getByText(/CREATE TABLE public\.invoices/)).toBeVisible();
  await confirm.getByRole("button", { name: "Run migration" }).click();
  await expect(page.getByText(/^Passed: 1 statement/)).toBeVisible({ timeout: 60_000 });
  await expect(page.getByText(/Holding the rows .* migration run left/)).toBeVisible();
  // Migrated rows are queryable.
  await page.getByRole("tab", { name: "Results" }).click();
  await run(page, "SELECT count(*) AS invoices FROM invoices");
  await expect(
    page.getByRole("region", { name: "Results, 1 row" }).getByRole("cell", { name: "0" }),
  ).toBeVisible({
    timeout: 30_000,
  });

  // Reset: the generated rows (and schema) back.
  await page.getByRole("button", { name: "Reset rows" }).first().click();
  await page.getByRole("alertdialog").getByRole("button", { name: "Reset rows" }).click();
  await expect(page.getByText("Rows reset")).toBeVisible({ timeout: 60_000 });
  await expect(page.getByText(/Holding the rows/)).toBeHidden();
  await run(page, "SELECT count(*) FROM invoices");
  await expect(page.getByText('relation "invoices" does not exist')).toBeVisible({
    timeout: 30_000,
  });
  expect(errors).toEqual([]);

  // Gone with its database and history.
  const playground = new URL(page.url()).pathname.split("/playgrounds/")[1];
  const deleted = await page.request.delete(
    `/api/orgs/${slug}/databases/shop/playgrounds/${playground}`,
    { headers },
  );
  expect(deleted.status()).toBe(204);
});

/** A JSON call to the API as the page's signed-in person (cookie, CSRF header, Origin). */
async function call(page: Page, method: string, path: string, data?: unknown) {
  const origin = new URL(page.url()).origin;
  const response = await page.request.fetch(path, {
    method,
    data,
    headers: { "X-Romulus-Csrf": "1", Origin: origin },
  });
  expect(response.ok(), `${method} ${path}: ${response.status()} ${await response.text()}`).toBe(
    true,
  );
  const text = await response.text();
  return text ? (JSON.parse(text) as Record<string, unknown>) : null;
}

async function person(browser: Browser, name: string): Promise<Page> {
  const page = await (await browser.newContext()).newPage();
  const email = `${name.toLowerCase()}-${Date.now().toString(36)}${Math.floor(Math.random() * 1e4)}@romulus.test`;
  const query = new URLSearchParams({ email, name, next: "/app/" });
  await page.goto(`/api/auth/dev-signin?${query.toString()}`);
  await page.waitForURL((url) => url.pathname.startsWith("/app/"));
  return page;
}

test("a reviewer queries a playground and has nothing to change in it", async ({ browser }) => {
  test.slow();
  // Alice (admin) makes the organization, v1 and its playground; Bob reviews.
  const alice = await person(browser, "Alice");
  const name = unique("Reviews");
  const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  await call(alice, "POST", "/api/orgs", { name, slug });
  await call(alice, "POST", `/api/orgs/${slug}/databases`, { name: "Shop", slug: "shop" });
  await call(alice, "POST", `/api/orgs/${slug}/databases/shop/versions`, {
    sql: V1,
    message: "Customers",
  });
  const bob = await person(browser, "Bob");
  const { email } = (await call(bob, "GET", "/api/me")) as { email: string };
  const invitation = (await call(alice, "POST", `/api/orgs/${slug}/invitations`, {
    email,
    role: "reviewer",
  })) as { url: string };
  await call(bob, "POST", `/api/invitations/${invitation.url.split("/invite/")[1]}/accept`);
  const playgrounds = `/api/orgs/${slug}/databases/shop/playgrounds`;

  // No playground yet: nothing for a reviewer on the version page.
  await bob.goto(`./${slug}/shop`);
  await expect(bob.getByRole("heading", { name: "Shop" })).toBeVisible();
  await expect(bob.getByRole("button", { name: "Playground", exact: true })).toHaveCount(0);
  await expect(bob.getByRole("link", { name: "Playground", exact: true })).toHaveCount(0);
  // Nor through the API.
  const refused = await bob.request.post(playgrounds, {
    data: {},
    headers: { "X-Romulus-Csrf": "1", Origin: new URL(bob.url()).origin },
  });
  expect(refused.status()).toBe(403);

  const created = (await call(alice, "POST", playgrounds, {})) as { id: string };
  await expect
    .poll(
      async () =>
        ((await call(alice, "GET", `${playgrounds}/${created.id}`)) as { status: string }).status,
      { timeout: 120_000 },
    )
    .toBe("ready");

  // Bob opens it from the version page and queries it.
  await bob.reload();
  await bob.getByRole("link", { name: "Playground", exact: true }).click();
  await bob.waitForURL(/\/playgrounds\/[0-9a-f-]+$/);
  await run(bob, "SELECT count(*) AS customers FROM customers");
  await expect(
    bob
      .getByRole("region", { name: "Results, 1 row" })
      .getByRole("columnheader", { name: /customers/ }),
  ).toBeVisible({ timeout: 30_000 });
  // Nothing to change: no Reset, no Run migration, no menu with Delete.
  await expect(bob.getByRole("button", { name: "Reset rows" })).toHaveCount(0);
  await expect(bob.getByRole("button", { name: "Run migration" })).toHaveCount(0);
  await expect(bob.getByRole("button", { name: "More actions" })).toHaveCount(0);
  // And the API agrees.
  const origin = new URL(bob.url()).origin;
  const reset = await bob.request.post(`${playgrounds}/${created.id}/reset`, {
    headers: { "X-Romulus-Csrf": "1", Origin: origin },
  });
  expect(reset.status()).toBe(403);

  await call(alice, "DELETE", `${playgrounds}/${created.id}`);
});
