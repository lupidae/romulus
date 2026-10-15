# romulus — MVP draft

DBA as a service. An organization keeps its Postgres databases in romulus,
versions their schema, reviews changes as diffs, and designs the next version
together: SQL on one side, a table graph on the other, both editable, everyone's
cursor visible.

remus is the twin: it reads a live database and pushes what it finds. romulus is
where that schema is kept, discussed and changed.

## Scope

**In the MVP**

- Sign in through a self-hosted SSO (OIDC).
- Organizations, invitations, three roles.
- Databases inside an organization.
- Versions: immutable schema snapshots, uploaded as SQL, or pushed by remus from
  CI.
- Diff between any two versions of a database.
- Drafts (branches) per database: SQL text pane + graph pane, both editable,
  live cursors, movable tables, groups, comments visible.
- Schema edits from the graph: rename / add / drop table and column, edit a
  comment, draw a foreign key.
- Publishing a draft as the next version, in one step.
- A product homepage.

**Later** (the model should allow them, the MVP does not build them)

- Fake data generation.
- Index suggestions, hot path detection, performance-oriented design help.
- Billing (the admin role exists, payment does not).
- Review comments on a draft, approvals before publishing.

## Roles

Per organization, one role per member.

| | admin | developer | reviewer |
|---|---|---|---|
| read databases, versions, diffs | ✓ | ✓ | ✓ |
| see the draft live (read-only, cursor visible) | ✓ | ✓ | ✓ |
| edit the draft, create versions, upload, API tokens | ✓ | ✓ | |
| create / delete databases | ✓ | ✓ | |
| invite, change roles, remove members | ✓ | | |
| billing (later) | ✓ | | |

The collab server enforces read-only on the websocket, not only the UI: a
reviewer's Yjs document updates are dropped server-side, while their awareness
updates still go through (that is how their cursor stays visible). The server
stamps every awareness state with the authenticated user's id, name and colour;
what a client claims about who it is is ignored.

Yjs docs live in the API process's memory (loaded on first connect, flushed and
unloaded after the last disconnect), so the MVP runs **one API instance**.
Scaling out later means routing each draft to one instance.

## Stack

| Layer | Choice | Why |
|---|---|---|
| SSO | **Zitadel** in docker-compose | OIDC, organizations built in, customer IdPs (SAML/OIDC) brokered later |
| Backend | Rust, **axum**, **sqlx** (Postgres, compile-time checked queries, migrations) | |
| Collab server | **yrs** (Yjs in Rust) inside the API process, y-sync protocol over websocket | one process, one auth check, shares the DB pool |
| Schema model | **remus-core** from crates.io | the model, JSON, and the SQL/DBML/Mermaid emitters for free |
| Frontend | **Vite + React + TanStack Router + TanStack Query** | typed routes and loaders |
| Text pane | **CodeMirror 6 + y-codemirror.next** | first-class Yjs binding, remote cursors built in |
| Graph pane | **React Flow (@xyflow/react)** + **elkjs** for first layout | custom table nodes, group nodes, viewport |
| SQL parsing (client) | **libpg_query** via wasm (`@pgsql/parser` / `libpg-query`) | Postgres's own parser, with statement locations |
| Homepage | static `site/`, Cloudflare, like remus | |

Auth is a backend-for-frontend: the API does the OIDC code flow and sets an
httpOnly session cookie. The SPA never holds a token, and the websocket is
authenticated by the same cookie. remus and CI use per-database API tokens.

- **Zitadel is identity only.** Organizations, memberships and roles live in
  romulus's tables; Zitadel organizations are not used in the MVP (customer IdP
  brokering later may change that).
- docker-compose provisions Zitadel automatically (instance, project, OIDC app,
  a test user) so `compose up` and the CI end-to-end tests need no clicking.
- **Cookie-authenticated endpoints are CSRF targets.** The session cookie is
  `SameSite=Lax`; mutating REST calls require a custom header and a matching
  `Origin`; the websocket upgrade checks `Origin` (Cross-Site WebSocket
  Hijacking).
