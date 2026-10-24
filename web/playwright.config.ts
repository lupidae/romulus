import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { defineConfig, devices } from "@playwright/test";

// This worktree's ports (scripts/dev-env). CI sets the variables directly.
const envFile = fileURLToPath(new URL("../.env", import.meta.url));
if (existsSync(envFile)) process.loadEnvFile(envFile);

const previewPort = process.env.ROMULUS_PREVIEW_PORT ?? "41005";
const e2eApiPort = process.env.ROMULUS_E2E_API_PORT ?? "41006";
const origin = `http://localhost:${previewPort}`;
// e2e/billing: hosted billing on, Stripe faked server-side (ROMULUS_BILLING=fake),
// on an API of its own that serves the production build itself. Every other
// spec runs with billing off (no caps), as self-hosted.
const billingPort = process.env.ROMULUS_E2E_BILLING_PORT ?? "40002";
const billingOrigin = `http://localhost:${billingPort}`;
// e2e/billing-consumers: the same with consumer sales on
// (ROMULUS_CONSUMER_SALES=true: the business-or-consumer checkout, withdrawal).
// e2e/billing runs businesses only, as the hosted service launches.
const consumersPort = process.env.ROMULUS_E2E_CONSUMERS_PORT ?? "40003";
const consumersOrigin = `http://localhost:${consumersPort}`;
// Absolute globs: Playwright matches them against absolute paths, and a
// relative "billing/**" would also match a checkout whose directory is
// named billing.
const specs = fileURLToPath(new URL("./e2e", import.meta.url));

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

// The playground runner and service cost memory (and start playgrounds):
// started only for a run that includes the playground spec, or every spec.
// ROMULUS_E2E_PLAYGROUNDS=1 or 0 decides instead.
const command = process.argv.indexOf("test");
const named =
  command < 0
    ? []
    : process.argv
        .slice(command + 1)
        .filter((arg) => !arg.startsWith("-") && /\.spec(\.ts)?$|\.ts$|\//.test(arg));
const playgrounds =
  process.env.ROMULUS_E2E_PLAYGROUNDS !== undefined
    ? process.env.ROMULUS_E2E_PLAYGROUNDS === "1"
    : named.length === 0 || named.some((arg) => arg.includes("playground"));
// The billing API: for a run that includes e2e/billing (or every spec).
// ROMULUS_E2E_BILLING=1 or 0 decides instead.
const billing =
  process.env.ROMULUS_E2E_BILLING !== undefined
    ? process.env.ROMULUS_E2E_BILLING === "1"
    : named.length === 0 || named.some((arg) => /billing(\/|$|\.)/.test(arg));
// The consumer-sales API: for a run that includes e2e/billing-consumers.
const consumers =
  process.env.ROMULUS_E2E_BILLING !== undefined
    ? process.env.ROMULUS_E2E_BILLING === "1"
    : named.length === 0 || named.some((arg) => arg.includes("billing-consumers"));

/** A billing API (Stripe faked) serving the production build on `port`. */
function billingServer(port: string, origin: string, extra: Record<string, string>) {
  return {
    // After the preview server (webServers start in order): its command
    // makes the production build (web/dist), served here with /api.
    command:
      "cargo build -q -p romulus-api && until [ -f web/dist/index.html ]; do sleep 1; done && exec target/debug/romulus-api",
    cwd: "..",
    env: env({
      ROMULUS_API_ADDR: `127.0.0.1:${port}`,
      ROMULUS_PUBLIC_URL: origin,
      ROMULUS_WEB_DIR: "web/dist",
      ROMULUS_BILLING: "fake",
      ROMULUS_DEV_SIGNIN: "true",
      // The hosted Terms, version 1.0 (its PDFs are embedded), on this origin.
      ROMULUS_LEGAL_URL: origin,
      ROMULUS_TERMS_VERSION: "1.0",
      ROMULUS_LEGAL_CONTACT: "legal@romulus.test",
      // No playground host: hours and sizes are the API's, not the host's.
      ROMULUS_PLAY_URL: "",
      ROMULUS_PLAY_KEY: "",
      RUST_LOG: process.env.RUST_LOG ?? "romulus_api=warn,tower_http=warn",
      ...extra,
    }),
    url: `http://127.0.0.1:${port}/api/health`,
    reuseExistingServer: true,
    timeout: 300_000,
    stdout: "ignore" as const,
  };
}

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
  projects: [
    {
      name: "chromium",
      testIgnore: [
        `${specs}/dev/**`,
        `${specs}/perf/**`,
        `${specs}/billing/**`,
        `${specs}/billing-consumers/**`,
      ],
      use: { ...devices["Desktop Chrome"] },
    },
    {
      name: "billing",
      testMatch: `${specs}/billing/**/*.spec.ts`,
      use: { ...devices["Desktop Chrome"], baseURL: `${billingOrigin}/app/` },
    },
    {
      name: "billing-consumers",
      testMatch: `${specs}/billing-consumers/**/*.spec.ts`,
      use: { ...devices["Desktop Chrome"], baseURL: `${consumersOrigin}/app/` },
    },
  ],
  webServer: [
    ...(playgrounds
      ? [
          {
            // The playground runner and service (playground.spec.ts), on this
            // worktree's ports and volume (.env), or the ones `just play` runs.
            command:
              "cargo build -q -p romulus-play-runner -p romulus-playground && (target/debug/romulus-play-runner & exec target/debug/romulus-playground)",
            cwd: "..",
            env: env({ RUST_LOG: process.env.RUST_LOG ?? "warn" }),
            url: `http://127.0.0.1:${process.env.ROMULUS_PLAY_PORT ?? "41009"}/health`,
            reuseExistingServer: true,
            timeout: 300_000,
            stdout: "ignore" as const,
          },
        ]
      : []),
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
    ...(billing
      ? [
          billingServer(billingPort, billingOrigin, {
            ROMULUS_SANDBOX_PREFIX: "romulus_e2e_billing",
            // Anyone may create a password account (e2e/billing/subscribe:
            // a plan chosen before signing up follows the emailed link).
            ROMULUS_SIGNUP: "open",
            // The founding-team offer on (the fake makes these codes).
            ROMULUS_FOUNDING_PROMO_MONTHLY: "promo_e2e_founding_monthly",
            ROMULUS_FOUNDING_PROMO_YEARLY: "promo_e2e_founding_yearly",
          }),
        ]
      : []),
    ...(consumers
      ? [
          billingServer(consumersPort, consumersOrigin, {
            ROMULUS_SANDBOX_PREFIX: "romulus_e2e_consumers",
            // Consumer sales open only with the full edition and a version
            // of their own (1.0 was for businesses): this one has no PDF.
            ROMULUS_CONSUMER_SALES: "true",
            ROMULUS_TERMS_EDITION: "full",
            ROMULUS_TERMS_VERSION: "2.0-test",
          }),
        ]
      : []),
  ],
});
