#!/usr/bin/env bash
# Print a Django project's migrations as the SQL romulus imports.
#
# Run it next to manage.py, with the project's virtualenv active, against an
# EMPTY scratch database (never production: the migrations are applied to
# it), named explicitly with --database:
#
#     createdb romulus_export
#     DATABASE_URL=postgres://localhost/romulus_export \
#       scripts/export-migrations/django.sh --database default > django.sql
#
# For every migration of `manage.py showmigrations --plan`, in that order,
# it prints `-- <app>.<migration>` and the SQL of `manage.py sqlmigrate`,
# then applies the migration to the scratch database: sqlmigrate reads the
# database to name constraints, so each migration's SQL must be printed in
# the state the ones before it left. RunPython operations run (that is
# your code, on your machine); their SQL is not printed, and romulus does
# not need it. Import the file in romulus (database page, "Import
# migrations"), or from CI:
#
#     curl -sSN --fail-with-body -H "Authorization: Bearer $ROMULUS_TOKEN" \
#       -H "Content-Type: application/sql" -H "Accept: application/x-ndjson" \
#       --data-binary @django.sql "$ROMULUS_URL/api/databases/<id>/imports?format=django" \
#       | tee /dev/stderr | tail -n 1 | grep -q '"event":"done"'
#
# (The progress streams, one event per line, so no proxy times out on a
# long import; the last line is `done`, or `failed` with the error.)
#
# Point the project's settings at the scratch database however it reads
# them (DATABASE_URL above is only an example). Options:
#     --database ALIAS   the settings' alias of the scratch database
#                        (required: there is no default, so that the
#                        development database is never picked by accident)
#     --manage PATH      manage.py (default: ./manage.py)
# PYTHON picks the interpreter (default: python3, the active virtualenv's).
# The database's name and host are printed (to stderr) before anything runs.
set -euo pipefail

die() { echo "django.sh: $*" >&2; exit 2; }

database=
manage=./manage.py
while [ $# -gt 0 ]; do
    case "$1" in
        --database|--manage)
            [ $# -ge 2 ] && [ -n "$2" ] && [ "${2#-}" = "$2" ] || die "$1 needs a value (see --help)"
            if [ "$1" = --database ]; then database=$2; else manage=$2; fi
            shift 2 ;;
        -h|--help) sed -n '2,33p' "$0"; exit 0 ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
done
[ -n "$database" ] || die "name the scratch database's alias with --database (see --help)"
[ -f "$manage" ] || die "no $manage here: run it next to manage.py (or pass --manage)"

# The shell's own banner and imports stay quiet; only the SQL is printed.
ROMULUS_EXPORT_DATABASE=$database exec "${PYTHON:-python3}" "$manage" shell --no-imports -v 0 -c '
import io
import os
import sys

from django.core.management import call_command
from django.db import connections
from django.db.migrations.executor import MigrationExecutor

alias = os.environ["ROMULUS_EXPORT_DATABASE"]
if alias not in connections:
    sys.exit(f"django.sh: no database {alias!r} in the settings")
connection = connections[alias]
db_name = connection.settings_dict.get("NAME")
db_host = connection.settings_dict.get("HOST") or "localhost"
print(f"django.sh: exporting through {alias!r}: database {db_name!r} on {db_host}", file=sys.stderr)
executor = MigrationExecutor(connection)
if executor.loader.applied_migrations:
    sys.exit(
        f"django.sh: the database {alias!r} already has migrations applied: "
        "point it at an empty scratch database"
    )
with connection.cursor() as cursor:
    tables = [t for t in connection.introspection.table_names(cursor) if t != "django_migrations"]
if tables:
    noun = "table" if len(tables) == 1 else "tables"
    sys.exit(
        f"django.sh: the database {db_name!r} is not empty ({len(tables)} {noun}): "
        "point it at an empty scratch database"
    )

graph = executor.loader.graph
plan = []
seen = set()
for leaf in sorted(graph.leaf_nodes()):
    for node in graph.forwards_plan(leaf):
        if node not in seen:
            seen.add(node)
            plan.append(node)

out = sys.stdout
for app, name in plan:
    sql = io.StringIO()
    call_command("sqlmigrate", app, name, database=alias, stdout=sql)
    out.write(f"-- {app}.{name}\n")
    out.write(sql.getvalue().rstrip("\n") + "\n")
    call_command("migrate", app, name, database=alias, verbosity=0, interactive=False)
'