- **Invitations are copy-link** in the MVP: the admin gets a one-time link, no
  email provider. Accepting requires being signed in with a **verified** email
  equal to the invited one. A signed-in user with no organization lands on
  "create an organization".
- A role change or removal closes that member's open websockets.
- **Types shared with the SPA** are generated from Rust (OpenAPI via `utoipa` →
  `openapi-typescript`), never written twice by hand.

## The core design: who is the source of truth

Both panes are editable, so one of them has to be canonical, or every concurrent
edit is a merge conflict between two representations.

**Proposal: the SQL text is canonical; the graph is derived from it, plus a
layout overlay.**

The draft's Y.Doc holds:

```
Y.Doc (one per draft)
├─ sql:     Y.Text                       canonical DDL, edited by both panes
├─ layout:  Y.Map<qualified name, {x, y}>
├─ groups:  Y.Map<group id, {name, color, members: qualified names[]}>
└─ awareness (not persisted): user, color, text cursor, graph pointer, selection
```

- **Text pane** edits `sql` directly. CRDT merge, cursors via y-codemirror.
- **Graph pane** renders the model compiled from `sql`. Graph edits fall into
  two kinds:
  - *Layout edits* (moving a table, grouping it) only touch `layout` / `groups`.
    They never change the schema, so they never conflict with text edits.
  - *Schema edits* (add/rename/drop a table or column, edit a comment, draw a
    foreign key) become **text edits on `sql`**: parse with libpg_query, find the
    statement and its location, splice the change. They go through the same CRDT
    as typing, so concurrent editing just works.
- Layout and groups are keyed by **qualified name** (remus's rule: identity is
  names, never OIDs). A rename moves the entry in the same transaction.

**Compiling SQL into the model: run it in Postgres.** Instead of writing a
SQL-to-model converter (remus deliberately has no SQL parser), the server applies
the draft SQL inside a transaction on an isolated **sandbox Postgres**, runs
remus's `introspect.sql`, and rolls back. The result is exactly the model remus
would produce from a real database: enums, domains, partitions, generated
columns, comments, all with Postgres's own semantics and error messages. Debounced
(~300 ms), and the result is broadcast to everyone in the draft.

Sandbox rules: separate cluster, no superuser, `statement_timeout`,
`lock_timeout`, a size cap on the SQL, always rolled back. Only DDL statements
are allowed, checked **server-side** with the `pg_query` crate (the client
check is a convenience, never trusted): an allowlist of statement nodes
(`CREATE TABLE/TYPE/DOMAIN/SCHEMA/VIEW/INDEX/POLICY`, `ALTER TABLE`,
`COMMENT ON`, `CREATE EXTENSION` for allowlisted names), no `DO`, no DML, no
`COPY`.

**Concurrency**: `CREATE DATABASE` cannot run in a transaction and is slow, and
two compiles in one database block each other on identical names. The sandbox
is a **pool of N pre-created empty databases**, each used by one compile at a
time. Compiles are debounced and coalesced per draft (a newer request replaces a
queued one) and rate-limited per draft.

The sandbox Postgres major version is pinned, and the `pg_query` / libpg_query
versions match it.

**Dependencies the SQL does not create (auto-stub).** Real schemas reference
roles, functions and extensions that are not in the text, and remus's SQL output
deliberately omits them, so a draft opened from a pushed version would otherwise
fail on its first compile. Before applying the draft, the compiler prepares the
sandbox transaction:

- **Roles** (cluster-global, and the sandbox user cannot create them): policy
  `TO <role>` clauses are rewritten to `PUBLIC` for the compile, and the
  original roles are put back into the compiled model from the parse tree.
