import { expect, type Locator, type Page } from "@playwright/test";

import { acceptTerms, createOrg, devSignIn, unique } from "./helpers";

// The hosted service's flows the billing projects share (e2e/billing:
// businesses only; e2e/billing-consumers: consumer sales on).

/** A new account, its Terms accepted, and a new organization: its slug. */
export async function signedInOrg(
  page: Page,
  who: string,
  { professional = true }: { professional?: boolean } = {},
) {
  await devSignIn(page, `${who}-${Date.now().toString(36)}@romulus.test`, "/app/new");
  await acceptTerms(page, { professional });
  return createOrg(page, unique("Billing"));
}

/** The checkout of a business: its name and country, every box ticked. */
export async function fillBusinessCheckout(checkout: Locator) {
  await checkout.getByRole("textbox", { name: "Business name" }).fill("Acme SAS");
  await checkout.getByRole("combobox", { name: "Country" }).fill("Fran");
  await checkout.page().getByRole("option", { name: "France" }).click();
  await checkout.getByRole("checkbox", { name: /I have read and accept the/ }).check();
  await checkout.getByRole("checkbox", { name: /trade, business or profession/ }).check();
}

/** Back from the fake checkout: the plan shows. */
export async function paid(page: Page, slug: string, plan: string) {
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing$`));
  await expect(
    page.getByRole("region", { name: /^(Plan|Offre)$/ }).getByText(plan, { exact: true }),
  ).toBeVisible();
}
