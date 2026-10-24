# site/: the romulus homepage

> This directory is the marketing homepage of lupidae's hosted service
> (romulus.lupidae.com), deployed to lupidae's Cloudflare account. Running
> or self-hosting romulus does not need it.

A static page, no framework: `public/` is exactly what gets served, and Pages
has no build step. It is the app's brand, not a lookalike: `public/tokens.css`
and the two fonts are copied from `web/` by a script, and a check fails when
they drift. Its hero is the app itself, running in the browser (below): built
from `web/` and `crates/romulus-wasm` into `public/hero/`, and committed.

```
public/            what Cloudflare Pages serves
  index.html       the page; the hero's static window is HTML, CSS and SVG; the plans' JSON (#plans-data)
  mentions-legales.html, legal-notice.html   the legal notice (LCEN), in French and in English
  legal/terms.html, legal/privacy.html, legal/dpa.html,
  fr/legal/conditions.html, fr/legal/confidentialite.html
                   the hosted service's terms, privacy policy and DPA in force (the professional
                   edition), rendered by tools/legal.mjs: do not edit
  legal/full/, fr/legal/full/   the full edition (consumers included), published, not in force
  compare/atlas.html, compare/bytebase.html, guides/test-prisma-migrations.html,
  guides/postgres-schema-diff.html
                   the comparisons and guides (/compare/…, /guides/…): the homepage's header and
                   footer, by hand
  img/             their screenshots of romulus (WebP)
  og/              their share images, rendered by tools/assets.mjs --pages
  styles.css       layout and the marketing type scale (colors only from tokens)
  tokens.css       generated from web/src/styles/globals.css: do not edit
  app.js           header hairline, the launch welcome, hero playback, loading the live hero, copy buttons
                   (announced in a status region), theme switch
  hero/            the live hero, built by tools/hero.mjs: do not edit
  _headers         CSP (with the inline theme script's hash), HSTS, caching
  404.html, robots.txt, sitemap.xml, site.webmanifest, og.png, icons
tools/sync.mjs     copies tokens and fonts from web/ (--check for CI)
tools/hero.mjs     builds public/hero/ (--check: is it built from the current sources?)
tools/assets.mjs   renders og.png (tools/og.html + the hero window), the icons, and each comparison's
                   and guide's og/<page>.png (tools/og-page.html: kicker, title, its picture or run)
tools/pricing.mjs  renders the plan cards from the page's #plans-data (--check: do they match?)
tools/legal.mjs    renders legal/*.md into the legal pages and prints the Terms PDFs the API attaches (--check)
legal/             the legal documents' sources, as approved (legal/README.md), and the PDFs' manifest.json
serve.mjs          a local server that behaves like Pages (_headers, 404, compression)
e2e/               Playwright + axe
wrangler.toml      the Cloudflare Pages project
```

## Run and check

```bash
npm ci --prefix site          # once (Playwright and axe); web/ needs `npm ci` too for sync
just site                     # serve on $ROMULUS_SITE_PORT (scripts/dev-env, slot + 7)
just site-check               # tokens/fonts in sync, the hero and the legal pages built, then Playwright + axe (CI: .github/workflows/site.yml)
just site-gen                 # after a token change in web/, or a change to the hero or mark (rebuilds the live hero)
just site-hero                # the live hero only, after a change to web/src/hero, the components it reuses, or romulus-wasm
just site-legal               # the legal pages and the Terms PDFs, after editing site/legal/
just site-lighthouse          # mobile and desktop reports in site/lighthouse/
```

The CSP allows one inline script by hash: if you edit the theme script in
`index.html` (it must stay identical in `404.html` and the two legal notice
pages; `tools/legal.mjs` copies it into the legal documents' pages, so run
it), update the hash in `_headers`; the e2e test says so when they
disagree.

## Deploy (Cloudflare Pages)

Not deployed by CI. With `npx wrangler login` done once (or
`CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` in the environment):

