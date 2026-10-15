import { expect, test } from "@playwright/test";

import { createOrg, devSignIn, inviteLink, ssoSignIn, unique, zitadelUsers } from "./helpers";

// Two people through the real identity provider: Zitadel's login, the OIDC
// callback, sessions, then the invitation round trip and a role change.
test("an admin invites a developer, who joins, and is then made a reviewer", async ({
  browser,
}) => {
  const [alice, bob] = zitadelUsers();
  const orgName = unique("E2E Org");

  const admin = await (await browser.newContext()).newPage();
  await admin.goto("./new");
  await expect(admin).toHaveURL(/\/app\/signin\?next=%2Fapp%2Fnew$/);
  await ssoSignIn(admin, alice);
  await expect(admin).toHaveURL(/\/app\/new$/);
  const slug = await createOrg(admin, orgName);
  const link = await inviteLink(admin, bob.email);
  expect(link).toMatch(/\/app\/invite\/[\w-]{43}$/);

  // Bob opens the link signed out: he is told who invited him, signs in, comes back.
  const invitee = await (await browser.newContext()).newPage();
  await invitee.goto(link);
  await expect(invitee.getByRole("heading", { name: `Join ${orgName} on romulus` })).toBeVisible();
  await expect(invitee.getByText(`invited ${bob.email} to join as a developer`)).toBeVisible();
  await invitee.getByRole("link", { name: "Sign in to accept" }).click();
  await ssoSignIn(invitee, bob);
  await expect(invitee).toHaveURL(/\/app\/invite\//);
  await invitee.getByRole("button", { name: `Join ${orgName}` }).click();
  await expect(invitee).toHaveURL(new RegExp(`/app/${slug}$`));
  await expect(invitee.getByText(`Welcome to ${orgName}`)).toBeVisible();

  // Alice sees Bob and makes him a reviewer.
  await admin.goto(`./${slug}/settings/members`);
  const bobRole = admin.getByRole("combobox", { name: `Role of ${bob.name}` });
  await expect(bobRole).toHaveText(/Developer/);
  await bobRole.click();
  await admin.getByRole("option", { name: /^Reviewer/ }).click();
  await expect(admin.getByText(`${bob.name.split(" ")[0]} is now a reviewer`)).toBeVisible();
  await expect(bobRole).toHaveText(/Reviewer/);

  // Bob now reads only: no role controls, no invitations.
  await invitee.goto(`./${slug}/settings/members`);
  await expect(invitee.getByRole("heading", { name: "Members" })).toBeVisible();
  await expect(invitee.getByRole("combobox")).toHaveCount(0);
  await expect(invitee.getByRole("link", { name: "Invitations" })).toHaveCount(0);
  const answer = await invitee.evaluate(async (path) => {
    const response = await fetch(path, {
      method: "POST",
      headers: { "X-Romulus-Csrf": "1", "content-type": "application/json" },
      body: JSON.stringify({ email: "mallory@romulus.test", role: "admin" }),
    });
    return response.status;
  }, `/api/orgs/${slug}/invitations`);
  expect(answer).toBe(403);
});

test("an invitation opened with another account offers to switch", async ({ browser }) => {
  const admin = await (await browser.newContext()).newPage();
  await devSignIn(admin, `admin-${Date.now()}@romulus.test`, "/app/new");
  const orgName = unique("Switch");
  await createOrg(admin, orgName);
  const link = await inviteLink(admin, "invitee@romulus.test", "Reviewer");

  const someoneElse = await (await browser.newContext()).newPage();
  await devSignIn(someoneElse, `other-${Date.now()}@romulus.test`);
  await someoneElse.goto(link);
  await expect(
    someoneElse
      .getByRole("alert")
      .filter({ hasText: "This invitation is for invitee@romulus.test" }),
  ).toBeVisible();
  await someoneElse.getByRole("button", { name: "Switch account" }).click();
  // Dev stacks offer the dev users; the invitation is where sign-in comes back to.
  await expect(someoneElse).toHaveURL(/\/app\/signin\?next=%2Fapp%2Finvite%2F/);
});

test("the last admin is told why they cannot leave or step down", async ({ page }) => {
  await devSignIn(page, `solo-${Date.now()}@romulus.test`, "/app/new");
  const slug = await createOrg(page, unique("Solo"));
  await page.goto(`./${slug}/settings/members`);

  await page.getByRole("button", { name: /^More actions for / }).click();
  await expect(page.getByRole("menuitem", { name: /^Leave / })).toBeDisabled();
  await expect(page.getByText("You are the only admin.")).toBeVisible();
  await page.keyboard.press("Escape");

  await page.getByRole("combobox", { name: /^Role of / }).click();
  await expect(page.getByRole("option", { name: /^Developer/ })).toHaveAttribute(
    "aria-disabled",
    "true",
  );
  await expect(page.getByText(/needs at least one admin/)).toBeVisible();
});

test("signing out ends the session", async ({ page }) => {
  await devSignIn(page, `leaver-${Date.now()}@romulus.test`, "/app/new");
  await page.getByRole("button", { name: /^Account: / }).click();
  await page.getByRole("menuitem", { name: "Sign out" }).click();
  await expect(page).toHaveURL(/\/app\/signin$/);
  await page.goto("./new");
  await expect(page).toHaveURL(/\/app\/signin\?next=%2Fapp%2Fnew$/);
});
