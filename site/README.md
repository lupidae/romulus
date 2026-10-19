# site/: the romulus homepage

A static page, no framework: `public/` is exactly what gets served, and Pages
has no build step. It is the app's brand, not a lookalike: `public/tokens.css`
and the two fonts are copied from `web/` by a script, and a check fails when
they drift. Its hero is the app itself, running in the browser (below): built
from `web/` and `crates/romulus-wasm` into `public/hero/`, and committed.

```
public/            what Cloudflare Pages serves
  index.html       the page; the hero's static window is HTML, CSS and SVG
  styles.css       layout and the marketing type scale (colors only from tokens)
  tokens.css       generated from web/src/styles/globals.css: do not edit
  app.js           header hairline, hero playback, loading the live hero, copy button, theme switch
  hero/            the live hero, built by tools/hero.mjs: do not edit
  _headers         CSP (with the inline theme script's hash), HSTS, caching
  404.html, robots.txt, sitemap.xml, site.webmanifest, og.png, icons
tools/sync.mjs     copies tokens and fonts from web/ (--check for CI)
tools/hero.mjs     builds public/hero/ (--check: is it built from the current sources?)
tools/assets.mjs   renders og.png (tools/og.html + the hero window) and the icons
serve.mjs          a local server that behaves like Pages (_headers, 404, compression)
e2e/               Playwright + axe
wrangler.toml      the Cloudflare Pages project
```

## Run and check

```bash
npm ci --prefix site          # once (Playwright and axe); web/ needs `npm ci` too for sync
just site                     # serve on $ROMULUS_SITE_PORT (scripts/dev-env, slot + 7)
just site-check               # tokens/fonts in sync, the hero built, then Playwright + axe (CI: .github/workflows/site.yml)
just site-gen                 # after a token change in web/, or a change to the hero or mark (rebuilds the live hero)
just site-hero                # the live hero only, after a change to web/src/hero, the components it reuses, or romulus-wasm
just site-lighthouse          # mobile and desktop reports in site/lighthouse/
```

The CSP allows one inline script by hash: if you edit the theme script in
`index.html` (it must stay identical in `404.html`), update the hash in
`_headers`; the e2e test says so when they disagree.

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
every other path, so the homepage stays static. The API's origin, its edge
key and the rest of the preview are in `deploy/preview/README.md`. `_headers`
applies to static files only (Pages never applies it to a Function's
responses): the homepage's CSP does not reach the app, whose headers come
from the API.

## Decisions

- Hosting: Cloudflare Pages. Canonical URL `https://romulus.lupidae.com/`
  (in the head of `index.html`, `robots.txt`, `sitemap.xml`; the custom
  domain is attached in the Pages dashboard).
- "Request access" opens `mailto:romulus@lupidae.com`.
- The push snippet follows the spec: `POST /api/databases/<id>/versions` with
  the database's bearer token, remus JSON as the body.
- Fonts are self-hosted (Inter with its optical-size axis, so large headings
  get Inter Display's shapes; JetBrains Mono for code), preloaded, `swap`.
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

