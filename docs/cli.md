# Testing migrations from CI (`romulus test`)

`romulus test` runs a pull request's new migrations before they merge, on a
real Postgres holding your database's latest version and generated rows, and
fails the job when one of them fails, locks a table dangerously or loses
data. It is the web app's **Test on data**, from a terminal: the same
playground, the same checks, the same report.

```text
$ romulus test db/migrations --database acme/shop
romulus test: acme/shop, PR #42: customer-emails (3f9c2a1)
2 new migrations (37 imported already), on v38 with 1200 generated rows:

  PASS  db/migrations/20261020_orders.sql (2 statements, 41 ms)
  FAIL  db/migrations/20261021_email.sql: statement 2 of 2 failed
          ALTER TABLE customers ALTER COLUMN email SET NOT NULL;
        ERROR 23502: column "email" of relation "customers" contains null values
        200 rows break it: (id)=(1), (id)=(2), (id)=(3)

FAILED: Failed at statement 3 of 3 of the up migration: column "email" of relation "customers" contains null values
Report: https://romulus.lupidae.com/app/acme/shop/tests/…
Draft:  https://romulus.lupidae.com/app/acme/shop/drafts/… (PR #42: customer-emails)
```

## How it works

1. It reads the migrations folder and sends every text file to romulus,
   which reads them as its migration import does: plain SQL, sqlx, Flyway,
   Prisma, dbmate, golang-migrate, node-pg-migrate, or the SQL that the
   [Rails, Django and Alembic exporters](../scripts/export-migrations/README.md)
   print. The format is detected (or `--format`).
2. romulus matches them with the migrations the database imported: the ones
   it has not imported yet are the new ones. A migration that changed after
   it was imported, or a new one sorted before imported ones, fails the job.
3. The new migrations, one after the other, go into a draft in migration
   mode on the database's latest version, named after the pull request
   (`PR #42: customer-emails`). Every push of the pull request updates the
   same draft, so your team sees the change coming and can comment on it.
4. romulus compiles them on the latest version (a migration that does not
   apply fails here, on its file and line), then tests them on a playground
   of their own, filled with generated rows: each statement is run and
   measured (locks, rewrites, duration), the schema checked, the rows counted
   and sampled before and after.
5. The report is printed, each finding on its file and line, with a link to
   the test's page. On GitHub, findings are also annotated in the pull
   request's diff and the report goes to the job's summary.

Running it again on the same text answers that text's test instead of
running another (`--fresh` runs a new one).

## What fails the job

| Exit | When |
|---|---|
| 0 | The new migrations passed, or no migration of the folder is new. |
| 1 | A check failed: a statement failed on the rows (a `NOT NULL` on a column with nulls, a unique index on duplicates…); the migrations do not apply to the latest version; the schema after them is not what they make; they change data no statement declares; a migration locks dangerously; a migration imported before was changed. |
| 2 | It could not test: a usage error, no token or a refused one, romulus unreachable, the playground could not be built, or `--timeout` ran out. |

**Dangerous locks.** A statement that rewrites a table that exists
(`ALTER COLUMN … TYPE` that converts, a volatile default…) holds an `ACCESS
EXCLUSIVE` lock for the whole copy: every read and write of the table waits.
A `CREATE INDEX` without `CONCURRENTLY` on a table that exists holds a `SHARE`
lock while it builds: every write waits. Both fail the job unless
`--allow-locks` (they are then listed as warnings). Tables a migration creates
itself never count.

**Data loss.** Data a statement says it removes (a `DROP COLUMN`, a `DELETE`,
an `UPDATE`) is a warning. Data that changes without a statement saying so
(a conversion that rounds, a `DROP TYPE … CASCADE` taking a column, a `DO`
block) fails the job until someone with the developer role acknowledges it on
the test's page; run the job again then, and the acknowledged test is the
answer.

## Before you start

- **The database's history comes from your migrations.** romulus tells new
  migrations from old ones by the ones it imported. Import the folder once
  (the database page, **Import migrations**), then keep importing on your
  main branch as migrations merge (below). A database whose versions come
  only from SQL uploads or schema pushes answers `history_not_imported`.
- **A personal access token with write access**, of someone with the
  developer role (or admin) in the organization: your account menu,
  **Access tokens**. Store it as a CI secret, `ROMULUS_TOKEN`. A token of a
  dedicated account for CI keeps the drafts it writes apart from yours.
  (The database API token `rom_…` that imports from CI is another thing: it
  cannot test.)
- **Playgrounds.** Tests run on playgrounds: the hosted service has them; a
  self-hosted install needs them set up ([self-host/playgrounds.md](self-host/playgrounds.md)).
  They run Postgres 17.

## GitHub Actions

