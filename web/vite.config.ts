/// <reference types="vitest/config" />
import { fileURLToPath, URL } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import { tanstackRouter } from "@tanstack/router-plugin/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv, type Plugin } from "vite";

// Ports come from the repo-root .env (scripts/dev-env), one set per worktree.
// The browser only ever sees one origin, laid out as in production, where the
// API serves the homepage at /, this SPA at /app and itself at /api. Here Vite
// serves /app and proxies /api (REST and websockets) with the Host header
// untouched, so the session cookie is first-party and the API sees the real
// Origin for its CSRF checks.
const BASE = "/app/";

// The homepage is not served in development: / (and /app) go to the app.
const toApp = (url: string | undefined) => url === "/" || url === "/app";
const rootToApp: Plugin = {
  name: "romulus-root-to-app",
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      if (!toApp(req.url)) return next();
      res.writeHead(302, { Location: BASE }).end();
    });
  },
  configurePreviewServer(server) {
    server.middlewares.use((req, res, next) => {
      if (!toApp(req.url)) return next();
      res.writeHead(302, { Location: BASE }).end();
    });
  },
};

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, fileURLToPath(new URL("..", import.meta.url)), "ROMULUS_");
  const port = (name: string, fallback: number) => Number(env[name] ?? fallback);
  const api = { target: `http://127.0.0.1:${port("ROMULUS_API_PORT", 41001)}`, ws: true };
  // The API, and OAuth's discovery documents it serves at the root, where MCP
  // clients look for them (crates/romulus-api/src/oauth/metadata.rs).
  const proxy = {
    "/api": api,
    "/.well-known/oauth-protected-resource": api,
    "/.well-known/oauth-authorization-server": api,
  };

  return {
    base: BASE,
    plugins: [
      tanstackRouter({ target: "react", autoCodeSplitting: true }),
      react(),
      tailwindcss(),
      rootToApp,
    ],
    resolve: {
      alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
    },
    server: {
      host: "localhost",
      port: port("ROMULUS_WEB_PORT", 41000),
      strictPort: true,
      proxy,
    },
    preview: {
      host: "localhost",
      port: port("ROMULUS_PREVIEW_PORT", 41005),
      strictPort: true,
      proxy,
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
