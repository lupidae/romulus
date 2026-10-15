import { type Browser, expect, type Page, test } from "@playwright/test";

import { unique } from "./helpers";

// Editing the schema from the graph in a live draft, with the collab server
// and the sandbox compiling for real: two developers at once (one in the
// graph, one typing), a reviewer watching, undo, and Tidy.

const V1 = `CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL UNIQUE
);

-- Orders, one per checkout.
CREATE TABLE orders (
    id bigint PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id)
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

/** Alice (admin), Carol (developer), Bob (reviewer); database `shop` at v1, a draft open for all. */
async function draft(browser: Browser) {
  const alice = await person(browser, "Alice");
  const orgName = unique("Graph");
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
  const draftId = alice.page.url().split("/drafts/")[1];
  for (const p of [carol, bob]) await p.page.goto(alice.page.url());
  for (const p of [alice, carol, bob]) {
    await expect(p.page.getByTestId("compile-status")).toHaveText("Compiled");
    await expect(node(p, "users")).toBeVisible();
  }
  return { slug, draftId, alice, carol, bob };
}

const node = (p: Person, table: string) =>
  p.page.locator(`.react-flow__node[data-id="public.${table}"]`);
const row = (p: Person, table: string, column: string) =>
  node(p, table).locator(`[data-column="${column}"]`);
const graph = (p: Person) => p.page.getByRole("application", { name: "Schema graph" });
/** The SQL in someone's editor, without the others' carets and name labels. */
const docText = (p: Person) =>
  p.page.evaluate(() => {
    const content = document.querySelector(".cm-content")!.cloneNode(true) as HTMLElement;
    content.querySelectorAll(".cm-ySelectionCaret").forEach((caret) => caret.remove());
    return content.textContent;
  });
const compiled = (p: Person) => expect(p.page.getByTestId("compile-status")).toHaveText("Compiled");

test("graph edits become SQL for everyone while others type, and undo whole", async ({
  browser,
}) => {
  test.slow();
  const { alice, carol, bob } = await draft(browser);
  const where = await node(bob, "users").boundingBox();

  // Carol types at the top while Alice renames a column in the graph.
  await carol.page.locator(".cm-content").click();
  await carol.page.keyboard.press("ControlOrMeta+Home");
  const typing = carol.page.keyboard.type("-- Carol was here\n", { delay: 40 });
  await row(alice, "users", "email").getByText("email", { exact: true }).dblclick();
  await alice.page.getByRole("textbox", { name: "Rename email" }).fill("mail");
  await alice.page.keyboard.press("Enter");
  await typing;

  for (const p of [alice, carol, bob]) {
    await expect
      .poll(() => docText(p))
      .toContain("ALTER TABLE public.users RENAME COLUMN email TO mail;");
    await expect.poll(() => docText(p)).toContain("-- Carol was here");
    await expect(row(p, "users", "mail")).toBeVisible();
    await compiled(p);
  }
  await expect.poll(() => docText(alice)).toBe(await docText(bob));
  // The table stayed where it was.
  const after = await node(bob, "users").boundingBox();
  expect(after?.x).toBeCloseTo(where!.x, 0);
  expect(after?.y).toBeCloseTo(where!.y, 0);

  // A column added from the + of the header, then a foreign key drawn from
  // it to the primary key; Postgres compiles both.
  await node(alice, "users").locator("[data-table-header]").hover();
  await alice.page.getByRole("button", { name: "Add a column to users" }).click();
  const add = alice.page.getByRole("dialog", { name: "Add a column" });
  await add.getByRole("textbox", { name: "Name" }).fill("manager_id");
  await add.getByRole("combobox", { name: "Type" }).fill("bigint");
  await add.getByRole("combobox", { name: "Type" }).press("Enter");
  await add.getByRole("button", { name: "Add column" }).click();
  await expect(row(bob, "users", "manager_id")).toBeVisible();
  await compiled(alice);

  const from = row(alice, "users", "manager_id");
  await from.hover();
  const grip = await from.locator("[data-link-grip]").boundingBox();
  const to = await row(alice, "users", "id").boundingBox();
  await alice.page.mouse.move(grip!.x + grip!.width / 2, grip!.y + grip!.height / 2);
  await alice.page.mouse.down();
  await alice.page.mouse.move(to!.x + 30, to!.y + to!.height / 2, { steps: 8 });
  await alice.page.mouse.up();
  const fk = alice.page.getByRole("dialog", { name: "Add a foreign key" });
  await fk.getByRole("combobox").click();
  await alice.page.getByRole("option", { name: /Set null/ }).click();
  await fk.getByRole("button", { name: "Add foreign key" }).click();
  for (const p of [alice, bob]) {
    await expect
      .poll(() => docText(p))
      .toContain(
        "ALTER TABLE public.users ADD CONSTRAINT users_manager_id_fkey FOREIGN KEY (manager_id) REFERENCES public.users (id) ON DELETE SET NULL;",
      );
    await compiled(p);
    await expect(row(p, "users", "manager_id").getByLabel("Foreign key")).toBeVisible();
  }

  // Dropping a column: confirmed beside it, gone for everyone…
  await row(alice, "orders", "user_id").click({ button: "right" });
  await alice.page.getByRole("menuitem", { name: "Drop column…" }).click();
  await alice.page
    .getByRole("dialog", { name: "Drop" })
    .getByRole("button", { name: "Drop column" })
    .click();
  await expect(row(bob, "orders", "user_id")).toHaveCount(0);
  await compiled(alice);
  // …and back with one undo, text and graph.
  await graph(alice).focus();
  await alice.page.keyboard.press("ControlOrMeta+z");
  await expect(row(bob, "orders", "user_id")).toBeVisible();
  await expect.poll(() => docText(bob)).not.toContain("DROP COLUMN");
  await compiled(bob);
});

test("Tidy rewrites the SQL canonically, the schema identical, and resets undo", async ({
  browser,
}) => {
  test.slow();
  const { slug, draftId, alice, carol, bob } = await draft(browser);
  const preview = `/api/orgs/${slug}/databases/shop/drafts/${draftId}/publish`;

  // Graph edits by two editors, each on their own undo history.
  await row(alice, "users", "email").getByText("email", { exact: true }).dblclick();
  await alice.page.getByRole("textbox", { name: "Rename email" }).fill("mail");
  await alice.page.keyboard.press("Enter");
  await node(carol, "orders").locator("[data-table-header]").click({ button: "right" });
  await carol.page.getByRole("menuitem", { name: "Add comment" }).click();
  await carol.page.getByRole("textbox", { name: "Comment" }).fill("One per checkout");
  await carol.page.keyboard.press("ControlOrMeta+Enter");
  await expect.poll(() => docText(bob)).toContain("COMMENT ON TABLE public.orders");
  await expect.poll(() => docText(bob)).toContain("RENAME COLUMN email TO mail");
  await compiled(alice);
  const before = (await call(alice.page, "GET", preview))!.model;

  await alice.page.getByRole("button", { name: "More actions" }).click();
  await alice.page.getByRole("menuitem", { name: "Tidy SQL…" }).click();
  const confirm = alice.page.getByRole("alertdialog", { name: "Tidy the SQL?" });
  await expect(confirm).toContainText("edits made before it can no longer be undone");
  await expect(confirm).toContainText("the 2 lines of -- comments are lost");
  await confirm.getByRole("button", { name: "Tidy SQL" }).click();
  await expect(alice.page.getByText("SQL tidied")).toBeVisible();
  // Carol is told her undo history went with it.
  await expect(carol.page.getByText("Undo history was cleared by Tidy")).toBeVisible();

  for (const p of [alice, carol, bob]) {
    await expect.poll(() => docText(p)).not.toContain("-- edits from the graph");
    await expect.poll(() => docText(p)).toContain("CREATE TABLE public.users");
    await expect
      .poll(() => docText(p))
      .toContain("COMMENT ON TABLE public.orders IS 'One per checkout';");
    await compiled(p);
    await expect(row(p, "users", "mail")).toBeVisible();
  }
  const after = (await call(alice.page, "GET", preview))!.model;
  expect(after).toEqual(before);

  // Undo after Tidy takes nothing back from before it: the text stays whole.
  const tidy = await docText(bob);
  for (const p of [alice, carol]) {
    await graph(p).focus();
    await p.page.keyboard.press("ControlOrMeta+z");
    await p.page.keyboard.press("ControlOrMeta+z");
  }
  await bob.page.waitForTimeout(500);
  expect(await docText(bob)).toBe(tidy);
  await expect(row(bob, "users", "mail")).toBeVisible();
  await compiled(bob);
});

test("a reviewer gets no way to edit the schema from the graph", async ({ browser }) => {
  test.slow();
  const { bob } = await draft(browser);
  await node(bob, "users").locator("[data-table-header]").hover();
  await expect(bob.page.getByRole("button", { name: "Add a column to users" })).toHaveCount(0);
  await expect(bob.page.getByRole("button", { name: "Add a table" })).toHaveCount(0);
  await row(bob, "users", "email").click({ button: "right" });
  await expect(bob.page.getByRole("menu")).toHaveCount(0);
  await row(bob, "users", "email").getByText("email", { exact: true }).dblclick();
  await expect(bob.page.getByRole("textbox", { name: "Rename email" })).toHaveCount(0);
  await expect(bob.page.locator("[data-link-grip]")).toHaveCount(0);
  await expect(bob.page.getByRole("button", { name: "More actions" })).toHaveCount(0);
});
