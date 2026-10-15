import { type Browser, expect, type Page, test } from "@playwright/test";
import * as encoding from "lib0/encoding";
import * as Y from "yjs";

import { unique } from "./helpers";

// Drafts, live: several people in one draft (two browser contexts or three),
// as the collab server sees them. The stack's database is shared between
// runs: every test makes its own organization.

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

async function person(browser: Browser, name: string): Promise<Person> {
  const page = await (await browser.newContext()).newPage();
  const email = `${name.toLowerCase()}-${Date.now().toString(36)}${Math.floor(Math.random() * 1e4)}@romulus.test`;
  const query = new URLSearchParams({ email, name, next: "/app/" });
  await page.goto(`/api/auth/dev-signin?${query.toString()}`);
  await page.waitForURL((url) => url.pathname.startsWith("/app/"));
  return { page, name, email };
}

/**
 * Alice (admin), Carol (developer) and Bob (reviewer) in a new organization
 * whose database `shop` has v1.
 */
async function team(browser: Browser) {
  const alice = await person(browser, "Alice");
  const orgName = unique("Drafts");
  const slug = orgName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  await call(alice.page, "POST", "/api/orgs", { name: orgName, slug });
  const join = async (role: "developer" | "reviewer", name: string) => {
    const invited = await person(browser, name);
    const created = (await call(alice.page, "POST", `/api/orgs/${slug}/invitations`, {
      email: invited.email,
      role,
    })) as { url: string };
    const token = created.url.split("/invite/")[1];
    await call(invited.page, "POST", `/api/invitations/${token}/accept`);
    return invited;
  };
  const carol = await join("developer", "Carol");
  const bob = await join("reviewer", "Bob");
  await call(alice.page, "POST", `/api/orgs/${slug}/databases`, { name: "Shop", slug: "shop" });
  await call(alice.page, "POST", `/api/orgs/${slug}/databases/shop/versions`, {
    sql: V1,
    message: "First cut",
  });
  return { slug, alice, carol, bob };
}

/** Alice starts a draft from the database page; everyone else opens it. */
async function openDraft(slug: string, alice: Person, ...others: Person[]) {
  await alice.page.goto(`./${slug}/shop`);
  await alice.page.getByRole("button", { name: "New draft" }).first().click();
  await alice.page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  const url = alice.page.url();
  for (const other of others) await other.page.goto(url);
  for (const p of [alice, ...others]) {
    await expect(p.page.getByTestId("compile-status")).toHaveText("Compiled");
    await expect(p.page.locator(".cm-content")).toContainText("CREATE TABLE orders");
  }
  return url;
}

const sqlOf = (p: Person) => p.page.locator(".cm-content");
/** The SQL in someone's editor, without the others' carets and name labels. */
const docText = (p: Person) =>
  p.page.evaluate(() => {
    const content = document.querySelector(".cm-content")!.cloneNode(true) as HTMLElement;
    content.querySelectorAll(".cm-ySelectionCaret").forEach((caret) => caret.remove());
    return content.textContent;
  });
const node = (p: Person, table: string) =>
  p.page.locator(`.react-flow__node[data-id="public.${table}"]`);

