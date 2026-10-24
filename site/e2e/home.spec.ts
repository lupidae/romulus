import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import AxeBuilder from "@axe-core/playwright";
import type { Page } from "@playwright/test";

import { price, readPlans, subscribeHref, withPlans } from "../tools/pricing.mjs";
import { ANALYTICS, expect, test } from "./fixtures";

const publicFile = (name: string) =>
  readFileSync(fileURLToPath(new URL(`../public/${name}`, import.meta.url)), "utf8");
const repoFile = (path: string) => readFileSync(fileURLToPath(new URL(`../../${path}`, import.meta.url)), "utf8");

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
  await expect(hero.getByRole("link", { name: "Choose a plan" })).toHaveAttribute("href", "#pricing");
  await expect(hero.getByRole("link", { name: "Start free" })).toHaveAttribute("href", "/app/signin");
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
    .getByRole("link", { name: "MCP" })
    .click();
  await expect(page).toHaveURL(/#mcp$/);
  await expect(page.getByRole("heading", { name: "Your agent speaks romulus." })).toBeInViewport();
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
    "Your agent speaks romulus.",
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

test("the agents section: in the nav, a readable session, the command to connect", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await withoutLiveHero(page);
  await page.goto("/");
  await page.getByRole("navigation", { name: "Sections" }).getByRole("link", { name: "MCP" }).click();
  await expect(page).toHaveURL(/#mcp$/);
  const section = page.locator("#mcp");
  await expect(section.getByRole("heading", { level: 2, name: "Your agent speaks romulus." })).toBeInViewport();

  // The session is text a screen reader reads, not a picture: Ada asks, the
  // agent calls three tools, then answers.
  const session = section.getByRole("figure", { name: /Ada's agent/ });
  await expect(session).toBeVisible();
  const turns = session.locator(".transcript > li");
  await expect(turns).toHaveCount(5);
  await expect(turns.first()).toContainText("Add a refunds table linked to invoices, and test the migration.");
  await expect(turns.last()).toContainText("Shall I request a review?");
  const tools = await session.locator(".call-name").allInnerTexts();
  expect(tools).toEqual(["create_draft", "generate_migration", "test_migration"]);
  await expect(session.getByText("Tool call:")).toHaveCount(3);
  await expect(session).toContainText("Up and down ran on 4,237 rows. Everything came back.");

  // Honest: the tools exist under these names, and their answers read so.
  const toolSources = ["drafts.rs", "testing.rs"]
    .map((file) => repoFile(`crates/romulus-api/src/mcp/tools/${file}`))
    .join("\n");
  for (const tool of tools) expect(toolSources, tool).toContain(`name: "${tool}"`);
  for (const said of [
    '"Created draft {} \\"{}\\" in {org}/{} ({}, {}).\\n"',
    "{} that can lose data or fail on existing rows.",
    '"PASSED (up and down)"',
  ])
    expect(toolSources).toContain(said);
  const check = repoFile("crates/romulus-migration-check/src/lib.rs");
  for (const said of ['"Up and down ran on {rows} rows."', '"{ran} Everything came back."'])
    expect(check).toContain(said);

  const button = section.getByRole("button", { name: "Copy the MCP command" });
  await button.click();
  await expect(button).toContainText("Copied");
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    "claude mcp add --transport http romulus https://romulus.lupidae.com/api/mcp\n",
  );
  await expect(section.getByRole("link", { name: "use an access token" })).toHaveAttribute(
    "href",
    "https://github.com/lupidae/romulus/blob/main/docs/mcp.md",
  );
});

test("subscribing leads, Free stays one click away, Talk to us is a mail", async ({ page }) => {
  const { signup } = readPlans(publicFile("index.html"));
  await page.goto("/");
  await expect(page.getByText("Request access")).toHaveCount(0);
  await expect(page.getByText(/upgrade anytime/i)).toHaveCount(0);
  const [header, heroRow, closingRow] = [
    page.getByRole("banner"),
    page.locator(".hero .cta-row"),
    page.locator(".closing .cta-row"),
  ];
  // "Choose a plan" leads (the pricing, where each plan subscribes); Free's
  // sign-up follows; the hero's Sign in, for whoever has an account, and the
  // closing's Talk to us are the quietest.
  await expect(header.getByRole("link", { name: "Choose a plan" })).toHaveAttribute("href", "#pricing");
  await expect(heroRow.getByRole("link")).toHaveText(["Choose a plan", "Start free", "Sign in"]);
  await expect(closingRow.getByRole("link")).toHaveText(["Choose a plan", "Start free", "Talk to us"]);
  for (const row of [heroRow, closingRow]) {
    await expect(row.getByRole("link", { name: "Choose a plan" })).toHaveClass(/\bprimary\b/);
    await expect(row.getByRole("link", { name: "Choose a plan" })).toHaveAttribute("href", "#pricing");
    await expect(row.getByRole("link", { name: "Start free" })).toHaveClass(/\bsecondary\b/);
    await expect(row.getByRole("link", { name: "Start free" })).toHaveAttribute("href", signup);
  }
  await expect(heroRow.getByRole("link", { name: "Sign in" })).toHaveClass(/\bghost\b/);
  await expect(closingRow.getByRole("link", { name: "Talk to us" })).toHaveClass(/\bghost\b/);
  await expect(closingRow.getByRole("link", { name: "Talk to us" })).toHaveAttribute(
    "href",
    "mailto:romulus@lupidae.com",
  );

  await heroRow.getByRole("link", { name: "Choose a plan" }).click();
  await expect(page).toHaveURL(/#pricing$/);
  await expect(page.getByRole("heading", { level: 2, name: "Flat plans. Bring the whole team." })).toBeInViewport();
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

test("each paid plan subscribes to itself for the period shown, Free signs up", async ({ page }) => {
  const plans = readPlans(publicFile("index.html"));
  await page.goto("/#pricing");
  const pricing = page.locator("#pricing");
  const period = pricing.getByRole("group", { name: "Billing period" });
  const card = (name: string) => pricing.getByRole("article", { name, exact: true });
  for (const plan of plans.plans) {
    const link = card(plan.name).getByRole("link", { name: plan.action });
    // One button shown per card (the other period's is hidden, out of the accessibility tree).
    await expect(link).toHaveCount(1);
    await expect(link).toHaveAttribute(
      "href",
      plan.monthly ? subscribeHref(plans, plan, "month") : plans.signup,
    );
    // Same fields, same order, in every card.
    await expect(card(plan.name).locator("dt")).toHaveText(plans.limits.map((l: { label: string }) => l.label));
  }
  // The API's plan ids (billing::plans), each paid one under its own name.
  const paid = plans.plans.filter((p: { monthly: number }) => p.monthly > 0);
  expect(paid.map((p: { id: string }) => p.id)).toEqual(["starter", "team", "business"]);
  const planTable = repoFile("crates/romulus-api/src/billing/plans.rs");
  for (const plan of paid) {
    expect(planTable).toContain(`Self::${plan.name} => "${plan.id}"`);
    expect(plan.action).toBe(`Subscribe to ${plan.name}`);
  }
  await expect(card("Team").getByRole("link", { name: "Subscribe to Team" })).toHaveAttribute(
    "href",
    "/app/signup?plan=team&interval=month",
  );
  await expect(card("Free").getByRole("link", { name: "Start free", exact: true })).toHaveAttribute(
    "href",
    plans.signup,
  );

  // Yearly: the same buttons, the year's links.
  await period.getByText(/^Yearly/).click();
  for (const plan of paid)
    await expect(card(plan.name).getByRole("link", { name: plan.action })).toHaveAttribute(
      "href",
      subscribeHref(plans, plan, "year"),
    );
  await expect(card("Free").getByRole("link", { name: "Start free", exact: true })).toHaveAttribute(
    "href",
    plans.signup,
  );
  await period.getByText("Monthly").click();
  await expect(card("Business").getByRole("link", { name: "Subscribe to Business" })).toHaveAttribute(
    "href",
    "/app/signup?plan=business&interval=month",
  );

  // Nothing reads as a purchase by a consumer: subscribing, by businesses.
  await expect(pricing).not.toContainText(/buy now|add to cart|personal use/i);
  await expect(pricing.getByRole("link", { name: "Start free", exact: true })).toHaveCount(1);
  await expect(pricing.getByRole("link", { name: "Talk to us" })).toHaveAttribute(
    "href",
    /^mailto:romulus@lupidae\.com\?subject=/,
  );
  await expect(pricing.locator(".plan.is-recommended")).toHaveCount(1);
  await expect(pricing.locator(".plan.is-recommended h3")).toHaveText("Team");
  // No fake urgency, no popularity claims.
  await expect(pricing).not.toContainText(/popular|limited time|only \d+ left/i);
  // Who the paid plans are for, and the VAT, said once, under the switch.
  const note = pricing.locator(".period-note");
  await expect(note).toContainText("Paid plans are for businesses and professionals.");
  await expect(note).toContainText("Prices excl. VAT; VAT not applicable, art. 293 B du CGI.");
  await expect(note.locator('[lang="fr"]')).toHaveText("art. 293 B du CGI");
  await expect(pricing.getByText(/VAT|TVA/)).toHaveCount(1);
});

test("the plans promise what the hosted Terms say", async ({ page }) => {
  await page.goto("/#pricing");
  const pricing = page.locator("#pricing");
  const limit = (label: string) =>
    pricing.locator(".plan-limits > div").filter({ has: page.locator("dt", { hasText: label }) }).locator("dd");
  // The cap counts one version's tables and views (billing::plans, `schema_objects`).
  await expect(limit("Tables and views in each version")).toHaveText(["50", "200", "1,000", "5,000"]);
  await expect(pricing.getByText("Tables and views per database")).toHaveCount(0);
  // Terms 7.1 and 13.3: support by email on paid plans, its times are targets.
  await expect(limit("Support")).toHaveText([
    "Community",
    "Email",
    "Email, aiming to answer in 2 business days",
    "Priority email",
  ]);
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
    expect(await blocks.count()).toBe(5);
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
  const legal = page.getByRole("navigation", { name: "Legal" });
  const documents = [
    ["Legal notice", "/legal-notice"],
    ["Terms", "/legal/terms"],
    ["Privacy", "/legal/privacy"],
    ["DPA (for businesses)", "/legal/dpa"],
  ];
  await expect(legal.getByRole("link")).toHaveText(documents.map(([name]) => name));
  for (const [name, path] of documents)
    await expect(legal.getByRole("link", { name, exact: true })).toHaveAttribute("href", path);
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

test("the FAQ says what ships: agents, paid plans, the export", async ({ page }) => {
  await withoutLiveHero(page);
  await page.goto("/#faq");
  const faq = page.locator(".faq");
  const open = async (question: string) => {
    const item = faq.locator("details").filter({ has: page.getByText(question, { exact: true }) });
    await item.locator("summary").click();
    const answer = item.locator("p");
    await expect(answer).toBeVisible();
    return answer;
  };

  const agents = await open("Does it work with my AI agent?");
  await expect(agents).toContainText("MCP server built in");
  await agents.getByRole("link", { name: "See how to connect it" }).click();
  await expect(page).toHaveURL(/#mcp$/);
  await expect(page.getByRole("heading", { level: 2, name: "Your agent speaks romulus." })).toBeInViewport();

  // Paid plans are bought as the organization is made (or later), and the export exists, on every plan.
  const cost = await open("What does it cost?");
  await expect(cost).toContainText("Choose a plan there and you subscribe as you create your organization");
  await expect(cost).toContainText("an admin moves the organization to a paid plan from its Billing page");
  await expect(await open("Can we move from hosted to self-hosted?")).toContainText(
    "On every plan, an admin can export the whole organization as one ZIP",
  );
  await expect(faq).not.toContainText(/not open yet|being built/);
});

test("an unknown path gets the 404 page", async ({ page }) => {
  const response = await page.goto("/no-such-page");
  expect(response?.status()).toBe(404);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("This page doesn't exist.");
  await expect(page.getByRole("link", { name: "Go to the homepage" })).toHaveAttribute("href", "/");
  await expect(page.getByRole("navigation", { name: "Legal" }).getByRole("link")).toHaveText([
    "Legal notice",
    "Terms",
    "Privacy",
    "DPA",
  ]);
});

test("the CSP allows exactly the inline theme script", async ({ request }) => {
  const response = await request.get("/");
  const csp = response.headers()["content-security-policy"];
  for (const page of [
    "index.html",
    "404.html",
    "mentions-legales.html",
    "legal-notice.html",
    "legal/terms.html",
    "fr/legal/conditions.html",
    "legal/privacy.html",
    "fr/legal/confidentialite.html",
    "legal/dpa.html",
    "legal/full/terms.html",
    "fr/legal/full/conditions.html",
    "legal/full/privacy.html",
    "fr/legal/full/confidentialite.html",
  ]) {
    const scripts = [...publicFile(page).matchAll(/<script>([\s\S]*?)<\/script>/g)];
    expect(scripts).toHaveLength(1);
    const hash = createHash("sha256").update(scripts[0][1]).digest("base64");
    expect(csp, `${page}'s inline script hash is missing from _headers`).toContain(`'sha256-${hash}'`);
  }
});

test("the visit count: one tag, its one origin in the CSP, the sign-up buttons counted", async ({
  page,
  request,
  analytics,
}) => {
  const csp = (await request.get("/")).headers()["content-security-policy"];
  const directive = (name: string) => csp.split(";").map((d) => d.trim()).find((d) => d.startsWith(`${name} `));
  expect(directive("script-src")).toContain(` ${ANALYTICS}`);
  expect(directive("connect-src")).toBe(`connect-src 'self' ${ANALYTICS}`);
  // No other origin anywhere in the policy.
  expect(new Set(csp.match(/https?:\/\/[^\s;]+/g))).toEqual(new Set([ANALYTICS]));

  await withoutLiveHero(page);
  await page.goto("/");
  const tag = page.locator(`script[src="${ANALYTICS}/script.js"]`);
  await expect(tag).toHaveCount(1);
  expect(await tag.evaluate((s: HTMLScriptElement) => ({ ...s.dataset, defer: s.defer }))).toEqual({
    websiteId: "00e44160-7141-4b01-a9d2-4b4bf27d8236",
    domains: "romulus.lupidae.com",
    doNotTrack: "true",
    excludeHash: "true",
    defer: true,
  });
  expect(analytics).toEqual([`${ANALYTICS}/script.js`]);

  // Every way to sign up says so (with the plan); signing in does not.
  const events = await page.locator("a[href^='/app/']").evaluateAll((links) =>
    links.map((a) => ({
      text: a.textContent!.trim(),
      event: a.getAttribute("data-umami-event"),
      plan: a.getAttribute("data-umami-event-plan"),
    })),
  );
  const signUps = events.filter((e) => /^(Start free|Subscribe to )/.test(e.text));
  expect(signUps.length).toBe(9);
  for (const e of signUps) expect(e.event, e.text).toBe("signup_started");
  expect(signUps.filter((e) => e.plan).map((e) => e.plan)).toEqual([
    "free",
    "starter",
    "starter",
    "team",
    "team",
    "business",
    "business",
  ]);
  for (const e of events.filter((e) => !signUps.includes(e))) expect(e.event, e.text).toBeNull();
});

test("the first visit's source is kept for the app on this origin, first touch wins", async ({ page, baseURL }) => {
  await withoutLiveHero(page);
  const kept = () => page.evaluate(() => JSON.parse(localStorage.getItem("romulus.source") ?? "null"));
  await page.goto("/?utm_source=newsletter&utm_campaign=launch&utm_content=hero&gclid=abc&email=a@b.c", {
    referer: "https://news.example.org/item?id=42",
  });
  const first = await kept();
  expect(first).toEqual({
    v: 1,
    at: expect.any(Number),
    utm_source: "newsletter",
    utm_campaign: "launch",
    utm_content: "hero",
    referrer: "https://news.example.org",
  });
  await page.goto("/?utm_source=ads&utm_medium=cpc", { referer: "https://ads.example.com/" });
  expect(await kept()).toEqual(first);

  // The app is this origin's /app (the Function proxies it): it reads the same
  // record. Here a page standing in for it says what it finds.
  await page.route("**/app/**", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<!doctype html><title>app</title><pre id=kept></pre><script>document.getElementById('kept').textContent = localStorage.getItem('romulus.source')</script>",
    }),
  );
  await page.locator(".hero").getByRole("link", { name: "Start free" }).click();
  await expect(page).toHaveURL(`${baseURL}/app/signin`);
  expect(JSON.parse((await page.locator("#kept").textContent())!)).toEqual(first);
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
