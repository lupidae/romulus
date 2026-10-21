#!/usr/bin/env node
// The hosted service's legal documents, from their one source (site/legal/*.md,
// the texts as approved) to what is published: a page each, in the legal
// notice's chrome, and each edition of the Terms in both languages as the PDFs
// the API attaches to its confirmation emails
// (crates/romulus-api/legal/<edition>-<version>-<lang>.pdf). It also writes the
// footer's legal links (`legalLinks`) into the site's other pages, so they are
// set in one place.
//
//   node tools/legal.mjs           write the pages; print the PDFs again when their source changed
//   node tools/legal.mjs --pdf     the same, and print the PDFs whatever the manifest says
//                                  (after a change to the print styles)
//   node tools/legal.mjs --check   exit 1 if a page differs from its source, or a PDF was not
//                                  printed from the current one (just site-check, just check)
//
// Markup only: the text is the source's, word for word. Raw HTML in a source
// is escaped, never passed through. A line break in a paragraph is kept (the
// lettered items "a) …", the model withdrawal form). PDF bytes are not
// reproducible (Chromium stamps them), so the check never prints again: it
// compares the PDFs and their sources with site/legal/manifest.json.
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const site = fileURLToPath(new URL("..", import.meta.url));
const repo = join(site, "..");
const pub = join(site, "public");
const sources = join(site, "legal");
const manifestFile = join(sources, "manifest.json");
const pdfDir = join(repo, "crates/romulus-api/legal");
export const ORIGIN = "https://romulus.lupidae.com";

/**
 * Every document, one entry each: its source in site/legal/, its language, where
 * it is published (public/<path>.html), its version in the other language
 * (`other`, a path here; none for the DPA), and for an edition of the Terms the
 * name its PDFs take (`pdf`: <pdf>-<version>-<lang>.pdf, the version read from
 * the source's "**Version:**" line).
 *
 * Two editions (site/legal/README.md): the professional one, in force, at the
 * canonical addresses; the full one (consumers included) under /legal/full/,
 * published but not in force (`inForce: false`: noindex, out of the sitemap,
 * a notice above the document pointing to the documents in force).
 */
export const documents = [
  {
    source: "terms-pro.en.md",
    lang: "en",
    path: "/legal/terms",
    other: "/fr/legal/conditions",
    pdf: "terms-pro",
    description:
      "The terms of service of romulus, the hosted service at romulus.lupidae.com, offered to professionals: plans and prices, payment and renewal, cancellation, data and liability.",
  },
  {
    source: "conditions-pro.fr.md",
    lang: "fr",
    path: "/fr/legal/conditions",
    other: "/legal/terms",
    pdf: "terms-pro",
    description:
      "Les conditions générales d’utilisation et de vente de romulus, le service hébergé sur romulus.lupidae.com réservé aux professionnels : offres et prix, paiement et renouvellement, résiliation, données et responsabilité.",
  },
  {
    source: "privacy-pro.en.md",
    lang: "en",
    path: "/legal/privacy",
    other: "/fr/legal/confidentialite",
    description:
      "How romulus, the hosted service at romulus.lupidae.com, processes personal data: what it collects and why, who receives it, how long it is kept, your rights and cookies.",
  },
  {
    source: "confidentialite-pro.fr.md",
    lang: "fr",
    path: "/fr/legal/confidentialite",
    other: "/legal/privacy",
    description:
      "Comment romulus, le service hébergé sur romulus.lupidae.com, traite les données personnelles : données collectées et finalités, destinataires, durées de conservation, vos droits, cookies.",
  },
  {
    source: "dpa.en.md",
    lang: "en",
    path: "/legal/dpa",
    description:
      "The data processing agreement (GDPR Article 28) between romulus and its business customers: instructions, security measures, breaches, subprocessors, transfers and audits.",
  },
  {
    source: "terms.en.md",
    lang: "en",
    path: "/legal/full/terms",
    other: "/fr/legal/full/conditions",
    pdf: "terms",
    inForce: false,
    description:
      "The full edition of the romulus terms of service, consumers included, which applies once romulus is sold to consumers too. Not in force.",
  },
  {
    source: "conditions.fr.md",
    lang: "fr",
    path: "/fr/legal/full/conditions",
    other: "/legal/full/terms",
    pdf: "terms",
    inForce: false,
    description:
      "L’édition complète des conditions générales de romulus, consommateurs compris, qui s’appliquera lorsque romulus sera aussi vendu aux consommateurs. Pas en vigueur.",
  },
  {
    source: "privacy.en.md",
    lang: "en",
    path: "/legal/full/privacy",
    other: "/fr/legal/full/confidentialite",
    inForce: false,
    description:
      "The full edition of the romulus privacy policy, consumers included, which applies once romulus is sold to consumers too. Not in force.",
  },
  {
    source: "confidentialite.fr.md",
    lang: "fr",
    path: "/fr/legal/full/confidentialite",
    other: "/legal/full/privacy",
    inForce: false,
    description:
      "L’édition complète de la politique de confidentialité de romulus, consommateurs compris, qui s’appliquera lorsque romulus sera aussi vendu aux consommateurs. Pas en vigueur.",
  },
];

