#!/usr/bin/env node
// Renders the homepage's images with Playwright's Chromium, from sources in
// this repository, so they never go stale by hand:
//
//   public/og.png                 1200×630 social image (tools/og.html + the hero window)
//   public/favicon.ico            16, 32 and 48 px, from public/favicon.svg
//   public/apple-touch-icon.png   180 px, full bleed (iOS rounds it)
//   public/icon-192.png, icon-512.png, icon-maskable-512.png   for site.webmanifest
//
//   npm --prefix site run assets     (after a change to the hero, the copy or the mark)
import { readFile, writeFile } from "node:fs/promises";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "@playwright/test";

const site = fileURLToPath(new URL("..", import.meta.url));
const pub = join(site, "public");
const origin = "http://assets.local";

const index = await readFile(join(pub, "index.html"), "utf8");
const pick = (re, what) => {
  const m = index.match(re);
  if (!m) throw new Error(`index.html: no ${what}`);
  return m[0];
};
const sprite = pick(/<svg class="sprite"[\s\S]*?<\/svg>\n/, "icon sprite");
const product = pick(/<figure class="product"[\s\S]*?<\/figure>/, "product figure");
const og = (await readFile(join(site, "tools/og.html"), "utf8"))
  .replace("<!-- sprite -->", sprite)
  .replace("<!-- product -->", product);

// The mark, drawn for each icon's shape. Colors are the brand's primary and
// its foreground (tokens.css): an image file cannot read CSS variables.
const markSvg = await readFile(join(pub, "favicon.svg"), "utf8");
const primary = markSvg.match(/fill="(#[0-9a-f]{6})"/i)[1];
const glyph = (scale) => {
  const t = 16 - 16 * scale;
  return `<g transform="translate(${t} ${t}) scale(${scale})"><path d="M11.5 23V13.75A4.75 4.75 0 0 1 16.25 9H17" fill="none" stroke="#fff" stroke-width="2.75" stroke-linecap="round"/><circle cx="21.5" cy="9" r="2.4" fill="#fff"/></g>`;
};
const square = (scale) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" fill="${primary}"/>${glyph(scale)}</svg>`;
const page = (svg, size) =>
  `<!doctype html><style>html,body{margin:0;background:transparent}svg{display:block;width:${size}px;height:${size}px}</style>${svg}`;

const types = { ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml", ".png": "image/png" };

const browser = await chromium.launch();
const context = await browser.newContext({ reducedMotion: "reduce", deviceScaleFactor: 1 });
await context.route(`${origin}/**`, async (route) => {
  const path = new URL(route.request().url()).pathname;
  if (path === "/og.html") return route.fulfill({ body: og, contentType: "text/html" });
  try {
    const body = await readFile(join(pub, path));
    return route.fulfill({ body, contentType: types[extname(path)] ?? "application/octet-stream" });
  } catch {
    return route.fulfill({ status: 404 });
  }
});
const tab = await context.newPage();

async function render(html, size, file, { transparent = true } = {}) {
  await tab.setViewportSize({ width: size, height: size });
  await tab.setContent(html);
  const png = await tab.screenshot({ omitBackground: transparent });
  if (file) await writeFile(join(pub, file), png);
  return png;
}

// Social image.
await tab.setViewportSize({ width: 1200, height: 630 });
await tab.goto(`${origin}/og.html`);
await tab.evaluate(() => document.fonts.ready);
await tab.screenshot({ path: join(pub, "og.png") });

// Icons.
await render(page(square(0.62), 180), 180, "apple-touch-icon.png", { transparent: false });
await render(page(markSvg, 192), 192, "icon-192.png");
await render(page(markSvg, 512), 512, "icon-512.png");
// Maskable: the glyph inside the 80% safe zone of a full-bleed square.
await render(page(square(0.5), 512), 512, "icon-maskable-512.png", { transparent: false });

// favicon.ico: PNG images in an ICO container (supported everywhere ICO is).
const sizes = [16, 32, 48];
const pngs = [];
for (const size of sizes) pngs.push(await render(page(markSvg, size), size));
const header = Buffer.alloc(6 + 16 * sizes.length);
header.writeUInt16LE(0, 0);
header.writeUInt16LE(1, 2);
header.writeUInt16LE(sizes.length, 4);
let offset = header.length;
sizes.forEach((size, i) => {
  const entry = 6 + 16 * i;
  header.writeUInt8(size, entry);
  header.writeUInt8(size, entry + 1);
  header.writeUInt8(0, entry + 2);
  header.writeUInt8(0, entry + 3);
  header.writeUInt16LE(1, entry + 4);
  header.writeUInt16LE(32, entry + 6);
  header.writeUInt32LE(pngs[i].length, entry + 8);
  header.writeUInt32LE(offset, entry + 12);
  offset += pngs[i].length;
});
await writeFile(join(pub, "favicon.ico"), Buffer.concat([header, ...pngs]));

await browser.close();
console.log("assets: wrote og.png, favicon.ico, apple-touch-icon.png, icon-192/512, icon-maskable-512");
