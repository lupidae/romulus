import { expect, type Page, test } from "@playwright/test";

import { fillBusinessCheckout, paid, signedInOrg } from "../billing-flows";
import { acceptTerms, emailedCode, emailedLink, slugOf, unique } from "../helpers";

// The homepage's plan buttons (site/public/index.html) lead to
// /app/signup?plan=<plan>&interval=<month|year>: the choice follows sign-in or
// sign-up, the Terms and the first organization, to its checkout (the API's
// fake Stripe here, on a businesses-only service).

/** The first organization, made on /new while a plan waits: its slug. */
async function createFirstOrg(page: Page, name: string, next: RegExp) {
  await expect(page).toHaveURL(/\/app\/new\?/);
  await expect(page.getByText(next)).toBeVisible();
  await page.getByLabel("Organization name").fill(name);
  await page.getByRole("button", { name: "Create organization" }).click();
  return slugOf(name);
}

/** Every checkout of the fake Stripe left by its back link, as a buyer cancelling does. */
async function cancelEveryCheckout(page: Page) {
  await page.route("**/api/billing/fake/checkout/**", async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.has("cancel")) return route.continue();
    url.searchParams.set("cancel", "true");
    await route.fulfill({ status: 303, headers: { Location: url.toString() } });
  });
}

test("signed out, a plan goes through single sign-on, the Terms and the first organization to its checkout", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.goto("./signup?plan=team&interval=year");
  await expect(page).toHaveURL(
    /\/app\/signin\?next=%2Fapp%2Fsignup%3Fplan%3Dteam%26interval%3Dyear$/,
  );
  await expect(page.getByText("Then you subscribe to Team, billed yearly.")).toBeVisible();

  // A new account on the identity provider (the OIDC round trip), the
  // address verified with the code it emails.
  const email = `subscriber-${Date.now().toString(36)}@romulus.test`;
  await page.getByRole("link", { name: "Continue with SSO" }).click();
  await expect(page.getByRole("heading", { name: "Sign in to romulus" })).toBeVisible();
  await page.getByRole("button", { name: "Create account" }).click();
  await page.locator("#firstname").fill("Sam");
  await page.locator("#lastname").fill("Subscriber");
  await page.locator("#email").fill(email);
  await page.locator("#register-password").fill("RomulusDev1!");
  await page.locator("#register-password-confirmation").fill("RomulusDev1!");
  await page.getByRole("button", { name: "Create account" }).click();
  await expect(page.getByRole("heading", { name: "Check your email" })).toBeVisible();
  await page.locator("#code").fill(await emailedCode(email));
  await page.getByRole("button", { name: "Verify" }).click();
  await page.getByRole("button", { name: "Continue to romulus" }).click();

  // Back with the plan: the Terms first, then the organization it is for.
  await expect(page).toHaveURL(/\/app\/signin\/terms\?next=%2Fapp%2Fsignup%3Fplan%3Dteam/);
  await acceptTerms(page);
  const slug = await createFirstOrg(
    page,
    unique("Subscribed"),
    /Next, you subscribe it to Team, billed yearly\./,
  );

  // Its billing page opens the checkout: the business, the Terms, then Stripe.
  const checkout = page.getByRole("dialog", { name: "Upgrade to Team" });
  await expect(checkout).toBeVisible();
  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing$`));
  await expect(checkout).toContainText("€490 a year");
  await expect(
    checkout.getByRole("checkbox", { name: /trade, business or profession/ }),
  ).toBeVisible();
  await expect(checkout.getByRole("radio")).toHaveCount(0); // never as a consumer
  await fillBusinessCheckout(checkout);
  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  await paid(page, slug, "Team");
  await expect(page.getByRole("region", { name: "Plan", exact: true })).toContainText(
    "€490 a year",
  );
  // The choice is spent: a reload opens no second checkout.
  await page.reload();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("cancelling the checkout lands on the billing page, Free in force", async ({ page }) => {
  await page.goto("./signup?plan=starter&interval=month");
  await expect(page.getByText("Then you subscribe to Starter, billed monthly.")).toBeVisible();
  // The dev sign-in keeps the sign-in page's `next`, as SSO does.
  const dev = page.getByRole("region", { name: "Dev sign-in, local stacks only" });
  await dev
    .getByRole("textbox", { name: "Sign in as any email" })
    .fill(`cancel-${Date.now().toString(36)}@romulus.test`);
  await dev.getByRole("button", { name: "Sign in" }).click();
  await acceptTerms(page);
  const slug = await createFirstOrg(page, unique("Cancelled"), /Next, you subscribe it to Starter/);

  const checkout = page.getByRole("dialog", { name: "Upgrade to Starter" });
  await expect(checkout).toContainText("€19 a month");
  await fillBusinessCheckout(checkout);
  await cancelEveryCheckout(page);
  await checkout.getByRole("button", { name: "Continue to payment" }).click();

  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing$`));
  await expect(page.getByText("Checkout cancelled")).toBeVisible();
  await expect(page.getByText("Nothing was charged.")).toBeVisible();
  await expect(page.getByRole("alert")).toHaveCount(0);
  const plan = page.getByRole("region", { name: "Plan", exact: true });
  await expect(plan.getByText("Free", { exact: true })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  // Subscribing again is still one click away.
  await expect(
    page.getByRole("region", { name: "Plans" }).getByRole("button", { name: "Upgrade to Starter" }),
  ).toBeVisible();
});

