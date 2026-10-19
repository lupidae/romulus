import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// Four versions pushed as remus JSON (no sandbox needed):
//   v1  customers (id, name), orders (id, customers_id → customers), legacy (id)
//   v2  customers gains `email`
//   v3  legacy dropped, orders gains `total`
//   v4  invoices (id, orders_id → orders)

function column(name: string, type = "bigint") {
  return {
    name,
    type,
    type_schema: "pg_catalog",
    type_name: type,
    type_kind: "b",
    not_null: name === "id",
    default: null,
    identity: null,
    generated: null,
    comment: null,
  };
}

function table(name: string, columns: string[][], refs: string[] = []) {
  return {
    schema: "public",
    name,
    kind: "r",
    comment: null,
    partition_key: null,
    view_definition: null,
    columns: [column("id"), ...columns.map(([n, t]) => column(n, t))],
    constraints: [
      {
        name: `${name}_pkey`,
        kind: "p",
        columns: ["id"],
        ref_schema: null,
        ref_table: null,
        ref_columns: [],
        on_update: null,
        on_delete: null,
        deferrable: false,
        definition: "PRIMARY KEY (id)",
        comment: null,
      },
      ...refs.map((target) => ({
        name: `${name}_${target}_id_fkey`,
        kind: "f",
        columns: [`${target}_id`],
        ref_schema: "public",
        ref_table: target,
        ref_columns: ["id"],
        on_update: "a",
        on_delete: "a",
        deferrable: false,
        definition: `FOREIGN KEY (${target}_id) REFERENCES public.${target}(id)`,
        comment: null,
      })),
    ],
  };
}

const model = (entities: unknown[]) =>
  JSON.stringify({ remus_version: "0.2", database: "shop", server_version: "17.6", entities });

const customers = (email: boolean) =>
  table("customers", [["name", "text"], ...(email ? [["email", "text"]] : [])]);
const orders = (total: boolean) =>
  table("orders", [["customers_id"], ...(total ? [["total", "numeric"]] : [])], ["customers"]);
const VERSIONS = [
  {
    message: "Customers and orders",
    model: model([customers(false), orders(false), table("legacy", [])]),
  },
  {
    message: "Customer emails",
    model: model([customers(true), orders(false), table("legacy", [])]),
  },
  { message: "Drop legacy, order totals", model: model([customers(true), orders(true)]) },
  {
    message: "Invoices",
    model: model([customers(true), orders(true), table("invoices", [["orders_id"]], ["orders"])]),
  },
];

const node = (page: Page, name: string) =>
  page.locator(`.react-flow__node[data-id="public.${name}"]`);
const row = (page: Page, table: string, column: string) =>
  node(page, table).locator(`[data-column="${column}"]`);

/** The tables a graph draws, as their nodes name them: "table public.users, 3 columns". */
async function drawn(page: Page) {
  return page
    .locator(".react-flow__node[data-id^='public.']")
    .evaluateAll((nodes) => nodes.map((n) => n.getAttribute("aria-label") ?? "").sort());
}

async function shot(page: Page, name: string) {
  await test.info().attach(name, { body: await page.screenshot(), contentType: "image/png" });
}

