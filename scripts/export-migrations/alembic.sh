#!/usr/bin/env bash
# Print an Alembic project's migrations as the SQL romulus imports.
#
# Run it where `alembic` works (the folder holding alembic.ini), with the
# project's virtualenv active:
#
#     scripts/export-migrations/alembic.sh > alembic.sql            # base to head
#     scripts/export-migrations/alembic.sh --config db/alembic.ini > alembic.sql
#
# Nothing touches a database: this is Alembic's offline mode (`alembic upgrade
# head --sql`), whose output has one `-- Running upgrade <from> -> <to>`
# section per revision. Each header gets the revision's message appended
# (`-- Running upgrade <from> -> <to>, add issue numbers`), which romulus uses
# in the version's message. Import the file in romulus (database page, "Import
# migrations"), or from CI:
#
#     curl -sSN --fail-with-body -H "Authorization: Bearer $ROMULUS_TOKEN" \
#       -H "Content-Type: application/sql" -H "Accept: application/x-ndjson" \
#       --data-binary @alembic.sql "$ROMULUS_URL/api/databases/<id>/imports?format=alembic" \
#       | tee /dev/stderr | tail -n 1 | grep -q '"event":"done"'
#
# (The progress streams, one event per line, so no proxy times out on a
# long import; the last line is `done`, or `failed` with the error.)
#
# Your env.py must support offline mode (`run_migrations_offline`, which the
# default template has). Revisions that branch and merge are printed in the
# order Alembic applies them.
#
# PYTHON picks the interpreter (default: python3, the active virtualenv's).
set -euo pipefail
exec "${PYTHON:-python3}" - "$@" <<'PY'
import argparse
import io
import re
import sys

# The project folder may hold a package named like a tool (an `alembic/`
# folder): never import from the current directory by accident. Alembic
# adds the project's own paths (`prepend_sys_path` in alembic.ini).
sys.path = [p for p in sys.path if p not in ("", ".")]

try:
    from alembic import command
    from alembic.config import Config
    from alembic.script import ScriptDirectory
except ImportError:
    sys.exit("alembic.sh: Alembic is not installed here; activate the project's virtualenv first.")

HEADER = re.compile(r"^-- Running upgrade (?P<from>.*?) -> (?P<to>\S+)\s*$")


def main() -> int:
    parser = argparse.ArgumentParser(description="Print an Alembic project's migrations as the SQL romulus imports.")
    parser.add_argument("--config", default="alembic.ini", help="path to alembic.ini")
    parser.add_argument("--revision", default="head", help="the last revision to export (default: head)")
    args = parser.parse_args()

    buffer = io.StringIO()
    config = Config(args.config, output_buffer=buffer)
    script = ScriptDirectory.from_config(config)
    messages = {}
    for revision in script.walk_revisions():
        doc = (revision.doc or "").strip().splitlines()
        messages[revision.revision] = doc[0].strip() if doc else ""

    command.upgrade(config, args.revision, sql=True)

    for line in buffer.getvalue().splitlines():
        match = HEADER.match(line)
        if match and messages.get(match.group("to")):
            line = f"{line.rstrip()}, {messages[match.group('to')]}"
        sys.stdout.write(line + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
PY
