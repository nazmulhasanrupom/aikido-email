#!/usr/bin/env bash
# Run the browser tests against a fresh server and a fresh database.
#   needs: docker compose -f docker-compose.dev.yml up -d (Postgres), cargo build (the binary with the UI inside), pnpm build in ui/
set -euo pipefail
cd "$(dirname "$0")/../.."
PORT="${E2E_PORT:-18090}"
DB="aikido_e2e"
DATA="$(mktemp -d)"
PG="${E2E_PG_CONTAINER:-aikido_email-postgres-1}"

docker exec "$PG" psql -U aikido -d postgres -q -c "DROP DATABASE IF EXISTS $DB" -c "CREATE DATABASE $DB"
cleanup() {
  [ -n "${E2E_KEEP_LOG:-}" ] && cp "$DATA/server.log" "$E2E_KEEP_LOG" || true
  [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null || true
  docker exec "$PG" psql -U aikido -d postgres -q -c "DROP DATABASE IF EXISTS $DB" || true
  rm -rf "$DATA"
}
trap cleanup EXIT

MOCK_PORT="${E2E_REOON_PORT:-18091}"
node ui/e2e/mock-reoon.mjs "$MOCK_PORT" &
MOCK_PID=$!
trap '[ -n "${MOCK_PID:-}" ] && kill "$MOCK_PID" 2>/dev/null || true; cleanup' EXIT

AIKIDO_REOON_BASE_URL="http://127.0.0.1:$MOCK_PORT" \
DATABASE_URL="postgres://aikido:aikido@127.0.0.1:55432/$DB" \
AIKIDO_DATA_DIR="$DATA" \
AIKIDO_BIND_ADDR="127.0.0.1:$PORT" \
AIKIDO_PUBLIC_URL="http://localhost:$PORT" \
AIKIDO_DRY_RUN=true \
  ./target/debug/aikido_email serve >"$DATA/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$PORT/health" >/dev/null && break
  sleep 0.5
done
curl -sf "http://127.0.0.1:$PORT/health" >/dev/null || { cat "$DATA/server.log"; exit 1; }

cd ui
AIKIDO_E2E_URL="http://localhost:$PORT" \
  pnpm exec playwright test "$@"
