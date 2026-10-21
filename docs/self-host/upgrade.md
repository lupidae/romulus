# Upgrade romulus

Each release, tagged `vX.Y.Z`, publishes its images as `X.Y.Z`, `X.Y`
and `latest` ([install.md](install.md#versions)). `ROMULUS_VERSION` in
`.env` is what runs: a version you pinned, or `latest`.

## Steps

1. Read the release notes.
2. Get the new release's files into the same `deploy/self-host` directory,
   keeping `.env` and `backups/`:
   - from a clone: `git fetch --tags && git checkout <the release's tag>`
     (both are ignored by git, so they stay);
   - from a source archive: unpack the new one, then copy `.env` and
     `backups/` into its `deploy/self-host`.
3. Run:

   ```bash
   ./install.sh upgrade 1.4.0      # to a published version
   ./install.sh upgrade            # to the script's default: latest, the newest release
   ```

## What it does

1. Dumps the app database to
   `backups/romulus-<time>-pre-upgrade-<old>-to-<new>.dump` (with a copy of
   `.env`). If that fails, nothing changes.
2. Sets `ROMULUS_VERSION` to the new version, and `ROMULUS_PREVIOUS_VERSION`
   to the old one.
3. Pulls the images. If that fails, it puts the old version back: nothing
   changed.
4. Starts the new containers and waits for `/api/health`. The API applies
   the new database migrations when it starts.

The API restarts, so romulus is away for a few seconds, plus the time the
migrations take. Open drafts reconnect on their own: on shutdown the API
writes every open draft, so nothing typed is lost.

Do not upgrade with `docker compose pull` yourself: it takes no backup first.

## Going back

Migrations only go forward. Once a newer API has migrated the database, an
older one refuses to start on it. If the upgrade does not come up, or you
need the old version back, restore the pre-upgrade dump with the old
version (`install.sh` prints this command when the upgrade fails):

```bash
./install.sh restore backups/romulus-<time>-pre-upgrade-1.3.0-to-1.4.0.dump --version 1.3.0
```

What was written after the upgrade leaves the live database. It is kept
twice: in a `pre-restore` dump, and as the database `romulus_before_restore`
([backup.md](backup.md#restore)).

## Changing settings

To change an answer (the URL, mail, the Postgres majors, playgrounds), run
`./install.sh` again: its questions start from what `.env` says. Or edit
`.env`, then run `./install.sh` (or `docker compose up -d` in this
directory). Neither changes the version.

## On arm64, from 0.1.0

0.1.0's images are amd64 only, which an arm64 host runs emulated. Later
releases publish amd64 and arm64 under the same tags, so the upgrade
pulls the arm64 builds and starts the containers on them, with the same
volumes. Nothing to do by hand.

## The other images

- `postgres:18.6` (the app database) is pinned in `compose.yaml`; a release
  that moves it says so.
- `caddy:2` follows Caddy 2's releases: each upgrade pulls the latest.
- Moving the app database to another Postgres major (19, one day) is a dump
  and a restore, which that release's notes will spell out. `upgrade` never
  does it on its own.
