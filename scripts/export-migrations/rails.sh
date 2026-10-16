#!/usr/bin/env bash
# Print a Rails project's migrations as the SQL romulus imports.
#
# Run it from the Rails app's root against an EMPTY scratch database (never
# production: the migrations are applied to it), for example:
#
#     RAILS_ENV=development DATABASE_URL=postgres://localhost/romulus_export \
#       scripts/export-migrations/rails.sh > rails.sql
#
# For each migration in db/migrate, oldest first, it runs the migration on
# the scratch database and prints `-- <version>_<name>.rb` and the SQL
# Active Record sent for it (its own bookkeeping left out: schema queries,
# transactions, schema_migrations, ar_internal_metadata, advisory locks).
# Statements your migrations run through models (`User.create!`) are
# printed too; romulus reports them as data and does not replay them.
# Import the file in romulus (database page, "Import migrations"), or from
# CI:
#
#     curl -fsS -H "Authorization: Bearer $ROMULUS_TOKEN" \
#       -H "Content-Type: application/sql" --data-binary @rails.sql \
#       "$ROMULUS_URL/api/databases/<id>/imports?format=rails"
#
# Works with Rails 6.0 and later (PostgreSQL adapter). RAILS runs the
# runner (default: bin/rails).
set -euo pipefail

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

unless context.get_all_versions.empty?
  abort "rails.sh: the database already has migrations applied: point it at an empty scratch database"
end

BOOKKEEPING = /\b(schema_migrations|ar_internal_metadata)\b|pg_(try_)?advisory_(un)?lock/
CONTROL = /\A\s*(BEGIN|COMMIT|ROLLBACK|SAVEPOINT|RELEASE)\b/i

context.migrations.each do |migration|
  statements = []
  subscriber = ActiveSupport::Notifications.subscribe("sql.active_record") do |*, payload|
    sql = payload[:sql].to_s
    next if %w[SCHEMA TRANSACTION].include?(payload[:name])
    next if sql.match?(CONTROL) || sql.match?(BOOKKEEPING)
    statements << sql.strip.sub(/;\s*\z/, "")
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