test("the time machine: a range superposed, a removed table ghosted, a new column tinted, its history, the cursor", async ({
  page,
}) => {
  test.slow();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await devSignIn(page, `tm-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Time"));
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
  expect(token.status()).toBe(201);
  const { secret } = (await token.json()) as { secret: string };
  for (const v of VERSIONS) {
    const push = await page.request.post(
      `/api/databases/${id}/versions?message=${encodeURIComponent(v.message)}`,
      {
        headers: { Authorization: `Bearer ${secret}`, "Content-Type": "application/json" },
        data: v.model,
      },
    );
    expect(push.status()).toBe(201);
  }

  // From the history page.
  await page.goto(`./${slug}/shop/history`);
  await page.getByRole("link", { name: "Time machine" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/shop/time-machine`));
  await expect(page.getByRole("heading", { name: "Time machine" })).toBeVisible();
  await expect(node(page, "invoices")).toBeVisible();

  // The range v1 → v3: the end handle back one version.
  const end = page.getByRole("slider", { name: "Range end" });
  await end.focus();
  await page.keyboard.press("ArrowLeft");
  await expect(end).toHaveAttribute("aria-valuetext", /^v3, /);
  await expect(page.getByRole("slider", { name: "Cursor" })).toHaveAttribute(
    "aria-valuetext",
    /^v3, Drop legacy/,
  );
  await expect(page).toHaveURL(/to=3/);
  // Superposed: legacy, gone in v3, is a ghost; invoices (v4) is not there.
  await expect(node(page, "invoices")).toHaveCount(0);
  await expect(node(page, "legacy").locator(".sg-node")).toHaveAttribute("data-ghost", "");
  await expect(node(page, "legacy")).toContainText("−v3");
  // email, added in v2, is tinted (the scale's v2 step); name, there in v1, is not.
  await expect(row(page, "customers", "email")).toHaveAttribute("data-era", /^[1-6]$/);
  await expect(row(page, "customers", "name")).not.toHaveAttribute("data-era", /.*/);
  await expect(row(page, "orders", "total")).toHaveAttribute("data-era", /^[1-6]$/);
  await shot(page, "superposed v1 → v3");

  // Hovering a column shows its history.
  await row(page, "orders", "total").hover();
  const card = page.getByRole("tooltip");
  await expect(card).toContainText("History");
  await expect(card).toContainText("v3 · added");

  // Selecting a table: its history, each version linking to its diff.
  await node(page, "customers").locator("[data-table-header]").click();
  const panel = page.getByRole("region", { name: "public.customers" });
  await expect(panel).toContainText("email");
  await expect(panel.getByRole("link", { name: /^v2: added/ })).toHaveAttribute(
    "href",
    new RegExp(`/app/${slug}/shop/diff/1\\.\\.\\.2$`),
  );

  // The cursor back to v2: legacy is alive again, total not there yet.
  const cursor = page.getByRole("slider", { name: "Cursor" });
  await cursor.focus();
  await page.keyboard.press("ArrowLeft");
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v2, Customer emails/);
  await expect(node(page, "legacy").locator(".sg-node")).not.toHaveAttribute("data-ghost", /.*/);
  await expect(row(page, "orders", "total")).toHaveCount(0);

  // At the cursor: exactly v2, as v2's own page draws it.
  await page.getByRole("radio", { name: "At cursor" }).click();
  await expect(page).toHaveURL(/mode=cursor/);
  await expect(node(page, "legacy").locator(".sg-node")).toBeVisible();
  // Leaving animations are over once a step settles.
  await page.waitForTimeout(500);
  const atCursor = await drawn(page);
  expect(atCursor).toEqual([
    "table public.customers, 3 columns",
    "table public.legacy, 1 columns",
    "table public.orders, 2 columns",
  ]);
  await expect(row(page, "customers", "email")).toBeVisible();
  await shot(page, "at the cursor, v2");

  // Back to v1 and forward with the keyboard: ] steps.
  await page.locator("body").press("[");
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v1, /);
  await expect(row(page, "customers", "email")).toHaveCount(0);
  await page.locator("body").press("]");
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v2, /);

  await page.goto(`./${slug}/shop/v/2`);
  await expect(node(page, "customers")).toBeVisible();
  expect(await drawn(page)).toEqual(atCursor);

  // The version page leads back to it, at that version.
  await page.getByRole("link", { name: "Time machine" }).click();
  await expect(page).toHaveURL(/time-machine\?at=2/);
  await expect(page.getByRole("slider", { name: "Cursor" })).toHaveAttribute(
    "aria-valuetext",
    /^v2, /,
  );
  expect(errors).toEqual([]);
});

