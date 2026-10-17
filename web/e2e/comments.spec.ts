import { type Browser, expect, type Page, test } from "@playwright/test";

import { unique } from "./helpers";

// Comment threads in a live draft (docs/MVP.md "Review round 1", item 6):
// a developer comments on a column, a reviewer replies and resolves, a
// rename carries the thread, a `---` line typed or pasted in the SQL becomes a
// thread and leaves no text (one left in it is refused by the server), an
// admin deletes a reviewer's reply (in the activity), and publishing keeps the
// discussion with the version.

const V1 = `CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL
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

/** Alice (admin) and Bob (reviewer); database `shop` at v1, a draft open for both. */
async function draft(browser: Browser) {
  const alice = await person(browser, "Alice");
  const orgName = unique("Comments");
  const slug = orgName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  await call(alice.page, "POST", "/api/orgs", { name: orgName, slug });
  const bob = await person(browser, "Bob");
  const invited = (await call(alice.page, "POST", `/api/orgs/${slug}/invitations`, {
    email: bob.email,
    role: "reviewer",
  })) as { url: string };
  await call(bob.page, "POST", `/api/invitations/${invited.url.split("/invite/")[1]}/accept`);
  await call(alice.page, "POST", `/api/orgs/${slug}/databases`, { name: "Shop", slug: "shop" });
  await call(alice.page, "POST", `/api/orgs/${slug}/databases/shop/versions`, { sql: V1 });

  await alice.page.goto(`./${slug}/shop`);
  await alice.page.getByRole("button", { name: "New draft" }).first().click();
  await alice.page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  await bob.page.goto(alice.page.url());
  for (const p of [alice, bob]) {
    await expect(p.page.getByTestId("compile-status")).toHaveText("Compiled");
    await expect(node(p, "users")).toBeVisible();
  }
  return { slug, alice, bob };
}

const node = (p: Person, table: string) =>
  p.page.locator(`.react-flow__node[data-id="public.${table}"]`);
const row = (p: Person, table: string, column: string) =>
  node(p, table).locator(`[data-column="${column}"]`);
const panel = (p: Person, title = "Comments") => p.page.getByRole("region", { name: title });
const thread = (p: Person, label: string, title = "Comments") =>
  panel(p, title).getByRole("button", { name: new RegExp(`^${label.replace(".", "\\.")}`) });
/** The SQL in someone's editor, without carets, name labels or comment lines. */
const docText = (p: Person) =>
  p.page.evaluate(() => {
    const content = document.querySelector(".cm-content")!.cloneNode(true) as HTMLElement;
    content
      .querySelectorAll(".cm-ySelectionCaret, .cm-threads")
      .forEach((widget) => widget.remove());
    return content.textContent;
  });

test("a column's thread: commented, carried by a rename, answered and resolved by a reviewer, kept by the version", async ({
  browser,
}) => {
  test.slow();
  const { slug, alice, bob } = await draft(browser);

  // Alice: comment mode (C), the column, a few words.
  await alice.page.getByRole("application", { name: "Schema graph" }).focus();
  await alice.page.keyboard.press("c");
  await expect(alice.page.getByRole("button", { name: "Comment mode" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await row(alice, "users", "email").click();
  const composer = alice.page.getByRole("combobox", { name: "Comment on users.email" });
  await expect(composer).toBeFocused();
  await composer.fill("Should email be citext, so lookups ignore case?");
  await composer.press("Enter");
  await expect(thread(alice, "users.email")).toBeVisible();

  // Bob sees a pin on the column, unread.
  const pin = bob.page.getByRole("button", { name: /^Comment on users\.email, unread/ });
  await expect(pin).toBeVisible();

  // Alice renames the table from the graph: the thread follows it.
  await node(alice, "users").locator(".sg-name").dblclick();
  await alice.page.getByRole("textbox", { name: "Rename users" }).fill("members");
  await alice.page.keyboard.press("Enter");
  await expect(node(bob, "members")).toBeVisible();
  await expect(thread(alice, "members.email")).toBeVisible();
  const moved = bob.page.getByRole("button", { name: /^Comment on members\.email, unread/ });
  await expect(moved).toBeVisible();

  // Bob, a reviewer, opens it from the pin, replies mentioning Alice, resolves.
  await moved.click();
  await expect(panel(bob)).toContainText("Should email be citext, so lookups ignore case?");
  const reply = bob.page.getByRole("combobox", { name: "Reply to Alice" });
  await reply.fill("Agreed. @Ali");
  // Alice, reading the thread, sees him write.
  await expect(panel(alice)).toContainText("Bob is typing…");
  await bob.page.getByRole("option", { name: /Alice/ }).click();
  await reply.pressSequentially("we have duplicates by case.");
  await reply.press("Enter");
  await expect(panel(alice)).toContainText("we have duplicates by case.");
  await expect(panel(alice).getByText("@Alice", { exact: true })).toBeVisible();
  await panel(bob).getByRole("button", { name: "Resolve" }).click();

  // Alice, reading it, sees it resolved (it stays open in front of her); its pin goes.
  await expect(panel(alice)).toContainText("Resolved by Bob");
  await expect(alice.page.getByRole("button", { name: /^Comment on members\.email/ })).toHaveCount(
    0,
  );

  // Publishing keeps the discussion with v2, read-only.
  await alice.page.getByRole("button", { name: "Publish v2" }).click();
  const dialog = alice.page.getByRole("dialog", { name: "Publish v2" });
  await expect(dialog).toContainText("stays with v2 as its review history");
  await dialog.getByRole("button", { name: "Publish v2" }).click();
  await alice.page.waitForURL(new RegExp(`/${slug}/shop$`));
  await alice.page.getByRole("button", { name: /^Discussion/ }).click();
  const history = panel(alice, "Discussion");
  await history.getByRole("radio", { name: /Resolved/ }).click();
  await thread(alice, "members.email", "Discussion").click();
  await expect(history).toContainText("From the draft's review");
  await expect(history).toContainText("we have duplicates by case.");
  await expect(history.getByRole("combobox", { name: /Reply/ })).toHaveCount(0);
});

test("a `---` line typed in the SQL becomes a thread on the statement below and leaves no text", async ({
  browser,
}) => {
  test.slow();
  const { alice, bob } = await draft(browser);

  // Alice opens a line above the orders table and writes a comment line.
  await alice.page.locator(".cm-line", { hasText: "-- Orders, one per checkout." }).click();
  await alice.page.keyboard.press("End");
  await alice.page.keyboard.press("Enter");
  await alice.page.keyboard.type("--- Why bigint ids here?");
  await alice.page.keyboard.press("Enter");

  // Nobody's SQL holds it; everyone's shows it above the statement.
  for (const p of [alice, bob]) {
    await expect(p.page.locator(".cm-thread")).toContainText("Alice: Why bigint ids here?");
    await expect.poll(() => docText(p)).not.toContain("---");
    await expect
      .poll(() => docText(p))
      .toContain("-- Orders, one per checkout.CREATE TABLE orders");
    await expect(p.page.getByRole("button", { name: /^Comment on orders/ })).toBeVisible();
  }
  await expect(alice.page.getByTestId("compile-status")).toHaveText("Compiled");

  // One undo brings the line back and takes the thread away.
  await alice.page.locator(".cm-content").press("ControlOrMeta+z");
  await expect.poll(() => docText(bob)).toContain("--- Why bigint ids here?");
  await expect(bob.page.locator(".cm-thread")).toHaveCount(0);
});

test("`---` typed into a statement's line or pasted becomes a thread; one left in the SQL is refused", async ({
  browser,
}) => {
  test.slow();
  const { slug, alice, bob } = await draft(browser);

  // Typed at the start of an existing line: Enter splits it, the words go.
  await alice.page.locator(".cm-line", { hasText: "CREATE TABLE orders (" }).click();
  await alice.page.keyboard.press("Home");
  await alice.page.keyboard.type("--- Who owns orders?");
  await alice.page.keyboard.press("Enter");
  await expect(bob.page.locator(".cm-thread")).toContainText("Alice: Who owns orders?");
  await expect
    .poll(() => docText(bob))
    .toContain("-- Orders, one per checkout.CREATE TABLE orders (");

  // Pasted above the users table: the text stays, the `---` line is a thread.
  await alice.page.locator(".cm-line", { hasText: "CREATE TABLE users (" }).click();
  await alice.page.keyboard.press("Home");
  await alice.page.evaluate(() => {
    const event = new ClipboardEvent("paste", {
      clipboardData: new DataTransfer(),
      bubbles: true,
      cancelable: true,
    });
    event.clipboardData!.setData("text/plain", "--- Pasted: is email unique?\n-- People.\n");
    document.querySelector(".cm-content")!.dispatchEvent(event);
  });
  await expect(bob.page.locator(".cm-thread", { hasText: "is email unique?" })).toBeVisible();
  await expect.poll(() => docText(bob)).toContain("-- People.CREATE TABLE users (");
  await expect.poll(() => docText(alice)).not.toContain("---");
  await expect(alice.page.getByTestId("compile-status")).toHaveText("Compiled");

  // Bob (a reviewer) replies; Alice, an admin, deletes his reply: recorded.
  await bob.page.locator(".cm-thread", { hasText: "Who owns orders?" }).click();
  const reply = bob.page.getByRole("combobox", { name: "Reply to Alice" });
  await reply.fill("Spam, spam, spam.");
  await reply.press("Enter");
  await alice.page.locator(".cm-thread", { hasText: "Who owns orders?" }).click();
  await panel(alice).getByRole("button", { name: "More on Bob's reply" }).click();
  await alice.page.getByRole("menuitem", { name: /Delete/ }).click();
  const confirm = alice.page.getByRole("alertdialog", { name: "Delete Bob's comment?" });
  await expect(confirm).toContainText("recorded in the database's activity");
  await confirm.getByRole("button", { name: "Delete comment" }).click();
  await expect(panel(bob)).not.toContainText("Spam, spam, spam.");
  await expect(panel(bob)).toContainText("Who owns orders?");

  // A `---` line nobody posted is an error, never kept in the SQL.
  await alice.page.locator(".cm-content").press("ControlOrMeta+End");
  await alice.page.keyboard.type("--- never posted", { delay: 10 });
  await expect(bob.page.getByTestId("compile-status")).toHaveText("1 error");
  await bob.page.getByTestId("compile-status").click();
  await expect(bob.page.getByRole("dialog")).toContainText(
    "This --- line is a comment that was never posted.",
  );

  // The admin's deletion is in the database's activity (the room's writer
  // records it a moment after).
  await alice.page.goto(`./${slug}/shop/settings`);
  const activity = alice.page.getByRole("list", { name: "Activity" });
  await expect(async () => {
    await alice.page.reload();
    await expect(activity.getByText("Alice deleted Bob's comment")).toBeVisible({ timeout: 2000 });
  }).toPass();
  await expect(activity.getByText("“Spam, spam, spam.”")).toBeVisible();
});
