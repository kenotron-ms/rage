#!/usr/bin/env bash
# rage distributed build integration test.
#
# Starts 3 Linux containers (1 hub + 2 spokes) via Docker Compose and verifies
# that tasks from the lage workspace are distributed across both spokes.
#
# Usage:
#   ./scripts/test-distributed.sh
#
# Requirements:
#   - Docker with Compose v2
#   - /Users/ken/workspace/lage cloned (or set WORKSPACE_PATH)

set -euo pipefail

WORKSPACE_PATH="${WORKSPACE_PATH:-/Users/ken/workspace/lage}"
TOKEN="${RAGE_HUB_TOKEN:-test-token-abc123}"
COMPOSE_FILE="docker/compose.hub-spoke.yaml"
TIMEOUT_SECS="${TIMEOUT_SECS:-120}"

echo ""
echo "=== rage distributed build integration test ==="
echo "Workspace: ${WORKSPACE_PATH}"
echo "Compose:   ${COMPOSE_FILE}"
echo ""

# Check prerequisites
if ! docker compose version &>/dev/null; then
    echo "ERROR: Docker Compose v2 not found" >&2
    exit 1
fi

if [ ! -d "${WORKSPACE_PATH}" ]; then
    echo "ERROR: Workspace not found: ${WORKSPACE_PATH}" >&2
    echo "  Run: gh repo clone microsoft/lage ${WORKSPACE_PATH}" >&2
    exit 1
fi

# Update compose file workspace path if needed
if [ "${WORKSPACE_PATH}" != "/Users/ken/workspace/lage" ]; then
    echo "Updating compose file for workspace: ${WORKSPACE_PATH}"
    sed -i.bak "s|/Users/ken/workspace/lage|${WORKSPACE_PATH}|g" "${COMPOSE_FILE}"
fi

echo "--- Building Docker image (first time may take 2-5 min) ---"
docker compose -f "${COMPOSE_FILE}" build --quiet

echo ""
echo "--- Starting hub + 2 spokes ---"
docker compose -f "${COMPOSE_FILE}" up -d --remove-orphans

echo ""
echo "--- Waiting for hub to be ready (max ${TIMEOUT_SECS}s) ---"
ELAPSED=0
while [ $ELAPSED -lt $TIMEOUT_SECS ]; do
    if docker compose -f "${COMPOSE_FILE}" exec -T hub ls /shared/rage-hub.json 2>/dev/null; then
        echo "Hub ready after ${ELAPSED}s (rendezvous file written)"
        break
    fi
    sleep 2
    ELAPSED=$((ELAPSED + 2))
done

if [ $ELAPSED -ge $TIMEOUT_SECS ]; then
    echo "ERROR: Hub did not start within ${TIMEOUT_SECS}s" >&2
    docker compose -f "${COMPOSE_FILE}" logs hub | tail -20
    docker compose -f "${COMPOSE_FILE}" down
    exit 1
fi

echo ""
echo "--- Waiting for build to complete ---"
BUILD_ELAPSED=0
while [ $BUILD_ELAPSED -lt $TIMEOUT_SECS ]; do
    # Check if hub has finished (no more tasks logged as 'dispatched')
    HUB_STATUS=$(docker compose -f "${COMPOSE_FILE}" logs --no-color hub 2>&1 | tail -5)
    if echo "${HUB_STATUS}" | grep -q "all tasks complete\|build done\|BuildDone"; then
        echo "Build completed after ${BUILD_ELAPSED}s"
        break
    fi
    sleep 5
    BUILD_ELAPSED=$((BUILD_ELAPSED + 5))
done

echo ""
echo "=== Results ==="

echo ""
echo "Hub logs (last 20 lines):"
docker compose -f "${COMPOSE_FILE}" logs --no-color hub 2>&1 | grep -E "\[rage" | tail -20

echo ""
echo "Spoke1 logs (task executions):"
SPOKE1_TASKS=$(docker compose -f "${COMPOSE_FILE}" logs --no-color spoke1 2>&1 | grep -c "\[rage-spoke\] running" || echo 0)
docker compose -f "${COMPOSE_FILE}" logs --no-color spoke1 2>&1 | grep "\[rage-spoke\]" | tail -10

echo ""
echo "Spoke2 logs (task executions):"
SPOKE2_TASKS=$(docker compose -f "${COMPOSE_FILE}" logs --no-color spoke2 2>&1 | grep -c "\[rage-spoke\] running" || echo 0)
docker compose -f "${COMPOSE_FILE}" logs --no-color spoke2 2>&1 | grep "\[rage-spoke\]" | tail -10

echo ""
echo "Spoke1 ran: ${SPOKE1_TASKS} tasks"
echo "Spoke2 ran: ${SPOKE2_TASKS} tasks"

echo ""
echo "--- Tearing down ---"
docker compose -f "${COMPOSE_FILE}" down

TOTAL_SPOKE_TASKS=$((SPOKE1_TASKS + SPOKE2_TASKS))

if [ "$TOTAL_SPOKE_TASKS" -gt 0 ]; then
    echo ""
    echo "✅ PASS: ${TOTAL_SPOKE_TASKS} tasks distributed across spokes"
    echo "   Spoke1: ${SPOKE1_TASKS} tasks"
    echo "   Spoke2: ${SPOKE2_TASKS} tasks"
    exit 0
else
    echo ""
    echo "❌ FAIL: No tasks were distributed to spokes"
    exit 1
fi
