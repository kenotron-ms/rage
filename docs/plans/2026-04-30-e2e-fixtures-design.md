# Rage E2E Fixture Smoke Test Suite Design

## Goal

Build a cross-platform CI smoke test suite for the rage build orchestrator using LLM-generated TypeScript monorepo fixtures that verify correctness *behaviors* — not just that the tool compiles. Exercises rage on Linux, macOS, and Windows in CI.

## Background

Rage currently has unit tests in each crate, but no end-to-end coverage that runs the actual binary against realistic monorepo workloads on all three target platforms. We need confidence that:

- Caching is correct (second run rebuilds nothing).
- Partial rebuilds only touch changed packages and their dependents.
- Failures propagate to dependents but do not poison siblings.
- Diamond dependencies build the shared node exactly once.
- Distributed execution (hub + spoke) works end-to-end against a real DAG.

A fixture-based smoke suite catches regressions in cache hashing, dependency ordering, error propagation, and the distributed executor — none of which a unit test can fully exercise.

## Approach

**Generate once, commit.** Fixtures are produced by an LLM (one-time, offline), reviewed, and committed to the repo. CI never calls an LLM API — it just runs the deterministic fixtures through a Node.js harness.

**Behavior-focused, not topology-focused.** Each fixture is named for the property it verifies (`cache-correctness`, `partial-rebuild`, …) rather than its graph shape. This keeps the suite extensible: adding a new behavior is one new directory + one new entry in the harness array.

**Real TypeScript with sentinel counters.** Each package compiles real `.ts` source via `tsc --build` and then writes (or increments) `dist/run-count.txt`. The harness reads those counter files and asserts exact values — that's how we know what actually ran.

**Independent pnpm workspaces.** Each fixture is its own pnpm workspace with isolated `node_modules` and isolated rage cache. No cross-fixture contamination.

**Data-driven harness.** A single `tests/fixtures/run.mjs` interprets a `FIXTURES` array of `{name, steps[], mode?}` records. New scenarios are pure data.

## Architecture

```
tests/fixtures/
├── run.mjs                       ← cross-platform harness (Node 24 ESM)
├── cache-correctness/            ← independent pnpm workspace
├── partial-rebuild/              ← independent pnpm workspace
├── error-propagation/            ← independent pnpm workspace
├── diamond-dep/                  ← independent pnpm workspace
└── distributed/                  ← independent pnpm workspace + rage.json
```

Each workspace declares its package graph entirely via `package.json` `dependencies` (e.g. `"@fix-pr/pkg-b": "workspace:*"`). No `lage.config.js`, no `rage.json` pipeline section — rage derives build order from the dependency graph automatically. The one exception is `distributed/`, which ships a minimal `rage.json` to configure the cache backend used for spoke artifact routing.

## Components

### Fixture Taxonomy (5 fixtures)

| Fixture | Tests | Graph | Key assertion |
|---|---|---|---|
| `cache-correctness` | Second run rebuilds nothing | A → B → C (linear chain) | After run 2, no changes: all counters still = 1 |
| `partial-rebuild` | Only changed packages + dependents rebuild | A → B → C, D (independent) | After touching A: A=2, B=2, C=2, D=1 |
| `error-propagation` | Failed packages block dependents, not siblings | A (independent), B (exits 1), C depends-on B | pkg-a counter=1, pkg-b counter=null, pkg-c counter=null |
| `diamond-dep` | Shared package builds exactly once | S → A, S → B, A+B → App | S counter=1 (not 2, despite two dependents) |
| `distributed` | Hub + spoke execute a DAG end-to-end | A, B (independent) → C(A), D(B) → E(C,D) | All 5 counters=1 after hub exits |

### Per-Package Layout (representative)

```
cache-correctness/
├── pnpm-workspace.yaml          ← packages: ['packages/*']
├── package.json                 ← { "private": true }
├── tsconfig.base.json           ← shared compiler options
└── packages/
    ├── pkg-core/
    │   ├── package.json         ← scripts.build runs tsc + counter increment
    │   ├── tsconfig.json        ← extends ../../tsconfig.base.json
    │   └── src/index.ts         ← ~10 lines, exports a typed const
    ├── pkg-utils/               ← deps: { "@fix-cc/pkg-core": "workspace:*" }
    └── pkg-app/                 ← deps: { "@fix-cc/pkg-utils": "workspace:*" }
```

### Counter Build Script (identical for every package)

```json
"build": "tsc --build && node -e \"const fs=require('fs');fs.mkdirSync('dist',{recursive:true});const n=+(fs.existsSync('dist/run-count.txt')?fs.readFileSync('dist/run-count.txt','utf8').trim():'0')+1;fs.writeFileSync('dist/run-count.txt',String(n))\""
```

