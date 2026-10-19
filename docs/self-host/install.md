# Install romulus on your own server

romulus runs on one Linux host with Docker. `deploy/self-host/install.sh`
sets it up: it checks the host, asks a few questions, writes
`deploy/self-host/.env`, starts the containers and creates the first admin.
Re-run it any time: it converges, and it never replaces a secret.

The other pages:

- [upgrade.md](upgrade.md): moving to a new release, and back.
- [backup.md](backup.md): what to keep, restoring, drills.
- [configuration.md](configuration.md): every setting, SMTP examples, the admin commands.
- [playgrounds.md](playgrounds.md): the host setup playgrounds need.
- [hardening.md](hardening.md): what to check before people rely on it.

## Requirements

- **Linux on x86-64.** The images are published for amd64; on arm64 they
  run emulated, slowly. macOS (Docker Desktop) works for a trial, not for
  production, and without playgrounds.
- **Docker Engine with the compose plugin, 2.20 or later.** The user who
  runs `install.sh` must be able to use Docker (root, or the `docker`
  group). See https://docs.docker.com/engine/install/.
- **4 GB of memory** for the base install, 8 GB with the other Postgres
  majors or playgrounds ([Sizing](#sizing)).
- **20 GB of free disk** is comfortable: about 3 GB of images, then the
  database and its backups.
- **HTTPS.** A domain name pointing at the host with ports 80 and 443 open,
  for the bundled Caddy; or your own reverse proxy.
- Optional: an **SMTP server** (any provider), for invitations, address
  checks and password resets by email.
- Optional: **gVisor and an XFS volume**, for playgrounds
  ([playgrounds.md](playgrounds.md)).

## Install

Get the files, then run the script from its directory:

```bash
git clone https://github.com/lupidae/romulus.git
cd romulus/deploy/self-host
./install.sh
```

Or download a release's source archive from the repository's Releases page,
unpack it, and run `./install.sh` in its `deploy/self-host` the same way.

Keep that directory. `.env` and `backups/` live in it, and every later
command (`upgrade`, `backup`, `restore`) runs from it.

The script is not meant to be piped from `curl` into `bash`: it asks
questions, and it runs Docker as you, so read it first.

### What it asks

| question | what to answer |
|---|---|
| Public URL | Where people open romulus: `https://romulus.example.com`. For a trial on this machine, `http://localhost:8080`. The origin only, no path: the app lives at `/app`. |
| Admin's email and name | The first admin, usually you. |
| Organization's name | One organization per install; everyone you invite joins it. |
| Run the bundled Caddy? | Asked for an https URL on a domain. Yes: Caddy gets a certificate from Let's Encrypt. No: you run your own proxy ([below](#your-own-reverse-proxy)). |
| Other Postgres majors | Databases target Postgres 17 by default. A database on 15, 16 or 18 compiles on that major's own Postgres, about 768 MB each. `none` is fine; re-run the script to add one later. |
| Set up SMTP now (recommended)? | Yes by default. Host, TLS mode (starttls or implicit), port, username, password, from address ([configuration.md](configuration.md#mail-smtp)). Strongly recommended: with mail, people locked out by wrong passwords get back in with a reset link on their own. Without it, invitation links are copied by hand and password links come from `./install.sh admin-link`. With mail, it offers to send a test email once running. |
| Enable playgrounds? | Linux only. It checks gVisor and the XFS volume; when something lacks, it prints exactly what to run, then lets you stop or continue without them ([playgrounds.md](playgrounds.md)). |

It then shows a summary and asks to go ahead. Nothing changes before that.
If port 8080 is taken, it asks for another one first.

### Without questions

Every answer is also an option, and an environment variable
(`./install.sh --help` lists them):

```bash
./install.sh --non-interactive \
  --url https://romulus.example.com \
  --admin-email ada@example.com --admin-name "Ada Lovelace" --org "Acme" \
  --caddy --majors none --no-playgrounds
```

The SMTP password is read from `ROMULUS_SMTP_PASSWORD` (never an option: it
would show in `ps`). On a re-run, what you leave out comes from `.env`.

### What it does

1. Checks the host: the OS, Docker and compose, the daemon, memory and disk
   (warnings below 4 GB of memory), the ports.
2. Writes `.env` (mode 600): your answers, and secrets generated once (the
   database and sandbox passwords, the playground keys). A secret already in
   `.env` is never replaced.
3. Pulls the images, starts everything, and waits for `/api/health`. The API
   applies its database migrations when it starts.
4. Runs `romulus-api setup`, which creates the admin (email verified, no
   password yet) and the organization, and prints a one-time link.
5. Prints the link, the URL and what is left to do.

Re-running it with the same answers changes nothing. If the admin has not
set a password yet, it prints a fresh link.

## First sign-in

Open the link printed at the end. It is valid for 24 hours, once. It lets
you choose the admin's password, then signs you in.

Lost it, or it expired? Get another one (valid for an hour):

```bash
./install.sh admin-link ada@example.com
```

Then invite your team from the organization's members page. Without SMTP,
copy each invitation link and send it yourself. By default an account can
only be created from an invitation, with the invited address
(`ROMULUS_SIGNUP=invite`; [configuration.md](configuration.md#accounts)).

## DNS and HTTPS

### The bundled Caddy

1. Create an A (and AAAA, if the host has IPv6) record for the domain,
   pointing at the host.
2. Open ports 80 and 443 to the internet (Let's Encrypt checks port 80).
3. Caddy gets the certificate on its own once the name resolves, and renews
   it. `./install.sh logs caddy` shows how it went.

The Caddyfile it uses is `deploy/self-host/Caddyfile`: TLS, HSTS, `/`
redirected to `/app/`, everything proxied to the API.

### Your own reverse proxy

The API is published on `127.0.0.1:8080` (`ROMULUS_BIND`, `ROMULUS_PORT`).
Proxy the whole origin to it, websockets included: live editing of drafts
and the playground console use them. Send the client's address in
`X-Forwarded-For`, replacing whatever the client sent.

Caddy: `deploy/self-host/Caddyfile.example`. nginx:

```nginx
# In the http block.
map $http_upgrade $connection_upgrade {
    default upgrade;
    ''      close;
}

server {
    listen 443 ssl;
    http2 on;
    server_name romulus.example.com;
    ssl_certificate     /etc/letsencrypt/live/romulus.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/romulus.example.com/privkey.pem;

    add_header Strict-Transport-Security "max-age=31536000" always;
    client_max_body_size 64m;                   # migration imports

    location = / { return 302 /app/; }
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;  # websockets
        proxy_set_header Connection $connection_upgrade;
        proxy_read_timeout 1h;                   # websockets stay open
        proxy_buffering off;                     # import progress streams
    }
}
```

A proxy on the host reaches the API through the compose network's gateway,
which `ROMULUS_TRUSTED_PROXIES` trusts by default. A proxy on another
machine: see [hardening.md](hardening.md#trusted-proxies).

## Sizing

Memory limits per container, and when each runs:

| container | limit | runs |
|---|---|---|
| api | 1 GB (`ROMULUS_API_MEMORY`) | always; drafts being edited live in it |
| postgres | 768 MB (`ROMULUS_POSTGRES_MEMORY`) | always |
| sandbox (Postgres 17) | 768 MB (`ROMULUS_SANDBOX_MEMORY`) | always; where SQL compiles |
| backup | 256 MB | always; sleeps between daily dumps |
| caddy | 128 MB | with the bundled Caddy |
| sandbox-15, -16, -18 | 768 MB each | when you enable that major |
| play-runner, playground | 128 MB + 512 MB | with playgrounds |
| each running playground | 512 MB | 2 at a time by default (`ROMULUS_PLAY_MAX_RUNNING`) |

The base install's limits add up to about 2.9 GB: a 4 GB host leaves the
rest to the system. Each other major adds 768 MB; playgrounds add about
1.7 GB. With everything, plan 8 GB. Limits are caps, not reservations: an
idle install uses much less.

Two CPUs are enough for a team. The sandboxes and the playground service
get a lower CPU weight, so under load the API and its database go first.

To change a limit, set it in `.env` (the commented lines at its end), then
run `./install.sh` again. Keep the sandbox at 768 MB or more: its compile
pool is sized for it (deploy/sandbox/README.md).

## The files

All in `deploy/self-host`:

| file | what |
|---|---|
| `install.sh` | install and operate (`./install.sh --help`) |
| `compose.yaml` | the containers; reads everything from `.env` |
| `.env` | your settings and secrets, mode 600; written by `install.sh`, ignored by git |
| `backups/` | daily dumps and the ones `install.sh` takes |
| `Caddyfile` | the bundled Caddy's configuration |
| `Caddyfile.example` | the same for a Caddy you run yourself |
| `backup.sh` | the daily dump, run by the `backup` container |

`docker compose …` run in this directory uses `.env` too, so `docker compose
ps` and `docker compose logs api` work as expected.

## When something is wrong

- `./install.sh status`: the containers and `/api/health`. Health says
  `database` (the app database), `sandbox` and `sandboxes` (SQL compiles, by
  Postgres major: `ok`, `unavailable` or `off`) and `playgrounds`.
- `./install.sh logs api` (or `postgres`, `sandbox`, `caddy`…). The API logs
  JSON lines.
- **Port taken**: re-run with `--port 8081` (and a matching URL).
- **"Pool overlaps with other one on this address space"**: the compose
  network's subnet (`ROMULUS_SUBNET`, `172.30.80.0/24`) collides with
  another network on this host. Pick a free one in `.env`, then
  `./install.sh` again.
- **The sandbox is `unavailable`**: uploads and drafts answer 503 until it
  answers. `./install.sh logs sandbox`.
- **"the volume …_postgres-data exists but .env is gone"**: the data of an
  earlier install is still there, and new passwords would not open it. Put
  `.env` back from your backups, or delete that install's volumes first.
