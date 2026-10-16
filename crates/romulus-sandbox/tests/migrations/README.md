# Migration fixtures

One project's history (organizations, users, projects, tasks renamed to
issues, a seed, a backfill, an `updated_at` trigger, an enum gaining a value,
partitioned events, `CREATE INDEX CONCURRENTLY`, a view with RLS, a column
replaced) in every format the import reads. `tests/sandbox/fixtures.rs`
imports each one and checks the versions, their messages, what was skipped,
and that the last model equals the whole history replayed as one text.

- `sqlx/`, `golang_migrate/`, `dbmate/`, `node_pg_migrate/`, `flyway/`,
  `plain/`: the same twelve migrations, laid out and wrapped as each tool
  expects (sections, down files, `-- no-transaction`, Flyway's repeatable,
  undo and `.conf` files). They must all end on the same model.
- `prisma/`: Prisma's own generated style (quoted PascalCase, `-- CreateTable`
  comments, warnings blocks) with the hand edits teams make (a seed, a
  rename instead of drop and add, a trigger, partitions, CONCURRENTLY).
- `alembic/alembic.sql`, `django/django.sql`, `rails/rails.sql`: the real
  output of `scripts/export-migrations/{alembic,django,rails}.sh` for the
  history written as an Alembic project (Alembic 1.20), a Django app
  (Django 6.1, with Django's own apps) and Rails migrations (Active Record
  8.1). Regenerate them with those scripts rather than editing them.
