# Connecting an AI agent (MCP)

romulus speaks the [Model Context Protocol](https://modelcontextprotocol.io):
an AI agent (Claude Code, Claude Desktop, Cursor, VS Code, or any MCP
client) can work in romulus for you. It reads your organizations' schemas,
versions and diffs, brings a database in from SQL or from your repository's
migration files, drafts a change, generates its migration with every risky
statement flagged, tests it on generated rows, writes the migration into your
repository, queries a playground, and asks for a review, with the same rules
as the web app.

There is nothing to install: the endpoint is part of romulus, at
`/api/mcp` under the address you open romulus at (a self-hosted install has
it too). Your agent acts as you. It connects one of two ways:

- **By signing in** (recommended): you add romulus to the agent with its URL
  only; the agent opens your browser on romulus, you sign in as usual and
  approve it. This is MCP's authorization flow (OAuth 2.1), which Claude
  Code, claude.ai, Claude Desktop, Cursor, VS Code and other clients speak.
- **With a personal access token**, for CI, scripts and agents that cannot
  open a browser.

## Connect

With `https://romulus.example.com` standing for your romulus's address (your
account menu, **Connect your agent**, shows these with it filled in):

**Claude Code**

```bash
claude mcp add --transport http romulus https://romulus.example.com/api/mcp
```

Then, in Claude Code, type `/mcp`, choose romulus and **Authenticate**: your
browser opens romulus to approve it. Add `--scope user` to the command to
have romulus in every project.

**Claude** (claude.ai, Claude Desktop): Settings, Connectors, **Add custom
connector**. Name it romulus, paste `https://romulus.example.com/api/mcp`,
then Connect, and approve it in romulus. Claude reaches romulus from
Anthropic's servers: this works with a romulus that is reachable from the
internet.

**Cursor** (`~/.cursor/mcp.json`, or `.cursor/mcp.json` in a project):

```json
{
  "mcpServers": {
    "romulus": { "url": "https://romulus.example.com/api/mcp" }
  }
}
```

Cursor then shows romulus as needing a login: click it to approve it.

**Any other client**: URL `https://romulus.example.com/api/mcp`, transport
Streamable HTTP. The client finds how to sign in from the URL.

### Approving an app

The agent opens romulus in your browser (signing in first if needed), which
asks whether to allow it. The page shows the app's name, as the app gave it,
and where it sends you back: `claude.ai`, or `localhost` for a program on your computer, which is
the part romulus can vouch for. Choose what it may do (read and write, as
your role allows, or read only), then **Allow** or **Deny**. Only allow an
app you just started connecting: a page asking out of the blue is someone
else's. romulus emails you when an app is connected (when your romulus sends
mail).

Then ask it, for instance: "What databases do we keep in romulus, and what
changed in the last version?"

Your apps are listed under **Connected apps** in your account menu, with
their access and when they were last used. **Disconnect** cuts one off at
once.

### With an access token

For CI, scripts and agents that cannot open a browser: on **Connect your
agent**, open **Advanced: connect with an access token** (or **Access
tokens** in your account menu). Create a token: a name (which agent, on which
computer), its access (read and write, or read only) and when it expires (90
days unless you choose 30, 365 or never). It is shown once, and romulus
emails you that it was created.

**Claude Code**

```bash
read -rs ROMULUS_TOKEN
claude mcp add --transport http romulus https://romulus.example.com/api/mcp \
  --header "Authorization: Bearer $ROMULUS_TOKEN"
```

The first line waits for you to paste the token and does not show it: a
token typed into a command would stay in your shell's history.

**Claude Desktop** (Settings, Developer, Edit config; it needs Node.js for
the `mcp-remote` bridge):

```json
{
  "mcpServers": {
    "romulus": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "https://romulus.example.com/api/mcp",
               "--header", "Authorization:${ROMULUS_AUTH}"],
      "env": { "ROMULUS_AUTH": "Bearer rmls_pat_…" }
    }
  }
}
```

**Cursor**:

