import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import AxeBuilder from "@axe-core/playwright";
import type { Page } from "@playwright/test";

import { documents as rendered, slug } from "../tools/legal.mjs";
import { ANALYTICS, expect, test } from "./fixtures";

const repoFile = (path: string) => readFileSync(fileURLToPath(new URL(`../../${path}`, import.meta.url)), "utf8");

// The legal notice (LCEN), in French and in English: static pages, linked
// from the footer and from each other.
const pages = [
  {
    path: "/mentions-legales",
    lang: "fr",
    title: "Mentions légales – romulus",
    other: { path: "/legal-notice", link: "Read in English", lang: "en" },
    legalNav: "Informations légales",
    privacy: { text: "romulus.lupidae.com/fr/legal/confidentialite", href: "/fr/legal/confidentialite" },
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
    privacy: { text: "romulus.lupidae.com/legal/privacy", href: "/legal/privacy" },
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

// The footer's legal links, in each language (site/tools/legal.mjs writes the same on its pages).
const legalLinks = {
  en: {
    nav: "Legal",
    links: [
      ["Legal notice", "/legal-notice"],
      ["Terms", "/legal/terms"],
      ["Privacy", "/legal/privacy"],
      ["DPA", "/legal/dpa"],
    ],
  },
  fr: {
    nav: "Informations légales",
    links: [
      ["Mentions légales", "/mentions-legales"],
      ["Conditions générales", "/fr/legal/conditions"],
      ["Confidentialité", "/fr/legal/confidentialite"],
      ["DPA", "/legal/dpa"],
    ],
  },
} as const;

// The hosted service's documents, rendered from site/legal/*.md by site/tools/legal.mjs:
// the professional edition, in force, at the canonical addresses, and the full
// edition (consumers included), published under /legal/full/ for later.
// The Terms' own version: 1.1 added the founding-team offer (Section 7.11).
const terms = { meta: ["Version: 1.1", "Effective date: 24 October 2026"] };
const conditions = { meta: ["Version\u00a0: 1.1", "Date d'entrée en vigueur\u00a0: 24 octobre 2026"] };
// The DPA keeps its own version.
const dpa = { meta: ["Version: 1.0", "Effective date: 21 October 2026"] };
// The privacy policy's own version: 1.1 added the emails' retention, 1.2 the
// audience measurement and the signup source.
const privacy = { meta: ["Version: 1.2", "Effective date: 24 October 2026"] };
const confidentialite = { meta: ["Version\u00a0: 1.2", "Date d'entrée en vigueur\u00a0: 24 octobre 2026"] };
const documents = [
  {
    path: "/legal/terms",
    source: "terms-pro.en.md",
    lang: "en",
    inForce: true,
    h1: "romulus Terms of Service",
    current: "Terms",
    meta: terms.meta,
    other: { path: "/fr/legal/conditions", link: "Lire en français", lang: "fr" },
    heading: { id: "9-consumers-and-non-professionals", text: "9. Consumers and Non-professionals" },
    table: "Table: 7. Plans, prices and payment",
  },
  {
    path: "/fr/legal/conditions",
    source: "conditions-pro.fr.md",
    lang: "fr",
    inForce: true,
    h1: "Conditions générales d'utilisation et de vente de romulus",
    current: "Conditions générales",
    meta: conditions.meta,
    other: { path: "/legal/terms", link: "Read in English", lang: "en" },
    heading: { id: "9-consommateurs-et-non-professionnels", text: "9. Consommateurs et Non-professionnels" },
    table: "Tableau\u00a0: 7. Offres, prix et paiement",
  },
  {
    path: "/legal/privacy",
    source: "privacy-pro.en.md",
    lang: "en",
    inForce: true,
    h1: "romulus Privacy Policy",
    current: "Privacy",
    meta: privacy.meta,
    other: { path: "/fr/legal/confidentialite", link: "Lire en français", lang: "fr" },
    heading: { id: "8-cookies-and-similar-technologies", text: "8. Cookies and similar technologies" },
    table: "Table: Cookies",
  },
  {
    path: "/fr/legal/confidentialite",
    source: "confidentialite-pro.fr.md",
    lang: "fr",
    inForce: true,
    h1: "Politique de confidentialité de romulus",
    current: "Confidentialité",
    meta: confidentialite.meta,
    other: { path: "/legal/privacy", link: "Read in English", lang: "en" },
    heading: { id: "5-transferts-hors-de-l-union-europeenne", text: "5. Transferts hors de l'Union européenne" },
    table: "Tableau\u00a0: Cookies",
  },
  {
    path: "/legal/dpa",
    source: "dpa.en.md",
    lang: "en",
    inForce: true,
    h1: "romulus Data Processing Agreement",
    current: "DPA",
    meta: dpa.meta,
    other: null,
    heading: { id: "annex-3-subprocessors", text: "Annex 3: Subprocessors" },
    table: "Table: Annex 3: Subprocessors",
  },
  {
    path: "/legal/full/terms",
    source: "terms.en.md",
    lang: "en",
    inForce: false,
    h1: "romulus Terms of Service",
    current: null,
    meta: terms.meta,
    other: { path: "/fr/legal/full/conditions", link: "Lire en français", lang: "fr" },
    heading: { id: "9-consumer-rights", text: "9. Consumer rights" },
    table: "Table: 7. Plans, prices and payment",
  },
  {
    path: "/fr/legal/full/conditions",
    source: "conditions.fr.md",
    lang: "fr",
    inForce: false,
    h1: "Conditions générales d'utilisation et de vente de romulus",
    current: null,
    meta: conditions.meta,
    other: { path: "/legal/full/terms", link: "Read in English", lang: "en" },
    heading: { id: "9-droits-des-consommateurs", text: "9. Droits des Consommateurs" },
    table: "Tableau\u00a0: 7. Offres, prix et paiement",
  },
  {
    path: "/legal/full/privacy",
    source: "privacy.en.md",
    lang: "en",
    inForce: false,
    h1: "romulus Privacy Policy",
    current: null,
    meta: privacy.meta,
    other: { path: "/fr/legal/full/confidentialite", link: "Lire en français", lang: "fr" },
    heading: { id: "8-cookies-and-similar-technologies", text: "8. Cookies and similar technologies" },
    table: "Table: Cookies",
  },
  {
    path: "/fr/legal/full/confidentialite",
    source: "confidentialite.fr.md",
    lang: "fr",
    inForce: false,
    h1: "Politique de confidentialité de romulus",
    current: null,
    meta: confidentialite.meta,
    other: { path: "/legal/full/privacy", link: "Read in English", lang: "en" },
    heading: { id: "5-transferts-hors-de-l-union-europeenne", text: "5. Transferts hors de l'Union européenne" },
    table: "Tableau\u00a0: Cookies",
  },
] as const;

// What a page of the full edition says above the document: it is not in force, and what is.
const notInForce = {
  en: { text: "Not in force.", links: [["Terms of Service", "/legal/terms"], ["Privacy Policy", "/legal/privacy"]] },
  fr: {
    text: "Pas en vigueur.",
    links: [
      ["Conditions générales", "/fr/legal/conditions"],
      ["Politique de confidentialité", "/fr/legal/confidentialite"],
    ],
  },
} as const;

const ORIGIN = "https://romulus.lupidae.com";
const source = (name: string) =>
  readFileSync(fileURLToPath(new URL(`../legal/${name}`, import.meta.url)), "utf8");
const heading = (path: string) => (path === "/mentions-legales" ? "Mentions légales" : "Legal notice");

/** The value of a label / value row (the first one with that label: the publisher's comes before the host's). */
const fact = (page: Page, label: string) =>
  page
    .locator(".legal-facts > div")
    .filter({ has: page.locator("dt", { hasText: new RegExp(`^${label}$`) }) })
    .first()
    .locator("dd");

/** The footer's legal links: all four, in the page's language, the page's own marked as current (none for a page they do not link). */
async function expectLegalNav(page: Page, lang: "en" | "fr", current: string | null) {
  const { nav, links } = legalLinks[lang];
  const legal = page.getByRole("navigation", { name: nav });
  await expect(legal.getByRole("link")).toHaveText(links.map(([text]) => text));
  for (const [text, href] of links) await expect(legal.getByRole("link", { name: text, exact: true })).toHaveAttribute("href", href);
  await expect(legal.locator('[aria-current="page"]')).toHaveText(current ? [current] : []);
  // On a French page the DPA, in English, says so.
  if (lang === "fr") {
    const dpa = legal.getByRole("link", { name: "DPA", exact: true });
    await expect(dpa).toHaveAttribute("lang", "en");
    await expect(dpa).toHaveAttribute("hreflang", "en");
  }
}

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
    // The hosted service's privacy policy, in the notice's language.
    await expect(main.getByRole("link", { name: legal.privacy.text })).toHaveAttribute("href", legal.privacy.href);

    const head = (selector: string) => page.locator(selector).getAttribute("href");
    expect(await head('link[rel="canonical"]')).toBe(`${ORIGIN}${legal.path}`);
    expect(await head('link[rel="alternate"][hreflang="fr"]')).toBe(`${ORIGIN}/mentions-legales`);
    expect(await head('link[rel="alternate"][hreflang="en"]')).toBe(`${ORIGIN}/legal-notice`);

    // Its own footer link is marked as the page you are on.
    await expectLegalNav(page, legal.lang, heading(legal.path));
    expect(errors).toEqual([]);
  });
}