The counter file at `dist/run-count.txt` is the single source of truth for "did this package actually execute on this run?"

### Harness (`tests/fixtures/run.mjs`)

A single ESM script. No bash conditionals, no PowerShell — Node.js APIs throughout for cross-platform parity.

```js
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
]
```

**Step types interpreted by `runFixture(fixture)`:**

| Step | Meaning |
|---|---|
| `{ run: true }` | `pnpm install` (once per fixture session), then `rage run build <fixture-dir>` |
| `{ run: true, expectFailure: true }` | Same, but asserts non-zero exit code |
| `{ assert: { pkg: N } }` | Read `packages/<pkg>/dist/run-count.txt`; `null` = file must be absent |
| `{ mutate: 'path/to/file', append: '...' }` | `appendFileSync` to invalidate cache |
| `mode: 'distributed'` on fixture | `run` step spawns hub + spoke via `child_process.spawn`, waits for hub exit |

**RAGE_BIN resolution:**

```js
const RAGE_BIN = process.env.RAGE_BIN
  ?? (process.platform === 'win32'
      ? '.\\target\\debug\\rage.exe'
      : './target/debug/rage');
```

**CLI surface:**

- `node tests/fixtures/run.mjs` — run all fixtures; collect failures; exit 1 if any failed.
- `node tests/fixtures/run.mjs --fixture <name>` — run one scenario.

### Distributed Mode

The `distributed` fixture runs hub and spoke as concurrent child processes via `child_process.spawn` (not `execSync`). The hub exits when the DAG completes; the harness waits on that exit code as the success/failure signal. Spoke is spawned alongside and torn down when the hub exits.

## Data Flow

1. CI builds rage (`cargo build --workspace`).
2. Harness iterates `FIXTURES` array.
3. For each fixture: `pnpm install` (once), then walk `steps[]` in order.
4. Each `run` step shells out to `rage run build <fixture-dir>`.
5. `assert` steps read `dist/run-count.txt` files and compare against expected map.
6. Failures collected; harness reports summary; exits 1 if any failed.

## Error Handling

- **`run` step failure** (unexpected non-zero exit): logged as fixture failure, harness continues to next fixture.
- **`run` step with `expectFailure: true`**: zero exit code is the failure (we *expected* a build error).
- **`assert` mismatch**: prints expected vs actual counter map for the fixture, marks fixture failed, continues.
- **Missing `dist/run-count.txt`**: treated as `null`. An assertion of `null` passes when absent; an assertion of `N` fails with a clear message.
- **`pnpm install` failure**: aborts that fixture only; other fixtures still run.

All failures are accumulated and reported at the end. The harness exits non-zero if *any* fixture failed.

## DE Bug Prerequisites

The distributed fixture cannot work until three known bugs in rage's distributed executor are fixed. They ship in the **same PR** as the fixtures — no skip guards, no feature flags. If the fixes regress, the distributed fixture breaks CI, and that is the correct behavior.

| Bug | File | Fix |
|---|---|---|
| `workspace_root: "/workspace"` hardcoded | `crates/hub/src/server.rs:95` | Pass actual workspace path from `cmd_hub` through to WorkItem |
| `sh -c` in spoke executor | `crates/spoke-client/src/lib.rs:140` | Use `scheduler::shell::std_command(cmd)` (same fix as Phase 1 runner.rs) |
| `mark_failed` doesn't unblock dependents | `crates/hub/src/dag.rs:113` | Mark downstream dependents as `Failed` state, flush ready queue |

## CI Integration

New job `e2e-smoke` in `.github/workflows/ci.yml`:

```yaml
e2e-smoke:
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
    - name: Run fixture smoke tests
      run: node tests/fixtures/run.mjs
```

Notes:
- `RAGE_BIN` env var is **not** set — the harness auto-detects the binary path based on `process.platform`.
- Each fixture commits its `pnpm-lock.yaml` so cache keys are stable.
- `fail-fast: false` so a single-platform regression doesn't mask the others.

## Testing Strategy

The fixtures **are** the tests. They run in CI on every PR across all three platforms. Local development:

- `node tests/fixtures/run.mjs` — full suite locally.
- `node tests/fixtures/run.mjs --fixture cache-correctness` — single scenario.
- `RAGE_BIN=/custom/path/rage node tests/fixtures/run.mjs` — override binary path.

The harness itself is small (≈200 lines), data-driven, and intentionally not unit-tested — its only job is to orchestrate fixture runs. Bugs in the harness will surface as obviously wrong assertion behavior on all platforms simultaneously.

## Open Questions

None — all key decisions made during design session.
