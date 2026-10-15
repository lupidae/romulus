#!/usr/bin/env bash
# Runs when a Superset workspace is deleted: this worktree's stack and its
# data go, and its port slot returns to the pool.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -f .env ]; then
    docker compose down -v --remove-orphans || true
fi
scripts/dev-env --release
