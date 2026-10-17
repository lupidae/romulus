import { readFileSync } from "node:fs";

import { type Browser, expect, type Page, test } from "@playwright/test";

import { unique } from "./helpers";

// Drafts in migration mode (docs/MVP.md "Review round 1", item 2), with the
// collab server and the sandbox compiling for real: a migration written by
// two people at once, the graph following it, publishing it with the
// version, and a schema draft switched to migration mode and updated.

const V1 = `CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL UNIQUE
);

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

/** Alice (admin), Carol (developer), Bob (reviewer); database `shop` at v1. */
async function team(browser: Browser) {
  const alice = await person(browser, "Alice");
  const orgName = unique("Modes");
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
  return { slug, alice, carol, bob };
}

const node = (p: Person, table: string) =>
  p.page.locator(`.react-flow__node[data-id="public.${table}"]`);
const row = (p: Person, table: string, column: string) =>
  node(p, table).locator(`[data-column="${column}"]`);
const compiled = (p: Person) => expect(p.page.getByTestId("compile-status")).toHaveText("Compiled");
const panel = (p: Person, name: string) => p.page.getByRole("tabpanel", { name });
/** A pane's editor text, without the others' carets and name labels. */
const textOf = (p: Person, name: string) =>
  panel(p, name)
    .locator(".cm-content")
    .evaluate((element) => {
      const content = element.cloneNode(true) as HTMLElement;
      content
        .querySelectorAll(".cm-ySelectionCaret, .cm-placeholder")
        .forEach((node) => node.remove());
      return content.textContent ?? "";
    });

/** Puts the caret at the end of a pane's editor. */
async function toEnd(p: Person, name: string) {
  await panel(p, name).locator(".cm-content").click();
  await p.page.keyboard.press("ControlOrMeta+End");
}

test("two developers write a migration; the graph follows; the version keeps it", async ({
  browser,
}) => {
  test.slow();
  const { slug, alice, carol, bob } = await team(browser);

  // "New migration", next to "New draft".
  await alice.page.goto(`./${slug}/shop`);
  await alice.page.getByRole("button", { name: "More ways to start a draft" }).click();
  await alice.page.getByRole("menuitem", { name: /New migration/ }).click();
  await alice.page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  for (const p of [carol, bob]) await p.page.goto(alice.page.url());
  for (const p of [alice, carol, bob]) {
    await expect(p.page.getByText("Migration from v1", { exact: true })).toBeVisible();
    await expect(p.page.getByRole("tab", { name: "Migration", selected: true })).toBeVisible();
    await compiled(p);
    await expect(node(p, "users")).toBeVisible();
  }
  await expect
    .poll(() => textOf(alice, "Migration"))
    .toContain("migration from v1, written by hand.");

  // Both type at once; Postgres runs v1, then the migration.
  await toEnd(alice, "Migration");
  await toEnd(carol, "Migration");
  await Promise.all([
    alice.page.keyboard.type("\nALTER TABLE public.users ADD COLUMN plan text;", { delay: 30 }),
    carol.page.keyboard.type("\nCREATE TABLE public.coupons (id bigint PRIMARY KEY);", {
      delay: 30,
    }),
  ]);
  for (const p of [alice, carol, bob]) {
    await expect(row(p, "users", "plan")).toBeVisible();
    await expect(node(p, "coupons")).toBeVisible();
    await compiled(p);
    await expect
      .poll(() => textOf(p, "Migration"))
      .toContain("ALTER TABLE public.users ADD COLUMN plan text;");
    await expect.poll(() => textOf(p, "Migration")).toContain("CREATE TABLE public.coupons");
  }
  // A mistake is underlined where it is, in the migration.
  const beforeMistake = await textOf(alice, "Migration");
  await alice.page.keyboard.type("\nALTER TABLE public.refunds ADD COLUMN reason text;");
  await expect(alice.page.getByTestId("compile-status")).toHaveText("1 error");
  await expect(panel(carol, "Migration").locator(".cm-lintRange-error")).toHaveText("refunds");
  // Undone, all of it (typing spans several undo steps when the machine is slow).
  await expect(async () => {
    if ((await textOf(alice, "Migration")) !== beforeMistake)
      await alice.page.keyboard.press("ControlOrMeta+z");
    expect(await textOf(alice, "Migration")).toBe(beforeMistake);
  }).toPass();
  await compiled(alice);

  // The reviewer watches: read-only, the schema as SQL read-only too.
  await expect(panel(bob, "Migration").locator(".cm-content")).toHaveAttribute(
    "contenteditable",
    "false",
  );
  await bob.page.getByRole("tab", { name: "SQL" }).click();
  await expect(bob.page.getByText("Read-only: what the migration makes of v1.")).toBeVisible();
  await expect.poll(() => textOf(bob, "SQL")).toContain("CREATE TABLE public.coupons");

  // Publishing keeps the migration with the version.
  await alice.page.getByRole("button", { name: "Publish v2" }).click();
  const dialog = alice.page.getByRole("dialog", { name: "Publish v2" });
  await dialog.getByRole("tab", { name: "Migration" }).click();
  await expect(dialog).toContainText("Written by hand in this draft.");
  await expect(dialog.locator(".cm-content")).toContainText("ADD COLUMN plan text");
  await dialog.getByRole("textbox", { name: "Message" }).fill("Plans and coupons");
  await dialog.getByRole("button", { name: "Publish v2" }).click();
  await alice.page.waitForURL(new RegExp(`/app/${slug}/shop$`));
  await expect(carol.page.getByText("Alice published this draft as v2")).toBeVisible();

  await alice.page.getByRole("button", { name: "Migration" }).click();
  const written = alice.page.getByRole("dialog", { name: "The migration of v2" });
  await expect(written.locator(".cm-content")).toContainText("CREATE TABLE public.coupons");
  const download = alice.page.waitForEvent("download");
  await written.getByRole("button", { name: "Download" }).click();
  const file = await download;
  expect(file.suggestedFilename()).toBe("v2_plans_and_coupons.sql");
  const content = readFileSync(await file.path(), "utf8");
  expect(content).toContain("ALTER TABLE public.users ADD COLUMN plan text;");
  expect(content).toContain("CREATE TABLE public.coupons (id bigint PRIMARY KEY);");
});

test("a schema draft becomes a migration, is updated onto newer versions, and goes back", async ({
  browser,
}) => {
  test.slow();
  const { slug, alice, carol } = await team(browser);
  await alice.page.goto(`./${slug}/shop`);
  await alice.page.getByRole("button", { name: "New draft" }).first().click();
  await alice.page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  await carol.page.goto(alice.page.url());
  await compiled(alice);
  await toEnd(alice, "SQL");
  await alice.page.keyboard.type("\nCREATE TABLE coupons (id bigint PRIMARY KEY);");
  await expect(node(alice, "coupons")).toBeVisible();
  await compiled(alice);

  // The Migration tab shows the generated migration, live.
  await alice.page.getByRole("tab", { name: "Migration" }).click();
  const generated = panel(alice, "Migration");
  await expect(generated).toContainText("Generated from the SQL: what to run on v1");
  await expect(generated.locator(".cm-content")).toContainText("CREATE TABLE public.coupons");

  // Edit migration: confirmed, then everyone edits the migration.
  await generated.getByRole("button", { name: "Edit migration" }).click();
  const confirm = alice.page.getByRole("alertdialog", { name: "Write the migration by hand?" });
  await confirm.getByRole("button", { name: "Edit migration" }).click();
  await expect(carol.page.getByText("Alice is writing the migration by hand")).toBeVisible();
  for (const p of [alice, carol]) {
    await expect(p.page.getByRole("tab", { name: "Migration", selected: true })).toBeVisible();
    await expect.poll(() => textOf(p, "Migration")).toContain("CREATE TABLE public.coupons");
    await compiled(p);
  }

  // v2 comes out (payments): the migration is applied to it as written.
  await call(alice.page, "POST", `/api/orgs/${slug}/databases/shop/versions`, {
    sql: `${V1}\nCREATE TABLE payments (id bigint PRIMARY KEY);\n`,
  });
  await alice.page.getByRole("button", { name: "Update draft (v2 is out)" }).click();
  await expect(alice.page.getByText("The migration applies to v2.")).toBeVisible();
  for (const p of [alice, carol]) {
    await expect(node(p, "payments")).toBeVisible();
    await expect(node(p, "coupons")).toBeVisible();
    await compiled(p);
  }

  // v3 already has coupons: the migration no longer applies, and says where.
  await call(alice.page, "POST", `/api/orgs/${slug}/databases/shop/versions`, {
    sql: `${V1}\nCREATE TABLE payments (id bigint PRIMARY KEY);\nCREATE TABLE coupons (id bigint PRIMARY KEY);\n`,
  });
  await alice.page.getByRole("button", { name: "Update draft (v3 is out)" }).click();
  await expect(alice.page.getByText("The migration no longer applies to v3")).toBeVisible();
  await expect(carol.page.getByTestId("compile-status")).toHaveText("1 error");
  // Postgres's own position: the name it already has.
  await expect(panel(carol, "Migration").locator(".cm-lintRange-error")).toHaveText("coupons");

  // It cannot be discarded onto v3 while it does not apply (that would
  // bring v2's schema back); emptied, it is v3, and goes.
  await alice.page.getByRole("button", { name: "More actions" }).click();
  await expect(alice.page.getByRole("menuitem", { name: /Discard migration…/ })).toBeDisabled();
  await alice.page.keyboard.press("Escape");
  // (Again until it is: the menu hands the focus back on its own time.)
  await expect(async () => {
    await panel(alice, "Migration").locator(".cm-content").click();
    await alice.page.keyboard.press("ControlOrMeta+a");
    await alice.page.keyboard.press("Backspace");
    expect((await textOf(alice, "Migration")).trim()).toBe("");
  }).toPass();
  await compiled(alice);
  await alice.page.getByRole("button", { name: "More actions" }).click();
  await alice.page.getByRole("menuitem", { name: "Discard migration…" }).click();
  const discard = alice.page.getByRole("alertdialog", { name: "Discard the migration?" });
  await expect(discard).toContainText("The schema becomes what the migration makes of v3");
  await discard.getByRole("button", { name: "Discard migration" }).click();
  for (const p of [alice, carol]) {
    await expect(p.page.getByRole("tab", { name: "SQL", selected: true })).toBeVisible();
    await expect.poll(() => textOf(p, "SQL")).toContain("CREATE TABLE public.coupons");
    await expect.poll(() => textOf(p, "SQL")).toContain("CREATE TABLE public.payments");
    await compiled(p);
  }
  await expect(carol.page.getByText("Alice discarded the migration")).toBeVisible();
});
