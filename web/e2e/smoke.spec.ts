import { expect, test } from "@playwright/test";

test("a signed-out visitor lands on sign-in, with SSO ready", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));

  await page.goto("./");

  await expect(page).toHaveURL(/\/app\/signin$/);
  await expect(page).toHaveTitle("Sign in – romulus");
  await expect(page.getByRole("heading", { name: "Sign in to romulus" })).toBeVisible();
  // Enabled only once /api/health answered through the same origin.
  const sso = page.getByRole("link", { name: "Continue with SSO" });
  await expect(sso).toHaveAttribute("href", "/api/auth/login");
  await expect(page.getByText("romulus is not answering")).toBeHidden();
  expect(errors).toEqual([]);
});

test("an unknown page offers a way back", async ({ page }) => {
  await page.goto("./no/such/page");

  await expect(page.getByRole("heading", { name: "Page not found" })).toBeVisible();
  await page.getByRole("link", { name: "Go to romulus" }).click();
  await expect(page).toHaveURL(/\/app\/signin$/);
});

test("dev-only routes do not exist in a production build", async ({ page }) => {
  await page.goto("./dev/kit");

  await expect(page.getByRole("heading", { name: "Page not found" })).toBeVisible();
});
