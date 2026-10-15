/// <reference types="vitest/config" />
import { fileURLToPath, URL } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv } from "vite";

// Ports come from the repo-root .env (scripts/dev-env), one set per worktree.
// The browser only ever sees the web origin: /api (REST and websockets) is
// proxied to the API with the Host header untouched, so the session cookie is
// first-party and the API sees the real Origin for its CSRF checks.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, fileURLToPath(new URL("..", import.meta.url)), "ROMULUS_");
  const port = (name: string, fallback: number) => Number(env[name] ?? fallback);
  const api = { target: `http://127.0.0.1:${port("ROMULUS_API_PORT", 41001)}`, ws: true };

  return {
    plugins: [
      tanstackRouter({ target: "react", autoCodeSplitting: true }),
      react(),
      tailwindcss(),
    ],
    resolve: {
      alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
    },
    server: {
      host: "localhost",
      port: port("ROMULUS_WEB_PORT", 41000),
      strictPort: true,
      proxy: { "/api": api },
    },
    preview: {
      host: "localhost",
      port: port("ROMULUS_PREVIEW_PORT", 41005),
      strictPort: true,
      proxy: { "/api": api },
    },
    test: {
      environment: "jsdom",
      globals: true,
      setupFiles: ["./src/test/setup.ts"],
      include: ["src/**/*.test.{ts,tsx}"],
      css: false,
    },
  };
});