/** Whether a document is in force (the default) or an edition published for later. */
export const inForce = (doc) => doc.inForce !== false;

/** public/<path>.html: Pages serves it at <path>, as serve.mjs does. */
export const outputOf = (doc) => join(pub, `${doc.path.slice(1)}.html`);

/**
 * The footer's legal links (`nav.footer-legal`) on every page of the site, in
 * the page's language; `lang` when the target is in another one. The link to
 * the page itself is marked aria-current, so a document the footer does not
 * link (an edition of the Terms not offered yet) is published all the same.
 */
export const legalLinks = {
  en: [
    { href: "/legal-notice", text: "Legal notice" },
    { href: "/legal/terms", text: "Terms" },
    { href: "/legal/privacy", text: "Privacy" },
    { href: "/legal/dpa", text: "DPA" },
  ],
  fr: [
    { href: "/mentions-legales", text: "Mentions légales" },
    { href: "/fr/legal/conditions", text: "Conditions générales" },
    { href: "/fr/legal/confidentialite", text: "Confidentialité" },
    { href: "/legal/dpa", text: "DPA", lang: "en" },
  ],
};

/**
 * The site's other pages whose footer gets the same legal links (the block
 * between `<nav class="footer-legal"` and `</nav>`). Not index.html: the
 * homepage's footer is the landing page's own, written by hand.
 */
export const staticPages = [
  { file: "404.html", lang: "en", path: null },
  { file: "legal-notice.html", lang: "en", path: "/legal-notice" },
  { file: "mentions-legales.html", lang: "fr", path: "/mentions-legales" },
];

/** The footer's legal links for a page in `lang` at `path`, indented by `indent` spaces. */
export function legalNav(lang, path, indent) {
  const pad = " ".repeat(indent);
  const links = legalLinks[lang].map(
    (l) =>
      `${pad}  <a href="${l.href}"${l.lang ? ` hreflang="${l.lang}" lang="${l.lang}"` : ""}${l.href === path ? ` aria-current="page"` : ""}>${escape(l.text)}</a>`,
  );
  return [`${pad}<nav class="footer-legal" aria-label="${words[lang].legal}">`, ...links, `${pad}</nav>`].join("\n");
}

const FOOTER_LEGAL = /^( *)<nav class="footer-legal" aria-label="[^"]*">\n[\s\S]*?\n\1<\/nav>/m;

/** A static page with its footer's legal links rewritten from `legalLinks`. */
export function withLegalNav(html, page) {
  const match = FOOTER_LEGAL.exec(html);
  if (!match) throw new Error(`${page.file}: no <nav class="footer-legal"> in its footer`);
  return html.slice(0, match.index) + legalNav(page.lang, page.path, match[1].length) + html.slice(match.index + match[0].length);
}

