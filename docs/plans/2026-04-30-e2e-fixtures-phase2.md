# E2E Fixture Smoke Test Suite — Phase 2 Implementation Plan

> **Execution:** Use the subagent-driven-development workflow to implement this plan.

**Goal:** Wire up a cross-platform Node.js harness (`tests/fixtures/run.mjs`) that drives the 5 fixtures landed in Phase 1, and add a matching `e2e-smoke` job to GitHub Actions CI.

**Architecture:** A single ESM script interprets a `FIXTURES` data array (one entry per scenario). Each entry has ordered `steps` of three kinds — `run` (execute rage), `assert` (check `dist/run-count.txt` values), `mutate` (touch a source file). The `distributed` fixture takes a separate code path that spawns `rage hub` and `rage spoke` as background processes. CI builds the rage debug binary on Linux/macOS/Windows and runs the harness against all fixtures.

**Tech Stack:** Node.js 24 ESM (no npm deps — only built-ins: `node:child_process`, `node:fs`, `node:path`, `node:url`), pnpm 9, Rust stable, GitHub Actions.

---

## Reference design

Full design: `docs/plans/2026-04-30-e2e-fixtures-design.md`. Phase 1 plan (already shipped): `docs/plans/2026-04-30-e2e-fixtures-phase1.md`.

Phase 1 produced these fixture workspaces — they already exist on disk, do not recreate:

```
tests/fixtures/cache-correctness/    (pkg-core → pkg-utils → pkg-app)
tests/fixtures/partial-rebuild/      (pkg-a → pkg-b → pkg-c, pkg-d independent)
tests/fixtures/error-propagation/    (pkg-a ok, pkg-b exit 1, pkg-c → pkg-b)
tests/fixtures/diamond-dep/          (pkg-shared → pkg-a, pkg-shared → pkg-b, pkg-a+pkg-b → pkg-app)
tests/fixtures/distributed/          (pkg-a, pkg-b → pkg-c, pkg-d → pkg-e)
```

Each package has a `build` script that runs `tsc --build` then increments `dist/run-count.txt`. Each fixture has a committed `pnpm-lock.yaml`.

---

## Conventions

- Working directory for every command in this plan is the repo root: `/Users/ken/workspace/ms/rage`.
- The rage debug binary lives at `target/debug/rage` (Linux/macOS) or `target\debug\rage.exe` (Windows). Phase 2 is developed on macOS, so `target/debug/rage` is the local path.
- Commit messages use conventional commit prefixes (`feat`, `test`, `ci`, `chore`).
- After each task, commit. No batched end-of-plan commits.

---

## Pre-flight: build rage and verify fixtures (run once before Task 1)

**Step 1: Build the rage binary**

Run: `cargo build --workspace`
Expected: builds successfully, produces `target/debug/rage`.

**Step 2: Confirm the binary exists**

Run: `ls target/debug/rage`
Expected: file exists. If not, the build failed — stop and fix.

**Step 3: Confirm all 5 fixtures are committed**

Run: `ls tests/fixtures/`
Expected output (exactly):
```
cache-correctness
diamond-dep
distributed
error-propagation
partial-rebuild
```

**Step 4: Confirm Node.js 24 is available**

Run: `node --version`
Expected: `v24.x.x` (or anything ≥ v18 — ESM has worked since Node 14, we just match CI).

If you get less than v18, install Node 24 via nvm or fnm before continuing.

---

## Task 1: Skeleton harness file (no logic)

**Files:**
- Create: `tests/fixtures/run.mjs`

**Step 1: Create the skeleton with imports, RAGE_BIN, FIXTURES array, and an empty `main()`**

Write `tests/fixtures/run.mjs` with exactly this content:

