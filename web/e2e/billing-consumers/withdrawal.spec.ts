import { expect, test } from "@playwright/test";

import { paid, signedInOrg } from "../billing-flows";

// Consumer sales on (this project's API: ROMULUS_CONSUMER_SALES=true, Stripe
// faked): the full edition of the Terms, a buyer chosen at checkout (never
// preselected), and a consumer's withdrawal: one button, one confirmation,
// the organization on Free at once.

test("a consumer subscribes, then withdraws in two clicks and is on Free", async ({ page }) => {
  const slug = await signedInOrg(page, "consumer", { professional: false });
  await page.goto(`./${slug}/settings/billing`);
  await page
    .getByRole("region", { name: "Plans" })
    .getByRole("button", { name: "Upgrade to Starter" })
    .click();
  const checkout = page.getByRole("dialog", { name: "Upgrade to Starter" });
  // Nothing is assumed about the buyer.
  for (const radio of await checkout.getByRole("radio").all()) {
    await expect(radio).not.toBeChecked();
  }
  await expect(checkout.getByRole("link", { name: /^Terms of Service/ })).toHaveAttribute(
    "href",
    /\/legal\/full\/terms$/,
  );
  await checkout.getByRole("checkbox", { name: /I have read and accept the/ }).check();
  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  await expect(
    checkout.getByText("Choose whether you are buying as a business or as an individual."),
  ).toBeVisible();

  await checkout.getByRole("radio", { name: /An individual/ }).click();
  await expect(checkout.getByRole("textbox", { name: "Business name" })).toHaveCount(0);
  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  await expect(checkout.getByRole("alert")).toContainText("tick both boxes");
  await checkout.getByRole("checkbox", { name: /expressly ask for my plan to start now/ }).check();
  await checkout.getByRole("checkbox", { name: /if I withdraw within these 14 days/ }).check();
  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  await paid(page, slug, "Starter");

  // The right of withdrawal, while it runs.
  const plan = page.getByRole("region", { name: "Plan", exact: true });
  await expect(plan.getByRole("heading", { name: "Right of withdrawal" })).toBeVisible();
  await plan.getByRole("button", { name: "Withdraw from contract here" }).click();
  const confirm = page.getByRole("alertdialog", { name: "Withdraw from the Starter plan?" });
  await expect(confirm).toContainText("moves to the Free plan; nothing is deleted");
  // The safe choice has the focus; confirming is one click.
  await expect(confirm.getByRole("button", { name: "Keep the plan" })).toBeFocused();
  await confirm.getByRole("button", { name: "Confirm withdrawal" }).click();

  await expect(page.getByText("Withdrawal received", { exact: true })).toBeVisible();
  await expect(plan.getByText("Free", { exact: true })).toBeVisible();
  await expect(plan).toContainText("Withdrawal received on");
  await expect(plan.getByRole("button", { name: "Withdraw from contract here" })).toHaveCount(0);
  await expect(plan.getByRole("button", { name: "Cancel subscription" })).toHaveCount(0);
});
