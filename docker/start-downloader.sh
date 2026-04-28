#!/bin/sh
set -eu

mkdir -p /app/.local/archives /app/.local/downloader

if [ -z "${DOWNLOADER_PEER_NAME:-}" ] || [ -z "${DOWNLOADER_API_ID:-}" ] || [ -z "${DOWNLOADER_API_HASH:-}" ]; then
  echo "downloader disabled: set DOWNLOADER_PEER_NAME, DOWNLOADER_API_ID and DOWNLOADER_API_HASH in .env"
  exec sleep infinity
fi

exec /usr/local/bin/downloader
