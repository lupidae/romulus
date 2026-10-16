import { type Browser, expect, type Page, test } from "@playwright/test";

import { unique } from "./helpers";

// Review round 1: an admin removes a version and restores it; approvals
// before publishing, given by someone who did not edit, reset by an edit.
// The stack's database is shared between runs: every test makes its own
// organization.

const V1 = `CREATE TABLE users (
    id bigint PRIMARY KEY,
    email text NOT NULL UNIQUE
);
`;
const V2 = `${V1}
CREATE TABLE orders (
    id bigint PRIMARY KEY,
    user_id bigint NOT NULL REFERENCES users (id)
);
`;

interface Person {
  page: Page;
  name: string;
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
  return { page, name };
}

/** Alice (admin), Carol (developer), Bob (reviewer); `shop` with v1 and v2. */
async function team(browser: Browser) {
  const alice = await person(browser, "Alice");
  const orgName = unique("Governance");
  const slug = orgName.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  await call(alice.page, "POST", "/api/orgs", { name: orgName, slug });
  const join = async (role: "developer" | "reviewer", name: string) => {
    const invited = await person(browser, name);
    const emailOf = (await call(invited.page, "GET", "/api/me")) as { email: string };
    const created = (await call(alice.page, "POST", `/api/orgs/${slug}/invitations`, {
      email: emailOf.email,
      role,
    })) as { url: string };
    const token = created.url.split("/invite/")[1];
    await call(invited.page, "POST", `/api/invitations/${token}/accept`);
    return invited;
  };
  const carol = await join("developer", "Carol");
  const bob = await join("reviewer", "Bob");
  await call(alice.page, "POST", `/api/orgs/${slug}/databases`, { name: "Shop", slug: "shop" });
  for (const [sql, message] of [
    [V1, "Users"],
    [V2, "Orders"],
  ])
    await call(alice.page, "POST", `/api/orgs/${slug}/databases/shop/versions`, { sql, message });
  return { slug, alice, carol, bob };
}

test("an admin removes a version, says why, and restores it", async ({ browser }) => {
  const { slug, alice } = await team(browser);
  const page = alice.page;

  await page.goto(`./${slug}/shop/v/2`);
  await page.getByRole("button", { name: "More actions on v2" }).click();
  await page.getByRole("menuitem", { name: "Remove version…" }).click();
  const dialog = page.getByRole("dialog", { name: "Remove v2?" });
  await expect(dialog).toContainText("v1 becomes the latest");
  // The reason is required.
  await dialog.getByRole("button", { name: "Remove v2" }).click();
  await expect(dialog).toContainText("Say why");
  await dialog.getByLabel("Reason").fill("Orders were pushed from staging");
  await dialog.getByRole("button", { name: "Remove v2" }).click();

  // Back on the database: v1 is the latest, the history has one version.
  await expect(page).toHaveURL(new RegExp(`/${slug}/shop$`));
  await expect(page.getByText("v2 removed")).toBeVisible();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Shop");
  await expect(page.getByText("1 version", { exact: true })).toBeVisible();
  await page.goto(`./${slug}/shop/history`);
  const versions = page.getByRole("list", { name: "Versions" });
  await expect(versions.getByRole("listitem")).toHaveCount(1);
  await expect(versions).not.toContainText("Orders");
  // Its address says what happened.
  await page.goto(`./${slug}/shop/v/2`);
  await expect(page.getByText("v2 was removed")).toBeVisible();

  // Settings: who removed it and why, in the list and the activity.
  await page.getByRole("link", { name: "Removed versions" }).click();
  const removed = page.getByRole("list", { name: "Removed versions" });
  await expect(removed).toContainText("Orders were pushed from staging");
  await expect(removed).toContainText("Removed by Alice");
  const activity = page.getByRole("list", { name: "Activity" });
  await expect(activity).toContainText("Alice removed v2");

  await removed.getByRole("button", { name: "Restore v2" }).click();
  await expect(page.getByText("v2 restored")).toBeVisible();
  await expect(page.getByText("No removed versions")).toBeVisible();
  await expect(activity).toContainText("Alice restored v2");
  await page.goto(`./${slug}/shop`);
  await expect(page.getByText("2 versions", { exact: true })).toBeVisible();
});

test("a reviewer approves, an edit resets it, and publishing waits for approvals", async ({
  browser,
}) => {
  test.slow();
  const { slug, alice, carol, bob } = await team(browser);
  await call(alice.page, "PATCH", `/api/orgs/${slug}`, { approvals_required: 1 });

  // Carol starts a draft and edits it: she is an editor, she cannot approve.
  await carol.page.goto(`./${slug}/shop`);
  await carol.page.getByRole("button", { name: "New draft" }).first().click();
  await carol.page.waitForURL(/\/drafts\/[0-9a-f-]+$/);
  await bob.page.goto(carol.page.url());
  for (const p of [carol, bob])
    await expect(p.page.getByTestId("compile-status")).toHaveText("Compiled");
  const sql = carol.page.locator(".cm-content");
  await sql.click();
  await carol.page.keyboard.press("ControlOrMeta+End");
  await carol.page.keyboard.type("\nCREATE TABLE coupons (code text PRIMARY KEY);\n", {
    delay: 10,
  });
  await expect(bob.page.locator(".cm-content")).toContainText("CREATE TABLE coupons");
  await expect(carol.page.getByTestId("compile-status")).toHaveText("Compiled");
  await expect(carol.page.getByRole("button", { name: /Approve/ })).toHaveCount(0);

  // Publishing waits: the dialog says so, and the button does nothing.
  await carol.page.getByRole("button", { name: "Publish v3" }).click();
  const publish = carol.page.getByRole("dialog", { name: "Publish v3" });
  await expect(publish.getByTestId("approval-gate")).toContainText("Waiting for 1 more approval");
  await expect(publish.getByRole("button", { name: "Publish v3" })).toBeDisabled();
  await publish.getByRole("button", { name: "Cancel" }).click();

  // Bob, a reviewer, approves: Carol sees it at once.
  await bob.page.getByRole("button", { name: "Approve this version of the draft" }).click();
  await expect(bob.page.getByText("The draft has the approvals it needs")).toBeVisible();
  await expect(carol.page.getByTestId("approvals")).toContainText("1 of 1 approval");

  // Carol edits again: the approval resets, for everyone.
  await sql.click();
  await carol.page.keyboard.press("ControlOrMeta+End");
  await carol.page.keyboard.type("-- reviewed\n", { delay: 10 });
  await expect(bob.page.getByTestId("approvals")).toContainText("0 of 1 approval");
  await expect(carol.page.getByText("An approval was reset")).toBeVisible();

  // Bob approves what is there now; Carol publishes.
  await expect(bob.page.locator(".cm-content")).toContainText("-- reviewed");
  await bob.page.getByRole("button", { name: "Approve this version of the draft" }).click();
  await expect(carol.page.getByTestId("approvals")).toContainText("1 of 1 approval");
  await carol.page.getByRole("button", { name: "Publish v3" }).click();
  await expect(publish.getByTestId("approval-gate")).toContainText("Approved by Bob");
  await publish.getByRole("button", { name: "Publish v3" }).click();
  await expect(carol.page).toHaveURL(new RegExp(`/${slug}/shop$`));
  await expect(carol.page.getByText("3 versions", { exact: true })).toBeVisible();
});
