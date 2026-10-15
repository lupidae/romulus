import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { defineConfig, devices } from "@playwright/test";

// Frame timings of the schema viewer on a production build that includes the
// dev routes (VITE_DEV_ROUTES=true, into dist-perf/), served by `vite
// preview` on this worktree's preview port. On demand, headed (GPU frame
// timings mean nothing headless): `npm --prefix web run perf`.
const envFile = fileURLToPath(new URL("../.env", import.meta.url));
if (existsSync(envFile)) process.loadEnvFile(envFile);

const origin = `http://localhost:${process.env.ROMULUS_PREVIEW_PORT ?? "41005"}`;

export default defineConfig({
  testDir: "./e2e/perf",
  workers: 1,
  reporter: "list",
  timeout: 120_000,
  use: { baseURL: `${origin}/app/`, viewport: { width: 1440, height: 900 } },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } },
    },
  ],
  webServer: {
    command: "npm run build:perf && vite preview --outDir dist-perf",
    url: `${origin}/app/`,
    // Never a normal preview left running: it has no dev routes.
    reuseExistingServer: false,
    timeout: 180_000,
  },
});
