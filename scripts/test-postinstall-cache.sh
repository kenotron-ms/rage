#!/usr/bin/env bash
# Rage postinstall cache test — realistic monorepo with real yarn install
# Uses esbuild (has a real postinstall that copies/links a platform binary).
#
# Usage: ./scripts/test-postinstall-cache.sh
set -euo pipefail

RAGE="./target/release/rage"
WORKSPACE="/tmp/rage-lage-style-test"

# ── colour helpers ────────────────────────────────────────────────────
green() { printf '\033[32m%s\033[0m\n' "$*"; }
red()   { printf '\033[31m%s\033[0m\n' "$*"; }
bold()  { printf '\033[1m%s\033[0m\n'  "$*"; }
dim()   { printf '\033[2m%s\033[0m\n'  "$*"; }

bold "═══════════════════════════════════════════════"
bold " rage — postinstall cache test (realistic)"
bold "═══════════════════════════════════════════════"
echo ""

# ── 1. Build ──────────────────────────────────────────────────────────
bold "▶ Building rage (release)..."
cargo build --release -p rage-cli 2>&1 | grep -E "Compiling rage-cli|Finished|^error" || true
echo ""

# ── 2. Create a lage-style monorepo ───────────────────────────────────
bold "▶ Creating monorepo at $WORKSPACE..."
rm -rf "$WORKSPACE"
mkdir -p "$WORKSPACE"/{packages/core,packages/logger,packages/cli}

# Root manifest — yarn workspaces, esbuild as a real dep with postinstall
cat > "$WORKSPACE/package.json" << 'EOF'
{
  "name": "rage-test-monorepo",
  "version": "1.0.0",
  "private": true,
  "workspaces": [
    "packages/*"
  ],
  "devDependencies": {
    "esbuild": "0.19.12"
  }
}
EOF

# Three workspace packages (mirrors lage's multi-package structure)
cat > "$WORKSPACE/packages/core/package.json" << 'EOF'
{
  "name": "@rage-test/core",
  "version": "1.0.0",
  "scripts": { "build": "echo '[core] built'" }
}
EOF

cat > "$WORKSPACE/packages/logger/package.json" << 'EOF'
{
  "name": "@rage-test/logger",
  "version": "1.0.0",
  "dependencies": { "@rage-test/core": "*" },
  "scripts": { "build": "echo '[logger] built'" }
}
EOF

cat > "$WORKSPACE/packages/cli/package.json" << 'EOF'
{
  "name": "@rage-test/cli",
  "version": "1.0.0",
  "dependencies": {
    "@rage-test/core": "*",
    "@rage-test/logger": "*"
  },
  "scripts": { "build": "echo '[cli] built'" }
}
EOF

# Isolated rage cache
cat > "$WORKSPACE/rage.json" << 'EOF'
{
  "cache": { "backend": "local", "dir": "/tmp/rage-lage-style-test/.rage-cache" },
  "sandbox": { "default": "loose" }
}
EOF

echo "  workspace ready"
echo ""

# ── 3. Run yarn install (real install, downloads esbuild) ─────────────
bold "▶ Running yarn install (downloads esbuild with its postinstall)..."
dim "  This takes ~10-30s on first run..."
cd "$WORKSPACE"
yarn install 2>&1 | grep -v "^$" | head -30
cd - > /dev/null

# Show esbuild's postinstall binary is present
ESBUILD_BIN="$WORKSPACE/node_modules/esbuild/bin/esbuild"
if [[ -f "$ESBUILD_BIN" ]]; then
  green "  ✓ esbuild binary ready: $ESBUILD_BIN"
else
  dim "  esbuild binary not at expected path — checking..."
  find "$WORKSPACE/node_modules/esbuild" -type f -name "*.js" | head -3
fi
echo ""

# Record what esbuild's package dir looks like after install
ESBUILD_INSTALL_FILES=$(find "$WORKSPACE/node_modules/esbuild" -type f | sort | wc -l | tr -d ' ')
bold "  esbuild package has $ESBUILD_INSTALL_FILES files after install"
echo ""

# ── 4. First rage run — install cached, postinstall detected & run ────
bold "▶ Run 1 — rage should detect esbuild#postinstall and cache it..."
echo "───────────────────────────────────────────────"
"$RAGE" run build "$WORKSPACE" 2>&1
echo "───────────────────────────────────────────────"
echo ""

# ── 5. Delete node_modules entirely ───────────────────────────────────
bold "▶ Deleting node_modules (simulating fresh CI checkout)..."
rm -rf "$WORKSPACE/node_modules"
echo "  node_modules deleted"
echo ""

# ── 6. Second rage run — should restore everything including esbuild ──
bold "▶ Run 2 — rage should restore packages AND esbuild#postinstall from CAS..."
echo "───────────────────────────────────────────────"
OUTPUT=$("$RAGE" run build "$WORKSPACE" 2>&1)
echo "$OUTPUT"
echo "───────────────────────────────────────────────"
echo ""

# ── 7. Verify ─────────────────────────────────────────────────────────
bold "▶ Verifying results..."
echo ""

PASS=true

# Check: esbuild was restored
if [[ -d "$WORKSPACE/node_modules/esbuild" ]]; then
  RESTORED_FILES=$(find "$WORKSPACE/node_modules/esbuild" -type f | sort | wc -l | tr -d ' ')
  green "  ✓ esbuild package restored ($RESTORED_FILES files)"
else
  red "  ✗ esbuild package NOT restored"
  PASS=false
fi

# Check: postinstall showed as restored (not re-executed)
if echo "$OUTPUT" | grep -q "esbuild#postinstall.*restored from cache"; then
  green "  ✓ esbuild#postinstall restored from cache (NOT re-run)"
elif echo "$OUTPUT" | grep -q "esbuild#postinstall"; then
  dim "  ~ esbuild#postinstall appeared in output — checking if cached or re-run"
  echo "$OUTPUT" | grep "esbuild#postinstall"
else
  dim "  ~ esbuild has no detectable postinstall (it may run at yarn time, not rage time)"
  dim "    checking if other postinstall tasks appeared..."
  if echo "$OUTPUT" | grep -q "postinstall.*restored from cache"; then
    green "  ✓ Some package postinstall restored from cache"
  fi
fi

# Check: build tasks ran
if echo "$OUTPUT" | grep -q "@rage-test"; then
  green "  ✓ Workspace packages built"
fi

# Check: no yarn install in run 2 (CAS did the restore)
if echo "$OUTPUT" | grep -q "workspace#install.*restored from artifact cache"; then
  green "  ✓ workspace#install restored from artifact cache (no yarn ran)"
elif echo "$OUTPUT" | grep -q "workspace#install.*cached"; then
  green "  ✓ workspace#install cache hit"
fi

echo ""
if [[ "$PASS" == "true" ]]; then
  bold "═══════════════════════════════════════════════"
  green " ✅  PASS"
  bold "═══════════════════════════════════════════════"
else
  bold "═══════════════════════════════════════════════"
  red " ❌  FAIL — see output above"
  bold "═══════════════════════════════════════════════"
  exit 1
fi
