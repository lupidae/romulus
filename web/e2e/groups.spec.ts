import { type Browser, expect, type Locator, type Page, test } from "@playwright/test";

import { unique } from "./helpers";

// Groups in a live draft, through the collab server: one developer makes,
// names and moves a group, another sees each step as it lands, a reviewer
// sees the group without its tools, and one undo puts the move back for
// everyone. The single-person flows are e2e/dev/groups.spec.ts.

const V1 = `CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL UNIQUE
);

CREATE TABLE orders (
    id bigint PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id)
);

CREATE TABLE products (
    id bigint PRIMARY KEY,
    name text NOT NULL
);
`;

interface Person {
  page: Page;
  name: string;
  email: string;
}

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

async function person(browser: Browser, name: string): Promise<Person> {
  const page = await (await browser.newContext()).newPage();
  const email = `${name.toLowerCase()}-${Date.now().toString(36)}${Math.floor(Math.random() * 1e4)}@romulus.test`;
  const query = new URLSearchParams({ email, name, next: "/app/" });
  await page.goto(`/api/auth/dev-signin?${query.toString()}`);
  await page.waitForURL((url) => url.pathname.startsWith("/app/"));
  return { page, name, email };
}

const node = (p: Person, table: string) =>
  p.page.locator(`.react-flow__node[data-id="public.${table}"]`);
const header = (p: Person, table: string) => node(p, table).locator("[data-table-header]");
const frame = (p: Person, name: string) =>
  p.page.locator(".react-flow__node-tableGroup [data-group]").filter({ hasText: name });

async function box(locator: Locator) {
  const b = await locator.boundingBox();
  if (!b) throw new Error("not on screen");
  return b;
}

/** Alice (admin), Carol (developer), Bob (reviewer); `shop` at v1; a draft open for all. */
async function draft(browser: Browser) {
  const alice = await person(browser, "Alice");
  const orgName = unique("Groups");
  const slug = orgName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  await call(alice.page, "POST", "/api/orgs", { name: orgName, slug });
  const join = async (role: "developer" | "reviewer", name: string) => {
    const invited = await person(browser, name);
    const created = (await call(alice.page, "POST", `/api/orgs/${slug}/invitations`, {
      email: invited.email,
      role,
    })) as { url: string };
    await call(invited.page, "POST", `/api/invitations/${created.url.split("/invite/")[1]}/accept`);
    return invited;
  };
  const carol = await join("developer", "Carol");
  const bob = await join("reviewer", "Bob");
  await call(alice.page, "POST", `/api/orgs/${slug}/databases`, { name: "Shop", slug: "shop" });
  await call(alice.page, "POST", `/api/orgs/${slug}/databases/shop/versions`, { sql: V1 });

  await alice.page.goto(`./${slug}/shop`);
  await alice.page.getByRole("button", { name: "New draft" }).first().click();
  await alice.page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  for (const p of [carol, bob]) await p.page.goto(alice.page.url());
  for (const p of [alice, carol, bob]) {
    await expect(p.page.getByTestId("compile-status")).toHaveText("Compiled");
    await expect(node(p, "users")).toBeVisible();
  }
  return { alice, carol, bob };
}

test("a group made, named and moved by one developer lands for everyone; undo puts it back", async ({
  browser,
}) => {
  test.slow();
  const { alice, carol, bob } = await draft(browser);

  // Alice picks two tables and groups them with G, naming the group in place.
  await header(alice, "users").click({ modifiers: ["Shift"] });
  await header(alice, "orders").click({ modifiers: ["Shift"] });
  await alice.page.keyboard.press("g");
  const name = alice.page.getByRole("textbox", { name: /^Rename Group \d+$/ });
  await expect(name).toBeFocused();
  await name.fill("Checkout");
  await name.press("Enter");
  for (const p of [alice, carol]) await expect(frame(p, "Checkout")).toContainText("2 tables");
  await expect(frame(bob, "Checkout")).toBeVisible();

  // Bob reviews: he sees the group, not its tools.
  await expect(frame(bob, "Checkout").locator("[data-group-handle]")).toHaveCount(0);
  await expect(bob.page.getByRole("button", { name: "Groups" })).toHaveCount(0);

  // Alice drags the group by its label: both tables move, at once, for Carol too.
  const before = {
    users: await box(node(carol, "users")),
    orders: await box(node(carol, "orders")),
  };
  const products = await box(node(carol, "products"));
  const handle = await box(frame(alice, "Checkout").locator("[data-group-handle]"));
  const at = { x: handle.x + 40, y: handle.y + handle.height / 2 };
  await alice.page.mouse.move(at.x, at.y);
  await alice.page.mouse.down();
  await alice.page.mouse.move(at.x + 120, at.y + 90, { steps: 12 });
  // The tables follow the pointer a frame at a time: let the last one draw.
  await alice.page.evaluate(
    () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
  );
  const released = await box(node(alice, "users"));
  await alice.page.mouse.up();
  // For Alice the tables stay where she let go: no jump back while the move
  // goes round through the collab server.
  expect(await box(node(alice, "users"))).toEqual(released);

  await expect
    .poll(async () => (await box(node(carol, "users"))).x - before.users.x)
    .toBeGreaterThan(60);
  const users = await box(node(carol, "users"));
  const orders = await box(node(carol, "orders"));
  expect(orders.x - before.orders.x).toBeCloseTo(users.x - before.users.x, 0);
  expect(orders.y - before.orders.y).toBeCloseTo(users.y - before.users.y, 0);
  expect(users.y - before.users.y).toBeGreaterThan(40);
  expect(await box(node(carol, "products"))).toEqual(products);
  expect(await box(node(alice, "users"))).toEqual(released);

  // One undo of Alice's takes the whole move back, for everyone.
  await alice.page.getByRole("application", { name: "Schema graph" }).focus();
  await alice.page.keyboard.press("ControlOrMeta+z");
  await expect
    .poll(async () => Math.round((await box(node(carol, "users"))).x))
    .toBe(Math.round(before.users.x));
  expect((await box(node(carol, "orders"))).y).toBeCloseTo(before.orders.y, 0);
  await expect(frame(carol, "Checkout")).toContainText("2 tables");
});
