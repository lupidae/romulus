import { expect, type Page, test } from "@playwright/test";

// The diff view on the blog fixture (/app/dev/diff): the change list, the
// graph and the SQL in step, the keyboard, the filter, phones.

const change = (page: Page, name: string) =>
  page.getByRole("button", { name: new RegExp(`^${name},`) });
const selectedNode = (page: Page, table: string) =>
  page.locator(`.react-flow__node[data-id="public.${table}"] [data-selected]`);

/**
 * Some line of the selected object's statements is on screen in the SQL diff
 * (its scroller is the merge view side by side, the editor's own otherwise).
 */
async function expectStatementInView(page: Page) {
  await expect
    .poll(() =>
      page.evaluate(() =>
        Array.from(document.querySelectorAll<HTMLElement>(".cm-statement")).some((el) => {
          const scroller = el.closest(".cm-mergeView") ?? el.closest(".cm-scroller");
          if (!scroller || !el.offsetParent) return false;
          const r = el.getBoundingClientRect();
          const s = scroller.getBoundingClientRect();
          return r.bottom > s.top && r.top < s.bottom;
        }),
      ),
    )
    .toBe(true);
}

async function open(page: Page, query = "") {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`./dev/diff${query}`);
  // The first visit after a cold start waits for Vite to bundle the deps.
  await expect(page.locator(".react-flow__node-table").first()).toBeVisible({ timeout: 30_000 });
  return errors;
}

test("a change picked in the list is selected in the graph and scrolled to in the SQL", async ({
  page,
}) => {
  const errors = await open(page);
  await expect(page.getByRole("heading", { name: /^Changes/ })).toContainText("7");
  // The union of both sides: the removed table is drawn too.
  await expect(page.locator(".react-flow__node-table")).toHaveCount(9);

  await change(page, "posts").click();
  await expect(selectedNode(page, "posts")).toBeVisible();
  await expectStatementInView(page);
  await expect(page).toHaveURL(/change=table%3Apublic\.posts/);

  await page.keyboard.press("j");
  await expect(change(page, "profiles")).toHaveAttribute("aria-current", "true");
  await expect(selectedNode(page, "profiles")).toBeVisible();
  // j and k work with the graph focused too, which has keys of its own.
  await page.getByRole("application", { name: "Schema graph" }).focus();
  await page.keyboard.press("k");
  await expect(change(page, "posts")).toHaveAttribute("aria-current", "true");

  // The graph's pick selects the change (fit first: users is off screen);
  // an enum has no node, only SQL.
  await page.keyboard.press("0");
  await expect(page.locator('.react-flow__node[data-id="public.users"]')).toBeInViewport();
  await page.waitForTimeout(400);
  await page.locator('.react-flow__node[data-id="public.users"]').click();
  await expect(change(page, "users")).toHaveAttribute("aria-current", "true");
  await change(page, "reaction_kind").click();
  await expect(page.locator(".react-flow__node [data-selected]")).toHaveCount(0);
  await expectStatementInView(page);
  expect(errors).toEqual([]);
});

test("filters by status, and opens on the change in the address", async ({ page }) => {
  const errors = await open(page, "?change=table:public.comments&show=changed");
  await expect(change(page, "comments")).toHaveAttribute("aria-current", "true");
  await expect(selectedNode(page, "comments")).toBeVisible();
  const rows = page.locator("[data-change]");
  await expect(rows).toHaveCount(3);
  await page.getByRole("radio", { name: "Removed: 1" }).click();
  await expect(rows).toHaveText([/profiles/]);
  await expect(page).toHaveURL(/show=removed/);
  expect(errors).toEqual([]);
});

test("the SQL side by side, folded, and unfolded", async ({ page }) => {
  const errors = await open(page, "?change=table:public.users");
  await page.getByRole("button", { name: "Layout" }).click();
  await page.getByRole("menuitemradio", { name: "Text only" }).click();
  await page.getByRole("radio", { name: "Side by side" }).click();
  await expect(page.getByRole("radio", { name: "Side by side" })).toBeChecked();
  await expect(page.getByRole("textbox", { name: "SQL of v1" })).toBeVisible();
  await expect(page.getByRole("textbox", { name: "SQL of v2" })).toBeVisible();
  const folds = page.locator(".cm-collapsedLines");
  await expect(folds.first()).toBeVisible();
  await page.getByRole("button", { name: "Fold unchanged lines" }).click();
  await expect(folds).toHaveCount(0);
  await page.getByRole("radio", { name: "Unified" }).click();
  await expect(page.getByRole("textbox", { name: "SQL of v1" })).toHaveCount(0);
  await expect(page.locator(".cm-deletedChunk").first()).toBeAttached();
  expect(errors).toEqual([]);
});

test("on a phone the three are tabs, and a pick goes on to the graph", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("./dev/diff");
  await expect(change(page, "posts")).toBeVisible({ timeout: 30_000 });
  await change(page, "posts").click();
  await expect(page.getByRole("tab", { name: "Graph" })).toHaveAttribute("aria-selected", "true");
  await expect(selectedNode(page, "posts")).toBeInViewport();
  await page.getByRole("tab", { name: "SQL" }).click();
  await expectStatementInView(page);
  expect(errors).toEqual([]);
});
