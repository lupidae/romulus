import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

import { price, readPlans, withPlans } from "../tools/pricing.mjs";

const publicFile = (name: string) =>
  readFileSync(fileURLToPath(new URL(`../public/${name}`, import.meta.url)), "utf8");

/** The static picture only: the live hero (e2e/hero.spec.ts) is not loaded. */
async function withoutLiveHero(page: Page) {
  await page.route("**/hero/**", (route) => route.abort());
}

/** Fails the test on any console error or uncaught exception (CSP violations included). */
function watchErrors(page: Page, allow?: RegExp) {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error" && !allow?.test(m.text())) errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(String(e)));
  return errors;
}

test("the hero says what romulus is and how to start", async ({ page }) => {
  // The live hero's requests are refused on purpose.
  const errors = watchErrors(page, /Failed to load resource/);
  await withoutLiveHero(page);
  await page.goto("/");

  await expect(page).toHaveTitle(/design, version and review your Postgres schema together/i);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(
    "Design, version and review your Postgres schema together.",
  );
  await expect(page.getByRole("banner")).toBeVisible();
  await expect(page.getByRole("main")).toBeVisible();
  await expect(page.getByRole("contentinfo")).toBeVisible();

  const hero = page.locator(".hero");
  await expect(hero.getByRole("link", { name: "Start free" })).toHaveAttribute("href", "/app/signin");
  await expect(hero.getByRole("link", { name: "Talk to us" })).toHaveAttribute(
    "href",
    "mailto:romulus@lupidae.com",
  );
  for (const region of [page.getByRole("banner"), hero]) {
    await expect(region.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", "/app/signin");
  }
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

  await page
    .getByRole("navigation", { name: "Sections" })
    .getByRole("link", { name: "Migrations" })
    .click();
  await expect(page).toHaveURL(/#migrations$/);
  await expect(
    page.getByRole("heading", { name: "Bring your migrations. Get the next one written." }),
  ).toBeInViewport();
});

test("the page covers what ships, section by section", async ({ page }) => {
  await page.goto("/");
  for (const name of [
    "Every version kept. Any two compared.",
    "Design the next version together.",
    "Review the change where it's made.",
    "Bring your migrations. Get the next one written.",
    "Try the migration on rows first.",
    "Push the real schema from CI.",
    "Postgres, down to the last policy.",
    "Roles that match how teams review.",
    "Flat plans. Bring the whole team.",
    "Run it on your own servers.",
  ])
    await expect(page.getByRole("heading", { level: 2, name })).toHaveCount(1);
  // Each database targets one of these majors (docs/MVP.md, "Several Postgres majors").
  await expect(
    page.getByRole("list", { name: "Postgres versions a database can target" }).getByRole("listitem"),
  ).toHaveText(["PostgreSQL 18", "PostgreSQL 17default", "PostgreSQL 16", "PostgreSQL 15"]);
  // The roles table matches docs/MVP.md "Roles" and CLAUDE.md's matrix.
  const row = (name: string) => page.getByRole("row", { name: new RegExp(`^${name}`) });
  await expect(row("Approve drafts others edited").getByRole("img", { name: "Yes" })).toHaveCount(3);
  await expect(row("Remove and restore versions").getByRole("img", { name: "Yes" })).toHaveCount(1);
  await expect(row("Create playgrounds").getByRole("img", { name: "No" })).toHaveCount(1);
});

test("the header and the pricing card lead to pricing and self-hosting", async ({ page }) => {
  await page.goto("/");
  const nav = page.getByRole("navigation", { name: "Sections" });
  await nav.getByRole("link", { name: "Pricing" }).click();
  await expect(page).toHaveURL(/#pricing$/);
  const pricingTitle = page.getByRole("heading", { level: 2, name: "Flat plans. Bring the whole team." });
  await expect(pricingTitle).toBeInViewport();

  await page.getByRole("link", { name: "How to self-host" }).click();
  await expect(page).toHaveURL(/#self-host$/);
  await expect(page.getByRole("heading", { level: 2, name: "Run it on your own servers." })).toBeInViewport();

  await page.goto("/");
  await nav.getByRole("link", { name: "Self-host" }).click();
  await expect(page).toHaveURL(/#self-host$/);
  await expect(page.getByRole("heading", { level: 2, name: "Run it on your own servers." })).toBeInViewport();

  await expect(page.getByRole("banner").getByRole("link", { name: "romulus on GitHub" })).toHaveAttribute(
    "href",
    "https://github.com/lupidae/romulus",
  );
});

test("one way in: every Start free goes where the plans do, Talk to us is a mail", async ({ page }) => {
  const { signup } = readPlans(publicFile("index.html"));
  await page.goto("/");
  await expect(page.getByText("Request access")).toHaveCount(0);
  const regions = [
    page.getByRole("banner"),
    page.locator(".hero .cta-row"),
    page.locator(".closing .cta-row"),
  ];
  for (const region of regions) {
    await expect(region.getByRole("link", { name: "Start free", exact: true })).toHaveAttribute(
      "href",
      signup,
    );
  }
  // The hero and the closing lead with Start free; Talk to us is the quieter
  // one, and the hero's Sign in, for whoever has an account, the quietest.
  const [, heroRow, closingRow] = regions;
  await expect(heroRow.getByRole("link")).toHaveText(["Start free", "Talk to us", "Sign in"]);
  await expect(heroRow.getByRole("link", { name: "Sign in" })).toHaveClass(/\bghost\b/);
  await expect(closingRow.getByRole("link")).toHaveText(["Start free", "Talk to us"]);
  for (const row of [heroRow, closingRow]) {
    await expect(row.getByRole("link", { name: "Start free" })).toHaveClass(/\bprimary\b/);
    await expect(row.getByRole("link", { name: "Talk to us" })).toHaveClass(/\bsecondary\b/);
    await expect(row.getByRole("link", { name: "Talk to us" })).toHaveAttribute(
      "href",
      "mailto:romulus@lupidae.com",
    );
  }
});

test("the plan cards are the page's own JSON, rendered", async () => {
  // site/tools/pricing.mjs writes them from #plans-data: run it after a change.
  const html = publicFile("index.html");
  expect(withPlans(html) === html, "run `npm --prefix site run pricing`").toBe(true);
  // Yearly is ten months: two months free, as the switch says.
  expect(readPlans(html).yearly_months).toBe(10);
});

test("the billing switch shows monthly or yearly prices", async ({ page }) => {
  const plans = readPlans(publicFile("index.html"));
  await page.goto("/#pricing");
  const pricing = page.locator("#pricing");
  const period = pricing.getByRole("group", { name: "Billing period" });
  // Both periods' prices are in each card: innerText is the one shown.
  const shown = async (plan: string, part = ".plan-price") => {
    const text = await pricing.getByRole("article", { name: plan, exact: true }).locator(part).innerText();
    return text.replace(/\s+/g, " ").trim();
  };
  const amount = (n: number) => price(n, plans.currency).replace(/\s/g, " ");

  await expect(period.getByRole("radio", { name: "Monthly" })).toBeChecked();
  for (const plan of plans.plans) expect(await shown(plan.name)).toBe(`${amount(plan.monthly)} per month`);
  expect(await shown("Starter")).toBe("19 € per month");
  expect(await shown("Starter", ".plan-alt")).toBe("or 190 € a year");

  // From the keyboard, as a radio group.
  await period.getByRole("radio", { name: "Monthly" }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(period.getByRole("radio", { name: /^Yearly/ })).toBeChecked();
  for (const plan of plans.plans)
    expect(await shown(plan.name)).toBe(`${amount(plan.monthly * plans.yearly_months)} per year`);
  expect(await shown("Business")).toBe("1,490 € per year");
  expect(await shown("Business", ".plan-alt")).toBe("or 149 € a month");

  await period.getByText("Monthly").click();
  expect(await shown("Team")).toBe("49 € per month");
});

test("every plan starts with a free sign-up, Team is the one recommended", async ({ page }) => {
  const plans = readPlans(publicFile("index.html"));
  await page.goto("/#pricing");
  const pricing = page.locator("#pricing");
  for (const plan of plans.plans) {
    const card = pricing.getByRole("article", { name: plan.name, exact: true });
    await expect(card.getByRole("link", { name: plan.action })).toHaveAttribute("href", plans.signup);
    // Same fields, same order, in every card.
    await expect(card.locator("dt")).toHaveText(plans.limits.map((l: { label: string }) => l.label));
  }
  await expect(pricing.getByRole("link", { name: "Start free", exact: true })).toHaveCount(1);
  await expect(pricing.getByRole("link", { name: "Start free, upgrade anytime" })).toHaveCount(3);
  await expect(pricing.getByRole("link", { name: "Talk to us" })).toHaveAttribute(
    "href",
    /^mailto:romulus@lupidae\.com\?subject=/,
  );
  await expect(pricing.locator(".plan.is-recommended")).toHaveCount(1);
  await expect(pricing.locator(".plan.is-recommended h3")).toHaveText("Team");
  // No fake urgency, no popularity claims.
  await expect(pricing).not.toContainText(/popular|limited time|only \d+ left/i);
  await expect(pricing.getByText("TVA non applicable, art. 293 B du CGI.")).toHaveAttribute("lang", "fr");
});

test("the push command copies to the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/#ci");
  const button = page.getByRole("button", { name: "Copy the push command" });
  await button.click();
  await expect(button).toContainText("Copied");
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  // The spec's push: POST /api/databases/<id>/versions with the database's
  // bearer token. Five shell lines, whatever wrapped on screen.
  expect(copied).toBe(
    [
      'remus -u "$DATABASE_URL" -f json \\',
      "  | curl -fsS --data-binary @- \\",
      '      -H "Authorization: Bearer $ROMULUS_TOKEN" \\',
      '      -H "Content-Type: application/json" \\',
      '      "$ROMULUS_URL/api/databases/$ROMULUS_DATABASE_ID/versions"',
      "",
    ].join("\n"),
  );
  await expect(button).toContainText("Copy", { timeout: 4000 });
});

test("each way to self-host copies its commands", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await page.goto("/#self-host");
  const ways: [string, string[]][] = [
    [
      "Copy the Docker install commands",
      ["git clone https://github.com/lupidae/romulus.git", "cd romulus/deploy/self-host", "./install.sh"],
    ],
    [
      "Copy the Helm install command",
      [
        "helm install romulus oci://ghcr.io/lupidae/charts/romulus \\",
        "  --namespace romulus --create-namespace \\",
        "  --set host=romulus.example.com",
      ],
    ],
    [
      "Copy the build commands",
      [
        "git clone https://github.com/lupidae/romulus.git && cd romulus",
        "docker build -t local/romulus-api:dev .",
        "docker build -t local/romulus-sandbox:dev deploy/sandbox",
        "cd deploy/self-host",
        "./install.sh --registry local --version dev --no-pull",
      ],
    ],
  ];
  for (const [name, lines] of ways) {
    const button = page.getByRole("button", { name });
    await button.click();
    await expect(button).toContainText("Copied");
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe([...lines, ""].join("\n"));
  }
  await expect(page.getByRole("link", { name: "the Kubernetes guide" })).toHaveAttribute(
    "href",
    "https://github.com/lupidae/romulus/blob/main/docs/self-host/kubernetes.md",
  );
  await expect(page.getByRole("link", { name: "Read the self-host docs" })).toHaveAttribute(
    "href",
    "https://github.com/lupidae/romulus/tree/main/docs/self-host",
  );
});

for (const width of [360, 390]) {
  test(`the commands wrap instead of clipping at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 800 });
    await page.goto("/#ci");
    const blocks = page.locator(".terminal-body");
    expect(await blocks.count()).toBe(4);
    for (const pre of await blocks.all()) {
      const { scroll, client } = await pre.evaluate((el) => ({
        scroll: el.scrollWidth,
        client: el.clientWidth,
      }));
      expect(scroll).toBeLessThanOrEqual(client);
    }
  });
}

test("the footer links the project, its docs, its security policy and a contact", async ({ page }) => {
  await page.goto("/");
  const footer = page.getByRole("navigation", { name: "Footer" });
  const href = (name: string) => footer.getByRole("link", { name, exact: true });
  await expect(href("GitHub")).toHaveAttribute("href", "https://github.com/lupidae/romulus");
  await expect(href("Self-host docs")).toHaveAttribute(
    "href",
    "https://github.com/lupidae/romulus/tree/main/docs/self-host",
  );
  await expect(href("Security policy")).toHaveAttribute(
    "href",
    "https://github.com/lupidae/romulus/blob/main/SECURITY.md",
  );
  await expect(href("Contact")).toHaveAttribute("href", "mailto:romulus@lupidae.com");
  // Terms and privacy wait for counsel: their place is kept, hidden and empty.
  const legal = page.locator(".footer-legal");
  await expect(legal).toBeHidden();
  await expect(legal.locator("a")).toHaveCount(0);
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
  await withoutLiveHero(page);
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
  await withoutLiveHero(page);
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
    // The live hero too: it loads once the page is idle on a desktop.
    test.setTimeout(90_000);
    await expect(page.locator(".hero-stage.is-live")).toBeAttached({ timeout: 60_000 });
    const results = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze();
    expect(results.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`)).toEqual([]);
  });
}