```json
{
  "mcpServers": {
    "romulus": {
      "url": "https://romulus.example.com/api/mcp",
      "headers": { "Authorization": "Bearer rmls_pat_…" }
    }
  }
}
```

**Any other client**: header `Authorization: Bearer rmls_pat_…`.

**Test connection** on the page checks the endpoint and the token from your
browser, and the page says when your agent's first request arrives. Your
tokens are listed under **Access tokens**, with when each was last used and
by which client.

## Tools

Tools that change shared state say so in their description, and a read-only
token is offered only the others.

| tool | what it does | changes shared state |
|---|---|---|
| `whoami` | you, the token's access, your organizations and roles | |
| `list_databases` | an organization's databases, their Postgres major and latest version | |
| `get_database` | one database: its latest version, open drafts, what publishing needs | |
| `get_schema` | a version's schema: SQL, a compact summary per table, or the remus JSON | |
| `list_versions` | the versions, newest first | |
| `diff_versions` | what changed between two versions, with each object's SQL before and after | |
| `search_schema` | tables, columns and enums by name or comment, across databases | |
| `create_draft` | a draft from the latest version (or an older version's SQL), with your SQL or edits | yes |
| `update_draft` | the draft's text, as a whole or as exact edits; live for whoever has it open | yes |
| `get_draft` | the draft's text, its compile problems, review state and generated migration | |
| `generate_migration` | a draft's migration, or between two versions, as a tool's files (plain, sqlx, Flyway, Prisma, dbmate, golang-migrate), renames detected | |
| `confirm_renames` | keeps, in a draft, which detected renames its migration applies | yes |
| `test_migration` | runs a migration up and down on generated rows (or a copy of a playground's), and reports | yes |
| `get_migration_test` | a test's progress, then its verdict and the data it found lost | |
| `request_review` | marks a draft ready for review, or takes that back | yes |
| `publish_draft` | publishes a draft as the next version, exactly as reviewed, with the organization's rules | yes |
| `create_playground` | a real Postgres with a version's schema and generated rows | yes |
| `playground_query` | read-only SQL on a playground, as the console runs it | |
| `list_playgrounds` | a database's playgrounds | |
| `create_database` | a new database in an organization | yes |
| `push_schema` | a new database's first version from SQL (`pg_dump --schema-only`, remus's SQL); refused once it has one | yes |
| `ci_setup` | how to push from CI with remus, filled in for a database, with the link to create its token | |
| `import_migrations` | a repository's migration files as the database's history, one version per migration, as the web import does; the agent reads the files and sends them. Where changes are reviewed, only into a database without versions | yes |
| `get_import` | an import's progress, then the versions it created, the statements it skipped and the files it ignored, or where it stopped | |
| `get_migration_files` | a draft's migration, or between two versions, as the files a migration tool expects, exactly as the web app downloads them, with where they go | |

Long work never holds a request open: `test_migration`,
`create_playground` and `import_migrations` wait 40 seconds at most, then
answer an id to poll (`get_migration_test`, `list_playgrounds`,
`get_import`).

**Prompts** (a client may offer them as commands): `onboard_database`,
`review_draft`, `plan_safe_migration`, `import_migrations_folder` (find the
repository's migrations, import them, summarize) and `write_draft_migration`
(a draft's migration written into the repository, named like the files
there).

**Resources**: each database's latest version as SQL,
`romulus://<org>/<database>/v<n>` (any version by the same template), the
CI guide, `romulus://guide/ci`, and the exporters of migrations written in
code, `romulus://export-migrations/<alembic|django|rails>.sh`.

## Migration files and your repository

romulus's MCP server runs on romulus, not on your computer: it cannot read
your disk, and it writes nothing there. The agent does both, with its own
tools, and the romulus tools say so.

**Importing a migrations folder** ("import my migrations folder", or the
`import_migrations_folder` prompt). The agent finds the folder, reads every
file and sends each one's path, relative to the repository, and its content
to `import_migrations`. romulus imports them exactly as the web app's
**Import migrations** does: the tool is detected from the files (Flyway,
Prisma, sqlx, dbmate, golang-migrate, node-pg-migrate, plain numbered SQL
files) unless the agent names it; each migration becomes a version, its
message the file name; data and session statements (`INSERT`, `SET`…) are
skipped and listed per file; migrations imported before are skipped, a
changed one is refused; a database with versions that were not imported
needs `append` (the agent asks you first). Every rule and limit of an import
applies: your role (developers and admins), your plan's version cap, one
import of a database at a time.

In an organization that reviews changes (approvals, or a passing migration
test, required before publishing), an agent's import only gives a database
without versions its history: that is how a repository is brought in. Once
the database has versions, the import is refused (`review_required`) and the
agent writes the change as a draft instead (`create_draft` in migration
mode), so approvals and tests apply as they do to everyone. A dry run, which
stores nothing, is always allowed, and your own import in the web app is not
affected.

- A message to the endpoint carries 4 MiB at most. Larger folders go over
  several calls: the first answers an upload id, the next add files to it,
  the last imports them (`commit`). A file larger than a call goes in parts
  (10 at most). An upload is yours alone (3 in progress at most) and expires
  an hour after its last files, and a day after it started whatever
  happens; sending a file again replaces it, so a retried call changes
  nothing. An upload holds what one import reads: 5,000 files, 8 MiB each,
  32 MiB in all, paths included. Paths are relative, 512 bytes at most,
  without invisible formatting characters (a bidi override or a zero-width
  space would make a path read as another).
- The import runs on after the call: `import_migrations` waits up to 40
  seconds, then answers the import's id for `get_import`. The answer lists
  the versions created, the statements skipped and the files ignored.
- When an import is refused or a migration fails, the files stay uploaded
  for an hour: the agent fixes the file named (sends it again) and imports
  again, without sending the rest.
- Rails, Django and Alembic migrations are code, which romulus never runs:
  the agent imports the SQL the tool prints, with the exporter scripts
  (`scripts/export-migrations`, served as the resources above). It asks you
  first: Django's and Rails' apply each migration to an empty scratch
  database you give it.
- The Activity shows each import an agent started, with its token; the
  versions show who imported them.

**Writing a migration into the repository** ("write this draft's migration
into my repo", or the `write_draft_migration` prompt). The agent finds the
repository's migration tool and folder, then asks `get_migration_files` for
the draft's migration (or one between two versions) in that tool's format.
The files are the web app's download, byte for byte: same names, same
content, same down migration where the tool has one. The answer says where
the tool expects them:

| format | files | where |
|---|---|---|
| `plain` | `<version>_<name>.sql`, in a transaction | wherever the project keeps SQL |
| `sqlx` | `<version>_<name>.sql` | `migrations/` |
| `flyway` | `V<n>__<name>.sql` (a `.sql.conf` for one run outside a transaction) | `src/main/resources/db/migration/`, or the folder `flyway.locations` names |
| `prisma` | `<timestamp>_<name>/migration.sql` | `prisma/migrations/` |
| `dbmate` | `<version>_<name>.sql`, up and down sections | `db/migrations/` |
| `golang_migrate` | `<version>_<name>.up.sql` and `.down.sql` | the folder given to `migrate -path` |

The agent passes `version` when the repository numbers its files its own way
(Flyway's next `V<n>`, a golang-migrate sequence), so the new files sort
after the existing ones. For Rails, Django and Alembic it takes `plain` and
writes a migration of that tool around the SQL. A long migration comes in
pages, each naming its files as the first did; a draft edited between two
pages makes the agent start again. Each file sits between two marker lines
carrying a code drawn for that answer, and its header gives its length in
bytes: a line inside a file (a table comment someone wrote, say) cannot pass
for the end of it or the start of another, and the agent checks it wrote
exactly that many bytes. It writes only to plain relative paths under the
migrations folder, and stops to tell you otherwise. Nothing is run against
a database, and nothing is committed, unless you ask.

## What an agent can do

Whether it connected by signing in or with a token:

- **It acts as you.** Every tool calls the same handlers the web app calls,
  as you: your role in each organization decides (reviewers read and
  comment, developers change schemas, admins also manage the organization),
  and so do the rate limits and plan limits that apply to you. A tool that
  your role does not allow answers so, and says who can.
- **Read-only access never writes**, whatever your role: a read-only token
  or app is offered the reading tools only, and the identity it lends
  refuses any change. Querying a playground is a read, as in the console. A
  read-only app that calls a write tool anyway is answered
  `insufficient_scope`, so its client can ask you for more.
- **Nothing bypasses the organization's rules.** There is no tool to
  approve a draft, and `publish_draft` publishes only what passes the
  approvals and the migration test the organization requires; when one
  holds, it says what is missing. Edits through `update_draft` are your
  edits: approvals of the earlier text stop counting, and yours never
  counts for that draft. `push_schema` only gives a database with no
  version its first one: every later version comes from a draft.
- **No CI secrets through the agent.** CI's API token is created by you in
  the database's settings (`ci_setup` links to them); an agent never sees
  it.
- **Every write is in the Activity** of the database it touched (its
  settings page, for developers and admins), with your name and the token's
  or the app's.
- **Cutting off is immediate**: Disconnect in Connected apps, or Access
  tokens' trash button. An expired token is refused with a message saying
  so. Resetting or changing your password revokes all your tokens and
  disconnects all your apps, as it signs you out everywhere else; deleting
  your account removes them.
- **You hear of every new token and app**: romulus emails you its name and
  access, with the link to revoke or disconnect it.
- **Only hashes are stored.** Personal tokens start with `rmls_pat_`, an
  app's access tokens with `rmls_oat_` and its refresh tokens with
  `rmls_ort_`, so secret scanners can recognize them in a commit; revoke one
  if they do.
- **Only at `/api/mcp`.** Neither kind of token opens anything else in the
  API, and the endpoint never accepts the browser's session cookie. A
  request with an `Origin` header other than romulus's own is refused (a web
  page cannot drive it from someone's browser).

## Self-hosted installs

Nothing to enable: the endpoint is `https://<your romulus>/api/mcp`, and
signing in works with your install's own sign-in (passwords, or single
sign-on). Behind your own reverse proxy:

- send `/.well-known/oauth-protected-resource/api/mcp` and
  `/.well-known/oauth-authorization-server` to the API, as well as `/app`
  and `/api`: that is where MCP clients find how to sign in. The bundle's Caddy and the Helm chart's Ingress send every path
  to the API already;
- pass the `Authorization` header through, allow requests of up to a
  minute (`test_migration`, `create_playground` and `import_migrations`
  wait up to 40 seconds) and bodies of 4 MiB (migration files arrive in
  chunks of up to that).

`ROMULUS_PUBLIC_URL` is what tokens are issued for: after changing it,
connected apps connect again. Tools that need playgrounds answer that
playgrounds are not available when your install runs without them. Claude's
connectors (claude.ai, Claude Desktop) reach romulus from Anthropic's
servers, so they need an install reachable from the internet; Claude Code,
Cursor and other local clients do not.

## The protocol

- Streamable HTTP, one JSON-RPC message per `POST /api/mcp`, answered with
  one JSON object. `GET` and `DELETE` answer 405: there is no stream and no
  session. Batches are refused.
- Protocol versions: 2026-07-28 (per-request metadata, `server/discover`,
  the `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers checked),
  and 2025-11-25, 2025-06-18 and 2025-03-26 after an `initialize`. Every
  request stands alone, so a restart of romulus never breaks a client.
- Requests are limited per token (a burst of 60, then 2 a second) and per
  person across their tokens (a burst of 120, then 4 a second). A client
  sending tokens that are not valid is refused for a while after 20 of
  them.
- The endpoint is written in romulus itself (`crates/romulus-api/src/mcp`)
  rather than with an SDK: it needs no streams, sessions or server-initiated
  requests, and it has to sit inside the API's own authentication, error
  format and limits.

### Signing in (OAuth)

romulus is the authorization server of its own MCP endpoint (MCP's
authorization spec, OAuth 2.1); your sign-in itself stays the usual one.

| what | where |
|---|---|
| protected resource metadata (RFC 9728) | `/.well-known/oauth-protected-resource/api/mcp` (the endpoint's own path; none at the root, which would describe the whole origin); `/api/mcp` answers `401` with `WWW-Authenticate: Bearer resource_metadata="…"` |
| authorization server metadata (RFC 8414) | `/.well-known/oauth-authorization-server`; the issuer is romulus's address |
| client registration (RFC 7591) | `POST /api/oauth/register`: open, limited per client address; public clients only (no secret) |
| authorization | `GET /api/oauth/authorize`, then the consent page `/app/oauth/authorize`; a request romulus refuses stays on that page, which offers to return to the app (`GET /api/oauth/refusals` builds the way back) |
| tokens | `POST /api/oauth/token`: `authorization_code`, `refresh_token` |
| revocation (RFC 7009) | `POST /api/oauth/revoke` |

- **Redirect URIs**: https, or http on a loopback address (`127.0.0.1`,
  `[::1]`, `localhost`) on any port (RFC 8252), such as Claude Code's
  `http://localhost:<port>/callback` and claude.ai's
  `https://claude.ai/api/mcp/auth_callback`. Custom schemes are refused.
- **Requests** need PKCE with `S256` (`plain` is refused), and a
  `resource`, when given, of `https://<your romulus>/api/mcp` (RFC 8707):
  tokens are for the MCP endpoint only. `state` is optional (PKCE protects
  the flow) and comes back unchanged when sent. Responses carry `iss` (RFC
  9207).
- **Scopes**: `romulus.read` (a read-only token's access) and
  `romulus.write` (read and write); none asked means both, and you may allow
  read only. A refresh may ask for less, for the access token it gets (the
  connection keeps what you allowed), never for more.
- **Lifetimes**: codes 60 seconds, once; access tokens an hour; refresh
  tokens rotate on every use and last 30 days unused, the connection 90 days
  at most. A code or a refresh token used twice revokes everything the
  connection holds. Clients nobody used for 30 days are deleted (a client
  told `invalid_client` registers again).

## When it does not work

| what you see | why |
|---|---|
| 401, "not valid" | the token was revoked or the app disconnected (a password reset or change does both), mistyped, or is not romulus's (`rmls_pat_`, `rmls_oat_`) |
| 401, "expired" | an app refreshes its token by itself; a personal token: create a new one |
| 401, "issued for another resource" | romulus's address changed since the app connected: connect it again |
| "This request is over" in the browser | the request to connect expired (20 minutes), was answered, or was opened in another browser: connect again from the agent |
| "romulus does not know this app" | its registration expired: connect again, it registers anew |
| "romulus can't accept this request" | the app asked in a way romulus refuses (no PKCE `S256`, an unknown scope, another resource): **Return to** sends it the reason; tell its maker |
| 403, `insufficient_scope` | the app was allowed to read only: connect it again with read and write |
| 403 | the request came with another site's `Origin` |
| 413 | one message was over 4 MiB: the agent sends fewer migration files per call, or a large file in parts |
| 429 | too many requests (with this token, with all your tokens, or with tokens that are not valid): wait the `Retry-After` seconds |
| 500, "could not check the access token" | romulus could not reach its database for a moment: try again; the token is fine |
| a tool answers "forbidden" | your role in that organization does not allow it |
| a tool answers "read_only_token" | the token is read-only: create one with write access |
| a tool answers "upload_expired" | migration files sent more than an hour before they were imported (or an upload started more than a day ago): the agent sends them again |
| a tool answers "review_required" | the organization reviews changes and the database has versions: the agent writes the change as a draft (`create_draft` in migration mode) for review, instead of importing it |
| a tool answers "plan_limit" | the organization's plan does not allow more (databases, versions, playground hours…): the answer says which plan does; an admin changes it on the organization's Billing page |