// The chrome's words, as legal-notice.html and mentions-legales.html write them.
const words = {
  en: {
    skip: "Skip to content",
    home: "romulus, home",
    github: "romulus on GitHub",
    signIn: "Sign in",
    start: "Start free",
    madeBy: `Made by <a href="https://github.com/lupidae">lupidae</a>, with its twin, <a href="https://remus.lupidae.com">remus</a>.`,
    footer: "Footer",
    selfHost: "Self-host docs",
    security: "Security policy",
    contact: "Contact",
    theme: "Theme",
    system: "System",
    light: "Light",
    dark: "Dark",
    license: `romulus is open source, under the <a href="https://github.com/lupidae/romulus/blob/main/LICENSE">AGPL-3.0</a>.`,
    legal: "Legal",
    switchTo: "Read in English",
    table: "Table:",
    // Above a document not in force (`inForce: false`): chrome, not part of it.
    notInForce: `<strong>Not in force.</strong> This edition of the documents applies once romulus is sold to consumers too. The documents in force are the <a href="/legal/terms">Terms of Service</a> and the <a href="/legal/privacy">Privacy Policy</a>.`,
    page: (n, total) => `Page ${n} of ${total}`,
    version: "Version",
  },
  fr: {
    skip: "Aller au contenu",
    home: "romulus, accueil",
    github: "romulus sur GitHub",
    signIn: "Se connecter",
    start: "Commencer gratuitement",
    madeBy: `Fait par <a href="https://github.com/lupidae">lupidae</a>, avec son jumeau, <a href="https://remus.lupidae.com">remus</a>.`,
    footer: "Pied de page",
    selfHost: "Auto-hébergement",
    security: "Politique de sécurité",
    contact: "Contact",
    theme: "Thème",
    system: "Système",
    light: "Clair",
    dark: "Sombre",
    license: `romulus est un logiciel libre, sous licence <a href="https://github.com/lupidae/romulus/blob/main/LICENSE">AGPL-3.0</a>.`,
    legal: "Informations légales",
    switchTo: "Lire en français",
    table: "Tableau\u00a0:",
    notInForce: `<strong>Pas en vigueur.</strong> Cette édition des documents s’appliquera lorsque romulus sera aussi vendu aux consommateurs. Les documents en vigueur sont les <a href="/fr/legal/conditions">Conditions générales</a> et la <a href="/fr/legal/confidentialite">Politique de confidentialité</a>.`,
    page: (n, total) => `Page ${n} sur ${total}`,
    version: "Version",
  },
};

export const escape = (text) =>
  String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

/** A heading's id: lowercase, accents stripped, every run of other characters one "-" ("9. Consumer rights" → "9-consumer-rights"). */
export const slug = (text) =>
  text
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");

/** The `**Version:** X` line of a document (French: `**Version :** X`). */
export function versionOf(markdown, name = "document") {
  const match = /^\*\*Version\s*:\*\*\s*(\S+)\s*$/m.exec(markdown);
  if (!match) throw new Error(`${name}: no "**Version:** X" line`);
  return match[1];
}

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** The inline theme script, exactly as index.html has it: the CSP allows it by hash. */
function themeScript(index) {
  const match = /\n(<script>\n[\s\S]*?<\/script>)\n/.exec(index);
  if (!match) throw new Error("index.html: no inline theme script");
  return match[1];
}

// A line of a paragraph that is an item: "a) …", "1° …".
const ITEM = /^([a-z]\)|\d+°)(?=\s)/;
// A clause's own number at the start of its paragraph: "3.1. ", "1. ".
const CLAUSE = /^(\d+(?:\.\d+)*\.)\s/;
const TAGS = ["strong", "em", "code", "a"];
const balanced = (html) =>
  TAGS.every((t) => html.split(new RegExp(`<${t}[ >]`)).length === html.split(`</${t}>`).length);

