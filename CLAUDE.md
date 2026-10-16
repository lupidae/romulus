# romulus

DBA as a service: an organization keeps its Postgres schemas here, versions
them, reviews diffs and designs the next version together (SQL and table graph,
live cursors). The spec is [docs/MVP.md](docs/MVP.md); read it before a feature.
remus (the CLI twin, `remus-core` on crates.io) is where the schema model lives.

## Repo map

```
crates/romulus-api/   axum API (lib + bin): src/<module>/mod.rs per feature, tests/api/
crates/romulus-diff/  model diff over remus-core, no I/O, builds for wasm; `utoipa` feature for the API
crates/romulus-migration/ model → model migration (ALTER … in dependency order), tool files; no I/O; oracle in tests/
crates/romulus-sandbox/ SQL → model in the sandbox Postgres: statement keys, allowlist, stubs, compile pool
deploy/sandbox/       the sandbox image: Postgres 17 + PostGIS + pgvector, allowlisted extensions trusted
migrations/           sqlx migrations, timestamp-named; .sqlx/ = offline query data
web/                  Vite + React SPA served under /app (see "Web" below)
site/                 the static homepage (tokens copied from web/; see site/README.md); its Pages
                      Function (functions/, edge/proxy.js) proxies /app and /api to the API
deploy/zitadel/       the provisioner that sets Zitadel up on `just up` (and on deploys), and the login's brand assets
deploy/preview/       the preview at romulus.lupidae.com: compose, deploy.sh, cloudflare.sh; README = runbook
Dockerfile            the API image (web build + release API), pushed to ghcr.io by .github/workflows/images.yml
scripts/dev-env       per-worktree ports and .env;  scripts/check-oidc  proves SSO works
scripts/export-migrations/  alembic.sh, django.sh, rails.sh: run in a user's project, print the SQL to import
docker-compose.yml    postgres (app), sandbox-postgres, zitadel + zitadel-db, provisioner, mail (Mailpit)
.superset/            workspace setup/teardown (Superset worktrees boot configured)
```

## Run it

Needs: rustup, Docker, Node 24 + npm, `just`, `cargo install sqlx-cli
--version 0.9.0 --locked --no-default-features --features postgres,rustls`,
and the pinned nightly rustfmt (`rustup toolchain install nightly-2026-10-14
--profile minimal --component rustfmt`; the date lives in the justfile and CI).

```bash
just up          # .env if missing, then the stack; Zitadel provisioned (~15 s from scratch)
just dev         # up + API (cargo run) + web (vite); open $ROMULUS_PUBLIC_URL/app/
just check       # the CI gate: fmt, clippy + tests (--all-features), sqlx prepare --check, gen-check, web, build
just e2e         # Playwright: production build + its own API, on the preview origin (needs `just up`)
just e2e-dev     # Playwright on dev-only screens (/app/dev/*, e.g. the schema viewer): dev server, no stack
just perf        # schema viewer frame times at 300 tables: headed, production build with dev routes
just gen         # after any API change: OpenAPI → web/src/api/{openapi.json,schema.gen.ts}
just site        # the homepage on $ROMULUS_SITE_PORT; `just site-check` = its CI gate
just migrate | just down | just nuke   # nuke = down -v + rm .dev (fresh Zitadel too)
scripts/check-oidc   # discovery, client secret, full PKCE login as alice
```

Test users (Zitadel, `.dev/zitadel/users.json`): `alice@romulus.test`,
`bob@romulus.test`, password `RomulusDev1!`, emails verified. Zitadel console:
`http://localhost:$ROMULUS_ZITADEL_PORT/ui/console`, `admin@romulus.localhost`,
same password. Anyone can also create an account on Zitadel's login ("Create
account"): the verification code arrives in the mail catcher,
`http://localhost:$ROMULUS_MAIL_PORT` (nothing leaves the machine).

## Ports and worktrees

Several worktrees (and clones) run their own stacks at once, so **nothing is a
fixed port**. `scripts/dev-env` gives each worktree a slot N (one registry per
user: `~/.local/state/romulus/slots`, so clones never collide) and writes
`.env` (gitignored; never copy it between worktrees) with:

