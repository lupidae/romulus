import { expect, test } from "@playwright/test";

import { createOrg, devSignIn, unique } from "./helpers";

// The sidebar: a rail of icons by default, opened with its button or ⌘\,
// remembered per person in the browser; on a phone, the drawer as before.

test("the sidebar starts as a rail, opens and closes, and is remembered", async ({ page }) => {
  await devSignIn(page, `rail-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Rail"));
  await page.goto(`./${slug}`);

  const sidebar = page.getByRole("complementary", { name: "Primary" });
  const databases = sidebar.getByRole("link", { name: "Databases" });
  await expect(databases).toBeVisible();
  await expect(sidebar).toHaveAttribute("data-collapsed", "");
  // On the rail an item is its icon; the label is the tooltip's.
  expect((await databases.boundingBox())!.width).toBeLessThan(48);
  await databases.hover();
  await expect(page.getByRole("tooltip", { name: "Databases" })).toBeVisible();
  // The organization's settings, named in full where the heading is gone.
  await expect(sidebar.getByRole("link", { name: "Organization settings" })).toBeVisible();

  await sidebar.getByRole("button", { name: "Expand sidebar" }).click();
  await expect(sidebar).not.toHaveAttribute("data-collapsed", "");
  await expect(sidebar.getByRole("link", { name: "General" })).toBeVisible();
  await expect.poll(async () => (await databases.boundingBox())!.width).toBeGreaterThan(150);

  await page.reload();
  await expect(sidebar.getByRole("link", { name: "General" })).toBeVisible();

  await page.keyboard.press("ControlOrMeta+\\");
  await expect(sidebar).toHaveAttribute("data-collapsed", "");
  await page.reload();
  await expect(sidebar).toHaveAttribute("data-collapsed", "");
});

test("on a phone the navigation is a drawer, labelled", async ({ browser }) => {
  const page = await (
    await browser.newContext({ viewport: { width: 390, height: 844 } })
  ).newPage();
  await devSignIn(page, `drawer-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Drawer"));
  await page.goto(`./${slug}`);
  await expect(page.getByRole("complementary", { name: "Primary" })).toBeHidden();
  await page.getByRole("button", { name: "Open navigation" }).click();
  const drawer = page.getByRole("dialog", { name: "Navigation" });
  await expect(drawer.getByRole("link", { name: "General" })).toBeVisible();
  await expect(drawer.getByText("Members", { exact: true })).toBeVisible();
});
