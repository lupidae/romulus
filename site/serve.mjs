#!/usr/bin/env node
// Serves public/ the way Cloudflare Pages will: index.html for directories,
// 404.html with a 404 for anything else missing, the headers in _headers,
// and compressed text, so local Lighthouse runs measure what ships.
//
//   node serve.mjs [port]     (default: $ROMULUS_SITE_PORT, see scripts/dev-env)
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { brotliCompressSync, gzipSync } from "node:zlib";

const root = fileURLToPath(new URL("./public/", import.meta.url));
const port = Number(process.argv[2] ?? process.env.ROMULUS_SITE_PORT);
if (!port) {
  console.error("serve: no port; run `just site` or pass one: node serve.mjs <port>");
  process.exit(1);
}

const types = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json",
  ".webmanifest": "application/manifest+json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".webp": "image/webp",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".txt": "text/plain; charset=utf-8",
  ".xml": "application/xml",
  // The live hero's parser and engine (Pages serves .wasm so, and compresses it).
  ".wasm": "application/wasm",
};
const compressible = new Set([".html", ".css", ".js", ".json", ".webmanifest", ".svg", ".txt", ".xml", ".wasm"]);

// _headers: a URL pattern line (`*` matches anything), then indented
// `Name: value` lines. Every matching block applies, in order.
async function loadHeaders() {
  const rules = [];
  let text = "";
  try {
    text = await readFile(join(root, "_headers"), "utf8");
  } catch {
    return rules;
  }
  for (const line of text.split("\n")) {
    if (!line.trim() || line.trim().startsWith("#")) continue;
    if (!/^\s/.test(line)) {
      const pattern = new RegExp(`^${line.trim().replace(/[.+?^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*")}$`);
      rules.push({ pattern, headers: [] });
    } else if (rules.length) {
      const i = line.indexOf(":");
      rules.at(-1).headers.push([line.slice(0, i).trim(), line.slice(i + 1).trim()]);
    }
  }
  return rules;
}

async function resolve(pathname) {
  const clean = normalize(decodeURIComponent(pathname)).replace(/^([/\\])+/, "");
  const file = join(root, clean);
  if (!file.startsWith(root.replace(new RegExp(`\\${sep}$`), ""))) return null;
  try {
    const info = await stat(file);
    if (info.isDirectory()) return resolve(join(pathname, "index.html"));
    return file;
  } catch {
    // Pages serves /about for about.html.
    if (!extname(file)) {
      try {
        await stat(`${file}.html`);
        return `${file}.html`;
      } catch {}
    }
    return null;
  }
}

createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  const rules = await loadHeaders();
  let file = await resolve(url.pathname);
  let status = 200;
  if (!file) {
    file = join(root, "404.html");
    status = 404;
  }
  let body = await readFile(file);
  const ext = extname(file);
  res.setHeader("Content-Type", types[ext] ?? "application/octet-stream");
  for (const rule of rules) {
    if (rule.pattern.test(url.pathname)) for (const [k, v] of rule.headers) res.setHeader(k, v);
  }
  const accept = req.headers["accept-encoding"] ?? "";
  if (compressible.has(ext)) {
    res.setHeader("Vary", "Accept-Encoding");
    if (/\bbr\b/.test(accept)) {
      body = brotliCompressSync(body);
      res.setHeader("Content-Encoding", "br");
    } else if (/\bgzip\b/.test(accept)) {
      body = gzipSync(body);
      res.setHeader("Content-Encoding", "gzip");
    }
  }
  res.writeHead(status);
  res.end(req.method === "HEAD" ? undefined : body);
}).listen(port, "127.0.0.1", () => {
  console.log(`site: http://localhost:${port}/`);
});