/** The document's HTML: its title, its version lines, and the body, in the page's markup. */
export function renderDocument(marked, markdown, doc) {
  const w = words[doc.lang];
  const tokens = marked.lexer(markdown);
  const at = (i, type) => tokens[i]?.type === type;
  if (!at(0, "heading") || tokens[0].depth !== 1) throw new Error(`${doc.source}: the first line must be its "# title"`);
  const title = tokens[0].text;
  // The version and effective date: the first paragraph, one "**Label:** value" per line.
  const metaAt = tokens.findIndex((t) => t.type === "paragraph");
  const meta = tokens[metaAt];
  if (!meta || !/^\*\*Version\s*:\*\*/.test(meta.raw)) throw new Error(`${doc.source}: the paragraph after the title must be its version lines`);
  const body = tokens.slice(metaAt + 1);

  // French spacing: a non-breaking space where French puts one, so ";", ":", "?",
  // "!" and "»" never start a line, nor "«" end one. Text only (never a URL or code).
  if (doc.lang === "fr")
    marked.walkTokens(tokens, (token) => {
      if (token.type === "text" && !token.tokens)
        token.text = token.text.replace(/ ([:;!?»%])/g, "\u00a0$1").replace(/« /g, "«\u00a0");
    });

  const ids = new Set();
  let label = title; // what a table is about: the heading (or bold line) above it
  const renderer = {
    heading({ tokens: inline, depth }) {
      const text = this.parser.parseInline(inline, this.parser.textRenderer);
      if (depth === 1) return `<h1>${this.parser.parseInline(inline)}</h1>\n`;
      let id = slug(text);
      for (let n = 2; ids.has(id); n++) id = `${slug(text)}-${n}`;
      ids.add(id);
      label = text;
      return `<h${depth} id="${id}">${this.parser.parseInline(inline)}</h${depth}>\n`;
    },
    paragraph({ tokens: inline }) {
      if (inline.length === 1 && inline[0].type === "strong")
        label = this.parser.parseInline(inline[0].tokens, this.parser.textRenderer);
      let html = this.parser.parseInline(inline);
      const clause = CLAUSE.exec(html);
      if (clause) html = `<span class="legal-clause">${clause[1]}</span>${html.slice(clause[1].length)}`;
      // Lines that are items hang under their marker; the <br> stays for reading modes.
      const lines = html.split("<br>");
      if (lines.length > 1 && lines.some((l) => ITEM.test(l)) && lines.every(balanced))
        return `<p class="legal-lines">${lines
          .map((l) =>
            ITEM.test(l)
              ? `<span class="legal-item">${l.replace(ITEM, '<span class="legal-mark">$1</span>')}</span>`
              : `<span class="legal-line">${l}</span>`,
          )
          .join("<br>")}</p>\n`;
      return `<p>${html}</p>\n`;
    },
    // A table sits in a frame of its own that scrolls when the table is wider,
    // never the page; the frame is a labelled region the keyboard can reach. The
    // first column heads its row. On a phone each row becomes a card, its cells
    // labelled by their column (data-label): explicit roles keep the table's
    // semantics when CSS changes its display.
    table(token) {
      const align = (cell) => (cell.align ? ` align="${cell.align}"` : "");
      const inline = (cell) => this.parser.parseInline(cell.tokens);
      const plain = (cell) => this.parser.parseInline(cell.tokens, this.parser.textRenderer).trim();
      const head = token.header
        .map((cell) =>
          plain(cell)
            ? `<th scope="col" role="columnheader"${align(cell)}>${inline(cell)}</th>`
            : `<td role="cell"></td>`,
        )
        .join("");
      const rows = token.rows
        .map(
          (row) =>
            `<tr role="row">${row
              .map((cell, i) => {
                if (i === 0) return `<th scope="row" role="rowheader"${align(cell)}>${inline(cell)}</th>`;
                const name = plain(token.header[i]);
                return `<td role="cell"${name ? ` data-label="${escape(name)}"` : ""}${align(cell)}>${inline(cell)}</td>`;
              })
              .join("")}</tr>\n`,
        )
        .join("");
      return `<div class="legal-table" role="region" aria-label="${escape(`${w.table} ${label}`)}" tabindex="0" data-columns="${token.header.length}">
<table role="table">
<thead role="rowgroup">
<tr role="row">${head}</tr>
</thead>
<tbody role="rowgroup">
${rows}</tbody>
</table>
</div>
`;
    },
    // Never pass raw HTML through: it is shown as the text it is.
    html({ text, block }) {
      return block ? `<p>${escape(text.trim())}</p>\n` : escape(text);
    },
  };
  marked.use({ renderer });
  const heading = marked.parser([tokens[0]]).trim();
  const metaLines = marked
    .parser([meta])
    .trim()
    .replace(/^<p>|<\/p>$/g, "")
    .split("<br>");
  return { title, heading, metaLines, body: marked.parser(body) };
}

