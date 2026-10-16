# The sandbox Postgres

Where `crates/romulus-sandbox` compiles draft SQL: PostgreSQL 17 (the
grammar `pg_query` parses), PostGIS and pgvector, run by `docker-compose.yml`
(`just sandbox`) and by CI. It holds nothing but empty pool databases;
every compile is rolled back. This page is what an operator needs to run it
safely; the crate's `allowlist` and `compiler` modules document the rest of
the threat model.

## Memory: one bad compile must not take the sandbox down

Postgres has no per-backend memory limit, and some allowed DDL makes it
evaluate expressions while compiling (constant folding). Layers, outermost
last:

1. **The allowlist** refuses calls that could build huge values from small
   input: functions with only constant arguments must be on a short
   allowlist wherever Postgres may fold them, and XML constants are refused.
2. **Role settings** for every compile role (set at bootstrap, not
   changeable by the SQL): `work_mem` 4 MB, `maintenance_work_mem` 16 MB,
   `hash_mem_multiplier` 1, `temp_file_limit` 64 MB, no parallel workers,
   no JIT, `statement_timeout`, `lock_timeout`.
3. **A 128 MB cap on each Postgres process's private memory**
   (`RLIMIT_DATA`, which Linux ≥ 4.7 applies to `malloc`'s mmaps too;
   shared buffers are shared memory and do not count). The image's
   entrypoint (`romulus-entrypoint.sh`) sets it with `ulimit -d` before
   starting the server, so the postmaster and every backend it forks
   inherit it; `ROMULUS_SANDBOX_MAX_DATA_KB` changes it (default 131072).
   It is not set with `docker run --ulimit data=…`: that also caps the
   container runtime's own init, which then may fail to start the
   container at all (it does on GitHub's runners). An allocation past it
   fails, and Postgres reports an ordinary `out of memory` error for that
   statement: the backend lives on, nothing restarts. Measured: every
   allowlisted extension installs well under it.
4. **The container limit** (768 MB) is sized for the pool: 4 compile
   backends, the admin and the probe connection at 128 MB each. If it is
   reached anyway, the kernel's OOM killer picks the process with the highest
   score: backends run at `oom_score_adj` 1000 (`PG_OOM_ADJUST_VALUE`), the
   postmaster at -1000 (the container's `oom_score_adj`), so a backend dies,
   never the postmaster. Postgres then restarts its backends (about 2 s);
   the compiler retries on fresh connections and takes its prefix lock back
   (`romulus-sandbox` tests: a backend killed mid-compile).

**Locks.** Creating an object locks it until the compile ends, and the lock
table is shared by every session: `max_locks_per_transaction` is 2048 (a
table of about 120 000 locks, some 26 MB of shared memory) so that large
schemas and the catalog probe (every extension at once) fit, and one compile
cannot starve the others; the allowlist refuses creating an extension twice
(each PostGIS install locks thousands of objects). Past the table, compiles
fail with `out of shared memory` until the greedy one ends, which the
compile deadline bounds.

Running it elsewhere (production):

- Keep 3 and 4, and the lock table: this image (its entrypoint sets the
  memory cap), `-c max_locks_per_transaction=2048`, `--oom-score-adj -1000`,
  `PG_OOM_ADJUST_FILE=/proc/self/oom_score_adj`, `PG_OOM_ADJUST_VALUE=1000`,
  and a memory limit of at least (pool size + 2) × 128 MB plus shared
  buffers. Raising the pool means raising the limit.
- On a dedicated VM rather than a container, Postgres's own advice applies:
  `vm.overcommit_memory = 2` so allocations fail instead of the OOM killer
  firing, and the postmaster started with `oom_score_adj` -1000; set the
  per-process cap in its service (systemd `LimitDATA=128M`).
- Give it its own host or VM: a busy sandbox must not starve the app
  database or the API.
- Data: on a volume with `fsync` on (an unclean stop with `fsync` off
  corrupts it; compiles never commit, so it costs nothing). WAL stays small
  (`wal_level=minimal`, `max_wal_size=128MB`); on tmpfs it would count as
  memory.

## Extensions and why they are trusted

Compiles run as unprivileged roles, which may only create *trusted*
extensions; a trusted extension's install script runs as the bootstrap
superuser. `trusted-extensions.txt` marks the allowlist trusted. That is
acceptable because:

- the scripts are fixed by this image (pinned packages), never user-supplied;
- the SQL cannot create functions, operators, casts or temporary objects
  that such a script could pick up from the search path, and every compile
  is rolled back, so nothing from one compile is there for the next. A
  migration import (`Compiler::replay`) does create the functions,
  procedures and triggers its migrations define, but always with a fixed
  stub body of romulus's (`BEGIN RETURN NULL; END`): a script capturing one
  of their calls runs nothing of the import's; operators, casts and
  aggregates are skipped;
- none of them reaches outside the database: the contrib ones are already
  trusted upstream (pgcrypto, uuid-ossp, citext, hstore, pg_trgm, ltree…),
  pgvector is types, functions and index methods, PostGIS and
  postgis_topology are geometry functions and plain SQL/PL/pgSQL tables.

Deliberately not offered: `postgis_raster` (out-of-database rasters read
files through GDAL), `dblink`, `postgres_fdw`, `file_fdw`, `adminpack`,
`pg_stat_statements`, `tablefunc` (runs SQL given as text). The server-side
allowlist refuses them before Postgres is asked.

Bump the image (and the `pg_query` crate) with care: a new extension version
can add functions; the constant-call allowlist only names functions known to
be safe, so new ones are refused where Postgres would fold them.
