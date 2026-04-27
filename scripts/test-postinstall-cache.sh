#!/usr/bin/env bash
# Test postinstall caching in rage.
#
# Creates a real minimal yarn workspace, lets rage run `yarn install` to
# populate the install marker, injects a fake native package into node_modules,
# then verifies rage caches and restores the postinstall output without
# re-running the script on the second pass.
#
# Why a two-phase setup (pre-heat + inject)?
#   Yarn v1 prunes unmanaged packages from node_modules during `yarn install`,
#   so we must let the install complete first, then inject our fake package.
#
# Why an absolute cache dir in rage.json?
#   A relative dir like ".rage-cache" resolves against the rage PROCESS cwd
#   (usually the repo root), not the workspace dir.  That means .rage-cache/
#   and artifacts/ persist across test runs even after rm -rf "$WORKSPACE",
#   causing Run 1 to show "(restored from cache)" instead of actually
#   executing the postinstall.  An absolute path puts both dirs inside
#   $WORKSPACE so the rm -rf at the top of each run cleans them completely.
#
# Usage: ./scripts/test-postinstall-cache.sh
set -euo pipefail

RAGE="./target/release/rage"
WORKSPACE="/tmp/rage-postinstall-test"
FAKE_PKG="$WORKSPACE/node_modules/fake-native-pkg"
BUILD_NODE="$FAKE_PKG/build.node"

# ── colour helpers ─────────────────────────────────────────────────────────
green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n'  "$*"; }

bold "═══════════════════════════════════════"
bold " rage — postinstall cache smoke test"
bold "═══════════════════════════════════════"

# ── 1. Build ───────────────────────────────────────────────────────────────
echo ""
bold "▶ Building rage (release)..."
cargo build --release -p rage-cli 2>&1 | grep -E "Compiling rage-cli|Finished|error\[" || true
echo ""

# ── 2. Set up a minimal yarn workspace ────────────────────────────────────
bold "▶ Setting up workspace at $WORKSPACE..."
rm -rf "$WORKSPACE"
mkdir -p "$WORKSPACE/packages/app"

# Root manifest — yarn workspaces
cat > "$WORKSPACE/package.json" << 'PKGJSON'
{
  "name": "rage-postinstall-test-ws",
  "version": "1.0.0",
  "private": true,
  "workspaces": ["packages/*"]
}
PKGJSON

# One real workspace package with a build script
cat > "$WORKSPACE/packages/app/package.json" << 'PKGJSON'
{
  "name": "@rage-test/app",
  "version": "1.0.0",
  "scripts": { "build": "echo 'app built'" }
}
PKGJSON

# Yarn classic (v1) lockfile — recognised by yarn 1.x without network access.
# Using berry format (__metadata: version: 8) also works for detection, but
# yarn v1 would prune node_modules more aggressively on the first install.
printf '# yarn lockfile v1\n' > "$WORKSPACE/yarn.lock"

# rage.json with an ABSOLUTE cache dir so .rage-cache/ and artifacts/ live
# inside $WORKSPACE and are cleaned up by rm -rf at the top of each run.
# Relative paths resolve against the rage process cwd (the repo root), not
# the workspace — causing stale cache hits on repeat invocations.
#
# sandbox.default=loose: the macOS strict sandbox (DYLD_INSERT_LIBRARIES)
# is not needed for this smoke test and can be slow on macOS 26 (Tahoe)
# where system shells strip the env var.  Loose mode uses plain sh, which
# is much faster and sufficient here.
cat > "$WORKSPACE/rage.json" << 'RAGEJSON'
{"cache":{"backend":"local","dir":"/tmp/rage-postinstall-test/.rage-cache"},"sandbox":{"default":"loose"}}
RAGEJSON

echo "  workspace ready"
echo ""

# ── 3. Pre-heat: let rage run `yarn install` and write the install marker ─
# We must do this BEFORE injecting fake-native-pkg because yarn v1 prunes
# unmanaged packages from node_modules during install.
# After this step:
#   • /tmp/rage-postinstall-test/.rage-cache/root-{fp}.done  ← install marker
#   • node_modules/@rage-test/app  ← workspace symlink created by yarn
#   • @rage-test/app#build cached  ← TwoPhase cache entry written
bold "▶ Pre-heating install cache (yarn install via rage)..."
echo "─────────────────────────────────────────"
"$RAGE" run build "$WORKSPACE" 2>&1
echo "─────────────────────────────────────────"
echo ""

