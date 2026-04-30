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
      { mutate: 'packages/pkg-a/src/index.ts', append: '\nexport const _cacheBust = 1;' },
      { run: true },
      { assert: { 'pkg-a': 2, 'pkg-b': 2, 'pkg-c': 1, 'pkg-d': 1 } },
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

// --- Helpers ---

function readCounter(fixtureDir, pkg) {
  const filePath = join(fixtureDir, 'packages', pkg, 'dist', 'run-count.txt');
  if (!existsSync(filePath)) return null;
  const raw = readFileSync(filePath, 'utf8');
  return parseInt(raw.trim(), 10);
}

function assert(condition, message) {
  if (!condition) {
    console.error('  FAIL: ' + message);
    return false;
  } else {
    console.log('  PASS: ' + message);
    return true;
  }
}

const installed = new Set();

function ensureInstalled(fixtureDir) {
  if (installed.has(fixtureDir)) return;
  console.log('Installing pnpm deps in ' + fixtureDir + '...');
  execSync('pnpm install --frozen-lockfile', { cwd: fixtureDir, stdio: 'inherit' });
  installed.add(fixtureDir);
}

function runRage(fixtureDir, { expectFailure = false } = {}) {
  try {
    execSync(`"${RAGE_BIN}" run build "${fixtureDir}"`, { cwd: fixtureDir, stdio: 'inherit' });
    if (expectFailure) {
      console.error('  FAIL: rage exited zero but expected non-zero');
      return false;
    } else {
      return true;
    }
  } catch (e) {
    if (expectFailure) {
      console.log('  PASS: rage exited non-zero as expected');
      return true;
    } else {
      console.error('  FAIL: ' + e.message);
      return false;
    }
  }
}

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
      let ok;
      if (expected === null) {
        ok = assert(actual === null, `${pkg}: counter file must be absent (got ${actual})`);
      } else {
        ok = assert(actual === expected, `${pkg}: counter=${actual}, expected=${expected}`);
      }
      if (!ok) allPassed = false;
    }
    return allPassed;
  }

  if (step.mutate !== undefined) {
    const filePath = join(fixtureDir, step.mutate);
    appendFileSync(filePath, step.append ?? '\n// cache-bust');
    console.log(`  Mutated ${step.mutate}`);
    return true;
  }

  console.error('  FAIL: unknown step type');
  return false;
}

// --- Fixture runner ---

async function runFixture(fixture) {
  const fixtureDir = join(__dirname, fixture.name);
  console.log('\n=== ' + fixture.name + ' ===');

  // Standard mode
  for (const step of fixture.steps) {
    const ok = await executeStep(step, fixtureDir, fixture.name);
    if (!ok) return false;
  }
  return true;
}

// --- Entry point ---

async function main() {
  if (process.env.RUN_SELF_TEST) {
    let allOk = true;

    allOk = assert(true === true, 'assert(true) returns true') && allOk;

    const negative = assert(false, 'this fail message is expected');
    if (negative !== false) {
      allOk = false;
      console.error('ERROR: assert(false) should have returned false, got: ' + negative);
    } else {
      console.log('PASS: assert(false) returned false (the FAIL above is expected)');
    }

    const counterResult = readCounter('/tmp/__definitely_does_not_exist__', 'pkg-x');
    allOk = assert(counterResult === null, `readCounter on missing path returns null (got ${counterResult})`) && allOk;

    process.exit(allOk ? 0 : 1);
  }

  // Parse --fixture <name> flag
  const args = process.argv.slice(2);
  let fixtureName = null;
  const fixtureIdx = args.indexOf('--fixture');
  if (fixtureIdx !== -1) {
    fixtureName = args[fixtureIdx + 1];
  }

  const toRun = fixtureName
    ? FIXTURES.filter(f => f.name === fixtureName)
    : FIXTURES;

  if (toRun.length === 0) {
    console.error('No fixture found: ' + fixtureName);
    process.exit(1);
  }

  const failures = [];
  for (const fixture of toRun) {
    const ok = await runFixture(fixture);
    if (!ok) failures.push(fixture.name);
  }

  console.log('\n=== SUMMARY ===');
  if (failures.length === 0) {
    console.log('All ' + toRun.length + ' fixture(s) PASSED.');
    process.exit(0);
  } else {
    console.error('FAILED: ' + failures.join(', '));
    process.exit(1);
  }
}

main().catch(err => {
  console.error('Unhandled error:', err);
  process.exit(1);
});
