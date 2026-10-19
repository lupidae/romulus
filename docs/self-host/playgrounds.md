# Playgrounds

A playground is a real Postgres holding one version's schema, filled with
generated rows. Everyone in the organization can query it in a console
(read-only), and developers run a draft's migration on its rows to see what
each statement does: the locks it takes, the tables it rewrites, how long
it runs, and the rows that make it fail. Reset puts the generated rows back.
The rows are synthetic and seeded: the same schema, size and seed give the
same rows. The design is in [docs/playground.md](../playground.md).

Playgrounds are optional. Everything else works without them; the app says
they are off.

## What they need

- **Linux.** Not macOS: playgrounds run in gVisor.
- **gVisor** (`runsc`), registered with Docker as two runtimes: `runsc` and
  `runsc-uds`. Each playground runs in its own gVisor sandbox: its own
  kernel, in user space, between Postgres and the host's.
- **An XFS volume with project quotas** for `ROMULUS_PLAY_DIR`
  (`/var/lib/romulus-play` by default). Each playground's directory gets a
  disk quota; reflinks make Reset instant.
- **Memory**: the runner 128 MB, the service 512 MB, and 512 MB per running
  playground, 2 running at most by default. About 1.7 GB in all.

`install.sh` checks all of this when you enable them, and prints what is
missing with the commands below. It never runs them itself.

## Set up the host

