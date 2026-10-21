set dotenv-load := true
set shell := ["bash", "-euo", "pipefail", "-c"]

# This worktree's compose project, derived from its path, never read from .env
# (scripts/dev-env): a copied .env cannot aim `nuke` at another stack.
compose := "docker compose -p " + `scripts/dev-env --project-name`
# rustfmt.toml needs nightly rustfmt; pinned so formatting never drifts.
nightly := "nightly-2026-10-14"

default:
    @just --list

# write .env: this worktree's ports and project (idempotent, keeps the slot)
env:
    @scripts/dev-env >/dev/null

# start the stack (databases + Zitadel, provisioned) and wait until it is ready
up: env
    #!/usr/bin/env bash
    set -euo pipefail
    # Created here so it belongs to you, not to the container (Linux bind mounts).
    mkdir -p .dev/zitadel
    {{compose}} up -d --wait || { {{compose}} logs zitadel-provision; exit 1; }
    # The playground image (deploy/playground), rebuilt only when its files changed.
    just playground-image
    # --wait returns once the one-shot provisioner is running, not once it is done.
    status=$(docker wait "$({{compose}} ps -aq zitadel-provision)")
    if [ "$status" != 0 ]; then
        {{compose}} logs zitadel-provision
        echo "up: zitadel-provision failed (exit $status)" >&2
        exit 1
    fi
    # Not $ROMULUS_ZITADEL_PORT: on a first run just loaded no .env yet.
    port=$(sed -n 's/^ROMULUS_ZITADEL_PORT=//p' .env)
    echo "up: ready. Zitadel http://localhost:$port, client in .dev/zitadel/oidc.env, users in .dev/zitadel/users.json"

# a sandbox Postgres (no Zitadel): the default major's (17), another's (`just sandbox 15`), or all of them (`just sandbox all`)
sandbox major="17": env
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{major}}" in
        17) {{compose}} up -d --build --wait sandbox-postgres ;;
        15 | 16 | 18) {{compose}} --profile pg{{major}} up -d --build --wait sandbox-postgres-{{major}} ;;
        all) {{compose}} --profile majors up -d --build --wait sandbox-postgres sandbox-postgres-15 sandbox-postgres-16 sandbox-postgres-18 ;;
        *) echo "sandbox: 15, 16, 17, 18 or all, not {{major}}" >&2; exit 1 ;;
    esac

# stop the stack (every major's sandbox included), keep its data (playgrounds
# too: they are not compose's)
down:
    {{compose}} --profile majors down --remove-orphans
    -docker ps -q --filter "label=romulus.play.scope=$(scripts/dev-env --project-name)" | xargs docker stop >/dev/null 2>&1

# stop the stack and delete its data and the provisioned credentials
nuke:
    {{compose}} --profile majors down -v --remove-orphans
    -docker ps -aq --filter "label=romulus.play.scope=$(scripts/dev-env --project-name)" | xargs docker rm -f >/dev/null 2>&1
    -docker volume rm "$(scripts/dev-env --project-name)_play" >/dev/null 2>&1
    rm -rf .dev

# apply migrations to DATABASE_URL (the API also applies them on start)
migrate:
    cargo run -q -p romulus-api -- migrate

# the stack, the API, the playground runner and service, and the web dev server; ctrl-c stops them
dev: up
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT
    cargo build -q -p romulus-play-runner -p romulus-playground
    cargo run -q -p romulus-play-runner &
    cargo run -q -p romulus-playground &
    cargo run -p romulus-api &
    npm --prefix web run dev &
    wait

# only the playground runner and service (the API and web run elsewhere); needs `just up`
play:
    #!/usr/bin/env bash
    set -euo pipefail
    trap 'kill 0' EXIT
    cargo build -q -p romulus-play-runner -p romulus-playground
    cargo run -q -p romulus-play-runner &
    cargo run -q -p romulus-playground &
    wait

# build the playground Postgres image (deploy/playground) as local development and tests use it
playground-image:
    #!/usr/bin/env bash
    set -euo pipefail
    # Labelled with a hash of its files: built again only when they change.
    hash=$(cd deploy/playground && find . -type f | LC_ALL=C sort | xargs shasum -a 256 | shasum -a 256 | cut -c1-16)
    built=$(docker image inspect -f '{{{{ index .Config.Labels "romulus.sources" }}}}' romulus-playground:dev 2>/dev/null || true)
    if [ "$built" != "$hash" ]; then
        docker build -q --label "romulus.sources=$hash" -t romulus-playground:dev deploy/playground >/dev/null
    fi

# regenerate the web app's API types from the Rust OpenAPI document
gen:
    cargo run -q -p romulus-api -- openapi > web/src/api/openapi.json
    npm --prefix web run gen

