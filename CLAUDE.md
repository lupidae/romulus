# romulus

DBA as a service: an organization keeps its Postgres schemas here, versions
them, reviews diffs and designs the next version together (SQL and table graph,
live cursors). The spec is [docs/MVP.md](docs/MVP.md); read it before a feature.
remus (the CLI twin, `remus-core` on crates.io) is where the schema model lives.

## Repo map

```
crates/romulus-api/   axum API (lib + bin): src/<module>/mod.rs per feature, tests/api/
crates/romulus-diff/  model diff over remus-core, no I/O, builds for wasm; `utoipa` feature for the API
migrations/           sqlx migrations, timestamp-named; .sqlx/ = offline query data
web/                  Vite + React SPA served under /app (see "Web" below)
site/                 the static homepage (tokens copied from web/; see site/README.md)
deploy/zitadel/       the provisioner that sets Zitadel up on `just up`
scripts/dev-env       per-worktree ports and .env;  scripts/check-oidc  proves SSO works
docker-compose.yml    postgres (app), sandbox-postgres, zitadel + zitadel-db, provisioner
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
same password.

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

- **One origin.** Production: the API serves the homepage at `/`, the SPA at
  `/app`, itself at `/api`. Dev mirrors it: Vite serves `/app` (base and router
  `basepath`) and proxies `/api` (REST and websockets) without rewriting `Host`,
  so cookies are first-party and the API sees the real `Origin`.
- **Sandbox Postgres is 17**, because `pg_query` 6.2.1 bundles libpg_query
  built from PostgreSQL 17.7. Bump both together. The app database is 18
  (`uuidv7()` ids).
- **Zitadel v4.19.4 with its built-in login (v1)**, one container, no proxy.
  Login v1 disappears in v5: upgrading means adding the `zitadel-login`
  container and a path-routing proxy. The OIDC callback is
  `$ROMULUS_PUBLIC_URL/api/auth/callback` (and the API port's, for direct
  testing); post-logout URIs are `/app/signin`, `/app/` and `/`.
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
  `organizations_slug_not_reserved` check and the `*_email_nfc` checks.
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
- **State**: `AppState { db, config, oidc, membership }` derives `FromRef`:
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
- **Secrets handed out** (session ids, invitation links, later API tokens):
  `token::generate()` (32 random bytes, base64url) and store
  `token::hash(..)` (sha256, fits the `*_hash bytea` checks).
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
                         Sidebar*
src/components/shell/    AppShell (top bar, org switcher, user menu, nav, panel), PageHeader,
                         PageBody, status pages (404/500), the sign-in preview
src/components/icons/    Iconoir only, generated: `npm --prefix web run icons:add -- <name>`
src/features/<module>/   per-feature code: queries (queryOptions), mutations, components
src/features/schema/     SchemaViewer / SchemaGraph / SchemaText (graph + SQL panes), remus model
                         types (parseSchema), fixtures; /app/dev/viewer shows them
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

## Testing

- API: integration tests in `crates/romulus-api/tests/api/<module>.rs` (add a
  `mod` line in `main.rs`), `#[sqlx::test(migrator = "romulus_api::db::MIGRATOR")]`
  with `db: PgPool`, `TestApp::new(db)` → `.get`, `.post_json` (adds CSRF
  header and Origin), `.send`; responses have `.status`, `.json()`, `.code()`.
  Fixtures: `harness::{user, org, member, database}`; signed in:
  `signed_in(&db, "Alice")` or `session(&db, user)`, then
  `app.as_user(&session).get/post/patch/delete(…)`; `Team::new(&db, "acme")`
  gives an org with one session per role plus an outsider. `idp::FakeIdp`
  plays the OIDC provider (`TestApp::with_config(db, idp.config())`). Unit
  tests inline in `#[cfg(test)] mod tests`, named as assertions.
- Web: Vitest + Testing Library next to the code (`*.test.tsx`) for behaviour;
  Playwright in `web/e2e/` for flows, against the production build on the
  preview origin with its own API (`playwright.config.ts` starts both, or
  reuses them). Prefer role/label queries; they double as an accessibility
  check. `e2e/helpers.ts`: `devSignIn(page, email, next)` for most tests,
  `ssoSignIn(page, user)` with `zitadelUsers()` for the real login,
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