As root, on Debian or Ubuntu (elsewhere, gVisor's own guide:
https://gvisor.dev/docs/user_guide/install/).

**1. gVisor**, from its apt repository:

```bash
sudo apt-get update && sudo apt-get install -y ca-certificates curl gnupg
curl -fsSL https://gvisor.dev/archive.key | sudo gpg --dearmor -o /usr/share/keyrings/gvisor-archive-keyring.gpg
echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/gvisor-archive-keyring.gpg] https://storage.googleapis.com/gvisor/releases release main" \
  | sudo tee /etc/apt/sources.list.d/gvisor.list >/dev/null
sudo apt-get update && sudo apt-get install -y runsc
```

It updates with the rest of the system (`apt upgrade`).

**2. Two Docker runtimes** in `/etc/docker/daemon.json` (merge them with
what the file already holds):

```json
{
  "runtimes": {
    "runsc": { "path": "/usr/bin/runsc" },
    "runsc-uds": { "path": "/usr/bin/runsc", "runtimeArgs": ["--host-uds=create"] }
  }
}
```

```bash
sudo systemctl reload docker
```

`runsc-uds` lets a playground with no network create its Postgres socket
in a directory the host shares with it, and connect to no host socket.
Reload, not restart: running containers are untouched.

**3. An XFS volume** at `/var/lib/romulus-play`. Here a 12 GB sparse file
(disk is used as data grows), mounted at every boot with project quotas:

```bash
sudo apt-get install -y xfsprogs
sudo truncate -s 12G /var/lib/romulus-play.img
sudo mkfs.xfs -m reflink=1 /var/lib/romulus-play.img
sudo mkdir -p /var/lib/romulus-play
echo '/var/lib/romulus-play.img /var/lib/romulus-play xfs loop,prjquota,noatime 0 0' | sudo tee -a /etc/fstab
sudo mount /var/lib/romulus-play
```

A spare disk or partition works the same: `mkfs.xfs -m reflink=1` on it,
and `prjquota,noatime` in its `/etc/fstab` line. Size it for the
playgrounds you will keep ([below](#sizing)).

**Check:**

```bash
docker info --format '{{json .Runtimes}}'                # runsc and runsc-uds, with --host-uds=create
docker run --rm --runtime=runsc hello-world              # runs under gVisor
findmnt -no FSTYPE,OPTIONS --target /var/lib/romulus-play   # xfs … prjquota
```

## Turn them on

Run the installer again and answer yes:

```bash
./install.sh
```

Or without questions (everything else comes from `.env`):

```bash
./install.sh --non-interactive --playgrounds
```

It checks the host again, pulls the playground images, starts `play-runner`
and `playground`, and waits for `/api/health` to say `"playgrounds":"ok"`.
Then open a version in the app: **Open playground**.

To turn them off: `./install.sh --non-interactive --no-playgrounds`. The
runner, the service and any running playground stop; their files stay in
`ROMULUS_PLAY_DIR`.

## Sizing

| | |
|---|---|
| a running playground | 512 MB of memory, 1 CPU |
| running at once | 2 (`ROMULUS_PLAY_MAX_RUNNING` in `.env`; 512 MB more each) |
| disk per playground | an XFS quota of 1 GiB (small, about 50 000 rows) or 2 GiB (medium, about a million), plus 16 MiB for its socket |
| free space kept | no new playground while the volume has less than its quota plus 2 GiB free |
| how many | 5 per organization (so per install), 3 per database, 3 made by one person |
| lifetime | stops 15 minutes after its last statement, starts again in seconds; deleted after 7 days unused |

A 12 GB volume holds a handful of playgrounds; make it bigger if your team
keeps several medium ones.

## How they are isolated

Playgrounds run SQL people type, so each layer assumes the one above can
fail:

| threat | how |
|---|---|
| escaping to the host | gVisor (`runsc-uds`), a read-only root, no capabilities, `no-new-privileges`, 128 processes, the image pinned to the release |
| one playground reaching another | one container and one cluster per playground, ids are UUIDs |
| superuser powers | the superuser exists only while the playground is created, on a private socket. Afterwards only three roles can sign in, none a superuser, none with a server-file role, no untrusted language. Every start puts their passwords and settings back |
| "read-only" console | Postgres privileges, not a parse of the SQL: the console's role has `SELECT` and nothing else (no `CREATE`, no `TEMP`, no large objects) |
| the network | each playground has none (`--network none`). The service is on an internal network only the API reaches; the runner has no network at all |
| the Docker socket | only `play-runner` mounts it. It does a fixed set of operations on `romulus-play-<id>` containers; nothing from a request becomes a Docker argument |
| forged calls | the API signs every call to the service (`ROMULUS_PLAY_KEY`, valid 60 s); only the service reaches the runner's socket |
| CPU and memory | 1 CPU and 512 MB per playground, 256 MB per Postgres process; 4 console statements at once host-wide; deadlines (30 s for reviewers, 2 min for developers, 10 min for a migration run) |
| disk | the XFS quota per playground, `temp_file_limit`, the free-space floor |
| logs | statements are never logged; the service logs ids and timings |

## Operating them

- Logs: `./install.sh logs play-runner playground`.
- The playgrounds themselves: `docker ps --filter label=romulus.play.scope=self-host`
  (each is `romulus-play-<id>`). Delete one in the app; the API removes
  whatever the host holds that no playground names.
- Their files: `ROMULUS_PLAY_DIR/pg/<id>` (data and the generated copy) and
  `ROMULUS_PLAY_DIR/run/<id>` (the socket).
- Secrets: `ROMULUS_PLAY_KEY` (API ↔ service) and `ROMULUS_PLAY_SECRET`
  (playgrounds' passwords) in `.env`. To rotate one, delete its line and run
  `./install.sh`: a new one is generated. A new `ROMULUS_PLAY_SECRET` makes
  existing playgrounds unreachable: delete them in the app first.

## Undo the host setup

Turn playgrounds off (above), then as root: `umount /var/lib/romulus-play`,
remove its `/etc/fstab` line and `/var/lib/romulus-play.img`, remove the two
runtimes from `/etc/docker/daemon.json` and `systemctl reload docker`,
`apt purge runsc`, and delete `/etc/apt/sources.list.d/gvisor.list` and
`/usr/share/keyrings/gvisor-archive-keyring.gpg`.