for (const doc of documents) {
  test(`${doc.path} is ${doc.source}, in its language and the site's chrome`, async ({ page }) => {
    test.setTimeout(60_000);
    const errors: string[] = [];
    page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
    page.on("pageerror", (e) => errors.push(String(e)));

    const response = await page.goto(doc.path);
    expect(response?.status()).toBe(200);
    await expect(page.locator("html")).toHaveAttribute("lang", doc.lang);
    await expect(page).toHaveTitle(`${doc.h1} – romulus`);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(doc.h1);
    await expect(page.getByRole("banner")).toBeVisible();
    await expect(page.getByRole("contentinfo")).toBeVisible();
    expect(await page.locator('meta[name="description"]').getAttribute("content")).toBeTruthy();

    // In force: indexed, nothing above the document. The full edition: noindex, and a notice
    // above it (chrome, not the document) that leads to the documents in force.
    const note = page.getByRole("main").getByRole("note");
    if (doc.inForce) {
      await expect(page.locator('meta[name="robots"]')).toHaveCount(0);
      await expect(note).toHaveCount(0);
    } else {
      expect(await page.locator('meta[name="robots"]').getAttribute("content")).toBe("noindex");
      const notice = notInForce[doc.lang];
      await expect(note).toContainText(notice.text);
      await expect(note.getByRole("link")).toHaveText(notice.links.map(([text]) => text));
      for (const [text, href] of notice.links) await expect(note.getByRole("link", { name: text })).toHaveAttribute("href", href);
      expect(await note.evaluate((el, h1) => el.compareDocumentPosition(h1) & Node.DOCUMENT_POSITION_FOLLOWING, await page.locator("h1").elementHandle())).toBeTruthy();
    }

    // Its version and effective date, as the document states them, under the title.
    const meta = page.locator(".legal-meta");
    for (const line of doc.meta) await expect(meta.getByText(line, { exact: true })).toBeVisible();

    const head = (selector: string) => page.locator(selector).getAttribute("href");
    expect(await head('link[rel="canonical"]')).toBe(`${ORIGIN}${doc.path}`);
    const main = page.getByRole("main");
    if (doc.other) {
      expect(await head(`link[rel="alternate"][hreflang="${doc.lang}"]`)).toBe(`${ORIGIN}${doc.path}`);
      expect(await head(`link[rel="alternate"][hreflang="${doc.other.lang}"]`)).toBe(`${ORIGIN}${doc.other.path}`);
      const link = main.getByRole("link", { name: doc.other.link });
      await expect(link).toHaveAttribute("href", doc.other.path);
      await expect(link).toHaveAttribute("lang", doc.other.lang);
      await expect(link).toHaveAttribute("hreflang", doc.other.lang);
    } else {
      // The DPA is in English only.
      await expect(page.locator('link[rel="alternate"]')).toHaveCount(0);
      await expect(main.getByRole("link", { name: /^(Lire en français|Read in English)$/ })).toHaveCount(0);
    }

    // Every heading of the source is there, with the id its text makes.
    const headings = [...source(doc.source).matchAll(/^#{2,3} (.+)$/gm)].map((m) => m[1]);
    expect(headings.length).toBeGreaterThan(3);
    const ids = await main.locator("h2, h3").evaluateAll((els) => els.map((el) => [el.id, el.textContent]));
    expect(ids.map(([, text]) => text!.replace(/\u00a0/g, " "))).toEqual(headings);
    expect(ids.map(([id]) => id)).toEqual(headings.map((text) => slug(text)));
    // (ids may start with a digit: valid in HTML and in a URL's fragment, not in a CSS #selector)
    await expect(page.locator(`[id="${doc.heading.id}"]`)).toHaveText(doc.heading.text);

    // Tables are labelled regions a keyboard reaches; bare addresses are links.
    const table = main.getByRole("region", { name: doc.table });
    await expect(table.getByRole("table")).toHaveCount(1);
    await expect(table).toHaveAttribute("tabindex", "0");
    await expect(main.getByRole("link", { name: "romulus@lupidae.com" }).first()).toHaveAttribute(
      "href",
      "mailto:romulus@lupidae.com",
    );

    await expectLegalNav(page, doc.lang, doc.current);
    expect(errors).toEqual([]);
  });
}

test("the tool and these tests know the same documents", () => {
  expect(rendered.map((d) => d.path)).toEqual(documents.map((d) => d.path));
  expect(slug("## 9. Consumer rights".slice(3))).toBe("9-consumer-rights");
  expect(slug("Données traitées : l'été")).toBe("donnees-traitees-l-ete");
});

test("the Terms in force are for professionals: no withdrawal form, section 9 for consumers and non-professionals", async ({ page }) => {
  for (const [path, line, nine] of [
    ["/legal/terms", "The Service is offered only to professionals", "9. Consumers and Non-professionals"],
    ["/fr/legal/conditions", "Le Service est réservé aux professionnels", "9. Consommateurs et Non-professionnels"],
  ]) {
    await page.goto(path);
    const main = page.getByRole("main");
    await expect(main.getByText(line).first()).toBeVisible();
    await expect(main.locator("blockquote")).toHaveCount(0);
    await expect(main.getByRole("heading", { level: 2, name: nine })).toBeVisible();
  }
});

test("the full edition's Terms say what the law asks for at checkout: the withdrawal form, the guarantee, the mediator", async ({ page }) => {
  await page.goto("/legal/full/terms");
  const main = page.getByRole("main");
  const form = main.locator("blockquote").filter({ hasText: "Model withdrawal form" });
  await expect(form).toBeVisible();
  await expect(form).toContainText("Date: ............");
  await expect(main.locator("blockquote").filter({ hasText: "legal guarantee of conformity" })).toBeVisible();
  // Lettered items stay one per line.
  await expect(main.locator(".legal-item").filter({ hasText: /^a\) use the Service in breach/ })).toBeVisible();
  await expect(main.getByRole("link", { name: "https://www.cm2c.net" }).first()).toHaveAttribute("href", "https://www.cm2c.net");

  // A section's address (the confirmation emails can point at one).
  await page.goto("/legal/full/terms#9-consumer-rights");
  await expect(page.getByRole("heading", { level: 2, name: "9. Consumer rights" })).toBeInViewport();
});

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

test("the legal notice's footer reaches the Terms, the privacy policy and the DPA in force, and the documents each other", async ({ page }) => {
  for (const [name, path, h1] of [
    ["Terms", "/legal/terms", "romulus Terms of Service"],
    ["Privacy", "/legal/privacy", "romulus Privacy Policy"],
    ["DPA", "/legal/dpa", "romulus Data Processing Agreement"],
  ]) {
    await page.goto("/legal-notice");
    await page.getByRole("navigation", { name: "Legal" }).getByRole("link", { name, exact: true }).click();
    await expect(page).toHaveURL(new RegExp(`${path}$`));
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(h1);
    await expect(page.getByRole("main").getByRole("note")).toHaveCount(0);
  }
  // The Terms the footer leads to are the professional edition.
  await page.goto("/legal-notice");
  await page.getByRole("navigation", { name: "Legal" }).getByRole("link", { name: "Terms", exact: true }).click();
  await expect(page.getByRole("main").getByText("The Service is offered only to professionals").first()).toBeVisible();

  // The Terms to their French version and back, then the French footer to the French privacy policy.
  await page.goto("/legal/terms");
  await page.getByRole("main").getByRole("link", { name: "Lire en français" }).click();
  await expect(page).toHaveURL(/\/fr\/legal\/conditions$/);
  await expect(page.locator("html")).toHaveAttribute("lang", "fr");
  await page.getByRole("navigation", { name: "Informations légales" }).getByRole("link", { name: "Confidentialité" }).click();
  await expect(page).toHaveURL(/\/fr\/legal\/confidentialite$/);
  await page.getByRole("main").getByRole("link", { name: "Read in English" }).click();
  await expect(page).toHaveURL(/\/legal\/privacy$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("romulus Privacy Policy");
});

test("the legal notice links the privacy policy in its own language", async ({ page }) => {
  for (const legal of pages) {
    await page.goto(legal.path);
    await page.getByRole("main").getByRole("link", { name: legal.privacy.text }).click();
    await expect(page).toHaveURL(new RegExp(`${legal.privacy.href}$`));
    await expect(page.locator("html")).toHaveAttribute("lang", legal.lang);
  }
});

test("what the notice says about data holds: no cookies, nothing from another origin but the visit count", async ({ page, context, baseURL }) => {
  // The homepage with its live editor (it loads once the page is idle on a desktop), then every legal page.
  test.setTimeout(90_000);
  const origin = new URL(baseURL!).origin;
  const elsewhere: string[] = [];
  page.on("request", (request) => {
    const url = request.url();
    if (!url.startsWith("data:") && new URL(url).origin !== origin) elsewhere.push(url);
  });
  await page.goto("/");
  await expect(page.locator(".hero-stage.is-live")).toBeAttached({ timeout: 60_000 });
  for (const legal of [...pages, ...documents]) {
    await page.goto(legal.path);
    const [group, dark] = legal.lang === "fr" ? ["Thème", "Sombre"] : ["Theme", "Dark"];
    await page.getByRole("group", { name: group }).getByRole("radio", { name: dark }).check();
    await expect(page.locator("html")).toHaveClass(/\bdark\b/);
  }
  // The homepage's audience measurement, from our own server (stubbed here):
  // its script, nothing sent from localhost (it counts on romulus.lupidae.com).
  expect(elsewhere).toEqual([`${ANALYTICS}/script.js`]);
  expect(await context.cookies()).toEqual([]);
});

test("every privacy policy says how long the emails sent are kept (1.1)", async ({ page }) => {
  const rows = [
    ["/legal/privacy", "Emails the Service sends", "30 days after it is sent; 90 days after delivery is given up"],
    ["/legal/full/privacy", "Emails the Service sends", "30 days after it is sent; 90 days after delivery is given up"],
    ["/fr/legal/confidentialite", "E-mails envoyés par le Service", "30 jours après son envoi ; 90 jours après l'abandon de sa remise"],
    ["/fr/legal/full/confidentialite", "E-mails envoyés par le Service", "30 jours après son envoi ; 90 jours après l'abandon de sa remise"],
  ];
  for (const [path, data, period] of rows) {
    await page.goto(path);
    // mail::outbox::purge: sent letters after 30 days, given-up ones after 90.
    const row = page.getByRole("row").filter({ has: page.getByRole("rowheader", { name: data, exact: true }) });
    await expect(row, path).toHaveCount(1);
    await expect(row.getByRole("cell"), path).toContainText(period);
  }
  expect(repoFile("crates/romulus-api/src/mail/outbox.rs")).toContain(
    "WHERE sent_at < now() - interval '30 days' OR failed_at < now() - interval '90 days'",
  );
});

test("every privacy policy says what the audience measurement and the signup source keep (1.2)", async ({ page }) => {
  const policies = [
    { path: "/legal/privacy", version: "Version: 1.2", rows: ["Signup source", "Audience measurement data"], kept: ["With the Account; deleted with it", "At most 25 months"] },
    { path: "/legal/full/privacy", version: "Version: 1.2", rows: ["Signup source", "Audience measurement data"], kept: ["With the Account; deleted with it", "At most 25 months"] },
    { path: "/fr/legal/confidentialite", version: "Version : 1.2", rows: ["Origine de l'inscription", "Données de mesure d'audience"], kept: ["Avec le Compte ; supprimée avec lui", "25 mois au plus"] },
    { path: "/fr/legal/full/confidentialite", version: "Version : 1.2", rows: ["Origine de l'inscription", "Données de mesure d'audience"], kept: ["Avec le Compte ; supprimée avec lui", "25 mois au plus"] },
  ];
  for (const policy of policies) {
    await page.goto(policy.path);
    await expect(page.getByRole("main"), policy.path).toContainText(policy.version);
    for (const [i, data] of policy.rows.entries()) {
      // Section 2 (what it is) and Section 6 (how long it is kept).
      const rows = page.getByRole("row").filter({ has: page.getByRole("rowheader", { name: data, exact: true }) });
      await expect(rows, `${policy.path}: ${data}`).toHaveCount(2);
      await expect(rows.nth(1).getByRole("cell"), policy.path).toContainText(policy.kept[i]);
    }
    // The one key the measurement keeps (web/src/features/marketing/analytics.ts), and its server.
    for (const key of ["romulus.analytics.once", "stats.lupidae.com", "ref_origin"])
      await expect(page.getByRole("main"), policy.path).toContainText(key);
    // The source rides in URLs: no storage key for it (it would need consent).
    await expect(page.getByRole("main"), policy.path).not.toContainText("romulus.source");
  }
  expect(repoFile("web/src/features/marketing/analytics.ts")).toContain('const ONCE_KEY = "romulus.analytics.once";');
  // Neither the homepage's app.js nor the app's source module keeps the source.
  expect(repoFile("site/public/app.js")).not.toMatch(/romulus\.source|setItem\([^)]*(utm|ref_origin|source)/);
  expect(repoFile("web/src/features/marketing/source.ts")).not.toMatch(/localStorage|sessionStorage/);
});

test("the sitemap lists the notices and the documents in force, never the full edition", async ({ request }) => {
  const sitemap = await (await request.get("/sitemap.xml")).text();
  for (const legal of [...pages, ...documents.filter((d) => d.inForce)])
    expect(sitemap).toContain(`<loc>${ORIGIN}${legal.path}</loc>`);
  for (const doc of documents.filter((d) => !d.inForce)) expect(sitemap).not.toContain(doc.path);
});

for (const legal of [...pages, ...documents]) {
  for (const colorScheme of ["light", "dark"] as const) {
    test(`axe finds no accessibility violations on ${legal.path} (${colorScheme})`, async ({ page }) => {
      // The documents are long: axe takes its time over every node's contrast.
      test.setTimeout(90_000);
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

// On a phone: the documents in force, and one page of the full edition (its notice).
for (const doc of documents.filter((d) => d.inForce || d.path === "/legal/full/terms")) {
  test(`axe finds no accessibility violations on ${doc.path} on a phone, its tables as cards`, async ({ page }) => {
    test.setTimeout(90_000);
    await page.setViewportSize({ width: 390, height: 800 });
    await page.goto(doc.path);
    // A row's cells, each under its column's name.
    const cell = page.getByRole("region", { name: doc.table }).locator("td[data-label]").first();
    expect(await cell.evaluate((td) => getComputedStyle(td, "::before").content)).toContain(
      (await cell.getAttribute("data-label"))!,
    );
    const results = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21aa"]).analyze();
    expect(results.violations.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`)).toEqual([]);
  });
}
