# The playground Postgres

One cluster per playground (docs/playground.md), started by the runner
(`crates/romulus-play-runner`) as `romulus-play-<id>`, with gVisor in the
preview (`runsc-uds`), `--network none`, a read-only root filesystem, no
capabilities, `no-new-privileges`, 128 pids, 512 MB and one CPU. Built as
`ghcr.io/lupidae/romulus-playground:<sha>` by `deploy/preview/deploy.sh build`
and `.github/workflows/images.yml`, and as `romulus-playground:dev` by
`just up` / `just playground-image`.

| file | what |
|---|---|
| `Dockerfile` | the sandbox's Postgres 17, PostGIS and pgvector (bump them together with deploy/sandbox); **nothing trusted**, every control file off the allowlist deleted |
| `extensions.txt` | the allowlist (the same as the sandbox's and `romulus_play_runner::protocol::ALLOWED_EXTENSIONS`: a test keeps the three equal) |
| `play-entrypoint.sh` | the per-process memory cap, the init on first start, the server's settings on the command line |
| `play-init.sql` | the three roles and their privileges, the `playground` database |
| `play-files` | the runner's file helper: create, snapshot, restore, clean, destroy a playground's directories; the volume's free space |

## What the init guarantees

- The superuser (`play_init`) exists only while the entrypoint inits, on a
  socket in `/tmp` nothing else mounts. Afterwards `pg_hba.conf` has one line:
  `local playground play_owner,play_read,play_service scram-sha-256` (plus a
  `host` line in local development, `PLAY_TCP=1`, where there is no gVisor).
- `play_owner` owns the schema (the service builds and runs migrations as
  it). `play_read` (the console) has `USAGE`/`SELECT` on whatever
  `play_owner` creates, no `CREATE`, no `TEMP`, no large objects, and cannot
  signal backends. `play_service` cancels and terminates statements past
  their deadline. None holds a `pg_*_server_*` role.
- The model's extensions are installed during the init, before any
  connection, and never again: nobody can `CREATE EXTENSION` afterwards.
- `temp_file_limit`, `log_min_error_statement=panic` and the rest are on the
  command line: only a superuser could change them, and none can log in.
- Passwords are `HMAC(ROMULUS_PLAY_SECRET, playground, role)`, passed by the
  runner from its own configuration and unset before the server starts.
- Every start, before the server listens, single-user mode puts the three
  roles' passwords and settings back (`repair` in the entrypoint): Postgres
  lets any role change its own password and settings, so `play_read` could
  otherwise lock the service out or follow everyone into their next session.
  The service restarts a playground when it sees either (a refused login,
  settings other than these: `host.rs`, `ROLE_SETTINGS`).
- "Read-only" therefore means no data writes: no privilege but `SELECT`, no
  `CREATE`, no `TEMP`, no large objects. Session settings stay the session's.

## Trying it by hand

```bash
just playground-image
docker volume create play-try
id=00000000-0000-0000-0000-000000000001
docker run --rm --network none --user 0 --cap-drop ALL --cap-add CHOWN --cap-add DAC_OVERRIDE \
  --cap-add FOWNER --mount type=volume,src=play-try,dst=/play --entrypoint play-files \
  romulus-playground:dev create $id
docker run -d --name play-try --network none --read-only --cap-drop ALL \
  --security-opt no-new-privileges --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
  --mount type=volume,src=play-try,dst=/var/lib/postgresql/data,volume-subpath=pg/$id/data \
  --mount type=volume,src=play-try,dst=/var/run/postgresql,volume-subpath=run/$id \
  -e PLAY_OWNER_PASSWORD=o -e PLAY_READ_PASSWORD=r -e PLAY_SERVICE_PASSWORD=s \
  -e 'PLAY_EXTENSIONS=vector:public' romulus-playground:dev
docker run --rm -it --network none --user 4242 -e PGPASSWORD=r \
  --mount type=volume,src=play-try,dst=/s,volume-subpath=run/$id,readonly \
  --entrypoint psql romulus-playground:dev -h /s -U play_read -d playground
docker rm -f play-try && docker volume rm play-try
```
