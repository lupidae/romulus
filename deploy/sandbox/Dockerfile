# The sandbox Postgres that compiles draft SQL (crates/romulus-sandbox), one
# image per supported major: `--build-arg PG_MAJOR=15` (15, 16, 17, 18; 17
# by default). A database compiles on its own major's sandbox
# (docs/MVP.md, "Several Postgres majors").
#
# The parser is one for every major: the `pg_query` crate (6.2.1 → libpg_query
# from PostgreSQL 17.7). Older majors' grammar is a subset of it (the server
# refuses what it lacks, with its own error); syntax new in 18 is refused by
# the parser until pg_query speaks 18. Each major's minor release is pinned
# below; PostGIS and pgvector are the same release on every major (PGDG builds
# them per major: `apt-cache madison postgresql-17-postgis-3` in a base image
# lists what PGDG still serves). Bump them together.
ARG PG_MAJOR=17

FROM postgres:15.19 AS pg15
FROM postgres:16.15 AS pg16
FROM postgres:17.11 AS pg17
FROM postgres:18.6 AS pg18

# Only the chosen major's base is pulled (BuildKit skips unused stages).
FROM pg${PG_MAJOR}

ARG POSTGIS_VERSION=3.6.4+dfsg-2.pgdg13+1
ARG PGVECTOR_VERSION=0.8.7-1.pgdg13+1

# PG_MAJOR here is the base image's own (ENV PG_MAJOR), the major it runs.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      "postgresql-${PG_MAJOR}-postgis-3=${POSTGIS_VERSION}" \
      "postgresql-${PG_MAJOR}-postgis-3-scripts=${POSTGIS_VERSION}" \
      "postgresql-${PG_MAJOR}-pgvector=${PGVECTOR_VERSION}" \
 && rm -rf /var/lib/apt/lists/*

# Compiles run as an unprivileged role, which may only create *trusted*
# extensions. Every extension on romulus's allowlist (romulus_sandbox::catalog::
# ALLOWED_EXTENSIONS) is marked trusted here: its script is fixed by this image,
# and the sandbox cluster holds nothing but rolled-back compiles. Extensions not
# on the allowlist (dblink, postgres_fdw, file_fdw, adminpack…) stay untrusted,
# and the server-side allowlist refuses them before Postgres is asked.
COPY trusted-extensions.txt /tmp/trusted-extensions.txt
RUN set -eu; \
    dir=/usr/share/postgresql/${PG_MAJOR}/extension; \
    while read -r ext; do \
      [ -n "$ext" ] || continue; \
      control="$dir/$ext.control"; \
      [ -f "$control" ] || { echo "missing extension $ext" >&2; exit 1; }; \
      sed -i '/^[[:space:]]*trusted[[:space:]]*=/d' "$control"; \
      echo "trusted = true" >> "$control"; \
    done < /tmp/trusted-extensions.txt; \
    rm /tmp/trusted-extensions.txt

# The per-process memory cap (see romulus-entrypoint.sh). Setting ENTRYPOINT
# clears the base image's CMD, hence CMD again.
COPY romulus-entrypoint.sh /usr/local/bin/romulus-entrypoint.sh
RUN chmod 0755 /usr/local/bin/romulus-entrypoint.sh
ENTRYPOINT ["romulus-entrypoint.sh"]
CMD ["postgres"]
