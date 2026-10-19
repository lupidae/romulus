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
  await zitadelSignIn(page, user);
}

/**
 * On Zitadel's login: the address (unless a login hint already filled it and
 * the page went on to the password), the password, back to romulus.
 */
export async function zitadelSignIn(page: Page, user: TestUser) {
  await page.waitForURL((url) => url.pathname.startsWith("/ui/login/"));
  if (await page.locator("#loginName").isVisible()) {
    await page.locator("#loginName").fill(user.email);
    await page.locator("#submit-button").click();
  }
  await page.locator("#password").fill(user.password);
  await page.locator("#submit-button").click();
  await page.waitForURL((url) => url.pathname.startsWith("/app/"));
}

/** Zitadel's origin in this stack (the issuer). */
export function zitadelPort() {
  const port = process.env.ROMULUS_ZITADEL_PORT;
  if (!port) throw new Error("ROMULUS_ZITADEL_PORT is not set: run scripts/dev-env");
  return port;
}

/**
 * The code Zitadel emailed to `email`, from the stack's mail catcher
 * (ROMULUS_MAIL_PORT): sign-up verification, as a person would read it.
 */
export async function emailedCode(email: string): Promise<string> {
  const port = process.env.ROMULUS_MAIL_PORT;
  if (!port) throw new Error("ROMULUS_MAIL_PORT is not set: run scripts/dev-env");
  const api = `http://localhost:${port}/api/v1`;
  // Every message to the address, newest first: romulus emails the
  // invitation there too.
  for (let attempt = 0; attempt < 120; attempt++) {
    const search = (await (
      await fetch(`${api}/search?query=${encodeURIComponent(`to:"${email}"`)}`)
    ).json()) as { messages?: { ID: string }[] };
    for (const { ID } of search.messages ?? []) {
      const message = (await (await fetch(`${api}/message/${ID}`)).json()) as { Text: string };
      const code = /Your code is (\w+)/.exec(message.Text)?.[1];
      if (code) return code;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`no code emailed to ${email}`);
}

/**
 * The one-time link romulus emailed to `email` for `page` (`welcome`: a
 * sign-up, `reset`: a password), the newest, from the stack's mail catcher.
 */
export async function emailedLink(email: string, page: "welcome" | "reset"): Promise<string> {
  const port = process.env.ROMULUS_MAIL_PORT;
  if (!port) throw new Error("ROMULUS_MAIL_PORT is not set: run scripts/dev-env");
  const api = `http://localhost:${port}/api/v1`;
  const pattern = new RegExp(`https?://\\S+/app/signin/${page}#token=[\\w-]{43}`);
  for (let attempt = 0; attempt < 120; attempt++) {
    const search = (await (
      await fetch(`${api}/search?query=${encodeURIComponent(`to:"${email}"`)}`)
    ).json()) as { messages?: { ID: string }[] };
    for (const { ID } of search.messages ?? []) {
      const message = (await (await fetch(`${api}/message/${ID}`)).json()) as { Text: string };
      const link = pattern.exec(message.Text)?.[0];
      if (link) return link;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`no ${page} link emailed to ${email}`);
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