test("two developers edit one draft at once while a reviewer watches", async ({ browser }) => {
  test.slow();
  const { slug, alice, carol, bob } = await team(browser);
  await openDraft(slug, alice, carol, bob);

  // Both type at once, at both ends of the text.
  await sqlOf(alice).click();
  await alice.page.keyboard.press("ControlOrMeta+End");
  await sqlOf(carol).click();
  await carol.page.keyboard.press("ControlOrMeta+Home");
  await Promise.all([
    alice.page.keyboard.type("\nCREATE TABLE invoices (id bigint PRIMARY KEY);\n", { delay: 20 }),
    carol.page.keyboard.type("CREATE TABLE teams (id bigint PRIMARY KEY);\n\n", { delay: 20 }),
  ]);
  for (const p of [alice, carol, bob]) {
    await expect(sqlOf(p)).toContainText("CREATE TABLE invoices");
    await expect(sqlOf(p)).toContainText("CREATE TABLE teams");
    await expect(node(p, "invoices")).toBeVisible();
    await expect(node(p, "teams")).toBeVisible();
  }
  await expect.poll(() => docText(alice)).toBe(await docText(carol));

  // Carets with names, both ways; a pointer on the graph.
  await expect(alice.page.locator(".cm-ySelectionInfo", { hasText: "Carol" })).toBeVisible();
  await expect(carol.page.locator(".cm-ySelectionInfo", { hasText: "Alice" })).toBeVisible();
  const graph = await alice.page.locator(".react-flow__pane").boundingBox();
  await alice.page.mouse.move(graph!.x + 200, graph!.y + 200);
  await alice.page.mouse.move(graph!.x + 240, graph!.y + 220, { steps: 4 });
  await expect(bob.page.getByTestId("remote-pointer").filter({ hasText: "Alice" })).toBeVisible();

  // Carol drags a table while Alice types: the move lands for everyone,
  // the typing too.
  const before = await node(bob, "users").boundingBox();
  const from = await node(carol, "users").boundingBox();
  await sqlOf(alice).click();
  await alice.page.keyboard.press("ControlOrMeta+End");
  const typing = alice.page.keyboard.type("-- typed during a drag\n", { delay: 30 });
  await carol.page.mouse.move(from!.x + 40, from!.y + 12);
  await carol.page.mouse.down();
  await carol.page.mouse.move(from!.x + 40, from!.y + 232, { steps: 12 });
  await carol.page.mouse.up();
  await typing;
  await expect
    .poll(async () => (await node(bob, "users").boundingBox())!.y - before!.y)
    .toBeGreaterThan(50);
  await expect(sqlOf(bob)).toContainText("-- typed during a drag");

  // A mistake shows for everyone, where it is; undoing it clears it.
  await alice.page.waitForTimeout(600);
  await alice.page.keyboard.type("CREATE TABLE broken (id bigint,);", { delay: 10 });
  for (const p of [alice, bob]) {
    await expect(p.page.getByTestId("compile-status")).toHaveText("1 error");
    await expect(p.page.locator(".cm-lintRange-error")).toBeVisible();
  }
  await alice.page.keyboard.press("ControlOrMeta+z");
  await expect(sqlOf(bob)).not.toContainText("broken");
  await expect(bob.page.getByTestId("compile-status")).toHaveText("Compiled");

  // Bob reads only: no typing, no dragging…
  await expect(bob.page.getByText("You’re viewing as a reviewer.")).toBeVisible();
  await expect(sqlOf(bob)).toHaveAttribute("contenteditable", "false");
  const text = await docText(alice);
  await sqlOf(bob).click();
  await bob.page.keyboard.type("DROP TABLE users;");
  // …and an update sent by hand, past the editor, is dropped by the server.
  const forged = new Y.Doc();
  forged.getText("sql").insert(0, "-- forged by a reviewer\n");
  const message = encoding.createEncoder();
  encoding.writeVarUint(message, 0); // sync
  encoding.writeVarUint(message, 2); // update
  encoding.writeVarUint8Array(message, Y.encodeStateAsUpdate(forged));
  const draftId = bob.page.url().split("/drafts/")[1];
  const sent = await bob.page.evaluate(
    async ({ path, bytes }) =>
      new Promise<boolean>((resolve) => {
        const socket = new WebSocket(`${location.origin.replace(/^http/, "ws")}${path}`);
        socket.binaryType = "arraybuffer";
        socket.onopen = () => {
          socket.send(new Uint8Array(bytes));
          setTimeout(() => {
            socket.close();
            resolve(true);
          }, 500);
        };
        socket.onerror = () => resolve(false);
      }),
    { path: `/api/orgs/${slug}/drafts/${draftId}/ws`, bytes: [...encoding.toUint8Array(message)] },
  );
  expect(sent).toBe(true);
  await alice.page.waitForTimeout(500);
  expect(await docText(alice)).toBe(text);
  await expect(sqlOf(carol)).not.toContainText("forged");
  await expect(sqlOf(alice)).not.toContainText("DROP TABLE");
});

test("publishing a draft creates the next version and closes it for everyone", async ({
  browser,
}) => {
  test.slow();
  const { slug, alice, bob } = await team(browser);
  await openDraft(slug, alice, bob);

  await sqlOf(alice).click();
  await alice.page.keyboard.press("ControlOrMeta+End");
  await alice.page.keyboard.type("\nCREATE TABLE invoices (id bigint PRIMARY KEY);\n", {
    delay: 10,
  });
  await expect(alice.page.getByTestId("compile-status")).toHaveText("Compiled");
  await expect(node(alice, "invoices")).toBeVisible();

  // Renamed inline; Bob sees the name change.
  await alice.page.getByRole("button", { name: /rename$/ }).click();
  await alice.page.getByLabel("Draft name").fill("Add invoices");
  await alice.page.keyboard.press("Enter");
  await expect(bob.page.getByRole("heading", { name: "Add invoices" })).toBeVisible();

  await alice.page.getByRole("button", { name: "Publish v2" }).click();
  const dialog = alice.page.getByRole("dialog", { name: "Publish v2" });
  await expect(dialog.getByText("1 table added since v1")).toBeVisible();
  await expect(dialog.getByText("public.invoices")).toBeVisible();
  await dialog.getByRole("tab", { name: "Graph" }).click();
  await expect(dialog.locator('.react-flow__node[data-id="public.invoices"]')).toBeVisible();
  await dialog.getByLabel("Message").fill("Invoices");
  await dialog.getByRole("button", { name: "Publish v2" }).click();

  await expect(alice.page.getByText("Published v2")).toBeVisible();
  await expect(alice.page).toHaveURL(new RegExp(`/app/${slug}/shop$`));
  await expect(bob.page.getByText("Alice published this draft as v2")).toBeVisible();
  await expect(sqlOf(bob)).toHaveAttribute("contenteditable", "false");

  const versions = (await call(alice.page, "GET", `/api/orgs/${slug}/databases/shop/versions`)) as {
    versions: { number: number; origin: string; message: string | null }[];
  };
  expect(versions.versions[0]).toMatchObject({ number: 2, origin: "draft", message: "Invoices" });
  const drafts = (await call(
    alice.page,
    "GET",
    `/api/orgs/${slug}/databases/shop/drafts`,
  )) as unknown as unknown[];
  expect(drafts).toEqual([]);
});

test("signing out in another tab sends an open draft to sign-in", async ({ browser }) => {
  test.slow();
  const { slug, alice } = await team(browser);
  await openDraft(slug, alice);

  // Same browser, same session: the server closes the draft's socket for good.
  await call(alice.page, "POST", "/api/auth/logout");
  await expect(alice.page).toHaveURL(/\/app\/signin/);
  await expect(alice.page.getByText("Your session expired")).toBeVisible();
});