- **Functions** referenced in defaults, checks, generated columns, indexes or
  policies: found by walking the parse tree for `FuncCall` nodes that do not
  resolve to a built-in, and stubbed in a `romulus_stub` pass (argument types
  from the casts in the call, return type from the column or context type).
  Stubs are never part of the model. If the heuristics prove too weak, adding
  signatures to remus's payload is the fallback, discussed with remus first.
- **Extensions**: the sandbox image ships a fixed set of common extensions
  (pgcrypto, uuid-ossp, citext, hstore, pg_trgm, postgis, vector…). An
  unavailable one is a clear compile error, not a crash.

Compile errors carry the Postgres message and position, mapped back to the draft
text. The last compile result (model or errors) is cached per draft, so a client
that reconnects gets the graph immediately.

The client parses too (libpg_query wasm), for instant syntax errors and
statement locations. The server compile decides what the graph shows.

The other remus outputs (JSON, DBML, Mermaid) are shown **read-only** in the
text pane, generated from the compiled model. Editable JSON would make two
canonical sources again.

## Where layout, groups and comments live

Schema in SQL, presentation in Yjs. The two never overlap, so there is nothing to
keep in sync.

| What | Canonical home | Why |
|---|---|---|
| tables, columns, types, constraints | `sql` (Y.Text) | it is the schema |
| comments on tables/columns/types | `sql`, as `COMMENT ON … IS '…'` | Postgres already stores them; remus already carries them (`comment` fields), so the graph shows them for free |
| `-- line comments` in the text | `sql` only | not in the catalog; shown in the text pane, not the graph |
| positions | `layout` Y.Map, key = qualified name | not schema; a drag must not show up as a text edit for everyone typing |
| groups | `groups` Y.Map, members = qualified names | same |

- Editing a comment in the graph adds or updates the `COMMENT ON` statement in
  the text.
- A table missing from `layout` gets a position from elkjs next to its
  foreign-key neighbours, and that position is then written into the map.
- **Renames**: a graph rename knows the old and new name and moves the layout
  and group entries in the same Yjs transaction. A text rename looks like
  drop + add after compile. If the new entity comes from the same `CREATE TABLE`
  statement (same statement index, matched by libpg_query locations), its layout
  entry is moved over. Otherwise the orphaned entry is kept until the version is
  saved, so undo brings the table back where it was.
- **Schema edits from the graph rewrite in place**: the edit finds every
  reference through the libpg_query parse tree and its locations (foreign keys
  in other statements, indexes, `COMMENT ON`, policies, view bodies) and
  rewrites them in one Yjs transaction. It never appends `ALTER … RENAME`, so
  the text stays a clean description of the schema, not a migration log.
- **Undo**: one `Y.UndoManager` per client over `sql`, `layout` and `groups`,
  tracking local origins only, so a graph edit that touches text and layout
  undoes as one step and nobody undoes someone else's typing.
- A version stores `source` (SQL) and `layout` (positions + groups) side by side.
  Export can embed the layout in the SQL as `-- @romulus` comments for people
  who want a single file in git; import reads it back. Not in the MVP.

## Drafts and versions: the UX

Versions are one straight line per database (v1, v2, v3…). Drafts are branches
off that line. Branching should be invisible: nobody has to know the word.

- **Database page**: the latest version (graph + SQL), the version history, and
  the open drafts with who is in them ("add billing · Alice, Bob editing").
- **New draft**: one button. No dialog; it starts from the latest version, is
  named "Draft from v12", and can be renamed later. The editor opens straight away.
- **Publish**: one button in the editor, "Publish v13". The dialog shows the diff
  against v12 and an optional message. Confirm creates v13 and closes the draft.
- **Someone published first**: the button turns into "Update draft (v13 is out)".
  It does a three-way merge of the SQL (base = v12 source, theirs = v13,
  ours = draft) and a key-by-key merge of layout and groups. Clean merges apply
  silently. Conflicts appear as inline blocks in the text pane with
  "keep mine / keep theirs" buttons. Publishing is blocked until none are left.
