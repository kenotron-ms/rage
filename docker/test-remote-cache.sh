#!/usr/bin/env bash
# Run remote cache integration tests with MinIO and Azurite.
# Requires Docker.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

echo "[test] Starting MinIO and Azurite..."
docker compose -f "${SCRIPT_DIR}/docker-compose.test.yml" up -d --wait

echo "[test] Running S3 integration tests (MinIO)..."
AWS_ACCESS_KEY_ID=minioadmin \
  AWS_SECRET_ACCESS_KEY=minioadmin \
  AWS_ENDPOINT_URL=http://localhost:9000 \
  AWS_DEFAULT_REGION=us-east-1 \
  cargo test -p cache --features s3 -- --ignored s3 2>&1

echo "[test] Running Azure Blob integration tests (Azurite)..."
AZURE_STORAGE_ACCOUNT=devstoreaccount1 \
  AZURE_STORAGE_KEY="Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==" \
  cargo test -p cache --features azure -- --ignored azure 2>&1

echo "[test] Stopping test services..."
docker compose -f "${SCRIPT_DIR}/docker-compose.test.yml" down

echo "[test] Integration tests complete."
