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

  // TODO: implement in Task 3
}

main().catch(err => {
  console.error('Unhandled error:', err);
  process.exit(1);
});
