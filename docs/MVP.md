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
- Billing (the admin role exists, payment does not). Since built for the hosted
  service: see "Billing" below.
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
| change billing (read: everyone) | ✓ | | |

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
├─ meta:    Y.Map: rewrite = {count, reason}, set by the server with each rewrite of `sql`
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
versions match it. (Since "Several Postgres majors" below: one sandbox per major
a database can target, one parser, 17's, for all of them.)

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
- **Schema edits from the graph append SQL**: each edit appends the matching
  statement (`ALTER TABLE … RENAME …`, `ADD COLUMN`, `ADD CONSTRAINT … FOREIGN
  KEY`, `COMMENT ON …`) under a `-- edits from the graph` marker, so Postgres
  itself carries a rename to every constraint, index, comment and view. A
  **Tidy** action rewrites the whole text in remus's canonical form (via the
  compiled model and `remus-sql`), as one Yjs transaction, for whoever wants a
  clean file again. Chosen over in-place rewriting (2026-10-15).
- **Undo**: one `Y.UndoManager` per client over `sql`, `layout` and `groups`,
  tracking local origins only, so a graph edit that touches text and layout
  undoes as one step and nobody undoes someone else's typing. A rewrite of
  the SQL by the server (Tidy, Update draft) clears every client's undo
  history: it reuses characters clients typed, elsewhere, and undoing an
  edit from before it would delete them wherever they are now. The server
  notes each rewrite in `meta`, in the rewrite's own transaction, so a
  client that syncs it later clears its history too.
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
- **Layout and groups merge key by key.** What one side changed comes from
  it. A table both sides moved keeps the draft's position: the people in the
  draft are arranging it now, a position is cosmetic, and moving tables
  under them would undo their work (v13's arrangement stays in its
  version). A group both changed merges field by field: a name or color
  only one side changed comes from it (v13 wins when both did); a member
  either side removed stays removed, a member either side added comes in;
  a table is never in two groups (it stays where the draft had it).
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
  name, which also means a rename shows up as drop + add in the change list
  (migrations rename: see "Migrations: renames and dependents").
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

Local development has a dev-only sign-in bypass (env-gated, refused unless
the public URL is local) so QA and end-to-end tests do not depend on Zitadel's
UI. In production the API serves the homepage at `/`, the SPA at `/app` and
itself at `/api`: one origin, which keeps the cookie and CSRF story simple.
Compiles are cached by SHA-256 of the SQL, so everyone in a draft costs one
sandbox run.

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

## Next: migration files (decided 2026-10-16, after the MVP is deployed)

Two features, built in this order:

