# Exporting migrations written in code

romulus imports migrations as SQL and never runs a project's code. Tools whose
migrations are code (Alembic, Django, Rails) print the SQL they run; these
scripts make them print it for every migration, with the headers romulus
splits on. Run them in your project, then import the file they write (the
database page's "Import migrations", or the CI endpoint below).

| Tool | Run it | Needs a database? | Headers romulus reads |
|---|---|---|---|
| Alembic | `alembic.sh > alembic.sql`, next to alembic.ini | No: Alembic's offline mode (`upgrade head --sql`) | `-- Running upgrade <from> -> <to>, <message>` |
| Django | `django.sh > django.sql`, next to manage.py | An **empty scratch** database: each migration is applied after `sqlmigrate` prints it (it reads constraint names from the database) | `-- <app>.<migration>` |
| Rails | `rails.sh > rails.sql`, from the app's root | An **empty scratch** database: each migration runs, the SQL Active Record sent is printed | `-- <version>_<name>.rb` |

`PYTHON` (Alembic, Django) and `RAILS` (Rails) pick the interpreter or the
runner; `--help` prints each script's options. Point the project at the
scratch database however its settings read it (`DATABASE_URL` in the
examples): the scripts refuse a database that is not empty (tables, or migrations applied). Django needs `--database ALIAS` (no default), Rails `DATABASE_URL` (checked against the database Active Record connects to; never production).

What the import does with the output: each section becomes a version; the
tool's bookkeeping (alembic_version, schema_migrations, ar_internal_metadata)
is left out; data statements (a seed, a backfill, Django's `RunPython`) are
skipped and listed per migration.

From CI:

```bash
scripts/export-migrations/alembic.sh > alembic.sql
curl -fsS -H "Authorization: Bearer $ROMULUS_TOKEN" \
  -H "Content-Type: application/sql" --data-binary @alembic.sql \
  "$ROMULUS_URL/api/databases/<id>/imports?format=alembic"
```

Migration folders (sqlx, Flyway, Prisma, dbmate, golang-migrate,
node-pg-migrate, plain `.sql` files) need no exporter: zip the folder and send
it (`Content-Type: application/zip`), or pick it in the web app. Importing
again is safe: migrations already imported are skipped.

The fixtures in `crates/romulus-sandbox/tests/migrations/{alembic,django,rails}`
are these scripts' real output; regenerate them with the scripts.
