#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${PGLITE_REPLICA_UPSTREAM_PORT:-5433}"
NAME=pglite-replica-demo

cleanup() { docker rm -f "$NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT
cleanup

echo "starting postgres:16 (wal_level=logical) on port $PORT ..."
docker run -d --name "$NAME" \
  -e POSTGRES_PASSWORD=postgres \
  -p "$PORT":5432 \
  postgres:16 \
  -c wal_level=logical -c max_wal_senders=10 -c max_replication_slots=10 >/dev/null

for _ in $(seq 1 60); do
  if docker exec "$NAME" pg_isready -U postgres >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker exec "$NAME" pg_isready -U postgres >/dev/null

PGLITE_REPLICA_UPSTREAM_PORT="$PORT" \
  cargo run -p pglite-examples --features replica --bin replica_sync
