# The sandbox Postgres that compiles draft SQL (crates/romulus-sandbox).
#
# Its major version is pinned to the one the `pg_query` crate parses
# (pg_query 6.2.1 → libpg_query from PostgreSQL 17.7): the allowlist checks
# the grammar the sandbox runs. Bump both together, and the package versions
# below with them (`apt-cache madison postgresql-17-postgis-3` in the base
# image lists what PGDG still serves).
FROM postgres:17.11

ARG POSTGIS_VERSION=3.6.4+dfsg-2.pgdg13+1
ARG PGVECTOR_VERSION=0.8.7-1.pgdg13+1

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      "postgresql-17-postgis-3=${POSTGIS_VERSION}" \
      "postgresql-17-postgis-3-scripts=${POSTGIS_VERSION}" \
      "postgresql-17-pgvector=${PGVECTOR_VERSION}" \
 && rm -rf /var/lib/apt/lists/*

# Compiles run as an unprivileged role, which may only create *trusted*
# extensions. Every extension on romulus's allowlist (romulus_sandbox::catalog::
# ALLOWED_EXTENSIONS) is marked trusted here: its script is fixed by this image,
# and the sandbox cluster holds nothing but rolled-back compiles. Extensions not
# on the allowlist (dblink, postgres_fdw, file_fdw, adminpack…) stay untrusted,
# and the server-side allowlist refuses them before Postgres is asked.
COPY trusted-extensions.txt /tmp/trusted-extensions.txt
RUN set -eu; \
    dir=/usr/share/postgresql/17/extension; \
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
