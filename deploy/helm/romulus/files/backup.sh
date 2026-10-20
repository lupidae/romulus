#!/usr/bin/env bash
# The `backup` service (compose.yaml): a dump of the app database every day
# at $BACKUP_AT (UTC), in the custom format, into /backups (ROMULUS_BACKUP_DIR
# on the host, ./backups by default). Each dump is written beside, checked to
# read back (pg_restore --list), and only then renamed to *.dump. Daily dumps
# older than $BACKUP_KEEP_DAYS days are pruned once a new one is written;
# dumps taken by `install.sh backup` (other labels) are never pruned.
#
#   backup.sh daily        run `nightly` every day at $BACKUP_AT
#   backup.sh nightly      dump once, then prune
#   backup.sh now [LABEL]  dump once; prints the dump's path
#
# Every step checks its own result rather than relying on `set -e`, which
# bash ignores inside a function called from `||`, `&&` or `if`: a failed
# pg_dump must never become a *.dump.
#
# Copy .env off the host too: it holds this install's settings and secrets,
# which bring a rebuilt host back as it was (docs/self-host/backup.md).
set -euo pipefail

: "${ROMULUS_DB:?}"
BACKUP_AT=${BACKUP_AT:-03:00}
BACKUP_KEEP_DAYS=${BACKUP_KEEP_DAYS:-14}
DIR=${BACKUP_DIR:-/backups}

log() { echo "backup: $*" >&2; }

# dump LABEL → the dump's path on stdout, or non-zero.
dump() {
    local label=${1:-manual} stamp file
    stamp=$(date -u +%Y%m%dT%H%M%SZ) || return 1
    file="$DIR/romulus-$stamp-$label.dump"
    if ! pg_dump --format=custom --no-owner --dbname="$ROMULUS_DB" --file="$file.partial"; then
        rm -f "$file.partial"
        log "pg_dump failed; nothing written"
        return 1
    fi
    if ! pg_restore --list "$file.partial" >/dev/null; then
        rm -f "$file.partial"
        log "the dump does not read back; nothing written"
        return 1
    fi
    mv "$file.partial" "$file" || return 1
    log "wrote $file ($(du -h "$file" | cut -f1))"
    echo "$file"
}

prune() {
    find "$DIR" -maxdepth 1 -name 'romulus-*-daily.dump' -mtime +"$BACKUP_KEEP_DAYS" -print -delete |
        sed 's/^/backup: pruned /' >&2
    find "$DIR" -maxdepth 1 -name '*.partial' -mmin +60 -delete
}

# Old dumps go only once a new one is safely written.
nightly() {
    if dump daily >/dev/null; then
        prune
    else
        log "the nightly dump failed; kept every existing dump"
        return 1
    fi
}

# Seconds from now until the next $BACKUP_AT, UTC.
until_next() {
    local now next
    now=$(date -u +%s)
    next=$(date -u -d "today $BACKUP_AT" +%s)
    [ "$next" -gt "$now" ] || next=$(date -u -d "tomorrow $BACKUP_AT" +%s)
    echo $((next - now))
}

mkdir -p "$DIR"
case "${1:-daily}" in
now) dump "${2:-manual}" ;;
nightly) nightly ;;
daily)
    log "daily at $BACKUP_AT UTC into $DIR, kept $BACKUP_KEEP_DAYS days"
    # PID 1 in its container: stop at once on `docker stop`.
    trap 'exit 0' TERM INT
    while true; do
        sleep "$(until_next)" &
        wait $!
        # A failed night is logged and retried the next one, never fatal.
        nightly || true
    done
    ;;
*)
    echo "usage: backup.sh daily | nightly | now [LABEL]" >&2
    exit 2
    ;;
esac
