# Configure romulus

Everything is set in `deploy/self-host/.env`, which `install.sh` writes
(mode 600). After a change, run `./install.sh` again, or `docker compose up
-d` in `deploy/self-host`: the containers whose settings changed restart.
`.env` values are written `KEY='value'`; a value cannot hold a single quote
or a newline.

An empty value means unset: the default applies.

## The install (`.env`, read by compose.yaml)

| variable | default | what |
|---|---|---|
| `ROMULUS_VERSION` | `install.sh`'s version | the images' tag; change it with `./install.sh upgrade` |
| `ROMULUS_PREVIOUS_VERSION` | | what ran before the last upgrade (written by `upgrade`) |
| `ROMULUS_REGISTRY` | `ghcr.io/lupidae` | where the images come from |
| `ROMULUS_PUBLIC_URL` | (asked) | the origin people use, e.g. `https://romulus.example.com` (the API's variable too, below) |
| `ROMULUS_DOMAIN` | the URL's host | the name the bundled Caddy gets a certificate for |
| `ROMULUS_BIND` | `127.0.0.1` | the address the API's port is published on. Keep it on loopback behind a proxy on this host |
| `ROMULUS_PORT` | `8080` | the API's port on the host |
| `ROMULUS_SUBNET` | `172.30.80.0/24` | the compose network's subnet, pinned so Caddy's address and the gateway are known |
| `ROMULUS_CADDY_IP`, `ROMULUS_PROXIES` | derived | written by `install.sh`: Caddy's fixed address (the subnet's `.10`), and the one proxy the API believes ([hardening.md](hardening.md#trusted-proxies)) |
| `COMPOSE_PROFILES` | | what runs besides the base: `caddy`, `pg15`, `pg16`, `pg18`, `playgrounds`, comma-separated |
| `COMPOSE_PROJECT_NAME` | `romulus` | names the containers and volumes (`romulus_postgres-data`…). Set only to run two installs on one host |
| `ROMULUS_SANDBOX_PG15`, `_PG16`, `_PG18` | | `on`: the API compiles that major on its sandbox. Goes with the `pg15`… profile; `install.sh` sets both |
| `ROMULUS_ADMIN_EMAIL`, `ROMULUS_ADMIN_NAME`, `ROMULUS_ORG_NAME`, `ROMULUS_ORG_SLUG` | (asked) | what `install.sh` passes to `romulus-api setup` |
| `ROMULUS_PLAY_URL` | | `http://playground:8080` when playgrounds are on, empty when off |
| `ROMULUS_PLAY_DIR` | `/var/lib/romulus-play` | the XFS directory playgrounds live in ([playgrounds.md](playgrounds.md)) |
| `ROMULUS_PLAY_MAX_RUNNING` | `2` | playgrounds running at once (512 MB each) |
| `ROMULUS_PLAY_SCOPE` | `self-host` | the label on this install's playground containers; change it only for a second install with playgrounds on the same host |
| `ROMULUS_BACKUP_DIR` | `./backups` | where dumps go ([backup.md](backup.md)) |
| `BACKUP_AT`, `BACKUP_KEEP_DAYS` | `03:00`, `14` | the daily dump's time (UTC) and how long daily dumps are kept |
| `ROMULUS_UID`, `ROMULUS_GID` | the installing user | who owns the dumps the `backup` container writes |
| `ROMULUS_API_MEMORY`, `ROMULUS_POSTGRES_MEMORY`, `ROMULUS_SANDBOX_MEMORY` | `1g`, `768m`, `768m` | memory limits ([install.md](install.md#sizing)) |
| `POSTGRES_PASSWORD`, `SANDBOX_PASSWORD`, `ROMULUS_PLAY_KEY`, `ROMULUS_PLAY_SECRET` | generated | secrets, 64 hex characters, generated once ([hardening.md](hardening.md#secrets)) |

## The API (`romulus-api`)

compose.yaml passes these to the `api` container. The defaults are the
binary's; the bundle's own value is in the third column when it differs.

| variable | default | bundle | what |
|---|---|---|---|
| `DATABASE_URL` | required | the `postgres` container | the app database |
| `ROMULUS_PUBLIC_URL` | required | `.env` | the browser's origin. Every unsafe request must come with this `Origin` (CSRF), cookies are `Secure` when it is https, and links in emails and from the CLI start with it |
| `ROMULUS_API_ADDR` | `127.0.0.1:41001` | `0.0.0.0:8080` (the image) | where it listens, inside the container |
| `ROMULUS_WEB_DIR` | unset | `/srv/web` (the image) | the web app's build, served at `/app` |
| `ROMULUS_AUTH` | `oidc` | `password` | sign-in methods, comma-separated: `password` (built-in email and password), `oidc` (an OpenID Connect provider, below) |
| `ROMULUS_SIGNUP` | `invite` | `invite` | who may create an account ([below](#accounts)) |
| `ROMULUS_SINGLE_ORG` | unset | `true` | one organization per install, made by `setup`; creating another is refused and hidden in the app |
| `ROMULUS_TRUSTED_PROXIES` | empty (none) | `ROMULUS_PROXIES` | IPs and CIDRs, comma-separated, whose `X-Forwarded-For` gives the client's address; `/0` is refused ([hardening.md](hardening.md#trusted-proxies)) |
| `ROMULUS_SMTP_HOST`, `_PORT`, `_TLS`, `_USERNAME`, `_PASSWORD`, `ROMULUS_MAIL_FROM` | mail off | `.env` | [Mail](#mail-smtp) |
| `ROMULUS_SANDBOX_URL` | unset | the `sandbox` container | an admin connection to Postgres 17's sandbox, where SQL compiles. Unset: uploads answer 503 and drafts do not compile |
| `ROMULUS_SANDBOX_URL_PG15`, `_PG16`, `_PG18` | unset | when enabled | the other majors' sandboxes. A major without one answers 503 for its databases only |
| `ROMULUS_SANDBOX_PREFIX` | `romulus_sandbox` | | names the compile pool's roles and databases in the sandboxes. One API per prefix and sandbox |
| `ROMULUS_PLAY_URL`, `ROMULUS_PLAY_KEY` | unset | when playgrounds are on | the playground service and the key the API signs its calls with (32 characters or more). Both or neither; neither: playgrounds are off |
| `ROMULUS_OIDC_ISSUER`, `ROMULUS_OIDC_CLIENT_ID`, `ROMULUS_OIDC_CLIENT_SECRET` | unset | `.env` | an OpenID Connect client: all three, or none |
| `ROMULUS_SESSION_COOKIE` | `romulus_session` | | the session cookie's name |
| `ROMULUS_LOG_FORMAT` | text | `json` (the image) | `json`: one JSON object per line |
| `RUST_LOG` | `romulus_api=debug,…` | `romulus_api=info,tower_http=info,sqlx=warn` | log levels, as `tracing`'s `EnvFilter` reads them |
| `ROMULUS_EDGE_KEY` | unset | unset | for the hosted preview's edge proxy only: requests without it are refused. Leave it unset |
| `ROMULUS_DEV_SIGNIN` | `false` | `false` | development only: refused unless the URL and the address are loopback. Never set it |

### Accounts

- `ROMULUS_SIGNUP=invite` (the default): an account can only be created
  from an invitation link, with the invited address. Admins invite from the
  organization's members page.
- `ROMULUS_SIGNUP=open`: anyone who reaches the URL can create an account,
  proving the address by email, so it needs mail. Keep `invite` unless the
  install is only reachable by people you trust.
- **Open sign-up without `ROMULUS_SINGLE_ORG`** lets anyone who signs up
  create organizations, and `romulus-api setup` refuses to run once any
  organization exists (it never takes over an install). On such a server,
  run `setup` before opening sign-up, or a stranger may create the first
  organization before you do. The bundle always runs single-organization.

### Sign-in with your own identity provider

`ROMULUS_AUTH=password,oidc` adds "sign in with SSO" beside passwords;
`oidc` alone replaces them. Register romulus as a confidential web client
with the authorization code flow and PKCE:

- redirect URI: `https://romulus.example.com/api/auth/callback`;
- post-logout redirect URIs: `https://romulus.example.com/app/signin`,
  `https://romulus.example.com/app/` and `https://romulus.example.com/`;
- scopes: `openid email profile`; the provider must say whether the email
  is verified (`email_verified`).

Then set `ROMULUS_OIDC_ISSUER` (the issuer URL, exactly as the provider's
discovery document spells it), `ROMULUS_OIDC_CLIENT_ID` and
`ROMULUS_OIDC_CLIENT_SECRET` in `.env`. The hosted romulus uses Zitadel;
other providers speak the same protocol but are less tested.

## Mail (SMTP)

Mail is off until `ROMULUS_SMTP_HOST` is set. With it, romulus sends
invitations, sign-up links (an account is created from the link, which
proves the address) and password resets. Without it, invitation links are
copied by hand (an invitation then vouches for its address: the account is
created at once), and an admin makes password links with
`./install.sh admin-link EMAIL`.

| variable | default | what |
|---|---|---|
| `ROMULUS_SMTP_HOST` | empty: mail off | the SMTP server |
| `ROMULUS_SMTP_TLS` | `starttls` | `starttls` (upgrade a plain connection), `implicit` (TLS from the start, usually port 465) or `none` (only for a relay on this host, a loopback address: refused otherwise) |
| `ROMULUS_SMTP_PORT` | 587 for `starttls`, 465 for `implicit`, 25 for `none` | |
| `ROMULUS_SMTP_USERNAME`, `ROMULUS_SMTP_PASSWORD` | none | both or neither |
| `ROMULUS_MAIL_FROM` | required with a host | the sender, e.g. `romulus <romulus@example.com>`. Use a domain your provider is set up to send for (SPF, DKIM), or messages land in spam |

Check it: `./install.sh --mail-test` (or, from `deploy/self-host`,
`docker compose exec api romulus-api mail-test you@example.com`). It sends
a test message, and prints the server's error if that fails.

**Resend**: the password is an API key with sending access, the username is
`resend`.

```bash
ROMULUS_SMTP_HOST='smtp.resend.com'
ROMULUS_SMTP_TLS='implicit'        # port 465; or 'starttls' on port 587
ROMULUS_SMTP_PORT='465'
ROMULUS_SMTP_USERNAME='resend'
ROMULUS_SMTP_PASSWORD='re_…'
ROMULUS_MAIL_FROM='romulus <romulus@mail.example.com>'   # a domain verified in Resend
```

**Amazon SES**: SMTP credentials made in the SES console (not your AWS
keys).

```bash
ROMULUS_SMTP_HOST='email-smtp.eu-west-1.amazonaws.com'
ROMULUS_SMTP_TLS='starttls'
ROMULUS_SMTP_PORT='587'
ROMULUS_SMTP_USERNAME='AKIA…'
ROMULUS_SMTP_PASSWORD='…'
ROMULUS_MAIL_FROM='romulus <romulus@example.com>'
```

**Any other provider** (Postmark, Mailgun, SendGrid, your company's
server): its SMTP host, port and credentials, the same way.

## Admin commands

The API image's binary runs these. From `deploy/self-host`:

```bash
docker compose exec api romulus-api <command>
```

| command | what |
|---|---|
| `setup --email EMAIL --name NAME --org ORG [--slug SLUG]` | creates the admin (verified email, no password yet), the organization and the membership; prints a one-time link to set the password (24 hours). Run again: a fresh link while the password was never set, otherwise nothing ("already set up"). Exit 2 when the organization exists with another admin. `install.sh` runs it |
| `password-link EMAIL` | prints a one-time password link (1 hour), for installs without mail or a locked-out admin. `./install.sh admin-link EMAIL` runs it |
| `unlock EMAIL` | clears the sign-in lockout of that address |
| `verify-email EMAIL` | marks the password account with that address verified (they always are; kept for repairs). Single sign-on accounts are left alone, with a warning: their provider verifies them |
| `mail-test EMAIL` | sends a test message; non-zero with the server's error if it fails, exit 3 when mail is off |
| `auth-events [--email EMAIL] [--limit N]` | recent sign-in events, as JSON lines |
| `migrate` | applies pending migrations and exits (`serve` does it too) |
| `serve` | runs the API (the default) |
| `openapi` | prints the API's OpenAPI document |

Each reads the container's environment, so it uses the same database and
URL as the running API.

## Playgrounds (the runner and the service)

compose.yaml sets these for `play-runner` and `playground`; the ones in
`.env` above are the ones to change.

| variable | default | bundle | what |
|---|---|---|---|
| `ROMULUS_PLAY_ROOT` | required | `ROMULUS_PLAY_DIR` | the playgrounds' directory on the host (XFS with project quotas) |
| `ROMULUS_PLAY_RUNTIME` | `runsc-uds` | `runsc-uds` | the Docker runtime playgrounds run with (gVisor) |
| `ROMULUS_PLAY_SCOPE` | `default` | `self-host` | the label on this install's playground containers |
| `ROMULUS_PLAY_IMAGE` | required | `romulus-playground:<version>` | a playground's Postgres |
| `ROMULUS_PLAY_QUOTA_IMAGE` | unset: no quotas | `romulus-play:<version>` | the image `xfs_quota` runs from |
| `ROMULUS_PLAY_SECRET` | required | `.env` | playgrounds' passwords derive from it (32 characters or more) |
| `ROMULUS_PLAY_KEY` | required | `.env` | the key the API signs the service's calls with |
| `ROMULUS_PLAY_MAX_RUNNING` | `3` | `2` | running at once, host-wide |
| `ROMULUS_PLAY_MAX_RUNNING_PER_ORG` | `2` | `ROMULUS_PLAY_MAX_RUNNING` | per organization (one, here) |
| `ROMULUS_PLAY_STATEMENTS` | `4` | `4` | console statements running at once, host-wide |
| `ROMULUS_PLAY_IDLE_MINUTES` | `15` | | a playground stops this long after its last statement |
| `ROMULUS_PLAY_MAX_DATA_KB` | `262144` | | each Postgres process's private memory cap in a playground |
| `ROMULUS_PLAY_ADDR`, `ROMULUS_PLAY_RUNNER_SOCKET`, `ROMULUS_PLAY_SOCKETS`, `ROMULUS_PLAY_DOCKER` | | fixed | how the three talk: leave them |
| `ROMULUS_PLAY_TCP` | `false` | | local development without gVisor only; refused with gVisor |