- **The SQL merge is statement-level, not line-level.** v13 may come from a
  remus push, whose SQL is generated and formatted differently from the
  hand-written draft; a line diff3 would conflict on the whole file. Each side
  is split into statements (libpg_query) keyed by the object they define
  (`table:public.users`, `comment:column:public.users.email`,
  `fk:public.orders.orders_user_id_fkey`…). Objects are merged three-way;
  unchanged objects keep the draft's text, formatting and `--` comments; only
  objects changed on both sides become conflicts. When theirs comes from a push,
  their statement text is what `remus-sql` renders for that object.
- The merge runs on the server and is applied to `sql` as a minimal edit in one
  Yjs transaction (never delete-all + insert-all, which would kill everyone's
  cursors and concurrent typing). Conflict blocks live in the Y.Text as
  markers, so everyone sees them and they survive a reload; the compiler
  rejects text with markers, which is what blocks publishing. The draft's
  `base_version_id` moves to v13.
- **Publishing** freezes the draft first (status change broadcast; the server
  drops further updates), then compiles and stores exactly that frozen state.
  Version numbers are assigned under a lock on the database row, with
  `UNIQUE (database_id, number)`; a publish whose base is no longer the latest
  is refused and turns into "Update draft".
- A remus push creates a version directly (origin `push`), on the same line:
  it is what the real database looks like now.
- Drafts can be deleted. Published drafts stay readable from their version.

## Versions

A version is immutable:

```
version
  id, database_id, number (1, 2, 3… per database)
  model        jsonb   the remus JSON, authoritative
  source       text    what was uploaded (SQL or JSON), kept verbatim
  source_kind  'sql' | 'remus_json'
  origin       'upload' | 'push' | 'draft'
  layout       jsonb   snapshot of layout + groups at that time
  created_by, created_at, message
```

- **Upload SQL**: the only upload format in the MVP. Compiled through the
  sandbox, as above.
- **remus JSON**: accepted only from the push endpoint, as a machine path. It is
  lossless, whereas remus's SQL output can reference roles and functions it does not
  create, so it may not compile in an empty sandbox.
- **remus push**: on day one this needs no remus change:
  ```bash
  remus -f json | curl -fsS -H "Authorization: Bearer $ROMULUS_TOKEN" \
    --data-binary @- https://app.romulus…/api/databases/<id>/versions
  ```
  A `remus push` subcommand can come later (see "remus impact").
  A push whose canonical model (`Schema::canonical`) equals the latest version's
  creates nothing and answers `200 unchanged`, so a CI job pushing on every run
  does not flood the history. Push bodies have a size limit.
- **Save draft as version**: compile, store model + current SQL + layout.
- A draft starts from the latest version: its SQL comes from the version's
  source if it was SQL, or from `remus-sql` rendering the model if it was JSON.

DBML and Mermaid uploads are not supported: remus emitters are one-way, and
reading them back would give a poorer model.

## Diff

A diff is computed between two **models**, not two texts:

- Semantic: tables / columns / constraints / indexes / enums / policies added,
  removed, changed (type, nullability, default, comment…). Matched by qualified
  name, which also means a rename shows up as drop + add in the MVP.
- Shown three ways: a change list, the graph with added/removed/changed
  highlighted, and a side-by-side text diff of the rendered SQL.

Lives in its own crate, `romulus-diff`, over `remus_core::Schema`, with no I/O.
It could move into remus later (diffing two snapshots in CI).

## Data model (Postgres, sqlx migrations)

