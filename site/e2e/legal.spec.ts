import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

// The legal notice (LCEN), in French and in English: static pages, linked
// from the footer and from each other.
const pages = [
  {
    path: "/mentions-legales",
    lang: "fr",
    title: "Mentions légales – romulus",
    other: { path: "/legal-notice", link: "Read in English", lang: "en" },
    legalNav: "Informations légales",
    facts: {
      Éditeur: "Lucas Lefort EI (entrepreneur individuel)",
      Siège: "108 rue de Gand, 59200 Tourcoing, France",
      SIREN: "949 815 138",
      SIRET: "949 815 138 00019",
      Immatriculation: "Registre national des entreprises (RNE)",
      TVA: "TVA non applicable, art. 293 B du CGI",
      Téléphone: "07 82 98 35 07",
      "E-mail": "romulus@lupidae.com",
      "Directeur de la publication": "Lucas Lefort",
      Hébergeur: "Cloudflare, Inc.",
      Adresse: "101 Townsend St, San Francisco, CA 94107, États-Unis",
    },
  },
  {
    path: "/legal-notice",
    lang: "en",
    title: "Legal notice – romulus",
    other: { path: "/mentions-legales", link: "Lire en français", lang: "fr" },
    legalNav: "Legal",
    facts: {
      Publisher: "Lucas Lefort EI (entrepreneur individuel, a French sole proprietorship)",
      "Registered office": "108 rue de Gand, 59200 Tourcoing, France",
      SIREN: "949 815 138",
      SIRET: "949 815 138 00019",
      VAT: "Not applicable, article 293 B of the French General Tax Code (TVA non applicable, art. 293 B du CGI)",
      Phone: "+33 7 82 98 35 07",
      Email: "romulus@lupidae.com",
      "Director of publication": "Lucas Lefort",
      Host: "Cloudflare, Inc.",
      Address: "101 Townsend St, San Francisco, CA 94107, United States",
    },
  },
] as const;

const heading = (path: string) => (path === "/mentions-legales" ? "Mentions légales" : "Legal notice");

/** The value of a label / value row (the first one with that label: the publisher's comes before the host's). */
const fact = (page: Page, label: string) =>
  page
    .locator(".legal-facts > div")
    .filter({ has: page.locator("dt", { hasText: new RegExp(`^${label}$`) }) })
    .first()
    .locator("dd");

for (const legal of pages) {
  test(`${legal.path} loads, in its language, with what the law asks for`, async ({ page }) => {
    const errors: string[] = [];
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    page.on("pageerror", (e) => errors.push(String(e)));

    const response = await page.goto(legal.path);
    expect(response?.status()).toBe(200);
    await expect(page.locator("html")).toHaveAttribute("lang", legal.lang);
    await expect(page).toHaveTitle(legal.title);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(heading(legal.path));
    await expect(page.getByRole("banner")).toBeVisible();
    await expect(page.getByRole("contentinfo")).toBeVisible();
    await expect(page.locator('meta[name="robots"]')).toHaveCount(0);

    // The EI mention follows the name immediately (Code de commerce, R. 526-27).
    for (const [label, value] of Object.entries(legal.facts)) await expect(fact(page, label)).toHaveText(value);
    const main = page.getByRole("main");
    await expect(main.getByRole("link", { name: "romulus@lupidae.com" }).first()).toHaveAttribute(
      "href",
      "mailto:romulus@lupidae.com",
    );
    await expect(main.getByRole("link", { name: "+1 (650) 319-8930" })).toHaveAttribute("href", "tel:+16503198930");
    await expect(main.locator('a[href="tel:+33782983507"]')).toHaveCount(1);
    await expect(main.getByRole("link", { name: "GNU Affero General Public License v3.0" })).toHaveAttribute(
      "href",
      "https://github.com/lupidae/romulus/blob/main/LICENSE",
    );
    // The privacy policy and the terms await counsel: named, never linked.
    await expect(main).toContainText("romulus.lupidae.com/legal/privacy");
    await expect(page.locator('a[href*="privacy"], a[href*="terms"]')).toHaveCount(0);

    const head = (selector: string) => page.locator(selector).getAttribute("href");
    expect(await head('link[rel="canonical"]')).toBe(`https://romulus.lupidae.com${legal.path}`);
    expect(await head('link[rel="alternate"][hreflang="fr"]')).toBe("https://romulus.lupidae.com/mentions-legales");
    expect(await head('link[rel="alternate"][hreflang="en"]')).toBe("https://romulus.lupidae.com/legal-notice");

    // Its own footer link is marked as the page you are on.
    const own = page.getByRole("navigation", { name: legal.legalNav }).getByRole("link");
    await expect(own).toHaveText([heading(legal.path)]);
    await expect(own).toHaveAttribute("href", legal.path);
    await expect(own).toHaveAttribute("aria-current", "page");
    expect(errors).toEqual([]);
  });
}

