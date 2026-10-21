# Connecting an AI agent (MCP)

romulus speaks the [Model Context Protocol](https://modelcontextprotocol.io):
an AI agent (Claude Code, Claude Desktop, Cursor, VS Code, or any MCP
client) can work in romulus for you. It reads your organizations' schemas,
versions and diffs, brings a database in from SQL, drafts a change, generates
its migration with every risky statement flagged, tests it on generated rows,
queries a playground, and asks for a review, with the same rules as the web
app.

There is nothing to install: the endpoint is part of romulus, at
`/api/mcp` under the address you open romulus at (a self-hosted install has
it too). The agent authenticates with a **personal access token** you
create, and acts as you.

## Connect

1. In romulus, open your account menu (top right), **Connect your agent**.
   Create a token there: a name (which agent, on which computer), its
   access (read and write, or read only) and when it expires (30, 90 or 365
   days, or never). The page fills the token into the configuration below;
   it is shown once.
2. Add romulus to your agent. With `https://romulus.example.com` standing for
   your romulus's address:

   **Claude Code**

   ```bash
   claude mcp add --transport http romulus https://romulus.example.com/api/mcp \
     --header "Authorization: Bearer rmls_pat_…"
   ```

   Add `--scope user` to have it in every project. `claude mcp list` shows
   it connected.

   **Claude Desktop** (Settings, Developer, Edit config; it needs Node.js
   for the `mcp-remote` bridge):

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

   **Cursor** (`~/.cursor/mcp.json`, or `.cursor/mcp.json` in a project):

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

   **Any other client**: URL `https://romulus.example.com/api/mcp`,
   transport Streamable HTTP, header `Authorization: Bearer rmls_pat_…`.

3. Back on the page, **Test connection** checks the endpoint and the token
   from your browser, and the page says when your agent's first request
   arrives. Then ask it, for instance: "What databases do we keep in romulus,
   and what changed in the last version?"

Your tokens are listed under **Access tokens** in the same menu, with when
each was last used and by which client.

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
| `push_schema` | the next version from SQL (`pg_dump --schema-only`, remus's SQL) | yes |
| `create_push_token` | an API token for CI to push versions, shown once | yes |
| `ci_setup` | how to push from CI with remus, filled in for a database | |

Long work never holds a request open: `test_migration` and
`create_playground` wait 40 seconds at most, then answer an id to poll
(`get_migration_test`, `list_playgrounds`).

**Prompts** (a client may offer them as commands): `onboard_database`,
`review_draft`, `plan_safe_migration`.

**Resources**: each database's latest version as SQL,
`romulus://<org>/<database>/v<n>` (any version by the same template), and
the CI guide, `romulus://guide/ci`.

## What a token can do

- **It acts as you.** Every tool calls the same handlers the web app calls,
  as you: your role in each organization decides (reviewers read and
  comment, developers change schemas, admins also manage the organization),
  and so do the rate limits and plan limits that apply to you. A tool that
  your role does not allow answers so, and says who can.
- **Read-only tokens never write**, whatever your role: they are offered the
  reading tools only, and the identity they lend refuses any change.
  Querying a playground is a read, as in the console.
- **Nothing bypasses the organization's rules.** There is no tool to
  approve a draft, and `publish_draft` publishes only what passes the
  approvals and the migration test the organization requires; when one
  holds, it says what is missing. Edits through `update_draft` are your
  edits: approvals of the earlier text stop counting, and yours never
  counts for that draft.
- **Every write is in the Activity** of the database it touched (its
  settings page, for developers and admins), with your name and the token's.
- **Revoking is immediate**: Access tokens, the trash button. An expired
  token is refused with a message saying so.
- **Only a hash is stored.** The secret starts with `rmls_pat_`, so secret
  scanners can recognize it in a commit; revoke it if one does.
- **Only at `/api/mcp`.** A personal token opens nothing else in the API,
  and the endpoint never accepts the browser's session cookie. A request
  with an `Origin` header other than romulus's own is refused (a web page
  cannot drive it from someone's browser).

## Self-hosted installs

Nothing to enable: the endpoint is `https://<your romulus>/api/mcp`. Behind
your own reverse proxy, pass the `Authorization` header through and allow
requests of up to a minute (`test_migration` and `create_playground` wait
up to 40 seconds). Tools that need playgrounds answer that playgrounds are
not available when your install runs without them.

## The protocol

- Streamable HTTP, one JSON-RPC message per `POST /api/mcp`, answered with
  one JSON object. `GET` and `DELETE` answer 405: there is no stream and no
  session. Batches are refused.
- Protocol versions: 2026-07-28 (per-request metadata, `server/discover`,
  the `MCP-Protocol-Version`, `Mcp-Method` and `Mcp-Name` headers checked),
  and 2025-11-25, 2025-06-18 and 2025-03-26 after an `initialize`. Every
  request stands alone, so a restart of romulus never breaks a client.
- Requests are limited per token (a burst of 60, then 2 a second).
- The endpoint is written in romulus itself (`crates/romulus-api/src/mcp`)
  rather than with an SDK: it needs no streams, sessions or server-initiated
  requests, and it has to sit inside the API's own authentication, error
  format and limits.

**Not yet: OAuth.** MCP's authorization flow would let a client connect by
signing in, without pasting a token (claude.ai's custom connectors work that
way). The endpoint already answers `401` with a `WWW-Authenticate: Bearer`
challenge; an authorization server issuing tokens for a person would add its
metadata there and resolve them to the same identity personal tokens do.

## When it does not work

| what you see | why |
|---|---|
| 401, "not valid" | the token was revoked, mistyped, or is not a personal one (`rmls_pat_`) |
| 401, "expired" | create a new token |
| 403 | the request came with another site's `Origin` |
| 429 | too many requests with this token: wait the `Retry-After` seconds |
| a tool answers "forbidden" | your role in that organization does not allow it |
| a tool answers "read_only_token" | the token is read-only: create one with write access |
