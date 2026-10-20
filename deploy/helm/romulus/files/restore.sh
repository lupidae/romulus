#!/usr/bin/env bash
# The app database becomes a dump, in the bundled Postgres's pod
# (docs/self-host/kubernetes.md, "Restore"). Stop the API first:
#
#   kubectl -n NS scale deploy/RELEASE-api --replicas=0
#   kubectl -n NS exec RELEASE-postgresql-0 -- bash /scripts/restore.sh /backups/romulus-….dump
#   kubectl -n NS exec -i RELEASE-postgresql-0 -- bash /scripts/restore.sh - < romulus-….dump
#   kubectl -n NS scale deploy/RELEASE-api --replicas=1
#
# As install.sh restore does: the dump is checked to read, the current
# database is dumped first (a `pre-restore` dump in /backups), the dump is
# restored into a new database, then swapped in by renaming, so a failure
# before the swap changes nothing. The replaced database stays as
# romulus_before_restore (the previous one of that name is dropped).
set -uo pipefail

log() { echo "restore: $*" >&2; }
die() {
    log "$*"
    exit 1
}
q() { PGOPTIONS='-c client_min_messages=warning' psql -X -q -v ON_ERROR_STOP=1 -U romulus -d postgres "$@"; }

[ $# -eq 1 ] || die "usage: restore.sh FILE.dump (or - to read it from stdin)"
dump=$1
if [ "$dump" = - ]; then
    dump=/tmp/romulus-restore.dump
    trap 'rm -f /tmp/romulus-restore.dump' EXIT
    (umask 077 && cat >"$dump") || die "could not read the dump from stdin"
fi
[ -f "$dump" ] || die "no such file: $dump"
pg_restore --list "$dump" >/dev/null 2>&1 || die "$dump is not a dump pg_restore reads (pg_dump --format=custom)"

# The API writes drafts on shutdown and holds connections: it must be gone.
users=$(q -tA -c "SELECT count(*) FROM pg_stat_activity WHERE datname = 'romulus' AND backend_type = 'client backend'") ||
    die "cannot reach the database"
if [ "$users" != 0 ]; then
    die "$users connection(s) to romulus: stop the API first (kubectl scale deploy/<release>-api --replicas=0), and wait for a running backup to end"
fi

if [ -d /backups ]; then
    ROMULUS_DB='postgresql://romulus@/romulus?host=/var/run/postgresql' BACKUP_DIR=/backups \
        bash /scripts/backup.sh now pre-restore >/dev/null || die "the pre-restore dump failed; the database is unchanged"
else
    log "no /backups volume: no pre-restore dump"
fi

q -c 'DROP DATABASE IF EXISTS romulus_restoring WITH (FORCE)' -c 'CREATE DATABASE romulus_restoring' || exit 1
if ! pg_restore -U romulus --no-owner --exit-on-error --single-transaction -d romulus_restoring "$dump"; then
    q -c 'DROP DATABASE IF EXISTS romulus_restoring WITH (FORCE)'
    die "pg_restore failed; the database is unchanged"
fi
q -c 'DROP DATABASE IF EXISTS romulus_before_restore WITH (FORCE)' || exit 1
# Nobody connects while the names move.
q -c 'ALTER DATABASE romulus ALLOW_CONNECTIONS false' || exit 1
renamed=""
for _ in 1 2 3 4 5 6 7 8 9 10; do
    q -tA -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'romulus'" >/dev/null
    if q -c 'ALTER DATABASE romulus RENAME TO romulus_before_restore' 2>/dev/null; then
        renamed=yes
        break
    fi
    sleep 1
done
if [ -z "$renamed" ]; then
    q -c 'ALTER DATABASE romulus ALLOW_CONNECTIONS true'
    q -c 'DROP DATABASE IF EXISTS romulus_restoring WITH (FORCE)'
    die "the database stayed in use; it is unchanged"
fi
q -c 'ALTER DATABASE romulus_before_restore ALLOW_CONNECTIONS true'
if ! q -c 'ALTER DATABASE romulus_restoring RENAME TO romulus'; then
    q -c 'ALTER DATABASE romulus_before_restore RENAME TO romulus'
    die "could not swap the restored database in; the database is unchanged"
fi
log "done; the replaced database is romulus_before_restore. Start the API again."
