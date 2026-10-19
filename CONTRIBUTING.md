# Contributing to romulus

romulus keeps an organization's Postgres schemas: versions, diffs, reviews
and the next version designed together (SQL and table graph, live). The
schema model comes from remus (`remus-core`), a separate project.

Thank you for helping. This file says how to build and test romulus, what a
pull request needs, and the terms contributions are made under.

## Before you start

- **Read [CLAUDE.md](CLAUDE.md).** It is the repository's conventions file,
  for people and coding agents alike: the repo map, the decisions already
  made, API, SQL and web conventions, testing, pitfalls.
- **Read the spec** ([docs/MVP.md](docs/MVP.md)) before a feature. Changing a
  decision listed there means changing MVP.md first, with a reason.
- **Open an issue first** for anything larger than a fix, so the design is
  agreed before the code.
- **Security issues never go in public issues**: see [SECURITY.md](SECURITY.md).
- Everyone taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Build and test

Prerequisites: rustup, Docker, Node 24 with npm, [`just`](https://github.com/casey/just),
sqlx-cli and the pinned nightly rustfmt:

```bash
cargo install sqlx-cli --version 0.9.0 --locked --no-default-features --features postgres,rustls
rustup toolchain install nightly-2026-10-14 --profile minimal --component rustfmt
```

The nightly's date lives in the justfile and CI; use the one the justfile
names.

```bash
just up        # writes .env if missing, then starts the stack (Postgres, sandboxes, Zitadel, mail)
just dev       # up, then the API and the web app; open $ROMULUS_PUBLIC_URL/app/
just check     # the CI gate: fmt, clippy, tests, sqlx prepare --check, gen-check, web, build
just e2e       # Playwright: production build and its own API (needs `just up`)
just e2e-dev   # Playwright on the dev-only screens: no stack needed
just down      # stop the stack; `just nuke` also deletes its data
```

Test users: `alice@romulus.test` and `bob@romulus.test`, password
`RomulusDev1!` (local stack only). The local stack also has romulus's own
email + password sign-in (the self-hosted one): its sign-up and reset links
land in the mail catcher, `http://localhost:$ROMULUS_MAIL_PORT`. CLAUDE.md,
"Run it", lists the other recipes (other Postgres majors' sandboxes,
playgrounds, the homepage).

**Ports are per worktree.** Several clones or worktrees can run their own
stacks at once, so nothing uses a fixed port. `scripts/dev-env` gives each
checkout a slot and writes its ports into `.env` (never copy `.env` between
checkouts). Read ports from the `ROMULUS_*_PORT` variables, never write a
port number in code. CLAUDE.md, "Ports and worktrees", has the table.

## Pull requests

- **Tests come with the change.** Every endpoint: its happy path, a permission
  refused for each role below the minimum, validation errors. Every screen:
  at least one Playwright path. Logic: unit tests next to the code.
- **`just check` is green** before you push. CI runs the same gate.
- **After an API change, run `just gen`** and commit the regenerated
  `web/src/api/openapi.json` and `schema.gen.ts`. Never hand-write an API type.
- **After a query or a migration change**, run `cargo sqlx prepare --workspace
  -- --all-targets` and commit `.sqlx/`. Never edit an applied migration.
- **The homepage's live hero is a committed build**: if you change what it is
  made of, run `just site-hero` and commit the output.
- **Screens**: use the UI kit and tokens (CLAUDE.md, "Web conventions"), and
  check light and dark, at 1440 and 390 px wide.
- One topic per pull request; describe what changes and why, and how you
  tested it.

## Contributor License Agreement

Contributions are accepted under the [Contributor License Agreement](CLA.md),
based on the Apache Individual Contributor License Agreement. You keep the
copyright in your contribution; you grant the maintainers a license to use,
relicense and sublicense it, which keeps the open-core model (the `ee/`
directory, commercial licenses) possible while romulus stays available under
the AGPL.

**The CLA assistant will ask you on your first pull request.** It comments on
the pull request with a link to CLA.md and how to sign, and the pull request
cannot be merged until you have. You sign once; it covers your later
contributions. If your employer has rights to your work, make sure you may
contribute it first (CLA.md, section 4).

## Licensing and file headers

- Everything outside `ee/` is licensed under the GNU AGPL v3 only
  ([LICENSE](LICENSE)). Everything in `ee/` is licensed under
  [ee/LICENSE](ee/LICENSE). [NOTICE](NOTICE) names the copyright holders.
- **No per-file copyright or license headers.** Do not add them: the root
  LICENSE covers every file outside `ee/`, and `ee/LICENSE` covers `ee/`.
- **Files copied from elsewhere** under another license keep their original
  notice, and get an `SPDX-License-Identifier` line for that license if they
  have none. Say where the file comes from in the pull request. Only copy
  code whose license is compatible with the AGPL.
- Dependencies keep their own licenses; prefer permissive ones (MIT, Apache-2.0,
  BSD, ISC) and say so in the pull request when you add one.
