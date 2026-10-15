set dotenv-load := true
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

# write .env (this worktree's ports and project name) unless it exists
env:
    @[ -f .env ] || scripts/dev-env

# start the stack (databases + Zitadel, provisioned) and wait until it is ready
up: env
    #!/usr/bin/env bash
    set -euo pipefail
    # Created here so it belongs to you, not to the container (Linux bind mounts).
    mkdir -p .dev/zitadel
    docker compose up -d --wait || { docker compose logs zitadel-provision; exit 1; }
    # --wait returns once the one-shot provisioner is running, not once it is done.
    status=$(docker wait "$(docker compose ps -aq zitadel-provision)")
    if [ "$status" != 0 ]; then
        docker compose logs zitadel-provision
        echo "up: zitadel-provision failed (exit $status)" >&2
        exit 1
    fi
    # Not $ROMULUS_ZITADEL_PORT: on a first run just loaded no .env yet.
    port=$(sed -n 's/^ROMULUS_ZITADEL_PORT=//p' .env)
    echo "up: ready. Zitadel http://localhost:$port, client in .dev/zitadel/oidc.env, users in .dev/zitadel/users.json"

# stop the stack, keep its data
down:
    docker compose down --remove-orphans

# stop the stack and delete its data and the provisioned credentials
nuke:
    docker compose down -v --remove-orphans
    rm -rf .dev

# apply migrations to DATABASE_URL (the API also applies them on start)
migrate:
    cargo run -q -p romulus-api -- migrate

# the stack, the API and the web dev server; ctrl-c stops the API and web
dev: up
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT
    cargo run -p romulus-api &
    npm --prefix web run dev &
    wait

# regenerate the web app's API types from the Rust OpenAPI document
gen:
    cargo run -q -p romulus-api -- openapi > web/src/api/openapi.json
    npm --prefix web run gen

# the full gate, as CI runs it (needs DATABASE_URL on a migrated database)
check:
    cargo +nightly fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace
    cargo sqlx prepare --workspace --check -- --all-targets
    npm --prefix web run typecheck
    npm --prefix web run lint
    npm --prefix web run format:check
    npm --prefix web run test
    npm --prefix web run build

# rustfmt needs nightly for imports_granularity / group_imports (rustfmt.toml)
fmt:
    cargo +nightly fmt --all
    npm --prefix web run format

# Playwright end-to-end tests against a running stack (`just up` first)
e2e:
    npm --prefix web run e2e