1. **Import migrations → version history.** A database's history can be rebuilt
   from a folder of migrations: each migration becomes one version (message =
   file name, origin `import`), replayed cumulatively in the sandbox from an empty
   database. Formats: plain ordered SQL folders (sqlx, dbmate, golang-migrate,
   node-pg-migrate), Flyway (`V<ver>__<name>.sql`; repeatable `R__` ignored),
   Prisma (`migrations/<ts>_<name>/migration.sql`). Code-based tools (Rails,
   Django, Alembic) are imported from the SQL the tool prints itself
   (`alembic upgrade head --sql`, `manage.py sqlmigrate`, Rails structure dumps)
   via a documented exporter script; romulus never runs user code.
   Data statements (INSERT/UPDATE/DELETE/COPY, `DO` blocks) are **skipped and
   reported** per file, and so are temporary tables, views and sequences (a
   backfill's helpers, whatever builds them) with every later statement of the
   migration that only concerns them, names resolved as Postgres does
   (`pg_temp` first: a temporary table shadows a real one until dropped); a
   draft or an upload still refuses temporary objects; functions and triggers are allowed in import mode so later
   DDL compiles (stubs never reach the model). Import runs from the web (upload a
   folder or zip) and from CI (an endpoint taking the files with the same bearer
   token as push), and refuses a database that already has versions unless asked
   to append.
   Built (task MI); what the build decided on top:
   - **Function bodies never run.** Import mode creates the functions,
     procedures and triggers migrations define (later DDL needs them), but
     always with a fixed PL/pgSQL stub body of romulus's, whatever the
     language: the sandbox's trusted extensions install as a superuser, and a
     function of the import's could capture one of their calls. Trigger
     `WHEN` conditions are dropped, rules do nothing, `CREATE TABLE … AS`
     runs `WITH NO DATA`. Operators, casts, aggregates, languages and foreign
     data are skipped and reported.
   - **DDL wrapped in a `DO` block is unwrapped**, not skipped, when the block
     is only DDL guarded by an `IF [NOT] EXISTS` on the catalogs or an
     `EXCEPTION WHEN duplicate_…/undefined_…` handler: Active Record 7.1+
     writes every `create_enum` that way and Drizzle its foreign keys.
     Guarded statements run in a fixed wrapper ignoring exactly what the
     block ignored (its handler's conditions; "already exists" for an
     `IF NOT EXISTS`, "does not exist" for an `IF EXISTS`), so a Drizzle
     foreign key to a missing table still fails; the block's own code (its
     condition) never runs. Any
     other `DO` block is skipped and reported, and a later "does not exist"
     error names it.
   - A guard's handler may name any condition (pg_dump-made baselines guard
     every object with `duplicate_object OR duplicate_table OR
     invalid_table_definition`), never `query_canceled` or `assert_failure`.
     A guarded enum type is created behind an existence check rather than in
     a subtransaction, so values added to it later stay usable in the
     replay's one transaction (migrations that `COMMIT` between adding and
     using them).
   - An allowlisted extension's operator class or type that the history uses
     but never creates (a dump with its `CREATE EXTENSION` removed) makes the
     replay install that extension first, as the database the history ran
     on had it; it shows in the first version's model.
   - A zip made on a Mac (`__MACOSX/._x.sql`, binary) imports: those files
     are ignored, not refused as SQL that is not UTF-8.
   - Each migration starts with `search_path` reset (a fresh session).
   - A migration is matched across imports by its id (the tool's version, the
     file name without `.sql` for plain folders, so picking another folder or a
     few files matches the same ids; two files of one name are refused) and the sha256 of its up SQL: the same files
     create nothing, new ones after the imported ones append, a changed one
     or a new one sorted before imported ones is refused (409). The up SQL is
     the version's `source` (`source_kind = 'migration'`, so its SQL tab and a
     draft from it render the model); `version_migrations` keeps the id,
     file, format, checksum and what was skipped.
   - At most 500 new migrations (2 000 replayed) and 16 MiB of SQL per import;
     one import replays at a time, organizations taking turns (two waiting
     per organization, one once half of the 16 places are taken, 10 minutes
     at most; the progress says "queued"; a queued import keeps its place
     when the page is left, like a running one finishes). One import per
     database at a time, decided before the answer starts: a retry while it
     runs answers 409 `concurrent_import`, "still being imported", streamed
     or not; one whose turn comes after the first stored the same
     migrations answers that nothing is left to import. Requests to `…/imports`
     get 15 minutes.
   - Progress streams as NDJSON (`Accept: application/x-ndjson`), the web's
     and CI's documented way: headers at once, a line (or a blank
     keep-alive) at least every 15 s, the last one `done` or `failed`. The
     plain JSON report comes only at the end, and a proxy may give up before
     it (Cloudflare: 524 after about 100 s) though the import lands.
   - Models are staged as the replay reads them (`import_staging`) and moved
     into `versions` when the import is stored: the API holds a few models,
     never the history. An import stores at most 256 MiB of models (422
     `models_too_large`). Rows a stopped process left are swept at startup
     and every 10 minutes (older than an hour, never an import's in flight).
     The replay's 5-minute deadline counts the sandbox's work, not the wait
     for its models to be stored (capped at three deadlines, wall clock).
   - Flyway placeholders (`${name}`) are not substituted: a migration using
     one fails where it is used, with a hint saying so.
   - A migration generated from an imported version is named by what the
     imported one says it does (`import::description`: `add_x` for
     `V3.1__add_x.sql`), never by its file name.
   - A push-scope token learns what it sent, read back (the plan, what is
     skipped, Postgres's errors replaying it on the imported history), the
     numbers its migrations get or got, whether the database has versions
     that were not imported, and each new version's change counts: about
     what a push answers. Never the history's SQL or models.
2. **Generate a migration from a diff.** From a draft (before publishing) or any
   two versions, produce a ready-to-review migration (`ALTER …` statements in
   dependency order, with destructive changes flagged) in the formats above
   (file naming per tool), downloadable or copyable.
   Built (crates/romulus-migration): renames are `ALTER … RENAME` (see
   "Migrations: renames and dependents" below; a rename left off is a drop
   and an add, with the `RENAME` offered in a comment); columns
   added in the middle go last (Postgres cannot reorder; a warning says so);
   an enum losing or reordering labels, or a domain changing type, is
   recreated and its columns converted; a table changing kind or partition
   key is dropped and created again (flagged); destructive statements wait in
   a closing "Removals" section unless something new needs their names.
   `CREATE INDEX CONCURRENTLY` is an option (its own files where the tool runs
   one transaction per file). Down migrations are the reverse migration, for
   the tools that have them (dbmate, golang-migrate), split the same way.
   Statements are flagged by what they risk: deleting data, failing on
   existing rows, widening access (row level security). A partition that
   becomes a table (or the reverse) is detached (attached), keeping its rows;
   the names of a detached partition's keys and indexes are Postgres's
   defaults, assumed and said so (the model does not list a partition's).
   A draft's migration is of the text the person reviewed (its hash).

## Review round 1 (decided 2026-10-16)

1. **Sidebar** collapsible, collapsed by default (icon rail; remembered per user; keyboard shortcut).
2. **Drafts have two modes; one source of truth at a time.**
   - *Schema mode* (today): the schema SQL is canonical; a **Migration** tab shows the
     generated migration (romulus-migration) live, read-only.
   - *Migration mode*: "Edit migration" switches the draft: the migration becomes a
     collaborative Y.Text and the canonical source; the schema/graph are derived by
     compiling base version + migration in the sandbox. Graph edits append to the
     migration. A draft can start in migration mode, written from scratch. Switching back
     to schema mode is allowed only by discarding the hand-written migration (explicit
     confirm). Publishing a migration-mode draft stores the derived schema as the version
     plus the migration text alongside it (downloadable later).
   Built (task R1-MIG); what the build decided on top:
   - **The doc's `meta.mode` is the truth; `drafts.mode` follows it.** Only rooms write
     `meta.mode`, in the Yjs transaction that seeds (or clears) the doc's `migration` Y.Text;
     then the room's writer stores `drafts.mode`, and a lost write of it is repaired from the
     doc at the next load. An update from a client changing `meta.mode` is overruled (written
     back); one changing the text that is not the source (a tab that had not seen a switch)
     is undone, and that tab told (`stale_edit`). In migration mode `sql` waits, unused.
   - **A switch is computed from the room's base**, carried with the text in the room's
     content: a seed computed before "Update draft" moved the base is refused, and computed
     again on the new base.
   - **The compile is the drafts' one, on base + migration**
     (`romulus_sandbox::draft_migration`): the base version's SQL, `SET search_path TO
     "$user", public`, then the migration, through the drafts' allowlist (not the import
     mode's); errors are relocated into the migration (one in the base's part sits at the
     migration's start, saying so). What a schema cannot hold is blanked in place and noted
     (`Code::NotRun`), never run: data statements (a backfill, the generator's `setval`; a
     `set_config` says the statements after it are checked without that setting),
     `BEGIN`/`COMMIT`, and the `CONCURRENTLY` keyword. `DO`, functions, `COPY`… are refused
     as in any draft. `draft_compiles.source_hash` hashes base and migration together; a
     base and migration over the size limit say the base counts.
   - **Never a source that says less than its model.** A schema derived from a migration is
     kept (by its version, or by the draft going back to schema mode) as canonical SQL only
     when that compiles back to the same model, as Tidy checks; otherwise as the base's SQL
     then what of the migration runs (`draft_migration::faithful_sql`), checked the same
     way. When neither compiles back, nothing is stored: 422 `unrepresentable`, naming what
     would come out differently. Canonical SQL has gaps (foreign tables, `NOT VALID`
     checks), and a lossy source would lose those objects at the next draft, then at its
     publish. Each of these checks is a compile, counted against the compile limits.
   - **The derived schema SQL comes with each compile** (`schema_sql` in the compile event,
     `drafts::tidy::canonical_sql` of the model): results never live in the doc.
   - **Edit migration** seeds `romulus_migration::editable` (the generated statements with
     their headings, hazard notes and warnings, no transaction, short lines for the pane);
     it needs SQL that compiles. **Discard migration** needs a migration that applies to the
     current base (after an Update, an older model would revert what the new base brought;
     emptied, the migration is the base): the schema it makes becomes the SQL, the smallest
     edit of what `sql` held, as canonical SQL with `drafts.base_sql` the base rendered the
     same way when both compile back (an Update merges like with like), else as the base's
     SQL then the migration, relative to the base's own text. A switch clears every
     client's undo history (silently: the switch is announced, `ServerEvent::Mode`).
   - **"New migration"** (beside "New draft") starts in migration mode with a two-line
     comment saying what to write; named "Migration from vN".
   - **Update draft in migration mode** re-applies the migration, as written, on the latest
     version: the layout merges as in schema mode, the base moves, and the answer carries
     the migration's errors on the new base (`diagnostics`); the people in the draft fix the
     migration. No statement merge.
   - **Publishing** stores the derived schema (`source` as above, `source_kind` `sql`, origin
     `draft`) and the migration in `versions.migration_sql` (only from drafts). What was
     reviewed is the migration on its base (`compile::input_hash`): after an Update, the same
     text is another review (409 `draft_changed`). Approvals (item 4) are of the same thing:
     a person approves the migration as they see it, the approval is kept against it on its
     base, and an Update outdates it.
     `GET …/versions/{n}/migration` serves it (`v5_<message>.sql`); the version page has a
     "Migration" button, the diff page's migration view links it for N−1 → N.
   - The schema-mode Migration tab is the draft migration endpoint, fetched per compile
     (the compile event's SHA-256), blocked with a reason while the SQL has errors, the
     compiler is away or the draft is stale; Tidy answers 409 `migration_mode`.
3. **Removing a version** (admins only): soft — hidden from history, diffs and imports;
   numbers never reused; restorable for 30 days; audit entry (who, when, why). Drafts based
   on it are re-based onto the previous version (they show "Update draft").
   Built (task R1-GOV): the version row stays while removed (`removed_at`, `removed_by`,
   `removal_reason`: the one change the immutability trigger allows) and every read of
   versions is of live ones: the latest version (a push's "unchanged", a new draft's start,
   the database page), the history, diffs, migrations, the machine API. An imported version
   removed is removed on purpose: later imports of the same files skip its migration
   ("removed in romulus", even once purged: `purged_migrations` remembers it), it puts no
   order on new migrations, and it still replays (the database ran it) while its SQL is
   kept. An import whose latest version is removed or restored meanwhile is refused (import
   again). Numbers come from
   `databases.last_version_number`, so a removed or purged version's number is never given
   again; the history skips the gap. The purge (hourly) deletes versions removed more than
   30 days ago, with the drafts published as them; one an open draft is still based on
   waits for that draft (it holds the text anyway, and "Update draft" merges from it). A
   draft whose base was removed is stale: "Update draft" merges the latest version left
   into it, from the removed version's text; with no version left it keeps its text,
   relative to nothing. Removals, restores and purges are **audit events**
   (`audit_events`, append only, no truncate, written in the transaction of what they
   record, the actor's name kept as it was), listed in the database's settings
   ("Activity", developers) with the publishes of drafts that needed approvals and the
   organization's changes of how many. Reverting the migration is refused while removed or
   purged versions exist (it would bring them back, or give a purged number again).
4. **Approvals** (org setting, N = 0 disables): publishing a draft needs N approvals from
   members other than the draft's editors (developers and reviewers may approve; reviewers
   gain an Approve button, nothing else). Any edit after an approval resets approvals. CI
   pushes and imports are not gated. Admins may override with a reason (logged).
   Built (task R1-GOV): an approval is of one text (the SHA-256 of the SQL the person
   looked at): it counts while the SQL hashes the same and its author has not edited the
   draft, so any edit resets them live, and taking the edit back brings them back (it is
   the text they approved). An editor is whoever sent an update that changed the SQL
   (moving tables does not count; a server rewrite, Tidy or "Update draft", resets
   approvals but makes nobody an editor), recorded by the collab room, which is the
   authority: publishing asks it who approved the frozen text. N is 0 to 10, and each change
   is an audit event (from, to, who). Admins publish without the approvals by giving a
   reason. Publishing freezes the approvals: checked again in the transaction storing the
   version, the draft row locked (withdrawn meanwhile, or its author gone from the
   organization: refused), kept with the version (`version_approvals`, immutable) and in
   the audit log (never purged); approving and withdrawing lock the draft row and only
   change an open draft's approvals. Drafts open before editors were recorded
   (`editors_unknown`) are approved by reviewers only, who never could edit. "Request
   review" is a state the drafts list shows (no notification yet); approvals can be
   withdrawn while the draft is open.
5. **Groups**: a better editing UX (create from selection, rename inline, color, add/remove
   tables by drag or menu, delete), and dragging a group moves all its tables (one Yjs
   transaction, one undo step).
6. **Comments, Figma-like**: threads pinned to a table, a column or a canvas point, with
   replies, @mentions, resolve, author and time. Stored as collaborative data, not schema;
   the SQL pane shows them as `--- alice: …` lines above the statement they're attached to,
   and typing a `---` line creates a comment there. They travel into the published version as
   review history; never into `COMMENT ON` (database comments stay as they are).
   Built (task R1-COM); what the build decided on top:
   - **The server writes them.** Threads and comments are two Y.Maps of the draft's doc
     (`threads`, `comments`: a reply is a key of its own, so concurrent replies never collide).
     The panel and the graph send operations (start, reply, edit, delete, resolve) that the
     server checks and writes as its own edits: authors stamped, only one's own comments
     edited (and deleted, but developers and admins delete anyone's: in the database's
     Activity), a thread never deleted with someone else's reply, sizes capped (500 threads, 200
     comments a thread, 4000 characters), and what the discussion weighs (1 MiB a draft, 256 KiB
     a person: far below the SQL's room, so comments never make a draft uneditable), a budget
     per person. Editors' own
     transactions (a `---` line removed as it becomes a thread, so one undo takes both back; a
     rename carrying anchors) are checked the same way, and what may not stand is written back
     in the update the others receive.
   - **Reviewers comment** through operations only: their document updates are never applied.
     A client id belongs to the person whose tab writes with it (writing with someone else's is
     refused and that tab told to reload; a tab that only announced an id yields it to the one
     writing with it, and a reviewer's announcement never holds one). Nothing is dropped
     silently, and a tab typing as it reconnects, or coming back from sleep with offline edits,
     loses nothing. Commenting does not make anyone an editor.
   - **`---` lines never stay in the SQL**: Enter after the words (at the end of the line or
     splitting it) and pasting turn them into threads; a draft whose text keeps one does not
     compile ("a comment that was never posted"), so it cannot be published.
   - **Anchors**: a table, a column (qualified names), a point of the canvas, or a statement's
     head (a `---` line above an index or a type keeps where it was written). Renames carry
     them, from the graph (the edit's transaction) or typed in the text (the server's layout
     move); a drop leaves them outdated, listed apart. The SQL pane shows open threads as
     read-only `--- name: …` lines drawn by the editor, never text in the document.
   - **Versions keep them**: publishing freezes the draft's threads into the version
     (read-only, with everyone's unread marks); a version's page also takes new threads
     (cheap: the same panel over REST), so released schemas can still be discussed, with the
     drafts' limits (weights, moderation, a rate limit).
   - Unread markers are per person, stored server-side; "who is typing" is awareness.

**Ideas, after the round:** several Postgres majors per database (sandbox per major; built,
below), then a **playground** (a real throwaway database per draft with generated fake data,
to run SQL and try migrations) — design document first: [docs/playground.md](playground.md)
(proposed, awaiting approval).

## Migrations: renames and dependents (decided 2026-10-19, built: task MIG2)

A rename is `ALTER … RENAME`, never a drop and an add that loses the data, and
everything a change drops to be possible comes back after it.

- **Detect, then confirm.** `romulus_diff::resolve` pairs removed and added objects
  that look like one object renamed: tables, views, materialized views, partitions,
  columns, constraints, indexes, policies, the sequences serial columns draw from,
  enums, domains, composites, enum labels (`RENAME VALUE`) and composite attributes.
  Functions and triggers are not modelled, so they are not either. Each pair is scored
  out of 100 (the rules are in `crates/romulus-diff/src/rename/mod.rs`): **high** (an
  exact structural match scoring 72 or more, with evidence beyond the shape: a similar
  name or the same comment; a table also its keys following it, or a move) is applied
  unless turned off; **medium** (from 45: a compatible type in the same place, most of
  a table's columns, or an exact match with unalike names, `password_hash` → `bio`) is
  offered, off. Two pairs within 5 points of each other for one object are
  **rivals**: both offered, neither chosen. The name only breaks ties (25 points at
  most).
- **Moves** (`ALTER … SET SCHEMA`) are renames into another schema: a table, view or
  type found under the same name in another schema is detected (high), and `SET
  SCHEMA` in a draft's text is known; a moved table takes its keys, indexes and
  sequences along. The schema is created first when the old model has none.
- **Known renames need no confirmation**: the `ALTER … RENAME` statements a draft's
  text runs beyond its base's text (graph edits append them; a migration-mode draft
  has them), read by `romulus_sandbox::known_renames`, and the renames a version was
  published with. Chains and swaps are followed object by object (a swap goes through
  a temporary name); renames waiting on each other's names are applied cycle by
  cycle, so one waiting on an unrelated name does not hold back a swap, and what names
  a swapped object by its new name (a column of a swapped table, under the table's
  final name) is tried again once the swap is done.
- **Levels**: types, tables, columns, labels and attributes, sequences, keys, indexes,
  policies, partitions, views, each on the old model with the renames chosen so far
  done. A key or index Postgres named after a renamed table or column
  (`users_email_key`) is renamed with it ("along with it"), without a choice of its
  own; one that only changed its name is its own candidate.
- **Postgres semantics.** The migration runs the renames first, then is planned from
  the old model as Postgres has it after them: indexes, keys, checks, policies,
  views, defaults and casts follow by OID, so they are rewritten as `pg_get_*def`
  prints them then, not dropped and created again. A view keeps its output names
  (`code AS old_name`); a view the rewrite cannot follow (a `UNION`, a subquery) is
  built again. A renamed column that changes type is converted after its rename.
- **Choices are the draft's**: its Y.Doc's `renames` map (rename key → on or off),
  live for everyone in it, switched by editors. Every migration endpoint takes
  `renames` (what the person has on screen) over the stored choices and answers every
  candidate. Publishing stores, with the version (`versions.renames`), the renames its
  migration ran and the ones it left off; a migration between versions goes by the
  renames of the versions in between, **version by version** (`RenameChoices::compose`:
  each version's renames a batch, its swaps included, done before the next version's,
  so a later rename lands on the object that version had under that name; each
  version's decisions as they read after the later versions' renames, off where two
  versions decided one key differently: one turned it off on purpose), reversed going
  back batch by batch, so
  v(N−1) → vN is the one seen at publish. The version diff page offers the same switches. **Approvals are of
  the text and the switches**: what is reviewed (`compile::review_hash`) covers the
  doc's decisions, so switching a rename outdates approvals as an edit does (and makes
  the person an editor), switching it back brings them back (a decision equal to
  romulus's own choice is removed from the doc), an approval names the switches seen,
  and a publish whose switches are not the doc's is refused (409 `renames_changed`).
  The publish dialog waits for the preview and the migration to catch up with a
  switch before it lets anyone publish.
- **Versions published before renames existed** store none: their migrations detect
  renames anew on every download (high ones on, as for a draft), so an older pair's
  migration may now rename where it used to drop and add. Download it again before
  running one generated earlier.
- **Function and trigger bodies are not in the model**: a migration that renames or
  converts columns, tables, views or types warns (`function_bodies`) that bodies
  naming them are not rewritten, by Postgres or by romulus.
- **Down migrations mirror the up** (`romulus_migration::down`): its renames reversed,
  nothing detected anew.
- **Dependents come back.** Every object dropped to make a change possible is created
  again after it, in dependency order, up and down: views (and views over them),
  policies, keys and the foreign keys referencing them (dropped before the key, added
  after), indexes, checks, defaults, generated columns, comments of a column added
  again, composite attributes and domains over a recreated enum. Fixed while building:
  a re-added column lost its comment; a recreated enum left defaults, checks,
  generated columns, composite attributes and domains on the old type; the
  per-partition copies of a foreign key to a partitioned table were created twice
  (and `render_sql` wrote them out); a removed table's partitions held names new ones
  took; an index on a dropped column was dropped `CONCURRENTLY` after the column took
  it; a dropped generated column went after the column it reads.
- **Proven by the oracle** (`crates/romulus-migration/tests/oracle`): a scenario per
  dependency kind and per rename kind (rewritten, and with `ALTER … RENAME` known),
  every scenario again with renames off, the down migration run after the up and
  checked against the old schema, random edits that rename what others depend on.
- Follow-ups: the change list and the graph of a diff still compare by name (a rename
  reads as removed + added there; a line above them says how many changes the
  migration runs as renames, with the way to it); renaming a schema itself is not
  detected.

## Test migration (decided 2026-10-19, built: task MTEST)

One click (a draft's migration view and its publish dialog, the diff page's
migration view, a version's written or imported migration) runs the
migration up and down on a playground of its own, filled with generated rows
(or a copy of someone's playground at the base version), and says whether
everything is alright: each statement measured (duration, locks, rewrites,
Postgres's error and the rows that break it), the schema exactly the target
after up and exactly the base after down, row counts kept, sampled rows (by
primary key) holding their values through renames and conversions, and the
data that does not come back, matched against the migration's `deletes_data`
flags (unflagged loss fails). Reports are kept and seen by the organization.
One engine, `crates/romulus-migration-check`, behind the feature and the
migration oracle. By default on a copy of the person's own playground at
the base version, if any. An organization may publish only drafts whose text
passed a test (a setting, off by default; deviation from the data model:
`organizations.require_passing_test`). docs/playground.md "Test migration"
has the decisions.

## Several Postgres majors (decided 2026-10-16, built: task PGV)

Each database targets a Postgres major: 15, 16, 17 (the default, the one sandbox there was) or
18. Its SQL compiles on that major's sandbox and its migrations are written for it.

- **Chosen at creation, changed by developers** (and admins, as the database's other settings;
  `databases.pg_major`, a check on the supported list: adding a major is a migration and a
  sandbox image). The picker lists them newest first, the default marked and, under each,
  whether this server compiles it now (`GET /api/postgres-majors`). A major this server has no
  sandbox for cannot be picked, and the API refuses it (creating, checking or moving: 422
  `major_unavailable`); one whose sandbox is only down can (it comes back). The database's header, its
  versions and the list (only when it is not the default) show it as a quiet badge.
- **Moving is a dry run first.** `POST …/major/check` compiles the latest version's SQL (its
  source, or its model rendered for a push or an import) on the new major and says whether it
  compiles, its errors in that text, and how its model reads there; `PUT …/major` does the same
  compile (served from the sandbox's cache) and refuses with 422 `incompatible_major` unless it
  compiles, 409 `latest_changed` when a version arrived meanwhile. A database with no version
  moves freely. **What compiled before a move is not stored after it**: a version carries the
  major it was compiled on (uploads, SQL pushes, publishes, imports' replays; not remus JSON),
  checked under the databases row lock that numbers versions; a move in between refuses it with
  409 `major_changed` (nothing stored; trying again compiles on the new major). The move is an audit event (`major_changed`, from and to). The settings card
  checks a major as soon as it is picked and offers the switch only when it compiles.
- **Versions keep what they were compiled on**, never recompiled: `versions.server_version` and
  `pg_major` are generated from the model (remus's `server_version`: the sandbox's, or the
  pushing server's), `database_major` is the database's major when the version was stored (a
  trigger). **A push from another major** (remus JSON from a server on 16 while the database
  targets 17) is accepted and flagged for good (`other_major`): a badge in the history and on
  its page, and while it is the latest a banner offering "Switch to Postgres 16…" (the dry run,
  preselected). SQL pushes, uploads, imports and drafts compile on the database's own major.
- **One sandbox cluster per major**, the same image built with `PG_MAJOR` (PostGIS and pgvector
  are the same release on every major). Dev runs 17 always and the others on demand (`just
  sandbox 15`, compose profiles, ports `40000 + 10·slot + major − 10`); the preview runs all
  four (768 MB each beside 17's 1 GB). `/api/health` lists each (`sandboxes`); a major without
  a sandbox answers 503 for its databases only.
- **The compiler routes by major**: uploads, dry runs, SQL pushes, import replays, drafts
  (`compile_for` keys are per major's compiler), publish, Tidy, mode switches and "Update draft".
  Caches are per major: each compiler's own, `draft_compiles.pg_major` (a room on another major
  compiles again), the web's migration queries keyed by the major. A move tells the database's
  open drafts (`ServerEvent::Major`): they compile on the new major at once, and say so.
  Approvals are of the text and stay.
- **One parser for every major**: pg_query's, PostgreSQL 17's grammar. An older major's grammar
  is a subset of it, and its server refuses what it lacks with its own error, located (an
  exclusion constraint on a partitioned table on 15: "not supported on partitioned tables").
  Syntax new in 18 (`VIRTUAL` generated columns, `NOT ENFORCED`, `WITHOUT OVERLAPS`…) fails to
  parse, with a hint saying the grammar is 17's, until pg_query speaks 18; so does a draft from a
  version pushed from 18 with a virtual column (remus-sql renders it `VIRTUAL`).
- **Before 17, an enum value cannot be used in the transaction that added it**, even one that
  created the type (`55P04`; 17 allows that case). A compile is one transaction, so a text that
  grows its enums and uses the new values fails on 15 and 16, with a hint (put the value in the
  `CREATE TYPE`, or move to 17); so does an import whose history adds a value and uses it in a
  later migration (the replay is one transaction too).
- **Migrations are planned for the database's major** (`romulus_migration::Options::major`; the
  files' header names it): a generated column's new expression is `SET EXPRESSION` from 17 on,
  dropped and added again before; what the new model needs of a newer major is kept, noted on
  its statement and warned about (`newer_postgres`: `NULLS NOT DISTINCT` and a column list in
  `ON DELETE SET NULL` 15+, an exclusion constraint on a partitioned table 17+, virtual
  generated columns, `NOT ENFORCED` and temporal keys 18+). Enum values a migration uses are
  committed before its transaction on every major (as before). The oracle plans for its
  sandbox's major and runs per major in CI: everything on 17, the scenarios and a smoke subset
  of the corpus pairs and random edits on 15, 16 and 18 (snapshots are 17's: each major prints
  views its own way).
- Follow-ups: replay imports on 15/16 with a commit between migrations (a scratch database per
  import) so enum values added then used import; check a hand-written migration's data
  statements against the major (they are blanked, never run: a `MERGE … RETURNING`, 17+, is not
  caught on 15); pg_query 18 when it ships (then 18's syntax compiles, and `VIRTUAL` columns
  round-trip).

## The time machine (decided 2026-10-19, built: task TM)

Several versions of a database superposed on one graph, with a timeline of its versions under
it: `/app/<org>/<db>/time-machine`, from the history page and from every version's strip (T).

- **Overlay and timeline.** A range of versions (two handles on the timeline) and a cursor in
  it (a playhead: dragged, `[`/`]`, played with K). *Superposed* draws the union of the whole
  range, whatever the cursor: what the cursor's version has in full, the rest faded back (its
  frame and fill fade, its words stay readable in muted ink, ≥ 4.5:1; gone by then: struck
  through, dashed; not there yet: plain; one gone that comes back later says `−v5 +v8`), so
  moving the cursor moves a
  highlight and playing walks it through the versions. *At cursor* draws exactly the version
  at the cursor, nothing else, and playing makes the schema evolve (M switches; the chosen
  mode is inverted and a line under it says what is shown: "All 8 versions superposed, v4
  highlighted", "The schema at v4"). The two differ whenever the range has more than the
  cursor's version. Each table, column and foreign key is colored by when it appeared in the
  range: six distinct hues at most (`--era-1`…`--era-6`), a run of versions each when the
  range has more, the legend saying which versions each color covers; what was there at the
  range's start uncolored; changed things carry `~`; tables say `+v4` / `−v7`. Headers carry a
  solid band, rows a bar, and zoomed out a table is filled with its color, so the map reads at
  overview zoom. The timeline is the legend: a bar per version, as high as what it changed,
  colored by its run. The header speaks of the range (what it changed, start to end), the
  timeline's bar of the cursor (its version, the mode).
- **Why categorical colors** (changed 2026-10-19, task TM2, from the one-hue ordinal ramp of
  TM): six steps of one violet could not be told apart on a column row or at overview zoom,
  and the age is read off the `+vN` labels and the legend anyway. The palette is two
  lightness tiers over six hue families (violet, green, indigo, magenta, teal, lime), clear of
  the accent, the warning and the destructive colors, validated for every pair (any two
  tables can sit side by side): color-blind ΔE ≥ 9.2 light / 10.3 dark, ≥ 3:1 on the card and
  the canvas in both themes. Ordered so that short ranges, which use the first colors, get
  the most distinct ones.
- **History of an element.** Hovering a column (or a table header, or an enum through a
  column) shows its history in the hover card; selecting a table lists its history and its
  parts' (columns, keys, checks, indexes, policies), each version linking to its diff with
  the version before it.
- **Filters fade, never hide**: only changes, a group, a name; the layout stays.
- **Layout computed once, for every table the history ever had** (the newest version's
  positions, each older one's for tables it alone had, elk for the rest), so nothing jumps
  when the cursor or the range moves; what appears or goes animates in and out. What elk
  placed is kept in the browser per database (versions are immutable, their layouts too), so
  the seconds of a large first layout are spent once; a version arriving while the view is
  open places only its new tables, the rest staying on screen.
- **Server-side history, one step per pair of versions.** `GET …/timeline` returns every
  version and, per object (tables and their parts by name within their table, types,
  partitions, extensions), its states: its value from one version to another and
  romulus-diff's structured changes into it. A step (romulus-diff between two consecutive
  versions) depends on its two models only, so it is cached by their ids: a version created,
  removed or restored costs the steps it makes, never the history (the 120-version,
  250-table fixture: 2 ms a step, 1.4 ms to fold, 1.1 MB of JSON). Chosen over computing it
  in the browser, which would mean downloading every model (100 × a few hundred kB) and
  shipping romulus-diff as wasm. The folded history is cached too, by the database and its
  ordered list of version ids (the same versions are the same history, for everyone allowed
  to see them). A build that misses takes its organization's turn (one at a time per
  organization), then one of the process's two; past the steps' budget, the database
  holding the most gives back first, so one long history cannot evict everyone else's. The client builds each frame from the states (2 ms at 220
  tables) and hands unchanged tables and edges to the graph as the same objects, so a frame
  redraws and reroutes only what changed.
- **Removed versions** are left out, as everywhere; admins can bring them in (an eye in the
  toolbar, `?removed=1`): flagged on the timeline, the history running through them, no diff
  links to them.
- **Renames** are a removal and an addition while romulus-diff matches by name; the
  timeline reads renames through one seam (`timeline::history::reported_renames`), and when
  the diff reports them a renamed object keeps its history under its new name (`renamed
  users → customers`), drawn under the name it had at the cursor.
- At most the newest 1000 versions (`earlier` counts the rest). One version: the page
  explains what the time machine needs.

## Self-host and open source (decided 2026-10-19, built: task OSS)

The decisions: AGPL-3.0, a CLA, one organization per self-hosted install, `ee/` later,
email + password sign-in for self-hosters. This is what the build decided on top.

- **Two sign-in methods, one session layer.** `ROMULUS_AUTH` lists `oidc` (Zitadel: the hosted
  service, unchanged) and `password` (romulus's own accounts: the self-host bundle). Both end in
  the same session cookie, CSRF rules and `CurrentUser`. A password account is a `users` row
  (`oidc_subject` `password|<uuid>`, one per address); an SSO account with the same address is
  another person, as with the dev sign-in.
- **Every password account's address is proved.** With mail (`ROMULUS_SMTP_*`), signing up sends
  a link and the account is created from it, its password chosen there: nobody can create an
  account for an address they do not read (no pre-hijacked accounts, no unverified state). Without
  mail, only an invitation's holder creates an account, with the invited address, at once: the
  invitation vouches (the admin chose whom to send it; it works once, 7 days). Open sign-up
  (`ROMULUS_SIGNUP=open`) needs mail; the default is invitations only.
- **Nothing tells who has an account.** Sign-in answers 401 `invalid_credentials` for an unknown
  address and a wrong password alike, after the same Argon2id work (a decoy hash). Sign-up with
  mail and "forgot password" answer 202 whatever the address (the owner of an existing account
  is emailed "you already have one"; nobody else is). The lock is per address, account or not.
- **Guessing is slow** (OWASP ASVS 2.2): per client a burst of 10 sign-ins then one per 3 s
  (an IPv6 client counts as its /48). Per address, every attempt is counted before its password
  is checked: the 10th in a series locks it, 15 minutes the first time in a day, 30 the second,
  an hour from the third on (never longer); a new series starts when a lock runs out, and a
  success or a reset starts the day over. A browser that signed in to the account before
  carries a device cookie: its attempts count against it, never the address, so its owner signs
  in through anyone else's lock; a reset link works through any lock too (mail is strongly
  recommended for that). No global requests-per-second cap (a few clients could fill it for
  everyone): the hashing pool is the guard on cost, a bounded queue (4 at once, 64 waiting, 2 s)
  that answers 503 `busy` only when it is actually full. The owner is emailed at the first lock
  of the day, and an address locked three times a day is logged. Wrong current passwords on
  "change password" count too. Behind a reverse proxy, `ROMULUS_TRUSTED_PROXIES` names who may
  say the client's address (the bundle: Caddy's fixed address, or the gateway for a proxy on
  the host, or nobody when the port is published beyond loopback).
- **Passwords**: 12 to 128 characters, any characters (NFC), no composition rules; refused when
  in the breach lists (SecLists' top million and NCSC's 100k, the entries long enough to
  matter, embedded gzipped), repetitive, sequential, or built on the account's name or address.
  Argon2id at OWASP's parameters (19 MiB, 2 passes), rehashed at sign-in when they change.
- **One-time links** (sign-up 24 h, emailed reset 30 min, an admin's 1 h, the first admin's 24 h):
  the token in the URL's fragment (never in a log or a `Referer`), sha256 stored, single use, a
  newer reset link retires the older. Setting a password ends every session of the account;
  changing it ends the others (this browser gets a new session id); the owner is emailed.
- **No email provider required.** Without mail, admins print links (`romulus-api password-link`),
  invitation links are copied as before, and "Forgot password?" says whom to ask. With mail,
  invitations are emailed too.
- **First run**: `romulus-api setup --email --name --org` creates the admin (no password) and the
  organization, and prints the link that sets the password; idempotent (a fresh link until the
  password is set). Refused when another admin set the install up.
- **Single-organization mode** (`ROMULUS_SINGLE_ORG`): creating an organization is refused
  (403 `single_org`) and hidden; someone signed in without a membership is told to ask for an
  invitation. Invitations, roles and everything else work as hosted.
- **The security log** (`auth_events`): sign-ins, failures, locks, sign-ups, resets, changes and
  every admin command, with the client address; append only, kept 90 days, read with
  `romulus-api auth-events`.
- **Data model additions**: `password_credentials`, `sign_in_failures` (by address),
  `auth_links`, `auth_events`; `users_password_email_key` (one password account per address).
- **The bundle** (`deploy/self-host`, docs/self-host): compose with the published images, an
  install script that asks about playgrounds and checks gVisor and XFS, upgrades, backups and
  restores; TLS at the user's reverse proxy (a Caddy profile included). The API redirects `/` to
  `/app/` (no homepage on a self-hosted install).
- **Mail without TLS** only to a relay on this host (loopback): sign-in links never travel in
  clear. `romulus-api verify-email` vouches for password accounts only (an identity provider
  verifies its own).
- Follow-ups: MFA (TOTP, WebAuthn), an online breached-password check (k-anonymity, opt-in), an
  admin page for accounts and the sign-in log, SSO configured from the app for self-hosters.

## Billing (decided 2026-10-20, built: task BILL)

Hosted only. Off unless `ROMULUS_BILLING` is set: then nothing is capped and
the billing routes answer 404, so a self-hosted install is unchanged (the
API refuses billing with `ROMULUS_SINGLE_ORG`). Stripe, test mode until the
founder switches to live. The contract it sells under (the Terms, the
checkout's boxes, withdrawal) is "Legal: terms, checkout, withdrawal" below.

- **The plan table** (`billing::plans`, the one place the numbers live;
  enforcement, the billing page and `romulus-api stripe-setup` read it):

  | | Free | Starter | Team | Business |
  |---|---|---|---|---|
  | € / month (a year: 10 months) | 0 | 19 | 49 | 149 |
  | members | 1 | unlimited | unlimited | unlimited |
  | databases | 1 | 3 | 10 | 50 |
  | versions per database | 50 | 500 | unlimited | unlimited |
  | tables + views per version | 50 | 200 | 1000 | 5000 |
  | history shown | 90 days | 1 year | all | all |
  | playgrounds: largest · hours a month | Small · 2 | Small · 20 | Medium · 80 | Medium · 300 |
  | required approvals | none | 1 | up to 10 | up to 10 + admin override |

- **Hard caps, never deletion.** Creating the next resource past a cap
  answers 402 `plan_limit`, `details` = the cap, the plan, the limit, the
  usage and the cheapest plan that lifts it (`billing::caps`); the message
  says so in a sentence (CI pushes print it). Checked where things are
  made: databases (under the organization's billing lock), versions and
  schema size (`versions::store::store_capped`, under the database's lock;
  unchanged pushes are never refused; imports all or none, counted before
  the replay and again when stored), invitations and their acceptance,
  raising the approvals setting (lowering always works), the override,
  playground and migration test sizes. Removed versions do not count.
  After a downgrade everything stays readable; only creating waits.
- **History** is hidden, not deleted: versions older than the window are
  left out of lists, the time machine and diffs, and opening one answers 402
  (`billing::caps::visible`, SQL `version_kept_visible`): the latest version
  and the base of an open draft always show. The history list says how many
  are hidden; an upgrade shows them again; the export holds them.
- **Playground hours**, the one metered resource (`billing::hours`): every
  minute the host's running playgrounds (migration tests' included) are
  counted to their organization's calendar month (UTC), a pause never
  billed. 80 %: the admins are emailed. 100 %: running playgrounds stop with
  the reason (their data stays until normal expiry; a test in progress
  finishes), new playgrounds, tests, console runs, resets and migration
  runs answer 402 until the month ends or the plan grows.
- **Stripe**: Checkout (subscription mode, billing address required, a
  business buyer's name required, VAT number collected, no Stripe Tax, the
  page in the buyer's language with "the Subscribe button places an order
  with an obligation to pay" under its button) after our own dialog (below,
  "Legal"): recorded (`legal_acceptances`) and audited (`terms_accepted`).
  No `consent_collection[terms_of_service]`: the Terms box is ours, with
  its version and wording, ticked before Stripe's page; a second, unversioned
  Stripe box would repeat it in other words. The customer is created first, carrying every
  invoice's footer and custom fields (`ROMULUS_INVOICE_FOOTER`, `_FIELDS`:
  the VAT mention and the seller, kept in configuration, not in code).
  Changing plans and cancelling go through the Customer Portal configured by
  `stripe-setup`: upgrades at once, prorated by Stripe; downgrades and
  shorter intervals at the period's end (shown as "switches on"); cancel at
  the period's end, no survey. The billing page's Cancel button opens the
  portal's cancel flow directly.
- **Webhooks** (`POST /api/billing/webhook`): verified on the raw body,
  each event id stored with what it applied (idempotent), the subscription
  always read again from Stripe under the organization's lock (order-proof;
  an old subscription's late event never undoes the live one), every change
  audited. A failed payment keeps the plan 14 days from the invoice's due
  date (`billing::state`), with emails at the failure, three days before the
  end and at the downgrade to Free (audited `plan_downgraded`).
- **Renewal reminders** (`billing::notices`): 40 days before a yearly
  renewal (more than one month, less than three: Article L215-1 of the
  French Consumer Code), once a period, to every admin: the date, the
  amount, how not to renew, a link to the Cancel button. The price-change
  letter exists (`notices::price_change`) for whoever moves subscriptions
  to a new price; nothing sends it on its own yet.
- **Data export** (`export`, every plan, self-hosted too, never capped): a
  database (any member) or the organization (admins) as a ZIP streamed as it
  is written: each version's DDL, model and original source, the migration
  between consecutive versions up and down with the renames chosen, the
  comments, a manifest. Hidden versions included, removed ones listed.
- Deviations from the data model: `org_billing`, `billing_events`,
  `playground_usage`, `billing_notices`; audit actions for billing, kept
  out of a database's activity.

## Legal: terms, checkout, withdrawal (decided 2026-10-21, built: task COMPLY)

The hosted service's contract, as its documents (published by the homepage,
`site/legal/`) promise. Off unless `ROMULUS_TERMS_VERSION` is set (with
`ROMULUS_LEGAL_URL`, the site, and `ROMULUS_LEGAL_CONTACT`, the seller's
address); billing needs it; a self-hosted install has no Terms at all. The
hosted service runs `ROMULUS_TERMS_VERSION=1.0` (the documents' version
1.0 of 21 October 2026); a new version of the documents is a new value, and
its PDFs (`crates/romulus-api/legal`, `just site-legal`).

- **Two editions of the Terms.** Launch: professionals only, on every plan
  (`ROMULUS_CONSUMER_SALES` off, the default): the professional edition at
  `/legal/terms`, `/fr/legal/conditions`, `/legal/privacy`,
  `/fr/legal/confidentialite`. Consumer sales open once the founder has joined
  a mediator: the flag on, the full edition (published now under
  `/legal/full/…`, noindex, marked not in force) with the business-or-consumer
  choice, the consumer's boxes and withdrawal. The flag alone is refused:
  it needs `ROMULUS_TERMS_EDITION=full` and a version other than 1.0 (which
  was published for businesses only). Each edition has its own
  wording version for the boxes (`checkout-boxes.json`); a change of edition
  is accepted again, like a new version. The DPA is one document
  (`/legal/dpa`).
- **The Terms before anything else.** At the first sign-in (every path:
  single sign-on, password, invitation), the app shows `/app/signin/terms`:
  the Terms box and, in the professional edition, the professional-use box
  (the wording in `checkout-boxes.json`), in English or French. That
  concludes the Free plan's contract. A password sign-up ticks the same
  boxes on its form, recorded when the account is created (kept on the
  emailed link meanwhile). Until then the API refuses creating or joining an
  organization and everything in one (403 `terms_required`, from the browser
  or an agent: `auth::Member`). A new version is asked for at the next
  sign-in; who accepted an earlier one may say "Not now" (that one applies
  meanwhile; in the professional edition the new one applies to whoever
  keeps using the Service after it takes effect, Terms 21.3). Access tokens
  never accept for a person.
- **Records** (`legal_acceptances`, not tied to the organization, never
  changed): who, when, the version and edition, the language, the wording
  version, the attestation; for a checkout also the organization, the plan,
  as whom, a business's name, country and VAT/SIREN, a consumer's requests
  and the Stripe Checkout session (whose subscription carries the record's
  id), the business Stripe's payment page named, and when the contract ended
  (the subscription over for good, a withdrawal, the organization deleted).
  Kept for the contract and 5 years after: a sign-in's from the account's
  end, a checkout's from its contract's (or the account's, if later); under
  the full edition a checkout's 10 years at least (L213-1). `legal::purge`
  daily.
- **Checkout.** Professional edition: no choice, a business (name and
  country required, an ISO 3166-1 code; VAT/SIREN optional), its "not as a
  consumer" box, the Terms. Full edition: the choice, never preselected; a
  business as above, or a consumer's two boxes. 422 naming the field
  otherwise; a stale wording 409 `checkout_copy_changed`; a consumer while
  sales are off 422. Stripe labels its button "Subscribe" or "Pay and
  subscribe" depending on the page: the Terms (3.4 d), the dialog and the
  message under Stripe's button name both. Stripe asks the business's name
  again on its page (its API cannot prefill it, checked against test mode):
  the name it collects is recorded from `checkout.session.completed`.
- **Withdrawal** (full edition, a consumer's subscription only): "Withdraw
  from contract here" / « Renoncer au contrat ici » (the wording of the
  French Terms and of L221-21's example) on the billing page until the end of
  the 14th day after subscribing, in Paris (pushed past a weekend or a
  French public holiday, Art. 642 CPC and Regulation 1182/71), then "Confirm
  withdrawal". Recorded first (`withdrawals`) and the organization on Free at
  once (`org_billing.withdrawn_at`), the acknowledgement queued with its
  time (to the buyer too when another admin withdrew), then Stripe:
  cancelled now, each paid invoice refunded for the whole days not used (a
  day begun is free), a refund already made never made again and none
  beyond what is left of the payment. Stripe failing is retried by the
  notices task, for 8 attempts or a week, then left to the operator (an
  error logged). A paid invoice with no payment intent refuses the
  withdrawal here (503 `withdrawal_by_email`): the consumer writes instead.
  Businesses never see it (403 `not_a_consumer`).
- **Emails**, in each reader's language (`users.locale`: chosen with the
  Terms, changed in account settings): the subscription's confirmation (the
  plan, price, renewal, how to cancel, the Terms as a PDF; a consumer's
  requests and how to withdraw), a cancellation's confirmation (when it ends,
  what changes), the renewal reminder with the last day to cancel in a box,
  the withdrawal's acknowledgement, every other billing email. Consumer-law
  citations only while consumers may buy. Every one goes through an outbox
  (`mail_outbox`): queued with what decides it, marked sent only once the
  mail server accepted it, retried, never sent twice.
- **Cancel your contract**: the app's footer (with the Terms and the Privacy
  Policy) and the account's "Your contract" page lead to the organization's
  Cancel button. Zitadel's login links neither the Terms nor the Privacy
  Policy: with either link, its sign-up form adds an unversioned "I accept"
  box of its own (and a privacy policy is information, never accepted).
  romulus asks for the Terms itself and its sign-in pages link them.
- **Notices** (admin commands, mail needed): `romulus-api legal-notice`
  (a change to the Terms or the Privacy Policy to everyone, to the DPA to
  admins, 30 days ahead unless the law requires sooner, once per person and
  version, linking the new version's page, which must be published first;
  what it says follows the edition in force) and `romulus-api
  price-change` (the plan table's new price to each subscription still on
  the old one, from its first renewal 30 days away).
- **French**: the Terms screen, checkout, the billing page, withdrawal and
  cancellation, and every billing email; the rest of the app stays English.
- Deviations from the data model: `legal_acceptances`, `withdrawals`,
  `legal_notices`, `mail_outbox`, `users.locale`, `auth_links.accepted_terms`,
  `org_billing.acceptance_id`, `started_at`, `withdrawn_at`,
  `checkout_acceptance_id`.

## Agents over MCP (decided 2026-10-20, built: task MCP)

People work in romulus from their AI agent (Claude Code, Claude Desktop, Cursor, any MCP
client) through a remote MCP endpoint the API serves itself, at `/api/mcp`
([docs/mcp.md](mcp.md)). What the build decided:

- **Personal access tokens, not OAuth (yet).** A person creates a token in their account
  (`rmls_pat_`, read or write, 90 days by default, or 30, 365 or none), shown once, stored as
  its sha256, revocable, its last use and client shown; its person is emailed on creation, and
  a password reset or change revokes them all. It is accepted at `/api/mcp` only; the endpoint
  never accepts the session cookie, and an `Origin` other than romulus's is refused. Limits run
  before any lookup where they can (per client for tokens that fail, per token, per person).
  Connecting by signing in followed (task OAUTH, next section).
- **One code path for permissions.** Every tool calls the REST handlers the web app calls,
  through the API's own router, as the token's person (a request extension only the server
  sets, which `CurrentUser` and `Member` resolve). Roles, validation, rate limits, plan limits
  and audit events are therefore the web app's. A read-only token is offered the reading
  tools only, and the identity it lends refuses every unsafe method (a playground query, a
  read by nature, is marked as one).
- **Governance holds.** No tool approves, and none overrides approvals or the passing-test
  gate; publishing takes the hash of the text the agent reviewed. An agent's edit of a draft
  is its person's edit (they become an editor: their approval of it never counts), applied by
  the collab room as a rewrite, so everyone in the draft sees it live. Every write is an
  `agent_action` audit event with the token, recorded as soon as it succeeded (a tool failing
  after it still leaves one). An agent creates a database's first version from SQL
  (`push_schema`, refused once one exists): every later version comes from a draft. It never
  handles CI secrets: CI's API token is made by its person in the web app.
- **New REST endpoints the tools needed**, usable by the web app too: a draft's text with its
  compile (`GET/PUT …/drafts/{id}/text`), its rename decisions (`PUT …/drafts/{id}/renames`), a
  one-shot playground query (`POST …/playgrounds/{id}/query`), schema search
  (`GET /orgs/{org}/search`).
- **The transport is hand-written, small and stateless**: one JSON-RPC message per POST, one
  JSON answer, no session, no stream; both protocol eras (`initialize`-based 2025 versions and
  2026-07-28). Long tools wait 40 s at most, then hand back an id to poll, well within what a
  proxy in front allows. The official Rust SDK was not used: the endpoint needs none of its
  streams or sessions, and has to sit inside the API's auth, errors and limits.
- **Tool answers are text only**: clients hand a model `structuredContent` instead of the
  text when both are present, and the text carries the ids, links and next steps.
- **Migration files between a repository and romulus** (task MCP-FILES). The MCP server
  cannot read the person's disk: the agent reads the files and sends them, and writes the
  files romulus answers. `import_migrations` goes through an upload (`imports::uploads`,
  REST first: `POST …/imports/uploads`, `…/{id}/files`, `…/{id}/import`, `GET …/{id}`): the
  files in one call or several (a message is 4 MiB at most; a file larger than a call in
  numbered parts, a path sent again replaces it), stored in Postgres (`import_uploads`,
  `import_upload_files`), never in the API's memory, private to their person, expiring an
  hour after the last files; then the web import's own engine (`run::prepare`: plan,
  refusals, caps, one version per migration), whose job the upload follows (progress, then
  report or error) so `get_import` polls it after the call's 40 s. A refused or failed import
  keeps the files: fix one, import again. Rails, Django and Alembic are imported from their
  exporters' SQL, served as MCP resources. `get_migration_files` is the web download (the
  same endpoints and options), paged at 50 KB of content, every page pinned by a cursor to
  the first one's version and the draft's reviewed text. Code tools (Rails, Django, Alembic)
  get `plain` SQL to wrap in a migration of theirs: romulus generates no Ruby or Python.
  Review first: in an organization that requires approvals or a passing test, an agent's
  import only brings a history into a database without live versions (onboarding); past
  that it is refused (403 `review_required`) and the change goes through a draft in
  migration mode, so the rules for publishing apply to agents too (`run::Gate`, decided on
  the plan; storing refuses a history that moved since). Dry runs pass; the web's and CI's
  imports are not gated. An upload lives a day at most, counts what it holds on its row
  (files, parts, bytes with the paths'), takes 10 parts a file and paths of 512 bytes
  (NFC, no format characters); its import starts in a task of its own and is kept alive by
  a heartbeat. A file's markers carry a code drawn per answer and its length, so no
  content (a comment, a name) can forge the end of one file or the start of another.

## Retention and deletion (decided 2026-10-21, built: task RETAIN)

What romulus keeps, and for how long; the hosted service's Terms and Privacy
Policy promise the same. All of it lives in the API's `retention` module.

- **Deleting on request**: an operator runs `romulus-api delete-user EMAIL` or
  `delete-org SLUG` (the request arrives by email; `--dry-run` says first what
  would go). Self-service deletion is for later: an endpoint calling the same
  `Deleter::delete_account` for the signed-in person.
- **An account** goes with its sessions, password, devices, links,
  personal access tokens, connected apps, memberships, approvals and unread
  markers, and with every organization it is the only member of. All of it
  under the account's row lock, planned and checked again there: a sign-in
  or a token's use meanwhile waits, and an inactive account used is kept.
  Its single sign-on user is deleted at Zitadel under that lock, before the rows
  (`ROMULUS_ZITADEL_TOKEN`, a machine user the provisioner makes with
  IAM_USER_MANAGER; without it the command prints the user to delete in the
  console, and the log warns with its id; rows failing after Zitadel's delete
  are logged as an error with the id: running it again finishes). What the person wrote in organizations they shared is the
  organization's and stays (versions, comments, frozen approvals: by a
  deleted account); the audit log keeps the name it recorded, as for someone
  who left. Requests about those go to the organization, the controller.
  Its contract records (`legal_acceptances`, `withdrawals`: "Legal: terms,
  checkout, withdrawal") stay, with its address, until their own retention
  ends (`legal::purge`); the notices it was sent (`legal_notices`) go.
- **An organization** goes in one statement with everything it holds
  (`ON DELETE CASCADE`; the history triggers allow it once the parent row is
  gone). Refused unless its Stripe subscription, if any, is over for good
  (`canceled`, `incomplete_expired`: an `incomplete` one may still be paid, a
  `paused` one resume): cancel it first (its invoices stay in Stripe). The
  same holds for an organization going with its only member. Its members
  keep their accounts.
- **The commands show the plan first**, `--dry-run` or refused, then the
  refusal (exit 4).
- **An organization whose admins all left**: it cannot happen, the invariant
  `organization_needs_an_admin` holds. Deleting on request the only admin of
  an organization others belong to is refused (appoint another admin, or ask
  for the organization to be deleted too); an organization left with no
  member at all is deleted with that last account; an inactive admin's
  deletion makes the most recently active developer (else reviewer) an admin,
  audited `admin_appointed` and emailed.
- **Confirmed by email** (mail on): the deleted address, or the
  organization's admins. Logged in the sign-in log (`account_deleted`,
  `organization_deleted`, kept its 90 days).
- **Purges**, hourly by one sweeper (one process at a time: an advisory
  lock): deleted databases 30 days after their deletion, invitations nobody
  accepted 30 days after they expired (revoked ones are deleted at once;
  accepted ones are the membership's record). Removed versions already purge
  after 30 days.
- **A purged database's audit events stay** the organization's history (who
  published what, with which approvals): `audit_events.database_id` is
  `ON DELETE SET NULL`, and `database_name`, recorded with every event about
  a database, says which; such events never show in another database's
  activity, and a last one, `database_purged`, says when it went. They go
  with the organization only.
- **Inactive accounts** (`ROMULUS_DELETE_INACTIVE_ACCOUNTS`, off by default so
  a self-hosted install never loses an account; the hosted service turns it
  on; needs mail, and `ROMULUS_ZITADEL_TOKEN` with single sign-on):
  `users.last_seen_at` moves (`retention::activity::account_used`, at most
  hourly) at every sign-in and with the use of a session, of a personal
  access token or a connected app's OAuth token, or of an API token the
  account made (CI pushing to its database). The migration counts those
  already recorded. At 3 years less 30 days the address is emailed the date;
  the account is deleted 30 days later at the earliest, if unused since
  (using it clears the notice). The notice goes once: claimed by an
  `UPDATE … RETURNING` (an hour's lease) before the send, recorded only once
  the mail server took it, the claim given back if not. A failure (mail
  refused, Zitadel down) puts that account off, an hour doubling each time
  up to a week, so failing accounts never hold the others up. An account
  alone in an organization whose subscription is not over is left alone.
- **The running API is told** what a deletion ended wherever it ran (the CLI
  is another process): `pg_notify` per database and membership in the
  deletion's transaction, a listener closing those drafts and sockets.
- **Source** (AGPL-3.0, section 13): `/api/health` and `/api/instance` say
  the commit built (`ROMULUS_COMMIT`, a build argument images.yml passes);
  the user menu links "Source code" to `ROMULUS_SOURCE_URL` at that commit
  (by default the repository the image was built from, so a fork points to
  itself).
- **Zitadel's mail**: the provisioner gives Zitadel the API's own SMTP
  settings (`ROMULUS_SMTP_*`, `ROMULUS_MAIL_FROM`), so sign-up codes and
  password resets arrive.
- Deviations from the data model: `users.last_seen_at`,
  `users.inactivity_notice_at`, `inactivity_failures`, `inactivity_retry_at`;
  `audit_events.database_name`, its `database_id` `ON DELETE SET NULL`;
  `users.updated_at` moves with identity columns only.

## Agents connect by signing in: OAuth (decided 2026-10-21, built: task OAUTH)

A person connects their agent with its URL alone; the agent opens the browser, they sign in
to romulus as usual and approve it (MCP's authorization flow, OAuth 2.1). What was decided:

- **romulus's API is the authorization server of its own MCP resource.** Zitadel stays the
  identity provider only (it has no dynamic client registration, and the MCP resource is
  romulus's). The endpoints are under `/api/oauth/…` (register, authorize, token, revoke) so
  the existing proxying holds; the discovery documents are at the origin's
  `/.well-known/oauth-protected-resource/api/mcp` (none at the root: it would describe the
  origin, RFC 9728 3.3) and `/.well-known/oauth-authorization-server`, which the homepage's
  Function proxies too. The
  issuer is the public URL's origin; tokens are for `<public URL>/api/mcp` only (RFC 8707).
- **Dynamic client registration, open, public clients only.** Limited per client address;
  no client secret (PKCE protects the code); https redirect URIs, or loopback http on any
  port (RFC 8252); nothing unverified is shown but the name the client gave, always next to
  its redirect URI's host. Clients unused for 30 days with no grant left are deleted. Client
  ID metadata documents (which Claude prefers when offered) are not served yet: fetching a
  URL a stranger names is its own piece of work.
- **romulus never redirects to a client on its own before the person decided.** Clients
  register themselves, so a refusal sent straight back to a redirect URI would make romulus an
  open redirect: refusals (PKCE, scope, resource, rate limit) stay on the consent page, which
  offers "Return to <host>", built and checked again by the API. Only the person's decision
  (a code, or `access_denied`) goes back by itself.
- **The consent screen is the web app's** (`/app/oauth/authorize`), after the usual sign-in.
  The request is kept by the API and bound to the browser that started it by a cookie: a
  link to someone else's request shows and approves nothing. The person may allow less than
  asked (read only). Localhost redirects get a warning: any local program can ask. PKCE
  (S256 only) is required; `state` is not (OAuth 2.1 with PKCE, and some MCP clients omit
  it): echoed back unchanged when sent.
- **Tokens are the personal tokens' identity.** Access tokens (`rmls_oat_`, an hour, sha256
  stored) resolve to the same agent acting as its person, so tools, roles, caps, limits and
  audit (`agent_action`, with the app's name) are unchanged. Scopes `romulus.read` and
  `romulus.write` are the tokens' read and write. Refresh tokens (`rmls_ort_`) rotate on
  every use; a replay, or a code exchanged twice, revokes the whole grant. 30 days unused,
  90 days at most. A read-only app calling a write tool gets `insufficient_scope` (step-up).
- **Connected apps** sit next to Access tokens in the account; Disconnect revokes every
  token of that app at once. A password reset or change disconnects them all, like tokens;
  deleting an account deletes its grants. The person is emailed when an app connects.

## Decided

- Uploads: SQL only (remus JSON accepted on the push endpoint).
- SQL text is the source of truth for the schema; layout and groups live in Yjs.
- SSO: Zitadel (hosted). Self-hosted installs sign in with email + password (above). Router:
  TanStack Router.
- Graph → text schema edits are in the MVP.
- Branches are supported, as drafts, with one-click creation and publishing.
- Homepage lives in `site/`.
- Missing roles, functions and extensions are auto-stubbed in the sandbox;
  stubs never reach the model.
- "Update draft" merges SQL per statement, keyed by the object defined, on the
  server, applied as a minimal Yjs edit.
- Graph schema edits append SQL statements; Tidy rewrites canonically.
- Invitations are copy-link; no email provider in the MVP.
- Zitadel is identity only; romulus owns organizations and roles.
- One API instance in the MVP (Yjs docs in memory).
- Hosted service: the homepage on Cloudflare Pages; `/app` and `/api` proxied
  to the API by a Pages Function, keeping one origin. No Cloudflare Access:
  romulus's own sign-in protects it.
- Hosted billing: Stripe, plans as hard caps that never delete, history hidden not
  deleted, playground hours metered; off (and absent) on self-hosted installs.
