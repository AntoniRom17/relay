#!/usr/bin/env sh
set -eu

URL="${1:-http://127.0.0.1:8080/demo}"
DURATION="${DURATION:-30s}"
CONCURRENCY="${CONCURRENCY:-64}"

if ! command -v hey >/dev/null 2>&1; then
  echo "error: hey is required (https://github.com/rakyll/hey)" >&2
  exit 1
fi

echo "Relay load run: url=$URL duration=$DURATION concurrency=$CONCURRENCY"
exec hey -z "$DURATION" -c "$CONCURRENCY" "$URL"