```
users            id, oidc_subject, email, name
sessions         id_hash, user_id, expires_at, created_at, last_seen_at, oidc refresh token (encrypted)
organizations    id, name, slug
memberships      org_id, user_id, role ('admin'|'developer'|'reviewer')
invitations      id, org_id, email, role, token_hash, expires_at, accepted_at, created_by
databases        id, org_id, name, slug, description, deleted_at        UNIQUE (org_id, slug)
versions         (above)                                                UNIQUE (database_id, number)
drafts           id, database_id, name, base_version_id, status ('open'|'published'|'deleted'),
                 published_version_id, created_by, created_at, updated_at
draft_compiles   draft_id, compiled_at, model jsonb | errors jsonb      last compile result
draft_updates    id, draft_id, update bytea, created_at       append-only Yjs updates
draft_snapshots  draft_id, state bytea, upto_update_id        compaction
api_tokens       id, database_id, name, token_hash, scope ('push'|'full'),
                 created_by, created_at, last_used_at, revoked_at
```

Invariant: an organization always has at least one admin (the last admin
cannot be demoted or removed).

## Repository layout

```
romulus/
  docker-compose.yml     postgres, sandbox-postgres, zitadel, api, web
  crates/
    romulus-api/         axum, sqlx, OIDC, REST, websocket + yrs
    romulus-diff/        model diff, no I/O
  migrations/
  web/                   vite + react + tanstack router
  site/                  homepage
  justfile               check, dev, migrate
  .github/workflows/     `just check` + end-to-end tests
  CLAUDE.md
```

## Testing

- `romulus-diff`: pure unit tests over `remus-core`'s `fixtures` feature.
- API: `#[sqlx::test]` databases; the sandbox compiler against the real sandbox
  image in compose (allowlist, stubs, timeouts, concurrent compiles).
- Web: component tests for the panes; Playwright end-to-end with **two browser
  contexts** editing one draft (cursors, concurrent text + graph edits,
  reviewer read-only, publish, update draft).

The libpg_query wasm and elkjs are loaded lazily, elkjs in a web worker: both
are heavy and only the editor needs them. Partitions stay collapsed under their
parent (remus's rule) and the graph has to stay usable at a few hundred tables.

## Milestones

1. **Skeleton**: compose up, log in through Zitadel, create an org, invite,
   roles enforced. `just check` green in CI.
2. **Databases & versions**: create a database, upload SQL (compiled through
   the sandbox, with auto-stubs), push remus JSON with curl, list versions,
   read-only graph + text of a version.
3. **Diff**: pick two versions, change list + highlighted graph + SQL diff.
4. **Collaborative drafts**: new draft, Yjs doc, text pane, compile-on-type,
   graph from the model, move tables, groups, comments shown, live cursors on
   both panes, publish.
5. **Graph → text editing**: rename / add / drop table and column, edit comment,
   draw a foreign key.
6. **Update draft**: three-way merge when someone published first, inline
   conflicts.
7. **Homepage** in `site/`.

Milestones 5 and 6 are the riskiest, so they come after a usable product (1–4).
All seven are the MVP.

## remus impact

- remus's `CLAUDE.md` puts "a server, realtime, storage, overlay files" out of
  scope. That still holds for remus itself: romulus is where they live, and
  the overlay (layout/groups keyed by qualified name) is the one remus already
  designed for.
- `remus push` would be the first network write in remus. Proposal: a `push`
  feature, off by default, so the static CI binary stays the same. Not needed for
  the MVP, since curl works.
- romulus depends on `remus-core` from crates.io; a new model field in remus is a
  romulus dependency bump.

## Decided

- Uploads: SQL only (remus JSON accepted on the push endpoint).
- SQL text is the source of truth for the schema; layout and groups live in Yjs.
- SSO: Zitadel. Router: TanStack Router.
- Graph → text schema edits are in the MVP.
- Branches are supported, as drafts, with one-click creation and publishing.
- Homepage lives in `site/`.
- Missing roles, functions and extensions are auto-stubbed in the sandbox;
  stubs never reach the model.
- "Update draft" merges SQL per statement, keyed by the object defined, on the
  server, applied as a minimal Yjs edit.
- Graph schema edits rewrite every reference in place, never append `ALTER`.
- Invitations are copy-link; no email provider in the MVP.
- Zitadel is identity only; romulus owns organizations and roles.
- One API instance in the MVP (Yjs docs in memory).
