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
| `ROMULUS_OPERATOR_EMAILS` | empty: off | `.env` | the people running this install, comma-separated: emailed as things happen and once a day ([Operator notifications](#operator-notifications)). Needs mail: the API refuses to start without it |
| `ROMULUS_OPERATOR_DIGEST_HOUR` | `7` | `.env` | the hour (UTC, 0 to 23) from which the daily digest goes |
| `ROMULUS_OPERATOR_IGNORE_DOMAINS` | empty: none | `.env` | email domains, comma-separated (`romulus.test,example.com`), whose accounts the notifications and the digest leave out: your team's, your tests' ([Ignored domains](#ignored-domains)) |
| `ROMULUS_SANDBOX_URL` | unset | the `sandbox` container | an admin connection to Postgres 17's sandbox, where SQL compiles. Unset: uploads answer 503 and drafts do not compile |
| `ROMULUS_SANDBOX_URL_PG15`, `_PG16`, `_PG18` | unset | when enabled | the other majors' sandboxes. A major without one answers 503 for its databases only |
| `ROMULUS_SANDBOX_PREFIX` | `romulus_sandbox` | | names the compile pool's roles and databases in the sandboxes. One API per prefix and sandbox |
| `ROMULUS_PLAY_URL`, `ROMULUS_PLAY_KEY` | unset | when playgrounds are on | the playground service and the key the API signs its calls with (32 characters or more). Both or neither; neither: playgrounds are off |
| `ROMULUS_OIDC_ISSUER`, `ROMULUS_OIDC_CLIENT_ID`, `ROMULUS_OIDC_CLIENT_SECRET` | unset | `.env` | an OpenID Connect client: all three, or none |
| `ROMULUS_ZITADEL_TOKEN` | unset | unset | with a Zitadel OIDC client: a personal access token of a machine user allowed to delete users (Zitadel's IAM_USER_MANAGER). `delete-user` then deletes the person's Zitadel user too; without it, it prints the user to delete in Zitadel's console |
| `ROMULUS_DELETE_INACTIVE_ACCOUNTS` | `false` | `false` | `true`: accounts nobody used for 3 years are deleted, after an email 30 days before ([below](#retention-and-deletion)). Needs mail, and `ROMULUS_ZITADEL_TOKEN` with a Zitadel OIDC client. Off, no account is ever deleted on its own |
| `ROMULUS_SOURCE_URL` | the repository the image was built from (`https://github.com/lupidae/romulus` for the official images) | | where the app's "Source code" link points, at the commit that runs (AGPL-3.0, section 13). Set it when you run a modified romulus: to your own repository |
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

## Operator notifications

Set `ROMULUS_OPERATOR_EMAILS` (one address or several, comma-separated) to
hear about what matters on your install as it happens. Each event is one
short email, its subject starting with `[romulus]`:

- a new account (a first sign-in with single sign-on, a password sign-up,
  `setup`) and a new organization;
- an account or an organization deleted: on request (`delete-user`,
  `delete-org`) or, with `ROMULUS_DELETE_INACTIVE_ACCOUNTS`, for
  inactivity; what went with it, and the Zitadel user to delete by hand if
  romulus could not;
- with hosted billing on (`ROMULUS_BILLING`, which the self-host bundle
  never sets): a subscription started (plan, interval, price, who bought it,
  and a business's name and country), upgraded or downgraded (or a change
  scheduled for the period's end), a cancellation scheduled, a subscription
  over (with Stripe's reason when the customer gave one), a failed payment,
  a grace period over (the organization moved to Free), a payment after it
  on the same subscription (back from Free), a consumer's withdrawal and its
  refund, and an organization reaching a cap of its plan for the first time
  (at most once per organization and cap in 30 days). These link to the
  customer or subscription in Stripe's dashboard, in test or live mode as
  the key is. A scheduled change names the plan it becomes, without a
  price (Stripe's schedule has it).

Every day from `ROMULUS_OPERATOR_DIGEST_HOUR` (UTC) a digest follows,
about the day before and the 7 days to it: new and active accounts and
organizations; with billing, paying organizations (active subscriptions) by
plan, the monthly recurring revenue (their list prices before discounts,
yearly plans divided by 12) and its change since the last digest,
organizations trialing and those whose payment failed, in their grace period
(neither counted in it), cancellations scheduled and the month's playground
hours; migration imports, migration tests and their outcomes, deletions.

Notifications are queued in the database in the transaction of what they
report (a failure never breaks anyone's action) and sent by the API's mail
outbox within half a minute, retried for 3 days if your mail server refuses
them. They hold names, addresses and business names; the outbox deletes
them 30 days after they were sent, 90 after they were given up ([Retention
and deletion](#retention-and-deletion)). What people typed (names, business
names, a cancellation's comment) is shown on one line, never as a link: the
only links are to Stripe's dashboard and to your install. Check the setup
with `operator-test` (below). On Kubernetes, set these variables with the
chart's `api.extraEnv`.

### Ignored domains

Your own team signing in, and your tests, would otherwise be news. List
their email domains in `ROMULUS_OPERATOR_IGNORE_DOMAINS`, comma-separated
(`romulus.test,example.com`; a leading `@` is allowed). An address matches
when its domain is one of them or under one (`ops@eu.example.com` matches
`example.com`; `ops@notexample.com` does not), whatever the case. Then:

- an account at one of them is never the subject of a notification: not
  when it is created, not when it is deleted;
- nor is an organization all of whose members are at one of them: its
  creation (by such an account), its deletion, its plan's caps, its
  billing; nor a subscription such an account bought, wherever;
- the digest counts none of them: such accounts (new, in all, active),
  such organizations and their imports, migration tests and playground
  hours, subscriptions such an account bought (or of such an organization:
  paying, MRR, trialing, cancelling, in grace), and their deletions.

An organization is judged by its members at the time: one with a single
member from outside is told of and counted. Empty (the default) leaves
nothing out.

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
| `operator-test` | sends a test notification and today's digest to `ROMULUS_OPERATOR_EMAILS` now, one line per message; non-zero when one was not sent. The day's digest still goes at its hour |
| `auth-events [--email EMAIL] [--limit N]` | recent sign-in events, as JSON lines |
| `delete-user EMAIL [--dry-run]` | deletes every account with that address and what it alone holds ([below](#retention-and-deletion)); `--dry-run` says what would go. Exit 4 while it is the only admin of an organization others belong to |
| `delete-org SLUG [--dry-run]` | deletes the organization and everything in it; `--dry-run` says what would go |
| `migrate` | applies pending migrations and exits (`serve` does it too) |
| `serve` | runs the API (the default) |
| `openapi` | prints the API's OpenAPI document |

Each reads the container's environment, so it uses the same database and
URL as the running API.

## Retention and deletion

What romulus deletes on its own, whatever the configuration:

- a **deleted database**, 30 days after its deletion, with its versions,
  drafts and comments (until then it is hidden, not gone); its audit events
  stay the organization's, with its name;
- a **removed version**, 30 days after its removal (or once no open draft
  is based on it);
- an **invitation** nobody accepted, 30 days after it expired (a revoked one
  at once);
- the **sign-in log**, after 90 days; **playgrounds**, 7 days after their
  last use.

On request, an admin of the install deletes an account or an organization:

```bash
docker compose exec api romulus-api delete-user ada@example.com --dry-run   # what would go
docker compose exec api romulus-api delete-user ada@example.com
docker compose exec api romulus-api delete-org acme --dry-run
```

- `delete-user` deletes the account (sessions, password, access tokens,
  memberships, approvals) and every organization it is the **only member**
  of. What the
  person wrote in organizations they shared stays there, shown as by a
  deleted account, and the audit log keeps the name it recorded: that is the
  organization's. It refuses while the person is the **only admin** of an
  organization others belong to: make another member an admin first, or
  delete the organization too.
- `delete-org` deletes the organization with its databases, versions,
  drafts, comments, members, invitations and audit log. Its members keep
  their accounts. On the hosted service it refuses while a subscription is
  not cancelled.
- Both print what would go first, even when they refuse; a refusal exits
  with status 4.
- With mail, the address (or the organization's admins) is told it is done.
- Run it while the API runs: open drafts close and the people removed are
  disconnected at once.
- Backups still hold what was deleted until they expire (`backup.md`).
- With Zitadel as the identity provider, its event store still holds the
  deleted user's email address, names, password hash and the IP address
  and browser of each sign-in: Zitadel offers no way to erase them.
  [privacy.md](privacy.md) has a tested SQL script that blanks them, and
  what else stays.
- Emails stay in the database's mail outbox (`mail_outbox`: the address,
  the subject and the text, so names, addresses and business names) until
  it deletes them: 30 days after they were sent, 90 days after they were
  given up (3 days of failures). This covers operator notifications; a
  deleted account's emails already queued go with these delays too.

`ROMULUS_DELETE_INACTIVE_ACCOUNTS=true` also deletes accounts nobody used
for 3 years (no sign-in, no session, and none of their access tokens or the
API tokens they made used): the address is emailed 30 days before, and
signing in once keeps the account. Where such an account is an organization's only
admin, its most recently active developer (else reviewer) becomes admin
(written to the audit log, and emailed); an account alone in an
organization takes the organization with it. Leave it off unless your
policy says otherwise: off, romulus never deletes an account on its own.

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
