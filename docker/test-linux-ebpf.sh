#!/usr/bin/env bash
# Run Linux eBPF sandbox tests inside Docker.
# Run from workspace root.
set -euo pipefail

WORKSPACE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

docker build -f "$WORKSPACE_ROOT/docker/Dockerfile.linux-test" -t rage-ebpf-test "$WORKSPACE_ROOT"
docker run --privileged rage-ebpf-test