const indent = (html, n) =>
  html
    .split("\n")
    .map((line) => (line ? " ".repeat(n) + line : line))
    .join("\n");

/** The whole page: the legal notice's head, header and footer around the document. */
export function renderPage(marked, markdown, doc, index) {
  const w = words[doc.lang];
  const { title, heading, metaLines, body } = renderDocument(marked, markdown, doc);
  const other = doc.other && documents.find((d) => d.path === doc.other);
  const alternates = other
    ? [doc, other]
        .sort((a, b) => a.lang.localeCompare(b.lang))
        .reverse()
        .map((d) => `<link rel="alternate" hreflang="${d.lang}" href="${ORIGIN}${d.path}">`)
        .join("\n") + "\n"
    : "";
  const switchLink = other
    ? `\n      <a class="legal-lang" href="${other.path}" hreflang="${other.lang}" lang="${other.lang}">${words[other.lang].switchTo}</a>`
    : "";
  return `<!doctype html>
<html lang="${doc.lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>${escape(title)} – romulus</title>
<meta name="description" content="${escape(doc.description)}">
${inForce(doc) ? "" : `<meta name="robots" content="noindex">\n`}<link rel="canonical" href="${ORIGIN}${doc.path}">
${alternates}<meta name="color-scheme" content="light dark">
<meta name="theme-color" content="#fbfaf7" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#121110" media="(prefers-color-scheme: dark)">
${themeScript(index)}
<link rel="preload" href="/fonts/inter-latin-opsz-normal.woff2" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="/tokens.css">
<link rel="stylesheet" href="/styles.css">
<link rel="icon" href="/favicon.ico" sizes="32x32">
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="/apple-touch-icon.png">
<link rel="manifest" href="/site.webmanifest">
</head>
<body>
<!-- Generated by site/tools/legal.mjs from site/legal/${doc.source}: edit the source, then run it. -->
<svg class="sprite" aria-hidden="true" focusable="false">
  <symbol id="i-mark" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" class="mark-bg"/><path d="M11.5 23V13.75A4.75 4.75 0 0 1 16.25 9H17" fill="none" stroke-width="2.75" stroke-linecap="round" class="mark-fg-stroke"/><circle cx="21.5" cy="9" r="2.4" class="mark-fg"/></symbol>
  <symbol id="i-sun" viewBox="0 0 24 24"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M12 18a6 6 0 1 0 0-12a6 6 0 0 0 0 12m10-6h1M12 2V1m0 22v-1m8-2l-1-1m1-15l-1 1M4 20l1-1M4 4l1 1m-4 7h1"/></symbol>
  <symbol id="i-moon" viewBox="0 0 24 24"><path fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5" d="M3 11.507a9.493 9.493 0 0 0 18 4.219c-8.507 0-12.726-4.22-12.726-12.726A9.49 9.49 0 0 0 3 11.507"/></symbol>
  <symbol id="i-computer" viewBox="0 0 24 24"><g fill="none" stroke="currentColor" stroke-width="1.5"><path stroke-linecap="round" stroke-linejoin="round" d="M2 21h15m4 0h1"/><path d="M2 16.4V3.6a.6.6 0 0 1 .6-.6h18.8a.6.6 0 0 1 .6.6v12.8a.6.6 0 0 1-.6.6H2.6a.6.6 0 0 1-.6-.6Z"/></g></symbol>
  <symbol id="i-github" viewBox="0 0 24 24"><g fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" stroke-width="1.5"><path d="M16 22.027v-2.87a3.37 3.37 0 0 0-.94-2.61c3.14-.35 6.44-1.54 6.44-7a5.44 5.44 0 0 0-1.5-3.75a5.07 5.07 0 0 0-.09-3.77s-1.18-.35-3.91 1.48a13.4 13.4 0 0 0-7 0c-2.73-1.83-3.91-1.48-3.91-1.48A5.07 5.07 0 0 0 5 5.797a5.44 5.44 0 0 0-1.5 3.78c0 5.42 3.3 6.61 6.44 7a3.37 3.37 0 0 0-.94 2.58v2.87"/><path d="M9 20.027c-3 .973-5.5 0-7-3"/></g></symbol>
</svg>

<a class="skip-link" href="#main">${w.skip}</a>

<header class="site-header" data-header>
  <div class="container header-inner">
    <a class="wordmark" href="/" aria-label="${w.home}">
      <svg class="mark" aria-hidden="true"><use href="#i-mark"/></svg>
      <span>romulus</span>
    </a>
    <div class="header-actions">
      <a class="button ghost header-github" href="https://github.com/lupidae/romulus" aria-label="${w.github}"><svg class="icon" aria-hidden="true"><use href="#i-github"/></svg><span>GitHub</span></a>
      <a class="button ghost" href="/app/signin">${w.signIn}</a>
      <a class="button secondary header-cta" href="/app/signin">${w.start}</a>
    </div>
  </div>
</header>

<main id="main" class="container legal">
${inForce(doc) ? "" : `  <p class="legal-status" role="note">${w.notInForce}</p>\n`}  <article class="legal-inner legal-doc">
    ${heading}
    <p class="legal-meta">
${metaLines.map((line) => `      <span>${line}</span>`).join("\n")}
      <span class="legal-print">${ORIGIN.replace("https://", "")}${doc.path}</span>${switchLink}
    </p>
${indent(body.trimEnd(), 4)}
  </article>
</main>

<footer class="site-footer">
  <div class="container footer-inner">
    <div class="footer-brand">
      <a class="wordmark" href="/" aria-label="${w.home}">
        <svg class="mark" aria-hidden="true"><use href="#i-mark"/></svg>
        <span>romulus</span>
      </a>
      <p>${w.madeBy}</p>
    </div>
    <nav class="footer-links" aria-label="${w.footer}">
      <a href="https://github.com/lupidae/romulus">GitHub</a>
      <a href="https://github.com/lupidae/romulus/tree/main/docs/self-host">${w.selfHost}</a>
      <a href="https://github.com/lupidae/romulus/blob/main/SECURITY.md">${w.security}</a>
      <a href="mailto:romulus@lupidae.com">${w.contact}</a>
      <a href="/app/signin">${w.signIn}</a>
    </nav>
    <fieldset class="theme-switch" data-theme-switch>
      <legend class="sr-only">${w.theme}</legend>
      <label title="${w.system}"><input type="radio" name="theme" value="system" checked><svg class="icon" aria-hidden="true"><use href="#i-computer"/></svg><span class="sr-only">${w.system}</span></label>
      <label title="${w.light}"><input type="radio" name="theme" value="light"><svg class="icon" aria-hidden="true"><use href="#i-sun"/></svg><span class="sr-only">${w.light}</span></label>
      <label title="${w.dark}"><input type="radio" name="theme" value="dark"><svg class="icon" aria-hidden="true"><use href="#i-moon"/></svg><span class="sr-only">${w.dark}</span></label>
    </fieldset>
  </div>
  <div class="container footer-base">
    <p>${w.license}</p>
${legalNav(doc.lang, doc.path, 4)}
  </div>
</footer>

<script type="module" src="/app.js"></script>
</body>
</html>
`;
}