```js
// tests/fixtures/run.mjs
// Node.js ESM — no npm deps needed, uses only Node built-ins

import { execSync, spawn } from 'node:child_process';
import { existsSync, readFileSync, appendFileSync } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));

// Auto-detect rage binary based on platform
const RAGE_BIN = process.env.RAGE_BIN
  ?? (process.platform === 'win32'
      ? resolve(__dirname, '..', '..', 'target', 'debug', 'rage.exe')
      : resolve(__dirname, '..', '..', 'target', 'debug', 'rage'));

// FIXTURES — add new scenarios by adding an entry here
const FIXTURES = [
  {
    name: 'cache-correctness',
    steps: [
      { run: true },
      { assert: { 'pkg-core': 1, 'pkg-utils': 1, 'pkg-app': 1 } },
      { run: true },
      { assert: { 'pkg-core': 1, 'pkg-utils': 1, 'pkg-app': 1 } },
    ],
  },
  {
    name: 'partial-rebuild',
    steps: [
      { run: true },
      { assert: { 'pkg-a': 1, 'pkg-b': 1, 'pkg-c': 1, 'pkg-d': 1 } },
      { mutate: 'packages/pkg-a/src/index.ts', append: '\n// cache-bust' },
      { run: true },
      { assert: { 'pkg-a': 2, 'pkg-b': 2, 'pkg-c': 2, 'pkg-d': 1 } },
    ],
  },
  {
    name: 'error-propagation',
    steps: [
      { run: true, expectFailure: true },
      { assert: { 'pkg-a': 1, 'pkg-b': null, 'pkg-c': null } },
    ],
  },
  {
    name: 'diamond-dep',
    steps: [
      { run: true },
      { assert: { 'pkg-shared': 1, 'pkg-a': 1, 'pkg-b': 1, 'pkg-app': 1 } },
    ],
  },
  {
    name: 'distributed',
    mode: 'distributed',
    steps: [
      { run: true },
      { assert: { 'pkg-a': 1, 'pkg-b': 1, 'pkg-c': 1, 'pkg-d': 1, 'pkg-e': 1 } },
    ],
  },
];

// Helpers go here (Task 2+)

// --- Entry point ---

async function main() {
  // TODO: implement in Task 3
}

main().catch(err => {
  console.error('Unhandled error:', err);
  process.exit(1);
});
```

**Step 2: Syntax-check the file**

Run: `node --check tests/fixtures/run.mjs`
Expected: no output, exit code 0.

If you see a SyntaxError, re-paste exactly. Do not improvise.

**Step 3: Run the skeleton**

Run: `node tests/fixtures/run.mjs 2>&1 | head -20`
Expected: no output (main does nothing), process exits 0.

Verify exit code: `node tests/fixtures/run.mjs; echo "exit=$?"`
Expected ends with: `exit=0`.

**Step 4: Commit**

```
git add tests/fixtures/run.mjs
git commit -m "test(fixtures): add run.mjs harness skeleton"
```

---

## Task 2: `readCounter` and `assert` helpers, plus a self-test

**Files:**
- Modify: `tests/fixtures/run.mjs`

**Step 1: Add the two helpers**

Replace the line `// Helpers go here (Task 2+)` with:

```js
// --- Helpers ---

function readCounter(fixtureDir, pkg) {
  const counterFile = join(fixtureDir, 'packages', pkg, 'dist', 'run-count.txt');
  if (!existsSync(counterFile)) return null;
  const raw = readFileSync(counterFile, 'utf8').trim();
  return parseInt(raw, 10);
}

function assert(condition, message) {
  if (!condition) {
    console.error(`  FAIL: ${message}`);
    return false;
  }
  console.log(`  PASS: ${message}`);
  return true;
}
```

**Step 2: Add a self-test gated on `RUN_SELF_TEST`**

Replace the `async function main()` body with:

