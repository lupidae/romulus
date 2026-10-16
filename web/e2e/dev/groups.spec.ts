import { expect, type Locator, type Page, test } from "@playwright/test";

// Groups in the graph of the dev editor (/app/dev/editor, a draft held in
// the tab): made from picked tables and named in place, renamed, recolored,
// moved with their tables, tables dragged in and out, undo. Two people
// seeing a group move is e2e/groups.spec.ts, on the real collab server.

const node = (page: Page, table: string) =>
  page.locator(`.react-flow__node[data-id="public.${table}"]`);
const header = (page: Page, table: string) => node(page, table).locator("[data-table-header]");
const frame = (page: Page, name: string) =>
  page.locator(".react-flow__node-tableGroup [data-group]").filter({ hasText: name });
const graph = (page: Page) => page.getByRole("application", { name: "Schema graph" });

async function open(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("./dev/editor");
  await expect(node(page, "users")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByTestId("compile-status")).toHaveText("Compiled");
  return errors;
}

async function box(locator: Locator) {
  const b = await locator.boundingBox();
  if (!b) throw new Error("not on screen");
  return b;
}

/** Drags with the pointer, in steps (React Flow's drag needs the moves). */
async function drag(
  page: Page,
  from: Locator,
  dx: number,
  dy: number,
  during?: () => Promise<void>,
) {
  const b = await box(from);
  const x = b.x + Math.min(b.width / 2, 60);
  const y = b.y + b.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y + dy, { steps: 12 });
  await during?.();
  await page.mouse.up();
}

/** Shift-clicks the tables, then G: a group named in place, given `name`. */
async function group(page: Page, tables: string[], name: string) {
  for (const t of tables) await header(page, t).click({ modifiers: ["Shift"] });
  await expect(page.getByRole("toolbar", { name: "Picked tables" })).toContainText(
    `${tables.length} tables picked`,
  );
  await page.keyboard.press("g");
  const field = page.getByRole("textbox", { name: /^Rename Group \d+$/ });
  await expect(field).toBeFocused();
  await field.fill(name);
  await field.press("Enter");
  await expect(frame(page, name)).toContainText(`${tables.length} tables`);
}

