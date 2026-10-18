# syntax=docker/dockerfile:1.7
# The API image, ghcr.io/lupidae/romulus-api: the API (release build) and the
# web app's production build, which the API serves at /app (ROMULUS_WEB_DIR,
# crates/romulus-api/src/web.rs) beside itself at /api. Target `play`:
# ghcr.io/lupidae/romulus-play, the playground runner and service. Built and pushed by
# CI (.github/workflows/images.yml), run by deploy/preview/compose.yaml.
# Context: the repository root (.dockerignore keeps local state out).

# --- the web app: Vite with base /app/ (web/vite.config.ts) -----------------
FROM node:24.22.0-trixie-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
# The exporter scripts the import page offers for download (imported as raw
# text by web/src/features/imports/commands.ts, from ../scripts).
COPY scripts/export-migrations/ /src/scripts/export-migrations/
RUN npm run build

# --- the API ------------------------------------------------------------------
# cargo-chef: the dependencies build in their own layer, keyed by the lock
# file, so a source change only recompiles the workspace.
FROM lukemathwalker/cargo-chef:0.1.78-rust-1.99.0-slim-trixie AS chef
# bindgen (pg_query's build script) needs libclang.
RUN apt-get update \
 && apt-get install -y --no-install-recommends clang libclang-dev \
 && rm -rf /var/lib/apt/lists/*
ENV CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=always
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
# The pinned toolchain (and its components) once, in a cached layer.
COPY rust-toolchain.toml ./
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --package romulus-api --recipe-path recipe.json
COPY . .
# The query macros check against .sqlx/, not a database.
ENV SQLX_OFFLINE=true
RUN cargo build --release --locked --package romulus-api

# --- the playground runner and service (crates/romulus-play-runner, -playground)
# One image for both: `docker build --target play` (deploy/preview/deploy.sh
# build). The runner also runs it, privileged and for a moment, for
# `xfs_quota` (docs/playground.md): hence xfsprogs.
FROM build AS play-build
RUN cargo build --release --locked --package romulus-play-runner --package romulus-playground

FROM debian:trixie-slim AS play
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates xfsprogs \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin romulus
COPY --from=play-build /src/target/release/romulus-play-runner /usr/local/bin/romulus-play-runner
COPY --from=play-build /src/target/release/romulus-playground /usr/local/bin/romulus-playground
ENV ROMULUS_LOG_FORMAT=json \
    RUST_LOG=romulus_playground=info,romulus_play_runner=info
USER romulus
CMD ["romulus-playground"]

# --- runtime ------------------------------------------------------------------
FROM debian:trixie-slim
# curl: the health check. TLS to the IdP uses rustls with its own roots.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --no-create-home --shell /usr/sbin/nologin romulus
COPY --from=build /src/target/release/romulus-api /usr/local/bin/romulus-api
COPY --from=web /src/web/dist /srv/web
ENV ROMULUS_API_ADDR=0.0.0.0:8080 \
    ROMULUS_WEB_DIR=/srv/web \
    ROMULUS_LOG_FORMAT=json \
    RUST_LOG=romulus_api=info,tower_http=info,sqlx=warn
USER romulus
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=5s --start-period=20s --retries=3 \
    CMD ["curl", "-fsS", "-o", "/dev/null", "http://127.0.0.1:8080/api/health"]
ENTRYPOINT ["romulus-api"]
CMD ["serve"]