```js
async function main() {
  if (process.env.RUN_SELF_TEST) {
    let allOk = true;
    // assert() positive case
    if (!assert(true === true, 'assert(true) returns true')) allOk = false;
    // assert() negative case — we expect this to log FAIL and return false
    const negative = assert(false, 'this fail message is expected');
    if (negative !== false) {
      console.error('  FAIL: assert(false) should return false');
      allOk = false;
    } else {
      console.log('  PASS: assert(false) returned false (the FAIL above is expected)');
    }
    // readCounter on missing path → null
    const missing = readCounter('/tmp/__definitely_does_not_exist__', 'pkg-x');
    if (!assert(missing === null, `readCounter on missing path returns null (got ${missing})`)) {
      allOk = false;
    }
    process.exit(allOk ? 0 : 1);
  }
  // TODO: implement in Task 3
}
```

**Step 3: Syntax-check**

Run: `node --check tests/fixtures/run.mjs`
Expected: exit 0, no output.

**Step 4: Run the self-test**

Run: `RUN_SELF_TEST=1 node tests/fixtures/run.mjs 2>&1`
Expected output (in order):
```
  PASS: assert(true) returns true
  FAIL: this fail message is expected
  PASS: assert(false) returned false (the FAIL above is expected)
  PASS: readCounter on missing path returns null (got null)
```
Process exits 0. The single `FAIL:` line is intentional — it's the negative case proving `assert(false)` does log and return false.

**Step 5: Verify no-self-test path still exits 0**

Run: `node tests/fixtures/run.mjs; echo "exit=$?"`
Expected: `exit=0` (main hits TODO and falls through).

**Step 6: Commit**

```
git add tests/fixtures/run.mjs
git commit -m "test(fixtures): add readCounter/assert helpers with self-test"
```

---

## Task 3: `runRage` + `executeStep` (run/assert) + `runFixture` (standard mode), wired to cache-correctness

**Files:**
- Modify: `tests/fixtures/run.mjs`

**Step 1: Add `ensureInstalled` and `runRage` after the `assert` helper**

Append after the `assert` function:

```js
const installed = new Set();

function ensureInstalled(fixtureDir) {
  if (installed.has(fixtureDir)) return;
  console.log(`  Installing pnpm deps in ${fixtureDir}...`);
  execSync('pnpm install --frozen-lockfile', { cwd: fixtureDir, stdio: 'inherit' });
  installed.add(fixtureDir);
}

function runRage(fixtureDir, { expectFailure = false } = {}) {
  try {
    execSync(`"${RAGE_BIN}" run build "${fixtureDir}"`, {
      cwd: fixtureDir,
      stdio: 'inherit',
    });
    if (expectFailure) {
      console.error('  FAIL: rage was expected to exit non-zero but exited 0');
      return false;
    }
    return true;
  } catch (e) {
    if (expectFailure) {
      console.log('  PASS: rage exited non-zero as expected');
      return true;
    }
    console.error(`  FAIL: rage exited with error: ${e.message}`);
    return false;
  }
}
```

**Step 2: Add `executeStep` (handling `run` and `assert` only) and `runFixture` (standard mode only)**

Append after `runRage`:

```js
// --- Core step executor ---

async function executeStep(step, fixtureDir, fixtureName) {
  if (step.run !== undefined) {
    ensureInstalled(fixtureDir);
    return runRage(fixtureDir, { expectFailure: step.expectFailure ?? false });
  }

  if (step.assert !== undefined) {
    let allPassed = true;
    for (const [pkg, expected] of Object.entries(step.assert)) {
      const actual = readCounter(fixtureDir, pkg);
      const ok = expected === null
        ? assert(actual === null, `${pkg}: counter file must be absent (got ${actual})`)
        : assert(actual === expected, `${pkg}: counter=${actual}, expected=${expected}`);
      if (!ok) allPassed = false;
    }
    return allPassed;
  }

  console.error(`  FAIL: unknown step type: ${JSON.stringify(step)}`);
  return false;
}

// --- Fixture runner ---

async function runFixture(fixture) {
  const fixtureDir = join(__dirname, fixture.name);
  console.log(`\n=== ${fixture.name} ===`);

  // Standard mode: execute steps in sequence
  for (const step of fixture.steps) {
    const ok = await executeStep(step, fixtureDir, fixture.name);
    if (!ok) return false;
  }
  return true;
}
```

