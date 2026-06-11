#!/usr/bin/env bash
# Start the local dev Postgres (pgvector) for delver-store.
# Lakebase-compatible target: pgvector yes, PostGIS not assumed (see docs/DECISIONS.md D-002).
set -euo pipefail
cd "$(dirname "$0")/.."

docker compose -f docker-compose.dev.yml up -d --wait db

export DATABASE_URL="postgres://delver:delver@localhost:5433/delver"
echo "dev db ready"
echo "DATABASE_URL=$DATABASE_URL"
