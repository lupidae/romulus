import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const publicFile = (name: string) =>
  readFileSync(fileURLToPath(new URL(`../public/${name}`, import.meta.url)), "utf8");

/** Fails the test on any console error or uncaught exception (CSP violations included). */
function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(String(e)));
  return errors;
}

test("the hero says what romulus is and offers both ways in", async ({ page }) => {
  const errors = watchErrors(page);
  await page.goto("/");

  await expect(page).toHaveTitle(/design, version and review your Postgres schema together/i);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(
    "Design, version and review your Postgres schema together.",
  );
  await expect(page.getByRole("banner")).toBeVisible();
  await expect(page.getByRole("main")).toBeVisible();
  await expect(page.getByRole("contentinfo")).toBeVisible();

  const hero = page.locator(".hero");
  await expect(hero.getByRole("link", { name: "Request access" })).toHaveAttribute(
    "href",
    /^mailto:romulus@lupidae\.com\?subject=/,
  );
  await expect(hero.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/app/signin");
  await expect(page.getByRole("img", { name: /romulus draft of a billing schema/ })).toBeVisible();
  expect(errors).toEqual([]);
});

test("every section link lands on a section", async ({ page }) => {
  await page.goto("/");
  const hrefs = await page
    .locator('a[href^="#"]')
    .evaluateAll((links) => links.map((a) => a.getAttribute("href")!));
  expect(hrefs.length).toBeGreaterThan(5);
  for (const href of hrefs) await expect(page.locator(href)).toHaveCount(1);

  await page.getByRole("navigation", { name: "Sections" }).getByRole("link", { name: "CI" }).click();
  await expect(page).toHaveURL(/#ci$/);
  await expect(page.getByRole("heading", { name: "Push the real schema from CI." })).toBeInViewport();
});

test("the push command copies to the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/#ci");
  const button = page.getByRole("button", { name: "Copy the push command" });
  await button.click();
  await expect(button).toContainText("Copied");
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  expect(copied).toContain('remus -u "$DATABASE_URL" -f json');
  expect(copied).toContain('-H "Authorization: Bearer $ROMULUS_TOKEN"');
  expect(copied).toContain('"$ROMULUS_URL/api/orgs/acme/databases/billing/versions"');
  await expect(button).toContainText("Copy", { timeout: 4000 });
});

test("the theme choice applies at once and is shared with the app", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  const html = page.locator("html");
  const theme = page.getByRole("group", { name: "Theme" });
  await expect(theme.getByRole("radio", { name: "System" })).toBeChecked();

  await theme.getByRole("radio", { name: "Dark" }).check();
  await expect(html).toHaveClass(/\bdark\b/);
  // Same key and value as web/src/lib/theme.ts.
  expect(await page.evaluate(() => localStorage.getItem("romulus-theme"))).toBe("dark");
  const background = () => page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  expect(await background()).toBe("rgb(18, 17, 16)");

  await page.reload();
  await expect(html).toHaveClass(/\bdark\b/);
  await expect(theme.getByRole("radio", { name: "Dark" })).toBeChecked();

  await theme.getByRole("radio", { name: "System" }).check();
  await expect(html).not.toHaveClass(/\bdark\b/);
  expect(await page.evaluate(() => localStorage.getItem("romulus-theme"))).toBeNull();
  expect(await background()).toBe("rgb(251, 250, 247)");
});

test("without a choice, the system theme decides", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  expect(await page.evaluate(() => getComputedStyle(document.body).backgroundColor)).toBe(
    "rgb(18, 17, 16)",
  );
});

for (const width of [360, 390, 1024, 1440]) {
  test(`nothing scrolls sideways at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/");
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow).toBe(0);
  });
}

test("the hero scene waits to be seen, then plays to the finished graph", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 700 });
  await page.goto("/");
  const product = page.locator(".product");
  await expect(product).not.toHaveClass(/is-playing/);
  await product.scrollIntoViewIfNeeded();
  await expect(product).toHaveClass(/is-playing/);
  // The column Ada typed ends up on the invoices table.
  await expect(page.locator(".row-new")).toHaveCSS("opacity", "1", { timeout: 10_000 });
});

test("with reduced motion, the scene is already finished", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  await expect(page.locator(".row-new")).toHaveCSS("opacity", "1");
  await expect(page.locator(".typed")).toHaveCSS("clip-path", "none");
});

test("FAQ answers open from the keyboard", async ({ page }) => {
  await page.goto("/#faq");
  const question = page.getByText("Does romulus connect to our production database?");
  await question.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText("romulus never holds a connection string")).toBeVisible();
});

test("an unknown path gets the 404 page", async ({ page }) => {
  const response = await page.goto("/no-such-page");
  expect(response?.status()).toBe(404);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("This page doesn't exist.");
  await expect(page.getByRole("link", { name: "Go to the homepage" })).toHaveAttribute("href", "/");
});

test("the CSP allows exactly the inline theme script", async ({ request }) => {
  const response = await request.get("/");
  const csp = response.headers()["content-security-policy"];
  for (const page of ["index.html", "404.html"]) {
    const scripts = [...publicFile(page).matchAll(/<script>([\s\S]*?)<\/script>/g)];
    expect(scripts).toHaveLength(1);
    const hash = createHash("sha256").update(scripts[0][1]).digest("base64");
    expect(csp, `${page}'s inline script hash is missing from _headers`).toContain(`'sha256-${hash}'`);
  }
});

test("share metadata, icons, robots and sitemap all resolve", async ({ page, request }) => {
  await page.goto("/");
  const meta = (selector: string) => page.locator(selector).getAttribute("content");
  const image = await meta('meta[property="og:image"]');
  expect(image).toBe("https://romulus.lupidae.com/og.png");
  expect(await meta('meta[name="twitter:card"]')).toBe("summary_large_image");
  expect(await page.locator('link[rel="canonical"]').getAttribute("href")).toBe(
    "https://romulus.lupidae.com/",
  );

  const og = await request.get("/og.png");
  expect(og.headers()["content-type"]).toBe("image/png");
  const png = await og.body();
  expect([png.readUInt32BE(16), png.readUInt32BE(20)]).toEqual([1200, 630]);

  const manifest = await (await request.get("/site.webmanifest")).json();
  const icons = await page
    .locator('link[rel~="icon"], link[rel="apple-touch-icon"]')
    .evaluateAll((links) => links.map((l) => l.getAttribute("href")!));
  for (const path of [...icons, ...manifest.icons.map((i: { src: string }) => i.src)]) {
    expect((await request.get(path)).status(), path).toBe(200);
  }
  expect(await (await request.get("/robots.txt")).text()).toContain(
    "Sitemap: https://romulus.lupidae.com/sitemap.xml",
  );
  expect(await (await request.get("/sitemap.xml")).text()).toContain(
    "<loc>https://romulus.lupidae.com/</loc>",
  );
});

for (const colorScheme of ["light", "dark"] as const) {
  test(`axe finds no accessibility violations (${colorScheme})`, async ({ page }) => {
    await page.emulateMedia({ colorScheme, reducedMotion: "reduce" });
    await page.goto("/");
    const results = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze();
    expect(results.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`)).toEqual([]);
  });
}