/** marked, set up for these documents: GitHub's Markdown (tables, bare URLs and addresses as links), line breaks kept. */
async function loadMarked() {
  const { Marked } = await import("marked");
  return () => new Marked({ gfm: true, breaks: true });
}

/**
 * Every page this tool writes, as it should be: { file, html, from }. The
 * documents' pages need marked (`newMarked`); the other pages' footers do not.
 */
async function renderAll(newMarked) {
  const index = await readFile(join(pub, "index.html"), "utf8");
  const others = await Promise.all(
    staticPages.map(async (page) => {
      const file = join(pub, page.file);
      return { file, html: withLegalNav(await readFile(file, "utf8"), page), from: "legalLinks in site/tools/legal.mjs" };
    }),
  );
  if (!newMarked) return others;
  const pages = await Promise.all(
    documents.map(async (doc) => {
      const markdown = await readFile(join(sources, doc.source), "utf8");
      return { file: outputOf(doc), html: renderPage(newMarked(), markdown, doc, index), from: `site/legal/${doc.source}` };
    }),
  );
  return [...pages, ...others];
}

/** The PDFs the sources call for: each edition of the Terms, one per language, named by its version. */
async function expectedPdfs() {
  return Promise.all(
    documents
      .filter((doc) => doc.pdf)
      .map(async (doc) => {
        const markdown = await readFile(join(sources, doc.source), "utf8");
        const version = versionOf(markdown, doc.source);
        const file = join(pdfDir, `${doc.pdf}-${version}-${doc.lang}.pdf`);
        return { doc, version, file, sourceSha: sha256(markdown) };
      }),
  );
}

