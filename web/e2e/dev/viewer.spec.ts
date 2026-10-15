import { expect, type Page, test } from "@playwright/test";

// The schema viewer on its dev fixtures (/app/dev/viewer): graph and text in
// step, search, keyboard, diff, layouts, phones and 300 tables.

const tables = (page: Page) => page.locator(".react-flow__node-table");
const graph = (page: Page) => page.getByRole("application", { name: "Schema graph" });
const sql = (page: Page) => page.getByRole("textbox", { name: "Schema as SQL" });
/** The statement highlighted in the text pane for the selected table. */
const highlighted = (page: Page) => page.locator(".cm-statement:visible").first();

async function open(page: Page, query: string) {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`./dev/viewer?${query}`);
  // The first visit after a cold start waits for Vite to bundle the deps.
  await expect(tables(page).first()).toBeVisible({ timeout: 30_000 });
  return errors;
}

test("draws a schema as tables and as SQL", async ({ page }) => {
  const errors = await open(page, "fixture=small");
  await expect(page).toHaveTitle("Schema viewer – romulus");
  await expect(tables(page)).toHaveCount(7);
  await expect(page.locator('.react-flow__node[data-id="public.posts"]')).toContainText(
    "author_id",
  );
  await expect(sql(page)).toContainText("CREATE TABLE public.comments");
  await expect(page.locator(".react-flow__edge")).toHaveCount(9);
  expect(errors).toEqual([]);
});

test("search focuses a table and the SQL jumps to its definition", async ({ page }) => {
  await open(page, "fixture=small");
  await page.getByRole("button", { name: "Find a table" }).click();
  const field = page.getByRole("combobox", { name: "Table or column name" });
  await expect(field).toBeFocused();
  await field.fill("comm");
  await field.press("Enter");
  await expect(page).toHaveURL(/table=public\.comments/);
  await expect(highlighted(page)).toContainText("CREATE TABLE public.comments");
});

