#!/usr/bin/env bash
# Runs when a Superset workspace is deleted: this worktree's stack and its
# data go, and its port slot returns to the pool. Containers, volumes and
# networks are found by the compose project label, and the project comes from
# this worktree's path, so this never touches another worktree's stack (and
# works even if .env is gone).
set -euo pipefail
cd "$(dirname "$0")/.."

project=$(scripts/dev-env --project-name)
label="label=com.docker.compose.project=$project"
containers=$(docker ps -aq --filter "$label")
[ -z "$containers" ] || docker rm -f $containers >/dev/null
volumes=$(docker volume ls -q --filter "$label")
[ -z "$volumes" ] || docker volume rm $volumes >/dev/null
networks=$(docker network ls -q --filter "$label")
[ -z "$networks" ] || docker network rm $networks >/dev/null
echo "teardown: removed the $project stack"
scripts/dev-env --release
