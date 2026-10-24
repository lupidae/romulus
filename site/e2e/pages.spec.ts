import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

// The comparisons and the guides (public/compare/, public/guides/), the
// homepage's sentence for an agent, and the welcome for launch visitors.
const ORIGIN = "https://romulus.lupidae.com";
const publicFile = (name: string) =>
  readFileSync(fileURLToPath(new URL(`../public/${name}`, import.meta.url)), "utf8");

const pages = [
  { path: "/compare/atlas", h1: "romulus vs Atlas", rival: "atlasgo.io", stronger: "Atlas" },
  { path: "/compare/bytebase", h1: "romulus vs Bytebase", rival: "bytebase.com", stronger: "Bytebase" },
  { path: "/guides/test-prisma-migrations", h1: "How to test Prisma migrations before they reach production", rival: "prisma.io" },
  { path: "/guides/postgres-schema-diff", h1: "How to diff two Postgres schemas", rival: null },
] as const;

const AGENT_SENTENCE =
  "Add the romulus MCP server at https://romulus.lupidae.com/api/mcp (HTTP transport; I'll sign in through my browser), then help me import my schema.";

/** Fails the test on any console error or uncaught exception (CSP violations included). */
function watchErrors(page: Page) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(String(e)));
  return errors;
}

/** The PNG's width and height, from its IHDR chunk. */
const pngSize = (png: Buffer) => [png.readUInt32BE(16), png.readUInt32BE(20)];

for (const article of pages) {
  test(`${article.path}: answers first, with the homepage's header and footer`, async ({ page, request }) => {
    const errors = watchErrors(page);
    const response = await page.goto(article.path);
    expect(response?.status()).toBe(200);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(article.h1);
    // The direct answer is the first paragraph after the title, and says something.
    const answer = page.locator("main h1 + p");
    await expect(answer).toHaveClass(/\banswer\b/);
    expect((await answer.innerText()).length).toBeGreaterThan(200);

    // The same chrome as the homepage: its sections, its footer, its legal links.
    await page.goto("/");
    const home = {
      sections: await page.getByRole("navigation", { name: "Sections" }).getByRole("link").allInnerTexts(),
      footer: await page.getByRole("navigation", { name: "Footer" }).getByRole("link").allInnerTexts(),
      more: await page.getByRole("navigation", { name: "Comparisons and guides" }).getByRole("link").allInnerTexts(),
      legal: await page.getByRole("navigation", { name: "Legal" }).getByRole("link").allInnerTexts(),
    };
    await page.goto(article.path);
    expect(await page.getByRole("navigation", { name: "Sections" }).getByRole("link").allInnerTexts()).toEqual(home.sections);
    for (const link of await page.getByRole("navigation", { name: "Sections" }).getByRole("link").all())
      expect(await link.getAttribute("href")).toMatch(/^\/#[a-z-]+$/);
    expect(await page.getByRole("navigation", { name: "Footer" }).getByRole("link").allInnerTexts()).toEqual(home.footer);
    const more = page.getByRole("navigation", { name: "Comparisons and guides" });
    expect(await more.getByRole("link").allInnerTexts()).toEqual(home.more);
    await expect(more.locator('[aria-current="page"]')).toHaveAttribute("href", article.path);
    expect(await page.getByRole("navigation", { name: "Legal" }).getByRole("link").allInnerTexts()).toEqual(home.legal);

    // One call to action: sign up, or self-host.
    const cta = page.getByRole("region", { name: /./ }).filter({ has: page.locator("#cta-title") });
    await expect(cta.getByRole("link", { name: "Start free" })).toHaveAttribute("href", "/app/signin");
    await expect(cta.getByRole("link", { name: "Self-host it" })).toHaveAttribute(
      "href",
      "https://github.com/lupidae/romulus/tree/main/docs/self-host",
    );

    // Its original element: a picture of romulus or a run's output, there and loaded.
    const made = page.locator(".shot img, .terminal-body.output");
    expect(await made.count()).toBeGreaterThan(0);
    for (const img of await page.locator(".shot img").all()) {
      expect((await img.getAttribute("alt"))?.length).toBeGreaterThan(20);
      // Lazy: it loads once near the screen.
      await img.scrollIntoViewIfNeeded();
      await expect.poll(() => img.evaluate((el: HTMLImageElement) => el.complete && el.naturalWidth > 0)).toBe(true);
      expect((await request.get((await img.getAttribute("src"))!)).status()).toBe(200);
    }

    // Sources: the pages it read, linked.
    const sources = page.locator("#sources + p + ol a");
    expect(await sources.count()).toBeGreaterThan(2);
    if (article.rival)
      expect(await sources.evaluateAll((links) => links.map((a) => (a as HTMLAnchorElement).href)).then((h) => h.some((u) => u.includes(article.rival!)))).toBe(true);
    expect(errors).toEqual([]);
  });

  test(`${article.path}: title, description, canonical and a large share card`, async ({ page, request }) => {
    await page.goto(article.path);
    const meta = (selector: string) => page.locator(selector).getAttribute("content");
    const title = await page.title();
    expect(title.length).toBeGreaterThan(20);
    expect(title.length).toBeLessThanOrEqual(65);
    const description = (await meta('meta[name="description"]'))!;
    expect(description.length).toBeGreaterThan(80);
    expect(description.length).toBeLessThanOrEqual(170);
    expect(await page.locator('link[rel="canonical"]').getAttribute("href")).toBe(`${ORIGIN}${article.path}`);
    expect(await meta('meta[property="og:url"]')).toBe(`${ORIGIN}${article.path}`);
    expect(await meta('meta[property="og:title"]')).toBe(title);
    expect(await meta('meta[name="twitter:card"]')).toBe("summary_large_image");
    const image = (await meta('meta[property="og:image"]'))!;
    expect(image).toMatch(new RegExp(`^${ORIGIN}/[a-z0-9/-]+\\.png$`));
    expect(await meta('meta[name="twitter:image"]')).toBe(image);
    expect(await meta('meta[property="og:image:width"]')).toBe("1200");
    expect(await meta('meta[property="og:image:height"]')).toBe("630");
    expect((await meta('meta[property="og:image:alt"]'))?.length).toBeGreaterThan(20);
    const png = await request.get(image.slice(ORIGIN.length));
    expect(png.headers()["content-type"]).toBe("image/png");
    expect(pngSize(await png.body())).toEqual([1200, 630]);
    // Structured data that parses, dated.
    const ld = JSON.parse((await page.locator('script[type="application/ld+json"]').textContent())!);
    expect(ld.url).toBe(`${ORIGIN}${article.path}`);
    expect(ld.dateModified).toMatch(/^\d{4}-\d{2}-\d{2}$/);
  });

  test(`${article.path}: in the sitemap and the homepage's footer`, async ({ page, request }) => {
    expect(await (await request.get("/sitemap.xml")).text()).toContain(`<loc>${ORIGIN}${article.path}</loc>`);
    await page.goto("/");
    const link = page.getByRole("navigation", { name: "Comparisons and guides" }).locator(`a[href="${article.path}"]`);
    await expect(link).toHaveCount(1);
    await link.click();
    await expect(page).toHaveURL(new RegExp(`${article.path}$`));
  });

  for (const colorScheme of ["light", "dark"] as const) {
    test(`${article.path}: axe finds no accessibility violations (${colorScheme})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme });
      await page.goto(article.path);
      const results = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze();
      expect(results.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`)).toEqual([]);
    });
  }

  for (const width of [360, 390, 1280]) {
    test(`${article.path}: nothing scrolls sideways at ${width}px`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(article.path);
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      expect(overflow).toBe(0);
    });
  }
}