test("one version: the time machine says what it needs", async ({ page }) => {
  await devSignIn(page, `tm1-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Time one"));
  const headers = { "X-Romulus-Csrf": "1", Origin: new URL(page.url()).origin };
  const created = await page.request.post(`/api/orgs/${slug}/databases`, {
    headers,
    data: { name: "Solo", slug: "solo" },
  });
  const { id } = (await created.json()) as { id: string };
  const token = await page.request.post(`/api/orgs/${slug}/databases/solo/tokens`, {
    headers,
    data: { name: "CI", scope: "push" },
  });
  const { secret } = (await token.json()) as { secret: string };
  await page.request.post(`/api/databases/${id}/versions`, {
    headers: { Authorization: `Bearer ${secret}`, "Content-Type": "application/json" },
    data: VERSIONS[0].model,
  });
  await page.goto(`./${slug}/solo/time-machine`);
  await expect(page.getByRole("heading", { name: "Only one version so far" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Open v1" })).toBeVisible();
});

/** A shop with the four versions above; returns its organization's slug. */
async function shop(page: Page, who: string) {
  await devSignIn(page, `${who}-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Time"));
  const headers = { "X-Romulus-Csrf": "1", Origin: new URL(page.url()).origin };
  const created = await page.request.post(`/api/orgs/${slug}/databases`, {
    headers,
    data: { name: "Shop", slug: "shop" },
  });
  const { id } = (await created.json()) as { id: string };
  const token = await page.request.post(`/api/orgs/${slug}/databases/shop/tokens`, {
    headers,
    data: { name: "CI", scope: "push" },
  });
  const { secret } = (await token.json()) as { secret: string };
  for (const v of VERSIONS) {
    const push = await page.request.post(
      `/api/databases/${id}/versions?message=${encodeURIComponent(v.message)}`,
      {
        headers: { Authorization: `Bearer ${secret}`, "Content-Type": "application/json" },
        data: v.model,
      },
    );
    expect(push.status()).toBe(201);
  }
  return { slug, headers };
}

test("play and pause, and the range from the keyboard", async ({ page }) => {
  const { slug } = await shop(page, "tm-play");
  await page.goto(`./${slug}/shop/time-machine`);
  const cursor = page.getByRole("slider", { name: "Cursor" });
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v4, /);

  // Played from the end, the range plays again from its start, and on.
  await page.getByRole("button", { name: "Play the range" }).click();
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v(2|3), /, { timeout: 8000 });
  await page.getByRole("button", { name: "Pause" }).click();
  const paused = await cursor.getAttribute("aria-valuetext");
  await expect(page.getByText(/^Paused at v\d/)).toBeAttached();
  await page.waitForTimeout(1500);
  expect(await cursor.getAttribute("aria-valuetext")).toBe(paused);
  // K plays to the end, where it stops by itself.
  await page.locator("body").press("k");
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v4, /, { timeout: 8000 });
  await expect(page.getByRole("button", { name: "Play the range" })).toBeVisible();
  await expect(page.getByText(/^Played to v4\./)).toBeAttached();

  // The range's handles take the arrow keys, Home and End.
  const start = page.getByRole("slider", { name: "Range start" });
  const end = page.getByRole("slider", { name: "Range end" });
  await start.focus();
  await page.keyboard.press("ArrowRight");
  await expect(start).toHaveAttribute("aria-valuetext", /^v2, /);
  await expect(page).toHaveURL(/from=2/);
  await end.focus();
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("ArrowLeft");
  // Never before the start: v3 is as far as it goes.
  await expect(end).toHaveAttribute("aria-valuetext", /^v3, /);
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v3, /);
  await start.focus();
  await page.keyboard.press("ArrowRight");
  await expect(start).toHaveAttribute("aria-valuetext", /^v2, /);
  await page.keyboard.press("Home");
  await expect(start).toHaveAttribute("aria-valuetext", /^v1, /);
  await end.focus();
  await page.keyboard.press("End");
  await expect(end).toHaveAttribute("aria-valuetext", /^v4, /);
  await expect(page).not.toHaveURL(/from=|to=/);
});

test("an admin brings removed versions in, and the view keeps its place", async ({ page }) => {
  const { slug, headers } = await shop(page, "tm-removed");
  const removed = await page.request.post(`/api/orgs/${slug}/databases/shop/versions/3/remove`, {
    headers,
    data: { reason: "Pushed from staging" },
  });
  expect(removed.status()).toBe(200);
  await page.goto(`./${slug}/shop/time-machine?at=2`);
  const cursor = page.getByRole("slider", { name: "Cursor" });
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v2, /);
  // v3 is not there: after v2 comes v4.
  await cursor.focus();
  await page.keyboard.press("ArrowRight");
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v4, /);
  await page.keyboard.press("ArrowLeft");

  await page.getByRole("button", { name: "Show removed versions" }).click();
  await expect(page).toHaveURL(/removed=1/);
  // The same view, at the same version, with v3 in (four versions), flagged.
  await expect(page.getByRole("slider", { name: "Range end" })).toHaveAttribute(
    "aria-valuemax",
    "3",
  );
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v2, /);
  await cursor.focus();
  await page.keyboard.press("ArrowRight");
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v3 \(removed\), /);

  // Out again: the cursor falls back to the version before.
  await page.getByRole("button", { name: "Show removed versions" }).click();
  await expect(page).not.toHaveURL(/removed=1/);
  await expect(page.getByRole("slider", { name: "Range end" })).toHaveAttribute(
    "aria-valuemax",
    "2",
  );
  await expect(cursor).toHaveAttribute("aria-valuetext", /^v2, /);
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("the timeline is a sheet over the graph", async ({ page }) => {
    const { slug } = await shop(page, "tm-phone");
    await page.goto(`./${slug}/shop/time-machine`);
    await expect(node(page, "invoices")).toBeVisible();
    const cursor = page.getByRole("slider", { name: "Cursor" });
    // Folded: the bar plays and steps; the track waits in the sheet.
    await expect(page.getByRole("button", { name: "Play the range" })).toBeVisible();
    await expect(cursor).toBeHidden();
    await page.getByRole("button", { name: "Show the timeline" }).click();
    await expect(cursor).toBeVisible();
    await expect(page.getByRole("radio", { name: "At cursor" })).toBeVisible();
    await page.getByRole("button", { name: "Previous version" }).click();
    await expect(cursor).toHaveAttribute("aria-valuetext", /^v3, /);
    await page.getByRole("button", { name: "Hide the timeline" }).click();
    await expect(cursor).toBeHidden();
    // A table picked opens the sheet on its history.
    await node(page, "customers").locator("[data-table-header]").click();
    await expect(page.getByRole("region", { name: "public.customers" })).toBeVisible();
    await expect(cursor).toBeVisible();
    await shot(page, "phone, the sheet on a table's history");
    // Nothing wider than the screen.
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
      390,
    );
  });
});
