import { expect, type Page, test } from "@playwright/test";

import { createOrg, devSignIn, emailedLink, inviteLink, unique } from "./helpers";

// romulus's own email + password sign-in (ROMULUS_AUTH has `password` on the
// e2e stack, its mail going to the mail catcher): an invitee creates an
// account from the emailed link, signs out and in again, forgets the
// password and resets it, then changes it.

async function signOut(page: Page, name: string) {
  await page.getByRole("button", { name: `Account: ${name}` }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page.getByRole("heading", { name: "Sign in to romulus" })).toBeVisible();
}

async function signIn(page: Page, email: string, password: string) {
  // The form, not the dev sign-in below it (local stacks have both).
  const form = page.getByRole("form", { name: "Sign in with your email" });
  await form.getByLabel("Email").fill(email);
  await form.getByLabel("Password", { exact: true }).fill(password);
  await form.getByRole("button", { name: "Sign in", exact: true }).click();
}

test("an invitee signs up with a password, signs in, resets and changes it", async ({
  browser,
}) => {
  // Two emails, five sign-ins: a long walk.
  test.setTimeout(120_000);
  const admin = await (await browser.newContext()).newPage();
  await devSignIn(admin, `pw-host-${Date.now()}@romulus.test`, "/app/new");
  const orgName = unique("Passwords");
  const slug = await createOrg(admin, orgName);
  const email = `paula-${Date.now().toString(36)}@romulus.test`;
  const link = await inviteLink(admin, email);
  // The server sends mail here: the invitee got the link by email too.
  await expect(admin.getByText(`Emailed to ${email}`)).toBeVisible();

  // Signed out, from the invitation: create the account with that address.
  const paula = await (await browser.newContext()).newPage();
  await paula.goto(link);
  await paula.getByRole("link", { name: "Create your account" }).click();
  await expect(paula.getByRole("heading", { name: `Join ${orgName} on romulus` })).toBeVisible();
  await expect(paula.getByLabel("Email")).toHaveValue(email);
  await paula.getByLabel("Name").fill("Paula Password");
  await paula.getByRole("button", { name: "Email me a link" }).click();
  await expect(paula.getByRole("heading", { name: "Check your email" })).toBeVisible();

  // The emailed link proves the address; the password is chosen there.
  await paula.goto(await emailedLink(email, "welcome"));
  await expect(paula.getByRole("heading", { name: `Finish joining ${orgName}` })).toBeVisible();
  // The token left the address bar.
  expect(paula.url()).not.toContain("token=");
  await expect(paula.getByLabel("Name")).toHaveValue("Paula Password");
  await paula.getByLabel("Password", { exact: true }).fill("short");
  await paula.getByRole("button", { name: `Create account and join ${orgName}` }).click();
  await expect(paula.getByText("Use at least 12 characters (5 so far)")).toBeVisible();
  await paula.getByLabel("Password", { exact: true }).fill("violet tractor mango");
  await paula.getByRole("button", { name: `Create account and join ${orgName}` }).click();
  await expect(paula).toHaveURL(new RegExp(`/app/${slug}$`));
  await expect(paula.getByText(`Welcome to ${orgName}`)).toBeVisible();

  // Out, then back in with the password; a wrong one says only that.
  await signOut(paula, "Paula Password");
  await signIn(paula, email, "violet tractor mangos");
  await expect(paula.getByText("Email or password is incorrect")).toBeVisible();
  await signIn(paula, email, "violet tractor mango");
  await expect(paula).toHaveURL(new RegExp(`/app/${slug}$`));

  // Forgotten: a reset link by email, then signed in with the new one.
  await signOut(paula, "Paula Password");
  await paula.getByRole("link", { name: "Forgot password?" }).click();
  await expect(paula.getByRole("heading", { name: "Reset your password" })).toBeVisible();
  await paula.getByLabel("Email").fill(email);
  await paula.getByRole("button", { name: "Send reset link" }).click();
  await expect(paula.getByRole("heading", { name: "Check your email" })).toBeVisible();
  await paula.goto(await emailedLink(email, "reset"));
  await expect(paula.getByRole("heading", { name: "Choose a new password" })).toBeVisible();
  await paula.getByLabel("New password").fill("granite pelican sky");
  await paula.getByRole("button", { name: "Save and sign in" }).click();
  await expect(paula.getByText("Password changed")).toBeVisible();
  await expect(paula).toHaveURL(new RegExp(`/app/${slug}$`));

  // Changed from the account menu: the current one first.
  await paula.getByRole("button", { name: "Account: Paula Password" }).click();
  await paula.getByRole("menuitem", { name: "Change password…" }).click();
  const dialog = paula.getByRole("dialog", { name: "Change password" });
  await dialog.getByLabel("Current password").fill("not my password at all");
  await dialog.getByLabel("New password").fill("orange kayak meadow");
  await dialog.getByRole("button", { name: "Change password" }).click();
  await expect(dialog.getByText("That is not your current password.")).toBeVisible();
  await dialog.getByLabel("Current password").fill("granite pelican sky");
  await dialog.getByRole("button", { name: "Change password" }).click();
  await expect(dialog).toBeHidden();
  await expect(paula.getByText("Other browsers were signed out.")).toBeVisible();
  await signOut(paula, "Paula Password");
  await signIn(paula, email, "orange kayak meadow");
  await expect(paula).toHaveURL(new RegExp(`/app/${slug}$`));
});

test("a used or unknown link says so and offers a new one", async ({ page }) => {
  await page.goto("./signin/reset#token=" + "x".repeat(43));
  await expect(
    page.getByRole("heading", { name: "This link does not work any more" }),
  ).toBeVisible();
  await expect(page.getByRole("link", { name: "Send me a new link" })).toBeVisible();
  await page.goto("./signin/welcome");
  await expect(
    page.getByRole("heading", { name: "This link does not work any more" }),
  ).toBeVisible();
});