const rel = (file) => relative(repo, file).split("\\").join("/");

async function readManifest() {
  try {
    return JSON.parse(await readFile(manifestFile, "utf8"));
  } catch {
    return { pdfs: [] };
  }
}

/** What is wrong with one PDF, in words: null when it was printed from its current source. */
async function pdfProblem(want, manifest) {
  const entry = manifest.pdfs.find((p) => p.pdf === rel(want.file));
  if (!entry) return `${rel(want.file)}: not in site/legal/manifest.json (version ${want.version} of ${want.doc.source})`;
  if (!existsSync(want.file)) return `${rel(want.file)}: missing`;
  if (entry.source !== `site/legal/${want.doc.source}`)
    return `${rel(want.file)}: printed from ${entry.source}, not site/legal/${want.doc.source}`;
  if (entry.source_sha256 !== want.sourceSha) return `${rel(want.file)}: printed from another text of site/legal/${want.doc.source}`;
  if (entry.page !== want.doc.path) return `${rel(want.file)}: printed from ${entry.page ?? "another page"}, not ${want.doc.path}`;
  if (sha256(await readFile(want.file)) !== entry.pdf_sha256)
    return `${rel(want.file)}: its bytes are not the ones the manifest recorded`;
  return null;
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

/** Prints the Terms from their pages, served by serve.mjs on a free port (so fonts, CSS and headers are the real ones). */
async function printPdfs(wanted) {
  const { chromium } = await import("@playwright/test");
  const tokens = await readFile(join(pub, "tokens.css"), "utf8");
  const faint = /--faint-foreground:\s*(#[0-9a-f]{3,8})/i.exec(tokens)?.[1] ?? "gray";
  const port = await freePort();
  const server = spawn(process.execPath, [join(site, "serve.mjs"), String(port)], { stdio: ["ignore", "ignore", "inherit"] });
  const base = `http://localhost:${port}`;
  try {
    for (let i = 0; ; i++) {
      try {
        if ((await fetch(`${base}/`)).ok) break;
      } catch {}
      if (i > 100) throw new Error("legal: serve.mjs did not start");
      await new Promise((r) => setTimeout(r, 50));
    }
    const browser = await chromium.launch();
    try {
      const context = await browser.newContext({ colorScheme: "light", reducedMotion: "reduce" });
      const page = await context.newPage();
      const printed = [];
      for (const want of wanted) {
        const w = words[want.doc.lang];
        const response = await page.goto(`${base}${want.doc.path}`, { waitUntil: "load" });
        if (!response?.ok()) throw new Error(`legal: ${want.doc.path} answered ${response?.status()}`);
        const title = await page.locator("h1").innerText();
        await page.evaluate(async (pdfTitle) => {
          await Promise.all(['400 1em "Inter Variable"', '600 1em "Inter Variable"'].map((f) => document.fonts.load(f)));
          await document.fonts.ready;
          document.title = pdfTitle;
        }, `${title} · ${w.version} ${want.version}`);
        // The footer is drawn apart from the page: no web fonts there, a system sans.
        const small = `box-sizing: border-box; width: 100%; padding: 0 18mm; display: flex; justify-content: space-between; font-family: Helvetica, Arial, sans-serif; font-size: 7.5pt; color: ${faint};`;
        const pdf = await page.pdf({
          format: "A4",
          margin: { top: "16mm", right: "18mm", bottom: "18mm", left: "18mm" },
          displayHeaderFooter: true,
          headerTemplate: "<span></span>",
          footerTemplate: `<div style="${small}"><span>${escape(title)} · ${w.version} ${escape(want.version)}</span><span>${w.page('<span class="pageNumber"></span>', '<span class="totalPages"></span>')}</span></div>`,
          outline: true,
          tagged: true,
        });
        await mkdir(dirname(want.file), { recursive: true });
        await writeFile(want.file, pdf);
        printed.push({
          pdf: rel(want.file),
          source: `site/legal/${want.doc.source}`,
          page: want.doc.path,
          version: want.version,
          source_sha256: want.sourceSha,
          pdf_sha256: sha256(pdf),
          bytes: pdf.length,
        });
        console.log(`legal: printed ${rel(want.file)} (${Math.round(pdf.length / 1024)} KB)`);
      }
      return printed;
    } finally {
      await browser.close();
    }
  } finally {
    server.kill();
  }
}

async function writeManifest(pdfs) {
  const text = `${JSON.stringify(
    {
      $comment:
        "Written by site/tools/legal.mjs: the Terms PDFs the API attaches, each with the source it was printed from. `node site/tools/legal.mjs --check` compares the hashes; do not edit by hand.",
      pdfs,
    },
    null,
    2,
  )}\n`;
  await writeFile(manifestFile, text);
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const check = process.argv.includes("--check");
  let newMarked = null;
  try {
    newMarked = await loadMarked();
  } catch (error) {
    if (error?.code !== "ERR_MODULE_NOT_FOUND") throw error;
    if (!check) {
      console.error("legal: marked is not installed: run `npm ci --prefix site` first");
      process.exit(1);
    }
  }

  if (check) {
    const problems = [];
    if (!newMarked) {
      // `just check` runs without the site's packages: the PDFs and the footers are still checked.
      console.warn(
        "legal: WARNING marked is not installed (npm ci --prefix site): the documents' pages were NOT re-rendered; only the PDFs and the footers are checked",
      );
    }
    for (const { file, html, from } of await renderAll(newMarked)) {
      const committed = existsSync(file) ? await readFile(file, "utf8") : null;
      if (committed === null) problems.push(`${rel(file)}: missing (from ${from})`);
      else if (committed !== html) problems.push(`${rel(file)}: differs from what ${from} makes of it`);
    }
    const manifest = await readManifest();
    for (const want of await expectedPdfs()) {
      const problem = await pdfProblem(want, manifest);
      if (problem) problems.push(problem);
    }
    // The sitemap lists the documents in force, and never one that is not (noindex).
    const sitemap = await readFile(join(pub, "sitemap.xml"), "utf8");
    for (const doc of documents) {
      const listed = sitemap.includes(`<loc>${ORIGIN}${doc.path}</loc>`);
      if (inForce(doc) && !listed) problems.push(`site/public/sitemap.xml: does not list ${doc.path}`);
      if (!inForce(doc) && listed) problems.push(`site/public/sitemap.xml: lists ${doc.path}, which is not in force (noindex)`);
    }
    if (problems.length) {
      console.error(
        `legal: the published documents are not made from site/legal/:\n${problems.map((p) => `  - ${p}`).join("\n")}\nRun \`node site/tools/legal.mjs\` (just site-legal) and commit what it writes.`,
      );
      process.exit(1);
    }
    console.log(`legal: ${newMarked ? "the pages, " : ""}the footers and the Terms PDFs match site/legal/ and legal.mjs`);
  } else {
    let written = 0;
    for (const { file, html } of await renderAll(newMarked)) {
      const committed = existsSync(file) ? await readFile(file, "utf8") : null;
      if (committed === html) continue;
      await mkdir(dirname(file), { recursive: true });
      await writeFile(file, html);
      written++;
      console.log(`legal: wrote ${rel(file)}`);
    }
    if (!written) console.log("legal: the pages are up to date");
    // Print what is stale (or everything, with --pdf); the manifest keeps the others.
    const manifest = await readManifest();
    const wanted = await expectedPdfs();
    const all = process.argv.includes("--pdf");
    const stale = [];
    for (const want of wanted) if (all || (await pdfProblem(want, manifest))) stale.push(want);
    if (stale.length) {
      const printed = await printPdfs(stale);
      const entry = (want) =>
        printed.find((p) => p.pdf === rel(want.file)) ?? manifest.pdfs.find((p) => p.pdf === rel(want.file));
      await writeManifest(wanted.map(entry));
    } else {
      console.log("legal: the Terms PDFs are up to date (--pdf prints them again)");
    }
  }
}
