import { expect, test } from "@playwright/test";

import { devSignIn, unique } from "./helpers";

// Without billing (self-hosted, as this project's API runs), the homepage's
// subscribe link is the usual way in: the plan is ignored.
test("without billing, a subscribe link is the usual onboarding", async ({ page }) => {
  await devSignIn(
    page,
    `selfhost-${Date.now().toString(36)}@romulus.test`,
    "/app/signup?plan=team&interval=year",
  );
  await expect(page).toHaveURL(/\/app\/new$/);
  await expect(page.getByText(/subscribe/i)).toHaveCount(0);
  await page.getByLabel("Organization name").fill(unique("Self-hosted"));
  await page.getByRole("button", { name: "Create organization" }).click();
  await expect(page.getByRole("heading", { name: "Invite your team" })).toBeVisible();
});
