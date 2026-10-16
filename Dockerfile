# syntax=docker/dockerfile:1
ARG RUST_VERSION=1.99.0
ARG DEBIAN=trixie

# ---- chef: Rust toolchain + cargo-chef, used to cache dependency builds ----
FROM rust:${RUST_VERSION}-slim-${DEBIAN} AS chef
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---- builder: cook deps (cached layer), then build the release binary ----
FROM chef AS builder
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked --bin codemap \
 && cp target/release/codemap /usr/local/bin/codemap

# ---- runtime: small image, non-root, git for commit/dirty info ----
FROM debian:${DEBIAN}-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 # Projects are mounted with the host's UID; without this git refuses with "dubious ownership".
 # Any path: workspace apps and linked repos (/links/backend) are mounted too. git is only read.
 && git config --system --add safe.directory '*' \
 && useradd --system --uid 10001 --create-home --home-dir /home/codemap --shell /usr/sbin/nologin codemap \
 && mkdir -p /data /workspace \
 && chown codemap:codemap /data
COPY --from=builder /usr/local/bin/codemap /usr/local/bin/codemap
ENV CODEMAP_DATA_DIR=/data \
    CODEMAP_IN_DOCKER=1
USER codemap
WORKDIR /workspace
VOLUME ["/data"]
ENTRYPOINT ["codemap"]
CMD ["--help"]

# ---- runtime-php: adds php-cli for `artisan route:list` (artisan mode) ----
FROM runtime AS runtime-php
USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      php-cli php-mbstring php-xml php-curl php-zip php-bcmath php-intl \
      php-sqlite3 php-mysql php-pgsql \
 && rm -rf /var/lib/apt/lists/*
USER codemap

# ---- dev: toolchain for `docker compose run --rm dev cargo ...` ----
FROM rust:${RUST_VERSION}-slim-${DEBIAN} AS dev
RUN apt-get update \
 && apt-get install -y --no-install-recommends git \
 && rm -rf /var/lib/apt/lists/* \
 && rustup component add rustfmt clippy \
 && cargo install cargo-insta --locked --version 1.49.0 \
 && rm -rf /usr/local/cargo/registry
WORKDIR /src
