# Harden a romulus install

What to check before people rely on it. The defaults already do most of it;
this page says what they are and what is left to you.

## TLS at the proxy

Serve romulus over https only. The bundled Caddy gets and renews the
certificate and sends HSTS; with your own proxy, do the same
([install.md](install.md#your-own-reverse-proxy)). Session cookies are
`Secure` only when `ROMULUS_PUBLIC_URL` is https, so an http URL is for a
trial on `localhost`, never for a public address.

## The API stays on loopback

The API's port is published on `127.0.0.1` (`ROMULUS_BIND`): only a proxy
on the same host reaches it. Do not set `ROMULUS_BIND=0.0.0.0` unless
something else terminates TLS in front of it and a firewall keeps everyone
else out.

Postgres and the sandboxes publish no port at all. To look at the database:

```bash
docker compose exec postgres psql -U romulus
```

## Firewall

Inbound, allow SSH and, with the bundled Caddy, 80 and 443. Nothing else.

Docker publishes ports through its own iptables rules, before `ufw` or
`firewalld` see the traffic: a port published on `0.0.0.0` is open whatever
`ufw` says. That is why the API is published on loopback, and why Caddy's
80 and 443 are the only public ones.

## Trusted proxies

romulus limits sign-in attempts and other requests per client address, and
logs it with sign-in events. Behind a proxy every request comes from the
proxy, so the API reads the client's address from `X-Forwarded-For`, but
only from the addresses in `ROMULUS_TRUSTED_PROXIES`. From anyone else, the
header is ignored: a client cannot pick its own address.

`install.sh` trusts exactly one address (`ROMULUS_PROXIES` in `.env`):

- **The bundled Caddy**: its fixed address on the compose network
  (`ROMULUS_CADDY_IP`, the subnet's `.10`).
- **A proxy on this host** (no Caddy, the API published on loopback): the
  compose network's gateway (the subnet's `.1`). Docker's userland proxy
  forwards loopback connections to the published port from there, and only
  processes on this host can reach a loopback port.
- **The API published beyond loopback** (`ROMULUS_BIND` other than
  `127.0.0.1`): nobody. Every request is counted under its own source
  address, and `X-Forwarded-For` is ignored.

Set `ROMULUS_TRUSTED_PROXIES` yourself (it wins over `ROMULUS_PROXIES`) only
for a proxy you run elsewhere: publish the API on an address it reaches,
firewall that port to the proxy alone, and name the proxy's address.

- **Rootless Docker**, or a daemon with `"userland-proxy": false`, forwards
  published ports from another source than the gateway (rootlesskit's own
  address, or the client's after NAT). The gateway is then never a
  client's address, so nothing is spoofable, but every request may seem to
  come from one address and per-client limits act as one global limit. Look
  at `romulus-api auth-events`: if every `ip` is the same, set
  `ROMULUS_TRUSTED_PROXIES` to that address only if no one but your proxy
  can connect from it.
- The proxy must replace `X-Forwarded-For` with the address it sees
  (`{remote_host}` in Caddy, `$remote_addr` in nginx), not append to what
  the client sent.
- Never name a client network (a LAN, a VPN range) or `0.0.0.0/0` (refused):
  anyone in it could choose their address, and every per-client limit would
  be theirs to dodge.

## Mail

**Turn mail on.** Configure SMTP ([configuration.md](configuration.md#mail-smtp);
`./install.sh` offers it) so that invitations, sign-up links and password
resets reach people. It is also how someone locked out by wrong passwords
gets back in on their own: a reset link works through any lock. Without
mail, only an admin can help them (`./install.sh admin-link EMAIL` makes a
one-time link to hand over, `romulus-api unlock EMAIL` lifts a lock).

Send from a domain your provider is set up for (SPF and DKIM), or messages
land in spam. With mail, an account is created only from a link sent to
its address; without it, from an invitation link, which anyone holding it
could use once: send invitation links only over channels you trust.

## Accounts and sign-in

- **Sign-up** is by invitation only (`ROMULUS_SIGNUP=invite`): an account
  is created from an invitation link, with the invited address. `open` lets
  anyone who reaches the URL create one; keep `invite` on anything public.
- **One organization**: creating another is refused (`ROMULUS_SINGLE_ORG`).
- **Passwords**: 12 to 128 characters, common and predictable ones refused,
  stored as Argon2id hashes (OWASP's parameters). Sessions are random tokens
  in an httpOnly cookie, stored hashed, valid a week from each use and never
  more than 30 days after sign-in. Setting a password from a link signs the
  account out everywhere; changing it signs out every other browser; the
  owner is emailed either way.
- **Nothing says who has an account**: a wrong password and an unknown
  address get the same answer, and so do sign-ups and "forgot password".
- **Lockout**: 10 wrong passwords in a row for one address lock it (an
  address with no account too): 15 minutes the first time in a day, 30
  minutes the second, an hour from the third on, never longer. When a lock
  runs out, a new series of 10 starts; a successful sign-in or a reset
  starts the day over. However many requests arrive at once, at most 10
  passwords are checked per lock. A browser that signed in to the account
  before is never locked out by someone else's guesses (a device cookie:
  its own failures count apart), and a reset link works through any lock
  (hence: turn mail on). The owner is emailed at the first lock of the
  day; an address locked three times in a day is logged as a warning
  (`docker compose logs api`). An admin clears a lock early:

  ```bash
  docker compose exec api romulus-api unlock person@example.com
  docker compose exec api romulus-api auth-events --email person@example.com   # what happened
  ```

  A locked-out admin is unlocked the same way, from the host.
- **Roles**: reviewers read and comment; developers edit drafts, versions,
  uploads and API tokens; admins manage members and settings. Give each
  person the least they need.
- **API tokens** (for CI pushes) are scoped to one database, shown once and
  stored hashed. Revoke the ones you no longer use in the database's
  settings.

## Secrets

`deploy/self-host/.env` holds every secret: mode 600, owned by the user who
ran `install.sh`. Keep that account's access tight, and its copies in your
backups encrypted.

| secret | rotate |
|---|---|
| `POSTGRES_PASSWORD` | `docker compose exec postgres psql -U romulus -c "ALTER ROLE romulus PASSWORD '<new>'"`, put the same value in `.env`, `./install.sh` |
| `SANDBOX_PASSWORD` | the same as `sandbox_admin`, in `sandbox` and each `sandbox-15/16/18` you run (one password for all) |
| `ROMULUS_PLAY_KEY` | delete its line in `.env`, `./install.sh`: a new one is generated |
| `ROMULUS_PLAY_SECRET` | delete every playground in the app first, then the same |
| `ROMULUS_SMTP_PASSWORD` | change it at the provider, then in `.env`, `./install.sh` |

## Load

Checking a password is deliberately slow and takes 19 MiB (Argon2id), so
the hashing pool is the guard on that cost: 4 checks at once, at most 64
waiting, each for at most 2 seconds. Only a pool that is actually full
answers 503 `busy`; there is no global requests-per-second cap that a few
clients could fill for everyone. The protections against guessing are per
client and per address: per client, 10 sign-in attempts at once, then one
every 3 seconds (an IPv6 client counts as its whole /48, so rotating
addresses or /64s gains nothing), and per address, the lockout above.
Sign-ups and resets: 5, then one every 20 seconds per client; at most 3
messages per address every 10 minutes, and 60, then one every 10 seconds,
in all.

## The Docker socket

Only `play-runner` mounts `/var/run/docker.sock`, and only with
playgrounds on. It has no network, a read-only root, and a fixed set of
operations ([playgrounds.md](playgrounds.md#how-they-are-isolated)).
Remember that access to Docker is root on the host: everyone in the
`docker` group can do anything.

## Backups off the host

Daily dumps land on the same disk as the database. Copy them and `.env`
elsewhere, encrypted, and restore one from time to time
([backup.md](backup.md)).

## Updates

- romulus: `./install.sh upgrade` when a release comes out (it backs up
  first; [upgrade.md](upgrade.md)). Security fixes are announced in the
  release notes; see SECURITY.md to report one.
- The host: its packages (`unattended-upgrades` on Debian and Ubuntu),
  Docker, and gVisor if you run playgrounds (both from apt).
- Caddy follows Caddy 2's releases on each upgrade.

## Logs

- `./install.sh logs api` (or `docker compose logs api` in
  `deploy/self-host`). The API logs JSON lines: requests, sign-ins, errors.
  The playground service logs ids and timings, never SQL.
- On disk: Docker's `json-file` driver, rotated at 10 MB, 5 files per
  container, under `/var/lib/docker/containers/<id>/`. Ship them elsewhere
  with your usual log collector if you keep logs longer.
- Sign-in events: `docker compose exec api romulus-api auth-events`.
- Who did what in the app (versions removed, members changed, approvals
  overridden): each database's **Activity**, the audit log, which cannot be
  edited.

## What romulus does not do (yet)

- **No check against breached-password lists** (only the common ones are
  refused): a password manager's generated passwords avoid the problem.
- **No multi-factor authentication** for password sign-in. If you need it
  now, sign in through your own identity provider with MFA
  (`ROMULUS_AUTH=oidc`, [configuration.md](configuration.md#sign-in-with-your-own-identity-provider)).
- No SAML or SCIM.
- No encryption at rest of its own: use disk encryption on the host.
- No automatic off-host backups.
- No high availability: one API instance, one database.
- No automatic updates.
