#!/usr/bin/env bash
# Print a Django project's migrations as the SQL romulus imports.
#
# Run it next to manage.py, with the project's virtualenv active, against an
# EMPTY scratch database (never production: the migrations are applied to it):
#
#     createdb romulus_export
#     DATABASE_URL=postgres://localhost/romulus_export \
#       scripts/export-migrations/django.sh > django.sql
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
#     curl -fsS -H "Authorization: Bearer $ROMULUS_TOKEN" \
#       -H "Content-Type: application/sql" --data-binary @django.sql \
#       "$ROMULUS_URL/api/databases/<id>/imports?format=django"
#
# Point the project's settings at the scratch database however it reads
# them (DATABASE_URL above is only an example). Options:
#     --database ALIAS   the settings' database alias (default: default)
#     --manage PATH      manage.py (default: ./manage.py)
# PYTHON picks the interpreter (default: python3, the active virtualenv's).
set -euo pipefail

database=default
manage=./manage.py
while [ $# -gt 0 ]; do
    case "$1" in
        --database) database=$2; shift 2 ;;
        --manage) manage=$2; shift 2 ;;
        -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
        *) echo "django.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
    esac
done
[ -f "$manage" ] || { echo "django.sh: no $manage here: run it next to manage.py (or pass --manage)" >&2; exit 2; }

# The shell's own banner and imports stay quiet; only the SQL is printed.
ROMULUS_EXPORT_DATABASE=$database exec "${PYTHON:-python3}" "$manage" shell --no-imports -v 0 -c '
import io
import os
import sys

from django.core.management import call_command
from django.db import connections
from django.db.migrations.executor import MigrationExecutor

alias = os.environ["ROMULUS_EXPORT_DATABASE"]
connection = connections[alias]
executor = MigrationExecutor(connection)
if executor.loader.applied_migrations:
    sys.exit(
        f"django.sh: the database {alias!r} already has migrations applied: "
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