test("the comparisons are dated and say where the other tool is stronger", async ({ page }) => {
  for (const article of pages.filter((p) => "stronger" in p)) {
    await page.goto(article.path);
    await expect(page.locator(".article-meta time")).toHaveAttribute("datetime", /^\d{4}-\d{2}-\d{2}$/);
    const table = page.getByRole("table");
    await expect(table.getByRole("columnheader")).toHaveText(["What", "romulus", article.stronger!, "Stronger"]);
    // Honest: the rival wins rows, said in words, and has its own section.
    const theirs = table.locator(".edge.them");
    expect(await theirs.count()).toBeGreaterThanOrEqual(3);
    for (const word of await theirs.allInnerTexts()) expect(word).toContain(article.stronger!);
    await expect(
      page.getByRole("heading", { level: 2, name: `Where ${article.stronger} is the better choice` }),
    ).toBeVisible();
    // The table scrolls in its frame on a phone, reachable by keyboard.
    await expect(page.getByRole("region", { name: "Side by side" })).toHaveAttribute("tabindex", "0");
  }
});

test("each page's inline theme script is the homepage's, allowed by the CSP", async ({ request }) => {
  const csp = (await request.get("/")).headers()["content-security-policy"];
  for (const article of pages) {
    const scripts = [...publicFile(`${article.path.slice(1)}.html`).matchAll(/<script>([\s\S]*?)<\/script>/g)];
    expect(scripts).toHaveLength(1);
    const hash = createHash("sha256").update(scripts[0][1]).digest("base64");
    expect(csp, `${article.path}'s inline script hash is missing from _headers`).toContain(`'sha256-${hash}'`);
  }
});

test("the sentence for an agent copies from the keyboard, with the MCP docs beside it", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.route("**/hero/**", (route) => route.abort());
  await page.goto("/#mcp");
  const block = page.locator(".agent-prompt");
  await expect(block.locator("#agent-prompt")).toHaveText(AGENT_SENTENCE);
  const button = block.getByRole("button", { name: "Copy the sentence for your agent" });
  await button.focus();
  await expect(button).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(button).toContainText("Copied");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(`${AGENT_SENTENCE}\n`);
  await expect(button).toContainText("Copy", { timeout: 4000 });
  await page.keyboard.press("Space");
  await expect(button).toContainText("Copied");
  await expect(block.getByRole("link", { name: "docs/mcp.md" })).toHaveAttribute(
    "href",
    "https://github.com/lupidae/romulus/blob/main/docs/mcp.md",
  );
});