**Step 3: Replace the `main()` TODO with full main() (CLI flag + summary)**

Replace the entire `async function main()` with:

```js
async function main() {
  if (process.env.RUN_SELF_TEST) {
    let allOk = true;
    if (!assert(true === true, 'assert(true) returns true')) allOk = false;
    const negative = assert(false, 'this fail message is expected');
    if (negative !== false) {
      console.error('  FAIL: assert(false) should return false');
      allOk = false;
    } else {
      console.log('  PASS: assert(false) returned false (the FAIL above is expected)');
    }
    const missing = readCounter('/tmp/__definitely_does_not_exist__', 'pkg-x');
    if (!assert(missing === null, `readCounter on missing path returns null (got ${missing})`)) {
      allOk = false;
    }
    process.exit(allOk ? 0 : 1);
  }

  // Optional: --fixture <name> to run a single fixture
  const args = process.argv.slice(2);
  const targetIdx = args.indexOf('--fixture');
  const targetName = targetIdx >= 0 ? args[targetIdx + 1] : null;

  const toRun = targetName
    ? FIXTURES.filter(f => f.name === targetName)
    : FIXTURES;

  if (toRun.length === 0) {
    console.error(`No fixture found: ${targetName}`);
    process.exit(1);
  }

  const failures = [];
  for (const fixture of toRun) {
    const ok = await runFixture(fixture);
    if (!ok) failures.push(fixture.name);
  }

  console.log('\n=== SUMMARY ===');
  if (failures.length === 0) {
    console.log(`All ${toRun.length} fixture(s) PASSED.`);
    process.exit(0);
  } else {
    console.error(`FAILED: ${failures.join(', ')}`);
    process.exit(1);
  }
}
```

**Step 4: Syntax-check**

Run: `node --check tests/fixtures/run.mjs`
Expected: exit 0, no output.

**Step 5: Self-test still passes**

Run: `RUN_SELF_TEST=1 node tests/fixtures/run.mjs 2>&1 | tail -5`
Expected: ends with `PASS: readCounter on missing path returns null (got null)`, exit 0.

**Step 6: Clean any stale `dist/` from prior local experimentation**

Run:
```bash
rm -rf tests/fixtures/cache-correctness/packages/*/dist
```
Expected: no error.

**Step 7: Run the cache-correctness fixture end-to-end**

Run: `node tests/fixtures/run.mjs --fixture cache-correctness 2>&1 | tee /tmp/run-cc.log`

Expected (key lines):
- `=== cache-correctness ===`
- `Installing pnpm deps in .../cache-correctness...`
- (rage log spam from first run)
- `PASS: pkg-core: counter=1, expected=1`
- `PASS: pkg-utils: counter=1, expected=1`
- `PASS: pkg-app: counter=1, expected=1`
- (rage log spam from second run — should mostly say cached/restored)
- `PASS: pkg-core: counter=1, expected=1` (still 1 — cache hit, did not rebuild)
- `PASS: pkg-utils: counter=1, expected=1`
- `PASS: pkg-app: counter=1, expected=1`
- `=== SUMMARY ===`
- `All 1 fixture(s) PASSED.`

Verify exit code: `echo $?` → `0`.

If the second run shows `counter=2`, that's a real cache bug in rage — stop, do not paper over it. Report it; the harness is correctly reporting reality.

**Step 8: Commit**

```
git add tests/fixtures/run.mjs
git commit -m "test(fixtures): wire run/assert steps and standard-mode runner"
```

---

## Task 4: `mutate` step type, validated against partial-rebuild

**Files:**
- Modify: `tests/fixtures/run.mjs`

**Step 1: Add the `mutate` branch to `executeStep`**

