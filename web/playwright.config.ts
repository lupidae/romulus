import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { defineConfig, devices } from "@playwright/test";

// This worktree's ports (scripts/dev-env). CI sets the variables directly.
const envFile = fileURLToPath(new URL("../.env", import.meta.url));
if (existsSync(envFile)) process.loadEnvFile(envFile);

const apiPort = process.env.ROMULUS_API_PORT ?? "41001";
const previewPort = process.env.ROMULUS_PREVIEW_PORT ?? "41005";
const baseURL = `http://localhost:${previewPort}/app/`;

/**
 * End-to-end tests run against a production build (vite preview, which
 * proxies /api like the dev server) and a real API on the stack's
 * databases: `just up` first. Servers already running are reused.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: [
    {
      command: "cargo run -q -p romulus-api",
      cwd: "..",
      url: `http://127.0.0.1:${apiPort}/api/health`,
      reuseExistingServer: true,
      timeout: 300_000,
      stdout: "ignore",
    },
    {
      command: "npm run build && npm run preview",
      url: baseURL,
      reuseExistingServer: true,
      timeout: 180_000,
    },
  ],
});
