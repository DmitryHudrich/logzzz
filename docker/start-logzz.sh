#!/bin/sh
set -eu

mkdir -p /app/.local/input /app/.local/archives /app/.local/reports

exec /usr/local/bin/logzz --config /app/config.yaml
