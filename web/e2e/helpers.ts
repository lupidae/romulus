import { readFileSync } from "node:fs";

import { expect, type Page } from "@playwright/test";

interface TestUser {
  email: string;
  password: string;
  name: string;
}

/** The users the Zitadel provisioner made (`just up`): alice, then bob. */
export function zitadelUsers(): [TestUser, TestUser] {
  const file = new URL("../../.dev/zitadel/users.json", import.meta.url);
  const users = JSON.parse(readFileSync(file, "utf8")) as TestUser[];
  if (users.length < 2) throw new Error("users.json lists fewer than two users: run `just up`");
  return [users[0], users[1]];
}

/** From romulus's sign-in page, through Zitadel's login (v1), back to romulus. */
export async function ssoSignIn(page: Page, user: TestUser) {
  await page.getByRole("link", { name: "Continue with SSO" }).click();
  await page.locator("#loginName").fill(user.email);
  await page.locator("#submit-button").click();
  await page.locator("#password").fill(user.password);
  await page.locator("#submit-button").click();
  await page.waitForURL((url) => url.pathname.startsWith("/app/"));
}

/**
 * The dev-only sign-in (ROMULUS_DEV_SIGNIN): any address, no identity
 * provider. Lands on `next` (the app may send someone with no organization
 * on to onboarding).
 */
export async function devSignIn(page: Page, email: string, next = "/app/") {
  const query = new URLSearchParams({ email, next });
  await page.goto(`/api/auth/dev-signin?${query.toString()}`);
  await page.waitForURL(
    (url) => url.pathname.startsWith("/app/") && url.pathname !== "/app/signin",
  );
}

/** A name no other run has used, so tests can share the stack's database. */
export function unique(prefix: string) {
  return `${prefix} ${Date.now().toString(36)}${Math.floor(Math.random() * 1e4).toString(36)}`;
}

/** "E2E abc" → "e2e-abc", as the onboarding form derives it. */
export function slugOf(name: string) {
  return name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
}

/** Creates an organization through onboarding and returns its slug. */
export async function createOrg(page: Page, name: string) {
  await page.goto("./new");
  await page.getByLabel("Organization name").fill(name);
  await page.getByRole("button", { name: "Create organization" }).click();
  await expect(page.getByRole("heading", { name: "Invite your team" })).toBeVisible();
  return slugOf(name);
}

/** From onboarding's second step or the invite dialog: the link for `email`. */
export async function inviteLink(
  page: Page,
  email: string,
  role?: "Admin" | "Developer" | "Reviewer",
) {
  await page.getByLabel("Email").fill(email);
  if (role) {
    await page.getByRole("combobox", { name: "Role" }).click();
    await page.getByRole("option", { name: new RegExp(`^${role}`) }).click();
  }
  await page.getByRole("button", { name: "Create invite link" }).click();
  return page.getByRole("textbox", { name: "Invitation link" }).first().inputValue();
}