| offset | variable | what |
|---|---|---|
| +0 | `ROMULUS_WEB_PORT` | Vite; the only origin the browser uses (`ROMULUS_PUBLIC_URL`) |
| +1 | `ROMULUS_API_PORT` | the API (`ROMULUS_API_ADDR`) |
| +2 | `ROMULUS_POSTGRES_PORT` | app database (`DATABASE_URL`) |
| +3 | `ROMULUS_SANDBOX_PORT` | sandbox Postgres (`ROMULUS_SANDBOX_URL`, superuser) |
| +4 | `ROMULUS_ZITADEL_PORT` | Zitadel, issuer `http://localhost:<port>` |
| +5 | `ROMULUS_PREVIEW_PORT` | `vite preview`: the origin Playwright drives |
| +6 | `ROMULUS_E2E_API_PORT` | the API Playwright starts, with `ROMULUS_PUBLIC_URL` = the preview origin |
| +7 | `ROMULUS_SITE_PORT` | the static homepage (`just site`, site/'s Playwright) |
| +8 | `ROMULUS_MAIL_PORT` | Mailpit: web UI and API of every email Zitadel sends (sign-up codes) |

Base port is `41000 + 10·N`. The compose project is `romulus-<dir>-<hash of
the worktree path>`, computed by `scripts/dev-env --project-name` and passed
with `-p` by every recipe, so `down -v` can only ever reach this worktree's
stack. Zitadel registers both browser origins (web and preview) for the OIDC
callback, and the e2e API's public URL is the preview origin: CSRF's exact
`Origin` check and the redirect agree with what Playwright's browser sends.
`ROMULUS_SLOT=<n> scripts/dev-env` moves a worktree; `scripts/dev-env
--release` frees its slot (Superset teardown does it). Never write a port
literal anywhere: read the variable.

## Decisions already made

Product and architecture: **docs/MVP.md, "Decided"** (SQL text is canonical,
graph edits append SQL with Tidy, Zitadel is identity only, one API instance,
copy-link invitations…). Change one with a reason, in MVP.md first.

Made in the skeleton (S0):

- **One origin.** Production: the homepage at `/`, the SPA at `/app`, the API
  at `/api`. The preview (deploy/preview) serves `/` from Cloudflare Pages and
  proxies `/app` and `/api` with a Pages Function to the API, which serves the
  SPA build itself (`ROMULUS_WEB_DIR`, the `web` module) and refuses requests
  without the Function's edge key (`ROMULUS_EDGE_KEY`, the `edge` module).
  Dev mirrors it: Vite serves `/app` (base and router `basepath`) and proxies
  `/api` (REST and websockets) without rewriting `Host`, so cookies are
  first-party and the API sees the real `Origin`.
- **Sandbox Postgres is 17**, because `pg_query` 6.2.1 bundles libpg_query
  built from PostgreSQL 17.7. Bump both together (and the PostGIS/pgvector
  versions in `deploy/sandbox/Dockerfile`); `Compiler::start` refuses a
  mismatch. The app database is 18 (`uuidv7()` ids).
- **Zitadel v4.19.4 with its built-in login (v1)**, one container, no proxy.
  Login v1 disappears in v5: upgrading means adding the `zitadel-login`
  container and a path-routing proxy. The OIDC callback is
  `$ROMULUS_PUBLIC_URL/api/auth/callback` (and the API port's, for direct
  testing); post-logout URIs are `/app/signin`, `/app/` and `/`.
- **Zitadel's login is branded** by the provisioner (label policy on the
  instance, which every organization inherits, and the project enforcing its
  organization's policy): logo per theme, Inter, colors from our tokens, no
  watermark, English only, romulus's copy for every page a sign-in or sign-up
  shows. Self-registration stays on: an invitee without an account creates
  one there (email verified with a code). The assets in
  `deploy/zitadel/assets` come from `npm --prefix web run zitadel:assets`;
  re-run it after a brand or token change (`just check` fails otherwise).
  Invitation links sign in straight through Zitadel with the invited address
  as `login_hint` (one click, dev stacks too); "switch account" also goes
  through Zitadel. The dev sign-in is only ever an explicit choice on
  `/app/signin`.
- **Dev sign-in**: `ROMULUS_DEV_SIGNIN=true` (on in `.env`) enables
  `GET /api/auth/dev-signin?email=…&next=/app/…[&name=…][&verified=false]`:
  signs in as that address (created if needed) without Zitadel, for QA and
  Playwright; the sign-in page lists alice and bob. Off, it is a 404 and not
  in the OpenAPI document. Config refuses it unless `ROMULUS_PUBLIC_URL` is
  loopback and `ROMULUS_API_ADDR` a loopback IP (never `0.0.0.0`). It reuses an existing user by email; a later real SSO login of
  that address is another user (different `sub`).
- **Sessions**: a random token in an httpOnly `SameSite=Lax` cookie
  (`Path=/api`, `Secure` on https), only its sha256 stored; expiry slides a
  week from each use, never past 30 days after sign-in. No IdP token is kept
  (`sessions.oidc_refresh_token` stays NULL; no `offline_access`). Logout is
  a POST answering the IdP's `end_session` URL; "switch account" signs in
  again with `prompt=select_account`. An expired session answers 401
  `session_expired` (the app says so on the sign-in page).
- **Invariants live in Postgres**: last admin (trigger, constraint
  `organization_needs_an_admin` → 409 `last_admin`), versions never updated
  and only deleted with their database (`versions_are_immutable`,
  `versions_are_permanent` → 409 `version_immutable`), remus JSON only from
  pushes, drafts based on their own database's versions.
- **The last-admin trigger is safe at every isolation level** because it
  writes (unchanged) the organization row, not just locks it: READ COMMITTED
  waits and recounts; REPEATABLE READ and SERIALIZABLE abort the stale
  transaction with 40001 (`db::is_serialization_failure`). Membership changes
  run at the default READ COMMITTED; if you raise the isolation, retry on
  40001 (or let it answer 409 `concurrent_change`).
- **Deviations from MVP.md's data model**: `UNIQUE (org_id, slug)` on
  databases only among live rows (a deleted database frees its slug);
  `drafts.base_version_id` may be NULL (a database with no version yet);
  added `users.email_verified` and `versions.created_by_token`; added
  `oidc_flows` (sign-ins in flight: PKCE verifier, nonce; where to land rides
  in the flow cookie, so an invitation token never sits in the database; at
  most 20 started per client IP, then one per 3 s), the
  `organizations_slug_not_reserved` check and the `*_email_nfc` checks;
  `databases_slug_not_reserved` (a database slug must not shadow
  `/app/<org>/settings`…) and length checks on database descriptions and
  version messages.
- **Design system**: Tailwind v4 + Radix primitives, owned components in
  `web/src/components/ui` (shadcn-style, not a dependency). Chosen because
  every agent already knows the idiom, tokens are plain CSS variables, and
  removing Tailwind's palette makes off-system colors impossible rather than
  discouraged. Tokens were generated with better-design from remus's palette
  (warm stone neutrals, terracotta accent: the twins look related).

## API conventions (`crates/romulus-api`)

- **Modules**: one directory per feature, `pub fn router() ->
  OpenApiRouter<AppState>` with paths relative to `/api`, handlers annotated
  with `#[utoipa::path]` and added with `.routes(routes!(handler))`. `lib.rs`
  already merges every module; you never edit it to add an endpoint. Each
  module's doc comment lists its planned endpoints and tables: keep it true.
- **API naming**: plural nouns, slugs in paths (`/orgs/{org}/databases/{db}`),
  JSON in snake_case, `POST` to create, `PATCH` to update, `DELETE` to remove;
  actions that are not CRUD are sub-resources (`POST …/drafts/{id}/publish`).
- **Errors**: handlers return `ApiResult<T>`. Every error is JSON
  `{"code", "message", "details"?}`; `code` is stable and the web app branches
  on it. Use the `ApiError` variants (`BadRequest`, `Validation` (422),
  `Unauthorized`, `Forbidden`, `NotFound`, `Conflict { code, message }`,
  `PayloadTooLarge`, `Unavailable`…). `?` on a `sqlx::Error` maps the database
  invariants (`last_admin`, `version_immutable`, `concurrent_change` for a
  40001) and is otherwise a logged 500 with a generic message.
  `db::constraint(&err)` / `db::is_unique_violation(&err)` map other
  constraint errors to a 409 with your own code. `thiserror`, no `anyhow`.
- **Extractors**: always `romulus_api::extract::{Json, Path, Query, Bytes}`,
  never axum's (clippy's `disallowed-types` refuses them). Their rejections
  answer in the API format: malformed JSON 400 `bad_request`, a missing or
  wrong field 422 `validation` naming it, wrong content type 415
  `unsupported_media_type`, a body over the limit 413 `payload_too_large`, a
  bad path or query parameter 400. `extract::Json` is also the response type.
  Anything else that errors outside a handler (a 405, the 30 s timeout's 503
  `timeout`, another extractor) is rewritten to the same format by
  `layers::errors_as_json`, but prefer a precise code from your handler.
