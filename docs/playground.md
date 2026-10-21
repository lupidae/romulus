# Playground: design (2026-10-16; phase 1 built 2026-10-17)

> **Decisions.** (1) gVisor isolation, no network egress, no superuser,
> per-playground resource caps and the separation from the app's
> containers/volumes/secrets are mandatory, not optional. (2) Console in the MVP:
> read-only queries for everyone + running the draft's migration for developers.
> (3) LLM-assisted values: later (phase 3), opt-in per organization, schema only.
> (4) Developers and admins create playgrounds; reviewers open and query existing
> ones. The other open questions take their recommended answers.


From docs/MVP.md, "Ideas, after the round": *a real throwaway database per draft
with generated fake data, to run SQL and try migrations*. The design
questions are in section 6, each with a recommended answer.

**In one paragraph.** A playground is a real Postgres holding one schema (a
version, or a draft's base) filled with generated rows. It runs as its own
cluster in its own gVisor container, with no network, a disk quota and a
memory cap, preferably on a host apart from the app. From a draft or a version, people query
it in a console (results, plans, a shared history) and run the draft's
migration on its rows. The report shows what fails and on which rows, which
locks each statement takes, which tables it rewrites, and how long it ran.
Reset puts the generated rows back in about a second. Data is synthetic only
and seeded: the same schema, size and seed give the same rows, so a lost
playground can always be built again.

## 1. What users get

**Opening one.** **Playground** in the draft editor's bar, **Open
playground** on a version page, a list in the History tab. A draft's
playground holds the draft's **base version**, because that is where real rows
live; the draft's migration then runs on them.

```
┌ New playground ────────────────────────────────────────────────┐
│ Schema    v12 (the draft's base) · 48 tables                   │
│ Rows      (•) Small   ~40 k rows   ~10 s                       │
│           ( ) Medium  ~1 M rows    ~1 min                      │
│           ( ) Large   not on this server                       │
│ Profile   (•) Realistic   ( ) Edge cases                       │
│ Seed      7f3a91  (same seed, same rows)        Language  en ▾ │
│ ▸ Rows per table…                 [Cancel]  [Create playground]│
└────────────────────────────────────────────────────────────────┘
```

Building streams progress, as imports do: schema, each table's rows,
constraints, `ANALYZE`. It ends with a report of what the generator could not
satisfy, which is always shown ("orders: 112 of 24 000 rows broke
`orders_total_matches` and were dropped").

```
┌ acme / billing / Playground · Draft from v12 ─ Ready · Small · seed 7f3a91 · Alice, Bob ┐
│ Tables              │ SELECT c.name, count(*) FROM customers c                [Run ⌘↵]  │
│  public             │ JOIN orders o ON o.customer_id = c.id                   [Explain] │
│   customers  1 200  │ WHERE o.created_at > now() - interval '30 days' GROUP BY 1;       │
│   orders    24 000  │───────────────────────────────────────────────────────────────────│
│   order_items 71 k  │ Results   Plan   Messages                  212 rows · 38 ms       │
│ History (shared)    │ name              │ count                                         │
│  Bob · migration ✗  │ Rhoda Wilkinson   │ 31                                            │
│  Alice · SELECT …   │ …                                                                 │
├─────────────────────┴───────────────────────────────────────────────────────────────────┤
│ Migration v12 → draft · 7 statements    [Run migration]   [Reset rows]   Expires in 6 d │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Console.** CodeMirror with the editor's SQL support. Each statement run
  gets its own result. The grid streams the first 1 000 rows in Postgres's own
  text output, so geometry, vectors, ranges and enums read as in psql.
  Errors are located in the text, like compile errors. The console shows the
  transaction state (idle, in a transaction, failed), and **Cancel** stops the
  running statement.
- **Plan.** **Explain** runs `EXPLAIN (FORMAT JSON, ANALYZE, BUFFERS)`; a
  statement that writes runs in a transaction that is then rolled back. The
  plan shows as a tree. Each node gives its own time, actual and estimated
  rows (flagged past 10×), and buffers. Its tables link to the schema graph.

  ```
  Hash Join                    212 rows (est. 180)    ███░░░░░  12 ms
  ├ Seq Scan on orders       24 000 rows              ██████░░  22 ms  ⚠ filter dropped 96 %
  └ Hash
    └ Seq Scan on customers   1 200 rows              █░░░░░░░   2 ms
  ```
- **History**, shared: who ran what, when, how long, rows or error. It never
  stores results, and any entry can be run again.
- **Run migration** resets the rows, then applies the draft's migration as
  the chosen tool would. In schema mode that is the generated migration; in
  migration mode, the written one. Its backfills run for real here, which a
  compile never does. Afterwards the playground holds the migrated rows, until
  **Reset rows**. Everyone in it is told who ran or reset what.

  ```
  Migration v12 → draft (plain) on Small rows: failed at statement 4 of 7
   ✓ 2 ALTER TABLE orders ADD COLUMN status …                  4 ms  ACCESS EXCLUSIVE on orders · no rewrite
   ✓ 3 ALTER TABLE orders ALTER COLUMN total TYPE numeric(12,2)
                                                             310 ms  ACCESS EXCLUSIVE on orders · rewrote it
                                                                     (24 000 rows; ≈ 9 min at 40 M, your estimate)
   ✗ 4 ALTER TABLE customers ALTER COLUMN email SET NOT NULL
       ERROR 23502: column "email" of relation "customers" contains null values
       118 of 1 200 rows have no email: id 17, 44, 91…   [Query them]
     5–7 not run.  Before the failure: no statement flagged deletes_data ran.
  ```
- **Lifetime.** A playground stops after 15 minutes without a session and
  starts again in seconds. It is deleted after 7 days unused, or with its
  draft; the page shows the time left. Recreating it with the same seed gives
  the same rows.

**What the page tells people it cannot do.** The rows are synthetic, so plans
are indicative. A migration that passes here can still meet a production row
the generator never imagined; the **Edge cases** profile narrows that gap.
Durations are measured on this server with no concurrent load, then
extrapolated: they are orders of magnitude. Lock modes and rewrites are exact.
Functions the schema calls are stubs returning NULL, as in compiles, because
function bodies are not in the model.

## 2. Fake data generation

**Input: the remus model.** For each column: type and kind (base, enum,
domain, composite, range), array, `NOT NULL`, default, identity, generated
expression, comment. For each table: PK, unique, FK, CHECK and exclusion
definitions as Postgres prints them, indexes (unique, partial). Also enum
labels, domains (base type and CHECKs), composites, partitions with their
bounds, and extensions. The model has no row counts or statistics: remus does
not read them.

**Pipeline: generate in Rust, let Postgres judge**, as the compile lets
Postgres decide what a schema is.

```
plan (pure Rust)                         load (per table, in FK order)
topological order of tables              COPY rows ──▶ staging.<t>   (unlogged, no constraints,
  (cycles broken at a nullable FK)                                     domains as their base type)
rows per table (preset × table role)     INSERT INTO <t> SELECT … FROM staging.<t>
generator per column (type, name,          WHERE (each CHECK, domains' too) IS NOT FALSE  ← Postgres's semantics
  CHECK shapes, enum labels)             ON CONFLICT DO NOTHING              ← PK, unique, exclusion
seeded RNG per column                    dropped = staged − inserted → the report
                                         read back the keys children need
                                         then: cycle UPDATEs, REFRESH matviews, ANALYZE, clean stop
```

- **Foreign keys** draw from parent keys read back from the database. That
  covers identity and defaulted keys, composite keys, and FKs to unique
  non-PK columns. A unique FK (1:1) samples without replacement. Fan-out is
  skewed (Zipf), because skew is what makes plans realistic. A
  self-reference or a cycle is loaded NULL first, then filled by a
  deterministic `UPDATE`. A cycle that is `NOT NULL` both ways and not
  deferrable is loaded with one foreign key dropped and then added back
  (which checks it; the owner may do both). If adding it back fails, the
  cycle's tables stay empty, and the report says why.
- **CHECKs.** These shapes are generated to pass: bounds, `BETWEEN`,
  `IN`/`= ANY (ARRAY[…])`, `length(x) <= n`, `a < b`, `num_nonnulls`, and
  regexes (via `rand_regex`). Any other CHECK is filtered by the `INSERT …
  WHERE`, and the report shows the share of rows each one dropped.
- **Types.** Numbers within precision, strings within length, uuid,
  date/time types, bytea, network types, json/jsonb (small objects, shaped by
  the column's name), bit strings, ranges, arrays, enums, domains, composites.
  Also the allowlisted extensions' types: citext, hstore, ltree, PostGIS (EWKT
  from the typmod) and pgvector (unit vectors of the right dimension). A type
  it cannot produce is NULL where the column allows it. Otherwise the table is
  skipped, and the report names the type.
- **Partitions.** Key values come from the bounds (`FROM … TO`, `IN`,
  modulus), parsed with `pg_query`. A `DEFAULT` partition also gets
  out-of-bound values. Partitions whose bounds are not understood get no rows.
- **Left to Postgres:** generated columns, identity columns, and defaults
  with no reason to override. Some are overridden anyway: every row would
  share `created_at DEFAULT now()`, and a stub default is NULL.

**Realistic values.** Column-name heuristics, checked against the type:
- People and places: `email`, names (a person on `users`/`customers`, a
  product or company elsewhere), `phone`, `address`, `city`, `country`
  (ISO codes when `char(2)`), `zip`.
- Codes and identifiers: `currency`, `url`, `slug`, `ip`, `user_agent`,
  `token`, `*_hash`, `locale`, `timezone`.
- Text and amounts: `title`, `description`/`body` (sentences),
  `price`/`amount` (positive, log-normal), `quantity`, `lat`/`lng`,
  `birth_date`.
- Timestamps: `*_at` falls in the last two years and stays ordered within a
  row (`created_at ≤ updated_at`, `starts_at < ends_at`).
- NULLs: `deleted_at` is mostly NULL; other nullable columns are about 10 %
  NULL.
- Emails and URLs use reserved domains only (`example.com`, `.test`).

**Profiles.** *Realistic* is the default. *Edge cases* aims at the rows that
break migrations: many NULLs, strings at their maximum length, duplicates
in non-unique columns, extreme numbers, empty and non-ASCII strings.

**Volume presets**, by each table's role in the FK graph. Each table's count
can be changed; the caps hold. Generation refuses to start when its byte
estimate exceeds the disk quota.

| preset | lookup tables | entity tables | event tables (FKs out, `*_at`, unreferenced) | cap |
|---|---|---|---|---|
| Small (default) | 10–50 | ~200 | ~2 000 | 50 k rows |
| Medium | 10–200 | ~10 000 | ~100 000 | 1 M rows, ~300 MB |
| Large (dedicated host only) | 10–500 | ~100 000 | ~1 M | 10 M rows, ~3 GB |

**Determinism.** Each column draws from its own ChaCha8 RNG, seeded with
`sha256(seed ‖ schema.table.column)`. Adding a column leaves the other
columns' values as they were. A playground is reproducible from (model,
preset, profile, seed, locale, generator version). The generator version is
stored with the playground, because heuristics change between releases.

**LLM-assisted values (off by default).** Some columns no heuristic knows: a
`tier` text column, `notes` on a veterinary table. For those, a model proposes
value vocabularies, never rows. It picks one of a closed list of generators
(`choice` with 30 values, `pattern` with a regex, `text` with 50 samples) and
returns schema-validated JSON. That JSON is never SQL and never runs. Results
are cached by each table's definition. The model sees the schema (names,
types, comments, CHECKs, enum labels), never rows. Turning it on sends schemas
to Anthropic, so an admin enables it per organization, and the setting says
so. Once per schema, at ~300 output tokens per uncovered column:

| schema | `claude-opus-5-5` ($4/$20 per M) | `claude-sonnet-5-5` ($2/$10) | `claude-haiku-4-5` ($1/$5) |
|---|---|---|---|
| 30 tables (~50 columns uncovered) | ~$0.35 | ~$0.18 | ~$0.09 |
| 300 tables (~500 uncovered) | ~$3.40 | ~$1.70 | ~$0.85 |

**Write it, or adopt a tool?**

| option | licence | fit | verdict |
|---|---|---|---|
| **Own crate `romulus-fakedata`** on [`fake`](https://github.com/cksac/fake-rs), `rand_chacha`, `rand_regex` | MIT/Apache-2.0 | Reads the remus model directly, deterministic per column, no new runtime. No I/O, so it builds for wasm and could preview rows in the browser. Type coverage and heuristics are ours to write. | **Recommended** |
| [synth](https://github.com/shuttle-hq/synth) (Rust) | Apache-2.0 | Own schema language; Postgres import "alpha"; no CHECKs. Last commit Sept 2024. | No |
| [Snaplet seed](https://github.com/snaplet/seed) (TS) | MIT | Good relational planner and `copycat` (determinism by hashing). Needs a Node service, introspects a live database itself, configured as code. Community-maintained since Snaplet closed. | No; borrow ideas |
| [Neosync](https://github.com/nucleuscloud/neosync) (Go) | MIT | Anonymisation platform. Archived Aug 2025 after an acquisition. | No |
| PostgreSQL Anonymizer | PostgreSQL | `anon.fake_*()` masks existing rows. A C extension (more code reachable from the console), with no FK planning. | No |
| Pure SQL (`generate_series`, `setseed`) | — | Fast, but realism needs dictionaries in the database, and determinism depends on plan order. | It is the load step above, not the generator |
| LLM rows, SaaS (Mockaroo, Tonic) | — | Slow and not deterministic, or schemas leave the deployment. | No |

**Built (2026-10-17, `crates/romulus-fakedata`): what the design left open.**

- **No `fake`.** It pulls rand's OS randomness, which does not build for
  `wasm32-unknown-unknown`, and its data and draws could change on a minor
  bump. The word lists are ours, so rows change only with the generator
  version. `rand_chacha` and `rand_regex` as planned.
- **Keys are computed, not read back.** Every cell is a pure function of
  (seed, table, column, row): one ChaCha8 stream per row on the column's
  key. A foreign key copies the parent's key for the parent row it picked,
  computed directly, so the plan needs no round trip and streams. Identity
  and serial keys that others point at are written `1, 2, 3…`, and their
  sequences moved past them. A child whose parent Postgres dropped is
  dropped by the load's parent-exists filter, and the report says so.
- **Unique by construction.** Each unique set gets one column that carries
  the row number (`rhoda.wilkinson17@example.com`), or is drawn as a
  combination of its foreign keys and small domains from a permutation, so
  `ON CONFLICT` is a safety net rather than the mechanism. Short types cap
  the row count.
- **CHECKs are read in Rust**, not with `pg_query` (C, so no wasm): the
  shapes are built to pass, and other readable CHECKs are evaluated per row
  and the row drawn again.
- **Staging tables are all `text`.** `COPY` never fails on a value, and the
  move into the real table keeps only `pg_input_is_valid` input, so
  PostgreSQL 16 or later. Each move is a `DO` block. If the bulk insert still
  raises, it falls back to one row at a time in subtransactions. The plan
  never aborts on data. What was dropped, and why, goes to an outcome table.
  The runner reads it with a third kind of step, `Outcome`.
- **Cycles.** A nullable foreign key in a cycle, or a nullable
  self-reference, is loaded NULL, then filled by an `UPDATE` that keeps
  CHECKs and existing parents. A cycle `NOT NULL` both ways loads in one
  block: every key between its tables is held (deferred when deferrable,
  else dropped and added back), and the rows left without a parent are
  deleted, in every direction, until none is (reported as dropped). Only
  if that block still fails does the cycle stay empty, as planned.
- **Determinism across machines.** `ln`, `cos` and `powf` come from the
  system's libm, which may differ by an ulp, so the generator uses the
  `libm` crate (pure Rust) for every transcendental. Regex samplers draw
  printable characters only: `.` and `[^@]` would otherwise draw NUL, which
  Postgres text cannot hold.
- **"Now" is fixed per generator version** (2026-10-01), since a
  playground must rebuild identically. Bumping it is a version change.
- **Forced row level security is off for the whole load** and back on at
  the end: the owner's parent checks and materialized view refreshes must
  see every row.
- English only for now. Counts set per table (`plan_with_rows`) replace
  the preset's; past the size's cap the other tables shrink first, then
  those.

## 3. Isolation and security

**Why the compile sandbox's model does not carry over.** The sandbox is safe
because almost nothing runs: allowlisted DDL, no functions, every compile
rolled back. Trusting its extensions is acceptable only because no user
function exists for their install scripts to pick up (deploy/sandbox/README.md).
A playground inverts all of that: people run DML, call any function, later
create PL/pgSQL functions, commit, and keep sessions open. **A playground
never uses the sandbox cluster**, and nothing here relaxes the sandbox.

"Never superuser" is necessary but not sufficient. Unprivileged SQL users have
repeatedly gained memory writes or superuser rights:

- arrays: [CVE-2021-32027](https://www.postgresql.org/support/security/CVE-2021-32027/), [CVE-2023-5869](https://www.postgresql.org/support/security/CVE-2023-5869/);
- SQL run as a superuser by anyone who can create objects: [CVE-2020-25695](https://www.postgresql.org/support/security/CVE-2020-25695/);
- injection into extension scripts: [CVE-2023-39417](https://www.postgresql.org/support/security/CVE-2023-39417/).

PostGIS and pgvector add C code on top. The design therefore assumes a
playground's Postgres **will** be compromised one day, and limits what that
gains.

**Threat model.** The attacker is anyone with an account. Sign-up is open and
a new account can create its own organization, so the attacker is a
developer in their own playground. What it can reach, most valuable first:
the host and the app (app database, `.env` secrets, the compile sandbox);
other organizations' playgrounds (schemas are confidential designs, rows are
synthetic); availability; the host network's reputation (scans, spam, mining).

| threat | mitigation |
|---|---|
| **Escape to the host**: an RCE in Postgres or an extension gives a shell in the container, then a kernel bug gives the host. | **gVisor** (`runsc`): the container's syscalls go to a user-space kernel. Read-only root filesystem, no capabilities, `no-new-privileges`, a pids limit. **A host of its own** (recommended), so an escape lands where there is no app data and no app secret. The image is pinned and rebuilt for each Postgres minor release. |
| **Other tenants**: shared catalogs, files or memory. | **One cluster per playground**: nothing shared but gVisor and the host. Opaque names. |
| **Superuser powers** (`COPY … PROGRAM`, untrusted languages, server files, `ALTER SYSTEM`). | The superuser exists only at init, inside the container, with no password and no `pg_hba` line afterwards. People's roles hold none of `pg_execute_server_program`, `pg_read_server_files`, `pg_write_server_files`. No untrusted language is installed. |
| **Extension scripts run as superuser** (the CVE-2023-39417 class). | Extensions come from the model's list, intersected with the allowlist, and init installs them **before anyone connects**. This image marks nothing trusted, so Postgres refuses `CREATE EXTENSION` to everyone. It deletes the control files of everything off the allowlist (dblink, postgres_fdw, file_fdw ship with the server). |
| **Egress** (fdw, untrusted languages, a shell). | `--network none`: only loopback, so even root has no route out. The service reaches Postgres over a Unix socket. |
| **CPU.** | cgroup `cpus: 1`. A global cap on statements running at once (a queue, like imports' `Turns`). The **service** enforces a deadline per statement: `statement_timeout` is user-settable (`SET`), so it is only a convenience. Past the deadline the service cancels, then terminates. |
| **Memory.** | The sandbox's `RLIMIT_DATA` per backend (256 MB here), set by the entrypoint and out of SQL's reach. A cgroup limit of 512 MB. An OOM kills that playground only. |
| **Disk** (`generate_series(1, 1e9)`, temp files, WAL). | An **XFS project quota** per playground (data and WAL, 1–2 GB). `temp_file_limit`, which only a superuser can raise. `wal_level=minimal`. Postgres has no quota per database, one more reason against a shared cluster. Reset repairs a full or corrupted cluster. |
| **Connections and API load.** | 3 sessions per person per playground. Rows stream through a bounded channel, so a slow client stalls only its own backend. At most 1 000 rows shown, 64 KiB per cell and 16 MiB per run, then cancel. |
| **Many playgrounds.** | Caps on playgrounds existing and running, per organization, per database and globally. Rate limits on create and reset. Idle stop, expiry. |
| **Persistence.** | What people change lives in their cluster, which Reset replaces and expiry deletes. The generated copy is made before anyone connects. Only metadata flows back (history, reports), shown as text. |
| **Members of one playground misleading each other** (a planted function or view). | Accepted: it is as trusted as the draft's people, its rows are synthetic, and reviewers are read-only. |
| **Forged calls to the service or runner.** | The API is the only public entry point (session, CSRF, `Member`). The service accepts only API-signed calls; the runner accepts only the service, and only fixed operations. |
| **Logs.** | Postgres logs no statements (`log_min_error_statement = panic`, as in the sandbox). The service logs ids and timings, never SQL. |

**Residual risks, honestly.** A gVisor escape plus a kernel bug reaches the
playground host: other playgrounds' schemas, not the app (when playgrounds run
on a host of their own). CPU side channels exist on any shared host. The runner
holds the Docker socket, which is root on its host, so it stays tiny and fixed. A customer who needs more gets a
dedicated playground host: the same design with one tenant.

**Options compared.**

| | A. Shared cluster, a DB per playground | B. Container per playground, hardened runc | **C. Container per playground, gVisor** | D. microVM (Firecracker, Kata) | E. Managed branching (Neon…) |
|---|---|---|---|---|---|
| A Postgres RCE gives | **every tenant** | its playground; the host needs a kernel bug | its playground; the host needs gVisor and kernel bugs | its VM | the provider's problem |
| Quotas per tenant | no disk quota; per-backend memory only | cgroups + XFS quota | same | VM size | plan limits |
| Egress | cluster-wide only | `--network none` | same | VM networking | open (dblink, fdw) |
| On ordinary cloud VMs and servers | yes | yes | **yes**: systrap needs no KVM | only where KVM is available | — |
| Cost | ~10 MB per DB | ~60–100 MB per playground | + gVisor; I/O slower (to measure) | + a guest kernel | money; schemas leave the deployment |
| Verdict | **No** | fallback | **Recommended** | later, on KVM hosts | No |

What Neon would bring is copy-on-write; XFS reflinks give it at playground sizes
(Reset, below). Self-hosting Neon (pageserver, safekeepers, compute nodes) is
far beyond one host.

**Sizing.** A playground is budgeted at 512 MB (cgroup) and 1 GB of disk
quota (Medium). Idle, it should use 100–150 MB (64 MB shared buffers plus
gVisor); phase 0 measured 58 MB fresh and about 200 MB after a 1 M-row load.
Reflink copies share unchanged blocks.

| resource | per playground | bounds |
|---|---|---|
| memory | 512 MB cgroup while running; 0 when stopped | running playgrounds ≈ memory left after the app ÷ 512 MB |
| disk | its XFS quota (1–2 GB; Medium 1 GB), held while it exists | existing playgrounds ≈ free disk ÷ quota |
| CPU | 1 CPU (`cpus: 1`) per running statement | statements at once without waiting ≈ the host's CPUs |

CPU binds first: a host with N CPUs runs N statements at once without
waiting; four are enough for a team preview. Starting caps: 2 running and 5
existing per organization, Large off. **Scaling out:** each playground host
runs a service and a runner and registers its capacity. The API places new
playgrounds by free room, and a playground stays on its host, as a draft room
stays on its API. Losing a host loses only rows, which the seed rebuilds;
history lives in the app database. KVM hosts can swap gVisor for microVMs
without the API noticing.

## 4. Architecture

```
browser
   │ /api: cookie, CSRF, Member
   ▼
romulus-api ── app DB: playgrounds, playground_runs · roles, limits, placement
   │ signed HTTP + websocket over a private link (private network or WireGuard)
   ▼
playground host ──────────────────────────────────────────────────────────────
│ playground service (unprivileged): build, generate, console, migration runs
│     │ fixed operations, local socket                │ Postgres over /run/<id>/.s.PGSQL.5432
│     ▼                                               ▼
│ runner (Docker socket) ── runsc, --network none ──▶ pg-<id>  Postgres 17, data on an XFS quota
│                                                     pg-<id>  …
└──────────────────────────────────────────────────────────────────────────────
```

- **romulus-api** owns what is durable and who may do what: the tables, the
  role matrix, limits, placement, and endpoints under
  `/api/orgs/{org}/databases/{db}/playgrounds/…`. It proxies the console
  websocket after `csrf::SameOrigin` and `Member`. It closes the socket on
  membership changes and on shutdown, as collab does.
- **playground service** (a new crate, `romulus-playground`, unprivileged)
  builds, generates, runs sessions and migrations over each playground's
  socket. It trusts only calls the API signed (HMAC over playground, user,
  role and expiry) on a private link: a private network or WireGuard.
- **runner** (tiny, privileged) does `create`, `start`, `stop`, `snapshot`,
  `restore`, `destroy` and `stats` for a playground id and a size class, plus
  XFS quotas. Image, flags and mounts live in its code; nothing in a request
  becomes a Docker argument.

**Lifecycle.**

```
requested ─▶ building ─▶ generating ─▶ ready ⇄ stopped (idle 15 min; disk kept)
                 └──── failed ◀──┘       ├─▶ resetting ─▶ ready
                                         └─▶ migrating ─▶ ready (migrated rows)
any ─▶ expired (7 days unused, draft published or deleted) ─▶ destroyed
```

1. **Init.** The container's entrypoint runs as superuser, before the socket
   is exposed. It runs `initdb`, creates the people's roles, the model's
   extensions and the schema's policy roles (real `NOLOGIN` roles, so RLS
   can be tried later), then leaves `pg_hba` with only scram for those roles
   on the socket. The image is a variant of the sandbox image: same Postgres
   and extensions, the `RLIMIT_DATA` entrypoint, nothing trusted. Settings:
   `listen_addresses=''`, `max_connections=20`, `shared_buffers=64MB`,
   `temp_file_limit=256MB`, `wal_level=minimal`, and `fsync=off`, which is
   fine because Reset answers a crash.
2. **Schema**, as the owner role. The SQL is what a new draft from that
   version starts with: the version's source, or `render_sql` when the
   source is not the whole schema. It goes through `romulus_sandbox::prepare`
   (stubs with the signatures the compile settled on, missing schemas,
   extension objects) and is committed.
3. **Rows**: section 2's pipeline, streamed as `COPY`.
4. **Generated copy**: `ANALYZE`, `CHECKPOINT`, clean stop, then
   `cp --reflink` of the data directory into `golden/`. On XFS that is
   instant, and blocks are shared until one changes.
5. **Reset**: stop, reflink `golden/` back over `data/`, start. About a
   second at any size. It also undoes a crash, a full disk or a corruption.

**Roles in a playground.** `play_owner` owns the schema (developers, from
phase 2). `play_read` gets `USAGE` and `SELECT` on everything, and no
`CREATE` or `TEMP` (reviewers, and everyone in phase 1). These are real
privileges, not a parse of the SQL, which a function or `EXECUTE` could
always fool. Passwords are HMAC(secret, playground, role), as the sandbox
derives its pool's, so nothing is stored.

**Migration runs.** The service resets, takes the migration
(`romulus_migration::migration(base, draft)` and `files(…, format)` in schema
mode; the written text in migration mode), and applies it as the tool would.
`tests/oracle/tools.rs` already encodes each tool's transaction rules; it
becomes a library function shared by the oracle and the playground. Each
statement runs in its own `BEGIN … COMMIT` where the tool allows it. The
service records:

- duration;
- locks, from `pg_locks` for its own backend before `COMMIT`: exact.
  `CONCURRENTLY` statements, which cannot run in a transaction, get the
  documented mode, marked as such;
- rewrites: the `relfilenode` changed, which is exact;
- rows touched and scans, from `pg_stat_xact_user_tables`;
- for a `deletes_data` cast, the rows it changes, counted beforehand
  (`(col::new)::old IS DISTINCT FROM col`);
- on failure: SQLSTATE, message and detail, located in the text; offending
  rows sampled by a query matching the statement (`IS NULL`, `GROUP BY …
  HAVING count(*) > 1`, `NOT (check)`, `NOT pg_input_is_valid(col::text,
  'type')`, `length(col) > n`).

With production row counts typed per table (phase 2), times are scaled:
linearly for scans and rewrites, n log n for index builds, labelled as
estimates. Lock levels need no data. They belong in `romulus-migration` as a
static annotation shown in every migration view (a small, separate task); the
playground adds the measured part.

**Console API.** A websocket per tab (`…/playgrounds/{id}/console`) is one
Postgres session, so a transaction can span runs and closing the tab rolls
it back.

```
client: {"type":"run","id":"r7","sql":"…","explain":false|"analyze"}   {"type":"cancel","id":"r7"}
server: {"type":"columns","id":"r7","columns":[{"name":"email","type":"text"}]}
        {"type":"rows","id":"r7","rows":[["17","rhoda@example.com"],…]}          ≤ 500 per message
        {"type":"done","id":"r7","tag":"SELECT 212","rows":212,"truncated":false,"ms":38}
        {"type":"error","id":"r7","sqlstate":"23502","message":"…","detail":"…","span":{…}}
        notice · plan · statement (index, span) · txn (idle | in_transaction | failed)
        {"type":"playground","event":"reset"|"migrated"|"stopping","by":{…}}    broadcast
```

- The SQL is split with `romulus_sandbox::parse` and run in the simple query
  protocol, so every value arrives as Postgres's text. `COPY … STDIN/STDOUT`
  is refused, with an explanation.
- Cancel is `pg_cancel_backend` from the service's control session (a role
  may signal its own backends), then `pg_terminate_backend` after a grace
  period.
- 1 000 rows are shown, up to 10 000 with "Show more"; past that the
  statement is cancelled and the grid says so. Cursor paging comes later.
- Deadlines: 30 s for reviewers, 2 min for developers. One statement at a
  time per session.

**Lifecycle REST** follows the API's conventions. `POST …/playgrounds`
(developer) and `POST …/{id}/migration` (developer) stream NDJSON progress,
like imports. The others are `GET` (list and one), `POST …/{id}/reset` and
`DELETE`. New error codes: `playground_limit`, `playground_expired`,
`host_unavailable`.

**Persistence (app database).**

```
playgrounds      id, org_id, database_id, draft_id NULL, version_id, model_sha256, preset, profile,
                 seed, locale, generator_version, row_counts jsonb, status, host, report jsonb,
                 created_by, created_at, last_used_at, expires_at
playground_runs  id, playground_id, user_id, kind (query|explain|migration|reset), sql (≤ 64 KiB),
                 migration_input_sha256 NULL, started_at, duration_ms, status, rows, error jsonb, report jsonb
```

Results are never stored. Runs go with their playground, and a playground
with its draft. A migration run keeps the draft's reviewed hash
(`compile::input_hash`), so the publish dialog can show "tried on data:
passed, 5 min ago" next to approvals, which cover the same text (phase 2).

**Collaboration.** Three ways to share a playground:

- (a) one per draft or version, shared, with sessions per person;
- (b) one per person;
- (c) shared, plus "fork for me": a reflink copy and a container.

**(a) now, (c) later.** Capacity is the constraint, and the use is
collaborative ("Bob ran the migration, here is the failure"). Reset and
migration runs are announced. Open transactions they end are told why. The
console text is per person, not a Yjs document; the history is shared.

**Web.** `features/playground/` holds the console session, results grid, plan
tree and migration report. The route `$org/$db/playgrounds/$id` is full
bleed. The draft editor gets **Playground**, and the Migration tab **Try on
data**. The plan tree is our own component in our tokens. PEV2 is Vue (a
second framework); it is the reference for what to show.

**Cost.** Cost is capacity: a playground costs memory while it runs and disk
while it exists. Money enters with bigger hosts or LLM-assisted values
(section 2).

## 5. Phasing

**Phase 0, spike** (decides section 3, ships nothing to users). On the
playground host, check:

- runsc (systrap) runs Postgres 17 with PostGIS and pgvector;
- the service connects through a Unix socket on a bind mount, which needs
  gVisor's host-UDS support. The fallback is a per-playground internal network
  that only the service joins;
- XFS quotas and reflinks work.

Exit criteria:

- start under 5 s;
- reset under 3 s;
- idle under 200 MB;
- 1 M rows loaded with runsc no worse than 2× runc.

Failing any of them means hardened runc (option B) on a separate host, never
a shared cluster.

**Phase 0 result: passed.** runsc release-20261012.0 (systrap, no KVM), with
the sandbox image (Postgres 17.11, PostGIS 3.6, pgvector 0.8.7) and a
512 MB cgroup:

| | runc | runsc | target |
|---|---|---|---|
| cold start, initdb included | 0.9 s | 2.5 s (3.6 s with `--network none` and the socket) | < 5 s |
| reset, template clone (100 k rows, `file_copy`) | 0.2 s | 0.4 s | < 3 s |
| reset, reflink copy of the stopped data dir (98 MB) and start | | 1.6 s + 0.7 s | < 3 s |
| idle, fresh cluster | 18 MB | 58 MB | < 200 MB |
| idle, after loading 1 M rows (page cache counted) | 159 MB | 204 MB | |
| 1 M rows with a vector and a geometry | 4.7 s | 3.5 s | ≤ 2× runc |

- **Socket:** the `runsc-uds` runtime (`--host-uds=all` in phase 0; `create`
  since, which is all a Postgres making its socket needs) lets Postgres in a
  `--network none` sandbox (loopback only, `listen_addresses=''`) create its
  socket in a bind mount, and a separate container connects through it. The
  internal-network fallback is not needed.
- **XFS:** a file-backed XFS volume works (`reflink=1`, `prjquota`). A 150 MB
  project quota stopped a 300 MB write
  from inside a runsc container at exactly 150 MB. Setting a quota needs
  `CAP_SYS_ADMIN` (`xfs_quota -x`), which the runner already has by holding the
  Docker socket: it runs `xfs_quota` in a short-lived privileged container
  with the volume mounted. No sudoers rule.

**Phase 1, MVP.**
- Playgrounds from a draft's base or a version: Small and Medium, both
  profiles, seeded.
- The generator: core types and extension types, FKs, uniques, CHECKs,
  enums, domains, list and range partitions, and its report.
- A **read-only console for everyone**: grid, `EXPLAIN ANALYZE` of reads as
  text, shared history, cancel.
- **Run migration** (developers, plain format): the per-statement report,
  migrated rows queryable, Reset.
- Idle stop, expiry and caps; the runner, the service, and the API endpoints
  and tables.
- Tests: generator unit tests; an oracle loading generated rows for every
  corpus schema with no CHECK or FK surprise; API tests per role; one
  Playwright path.

**Phase 1 as built (2026-10-17).** The runner (`crates/romulus-play-runner`),
the service (`crates/romulus-playground`), the image (`deploy/playground`), the
API module (`playgrounds`) and the page (`/app/<org>/<db>/playgrounds/<id>`,
with **Playground** in the draft editor's bar and on version pages, and a list
in the History tab). Where it differs from the text above, and why:

- **Files** are touched only by `play-files` (deploy/playground) in short-lived
  helper containers, never by the runner itself: one code path for the host
  directory (a playground host) and for a named volume (local development,
  where Docker may run in a VM).
- **Local development** runs playgrounds with `runc` and reaches Postgres by
  TCP on loopback (`ROMULUS_PLAY_TCP`, which the runner refuses with gVisor):
  a socket made in Docker's VM cannot be reached from macOS. A playground host
  uses `runsc-uds` and the socket, as section 4 says.
- **Policy roles** become `PUBLIC`, as in compiles (`romulus_sandbox::playground`):
  only the three roles exist. Real `NOLOGIN` roles come with `SET ROLE` (phase 2).
- **Migration runs**: each statement in its own transaction, `CONCURRENTLY`
  outside one with its documented lock; transaction control in a written
  migration is skipped and said so. Per-tool rules stay phase 2. Offending rows
  are sampled for NOT NULL and unique violations.
- **The generator** is `romulus-fakedata`, behind a seam of its shape
  (`server::seed::Seeder`: its steps, `COPY` into staging tables, SQL, the
  outcome query fed back into its report).
- **Postgres 17 only**: the playground image is 17; a database targeting
  another major (docs/MVP.md, "Several Postgres majors") gets no playground
  for now (422 `major_unsupported`), since its migration's locks and
  rewrites would be 17's. An image per major is a follow-up.
- **Status** shown live: `GET` a playground asks the service whether it runs;
  a build's progress is polled (the API keeps it in memory), not streamed.
- **After review (PLAY-RUN-fix):** caps on the host (30 live), per person (3,
  across organizations) and on free disk, counted again under an advisory
  lock when inserting; console runs rate limited per person (2 in flight) and
  only runs that ran something kept (500 per playground); idleness from the
  last statement, open tabs or not (a stopped tab's next run starts it); a
  busy playground refuses a reset, a migration run or a stop at once (409),
  and delete cancels a build or run in progress; a migration run has 500
  statements and 10 minutes at most, one per person at a time. "Read-only"
  means no data writes: a role changing its own password or settings
  (Postgres allows it) is undone by a restart, which puts all three roles
  back on every start.

**Test migration (task MTEST, 2026-10-19).** One click runs a migration up
and down on a playground of its own, filled with generated rows, and says
whether everything is alright. Decisions:

- **One engine, `crates/romulus-migration-check`**, for the feature and the
  migration oracle (`romulus-migration/tests/oracle`, its plain tool runs):
  the two cannot disagree on what "the migration works" means. Given a
  Postgres session (a small trait: run with a limit, `COPY` in; sqlx's
  connection behind a feature, a playground's in `romulus-playground`), the
  base and target models, up and down (down optional) and where the rows
  come from (romulus-fakedata, provided SQL, or already there), it runs:
  seed → the database is the base model exactly, every table counted, a
  sample of rows by primary key (the same rows each run: lowest `md5` of the
  key) → up, each statement measured (duration; locks from `pg_locks` before
  its `COMMIT`, exact, documented for `CONCURRENTLY`; rewrites; rows
  changed; the offending rows of a failure) → the schema is the target model
  exactly, counts kept, sampled rows found again by key with their values →
  down, measured → the schema is the base model again, counts and every
  sampled value back as they were → the report.
- **Renames and conversions are followed from the statements' own parse
  trees**, never from a guess: every `ALTER … RENAME`, `SET SCHEMA`, `DROP`,
  `ALTER COLUMN … TYPE … USING`, `UPDATE`, `DELETE`, `TRUNCATE`, `INSERT`,
  `ALTER TYPE … RENAME VALUE` is read (`effects::Walk`), and each table and
  column keeps its identity through them. A converted column must hold
  what the migration's own cast gives (its `USING`, evaluated by Postgres on
  the old values, typed as they were), and what does not cast back to the
  old type is named as rounded or cut; a type renamed, or dropped and
  recreated, is followed to its name at that point (a cast to a name the
  walk cannot resolve fails the comparison unless its statement is
  flagged). `DROP TYPE`/`DROP DOMAIN … CASCADE` explains the columns of
  that type only. Partitions are their parent's family: what a statement
  does to the parent (without `ONLY`) it does to them, and rows moving
  with a partition detached or attached are moved, not lost. `DO`, `CALL`
  and `MERGE` cannot be read: what they may explain is "not verifiable"
  and needs acknowledging (below).
- **Data loss is matched against the flags.** A value not back after the
  down migration (or, with no down, lost by the up one: a dropped column, a
  rounding conversion, deleted rows) is data loss, explained by the
  statement that dropped, converted, deleted or changed it. Expected when
  that statement is flagged `deletes_data` (a hand-written one: by what it
  does); a loss no statement explains, or one explained by a planned
  statement without the flag, **fails the test**: the migration did
  something it did not say. A hand-written statement that loses data
  without saying so (a conversion that rounds, a cascade taking columns) or
  an unreadable one is neither: the verdict is **needs acknowledgement**,
  the headline says the migration does not declare it, and a developer
  acknowledges it in the report (audited with the test,
  `migration_test_acknowledged`; where the organization requires passing
  tests, someone other than the draft's author and editors, or an admin)
  or changes the migration. Rows found after up that no statement adds fail
  too; values that do not cast back to compare are "not compared", a
  warning; renamed enum labels are mapped in arrays as in scalars. The
  oracle running on rows found two such hazards on its first run
  (conversions to a recreated domain that refuses NULLs or lost labels:
  now `can_fail`).
- **Nothing passes unverified.** A table without a primary key is checked
  by per-column aggregates (count, and a hash of the sorted values): a
  column the migration keeps must be the same; one it converts must be the
  same, or come back the same cast back to its old type (what does not
  was rounded or cut: data loss, failing when the statement is planned
  without its flag); one whose enum labels were renamed must map back.
  What cannot be compared that way (an `UPDATE`, code it cannot read, a
  cast back that fails) is "not verified": a warning when its statement
  says it changes the data, else for a person to acknowledge, never a
  clean pass. A table too large to read as a whole within a statement's
  limit is "not verified (too large)": a warning, or to acknowledge when
  the migration changes it. A sampled table losing its key column falls back to
  another unique key of NOT NULL columns, else still records the losses of
  dropped and converted columns. Sequences must not go backwards: their
  next value before up is compared with theirs after down. Every table the
  migration touches is listed: those that held no rows are named in the
  report, and a run where every table it changes (something rows matter
  to: data changed or lost, a constraint validated, `NOT NULL` set, a
  unique or expression index) was empty says "nothing to check on" (it
  never satisfies the publish gate); renames, nullable columns and plain
  indexes need no rows. The report also says what the
  checks never compare: grants and owners, whether materialized views hold
  rows, triggers' and functions' bodies, policies' roles (`PUBLIC` here).
- **Verdicts**: `passed` (up and down, everything back or lost as
  declared), `passed_up_only` (no down migration ran: never enough for the
  publish gate), `needs_acknowledgement`, `failed`. An acknowledged test
  counts as passed and keeps its verdict and who acknowledged it; an
  up-only one becomes `passed_up_only`, never enough for the gate.
- **The playground**: short-lived, built for the test like any (the base
  version's schema, rows of the test's size, profile and seed), deleted
  after its report; it holds a place among the host's live playgrounds and
  its disk quota while it lives (not among the organization's playgrounds:
  it goes within minutes). Tests take turns (two at a time, one per
  organization), their place in line shown; a host whose running
  playgrounds are all in use is asked again for up to 10 minutes. The whole
  test has the migration run's budget (10 min; each statement the develop
  deadline), Cancel stops it and deletes its playground. Policies are
  `PUBLIC` in a playground: up and down statements' policy roles become
  `PUBLIC` as the schema's did (`romulus_sandbox::playground::migration`),
  and the schema checks do not compare roles.
- **What is tested**: a draft as reviewed (the migration tab's `sha256`),
  in schema mode up and down as planned, in migration mode up as written
  (a down is generated only when asked for); two versions, both ways as
  the diff page plans them; a version's own written or imported migration,
  from the live version before it (up only unless a down is asked for).
- **Reports are kept** (`migration_tests`), seen by the whole organization,
  listed per draft, pair of versions or version; developers and admins run
  them (as Run migration), reviewers read them. A test left running by a
  restart ends as an error, its playground swept.
- **Run migration** uses the same runner (`statements::run`): one way to
  measure a statement.
- **On someone's playground's rows** (phase 2): a test may run on a copy of
  a playground holding the base version as built (none holding migrated
  rows, none of another version: 409 `playground_migrated`,
  `playground_other_version`, "test on fresh generated data instead"). The
  runner's fixed `fork` operation creates the test's own playground (its
  container, directories, quota) and copies the source's data directory
  into it (`cp --reflink=auto`, the source stopped cleanly for the copy, a
  couple of seconds; its consoles hear pause and resume, as with a reset);
  the copy starts with its own passwords (the entrypoint puts them back on
  every start). Only a playground of the test's own organization is
  copied (the service refuses another's), at most one copy per source a
  minute, the source checked again (version, migrated rows) just before
  the copy; its page tells the owner a test copied it. The engine checks the copy is at the base model before
  anything runs (`Seed::Existing`); the report names whose rows they were,
  and says playground rows are not production data (romulus never connects
  to anyone's database). The source is never changed; the copy is deleted
  after the report, counted in the caps meanwhile. In the app, "Test on
  data" runs on a copy of the person's own playground at the base version
  when there is one (the draft's first), its menu choosing another's or
  fresh generated rows.
- **The migration tested is the one shown**: the renames the migration
  view applies (a draft's known ones and decisions, the versions' stored
  ones, those switched on screen) and its `concurrently`; the down one
  reverses the up one's renames. A draft's test is of its text as it is
  (its input hash), so the publish dialog says how that text tested.
- **Publishing only after a passing test** is an organization setting
  (off by default, admins, audited). A draft that changes the schema then
  publishes only once a test of **exactly the migration publishing would
  run** passed (409 `passing_test_required`): the gate key is the SHA-256
  of the base version's id, the effective renames and the planned up and
  down SQL (`plan::gate_key`; `CONCURRENTLY` as the test ran it, the gate
  accepting either form), so renames switched since the test, or an
  Update draft onto another base with the text unchanged, need a new test.
  It counts only a test whose down ran (acknowledged or not; in migration mode, a draft
  written without a down needs one generated: the publish dialog says so),
  checked something on rows, and passed or had its loss acknowledged. A
  draft with nothing to migrate (no base, nothing planned; migration mode:
  emptiness from the written statements, so a data-only migration is
  tested) is not held. Where tests cannot run (no playgrounds, a database
  on another Postgres major) it answers 409 `tests_unavailable`; an admin
  may publish anyway, saying why (`test_override_reason`, audited as
  `passing_test_overridden` with the version). Never pushes or imports.

**Phase 2, a developer's tool.**
- A writing console for developers (`play_owner`).
- The visual plan.
- Per-tool migration runs (transaction rules, `CONCURRENTLY`).
- Production row counts and estimates.
- ~~"Tried on data" in the publish dialog~~ (built with Test migration).
- Saved queries.
- `SET ROLE` to policy roles, for RLS.
- Fork for me.

**Phase 3, realism and reach.**
- LLM vocabularies (opt-in).
- Per-column overrides.
- Sample rows previewed in the browser (wasm).
- Large on a bigger host.
- CSV export.
- Index suggestions from plans.
- The database's own Postgres major, once the sandbox has several.

**Phase 4, scale.**
- Several hosts with placement.
- A warm pool of initialised clusters.
- Generated copies shared between identical builds.
- KVM hosts with microVMs.
- Dedicated hosts on request.

## 6. Open questions (recommended answer first)

1. **Isolation.** *Recommended:* one cluster per playground in a gVisor
   container with no network, confirmed by phase 0; hardened runc as the
   fallback. A shared cluster is refused whatever the spike shows.
2. **Where it runs.** *Recommended:* a host of its own with an XFS disk,
   apart from the app, reached over a private link. Running playgrounds on
   the app's host puts an escape next to the app's database and secrets;
   there, gVisor, no egress, no superuser, per-playground caps and the
   separation from the app's containers, volumes and secrets are mandatory.
3. **Shared or per person.** *Recommended:* shared per draft or version,
   with sessions per person and announced resets; "fork for me" in phase 2.
4. **Console rights in the MVP.** *Recommended:* read-only for everyone,
   migration runs for developers; a writing console for developers in phase 2.
5. **Who creates playgrounds.** *Recommended:* developers and admins (they
   cost capacity, like databases do). Reviewers open and query existing ones.
6. **Lifetimes and caps.** *Recommended:* stop after 15 minutes idle; delete
   after 7 days unused or with the draft; 2 running and 5 existing per
   organization; Large off until capacity is measured.
7. **LLM-assisted values.** *Recommended:* phase 3, off by default, enabled
   per organization by an admin, schema only, cached. Default model
   `claude-opus-5-5` (~$0.35 for 30 tables, ~$3.40 for 300, once). The
   cheaper models in the table are a setting if you prefer.
8. **Production row counts.** *Recommended:* typed per table in phase 2.
   Later, ask remus for `reltuples`: counts only, never `pg_stats` values,
   which hold real data. Discuss with remus first.
9. **Real data (masked copies of production).** *Recommended:* never in this
   design. It changes the threat model and brings customers' data-protection
   duties into romulus. Synthetic only.
10. **Postgres major.** *Recommended:* 17, the sandbox's, until "several
    majors per database" lands; then the database's own.
11. **Generator.** *Recommended:* our own crate on `fake`, with Postgres
    judging CHECKs and uniques, rather than Snaplet seed as a Node sidecar.
