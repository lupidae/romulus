#!/usr/bin/env bash
# Runs once in every new Superset workspace (a git worktree of this repo).
# Fast and idempotent: ports and .env, then dependencies. The stack itself
# (docker) and the first cargo build start with `just dev`, the run command.
set -euo pipefail
cd "$(dirname "$0")/.."

# This worktree's port slot, compose project and .env (CLAUDE.md, "Ports").
scripts/dev-env

npm ci --prefix web --no-audit --no-fund --loglevel=error

# Crates are shared through ~/.cargo, so this is quick after the first time.
cargo fetch --locked --quiet

missing=()
for tool in just docker; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
cargo sqlx --version >/dev/null 2>&1 || missing+=("sqlx-cli")
if [ ${#missing[@]} -gt 0 ]; then
    echo "setup: not installed: ${missing[*]} (see CLAUDE.md, \"Run it\")" >&2
fi
echo "setup: ready. Run \`just dev\`, then open $(sed -n 's/^ROMULUS_PUBLIC_URL=//p' .env)/app/"
