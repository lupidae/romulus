# The playground Postgres (docs/playground.md, sections 3 and 4): one
# cluster per playground, in its own container, which the runner
# (crates/romulus-play-runner) starts with gVisor, no network, a read-only
# root filesystem and no capabilities.
#
# A variant of deploy/sandbox: the same Postgres, PostGIS and pgvector (bump
# them together), the same per-process memory cap, but **nothing trusted**.
# Here people run DML and call any function, so an extension script must
# never run after init: every `trusted = true` is removed (Postgres then
# refuses CREATE EXTENSION to everyone but the init superuser), and the
# control files of everything off romulus's allowlist are deleted (dblink,
# postgres_fdw, file_fdw ship with the server).
#
# The same image runs the runner's file helper (`play-files`: create, copy,
# delete a playground's directories), as root, with no network.
FROM postgres:17.11

ARG POSTGIS_VERSION=3.6.4+dfsg-2.pgdg13+1
ARG PGVECTOR_VERSION=0.8.7-1.pgdg13+1

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      "postgresql-17-postgis-3=${POSTGIS_VERSION}" \
      "postgresql-17-postgis-3-scripts=${POSTGIS_VERSION}" \
      "postgresql-17-pgvector=${PGVECTOR_VERSION}" \
 && rm -rf /var/lib/apt/lists/*

# Keep plpgsql and the allowlist (romulus_sandbox::catalog::ALLOWED_EXTENSIONS,
# the sandbox's trusted-extensions.txt), none of them trusted; delete the rest.
COPY extensions.txt /tmp/extensions.txt
RUN set -eu; \
    dir=/usr/share/postgresql/17/extension; \
    while read -r ext; do \
      [ -n "$ext" ] || continue; \
      [ -f "$dir/$ext.control" ] || { echo "missing extension $ext" >&2; exit 1; }; \
      # PostGIS's are symlinks (to postgis-3.control…), deleted below: copies.
      if [ -L "$dir/$ext.control" ]; then cp --remove-destination "$(readlink -f "$dir/$ext.control")" "$dir/$ext.control"; fi; \
    done < /tmp/extensions.txt; \
    for control in "$dir"/*.control; do \
      ext=$(basename "$control" .control); \
      if [ "$ext" = plpgsql ] || grep -qxF "$ext" /tmp/extensions.txt; then \
        sed -i '/^[[:space:]]*trusted[[:space:]]*=/d' "$control"; \
      else \
        rm -f "$control" "$dir/$ext"--*.sql; \
      fi; \
    done; \
    if grep -l '^[[:space:]]*trusted' "$dir"/*.control; then echo "an extension is still trusted" >&2; exit 1; fi; \
    rm /tmp/extensions.txt

COPY play-entrypoint.sh play-files /usr/local/bin/
COPY play-init.sql /usr/local/share/romulus/play-init.sql
RUN chmod 0755 /usr/local/bin/play-entrypoint.sh /usr/local/bin/play-files \
 && chmod 0644 /usr/local/share/romulus/play-init.sql

# The runner starts it as postgres (999) with the data directory and the
# socket directory mounted; the root filesystem is read-only, /tmp a tmpfs.
USER postgres
STOPSIGNAL SIGINT
ENTRYPOINT ["play-entrypoint.sh"]
