import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { defineConfig, devices } from "@playwright/test";

// This worktree's ports (scripts/dev-env). CI sets the variables directly.
const envFile = fileURLToPath(new URL("../.env", import.meta.url));
if (existsSync(envFile)) process.loadEnvFile(envFile);

const previewPort = process.env.ROMULUS_PREVIEW_PORT ?? "41005";
const e2eApiPort = process.env.ROMULUS_E2E_API_PORT ?? "41006";
const origin = `http://localhost:${previewPort}`;

// The tests drive a production build (vite preview) on its own origin, served
// by its own API whose public URL is that origin. The API's CSRF check (exact
// Origin) and the OIDC redirect (registered by the Zitadel provisioner) then
// agree with what the browser sends, and the dev server and dev API stay free.
const env = (extra: Record<string, string>) => ({
  ...Object.fromEntries(
    Object.entries(process.env).filter((e): e is [string, string] => e[1] !== undefined),
  ),
  ...extra,
});

export default defineConfig({
  testDir: "./e2e",
  // Dev-only screens run against the dev server (playwright.dev.config.ts),
  // frame timings on demand (playwright.perf.config.ts).
  testIgnore: ["dev/**", "perf/**"],
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: `${origin}/app/`,
    trace: "retain-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: [
    {
      command: "cargo run -q -p romulus-api",
      cwd: "..",
      env: env({
        ROMULUS_API_ADDR: `127.0.0.1:${e2eApiPort}`,
        ROMULUS_PUBLIC_URL: origin,
        // One process per sandbox pool: the dev API may hold the default one.
        ROMULUS_SANDBOX_PREFIX: "romulus_e2e",
        RUST_LOG: process.env.RUST_LOG ?? "romulus_api=warn,tower_http=warn",
      }),
      url: `http://127.0.0.1:${e2eApiPort}/api/health`,
      reuseExistingServer: true,
      timeout: 300_000,
      stdout: "ignore",
    },
    {
      command: "npm run build && npm run preview",
      // The preview server proxies /api to this port (vite.config.ts).
      env: env({ ROMULUS_API_PORT: e2eApiPort }),
      url: `${origin}/app/`,
      reuseExistingServer: true,
      timeout: 180_000,
    },
  ],
});