Inside `executeStep`, immediately before the final `console.error('  FAIL: unknown step type...')` line, insert:

```js
  if (step.mutate !== undefined) {
    const filePath = join(fixtureDir, step.mutate);
    appendFileSync(filePath, step.append ?? '\n// cache-bust');
    console.log(`  Mutated ${step.mutate}`);
    return true;
  }
```

**Step 2: Syntax-check**

Run: `node --check tests/fixtures/run.mjs`
Expected: exit 0.

**Step 3: Reset the partial-rebuild fixture to a clean state**

The mutate step appends to `packages/pkg-a/src/index.ts`. We need a clean source file, otherwise repeated runs will pile up `// cache-bust` lines and the diff stays in git. Use git to reset:

Run:
```bash
git checkout -- tests/fixtures/partial-rebuild/packages/pkg-a/src/index.ts
rm -rf tests/fixtures/partial-rebuild/packages/*/dist
```
Expected: no error. The `index.ts` is back to its committed state.

**Step 4: Run the partial-rebuild fixture**

Run: `node tests/fixtures/run.mjs --fixture partial-rebuild 2>&1 | tee /tmp/run-pr.log`

Expected (key lines, in order):
- `=== partial-rebuild ===`
- (first rage run)
- `PASS: pkg-a: counter=1, expected=1`
- `PASS: pkg-b: counter=1, expected=1`
- `PASS: pkg-c: counter=1, expected=1`
- `PASS: pkg-d: counter=1, expected=1`
- `Mutated packages/pkg-a/src/index.ts`
- (second rage run)
- `PASS: pkg-a: counter=2, expected=2`
- `PASS: pkg-b: counter=2, expected=2`
- `PASS: pkg-c: counter=2, expected=2`
- `PASS: pkg-d: counter=1, expected=1`   ← this is the key behavioral assertion
- `All 1 fixture(s) PASSED.`

Exit code 0.

**Step 5: Reset the mutated source so it doesn't leak into the commit**

Run:
```bash
git checkout -- tests/fixtures/partial-rebuild/packages/pkg-a/src/index.ts
```
Expected: no error.

Verify cleanly:
```bash
git status tests/fixtures/partial-rebuild/
```
Expected: nothing modified (the dist/ directories are gitignored).

**Step 6: Commit**

```
git add tests/fixtures/run.mjs
git commit -m "test(fixtures): add mutate step type for partial-rebuild scenarios"
```

---

## Task 5: Validate `error-propagation` and `diamond-dep`

These fixtures use only step types we've already implemented. Task 5 is purely verification — no new code. If something fails here, the bug is either in the harness logic or in rage itself; do not paper over.

**Files:** none modified unless a bug is found.