test("the keyboard shortcut opens the search", async ({ page }) => {
  await open(page, "fixture=small");
  await graph(page).focus();
  await page.keyboard.press("ControlOrMeta+k");
  await expect(page.getByRole("dialog", { name: "Find a table" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
});

test("selecting a table in the graph shows its statements", async ({ page }) => {
  await open(page, "fixture=small");
  await page.locator('.react-flow__node[data-id="public.users"] [data-table-header]').click();
  await expect(page).toHaveURL(/table=public\.users/);
  await expect(highlighted(page)).toContainText("CREATE TABLE public.users");
  // Clicking the empty canvas clears it.
  await page.locator(".react-flow__pane").click({ position: { x: 10, y: 300 } });
  await expect(page).not.toHaveURL(/table=/);
});

test("the caret in a statement selects its table in the graph", async ({ page }) => {
  await open(page, "fixture=small");
  // CodeMirror renders only the lines in view: the first table is there.
  await sql(page).getByText("CREATE TABLE public.comments").click();
  await expect(page).toHaveURL(/table=public\.comments/);
  await expect(page.locator('[aria-live="polite"]').first()).toContainText("public.comments");
});

test("arrow keys move between tables, Enter zooms, Escape clears", async ({ page }) => {
  await open(page, "fixture=small");
  await graph(page).focus();
  await page.keyboard.press("ArrowRight");
  await expect(page).toHaveURL(/table=public\./);
  const first = new URL(page.url()).searchParams.get("table");
  await page.keyboard.press("ArrowLeft");
  await expect.poll(() => new URL(page.url()).searchParams.get("table")).not.toBe(first);
  await page.keyboard.press("Space");
  await expect(page.getByRole("tooltip")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("tooltip")).toBeHidden();
  await page.keyboard.press("Escape");
  await expect(page).not.toHaveURL(/table=/);
});

test("other formats show the same table", async ({ page }) => {
  await open(page, "fixture=multi-schema&table=shop.orders");
  await page.getByRole("tab", { name: "DBML" }).click();
  await expect(page.getByRole("textbox", { name: "Schema as DBML" })).toBeVisible();
  await expect(highlighted(page)).toContainText('Table "shop"."orders"');
  await page.getByRole("tab", { name: "JSON" }).click();
  await expect(
    page.locator(".cm-statement:visible").filter({ hasText: '"name": "orders"' }),
  ).toHaveCount(1);
});

test("partitions fold under their table", async ({ page }) => {
  await open(page, "fixture=multi-schema");
  // Zoomed out, nodes show only their names: search zooms in on the table.
  await page.getByRole("button", { name: "Find a table" }).click();
  await page.getByRole("combobox", { name: "Table or column name" }).fill("events");
  await page.keyboard.press("Enter");
  const events = page.locator('.react-flow__node[data-id="audit.events"]');
  const toggle = events.getByRole("button", { name: /5 partitions/ });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(events).toContainText("events_2027_h1");
});

test("a diff marks what changed and steps through it", async ({ page }) => {
  await open(page, "fixture=diff");
  await expect(tables(page)).toHaveCount(9);
  await expect(page.locator('.react-flow__node[data-id="public.profiles"]')).toContainText(
    "Removed",
  );
  await expect(page.locator('.react-flow__node[data-id="public.users"]')).toContainText("Changed");
  await page.getByRole("button", { name: "2 added: show the next added table" }).click();
  await expect(page).toHaveURL(/table=public\.(post_reactions|subscriptions)/);
});

test("either pane can be hidden, and the view can go full screen", async ({ page }) => {
  await open(page, "fixture=small");
  await page.getByRole("button", { name: "Layout" }).click();
  await page.getByRole("menuitemradio", { name: "Text only" }).click();
  await expect(graph(page)).toBeHidden();
  await expect(sql(page)).toBeVisible();
  await page.getByRole("button", { name: "Layout" }).click();
  await page.getByRole("menuitemradio", { name: "Graph and text" }).click();
  await expect(graph(page)).toBeVisible();
  await page.getByRole("button", { name: "Layout" }).click();
  await page.getByRole("menuitemcheckbox", { name: "Full screen" }).click();
  await expect(page.getByRole("banner")).not.toBeInViewport();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("banner")).toBeInViewport();
});

test("the split can be resized from the keyboard and is remembered", async ({ page }) => {
  await open(page, "fixture=small");
  const handle = page.getByRole("separator", { name: "Resize the graph and the text" });
  await handle.focus();
  await page.keyboard.press("Home");
  await expect(handle).toHaveAttribute("aria-valuenow", "25");
  await page.reload();
  await expect(
    page.getByRole("separator", { name: "Resize the graph and the text" }),
  ).toHaveAttribute("aria-valuenow", "25");
});

test.describe("on a phone", () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test("graph and SQL are tabs", async ({ page }) => {
    await open(page, "fixture=small&table=public.posts");
    await expect(graph(page)).toBeVisible();
    await expect(sql(page)).toBeHidden();
    await page.getByRole("tab", { name: "SQL" }).first().click();
    await expect(sql(page)).toBeVisible();
    await expect(highlighted(page)).toContainText("CREATE TABLE public.posts");
    await expect(graph(page)).toBeHidden();
  });
});

test("300 tables lay out, render and stay searchable", async ({ page }) => {
  const started = Date.now();
  const errors = await open(page, "fixture=large");
  await expect(tables(page)).toHaveCount(300);
  // Generous: a dev build, cold, next to other tests.
  expect(Date.now() - started).toBeLessThan(30_000);
  await page.keyboard.press("ControlOrMeta+k");
  await page.getByRole("combobox", { name: "Table or column name" }).fill("shipment_events");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/table=logistics\.shipment_events/);
  await expect(highlighted(page)).toContainText("CREATE TABLE logistics.shipment_events");
  expect(errors).toEqual([]);
});

test("an empty schema says so", async ({ page }) => {
  await page.goto("./dev/viewer?fixture=empty");
  await expect(page.getByRole("heading", { name: "No tables yet" })).toBeVisible();
});
