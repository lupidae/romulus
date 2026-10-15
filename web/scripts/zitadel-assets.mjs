#!/usr/bin/env node
// Renders the romulus logo for Zitadel's hosted login (its label policy):
// the wordmark for each theme and the mark alone as the icon, as PNGs in
// deploy/zitadel/assets, which deploy/zitadel/provision.sh uploads. Run it
// again after a brand change:
//
//   npm --prefix web run zitadel:assets
//
//   npm --prefix web run zitadel:assets -- --check   (in `just check`: fails
//   when brand.json no longer matches the tokens)
//
// Colors are the tokens of src/styles/globals.css for each theme (also
// written to brand.json, the label policy's colors); the mark is the one in
// src/components/brand/logo.tsx. Also copies Inter (the app's font) beside them.
import { copyFile, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { chromium } from "@playwright/test";

const web = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(web, "../deploy/zitadel/assets");
const font = join(
  web,
  "node_modules/@fontsource-variable/inter/files/inter-latin-wght-normal.woff2",
);

const css = await readFile(join(web, "src/styles/globals.css"), "utf8");
const token = (block, name) => {
  const body = css.slice(css.indexOf(block)).split("}")[0];
  const match = body.match(new RegExp(`--${name}:\\s*(#[0-9a-fA-F]{6})`));
  if (!match) throw new Error(`no --${name} in ${block}`);
  return match[1];
};
const themes = {
  light: { block: ":root {" },
  dark: { block: ".dark {" },
};

const mark = (primary, glyph) => `
  <svg viewBox="0 0 32 32" width="28" height="28" aria-hidden="true">
    <rect width="32" height="32" rx="8" fill="${primary}" />
    <path d="M11.5 23V13.75A4.75 4.75 0 0 1 16.25 9H17" fill="none" stroke="${glyph}"
      stroke-width="2.75" stroke-linecap="round" />
    <circle cx="21.5" cy="9" r="2.4" fill="${glyph}" />
  </svg>`;

const fontData = (await readFile(font)).toString("base64");
const page = (body) => `<!doctype html><html><head><style>
  @font-face { font-family: Inter; src: url(data:font/woff2;base64,${fontData}) format("woff2"); font-weight: 100 900; }
  html, body { margin: 0; background: transparent; }
  /* The login fits the logo in a 250px-wide box, scaling by width: the
     image is that wide, with the wordmark centered at the size it should
     appear (a 28px mark, like romulus's own sign-in page). */
  .logo { display: inline-flex; align-items: center; justify-content: center; gap: 9px;
    width: 250px; padding: 6px 0;
    font: 600 22px/1 Inter; letter-spacing: -0.02em; }
  .icon { display: inline-flex; padding: 0; }
</style></head><body>${body}</body></html>`;

// The label policy's colors, read by provision.sh.
const brand = Object.fromEntries(
  Object.entries(themes).map(([theme, { block }]) => [
    theme,
    Object.fromEntries(
      ["primary", "background", "destructive", "foreground"].map((n) => [n, token(block, n)]),
    ),
  ]),
);
const brandJson = `${JSON.stringify(brand, null, 2)}\n`;
if (process.argv.includes("--check")) {
  const committed = await readFile(join(out, "brand.json"), "utf8").catch(() => "");
  if (committed !== brandJson) {
    console.error(
      "zitadel:assets: deploy/zitadel/assets is older than the tokens in globals.css; run `npm --prefix web run zitadel:assets` and commit",
    );
    process.exit(1);
  }
  console.log("zitadel:assets: up to date");
  process.exit(0);
}
await writeFile(join(out, "brand.json"), brandJson);

const browser = await chromium.launch();
const tab = await browser.newPage({ deviceScaleFactor: 4 });
for (const [theme, { block }] of Object.entries(themes)) {
  const [primary, onPrimary, text] = ["primary", "primary-foreground", "foreground"].map((n) =>
    token(block, n),
  );
  await tab.setContent(
    page(
      `<span class="logo">${mark(primary, onPrimary)}<span style="color:${text}">romulus</span></span>`,
    ),
  );
  await tab.evaluate(() => document.fonts.ready);
  await tab
    .locator(".logo")
    .screenshot({ path: join(out, `logo-${theme}.png`), omitBackground: true });
  await tab.setContent(page(`<span class="icon">${mark(primary, onPrimary)}</span>`));
  await tab
    .locator(".icon")
    .screenshot({ path: join(out, `icon-${theme}.png`), omitBackground: true });
  console.log(`${theme}: primary ${primary}, on primary ${onPrimary}, text ${text}`);
}
await browser.close();
await copyFile(font, join(out, "inter.woff2"));
await copyFile(
  join(web, "node_modules/@fontsource-variable/inter/LICENSE"),
  join(out, "inter-LICENSE.txt"),
);
console.log(`wrote ${out}`);