**Step 1: Clean stale dist/**

Run:
```bash
rm -rf tests/fixtures/error-propagation/packages/*/dist
rm -rf tests/fixtures/diamond-dep/packages/*/dist
```

**Step 2: Run error-propagation**

Run: `node tests/fixtures/run.mjs --fixture error-propagation 2>&1 | tee /tmp/run-ep.log`

Expected (key lines):
- `=== error-propagation ===`
- (rage run, with pkg-b failing)
- `PASS: rage exited non-zero as expected`
- `PASS: pkg-a: counter=1, expected=1`
- `PASS: pkg-b: counter file must be absent (got null)`
- `PASS: pkg-c: counter file must be absent (got null)`
- `All 1 fixture(s) PASSED.`

Exit code 0.

If pkg-c counter is non-null, that's a real error-propagation bug in rage — report it.

**Step 3: Run diamond-dep**

Run: `node tests/fixtures/run.mjs --fixture diamond-dep 2>&1 | tee /tmp/run-dd.log`

Expected:
- `=== diamond-dep ===`
- `PASS: pkg-shared: counter=1, expected=1`   ← built once, despite two dependents
- `PASS: pkg-a: counter=1, expected=1`
- `PASS: pkg-b: counter=1, expected=1`
- `PASS: pkg-app: counter=1, expected=1`
- `All 1 fixture(s) PASSED.`

Exit 0.

If pkg-shared counter is 2, that's a real "build once" bug in rage — report it.

**Step 4: No commit (no code change). Move on to Task 6.**

If you DID need to fix something to make the harness handle these fixtures correctly — e.g. the `expectFailure` branch was wrong, or null-counter assertions misbehaved — commit that fix here:

```
git add tests/fixtures/run.mjs
git commit -m "test(fixtures): fix <specific issue> uncovered by error-propagation/diamond-dep"
```

---

## Task 6: Distributed mode (`runDistributed` + the `mode === 'distributed'` branch)

**Files:**
- Modify: `tests/fixtures/run.mjs`

**Step 1: Add `runDistributed` after `runRage`**

Insert after `runRage`:

```js
async function runDistributed(fixtureDir) {
  const addrFile = join(fixtureDir, '.rage-hub-addr.json');

  // Start hub in background
  const hub = spawn(RAGE_BIN, [
    'hub',
    '--workspace', fixtureDir,
    '--addr-file', addrFile,
  ], { cwd: fixtureDir, stdio: 'inherit' });

  // Start spoke in background (polls addr-file for up to 60s)
  const spoke = spawn(RAGE_BIN, [
    'spoke',
    '--workspace', fixtureDir,
    '--addr-file', addrFile,
  ], { cwd: fixtureDir, stdio: 'inherit' });

  // Wait for hub to exit (hub exits when DAG is done or on error)
  return new Promise((resolve) => {
    const timeout = setTimeout(() => {
      hub.kill();
      spoke.kill();
      console.error('  FAIL: distributed run timed out after 120s');
      resolve(false);
    }, 120_000);

    hub.on('close', (code) => {
      clearTimeout(timeout);
      spoke.kill(); // spoke reconnect loop — stop it now hub is done
      if (code === 0) {
        console.log('  PASS: hub exited 0 (DAG complete)');
        resolve(true);
      } else {
        console.error(`  FAIL: hub exited ${code}`);
        resolve(false);
      }
    });
  });
}
```

**Step 2: Add the distributed branch to `runFixture`**

Replace the entire `runFixture` function with:

```js
async function runFixture(fixture) {
  const fixtureDir = join(__dirname, fixture.name);
  console.log(`\n=== ${fixture.name} ===`);

  if (fixture.mode === 'distributed') {
    // For distributed: install first, then run hub+spoke
    ensureInstalled(fixtureDir);
    const runOk = await runDistributed(fixtureDir);
    if (!runOk) return false;
    // Then assert counters
    for (const step of fixture.steps) {
      if (step.assert) {
        const ok = await executeStep(step, fixtureDir, fixture.name);
        if (!ok) return false;
      }
    }
    return true;
  }

  // Standard mode: execute steps in sequence
  for (const step of fixture.steps) {
    const ok = await executeStep(step, fixtureDir, fixture.name);
    if (!ok) return false;
  }
  return true;
}
```

**Step 3: Syntax-check**

Run: `node --check tests/fixtures/run.mjs`
Expected: exit 0.

**Step 4: Confirm `rage hub` and `rage spoke` are real subcommands**

Run: `target/debug/rage hub --help 2>&1 | head -10`
Expected: usage text mentioning `--workspace` and `--addr-file`.

Run: `target/debug/rage spoke --help 2>&1 | head -10`
Expected: usage text mentioning `--workspace` and `--addr-file`.

If either returns "unknown command", the design assumed CLI surface that doesn't exist yet. STOP and surface the gap to the user — do not invent a workaround.

**Step 5: Clean stale dist and any prior addr-file**

Run:
```bash
rm -rf tests/fixtures/distributed/packages/*/dist
rm -f tests/fixtures/distributed/.rage-hub-addr.json
```

**Step 6: Run the distributed fixture**

Run: `node tests/fixtures/run.mjs --fixture distributed 2>&1 | tee /tmp/run-dist.log`

Expected:
- `=== distributed ===`
- `Installing pnpm deps in .../distributed...`
- (interleaved hub/spoke logs)
- `PASS: hub exited 0 (DAG complete)`
- `PASS: pkg-a: counter=1, expected=1`
- `PASS: pkg-b: counter=1, expected=1`
- `PASS: pkg-c: counter=1, expected=1`
- `PASS: pkg-d: counter=1, expected=1`
- `PASS: pkg-e: counter=1, expected=1`
- `All 1 fixture(s) PASSED.`

Exit code 0.

If the run hangs past 120s, the timeout fires and you get a `FAIL: distributed run timed out`. Investigate the hub/spoke logs in the output before assuming the harness is wrong.

**Step 7: Confirm addr-file was cleaned up by the kill (or clean it manually)**

Run:
```bash
rm -f tests/fixtures/distributed/.rage-hub-addr.json
git status tests/fixtures/distributed/
```
Expected: nothing modified.

If `.rage-hub-addr.json` is showing up in `git status`, add it to `.gitignore` at the repo root (or a fixture-local `.gitignore`):

```bash
echo ".rage-hub-addr.json" >> .gitignore
```

**Step 8: Commit**

```
git add tests/fixtures/run.mjs .gitignore
git commit -m "test(fixtures): add distributed-mode hub+spoke harness branch"
```

(Drop `.gitignore` from the `git add` line if you didn't modify it.)

---

## Task 7: Full-suite green run

No code changes — this is the gate before touching CI.

**Step 1: Reset every fixture to a clean state**

Run:
```bash
git checkout -- tests/fixtures/partial-rebuild/packages/pkg-a/src/index.ts
find tests/fixtures -type d -name dist -exec rm -rf {} + 2>/dev/null
rm -f tests/fixtures/distributed/.rage-hub-addr.json
git status tests/fixtures/
```
Expected: `git status` shows nothing modified (dist/ dirs are ignored or absent).

**Step 2: Run the full harness**

Run: `node tests/fixtures/run.mjs 2>&1 | tee /tmp/run-all.log`

Expected at the very end:
```
=== SUMMARY ===
All 5 fixture(s) PASSED.
```
Exit code 0. Confirm: `echo $?` → `0`.

**Step 3: Confirm no fixture sources got mutated and left behind**

Run: `git status tests/fixtures/`
Expected: nothing modified (the partial-rebuild reset happens out-of-band; if `index.ts` shows modified, the harness left state behind — that's a harness bug we should NOT fix by adding cleanup, since CI runs in a fresh checkout. Just ensure the dev workflow knows to reset).

If anything is dirty, run: `git checkout -- tests/fixtures/`. (No commit — this is just dev hygiene.)

**Step 4: No commit (no source change). Proceed to Task 8.**

---

## Task 8: Add the `e2e-smoke` CI job

**Files:**
- Modify: `.github/workflows/ci.yml`

**Step 1: Read the current ci.yml to confirm insertion point**

Run: `tail -25 .github/workflows/ci.yml`
Expected: ends with the `sandbox-smoke-windows` job's `Run Windows smoke test` step in `pwsh` shell. Confirm the file ends with a single trailing newline (the last visible character is `pwsh` followed by newline).

**Step 2: Append the `e2e-smoke` job at the end of the file**

The new job must be at the same indentation level as the other top-level jobs (2 spaces, since each job key is nested under `jobs:`). Append exactly this — and make sure your editor keeps spaces, not tabs:

```yaml

  # E2E fixture smoke tests — runs the cross-platform Node.js harness against
  # all 5 committed fixture workspaces. Verifies cache correctness, partial
  # rebuilds, error propagation, diamond dependencies, and distributed exec.
  e2e-smoke:
    name: E2E fixture smoke tests (${{ matrix.os }})
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
      fail-fast: false
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: pnpm/action-setup@v4
        with:
          version: 9
      - uses: actions/setup-node@v4
        with:
          node-version: '24'
      - uses: actions/cache@v4
        with:
          path: ~/.local/share/pnpm/store
          key: fixtures-pnpm-${{ matrix.os }}-${{ hashFiles('tests/fixtures/**/pnpm-lock.yaml') }}
      - name: Build rage
        run: cargo build --workspace
      - name: Run e2e fixture smoke tests
        run: node tests/fixtures/run.mjs
