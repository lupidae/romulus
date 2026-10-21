import { createRequire } from "node:module";

import { expect, type Page, test } from "@playwright/test";
import type * as Axe from "axe-core";

import { createOrg, devSignIn, unique } from "./helpers";

const AXE = createRequire(import.meta.url).resolve("axe-core/axe.min.js");

/** What axe finds wrong inside `selector`: one line per rule, with the elements. */
async function violations(page: Page, selector: string): Promise<string[]> {
  if (!(await page.evaluate(() => "axe" in window))) await page.addScriptTag({ path: AXE });
  return page.evaluate(async (selector) => {
    const { axe } = window as unknown as { axe: typeof Axe };
    const { violations } = await axe.run(selector, { resultTypes: ["violations"] });
    return violations.map(
      (v) => `${v.id}: ${v.nodes.map((node) => node.target.join(" ")).join(", ")}`,
    );
  }, selector);
}

test("the API tokens card and its dialog pass axe, the access select named by its label", async ({
  page,
}) => {
  await devSignIn(page, `tokens-${Date.now().toString(36)}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Tokens"));
  await page.goto(`./${slug}`);
  await page.getByRole("button", { name: "New database" }).click();
  const create = page.getByRole("dialog", { name: "New database" });
  await create.getByLabel("Name").fill("Billing");
  await create.getByRole("button", { name: "Create database" }).click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/billing$`));

  await page.goto(`./${slug}/billing/settings`);
  await expect(page.getByRole("button", { name: "New token" })).toBeVisible();
  expect(await violations(page, "#tokens")).toEqual([]);

  await page.getByRole("button", { name: "New token" }).click();
  const dialog = page.getByRole("dialog", { name: "New API token" });
  await dialog.getByLabel("Name").fill("CI");
  await dialog.getByRole("combobox", { name: "Access" }).click();
  await page.getByRole("option", { name: /^Push and read/ }).click();
  await expect(dialog.getByRole("combobox", { name: "Access" })).toHaveText("Push and read");
  expect(await violations(page, '[role="dialog"]')).toEqual([]);

  await dialog.getByRole("button", { name: "Create token" }).click();
  await expect(page.getByRole("textbox", { name: "API token" })).toHaveValue(/^rom_/);
  await page.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("list", { name: "API tokens" }).getByRole("listitem")).toHaveCount(1);
  expect(await violations(page, "#tokens")).toEqual([]);
});