# fail when the committed web API types lag behind the Rust OpenAPI document
gen-check:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    cargo run -q -p romulus-api -- openapi > "$tmp/openapi.json"
    cd web
    npx openapi-typescript "$tmp/openapi.json" -o "$tmp/schema.gen.ts" >/dev/null 2>&1
    npx prettier --stdin-filepath src/api/schema.gen.ts < "$tmp/schema.gen.ts" > "$tmp/schema.fmt.ts"
    if ! diff -q "$tmp/openapi.json" src/api/openapi.json >/dev/null || ! diff -q "$tmp/schema.fmt.ts" src/api/schema.gen.ts >/dev/null; then
        echo "gen-check: web/src/api is stale, run \`just gen\` and commit" >&2
        exit 1
    fi

# Stripe's side of hosted billing from the plan table (crates/romulus-api/src/billing/setup.rs):
# products, prices, the Customer Portal; idempotent. Reads STRIPE_SECRET_KEY, refuses a live key
# unless --live. E.g. `just stripe-setup --public-url https://romulus.example --terms-url …`
stripe-setup *args:
    cargo run -q -p romulus-api -- stripe-setup {{args}}

# the full gate, as CI runs it (needs DATABASE_URL on a migrated database)
check:
    cargo +{{nightly}} fmt --all --check
    # --all-features: the `utoipa` features of romulus-diff and romulus-sandbox
    # (and their tests) included; the feature-off builds are linted too.
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo clippy -p romulus-diff -p romulus-sandbox -p romulus-migration --all-targets -- -D warnings
    cargo test --workspace --all-features
    cargo sqlx prepare --workspace --check -- --all-targets
    just gen-check
    npm --prefix web run typecheck
    npm --prefix web run lint
    npm --prefix web run format:check
    npm --prefix web run zitadel:assets -- --check
    npm --prefix web run test
    npm --prefix web run build
    # The homepage's committed hero: built from what it says (see site/tools/hero.mjs).
    node site/tools/hero.mjs --check
    # The Terms PDFs the API attaches (and the legal pages, with the site's packages): printed from site/legal/.
    node site/tools/legal.mjs --check

# rustfmt needs nightly for imports_granularity / group_imports (rustfmt.toml)
fmt:
    cargo +{{nightly}} fmt --all
    npm --prefix web run format

# Playwright end-to-end tests against a running stack (`just up` first)
e2e:
    npm --prefix web run e2e

# Playwright on the dev-only screens (/app/dev/*): the Vite dev server, no stack
e2e-dev:
    npm --prefix web run e2e:dev

# frame times of the schema viewer at 300 tables, headed, on a production build
perf:
    npm --prefix web run perf

# serve the homepage (site/public) the way Cloudflare Pages will, on this worktree's site port
site: env
    #!/usr/bin/env bash
    set -euo pipefail
    # Not $ROMULUS_SITE_PORT: on a first run just loaded no .env yet.
    port=$(sed -n 's/^ROMULUS_SITE_PORT=//p' .env)
    node site/serve.mjs "$port"

# the homepage's checks: tokens and fonts in sync with web/, the live hero and the legal pages built from their sources, the edge proxy's tests, then Playwright and axe
site-check: env
    node site/tools/sync.mjs --check
    node site/tools/hero.mjs --check
    node site/tools/legal.mjs --check
    npm --prefix site run test:edge
    npm --prefix site run e2e

# re-copy tokens and fonts from web/, rebuild the live hero, re-render the social image and the icons
site-gen:
    node site/tools/sync.mjs
    node site/tools/hero.mjs
    node site/tools/assets.mjs

# rebuild the homepage's live hero only (crates/romulus-wasm + web/src/hero → site/public/hero)
site-hero:
    node site/tools/hero.mjs

# re-render the legal pages and the Terms PDFs the API attaches, after editing site/legal/
site-legal:
    node site/tools/legal.mjs

# Lighthouse, mobile and desktop, against the homepage; reports in site/lighthouse/
site-lighthouse: env
    #!/usr/bin/env bash
    set -euo pipefail
    port=$(sed -n 's/^ROMULUS_SITE_PORT=//p' .env)
    url="http://localhost:$port/"
    if ! curl -fs -o /dev/null "$url"; then
        node site/serve.mjs "$port" >/dev/null & server=$!
        trap 'kill $server' EXIT
        until curl -fs -o /dev/null "$url"; do sleep 0.2; done
    fi
    mkdir -p site/lighthouse
    for preset in mobile desktop; do
        flag=""
        if [ "$preset" = desktop ]; then flag="--preset=desktop"; fi
        npx --yes lighthouse@13.5.0 "$url" $flag --quiet --chrome-flags=--headless=new \
            --output=html --output=json --output-path="site/lighthouse/$preset"
        node -e 'const r = require(process.argv[1]); console.log(process.argv[2].padEnd(8), Object.values(r.categories).map((c) => `${c.title} ${Math.round(c.score * 100)}`).join(" · "))' \
            "$PWD/site/lighthouse/$preset.report.json" "$preset"
    done

# publish the homepage and its Function (/app, /api → the preview's API) to Cloudflare Pages:
# lupidae's hosted homepage, maintainers only (site/README.md)
# (site/wrangler.toml; `npx wrangler login` once, or CLOUDFLARE_API_TOKEN/CLOUDFLARE_ACCOUNT_ID)
site-deploy:
    cd site && npx --yes wrangler@4.151.0 pages deploy --branch main
