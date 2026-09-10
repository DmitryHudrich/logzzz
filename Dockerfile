# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release -p logzz -p downloader \
    && mkdir -p /out \
    && cp target/release/logzz target/release/downloader /out/

FROM debian:bookworm-slim

# `unrar` (RARLAB, non-free) reliably handles RAR3 and RAR5; p7zip's bundled RAR codec
# chokes on RAR5, so it is kept only as a secondary extractor. Enabling `non-free` is
# required to pull `unrar` on Debian bookworm. bookworm-slim ships the deb822
# `debian.sources` (with Signed-By), so add the components there instead of dropping a
# one-line list for the same origin, which apt rejects as a Signed-By conflict.
RUN sed -i 's/^Components: main$/Components: main non-free non-free-firmware/' \
        /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates p7zip-full unrar \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 1000 logzz \
    && useradd --system --uid 1000 --gid logzz --no-create-home --shell /usr/sbin/nologin logzz

WORKDIR /app

COPY --from=builder /out/logzz /usr/local/bin/logzz
COPY --from=builder /out/downloader /usr/local/bin/downloader
COPY migrations /app/migrations
COPY docker /app/docker

RUN chmod +x /app/docker/*.sh && chown -R logzz:logzz /app

# Matches the default first-user uid/gid on most Linux distros so the bind-mounted
# ./.local directory is writable out of the box; see README for the override if your
# host uid differs.
USER logzz:logzz