```yaml
# .github/workflows/migrations.yml
name: Migrations
on:
  pull_request:
    paths: ["db/migrations/**"]
  push:
    branches: [main]
    paths: ["db/migrations/**"]

jobs:
  # Every pull request: its new migrations, tested on rows.
  test:
    if: github.event_name == 'pull_request'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: lupidae/romulus@v0.3.0   # a release: the CLI of that release
        with:
          migrations: db/migrations
          database: acme/shop
          token: ${{ secrets.ROMULUS_TOKEN }}
          # url: https://romulus.example.com   # a self-hosted romulus
          # args: --allow-locks --timeout 20m

  # Main: the merged migrations become versions (the database page,
  # Import migrations, From CI, gives this command with the database's id).
  import:
    if: github.event_name == 'push'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - name: Import into romulus
        env:
          ROMULUS_TOKEN: ${{ secrets.ROMULUS_IMPORT_TOKEN }}   # a database API token, rom_…
        run: |
          cd db/migrations && zip -qr - . | curl -sSN --fail-with-body \
            -H "Authorization: Bearer $ROMULUS_TOKEN" -H "Content-Type: application/zip" \
            -H "Accept: application/x-ndjson" --data-binary @- \
            https://romulus.lupidae.com/api/databases/<database id>/imports \
            | tee /dev/stderr | tail -n 1 | grep -q '"event":"done"'
```

The action downloads the CLI of the release it is pinned to (checked against
the release's `SHA256SUMS`), on Linux (x64, arm64) or macOS (arm64) runners.
Its outputs: `result` (`passed`, `nothing_new`, `failed`, `error`,
`timeout`) and `report` (the test's page).

Pull requests from forks get no secrets: the job then stops with exit 2,
"ROMULUS_TOKEN is not set".

## Any other CI

Download the binary for your platform from the
[releases](https://github.com/lupidae/romulus/releases) (`romulus-<target>.tar.gz`:
`x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`, static, for
any Linux; `aarch64-apple-darwin`), check it against `SHA256SUMS`, and run
it. GitLab CI, for instance:

```yaml
test-migrations:
  image: alpine:3
  rules:
    - if: $CI_PIPELINE_SOURCE == "merge_request_event"
  script:
    - apk add --no-cache curl
    - curl -fsSL https://github.com/lupidae/romulus/releases/download/vX.Y.Z/romulus-x86_64-unknown-linux-musl.tar.gz | tar -xz
    - ./romulus test db/migrations --database acme/shop   # ROMULUS_TOKEN: a masked CI variable
```

Or build it: `cargo install --locked --git https://github.com/lupidae/romulus romulus-cli`.

## Options

```text
romulus test <migrations-dir> --database <org>/<db> [options]
```

| Option | |
|---|---|
| `-d`, `--database <org>/<db>` | The database: the organization's slug and the database's, as in the app's address. Or `ROMULUS_DATABASE`. |
| `--format <tool>` | `auto` (default), `plain`, `sqlx`, `flyway`, `prisma`, `dbmate`, `golang_migrate`, `node_pg_migrate`, `alembic`, `django`, `rails`. |
| `--label <text>` | The draft's name. By default the CI's: `PR #42: <branch>` (GitHub Actions, GitLab CI), `Branch <name>` on a push. A later run with the same label updates the same draft (yours, in migration mode); if a version was published since, a new draft replaces it. |
| `--size <small\|medium>` | How many generated rows (default `small`). |
| `--down <none\|generate>` | Also run a down migration generated from the change (default `none`: the migrations' own down files are not run). |
| `--allow-locks` | Table rewrites and index builds that block writes do not fail the job (still reported). |
| `--fresh` | Test again even when this exact text was tested. |
| `--timeout <duration>` | Give up after `90s`, `15m` (default), `1h`: exit 2; the test goes on in romulus. |
| `--json` | Print the outcome as JSON (below) instead of text. |
| `--url <url>` | romulus's address. Or `ROMULUS_URL`; by default `https://romulus.lupidae.com`. |

The token is read from `ROMULUS_TOKEN` only, never from an argument (a
process list would show it). There are no prompts.

## JSON

`--json` prints one object: `result` (`passed`, `nothing_new`, `failed`,
`error`, `timeout`) and `exit_code`; `database`, `label`, `commit`;
`imported` (migrations the database had already) and `base_version`;
`migrations` (each with `path`, `status`: `passed`, `failed`, `not_run`,
`invalid`, `untested`, `statements`, `duration_ms`, and its `failure`: the
statement, Postgres's error, the rows that break it); `findings` (each with
`kind`, `fails`, `path`, `line`, `message`; kinds `statement_failed`,
`does_not_apply`, `does_not_parse`, `cannot_import`, `rewrite`,
`blocking_index`, `undeclared_data_loss`, `data_loss`, `check_failed`,
`not_checked`); `verdict` and `headline` (the test's); `test_id`,
`test_page`, `draft`; and `error` (`code`, `message`) when it could not test.

## How it talks to romulus

The CLI is a client of romulus's MCP endpoint (`/api/mcp`, the one AI agents
use, [mcp.md](mcp.md)) with your token: `import_migrations` (the files, as an
upload), `test_migration` with that upload, and `get_migration_test` until the
verdict. It asks those tools for their data as JSON (`_meta`
`romulus/data`). Everything it does is in the database's Activity, as your
token's.
