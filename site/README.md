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

Not deployed by CI. With `npx wrangler login` done once:

```bash
just site-deploy              # wrangler@4.151.0 pages deploy, project "romulus-site"
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
`/api/…` wherever they appear). The homepage is hosted on Pages and the app's
host is not decided yet, so until then those links 404 on Pages. When it is,
add one of these to `public/_redirects`:

```
# the app on its own host: send visitors there
/app/*  https://<app host>/app/:splat  302
```

or keep one origin by proxying `/app/*` and `/api/*` to the API (a Pages
Function or a Worker route in front of Pages), which is the MVP's shape: the
API serves the SPA at `/app` and itself at `/api`. If the API ever serves this
directory at `/` instead, it must send the `_headers` equivalents itself.

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
