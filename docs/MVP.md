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
   Built (crates/romulus-migration): renames appear as a drop and an add,
   flagged "possibly a rename" with the `RENAME` offered in a comment; columns
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
- Graph schema edits append SQL statements; Tidy rewrites canonically.
- Invitations are copy-link; no email provider in the MVP.
- Zitadel is identity only; romulus owns organizations and roles.
- One API instance in the MVP (Yjs docs in memory).
- Preview hosting (deploy/preview): the homepage on Cloudflare Pages; `/app`
  and `/api` proxied to the API by a Pages Function, keeping one origin; the
  API, Zitadel and the databases in containers, behind a Cloudflare
  Tunnel. No Cloudflare Access: romulus's own sign-in protects it.
