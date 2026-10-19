# Back up and restore romulus

## What to keep

- **The app database** (the `postgres-data` volume): organizations,
  members, every version, drafts (their live documents are saved in it),
  comments, approvals, the audit log. Everything that matters is in it.
- **`.env`**: this install's settings (URL, mail, profiles) and secrets
  (the containers' passwords, the playground keys, the SMTP password). A
  dump restores without it, into any install; with it, a rebuilt host comes
  back exactly as it was. It holds secrets: store it as carefully as the
  dumps.

Not worth keeping: the sandboxes (every compile is rolled back), Caddy's
volume (certificates are issued again), playgrounds (built again from a
version and a seed).

## Daily dumps

The `backup` container dumps the app database every day at 03:00 UTC into
`deploy/self-host/backups`, and keeps 14 days of them. Change it in `.env`:

| variable | default | |
|---|---|---|
| `BACKUP_AT` | `03:00` | the time, UTC |
| `BACKUP_KEEP_DAYS` | `14` | daily dumps older than this are deleted, once a new one is written |
| `ROMULUS_BACKUP_DIR` | `./backups` | where dumps go, on the host |

Dumps are `pg_dump --format=custom` files named
`romulus-<UTC time>-<label>.dump`. Each is written beside, checked to read
back (`pg_restore --list`), and only then renamed: a failed dump never
leaves a `.dump` behind, and a failed night deletes nothing.

## On demand

```bash
./install.sh backup            # into backups/
./install.sh backup /mnt/disk  # or anywhere
```

It writes `romulus-<time>-manual.dump` and, beside it, a copy of `.env`
(`romulus-<time>-manual.env`, mode 600). `upgrade` and `restore` take
theirs the same way (`pre-upgrade-…`, `pre-restore`). Only daily dumps are
deleted automatically: remove old manual ones yourself.

## Off-host copies

Dumps on the same disk as the database are not a backup: copy them, and
`.env`, somewhere else. For example every day after the dump, with restic
(encrypted, deduplicated):

```bash
# crontab -e, as the user who ran install.sh
30 3 * * * restic -r sftp:backup@backup.example.com:romulus --password-file ~/.restic-pass backup /srv/romulus/deploy/self-host/backups /srv/romulus/deploy/self-host/.env --quiet
```

Or `rsync -a` to another host, `rclone copy` to object storage: anything
that copies files. Check from time to time that the copies are there.

## Restore

```bash
./install.sh restore backups/romulus-20261019T030000Z-daily.dump
```

1. Checks that the file is a dump it can read.
2. Dumps the current database first (`pre-restore`). If that fails, nothing
   changes.
3. Stops the API, restores the dump into a new database, then swaps it in
   by renaming. A failure before the swap changes nothing, and the API
   starts again.
4. Keeps the replaced database as `romulus_before_restore` (replacing an
   older one of that name).
5. Starts everything and waits for `/api/health`.

A dump from an older version restores as is: the API migrates it forward
when it starts. A dump from a newer version needs that version:

```bash
./install.sh restore romulus-….dump --version 1.4.0
```

Once you are happy with the result, drop the replaced database:

```bash
docker compose exec postgres dropdb -U romulus romulus_before_restore
```

## On a new host

1. Install the same version: put `deploy/self-host` on the new host with
   your `.env` in it, then run `./install.sh` (it starts from `.env`'s
   answers and secrets).
2. Restore the latest dump: `./install.sh restore romulus-….dump`.
3. Point DNS at the new host.

## A restore drill

A backup you never restored is a hope. Every few months, restore the latest
dump on a scratch install, beside the real one or on another machine:

```bash
cp -R /srv/romulus/deploy/self-host /tmp/romulus-drill && cd /tmp/romulus-drill
rm -rf .env backups
ROMULUS_SUBNET=172.30.81.0/24 ./install.sh --non-interactive --project romulus-drill \
  --port 8090 --url http://localhost:8090 \
  --admin-email drill@example.com --org Drill --no-caddy --majors none --no-playgrounds
./install.sh restore /srv/romulus/deploy/self-host/backups/romulus-<latest>-daily.dump
./install.sh admin-link you@example.com    # an account from the dump; sign in on http://localhost:8090/app/
./install.sh uninstall --delete-data --confirm romulus-drill
```

`--project`, `--port` and `ROMULUS_SUBNET` keep the drill's containers,
volumes and network apart from the real install's. On a remote host, reach
port 8090 through an SSH tunnel (`ssh -L 8090:127.0.0.1:8090 host`).