test("signed in with organizations: choose the one, then its checkout", async ({ page }) => {
  const slug = await signedInOrg(page, "chooser");
  await page.goto("./signup?plan=business&interval=month");
  await expect(
    page.getByRole("heading", { name: "Subscribe to Business", level: 1 }),
  ).toBeVisible();
  await expect(page.getByText("€149 a month")).toBeVisible();
  const choices = page.getByRole("navigation", { name: "Your organizations" });
  await expect(choices.getByRole("link", { name: /On Free/ })).toHaveCount(1);
  await expect(choices.getByRole("link", { name: "Create a new organization" })).toHaveAttribute(
    "href",
    "/app/new?plan=business&interval=month",
  );
  await choices.getByRole("link", { name: /On Free/ }).click();

  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing$`));
  const checkout = page.getByRole("dialog", { name: "Upgrade to Business" });
  await expect(checkout).toContainText("€149 a month");
  // Closing it leaves the page as it was, the period chosen shown.
  await checkout.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(
    page.getByRole("region", { name: "Plans" }).getByRole("radio", { name: "Monthly" }),
  ).toHaveAttribute("aria-checked", "true");
});

test("the founding offer's link: Team at its price while subscribed, applied at checkout", async ({
  page,
}) => {
  // The fake's 20 places last as long as its API: a reused one may have none.
  const count = (await (await page.request.get("/api/billing/offers/founding")).json()) as {
    left: number;
  };
  test.skip(count.left === 0, "the e2e API's founding places are used up: restart it");

  const slug = await signedInOrg(page, "founder");
  await page.goto("./signup?plan=team&interval=month&offer=founding");
  await expect(page.getByRole("heading", { name: "Subscribe to Team", level: 1 })).toBeVisible();
  await expect(page.getByText("€29 a month")).toBeVisible();
  const choices = page.getByRole("navigation", { name: "Your organizations" });
  await expect(choices.getByRole("link", { name: "Create a new organization" })).toHaveAttribute(
    "href",
    "/app/new?plan=team&interval=month&offer=founding",
  );
  await choices.getByRole("link", { name: /On Free/ }).click();

  await expect(page).toHaveURL(new RegExp(`/app/${slug}/settings/billing$`));
  const checkout = page.getByRole("dialog", { name: "Upgrade to Team" });
  await expect(checkout).toContainText("€29 a month");
  await expect(checkout).toContainText(
    "Founding team offer: €29 instead of €49, for as long as the subscription stays on this plan.",
  );
  await fillBusinessCheckout(checkout);
  await checkout.getByRole("button", { name: "Continue to payment" }).click();
  await paid(page, slug, "Team");
  await expect(page.getByRole("region", { name: "Plan", exact: true })).toContainText(
    "€29 a month",
  );
});

test("the founding offer is Team's only: another plan's link drops it", async ({ page }) => {
  await signedInOrg(page, "not-founder");
  await page.goto("./signup?plan=starter&interval=month&offer=founding");
  await expect(page.getByText("€19 a month")).toBeVisible();
  // What is carried on (here to a new organization) has no offer.
  await expect(
    page
      .getByRole("navigation", { name: "Your organizations" })
      .getByRole("link", { name: "Create a new organization" }),
  ).toHaveAttribute("href", "/app/new?plan=starter&interval=month");
});

test("an unknown plan is the usual way in", async ({ page }) => {
  for (const query of ["plan=gold&interval=year", "plan=free", ""]) {
    await page.goto(`./signup?${query}`);
    await expect(page).toHaveURL(/\/app\/signin$/);
    await expect(page.getByText(/Then you subscribe/)).toHaveCount(0);
  }
  const slug = await signedInOrg(page, "unknown-plan");
  await page.goto("./signup?plan=enterprise");
  await expect(page).toHaveURL(new RegExp(`/app/${slug}$`));
});

test("a password sign-up's emailed link keeps the plan", async ({ browser }) => {
  test.setTimeout(90_000);
  const page = await (await browser.newContext({ locale: "en-US" })).newPage();
  await page.goto("./signup?plan=starter&interval=year");
  await page.getByRole("link", { name: "Create an account" }).click();
  await expect(page).toHaveURL(/\/app\/signin\/new\?next=/);
  await expect(page.getByText("Then you subscribe to Starter, billed yearly.")).toBeVisible();
  const email = `password-subscriber-${Date.now().toString(36)}@romulus.test`;
  await page.getByLabel("Name").fill("Pat Password");
  await page.getByLabel("Email").fill(email);
  await page.getByRole("checkbox", { name: /I have read and accept the/ }).check();
  await page.getByRole("checkbox", { name: /only for professional purposes/ }).check();
  await page.getByRole("button", { name: "Email me a link" }).click();
  await expect(page.getByRole("heading", { name: "Check your email" })).toBeVisible();

  await page.goto(await emailedLink(email, "welcome"));
  await page.getByLabel("Password", { exact: true }).fill("violet tractor mango");
  await page.getByRole("button", { name: "Create account" }).click();
  // The Terms were accepted on the form: straight on to the organization.
  await createFirstOrg(
    page,
    unique("Password"),
    /Next, you subscribe it to Starter, billed yearly\./,
  );
  await expect(page.getByRole("dialog", { name: "Upgrade to Starter" })).toContainText(
    "€190 a year",
  );
});