```bash
just site-deploy              # wrangler@4.151.0 pages deploy --branch main: production, Function included
```

Or connect the repository in the Pages dashboard with these settings:

| setting | value |
|---|---|
| Framework preset | None |
| Build command | *(empty)* |
| Build output directory | `site/public` |
| Root directory | `/` |

Then attach the custom domain `romulus.lupidae.com` in the project's Custom
domains tab (Pages has no wrangler.toml setting for it).

**Links into the app** are root-relative (`/app/signin`, and `/app/…`,
`/api/…` wherever they appear): one origin. The Pages Function
(`functions/_middleware.js`, logic and tests in `edge/`) proxies `/app` and
`/api` to the API, websockets included, and `public/_routes.json` keeps it off
every other path, so the homepage stays static. The API's origin and its edge
key are set in the Pages project's settings. `_headers`
applies to static files only (Pages never applies it to a Function's
responses): the homepage's CSP does not reach the app, whose headers come
from the API.

## Decisions

- Hosting: Cloudflare Pages. Canonical URL `https://romulus.lupidae.com/`
  (in the head of `index.html`, `robots.txt`, `sitemap.xml`; the custom
  domain is attached in the Pages dashboard).
- Subscribing leads: "Choose a plan" (header, hero, closing, primary) goes to
  the pricing, where each paid plan's button subscribes to it; "Start free"
  (hero, closing, the Free card) goes to the plans' `signup` (`/app/signin`);
  "Talk to us" opens `mailto:romulus@lupidae.com` (closing, the Business
  card, the footer's Contact). The returning visitor's "Sign in" is in the
  header and, quietest (a ghost button), third in the hero's row.
- The push snippet follows the spec: `POST /api/databases/<id>/versions` with
  the database's bearer token, remus JSON as the body.
- The agents section (`#mcp`) shows a session as a client would: the tools'
  names, arguments and answers are the real ones
  (`crates/romulus-api/src/mcp/tools`), shortened to the lines that matter
  (an ellipsis marks a cut inside a line or a listing), and
  the test's row count is in the range a Small test seeds for the hero's
  billing schema. Change it when the tools' answers change. Its command is Claude
  Code's, with the hosted URL; docs/mcp.md has the other clients.
- The agents section also has a sentence to paste into any agent ("Tell your
  agent": the MCP URL, HTTP, signing in through the browser, then importing
  a schema), the same as at the top of the repository's README; the e2e test
  checks the two agree and that docs/mcp.md still describes that flow.
- Comparisons and guides (`compare/`, `guides/`): the answer is the first
  paragraph; a comparison has a dated "Side by side" table whose last column
  says in words which tool is stronger (the other tool wins rows, and has its
  own "Where … is the better choice" section); every claim about another tool
  comes from its own pages, linked under Sources with the day they were
  read. Each page shows something made with romulus itself: a screenshot of a
  local install (`img/`, sample data) or the output of a real `romulus test`
  run, the local address replaced by romulus.example.com. Re-read the other
  tool's pages and change the date when you edit a comparison. Each has its
  own share image (`og/`, `npm --prefix site run assets -- --pages`), and is
  in the sitemap and the homepage's footer ("Comparisons and guides").
- A launch welcome: `?ref=hn`, `?ref=producthunt` (or the same
  `utm_source`) add one line under the header (app.js: "Welcome, Hacker News:
  self-host free, or try the founding-team offer"), on every page app.js
  runs on; closing it is remembered in this browser (localStorage), nothing
  else is.
- Fonts are self-hosted (Inter with its optical-size axis, so large headings
  get Inter Display's shapes; JetBrains Mono for code), preloaded, `swap`.
- Pricing: the plans live in one place, the JSON block `#plans-data` in
  `index.html` (prices, limits, the buttons' target). Change it, then
  `npm --prefix site run pricing` rewrites the cards between the `plans`
  markers; the e2e test fails until the two agree. Yearly is
  `yearly_months` (10) times the monthly price. The cards are static HTML
  holding both periods' prices, and the Monthly/Yearly switch is native
  radios and CSS (`:has()`), no script. A paid plan's button ("Subscribe
  to Team") is two links, one per period, the one shown following the
  switch: `subscribe?plan=<id>&interval=month|year` (`/app/signup`, the
  API's plan ids from `billing::plans`), which carries the choice through
  sign-in or sign-up (Zitadel's login creates the account), the Terms and
  the first organization, to that organization's checkout (docs/MVP.md,
  "Subscribing from the homepage"). Free's goes to `signup`
  (`/app/signin`); an organization on Free still buys a plan from its
  Billing page. Paid plans are for businesses (the line under the switch;
  the checkout asks the business's name and its professional use). The
  limits' labels and the support lines say what
  the hosted Terms say (7.1, 13.3): the table cap counts one version's
  tables and views, support times are targets. The line under the switch is
  the pricing's one VAT mention (Terms 7.3). A plan's `offer` (the
  founding teams', on Team: docs/MVP.md "The founding-team offer") is a line
  under its button, hidden in the HTML; `app.js` asks its `count` endpoint
  (`/api/billing/offers/founding`, through the Function) and shows it, with
  the places left, only while there are some. Its links add `&offer=<id>`;
  its words follow the Terms (7.11: the price holds for as long as the
  subscription stays on the plan, so never "for life").
- Legal: the legal notice the LCEN requires is `mentions-legales.html`
  (French) and `legal-notice.html` (its English translation), served at
  `/mentions-legales` and `/legal-notice`, each linking to the other. The
  hosted service's terms, privacy policy and DPA are published from
  `site/legal/*.md` (as approved, word for word: only markup is added) by
  `tools/legal.mjs`, in the notice's chrome, in two editions
  (`site/legal/README.md`). romulus is sold to professionals only, so the
  professional edition is in force, at the canonical addresses:
  `/legal/terms` ⇄ `/fr/legal/conditions`, `/legal/privacy` ⇄
  `/fr/legal/confidentialite` (each pair linked both ways, with hreflang
  alternates), and `/legal/dpa`, in English only, for both editions. The
  full edition (consumers included) is published for later at
  `/legal/full/terms` ⇄ `/fr/legal/full/conditions` and `/legal/full/privacy`
  ⇄ `/fr/legal/full/confidentialite`: `noindex`, out of the sitemap, linked
  from nowhere, with a notice above the document (chrome, not part of it)
  saying it is not in force and linking the documents that are. When
  consumer sales open, the full edition becomes a new version at the
  canonical addresses (`site/legal/README.md` says how). Headings get ids
  from their text (lowercase, accents stripped, other characters one `-`:
  `/legal/full/terms#9-consumer-rights`); tables sit in a labelled frame that
  scrolls when needed and become one card per row on a phone; `@media
  print` gives A4 with the text alone. The same tool prints each edition of
  the Terms, in both languages, to the PDFs the API attaches to its
  confirmation emails (`crates/romulus-api/legal/terms-pro-<version>-<lang>.pdf`
  for the professional edition, `terms-<version>-<lang>.pdf` for the full
  one, with `site/legal/manifest.json`); `--check` (in `just check` and
  `just site-check`) fails when a page or a PDF is not made from the
  current sources, or the sitemap lists a document not in force or misses
  one that is. All of it is static like the homepage (the edge tests check
  `_routes.json` keeps the Function off every document). The footer's
  `nav.footer-legal` holds the notice, the terms, the privacy policy and
  the DPA in force, in the page's language (the DPA marked as English on
  French pages), the page's own marked `aria-current`: its links are
  `legalLinks` in `tools/legal.mjs`, which writes them into the legal pages
  and `404.html` (`--check` fails when a page's differ). The homepage's
  footer is `index.html`'s own, by hand: the notice, the terms, the privacy
  policy and the DPA (for business customers) at their canonical addresses. The notice links the privacy
  policy, and its data paragraph says the site sets no cookies and loads
  nothing from another origin but the homepage's audience measurement:
  `e2e/legal.spec.ts` checks both on every legal page, so a change that
  breaks either must change the notice too.
- Analytics: the homepage counts visits with Umami at
  `https://stats.lupidae.com` (the tag in `index.html`, its website id
  committed: not a secret; `data-domains="romulus.lupidae.com"`, so
  previews and local runs send nothing; Do Not Track honoured;
  `data-before-send="romulusBeforeSend"`, in `app.js`, keeps the address's
  `utm_*` only and the referrer's origin), the one other origin in
  `_headers` (`script-src`, `connect-src`). The sign-up buttons (Start free,
  Subscribe) carry `data-umami-event="signup_started"` and the plan
  (`tools/pricing.mjs` writes the cards'). `app.js` adds this page's `utm_*`
  and the referrer's origin (`ref_origin`) to the links into the app, which
  reports them with the sign-up (docs/MVP.md, "Signup sources and
  analytics"): in URLs only, nothing in the browser's storage. The e2e
  tests import `test` from `e2e/fixtures.ts`, which answers
  stats.lupidae.com with an empty script: no test ever reaches it.
- Mobile performance: no `filter: blur()` or `mask-image` layers (the glows
  are plain radial gradients) and no layout reads during load. Those three
  cost ~2.5 s of main thread on Lighthouse's throttled phone.

## The live hero

The hero's window is romulus's own draft editor, running in the visitor's
browser with no account and no server: edit the SQL and the graph (with its
groups and graph edits) follows, select a table in either pane, save a
version, change it and see the diff (`SchemaDiffView`) and the migration
(`MigrationView`, up and down), and comment (a `---` line, pins, the panel,
replies, resolve). Nothing is sent anywhere: the draft, its versions and its
discussion stay in `localStorage`; Reset starts over. Collaboration, real
Postgres compiles and playgrounds are the app's (the note under the window
says so, beside the link into it).

```
web/src/hero/          the island: main.tsx (mount), boundary.tsx, hero-app.tsx,
                       engine.ts, seed.ts, store.ts, toaster.tsx, hero.css,
                       contain-scroll.ts
web/vite.hero.config.ts  its build: one script and stylesheet, chunks, two wasm files
crates/romulus-wasm/   the engine (wasm-bindgen): SQL to a remus model, romulus-diff,
                       romulus-migration, remus's JSON/DBML/Mermaid
tools/hero.mjs         cargo (profile `wasm`), wasm-bindgen, wasm-opt, vite → public/hero/
```

**SQL to a model, without Postgres.** The app compiles SQL in a sandbox
Postgres and reads the model back with remus's introspection; remus itself
has no SQL parser (remus-sql only renders a model as SQL). In the browser,
the text is parsed by libpg_query (Postgres 17's own parser, the npm
`libpg-query@17` wasm build: the grammar the API parses with), and
`romulus-wasm` runs the parse tree against a catalog in miniature, printing
every definition as Postgres's `ruleutils` does (defaults, checks, keys,
indexes, policies, pretty-printed views, partition bounds) and raising
Postgres's errors where a parser and a catalog can tell (a missing table or
column, duplicates, a foreign key without a unique key or of another type,
dependencies on DROP), with the drafts' allowlist and its words. Chosen
over building pg_query's C for wasm (setjmp/longjmp under
wasm32-unknown-unknown, an emscripten or WASI toolchain in CI) and over a
reduced parser. Its tests run natively: pg_query's protobuf tree is written
as libpg_query's JSON (byte for byte the wasm's), and an oracle compiles the
same SQL in the sandbox Postgres, models and error messages compared
(`crates/romulus-wasm/tests`). What it does not do, the app does: functions,
triggers and data never run anywhere here (the allowlist refuses them, as in
a draft), and expressions it does not analyse keep their text.

**Loading.** On a desktop (`pointer: fine`, 64rem and wider, no data saver)
`app.js` loads the island once the page is idle; on a phone it waits for
"Try it live" (a megabyte nobody asked for). The island and the static
picture share one grid cell: the island is drawn first, then the picture is
made invisible (not removed), so nothing moves (CLS 0). Without JavaScript,
or if any part fails to load, the picture stays, with a word about it under
the window (`[data-hero-status]`, a live region) and "Try again".

**When something breaks.** The engine never throws for what the SQL is: a
statement it does not model is a note (`not_run`: "… is not supported in the
browser preview"), never a silent skip. A failure of the engine itself (a
panic, which aborts the wasm: `panic = "abort"`, the message on the console
by the crate's panic hook) is an error on the text ("The browser preview
could not check this SQL"); `engine.ts` then starts romulus-wasm afresh
(wasm-bindgen's `__wbg_reset_state`) or loads the parser again, and the next
edit is checked. What this browser kept is validated value by value
(`store.ts`), and forgotten when any of it does not read. A render that
throws (`boundary.tsx`), or an engine that cannot start again, takes the
island down: it forgets what it kept, and the page shows the picture again
with "The live editor ran into a problem and stopped" and "Try again".

**Size** (`public/hero/manifest.json` has every file): 875 KB brotli up
front (1.10 MB gzip): the script 276 KB and its chunks 120 KB, the CSS
16 KB, romulus-wasm 297 KB, libpg-query 165 KB. elk's layout worker (337 KB)
loads only when a table has no position (the seed has them all).
Lighthouse: mobile and desktop 100 for performance and accessibility.

**Styles.** `hero.css` is the app's Tailwind (preflight, `theme.css`,
utilities), every rule scoped to `.romulus-hero` by a PostCSS step in
`vite.hero.config.ts`; the tokens and fonts are this site's. The site's own
rules are in the `site` cascade layer, below the hero's, so inside the island
the app's styles win and outside it they match nothing; the few element
rules of the site that would still leak in (an SVG's `max-width`, the focus
ring's radius) are undone in `hero.css`. Menus, popovers and dialogs render
in a second `.romulus-hero` element at the end of the body (the kit's
`PortalContainerProvider`), outside the window that clips.

**CSP.** Only `'wasm-unsafe-eval'` was added (two WebAssembly modules). The
hero build swaps the kit's toaster (sonner writes an inline `<style>`) for
`hero/toaster.tsx`, and makes CodeMirror's style-mod use a constructable
stylesheet (CSSOM, which `style-src` does not govern) instead of a `<style>`
element. A second build-time patch keeps CodeMirror from scrolling the
window (only its own scroller), and `contain-scroll.ts` does the same for
`scrollIntoView` inside the island: opening a thread never moves the page.
Both patches fail the build if the code they patch changes.

**Keeping it current.** The build is a snapshot of `web/` and the engine:
`just site-hero` after changing `web/src/hero`, the components it reuses or
the engine, then commit everything it writes. `site/tools/hero.mjs --check`
(in `just check` and `just site-check`) fails when a committed file differs
from what the build recorded (`manifest.json` has every file's SHA-256), or
when the hero's own inputs changed since (`web/src/hero`,
`vite.hero.config.ts`, `crates/romulus-wasm`, the `wasm` profile, the build
script); it notes, without failing, when the engine's other inputs
(romulus-diff, romulus-migration, every crate they resolve to in
`Cargo.lock`, remus included) or the app's components and lockfile did:
those are shared with the app and the API, and failing there would make
every such PR rebuild and commit the hero's binaries. The toolchain:
rustup's `wasm32-unknown-unknown` target, `cargo install wasm-bindgen-cli
--version` the crate's (pinned in its Cargo.toml) `--locked`, and binaryen's
`wasm-opt` at the version `hero.mjs` pins (another one writes other bytes);
the engine builds with `cargo build --locked`.