- **State**: `AppState { db, config, oidc, membership, compiler, limits, collab }` derives `FromRef`:
  take `State<PgPool>` or `State<Arc<Config>>` directly.
- **Who is calling** (`auth`): take `CurrentUser` for anything a signed-in
  person does, and `Member<min::Reviewer | min::Developer | min::Admin>` for
  everything under `/orgs/{org}/…` (the path segment must be named `org`):
  it loads the membership, answers 404 to non-members and 403 below the
  minimum. The matrix (docs/MVP.md "Roles"): reads → `Member` (reviewer is
  the default); edit drafts, versions, uploads, API tokens, create/delete
  databases → `Member<min::Developer>`; members, invitations, org settings →
  `Member<min::Admin>`. `Option<CurrentUser>` works for pages that also serve
  signed-out visitors. `ApiError::Denied { code, .. }` is a 403 with its own
  code; `ApiError::SessionExpired` is the 401 above.
- **Membership changes and live connections**: `orgs` publishes every
  committed role change or removal on `state.membership`
  (`orgs::events::MembershipEvents`, a broadcast channel). A websocket task
  subscribes and closes when `change.concerns(org_id, user_id)`, or on
  `Lagged` (see the module docs). Membership writes go through
  `db::begin_read_committed`.
- **Email addresses**: store `email::normalize(..)` (trimmed, NFC; the
  database refuses other forms) and compare in SQL with
  `lower(x COLLATE pg_unicode_fast)` on both sides (Unicode case mapping
  whatever the database locale; the indexes use the same expression), or
  `email::key` in Rust (a test keeps the two in agreement).
- **Secrets handed out** (session ids, invitation links, API tokens):
  `token::generate()` (32 random bytes, base64url) and store
  `token::hash(..)` (sha256, fits the `*_hash bytea` checks). API tokens are
  `rom_` + that (`databases::tokens::PREFIX`), shown once.
- **The machine API** (remus, CI) lives at `/api/databases/{database_id}/…`
  and authenticates with `databases::tokens::Bearer<scope::Push | Full>`
  only (the path segment must be named `database_id`; it answers 401
  `invalid_token`, 403 `wrong_database` / `insufficient_scope`, and records
  `last_used_at`). The CSRF guard answers an unsafe request there without a
  bearer with 401 `invalid_token`, not `csrf`. Browser endpoints stay
  org-scoped (`/orgs/{org}/databases/{db}/…`).
- **Creating a version** always goes through `versions::store::store`: it
  numbers it under `FOR NO KEY UPDATE` on the databases row (no gaps, no
  duplicates), inherits the previous layout unless given one (`layout`),
  skips an unchanged model when asked (pushes), and with `expected_base`
  refuses with `StoreError::StaleBase` (409 `stale_base`) when the latest
  version moved (drafts publishing).
- **A version's layout** has one shape everywhere,
  `versions::layout::VersionLayout`: `{"layout": {"<schema>.<table>": {"x",
  "y"}}, "groups": {"<id>": {"name", "color", "members"}}}`, stored pruned to
  the model's tables, never null (empty is `{"layout": {}, "groups": {}}`;
  `{}` reads as empty). The viewer gets `layout.layout` and `layout.groups`
  as `<SchemaViewer layout groups>`.
- **Diffs** (`versions::diff`, `GET …/diff?from=&to=`): romulus-diff's
  `SchemaDiff`, both models, both as SQL (`romulus_diff::normalize` then
  `compile::render_sql`, the newer version's header on both, so an upload
  and a push of one schema read the same) and the union's layout (the newer
  version's, the older one's positions for tables only it has). The
  serialised content (all but the two summaries, whose authors can be
  renamed) is cached in `AppState::diffs` by the two version ids; a request
  resolves numbers to summaries first and loads full versions only on a miss.
- **Migrations** (`versions::migration`, `GET …/migration?from=&to=&format=`;
  `drafts::migration`, `GET …/drafts/{id}/migration?format=&sha256=`, base →
  draft; `sha256` of the text reviewed is required, 409 `draft_changed`):
  `romulus_migration::migration(old, new)` plans the statements (phases in
  `plan.rs`: what blocks a change dropped first, types, tables, keys, views,
  security, comments, then the removals), `files(up, down, format, meta)`
  lays them out per tool (`plain`, `sqlx`, `flyway`, `prisma`, `dbmate`,
  `golang_migrate`: the names migration import uses too) with a best-effort
  down migration (`migration(new, old)`). Each statement carries a
  `hazard`: `deletes_data`, `can_fail` (on existing rows) or
  `widens_access` (RLS off or unforced, a restrictive policy dropped, a
  permissive one added or changed); `destructive` = the first two. A cast
  that cuts values (to `name`, `"char"`, numeric → integer…) deletes data;
  shortening `varchar(n)`/`bit(n)` converts without `USING`, so Postgres
  refuses an overlong value (can fail) instead of cutting it. Every
  change kind is proven by the oracle (`crates/romulus-migration/tests/oracle`):
  the compile (`render_sql(old)` + the migration, in the sandbox, must
  introspect as `new`), the tool runs (`tools.rs`: each tool's files
  applied as the tool applies them on a scratch database, up then down;
  scenarios also built from their own text) and the rows (`data.rs`: each
  hazard against seeded rows: fails, or changes them, as labelled);
  scenarios, corpus pairs, random mutations; `ROMULUS_ORACLE_SEEDS=100` and
  `ROMULUS_ORACLE_TOOL_SEEDS=20` for a deep run. Change the planner, run it.
  Without `ROMULUS_SANDBOX_URL` it skips, except in CI (`CI` set): it fails.
  Objects an extension owns (`extensions::PROVIDED`, checked against the
  sandbox catalog) are never created or dropped; partitions becoming tables
  (and back) are detached and attached (`partitions.rs`), never dropped; the
  model has no names for a partition's own keys and indexes, so a detached
  one's are Postgres's defaults, guessed (`IF EXISTS`/`IF NOT EXISTS`, a
  `guessed_names` warning).
