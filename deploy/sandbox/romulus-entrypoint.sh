#!/bin/sh
# Cap each Postgres process's private memory (RLIMIT_DATA, in KiB) before
# the official entrypoint starts the server: the limit is inherited by the
# postmaster and every backend it forks, so an oversized value fails with
# an ordinary "out of memory" error (deploy/sandbox/README.md). Set here,
# not with `docker run --ulimit`, which also applies to the container
# runtime's own init and can stop the container from starting at all.
set -eu
ulimit -d "${ROMULUS_SANDBOX_MAX_DATA_KB:-131072}"
exec docker-entrypoint.sh "$@"
