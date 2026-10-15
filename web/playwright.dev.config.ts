import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { defineConfig, devices } from "@playwright/test";

// Dev-only screens (/app/dev/*: the kit, the shell, the schema viewer on
// fixtures) exist in dev builds only, so these tests drive the Vite dev
// server. They need neither the API nor the stack: `npm run e2e:dev`.
const envFile = fileURLToPath(new URL("../.env", import.meta.url));
if (existsSync(envFile)) process.loadEnvFile(envFile);

const origin = `http://localhost:${process.env.ROMULUS_WEB_PORT ?? "41000"}`;

export default defineConfig({
  testDir: "./e2e/dev",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : "list",
  use: { baseURL: `${origin}/app/`, trace: "retain-on-failure" },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: "npm run dev",
    url: `${origin}/app/`,
    reuseExistingServer: true,
    timeout: 120_000,
  },
});