test("the homepage's footer leads to the legal notice, and each language to the other", async ({ page }) => {
  await page.route("**/hero/**", (route) => route.abort());
  await page.goto("/");
  await page.getByRole("navigation", { name: "Legal" }).getByRole("link", { name: "Legal notice" }).click();
  await expect(page).toHaveURL(/\/legal-notice$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Legal notice");

  for (const legal of [pages[1], pages[0], pages[1]]) {
    const link = page.getByRole("main").getByRole("link", { name: legal.other.link });
    await expect(link).toHaveAttribute("lang", legal.other.lang);
    await expect(link).toHaveAttribute("hreflang", legal.other.lang);
    await link.click();
    await expect(page).toHaveURL(new RegExp(`${legal.other.path}$`));
    await expect(page.locator("html")).toHaveAttribute("lang", legal.other.lang);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(heading(legal.other.path));
  }

  // Home again from the wordmark (the loop ends on the French page).
  await page.getByRole("banner").getByRole("link", { name: "romulus, accueil" }).click();
  await expect(page).toHaveURL(/\/$/);
});

test("what the notice says about data holds: no cookies, nothing from another origin", async ({ page, context, baseURL }) => {
  // The homepage with its live editor (it loads once the page is idle on a desktop), then both notices.
  test.setTimeout(90_000);
  const origin = new URL(baseURL!).origin;
  const elsewhere: string[] = [];
  page.on("request", (request) => {
    const url = request.url();
    if (!url.startsWith("data:") && new URL(url).origin !== origin) elsewhere.push(url);
  });
  await page.goto("/");
  await expect(page.locator(".hero-stage.is-live")).toBeAttached({ timeout: 60_000 });
  for (const legal of pages) {
    await page.goto(legal.path);
    const [group, dark] = legal.lang === "fr" ? ["Thème", "Sombre"] : ["Theme", "Dark"];
    await page.getByRole("group", { name: group }).getByRole("radio", { name: dark }).check();
    await expect(page.locator("html")).toHaveClass(/\bdark\b/);
  }
  expect(elsewhere).toEqual([]);
  expect(await context.cookies()).toEqual([]);
});

test("the sitemap lists both notices", async ({ request }) => {
  const sitemap = await (await request.get("/sitemap.xml")).text();
  for (const legal of pages) expect(sitemap).toContain(`<loc>https://romulus.lupidae.com${legal.path}</loc>`);
});

for (const legal of pages) {
  for (const colorScheme of ["light", "dark"] as const) {
    test(`axe finds no accessibility violations on ${legal.path} (${colorScheme})`, async ({ page }) => {
      await page.emulateMedia({ colorScheme });
      await page.goto(legal.path);
      const results = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze();
      expect(results.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`)).toEqual([]);
    });
  }

  for (const width of [360, 390]) {
    test(`${legal.path} does not scroll sideways at ${width}px`, async ({ page }) => {
      await page.setViewportSize({ width, height: 800 });
      await page.goto(legal.path);
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      expect(overflow).toBe(0);
    });
  }
}
