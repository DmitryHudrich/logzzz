FROM rust:1-bookworm AS builder

WORKDIR /app
COPY . .
RUN cargo build --release -p logzz -p downloader

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates p7zip-full \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/logzz /usr/local/bin/logzz
COPY --from=builder /app/target/release/downloader /usr/local/bin/downloader
COPY migrations /app/migrations
COPY docker /app/docker

RUN chmod +x /app/docker/*.sh
