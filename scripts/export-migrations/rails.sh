#!/usr/bin/env bash
# Print a Rails project's migrations as the SQL romulus imports.
#
# Run it from the Rails app's root against an EMPTY scratch database (never
# production: the migrations are applied to it), named by DATABASE_URL
# (required: the script refuses to fall back to database.yml, and checks
# Active Record connected to that database):
#
#     createdb romulus_export
#     DATABASE_URL=postgres://localhost/romulus_export \
#       scripts/export-migrations/rails.sh > rails.sql
#
# For each migration in db/migrate, oldest first, it runs the migration on
# the scratch database and prints `-- <version>_<name>.rb` and the SQL
# Active Record sent for it (its own bookkeeping left out: schema queries,
# transactions, and its exact statements on schema_migrations and
# ar_internal_metadata and advisory locks).
# Statements your migrations run through models (`User.create!`) are
# printed too; romulus reports them as data and does not replay them.
# Import the file in romulus (database page, "Import migrations"), or from
# CI:
#
#     curl -sSN --fail-with-body -H "Authorization: Bearer $ROMULUS_TOKEN" \
#       -H "Content-Type: application/sql" -H "Accept: application/x-ndjson" \
#       --data-binary @rails.sql "$ROMULUS_URL/api/databases/<id>/imports?format=rails" \
#       | tee /dev/stderr | tail -n 1 | grep -q '"event":"done"'
#
# (The progress streams, one event per line, so no proxy times out on a
# long import; the last line is `done`, or `failed` with the error.)
#
# Works with Rails 6.0 and later (PostgreSQL adapter). RAILS runs the
# runner (default: bin/rails). Refuses RAILS_ENV=production.
set -euo pipefail

[ -n "${DATABASE_URL:-}" ] || {
    echo "rails.sh: set DATABASE_URL to an empty scratch database (see --help)" >&2
    exit 2
}
case "${1:-}" in
    -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
    "") ;;
    *) echo "rails.sh: unknown option $1 (see --help)" >&2; exit 2 ;;
esac
case "${RAILS_ENV:-}${RACK_ENV:-}" in
    *production*) echo "rails.sh: refusing to run with RAILS_ENV=production" >&2; exit 2 ;;
esac

rails=${RAILS:-bin/rails}
[ -x "$rails" ] || command -v "$rails" >/dev/null || {
    echo "rails.sh: no $rails here: run it from the app's root (or set RAILS)" >&2
    exit 2
}

script=$(mktemp "${TMPDIR:-/tmp}/romulus-rails-export.XXXXXX")
trap 'rm -f "$script"' EXIT
cat > "$script" <<'RUBY'
# Only the SQL goes to stdout: Active Record's own progress lines
# (`== CreateUsers: migrating ==`) are turned off.
ActiveRecord::Migration.verbose = false
pool = ActiveRecord::Base.connection_pool
context =
  if pool.respond_to?(:migration_context)
    pool.migration_context
  else
    ActiveRecord::Base.connection.migration_context
  end

abort "rails.sh: refusing to run in production" if defined?(Rails) && Rails.env.production?
# The database Active Record connected to must be DATABASE_URL's, not one
# database.yml chose.
require "uri"
wanted = URI(ENV.fetch("DATABASE_URL")).path.delete_prefix("/")
connected = ActiveRecord::Base.connection.current_database
unless connected == wanted
  abort "rails.sh: Active Record connected to #{connected.inspect}, not DATABASE_URL's #{wanted.inspect}"
end
$stderr.puts "rails.sh: exporting through database #{connected.inspect}"

unless context.get_all_versions.empty?
  abort "rails.sh: the database already has migrations applied: point it at an empty scratch database"
end
tables = ActiveRecord::Base.connection.tables - %w[schema_migrations ar_internal_metadata]
unless tables.empty?
  abort "rails.sh: the database #{connected.inspect} is not empty (#{tables.size} #{tables.size == 1 ? "table" : "tables"}): point DATABASE_URL at an empty scratch database"
end

# Active Record's own statements, exactly: anything else that mentions its
# tables is the migration's.
TABLE = /"?(?:schema_migrations|ar_internal_metadata)"?/
BOOKKEEPING = [
  /\ACREATE TABLE (?:IF NOT EXISTS )?#{TABLE} \(/,
  /\A(?:INSERT INTO|DELETE FROM|UPDATE) #{TABLE}[\s(]/,
  /\ASELECT [^;]* FROM #{TABLE}(?:\s|\z)/,
  /\ASELECT pg_(?:try_)?advisory_(?:un)?lock\(\d+\)\z/,
].freeze
CONTROL = /\A\s*(BEGIN|COMMIT|ROLLBACK|SAVEPOINT|RELEASE)\b/i

context.migrations.each do |migration|
  statements = []
  subscriber = ActiveSupport::Notifications.subscribe("sql.active_record") do |*, payload|
    sql = payload[:sql].to_s
    next if %w[SCHEMA TRANSACTION].include?(payload[:name])
    sql = sql.strip.sub(/;\s*\z/, "")
    next if sql.match?(CONTROL) || BOOKKEEPING.any? { |pattern| sql.match?(pattern) }
    statements << sql
  end
  begin
    context.run(:up, migration.version)
  ensure
    ActiveSupport::Notifications.unsubscribe(subscriber)
  end
  $stdout.puts "-- #{File.basename(migration.filename)}"
  statements.each { |sql| $stdout.puts "#{sql};" }
end
RUBY
exec "$rails" runner "$script"