```

**Step 3: Validate YAML structure**

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))" && echo OK`
Expected: `OK`.

If it errors with a YAML parse error, the indentation is off. Re-read the file and fix.

**Step 4: Confirm the job appears at the right level**

Run: `grep -n '^  [a-z-]\+:$' .github/workflows/ci.yml`
Expected output (these are the top-level job keys, in order):
```
14:  test:
58:  build:
107:  sandbox-smoke-macos:
129:  sandbox-smoke-linux:
154:  sandbox-smoke-windows:
???:  e2e-smoke:
```
The exact line numbers may vary slightly — what matters is `e2e-smoke:` appears at indentation level 2 (two leading spaces) and is the last entry.

**Step 5: Confirm no tabs sneaked in**

Run: `grep -Pn '\t' .github/workflows/ci.yml`
Expected: no output. (GitHub Actions YAML parsers are unforgiving about tabs.)

**Step 6: Run the harness one final time to make sure nothing regressed**

Run:
```bash
git checkout -- tests/fixtures/partial-rebuild/packages/pkg-a/src/index.ts
find tests/fixtures -type d -name dist -exec rm -rf {} + 2>/dev/null
rm -f tests/fixtures/distributed/.rage-hub-addr.json
node tests/fixtures/run.mjs 2>&1 | tail -5
```
Expected ends with: `All 5 fixture(s) PASSED.` and exit 0.

