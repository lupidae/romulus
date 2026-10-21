import { expect, test } from "@playwright/test";

import { fillBusinessCheckout, paid, signedInOrg } from "../billing-flows";
import {
  acceptTerms,
  createOrg,
  devSignIn,
  emailed,
  emailedLink,
  inviteLink,
  unique,
} from "../helpers";

// The hosted service sold to businesses only (this project's API: billing
// faked, ROMULUS_CONSUMER_SALES off): the Terms and the professional-use
// attestation before anything else, a checkout for businesses only, the
// billing pages in French, and "Cancel your contract" under every page.

test("a new account accepts the Terms and attests professional use before anything else", async ({
  page,
}) => {
  const email = `terms-${Date.now().toString(36)}@romulus.test`;
  await devSignIn(page, email, "/app/new");
  await expect(page).toHaveURL(/\/app\/signin\/terms\?next=%2Fapp%2Fnew$/);
  await expect(page.getByRole("heading", { name: "Before you continue", level: 1 })).toBeVisible();
  await expect(page.getByText("romulus is for professional use only.")).toBeVisible();
  const terms = page.getByRole("link", { name: /^Terms of Service/ });
  await expect(terms).toHaveAttribute("href", /\/legal\/terms$/);
  await expect(terms).toHaveAttribute("target", "_blank");
  await expect(page.getByRole("link", { name: /^Privacy Policy/ })).toHaveAttribute(
    "href",
    /\/legal\/privacy$/,
  );
  for (const box of await page.getByRole("checkbox").all()) await expect(box).not.toBeChecked();

  // Nothing goes through without both boxes.
  await page.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByText("Accept the Terms of Service to continue.")).toBeVisible();
  await expect(
    page.getByText("Confirm that you will use romulus for professional purposes."),
  ).toBeVisible();
  await expect(page).toHaveURL(/\/app\/signin\/terms/);
  // The way out is as plain as the way in.
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();

  // In French, with the French Terms.
  await page.getByRole("radio", { name: "Français" }).click();
  await expect(page.getByRole("heading", { name: "Avant de continuer", level: 1 })).toBeVisible();
  await expect(
    page.getByRole("link", { name: /^Conditions générales d'utilisation et de vente/ }),
  ).toHaveAttribute("href", /\/fr\/legal\/conditions$/);
  await page.getByRole("checkbox", { name: /J'ai lu et j'accepte les/ }).check();
  await page.getByRole("checkbox", { name: /exclusivement à des fins professionnelles/ }).check();
  await page.getByRole("button", { name: "Continuer" }).click();

  // On to where they were going; asked once.
  await expect(page).toHaveURL(/\/app\/new$/);
  await page.reload();
  await expect(page).toHaveURL(/\/app\/new$/);
  const slug = await createOrg(page, unique("Terms"));
  // The language chosen is theirs: the billing page speaks it.
  await page.goto(`./${slug}/settings/billing`);
  await expect(page.getByRole("heading", { name: "Facturation", level: 1 })).toBeVisible();
});

test("checkout is for businesses only, checks every statement, and confirms with the Terms", async ({
  page,
}) => {
  const slug = await signedInOrg(page, "pro");
  await page.goto(`./${slug}/settings/billing`);
  await page
    .getByRole("region", { name: "Plans" })
    .getByRole("button", { name: "Upgrade to Team" })
    .click();
  const checkout = page.getByRole("dialog", { name: "Upgrade to Team" });
  await expect(
    checkout.getByText(
      "For professional use only. On the payment page, add the name of your business and, if it has one, its VAT number.",
    ),
  ).toBeVisible();
  await expect(checkout.getByRole("radio")).toHaveCount(0);
  await expect(checkout.getByRole("checkbox", { name: /start now/ })).toHaveCount(0);
  await expect(checkout).toContainText("excluding taxes");

  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  for (const problem of [
    "Enter your business's name.",
    "Choose your business's country.",
    "Accept the Terms of Service to continue.",
    "Confirm that you are subscribing for your business.",
  ]) {
    await expect(checkout.getByText(problem)).toBeVisible();
  }
  await expect(page).toHaveURL(/settings\/billing$/);

  await fillBusinessCheckout(checkout);
  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  await paid(page, slug, "Team");
  // No withdrawal for a business.
  await expect(page.getByRole("button", { name: "Withdraw from contract here" })).toHaveCount(0);

  // The confirmation, with the Terms as a PDF (the mail catcher).
  const me = (await (await page.request.get("/api/me")).json()) as { email: string };
  const confirmation = await emailed(me.email, /Team plan for .* has started/);
  expect(confirmation.Text).toContain("renews automatically every month");
  expect(confirmation.Attachments.map((a) => a.ContentType)).toEqual(["application/pdf"]);

  // "Cancel your contract" under the page leads to the Cancel button.
  await page.goto(`./${slug}`);
  await page
    .getByRole("navigation", { name: "Legal" })
    .getByRole("link", { name: "Cancel your contract" })
    .click();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing\\?action=cancel$`));
  await expect(page.getByRole("button", { name: "Cancel subscription" })).toBeFocused();
});

test("the billing page and the checkout speak French to a French account", async ({ page }) => {
  await devSignIn(page, `fr-${Date.now().toString(36)}@romulus.test`, "/app/new");
  await page.getByRole("radio", { name: "Français" }).click();
  await page.getByRole("checkbox", { name: /J'ai lu et j'accepte les/ }).check();
  await page.getByRole("checkbox", { name: /exclusivement à des fins professionnelles/ }).check();
  await page.getByRole("button", { name: "Continuer" }).click();
  await expect(page).toHaveURL(/\/app\/new$/);
  const slug = await createOrg(page, unique("Facturation"));
  await page.goto(`./${slug}/settings/billing`);
  await expect(page.getByRole("heading", { name: "Facturation", level: 1 })).toBeVisible();
  await expect(page.getByRole("region", { name: "Offre", exact: true })).toContainText(
    "Rien à payer",
  );
  await expect(page.getByRole("region", { name: "Consommation" })).toContainText(
    "Bases de données",
  );
  const plans = page.getByRole("region", { name: "Offres" });
  await expect(plans).toContainText("Réservé aux professionnels.");
  await plans.getByRole("button", { name: "Passer à l'offre Starter" }).click();
  const checkout = page.getByRole("dialog", { name: "Passer à l'offre Starter" });
  await expect(checkout).toContainText("19 € par mois");
  await expect(
    checkout.getByText(/^Réservé aux professionnels\. Sur la page de paiement/),
  ).toBeVisible();
  await expect(
    checkout.getByRole("textbox", { name: "Nom ou dénomination de l'entreprise" }),
  ).toBeVisible();
  await expect(
    checkout.getByRole("link", { name: /^Conditions générales d'utilisation et de vente/ }),
  ).toHaveAttribute("href", /\/fr\/legal\/conditions$/);
  await checkout.getByRole("button", { name: "Continuer vers le paiement" }).click();
  await expect(
    checkout.getByText("Acceptez les Conditions générales pour continuer."),
  ).toBeVisible();
  // The footer too.
  await page.keyboard.press("Escape");
  await expect(
    page
      .getByRole("navigation", { name: "Informations légales" })
      .getByRole("link", { name: "Résilier votre contrat" }),
  ).toBeVisible();
});

test("an invitee's password sign-up accepts the Terms on its form, then is never asked again", async ({
  browser,
}) => {
  test.setTimeout(120_000);
  const admin = await (await browser.newContext()).newPage();
  await devSignIn(admin, `terms-host-${Date.now().toString(36)}@romulus.test`, "/app/new");
  await acceptTerms(admin);
  const orgName = unique("Sign-up terms");
  const slug = await createOrg(admin, orgName);
  // The Free plan has one member: a paid one invites.
  await admin.goto(`./${slug}/settings/billing`);
  await admin
    .getByRole("region", { name: "Plans" })
    .getByRole("button", { name: "Upgrade to Starter" })
    .click();
  const checkout = admin.getByRole("dialog", { name: "Upgrade to Starter" });
  await fillBusinessCheckout(checkout);
  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  await paid(admin, slug, "Starter");
  await admin.goto(`./${slug}/settings/members`);
  await admin.getByRole("button", { name: "Invite" }).click();
  const email = `terms-invitee-${Date.now().toString(36)}@romulus.test`;
  const link = await inviteLink(admin, email);

  // A French browser reads the boxes, and who romulus is for, in French.
  const french = await (await browser.newContext({ locale: "fr-FR" })).newPage();
  await french.goto(link);
  await french.getByRole("link", { name: "Create your account" }).click();
  await expect(french.getByText("romulus est réservé à un usage professionnel.")).toBeVisible();
  await expect(
    french.getByRole("link", { name: /^Conditions générales d'utilisation et de vente/ }),
  ).toHaveAttribute("href", /\/fr\/legal\/conditions$/);

  // In English: nothing goes without both boxes.
  const invitee = await (await browser.newContext({ locale: "en-US" })).newPage();
  await invitee.goto(link);
  await invitee.getByRole("link", { name: "Create your account" }).click();
  await expect(invitee.getByText("romulus is for professional use only.")).toBeVisible();
  for (const box of await invitee.getByRole("checkbox").all()) await expect(box).not.toBeChecked();
  await invitee.getByLabel("Name").fill("Terry Terms");
  await invitee.getByRole("button", { name: "Email me a link" }).click();
  await expect(invitee.getByText("Accept the Terms of Service to continue.")).toBeVisible();
  await expect(
    invitee.getByText("Confirm that you will use romulus for professional purposes."),
  ).toBeVisible();
  await invitee.getByRole("checkbox", { name: /I have read and accept the/ }).check();
  await invitee.getByRole("checkbox", { name: /only for professional purposes/ }).check();
  await invitee.getByRole("button", { name: "Email me a link" }).click();
  await expect(invitee.getByRole("heading", { name: "Check your email" })).toBeVisible();

  // The account is made from the emailed link, the Terms accepted with it:
  // straight into the organization, no Terms screen.
  await invitee.goto(await emailedLink(email, "welcome"));
  await invitee.getByLabel("Password", { exact: true }).fill("violet tractor mango");
  await invitee.getByRole("button", { name: `Create account and join ${orgName}` }).click();
  await expect(invitee).toHaveURL(new RegExp(`/app/${slug}$`));
  await invitee.reload();
  await expect(invitee).toHaveURL(new RegExp(`/app/${slug}$`));
});
