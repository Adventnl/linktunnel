#!/usr/bin/env bash
# Runs the storage conformance suite against real PostgreSQL and Redis servers in containers,
# then removes them. Requires a running Docker daemon.
set -euo pipefail
cd "$(dirname "$0")/.."

PG_PORT="${NEXUS_TEST_PG_PORT:-55432}"
REDIS_PORT="${NEXUS_TEST_REDIS_PORT:-56379}"
PG_NAME="nexus-test-postgres-$$"
REDIS_NAME="nexus-test-redis-$$"

cleanup() { docker rm -f "$PG_NAME" "$REDIS_NAME" >/dev/null 2>&1 || true; }
trap cleanup EXIT

echo "== starting PostgreSQL and Redis"
docker run -d --rm --name "$PG_NAME" -e POSTGRES_PASSWORD=nexus -e POSTGRES_DB=nexus \
  -p "127.0.0.1:${PG_PORT}:5432" postgres:17-alpine >/dev/null
docker run -d --rm --name "$REDIS_NAME" -p "127.0.0.1:${REDIS_PORT}:6379" redis:8-alpine >/dev/null

echo "== waiting for readiness"
for _ in $(seq 1 60); do
  docker exec "$PG_NAME" pg_isready -h 127.0.0.1 -U postgres -d nexus >/dev/null 2>&1 \
    && docker exec "$REDIS_NAME" redis-cli ping 2>/dev/null | grep -q PONG && break
  sleep 1
done
docker exec "$PG_NAME" pg_isready -h 127.0.0.1 -U postgres -d nexus
docker exec "$REDIS_NAME" redis-cli ping

echo "== running the conformance suite"
NEXUS_REQUIRE_SERVICES=1 \
NEXUS_TEST_POSTGRES_URL="postgres://postgres:nexus@127.0.0.1:${PG_PORT}/nexus" \
NEXUS_TEST_REDIS_URL="redis://127.0.0.1:${REDIS_PORT}" \
  cargo test -p nexus-signal --features "sqlite postgres redis" --test store_conformance -- --nocapture