# ── 4. Inject fake-native-pkg AFTER yarn install is done ──────────────────
# The postinstall script writes a timestamped file so we can detect if the
# script ran vs if the output was restored from cache.
# Because fake-native-pkg is NOT in yarn.lock, rage uses the CAS key
# blake3("rage-fallback:fake-native-pkg:<platform>:<node-version>").
mkdir -p "$FAKE_PKG"
cat > "$FAKE_PKG/package.json" << 'PKGJSON'
{
  "name": "fake-native-pkg",
  "version": "1.0.0",
  "scripts": {
    "postinstall": "node -e \"require('fs').writeFileSync('build.node','COMPILED:'+Date.now())\""
  }
}
PKGJSON

echo "  fake-native-pkg injected into node_modules"
echo ""

# ── 5. Run 1 — postinstall should EXECUTE and delta stored in CAS ─────────
bold "▶ Run 1 — postinstall should EXECUTE and delta stored in CAS..."
echo "─────────────────────────────────────────"
RUN1_TMP=$(mktemp)
trap 'rm -f "$RUN1_TMP" 2>/dev/null' EXIT
"$RAGE" run build "$WORKSPACE" 2>&1 | tee "$RUN1_TMP"
RUN1_OUT=$(cat "$RUN1_TMP")
echo "─────────────────────────────────────────"
echo ""

if [[ ! -f "$BUILD_NODE" ]]; then
  red "✗ FAIL — build.node was not created (postinstall did not run)"
  exit 1
fi

# Verify Run 1 shows actual execution, not a cache hit
if echo "$RUN1_OUT" | grep -q "fake-native-pkg#postinstall.*restored from cache"; then
  red "✗ FAIL — Run 1 shows '(restored from cache)' — postinstall should have EXECUTED"
  red "  Hint: the cache dir may not be inside the workspace. Check rage.json."
  exit 1
fi
if ! echo "$RUN1_OUT" | grep -q "fake-native-pkg#postinstall"; then
  red "✗ FAIL — Run 1 output does not mention fake-native-pkg#postinstall"
  exit 1
fi

FIRST=$(cat "$BUILD_NODE")
green "✓ build.node created: $FIRST"
echo ""

# ── 6. Delete the compiled artifact (simulate a lost build output) ────────
bold "▶ Deleting build.node (simulating a lost compiled artifact)..."
rm "$BUILD_NODE"
echo "  build.node deleted"
echo ""

# ── 7. Run 2 — build.node should be RESTORED from CAS ────────────────────
bold "▶ Run 2 — build.node should be RESTORED from CAS (script must NOT re-run)..."
echo "─────────────────────────────────────────"
RUN2_TMP=$(mktemp)
trap 'rm -f "$RUN1_TMP" "$RUN2_TMP" 2>/dev/null' EXIT
"$RAGE" run build "$WORKSPACE" 2>&1 | tee "$RUN2_TMP"
RUN2_OUT=$(cat "$RUN2_TMP")
echo "─────────────────────────────────────────"
echo ""

# Verify Run 2 shows restoration, not re-execution
if ! echo "$RUN2_OUT" | grep -q "fake-native-pkg#postinstall.*restored from cache"; then
  red "✗ FAIL — Run 2 does not show '(restored from cache)'"
  red "  postinstall may have re-run — caching is broken"
  exit 1
fi

# ── 8. Verify ─────────────────────────────────────────────────────────────
bold "▶ Verifying..."
if [[ ! -f "$BUILD_NODE" ]]; then
  red "✗ FAIL — build.node was NOT restored from cache"
  exit 1
fi
SECOND=$(cat "$BUILD_NODE")

if [[ "$FIRST" == "$SECOND" ]]; then
  echo ""
  bold "═══════════════════════════════════════"
  green " ✅ PASS — timestamps match"
  echo "  Content: $SECOND"
  bold "═══════════════════════════════════════"
else
  echo ""
  bold "═══════════════════════════════════════"
  red " ❌ FAIL"
  echo "  Run 1: $FIRST"
  echo "  Run 2: $SECOND"
  echo "  Timestamps differ — postinstall ran again (caching broken)"
  bold "═══════════════════════════════════════"
  exit 1
fi