test("the sentence for an agent is true: the README carries it, and the endpoint is the one the docs give", async () => {
  const readme = readFileSync(fileURLToPath(new URL("../../README.md", import.meta.url)), "utf8");
  expect(readme.slice(0, 800)).toContain(AGENT_SENTENCE);
  expect(readme.slice(0, 1200)).toContain("[docs/mcp.md](docs/mcp.md)");
  const mcp = readFileSync(fileURLToPath(new URL("../../docs/mcp.md", import.meta.url)), "utf8");
  // The endpoint is /api/mcp, over HTTP, and the client signs in through the browser (OAuth).
  expect(mcp).toContain("claude mcp add --transport http romulus https://romulus.example.com/api/mcp");
  expect(mcp).toContain("the agent opens your browser on romulus");
});

test.describe("the welcome for a launch post's visitors", () => {
  for (const [query, source] of [
    ["?ref=hn", "Hacker News"],
    ["?ref=producthunt", "Product Hunt"],
    ["?utm_source=hn&utm_medium=social", "Hacker News"],
    ["?utm_source=producthunt", "Product Hunt"],
    ["?ref=HN", "Hacker News"],
  ]) {
    test(`${query} is welcomed as ${source}`, async ({ page }) => {
      await page.route("**/hero/**", (route) => route.abort());
      await page.goto(`/${query}`);
      const welcome = page.getByRole("complementary", { name: "Welcome" });
      await expect(welcome).toHaveText(`Welcome, ${source}: self-host free, or try the founding-team offer.`);
      // Right under the header.
      expect(await page.evaluate(() => document.querySelector(".site-header")?.nextElementSibling?.className)).toBe(
        "welcome-line",
      );
      await expect(welcome.getByRole("link", { name: "self-host free" })).toHaveAttribute("href", "/#self-host");
      await expect(welcome.getByRole("link", { name: "founding-team offer" })).toHaveAttribute("href", "/#pricing");
    });
  }

  test("nobody else sees it", async ({ page }) => {
    await page.route("**/hero/**", (route) => route.abort());
    for (const path of ["/", "/?ref=twitter", "/?utm_source=newsletter", "/compare/atlas"]) {
      await page.goto(path);
      await expect(page.locator(".site-header")).toBeVisible();
      await expect(page.locator(".welcome-line")).toHaveCount(0);
    }
  });

  test("it closes from the keyboard and stays closed", async ({ page }) => {
    await page.route("**/hero/**", (route) => route.abort());
    await page.goto("/?ref=hn");
    const close = page.getByRole("button", { name: "Close the welcome" });
    await close.focus();
    await page.keyboard.press("Enter");
    await expect(page.locator(".welcome-line")).toHaveCount(0);
    await page.goto("/compare/bytebase?utm_source=producthunt");
    await expect(page.locator(".site-header")).toBeVisible();
    await expect(page.locator(".welcome-line")).toHaveCount(0);
  });

  test("it shows on the comparisons and guides too", async ({ page }) => {
    await page.goto("/guides/postgres-schema-diff?ref=producthunt");
    await expect(page.getByRole("complementary", { name: "Welcome" })).toContainText("Welcome, Product Hunt");
  });

  test("it is fine on a phone", async ({ page }) => {
    await page.route("**/hero/**", (route) => route.abort());
    await page.setViewportSize({ width: 360, height: 800 });
    await page.goto("/?ref=producthunt");
    await expect(page.getByRole("complementary", { name: "Welcome" })).toBeVisible();
    const overflow = await page.evaluate(
      () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
    );
    expect(overflow).toBe(0);
    const box = await page.getByRole("button", { name: "Close the welcome" }).boundingBox();
    expect(box!.x + box!.width).toBeLessThanOrEqual(360);
  });

  test("axe finds no violations with it open", async ({ page }) => {
    await page.route("**/hero/**", (route) => route.abort());
    await page.goto("/compare/atlas?ref=hn");
    await expect(page.locator(".welcome-line")).toBeVisible();
    const results = await new AxeBuilder({ page })
      .include(".welcome-line")
      .withTags(["wcag2a", "wcag2aa", "wcag21aa"])
      .analyze();
    expect(results.violations.map((v) => v.id)).toEqual([]);
  });
});

test("the homepage's share card is a large X card, its image absolute and 1200×630", async ({ page, request }) => {
  await page.goto("/");
  const meta = (selector: string) => page.locator(selector).getAttribute("content");
  expect(await meta('meta[name="twitter:card"]')).toBe("summary_large_image");
  for (const selector of ['meta[property="og:image"]', 'meta[name="twitter:image"]'])
    expect(await meta(selector)).toBe(`${ORIGIN}/og.png`);
  expect(await meta('meta[name="twitter:image:alt"]')).toBeTruthy();
  const png = await (await request.get("/og.png")).body();
  expect(pngSize(png)).toEqual([1200, 630]);
  // Under X's 5 MB for a card image.
  expect(png.length).toBeLessThan(5 * 1024 * 1024);
});
