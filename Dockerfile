FROM rust:1-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release -p logzz -p downloader

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates p7zip-full \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 1000 logzz \
    && useradd --system --uid 1000 --gid logzz --no-create-home --shell /usr/sbin/nologin logzz

WORKDIR /app

COPY --from=builder /app/target/release/logzz /usr/local/bin/logzz
COPY --from=builder /app/target/release/downloader /usr/local/bin/downloader
COPY migrations /app/migrations
COPY docker /app/docker

RUN chmod +x /app/docker/*.sh && chown -R logzz:logzz /app

# Matches the default first-user uid/gid on most Linux distros so the bind-mounted
# ./.local directory is writable out of the box; see README for the override if your
# host uid differs.
USER logzz:logzz