test("picked tables become a group named in place, renamed and recolored", async ({ page }) => {
  const errors = await open(page);
  await group(page, ["users", "profiles"], "Accounts");
  await expect(page.getByRole("toolbar", { name: "Picked tables" })).toHaveCount(0);

  // Rename: double-click the name.
  await frame(page, "Accounts").getByText("Accounts", { exact: true }).dblclick();
  const field = page.getByRole("textbox", { name: "Rename Accounts" });
  await field.fill("People");
  await field.press("Enter");
  await expect(frame(page, "People")).toBeVisible();

  // Recolor: the group's menu, from a right click on its label.
  await frame(page, "People").getByText("People", { exact: true }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Color" }).click();
  await page.getByRole("menuitemradio", { name: "Green" }).click();
  await expect(frame(page, "People")).toHaveAttribute("data-color", "green");

  // One step back at a time: the color, then the name.
  await graph(page).focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(frame(page, "People")).not.toHaveAttribute("data-color", "green");
  await page.keyboard.press("ControlOrMeta+z");
  await expect(frame(page, "Accounts")).toBeVisible();
  expect(errors).toEqual([]);
});

test("dragging a group by its label moves its tables; one undo puts them back", async ({
  page,
}) => {
  const errors = await open(page);
  await group(page, ["users", "profiles"], "Accounts");
  const users = await box(node(page, "users"));
  const profiles = await box(node(page, "profiles"));
  const posts = await box(node(page, "posts"));

  await drag(page, frame(page, "Accounts").locator("[data-group-handle]"), -60, -120);
  const movedUsers = await box(node(page, "users"));
  const movedProfiles = await box(node(page, "profiles"));
  expect(movedUsers.x - users.x).toBeCloseTo(-60, -1);
  expect(movedUsers.y - users.y).toBeCloseTo(-120, -1);
  expect(movedProfiles.x - profiles.x).toBeCloseTo(movedUsers.x - users.x, 0);
  expect(movedProfiles.y - profiles.y).toBeCloseTo(movedUsers.y - users.y, 0);
  // A table outside the group stays.
  expect(await box(node(page, "posts"))).toEqual(posts);

  await graph(page).focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect.poll(async () => (await box(node(page, "users"))).y).toBeCloseTo(users.y, 0);
  expect((await box(node(page, "profiles"))).x).toBeCloseTo(profiles.x, 0);
  // The group itself was made before: still there.
  await expect(frame(page, "Accounts")).toContainText("2 tables");
  expect(errors).toEqual([]);
});

test("Esc during a group drag puts it back", async ({ page }) => {
  await open(page);
  await group(page, ["users", "profiles"], "Accounts");
  const users = await box(node(page, "users"));
  await drag(page, frame(page, "Accounts").locator("[data-group-handle]"), 80, 80, () =>
    page.keyboard.press("Escape"),
  );
  expect(await box(node(page, "users"))).toEqual(users);
});

test("a table dragged onto a group joins it, and leaves it when dragged out", async ({ page }) => {
  const errors = await open(page);
  await group(page, ["users", "profiles"], "Accounts");
  const accounts = await box(frame(page, "Accounts"));
  const tags = await box(header(page, "tags"));

  // Into the group: the drop says what it will do before it happens.
  const into = {
    dx: accounts.x + accounts.width / 2 - (tags.x + 60),
    dy: accounts.y + accounts.height - 40 - (tags.y + tags.height / 2),
  };
  await drag(page, header(page, "tags"), into.dx, into.dy, async () => {
    await expect(page.getByRole("status").filter({ hasText: "Add to Accounts" })).toBeVisible();
    await expect(frame(page, "Accounts")).toHaveAttribute("data-drop-target", "");
  });
  await expect(frame(page, "Accounts")).toContainText("3 tables");

  // Out again, far below.
  await drag(page, header(page, "tags"), 0, 260, async () => {
    await expect(
      page.getByRole("status").filter({ hasText: "Remove from Accounts" }),
    ).toBeVisible();
  });
  await expect(frame(page, "Accounts")).toContainText("2 tables");

  // Each drop is one step: undo brings it back in, at its place inside.
  await graph(page).focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(frame(page, "Accounts")).toContainText("3 tables");
  expect(errors).toEqual([]);
});

test("a table's menu adds it to a group and takes it out; a deleted group leaves its tables", async ({
  page,
}) => {
  await open(page);
  await group(page, ["users", "profiles"], "Accounts");

  await header(page, "tags").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Add to group" }).click();
  await page.getByRole("menuitem", { name: "Accounts" }).click();
  await expect(frame(page, "Accounts")).toContainText("3 tables");

  await header(page, "tags").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Remove from Accounts" }).click();
  await expect(frame(page, "Accounts")).toContainText("2 tables");

  const users = await box(node(page, "users"));
  await frame(page, "Accounts").getByText("Accounts", { exact: true }).click({ button: "right" });
  await page.getByRole("menuitem", { name: "Delete group" }).click();
  await expect(frame(page, "Accounts")).toHaveCount(0);
  await expect(page.getByText("Deleted Accounts")).toBeVisible();
  expect(await box(node(page, "users"))).toEqual(users);

  await graph(page).focus();
  await page.keyboard.press("ControlOrMeta+z");
  await expect(frame(page, "Accounts")).toContainText("2 tables");
});

test("reviewers see no group tools", async ({ page }) => {
  await page.goto("./dev/editor?role=reviewer");
  await expect(node(page, "users")).toBeVisible({ timeout: 30_000 });
  await expect(page.getByRole("button", { name: "Groups" })).toHaveCount(0);
  await header(page, "users").click({ modifiers: ["Shift"] });
  await expect(page.getByRole("toolbar", { name: "Picked tables" })).toHaveCount(0);
});
