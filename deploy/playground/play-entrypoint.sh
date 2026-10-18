#!/bin/sh
# A playground's Postgres (docs/playground.md, "Lifecycle", step 1). Started
# by the runner as postgres (999), with:
#   /var/lib/postgresql/data   the data directory (on the playground volume)
#   /var/run/postgresql        the socket directory, which the service mounts too
#   /tmp                       a tmpfs: the root filesystem is read-only
# and, from the runner's own configuration (never from a request):
#   PLAY_OWNER_PASSWORD, PLAY_READ_PASSWORD, PLAY_SERVICE_PASSWORD
#                              HMAC(secret, playground, role): nothing is stored
#   PLAY_EXTENSIONS            "name:schema …", checked against the allowlist by the runner
#   PLAY_MAX_DATA_KB           each process's private memory (RLIMIT_DATA, as in the sandbox)
#   PLAY_TCP=1                 local development only: listen on TCP too (no gVisor there)
#
# On the first start (an empty data directory) it runs initdb and the init
# as a superuser, on a private socket in /tmp that nothing else can reach,
# before the real socket exists. Afterwards the superuser has no pg_hba
# line: only the three roles below can connect, with scram, on the socket.
#
# On every start, before the server listens, single-user mode (no socket at
# all) puts the three roles' passwords and settings back: Postgres lets any
# role change its own password and settings (ALTER ROLE play_read …), which
# would lock the service out or follow everyone into their next session.
# The service restarts a playground when it sees either (host.rs, repair).
set -eu

# Before anything forks, so the postmaster and every backend inherit it.
ulimit -d "${PLAY_MAX_DATA_KB:-262144}"

export PGDATA=/var/lib/postgresql/data
socket_dir=/var/run/postgresql

for var in PLAY_OWNER_PASSWORD PLAY_READ_PASSWORD PLAY_SERVICE_PASSWORD; do
    eval "value=\${$var:-}"
    [ -n "$value" ] || { echo "play-entrypoint: $var is not set" >&2; exit 1; }
    # base64url from the runner: nothing to quote in SQL.
    case "$value" in
        *[!A-Za-z0-9_-]*) echo "play-entrypoint: $var is not base64url" >&2; exit 1 ;;
    esac
done

init() {
    initdb -D "$PGDATA" -U play_init -A trust -E UTF8 --locale=C.UTF-8 \
        --no-instructions --no-sync >/dev/null
    private=$(mktemp -d /tmp/init.XXXXXX)
    pg_ctl -D "$PGDATA" -w -s -l /tmp/init.log \
        -o "-c listen_addresses='' -c unix_socket_directories=$private -c fsync=off -c wal_level=minimal -c max_wal_senders=0" \
        start || { cat /tmp/init.log >&2; exit 1; }
    psql -X -q -v ON_ERROR_STOP=1 -h "$private" -U play_init -d postgres \
        -v owner_password="$PLAY_OWNER_PASSWORD" \
        -v read_password="$PLAY_READ_PASSWORD" \
        -v service_password="$PLAY_SERVICE_PASSWORD" \
        -f /usr/local/share/romulus/play-init.sql
    # The model's extensions, before anyone connects: their scripts run as
    # the superuser here, and never again (nothing is trusted).
    for spec in ${PLAY_EXTENSIONS:-}; do
        psql -X -q -v ON_ERROR_STOP=1 -h "$private" -U play_init -d playground \
            -v ext="${spec%%:*}" -v schema="${spec#*:}" <<'SQL'
SET client_min_messages = warning;
CREATE SCHEMA IF NOT EXISTS :"schema" AUTHORIZATION play_owner;
GRANT USAGE ON SCHEMA :"schema" TO play_read;
CREATE EXTENSION IF NOT EXISTS :"ext" WITH SCHEMA :"schema" CASCADE;
SQL
    done
    pg_ctl -D "$PGDATA" -w -s -m fast stop
    rm -rf "$private" /tmp/init.log
    {
        echo "# romulus playground: these roles only, scram only (play-entrypoint.sh)"
        echo "local playground play_owner,play_read,play_service scram-sha-256"
        if [ "${PLAY_TCP:-}" = 1 ]; then
            echo "host playground play_owner,play_read,play_service 0.0.0.0/0 scram-sha-256"
            echo "host playground play_owner,play_read,play_service ::/0 scram-sha-256"
        fi
    } > "$PGDATA/pg_hba.conf"
    : > "$PGDATA/pg_ident.conf"
}

# The roles as the service expects them (host.rs, ROLE_SETTINGS): passwords,
# the settings below and nothing else, none per database, none on the
# database. Single-user mode: no socket, no other session.
repair() {
    out=$(postgres --single -D "$PGDATA" -c listen_addresses='' -c log_min_messages=warning \
        -c log_min_error_statement=panic postgres 2>&1 <<SQL
ALTER ROLE play_owner PASSWORD '$PLAY_OWNER_PASSWORD'
ALTER ROLE play_read PASSWORD '$PLAY_READ_PASSWORD'
ALTER ROLE play_service PASSWORD '$PLAY_SERVICE_PASSWORD'
ALTER ROLE play_owner RESET ALL
ALTER ROLE play_owner IN DATABASE playground RESET ALL
ALTER ROLE play_read RESET ALL
ALTER ROLE play_read IN DATABASE playground RESET ALL
ALTER ROLE play_service RESET ALL
ALTER ROLE play_service IN DATABASE playground RESET ALL
ALTER DATABASE playground RESET ALL
ALTER ROLE play_owner SET idle_in_transaction_session_timeout = '10min'
ALTER ROLE play_read SET default_transaction_read_only = on
ALTER ROLE play_read SET idle_in_transaction_session_timeout = '10min'
ALTER ROLE play_service SET search_path = pg_catalog
SQL
    ) || { echo "play-entrypoint: the roles could not be put back" >&2; exit 1; }
    case "$out" in
        *ERROR*) echo "play-entrypoint: the roles could not be put back" >&2; exit 1 ;;
    esac
}

if [ ! -s "$PGDATA/PG_VERSION" ]; then
    init
fi
repair
unset PLAY_OWNER_PASSWORD PLAY_READ_PASSWORD PLAY_SERVICE_PASSWORD

listen=""
[ "${PLAY_TCP:-}" = 1 ] && listen="*"

# On the command line, so nothing in the data directory (postgresql.auto.conf)
# overrides them; only a superuser could change the rest, and none can log in.
exec postgres \
    -c listen_addresses="$listen" \
    -c unix_socket_directories="$socket_dir" \
    -c unix_socket_permissions=0777 \
    -c max_connections=20 \
    -c superuser_reserved_connections=0 \
    -c shared_buffers=64MB \
    -c work_mem=8MB \
    -c maintenance_work_mem=64MB \
    -c temp_file_limit=256MB \
    -c wal_level=minimal \
    -c max_wal_senders=0 \
    -c max_wal_size=256MB \
    -c min_wal_size=32MB \
    -c fsync=off \
    -c synchronous_commit=off \
    -c full_page_writes=off \
    -c jit=off \
    -c max_parallel_workers_per_gather=0 \
    -c log_min_error_statement=panic \
    -c log_min_messages=warning \
    -c log_error_verbosity=terse \
    -c log_connections=off \
    -c log_disconnections=off \
    -c log_statement=none \
    -c log_checkpoints=off \
    -c log_lock_waits=off