**Step 7: Commit**

```
git checkout -- tests/fixtures/partial-rebuild/packages/pkg-a/src/index.ts
git add tests/fixtures/run.mjs .github/workflows/ci.yml
git commit -m "feat(ci): add e2e-smoke cross-platform fixture harness + CI job"
```

If `.gitignore` was modified back in Task 6, include it:
```
git add .gitignore
git commit --amend --no-edit
```

**Step 8: Final verification — git log and clean tree**

Run: `git log --oneline -10`
Expected: top entries include the commits from Tasks 1–8.

Run: `git status`
Expected: working tree clean (nothing modified, nothing untracked apart from any files outside `tests/fixtures/` and `.github/workflows/`).

---

## Reference: Useful commands while working

- Run a single fixture: `node tests/fixtures/run.mjs --fixture cache-correctness`
- Run all fixtures: `node tests/fixtures/run.mjs`
- Run only the harness self-test: `RUN_SELF_TEST=1 node tests/fixtures/run.mjs`
- Syntax-check: `node --check tests/fixtures/run.mjs`
- Validate ci.yml: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"`
- Reset a mutated fixture: `git checkout -- tests/fixtures/partial-rebuild/packages/pkg-a/src/index.ts`
- Wipe all dist/ directories: `find tests/fixtures -type d -name dist -exec rm -rf {} +`
- Override the rage binary path: `RAGE_BIN=/custom/path/to/rage node tests/fixtures/run.mjs`

## Reference: What to do if a fixture genuinely fails

If during Task 3, 4, 5, 6, or 7 a fixture fails with a counter mismatch, **do not adjust the harness's expected values**. The expected values come from the design doc and represent the correct behavior of rage. A mismatch means either:

1. A real bug in rage (caching, dependency ordering, error propagation, etc.) — report it, do not paper over.
2. A bug in the Phase 1 fixture content — fix the fixture, not the harness.
3. A bug in the harness code you just wrote — fix it.

The harness exists precisely to catch (1). Trust the assertions.