- **Rate limits**: `rate::Limits` in `AppState` (per API token for pushes,
  checked before the body is read; per person and per database for SQL
  compiles; per database for imports and, apart, their previews), 429 `rate_limited` with `Retry-After`. Handlers that must decide
  before reading a large body take `axum::body::Body` and read it with
  `extract::read_body(headers, body, limit)` (413 in the API's format).
- **CSRF** is global middleware: every non-GET request needs
  `X-Romulus-Csrf: 1` and an `Origin` equal to `ROMULUS_PUBLIC_URL`, unless it
  carries `Authorization: Bearer` (API tokens). Bearer requests must never fall
  back to the cookie (`CurrentUser` refuses them). Websocket upgrades take
  the `csrf::SameOrigin` extractor, which checks `Origin` the same way.
- **OpenAPI**: give every handler a unique `operation_id` in
  `#[utoipa::path]`; the web types key operations by it (a test fails on
  duplicates).
- **Shared types**: `domain.rs` maps every Postgres enum (`Role`,
  `SourceKind`, `VersionOrigin`, `DraftStatus`, `TokenScope`) and re-exports
  `remus_core::Schema`. Don't redefine them in a module.
- **Config** comes from the environment only (`config.rs`); a debug build's
  `main` first loads `.env` and `.dev/zitadel/oidc.env` (real env always
  wins), a release build reads its real environment only. A new setting is a
  field + a test there, and a line in `scripts/dev-env` if dev needs it.
- **Graceful shutdown** waits for open connections: anything long-lived
  (websockets) must close on the shutdown signal.

## SQL and migrations

- New migration: `cargo sqlx migrate add -r <name> --source migrations`
  (timestamp names, so parallel branches don't collide). Never edit an applied
  one. Then `just migrate` and `cargo sqlx prepare --workspace -- --all-targets`,
  and commit `.sqlx/`.
- Style: lowercase SQL keywords are fine but be consistent within a file;
  `uuid DEFAULT uuidv7()` ids, `timestamptz`, `NOT NULL` unless absence means
  something, an index on every FK you query by, `COMMENT ON` for anything
  non-obvious, enums as Postgres enums (mapped in `domain.rs`).
- Queries: the `query!` / `query_as!` macros only (checked against the
  database at compile time). Custom enums need an annotation:
  `SELECT role AS "role: Role"`, bind with `role as Role`.
- With `.env` present the macros check online: with the stack down, build
  with `SQLX_OFFLINE=true`.

## Web conventions (`web/`)

```
src/routes/              TanStack file routes (only route + page code); -prefixed files are ignored
src/components/ui/       the kit: Button, Input/Textarea (Input `prefix`), Field, Select
                         (SelectItem `description`), Dialog, ConfirmDialog, Popover, Sheet, Menu,
                         Toaster/toast, Avatar/AvatarStack, Badge, Tabs, Table, CopyField,
                         EmptyState, Skeleton, Tooltip, Kbd, Alert, Card, Separator, Spinner,
                         Sidebar* (items take `icon`/`label`; a rail inside SidebarRail),
                         TabNav (link tabs, one URL each), CodeBlock (copyable
                         lines), FileButton (opens the file picker), Checkbox, Combobox
                         (a field with suggestions: ARIA combobox, the field keeps focus)
src/components/shell/    AppShell (top bar, org switcher, user menu, nav: a rail by default,
                         opened with ⌘\, remembered per user; panel), PageHeader,
                         PageBody, status pages (404/500), the sign-in preview
src/components/icons/    Iconoir only, generated: `npm --prefix web run icons:add -- <name>`
src/features/<module>/   per-feature code: queries (queryOptions), mutations, components
src/features/schema/     SchemaViewer / SchemaGraph / SchemaText (graph + SQL panes), remus model
                         types (parseSchema), fixtures; /app/dev/viewer shows them
src/features/schema/edits/  graph edits as data (GraphEdit) and their SQL (sqlFor, quoteIdent
                         with PG 17's keywords), what depends on what, foreign key checks and
                         names, the optimistic model (applyEdit/reflects), appendTo (the marker)
src/features/schema/graph/editing/  the graph's editing UI (menus, inline names, popovers,
                         foreign key drag, new table); on when SchemaGraph gets `editor`
src/features/databases/  databases, versions, API tokens: queries, mutations, the upload dialog
                         (useOpenUpload), VersionView, DraftsSlot (where drafts list, above the
                         history)
src/features/imports/    the migration import page (ImportFlow: drop, review, progress, result),
                         the files picked (files.ts), the API calls and NDJSON progress (api.ts),
                         MigrationButton (an imported version's migration file)
src/features/diff/       SchemaDiffView (change list | graph | SQL merge view; no router, no
                         queries: reuse it wherever two models are compared), CompareBar,
                         diffQuery; /app/<org>/<db>/diff/<from>...<to>, /app/dev/diff shows it
src/features/migration/  MigrationView (tool picker, concurrently, warnings, files with destructive
                         lines tinted, Copy, Download/.zip; no router): the diff page's Migration
                         view (?view=migration) and the publish dialog's Migration tab
src/api/                 client.ts (typed openapi-fetch + CSRF header), generated types
src/styles/globals.css   every token; the only file with raw colors
```

- **Routes** live under `/app`. Org pages are `routes/$org/…`
  (`/app/<slug>/…`): `routes/$org.tsx` is the signed-in layout (it renders
  `<AppShell>` with the real user and orgs, and 404s for non-members); add a
  page as a child file and render `<PageHeader>` in it (the layout already
  wraps `<PageBody>`). Read the org with `useSuspenseQuery(orgQuery(slug))`
  and gate actions on `canWrite(org.role)` / `canManageMembers(org.role)`
  (`features/orgs/roles.ts`); the API enforces it anyway. Slugs must not
  collide with top-level routes: `signin`, `new`, `invite`, `dev`,
  `settings`, `api`, `app` are reserved (API and database refuse them). A
  signed-in route outside `$org` calls `requireUser(queryClient,
  location.href)` (`features/auth/session.ts`) in `beforeLoad`. A 401 from
  any query or mutation sends the user to sign-in and back; a 403 refetches
  the orgs (main.tsx). `/app/dev/kit` and `/app/dev/shell` exist in dev
  builds only.
- **Database pages** are `routes/$org/$db.tsx` (header, tabs Schema /
  History / Settings, the upload dialog) with children `index` (the latest
  version), `v/$number`, `history`, `settings`; drafts (milestone 4) go under
  `$db/drafts/…`. They declare `staticData: { fullBleed: true }`, so the org
  layout skips `PageBody` and the viewer gets the whole panel; a child page
  that wants the usual measure pads itself. Database slugs must not shadow
  the org's own pages (`settings`, `new`, `members`, `invitations`,
  `search`, `activity`). Versions are cached forever, keyed by database id
  (`databaseKeys.version`): they never change.
- **Data**: TanStack Query owns server state. Export `queryOptions` from
  `features/<module>/`, call `api.GET/POST(...)` through `unwrap` (throws
  `ApiError` with the API's `code`). Never hand-write an API type: change the
  Rust, `just gen`. Loaders use `context.queryClient.ensureQueryData`.
- **Design system rules** (lint enforces most; review the rest):
  - Use the kit. No raw `<button>/<input>/<select>/<textarea>` outside
    `components/ui`; extend the kit when it lacks something, don't fork a
    one-off. New primitive: build on Radix (`radix-ui`), style with tokens,
    show it on `/app/dev/kit`.
  - Tokens only: semantic classes (`bg-card`, `text-muted-foreground`,
    `border-border`, `bg-primary`, `text-destructive`, `shadow-popover`…).
    Tailwind's palette is removed, so `bg-blue-500` silently does nothing; hex,
    `rgb()`, inline color styles and `dark:` variants are lint errors (tokens
    already switch theme). Lengths in rem or the spacing scale.
  - Type scale is app-sized: `text-sm` (13px) for dense UI, `text-base` (14px)
    body, `text-xl` page titles. Sentence case, never spaced capitals.
  - One primary action per view (`variant="primary"`); `secondary` is the
    default. Status color always comes with a word or icon.
  - Every list has its loading (Skeleton of the real shape), empty
    (EmptyState with the action that fills it) and error states. Mutations
    confirm with `toast`, never only with a toast for important information.
  - Keyboard: everything reachable and visibly focused; icon-only buttons get
    an `aria-label` and usually a `Tooltip`; dialogs need a `DialogTitle`
    and usually a `DialogDescription` (it becomes their accessible
    description). A loading `Button` keeps its label as its accessible name.
  - Redirect targets from the URL go through `safePath` (`lib/safe-path.ts`).
  - Check your screen in light and dark, at 1440 and 390 px, before you call
    it done.
- **Collaborator colors**: `presenceIndex(userId)` (FNV-1a, 1–8) picks the
  `presence-N` token for avatars and cursors; the server can compute the same.

## Sandbox compiler (`crates/romulus-sandbox`)

The API holds one `Compiler`, behind `compile::SqlCompiler` (`AppState::
compiler`): never call `Compiler::start` elsewhere (one process per pool
prefix). `SqlCompiler::sandbox(&config)` starts it in the background and
retries until the sandbox answers, so the API boots without it; until then
compiles answer 503. `ROMULUS_SANDBOX_PREFIX` (default `romulus_sandbox`)
names the pool: Playwright's API uses `romulus_e2e`. `compile(sql)` for
uploads, `compile_for(draft_id, sql)` for drafts (coalesced per draft: a
superseded caller gets `compile::Rejected::Superseded` and should drop it).
Both return a `Compiled`: `model` (canonical, `database` = `"sandbox"`: set
the real name) or `diagnostics`, each with a stable `code` and a `Span` in
bytes, UTF-16 (CodeMirror/Yjs) and line/column. A sandbox failure is a 503,
never stored. `/api/health` reports `sandbox` (`ok`, `unavailable`, `off`)
from `Compiler::check_admin`. Tests use `SqlCompiler::from_fn`.

- **Pure tooling**: `parse(sql)` → statements with spans and `Key`s
  (`table:public.users`, `fk:public.orders.x_fkey`, `comment:column:…`;
  `Key` round-trips through its string form). Unnamed objects are keyed by
  the exact name Postgres gives them (suffixes included), and
  `Statement::keys` lists every object a statement touches: the
  statement-level merge and graph edits build on that (identity rules and
  limits in `key.rs`). `parse` never fails: each broken statement is a
  located error, the rest are still returned. `allowlist::check` is the
  server-side DDL gate.
- **Calling it from handlers**: `compile` is cancel-safe (a dropped request
  never leaves a sandbox database dirty) and limited to
  `upload_concurrency` of the pool so uploads cannot starve drafts;
  per-user/org rate limits on uploads are the API's to add. Only results
  the text alone determines are cached.
- **What the compile adds, never in the model**: stubs for functions the SQL
  calls but does not define (created where Postgres would look; expressions
  print as on the real database), policy roles rewritten to PUBLIC and
  restored, missing schemas, materialized views `WITH NO DATA`, statements
  recreating extension objects (PostGIS's `spatial_ref_sys`…) skipped with
  an info diagnostic. `crates/romulus-sandbox/src/prepare.rs` documents it.
- **`render_sql(&Schema)`** is the SQL for a draft started from a JSON
  version: remus-sql plus fixes (policy parentheses, index order), tested to
  compile back to the same model. Use it, not `remus_sql::render`.
- **One process per pool prefix** (`SandboxConfig::prefix`, an advisory
  lock enforces it): the API uses `romulus_sandbox`, the crate's tests
  `romulus_test*`. Queries to the sandbox are runtime `sqlx::query`/`raw_sql`
  (not the macros: it is not the app database).
- **Tests**: `just sandbox` starts only the sandbox (no Zitadel); the
  integration tests read `ROMULUS_SANDBOX_URL` from `.env` and are skipped,
  loudly, without it. CI starts the image itself.
- **Operating the sandbox** (memory caps, OOM behaviour, why the extensions
  are trusted): `deploy/sandbox/README.md`. Wire `Compiler::check_admin` into
  the API's health check: `PrefixInUse` there means another process took the
  pool and this one has stopped compiling.
- Known remus gaps (documented on `render_sql`): `NOT VALID` CHECKs come back
  validated, stand-alone range types are not modelled; its other workarounds
  go away with the remus release fixing remus#16.

## Migration imports (`imports`, `romulus_sandbox::import`)

A folder of migrations becomes the history, one version per migration
(origin `import`, `source_kind = 'migration'`, message = file name, source =
the up SQL, a `version_migrations` row: id, path, format, checksum, what was
skipped). docs/MVP.md "Next: migration files" has the decisions.

- **Pure part** (`romulus_sandbox::import`): `plan(files, format)` detects the
  `Format`: `Tool(romulus_migration::Format)` (the tools migrations are
  generated for: their one list, names included) or `Read(ReadOnly)`
  (`node_pg_migrate`, `alembic`, `django`, `rails`, read only); the stored
  `migration_format` (`domain::MigrationFormat`) keeps the same names. It orders, keeps up sections (blanking the rest, so lines
  stay the file's) and lists ignored files; `analyze(sql)` classifies
  statements (`statements.rs`: skipped and reported, stripped, rewritten,
  kept) and unwraps DDL-only `DO` blocks (`plpgsql.rs`). Exported files
  (Alembic, Django, Rails) are split on their headers (`export.rs`).
- **Replay** (`Compiler::replay(migrations, from, on_event)`): one session,
  one transaction, rolled back; the whole script once (stub corrections),
  then a migration at a time with a model read from `from` on (earlier ones
  are already versions). Import mode of the allowlist (`check_import`) lets
  through only what the rewrites produce: functions are ALWAYS stub bodies
  (a test asserts no import body reaches the script; never relax it: trusted
  extensions install as superuser). Remus's introspection empties
  `search_path`: the replay resets it after each read.
- **API** (`imports`): preview (static, no sandbox), import (org-scoped,
  NDJSON progress with `Accept: application/x-ndjson`, else the report) and
  `POST /api/databases/{database_id}/imports` (bearer `push`). Bodies:
  multipart (file name = path), zip, or one SQL text (`?filename=`).
  Refusals before the replay are ordinary errors (`has_versions`,
  `migration_changed`, `migration_out_of_order`, `too_many_migrations`,
  `import_failed` for a syntax error); a replay failure is 422 `import_failed`
  (`details: ImportFailure`, lines in the file) or the stream's `failed`
  event. Models are staged as the replay reads them (`import_staging`; the
  replay waits on a bounded channel, so the job holds one at a time) and
  `versions::store::store_import` moves them into versions, all or none,
  numbered under the databases row lock. The job runs in its own task: a
  client leaving does not stop it. CI uses the NDJSON progress (blank
  keep-alive lines; the plain report can be cut by a proxy). Before the sandbox it waits for its turn
  (`state.imports`, `imports::Turns`: organizations round-robin, a
  `queued` event); ids, paths and names over what the tables store are
  refused before the replay (`invalid_migrations`). Guarded statements
  unwrapped from `DO` blocks ignore exactly their block's conditions.
- **Tests**: `tests/sandbox/import.rs` (replay, hostile inputs),
  `tests/sandbox/fixtures.rs` (one history in every format, real Alembic,
  Django and Rails exports in `tests/migrations/`), `tests/api/imports.rs`
  (the fake compiler: `TestApp::compiling`), `e2e/imports.spec.ts`.

## Removed versions, approvals, the audit log

docs/MVP.md "Review round 1" items 3 and 4 have the decisions.

- **Removed versions** (`versions::removal`): `versions.removed_at` hides a
  version; every read of versions filters `removed_at IS NULL` ("the latest
  version" is the latest live one), except a draft's base read by id
  (`drafts::versions::by_id`: it may be removed). A new version's number is
  `databases.last_version_number + 1` (a trigger keeps it), never
  `max(number) + 1`. The web reads `Database.versions` (how many),
  `next_number` and `missing` (numbers without a live version: `neighbours`,
  `wasRemoved` in `features/databases/queries.ts`), never `latest.number`
  as a count. Restorable for `RESTORE_DAYS`, purged by `removal::purger`.
- **Audit log** (`audit`): `audit::record(tx, &Entry)` in the transaction
  of what it records (`NewVersion::audit` for a version being stored);
  `audit_events` refuses updates and deletes.
- **Approvals** (`drafts::approvals`, `collab::review`): the room tracks
  editors (first update changing `sql` → `draft_editors`) and approvals
  (rows in `draft_approvals`, each with the SHA-256 of the text approved),
  sends `ServerEvent::Review`, and answers `Content::approved_by` for the
  frozen text; publishing calls `approvals::gate`. Approving checks with
  the room first (`Hub::check_approval`), then writes, then
  `Hub::approved`. Web: `features/draft/review.ts`, `ReviewControl` in the
  editor bar, `useApprovalGate` in the publish dialog.

## Drafts and the collab server (`drafts`, `collab`)

A draft's content is a Y.Doc (`sql` Y.Text, `layout` and `groups` Y.Maps of
plain JSON values, docs/MVP.md) served live by `collab`; `drafts` is its REST
side (list with presence, one-click create, rename, soft delete, publish).

- **Hub and rooms**: `state.collab` (`collab::Hub`) holds one room per loaded
  draft (`collab::room`): an actor task owning the doc, the connections, the
  awareness states and a single writer task per draft (batched appends to
  `draft_updates`, compaction into `draft_snapshots`, the compile cache in
  `draft_compiles`). Rooms load on the first join or REST call, unload
  `Settings::idle_unload` after the last connection, and are written and
  closed (1012) by `Hub::shutdown` on graceful shutdown. One API instance.
- **Websocket** `GET /api/orgs/{org}/drafts/{id}/ws`: `csrf::SameOrigin` +
  `Member`. y-sync as y-websocket speaks it, plus **tag 100**:
  `collab::protocol::ServerEvent` JSON (welcome, compile, status, renamed,
  version_created, updated, save_failed), a message only the server sends
  (results never live in the doc: clients could forge them). Decoding runs in the socket task behind
  `catch_unwind` (yrs debug-asserts on client ids past 2^53). Close codes:
  1012 restart, 4000 too slow, 4001 membership changed (reconnect), 4404
  deleted (final: y-websocket treats 4400–4499 as such).
- **Rights**: reviewers' (and everyone's, unless the draft is open) document
  updates are dropped; awareness passes, sanitized (`collab::awareness`):
  `user` = the server's stamp `{id, name, role, presence, color, colorLight}`
  (presence tokens), other fields limited to `cursor`, `pointer`,
  `selection`; a connection only speaks for the Yjs client ids it announced
  first. Freezing (publish) drops edits, then `SyncStep1` to everyone on
  unfreeze brings them back.
- **Compile on type**: through the one `state.compiler` (`SqlCompiler::compile_for`,
  coalesced per draft; `AppState::with_compiler` hands it to the hub too);
  debounced 300 ms (at most 1 s).
  A rename typed in the text (same `CREATE` statement index, the last
  compile that succeeded vs this one) moves the layout and group entries,
  written by the server. `draft_compiles.good_model` is the graph while the
  text has errors.
- **Versions from drafts** go through `versions::store` with the draft's
  `VersionLayout` and `expected_base`: base moved → 409 `stale_base`, and
  the editor offers "Update draft". Creating a version (upload, push,
  publish) calls `collab.version_created`; deleting a database calls
  `collab.database_deleted`.
- **Update draft** (`POST …/drafts/{id}/update`, `drafts::update`): freezes
  the room (`LiveStatus::Updating`), merges with `romulus_sandbox::merge`
  (statement-level: atoms per object, "theirs changed" decided on
  `render_sql` of the two models, ours against `drafts.base_sql`, the text
  the draft is relative to), writes the merged SQL with `Hub::rewrite`
  (`patch::changes`, like Tidy; allowed while frozen for an update) and the
  layout with `Hub::merge_layout` (which waits until everything is written),
  then moves `base_version_id`, stores the new `base_sql`, broadcasts
  `updated`. Statements under the graph edits' marker merge by their own
  keys; the marker line itself never moves.
  Conflicts are blocks of SQL comments (`-- <<<<<<< yours` … `-- =======
  vN` … `-- >>>>>>> vN`); `SqlCompiler` refuses text with any
  (`Code::Conflict`), which is what blocks publishing. The web app finds
  them with `features/draft/conflicts.ts` and draws them in CodeMirror
  (a `StateField`) with Keep yours / theirs / both.
- **Tests** (`tests/api/live.rs`): `live_app(db, quick())` (`draft_compiler()`:
  a table per `CREATE TABLE`, an error at `ERROR`), `serve(&app)` for a real
  socket, `Client::join(addr, path, &session)` (a y-websocket client made of
  yrs: `edit`, `announce`, `until`, `settle`, `events`, `awareness`),
  `version(db, database, n, sql, layout)`, `stored_sql(db, draft)`;
  `app.state.collab.shutdown()` writes everything before asserting on rows.
- **Web** (`features/draft`): `DraftSession` (doc, y-websocket provider,
  server events, compile state, one `Y.UndoManager` over sql+layout+groups
  tracking `LOCAL` and y-codemirror's origin; `PLACED` writes, the graph
  placing tables, are never undone), React hooks in `use-session.ts`
  (`useSyncExternalStore` over Yjs), CodeMirror extensions in `collab.ts`
  (yCollab, diagnostics, remote carets in tokens). The editor is the
  full-screen route `routes/$org_.$db.drafts.$draft.tsx`. The database page
  has `NewDraftButton` and `OpenDrafts` in its header and `<DraftsSection>`
  in `DraftsSlot` (History tab). `SchemaViewer` takes `picked`/`onPickedChange`
  (shift- or ⌘-click), `graphControls`, `graphEmpty`.
- **Schema edits from the graph** (milestone 5, docs/MVP.md "Schema edits
  from the graph append SQL"): the graph only emits `GraphEdit`s (data,
  decisions included: CASCADE, USING, the FK's name, all planned from the
  model in `edits/plan.ts`); `DraftSession.editSchema(edit, shownModel,
  position?)` appends `sqlFor(edit)` under `-- edits from the graph` (written
  once, at the end) and moves the layout/groups (renames, new tables) in ONE
  `LOCAL` transaction between `undo.stopCapturing()` calls: one ⌘Z each. A
  dropped table keeps its layout entry (pruned at publish). Nothing about an
  edit is stored as truth: `graph-edits.ts` `judge`s every edit of the tab
  again from the text (its statements still there?) and the compile (shows
  it? an error inside its statements?): pending (applied to the model shown,
  marked), failed (still shown, but drops; the compile's message and "Undo
  it" on the element), reverting (undone, the model not caught up yet: its
  entities put back from the snapshot taken when it was made), idle.
  `useGraphEdits` gives the viewer the model to show and the `editor`.
  Reviewers get no `editor`: no affordance at all. The SQL pane lights what an
  edit wrote (`showGraphEdit`). Every statement kind is snapshot-tested, and
  `romulus-sandbox/tests/graph_edits.rs` checks the snapshots parse and pass
  the allowlist: change a statement, run vitest with `-u`, both follow.
- **Groups on the graph** (docs/MVP.md "Review round 1", item 5): layout, not
  schema, but handed out the same way: `editor.groups` (`GroupEditor`, built
  in `useGraphEdits` over `DraftSession.createGroup/updateGroup/deleteGroup/
  assignGroup/placeTables`, each one `LOCAL` step between `stopCapturing`).
  Pure geometry and membership in `graph/groups.ts` (frames follow their
  tables; a table dropped in a frame joins it, outside the others' frame
  leaves it; a group left empty goes). The UI is `graph/group-node.tsx` (the
  label is the drag handle; zoomed out, the area label) and
  `graph/editing/groups.tsx` (toolbar panel, pick bar, drop hint); G groups
  the picked tables. A drag writes once, on drop: a group's tables in one
  transaction. The draft's layout reaches the graph with the parent's next
  render (rAF), so a drop is held (`dropped`, `heldWhile`) until the layout
  prop is a new object, which then has the move: no jump back, and the hold
  cannot outlive an undo. Drag positions are whole units, as stored.
  Smoothness at 300 tables (`e2e/perf/groups.perf.spec.ts`, dev editor
  `?fixture=large&groups=schemas`): edges moving with both ends are shifted,
  many edges leaving a drag are routed around their ends only, a drop routes
  only `affectedEdges` a render later (deferred), unchanged node and edge
  objects are reused.
- **Tidy**: `POST …/drafts/{id}/tidy` [developer] renders the compiled model
  (`drafts::tidy::canonical_sql`: `render_sql` without remus's header),
  refuses text with errors (422 `compile_failed`) and a rewrite that compiles
  to another model (422 `tidy_unsafe`: foreign tables…), then
  `Hub::rewrite(draft, sha256_of_the_text_it_read, collab::patch::changes(old,
  new))`: the room applies the smallest edit (lines, then SQL tokens) in one
  transaction if the text is still that one (`RewriteError::Changed`
  otherwise: read again; the handler retries 3 times, then 409
  `draft_changed`). Carets in untouched text survive. **Any server rewrite of a
  draft's SQL** (Update draft's merge too) must call
  `Roots::note_rewrite(txn, reason)` in its own transaction (`Hub::rewrite`
  does): clients clear their undo history and graph edits when `meta.rewrite`
  changes (a pre-rewrite undo would delete characters the rewrite reused),
  and tell the person ("Undo history was cleared by Tidy"; `reason`
  `update` has its own copy).
- **Duplicate markers**: two editors making their first graph edit before
  seeing each other's append two markers; the room removes all but the first
  (`doc::extra_markers`, a server edit) before each compile of an open draft.
- **Without the stack**: `/app/dev/editor` (`?role=reviewer`,
  `?fixture=empty|large`) runs the real editor on a `DraftSession` with
  `local` (no socket; its "compile" replays the graph edits found in the
  text onto the blog fixture; "Fail the next edit", "Someone types");
  `e2e/dev/graph-edits.spec.ts` drives it. The two-person flows with the
  sandbox are `e2e/graph-edits.spec.ts`.
- Pitfalls: y-protocols ignores a stranger's first awareness state at clock
  0 (the session sets a field before connecting); a compile event can arrive
  before the doc (the session checks it again on sync); compare editor text
  with `textContent` in Playwright (`toHaveText` ignores line breaks). The
  graph's editing context must stay stable across drags (its actions read
  the latest state through a ref): a value changing per frame redraws every
  node. Drafts are seeded with their version's model as the first compile:
  a test whose fake compiler disagrees with the version's model sees that
  model until the text changes.

## Testing

- API: integration tests in `crates/romulus-api/tests/api/<module>.rs` (add a
  `mod` line in `main.rs`), `#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]`
  with `db: PgPool`, `TestApp::new(db)` → `.get`, `.post_json` (adds CSRF
  header and Origin), `.send`; responses have `.status`, `.json()`, `.code()`.
  Fixtures: `harness::{user, org, member, database}`; signed in:
  `signed_in(&db, "Alice")` or `session(&db, user)`, then
  `app.as_user(&session).get/post/patch/delete(…)`; `Team::new(&db, "acme")`
  gives an org with one session per role plus an outsider; `api_token(&db,
  database, scope)`, `model(&["users"])` / `model_json(..)` (remus models),
  `app.push(database, token, content_type, body)`, and
  `TestApp::compiling(db)` with a fake compiler (`CREATE TABLE x` makes a
  table, `oops` is a located syntax error). `idp::FakeIdp`
  plays the OIDC provider (`TestApp::with_config(db, idp.config())`). Unit
  tests inline in `#[cfg(test)] mod tests`, named as assertions.
- Web: Vitest + Testing Library next to the code (`*.test.tsx`) for behaviour;
  Playwright in `web/e2e/` for flows, against the production build on the
  preview origin with its own API (`playwright.config.ts` starts both, or
  reuses them). Prefer role/label queries; they double as an accessibility
  check. `e2e/helpers.ts`: `devSignIn(page, email, next)` for most tests,
  `ssoSignIn(page, user)` (from our sign-in page) or `zitadelSignIn(page,
  user)` (already on Zitadel's) with `zitadelUsers()` for the real login,
  `emailedCode(email)` for a code Zitadel emailed (sign-up),
  `createOrg`, `inviteLink`, `unique(…)` (the stack's database is shared
  between runs: name things uniquely).
- Every feature PR: its endpoints tested (happy path, permission denied per
  role, validation), its screens with at least one Playwright path. `just check`
  green before pushing.

## Pitfalls

- Cookies are not port-scoped: two stacks on localhost share cookies. The
  session cookie name carries the slot (`ROMULUS_SESSION_COOKIE`); Zitadel's own
  cookies collide anyway, so use separate browser profiles (or Playwright
  contexts) to sign in to two stacks at once.
- Zitadel routes by `Host` and the issuer must match exactly
  (`http://localhost:$ROMULUS_ZITADEL_PORT`, not 127.0.0.1). A container
  calling it uses `http://zitadel:8080` with `Host: localhost:<port>`; a
  containerised API (CI e2e later) can't use the issuer URL as is. Refresh
  tokens need the `offline_access` scope.
- `docker compose up --wait` returns while the one-shot provisioner still
  runs; `just up` waits on it. If `.dev/zitadel` is deleted but the volumes
  kept, `just up` regenerates the client secret; if the PAT volume is lost,
  `just nuke`.
- React StrictMode mounts effects twice: anything owning a resource (Yjs
  provider, CodeMirror view, websocket) is created in an effect with cleanup,
  never in `useMemo`/render. A provider rebuilt in render silently stops syncing.
- React Flow: the multi-select key comes from keydown events, so handle
  selection in `onNodeClick`; controlled nodes need explicit `width`/`height`
  or the minimap draws nothing; don't name a node type `group` (built-in
  styles); drag stop can fire before React re-renders, so read drag moves from
  a ref.
- CodeMirror block widgets must come from a `StateField`, not a view plugin.
  Adding npm deps while Vite runs can load `@codemirror/state` twice
  ("Unrecognized extension value"): restart with `npx vite --force`.
- `openapi-fetch` must resolve `fetch` per call (it does in `client.ts`), or
  test stubs and mocks are ignored.
- TypeScript stays on 5.x until typescript-eslint and openapi-typescript
  support 7; ESLint stays on 9 for jsx-a11y.
