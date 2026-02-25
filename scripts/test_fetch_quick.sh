#!/usr/bin/env bash
set -euo pipefail

# Quick sanity test: fetch a tiny block range and store in SQLite.
# Requires: MEV_RPC_URL in environment (source .env)

if [[ -z "${MEV_RPC_URL:-}" ]]; then
  echo "MEV_RPC_URL is not set. Run: source .env"
  exit 1
fi

DB_PATH="${MEV_DB_PATH:-data/mev.sqlite}"
START_BLOCK="${1:-21500000}"
END_BLOCK="${2:-21500005}"

mkdir -p "$(dirname "$DB_PATH")"

echo "Running quick fetch test"
echo "  range: ${START_BLOCK}-${END_BLOCK}"
echo "  db: ${DB_PATH}"

time cargo run -p mev-cli -- \
  --db-path "$DB_PATH" \
  fetch \
  --start-block "$START_BLOCK" \
  --end-block "$END_BLOCK"

echo "Quick fetch completed."
