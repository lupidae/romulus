# site/: the romulus homepage

A static page, no build step and no framework: `public/` is exactly what gets
served. It is the app's brand, not a lookalike: `public/tokens.css` and the two
fonts are copied from `web/` by a script, and a check fails when they drift.

```
public/            what Cloudflare Pages serves
  index.html       the page; the hero's product window is HTML, CSS and SVG
  styles.css       layout and the marketing type scale (colors only from tokens)
  tokens.css       generated from web/src/styles/globals.css: do not edit
  app.js           header hairline, hero playback, copy button, theme switch
  _headers         CSP (with the inline theme script's hash), HSTS, caching
  404.html, robots.txt, sitemap.xml, site.webmanifest, og.png, icons
tools/sync.mjs     copies tokens and fonts from web/ (--check for CI)
tools/assets.mjs   renders og.png (tools/og.html + the hero window) and the icons
serve.mjs          a local server that behaves like Pages (_headers, 404, compression)
e2e/               Playwright + axe
wrangler.toml      the Cloudflare Pages project
```

## Run and check

```bash
npm ci --prefix site          # once (Playwright and axe); web/ needs `npm ci` too for sync
just site                     # serve on $ROMULUS_SITE_PORT (scripts/dev-env, slot + 7)
just site-check               # tokens/fonts in sync, then Playwright + axe (CI: .github/workflows/site.yml)
just site-gen                 # after a token change in web/, or a change to the hero or mark
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
