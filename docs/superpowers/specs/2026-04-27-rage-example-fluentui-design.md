# Design: rage-example-fluentui

**Date:** 2026-04-27  
**Status:** Approved  
**Scope:** Personal fork of microsoft/fluentui with all Nx/just-scripts tooling replaced by rage

---

## What This Is

A full fork of `microsoft/fluentui` used as a realistic benchmark for rage. The fork replaces the entire Nx + just-scripts task orchestration layer with rage, demonstrating how little configuration rage needs to orchestrate a 115+ package TypeScript monorepo.

This is a personal benchmark repo — not a contribution to or PR against the upstream fluentui project.

---

## Goals

1. **Benchmark** — exercise rage's scheduler, wave-parallel executor, two-phase cache, sandbox, and TypeScript plugin against a real-world 115+ package monorepo
2. **Demonstrate simplicity** — show that rage's TypeScript plugin auto-discovers everything Nx requires explicit configuration for
3. **Validate correctness** — confirm `rage run build` / `typecheck` / `lint` / `test` produce correct results across all packages

---

## Repo Structure

**Name:** `rage-example-fluentui`  
**Owner:** Personal GitHub (not a Microsoft org repo)  
**Base:** Fork of `microsoft/fluentui` pinned at a specific commit (HEAD at fork time)

### Branch Strategy

```
microsoft/fluentui (upstream)
        │
        │ fork (pinned at HEAD)
        ▼
main ──────────────────────────────────────────  (exact copy of upstream, never modified)
        │
        │ one commit: "chore: replace Nx + just-scripts with rage"
        ▼
rage ────●─────────────────────────────────────  (the benchmark — this is what we run)
```

- `main` = upstream fluentui, frozen. Exists purely so `git diff main rage` tells the complete migration story at a glance.
- `rage` = the working benchmark branch. All benchmark runs happen here.

---

## The Migration Commit

A single commit on the `rage` branch replaces the entire Nx + just-scripts layer with rage.

### Files Removed

| File / Directory | Why |
|---|---|
| `nx.json` | rage discovers task topology from tsconfig project references — no manual pipeline config needed |
| `tools/workspace-plugin/` | Custom Nx inference plugin — rage's TypeScript plugin handles inference natively |
| Per-package `project.json` files | Nx per-project target definitions — not needed by rage |
| `just-scripts` devDependency | rage's TypeScript plugin runs `tsc` directly, bypassing just-tasks |
| Nx devDependencies | `nx`, `@nx/devkit`, `@nx/eslint`, `@nx/jest`, `@nx/js`, `@nx/workspace`, `@nx/node`, `@nx/plugin` |

### Files Added

```json
// rage.json (added at repo root)
{
  "plugins": ["rage-typescript"]
}
```

That's the entire rage configuration. The TypeScript plugin takes care of the rest.

### Root `package.json` Script Changes

```diff
  "scripts": {
-   "clean": "nx run-many -t clean --verbose",
+   "build":     "rage run build",
+   "typecheck": "rage run typecheck",
+   "test":      "rage run test",
+   "lint":      "rage run lint",
    ...
  },
  "devDependencies": {
-   "nx":            "21.6.10",
-   "@nx/devkit":    "21.6.10",
-   "@nx/eslint":    "21.6.10",
-   "@nx/jest":      "21.6.10",
-   "@nx/js":        "21.6.10",
-   "@nx/workspace": "21.6.10",
-   "@nx/node":      "21.6.10",
-   "@nx/plugin":    "21.6.10",
-   "just-scripts":  "1.8.2",
    ...
  }
```

---

## How rage Handles FluentUI

### Task Discovery

rage's TypeScript plugin scans every package for `tsconfig.json`. All 115+ fluentui packages have one. For each matched package, the plugin infers two tasks:

| Task | Command | Inputs | Outputs |
|---|---|---|---|
| `build` | `tsc` | `src/**/*.ts(x)`, `tsconfig*.json`, `package.json` | `dist/**`, `lib/**`, `**/*.d.ts` |
| `typecheck` | `tsc --noEmit` | same | (none) |

For `lint` and `test`, rage runs the script defined in each package's `package.json` directly (ESLint and Jest respectively), ordered by the package dependency graph.

### Ordering

rage reads TypeScript project references from `tsconfig.json` to build the dependency DAG. FluentUI uses project references extensively (e.g., `@fluentui/react-button` references `@fluentui/react-utilities`, `@fluentui/react-theme`), so rage's ordering is automatically correct.

### Caching

Two-phase cache keyed on:
1. **WF (weak fingerprint):** command + tool binary hash + declared input globs + env
2. **SF (strong fingerprint):** WF + actual files accessed during execution (from sandbox)

ABI fingerprint for early cutoff: blake3 of all `.d.ts` outputs. If a dependency's `.d.ts` didn't change, downstream packages skip their build.

### Sandbox

- **macOS:** `DYLD_INSERT_LIBRARIES` Mach-O dylib intercepts file-access syscalls
- **Linux:** eBPF tracepoints via aya

All file accesses are recorded per task, feeding the two-phase cache.

---

## FluentUI Package Scale

| Area | Package Count (approx) |
|---|---|
| `packages/react-components/*` | ~80 |
| `packages/tokens` | 1 |
| `packages/react-*` (v8 compat) | ~10 |
| `scripts/*`, `tools/*` | ~15 |
| `apps/*` | ~5 |
| **Total** | **~115** |

All packages are yarn workspaces. The yarn lockfile and `.yarnrc.yml` are read by rage's TypeScript plugin for postinstall policy.

---

## What rage Does NOT Replace

These parts of fluentui remain untouched — they're outside rage's scope:

| Tooling | Role | Stays? |
|---|---|---|
| `beachball` | Versioning and changelog | ✅ Yes |
| `husky` | Git hooks | ✅ Yes |
| `eslint` | Linting (rage just runs it) | ✅ Yes |
| `jest` | Testing (rage just runs it) | ✅ Yes |
| `storybook` | Story builds (not part of benchmark) | ✅ Yes (unused) |
| `cypress` / `playwright` | E2E (not part of benchmark) | ✅ Yes (unused) |
| `webpack` | Bundle builds (not part of benchmark) | ✅ Yes (unused) |

The benchmark focuses on `rage run build`, `rage run typecheck`, `rage run lint`, and `rage run test` — the core TypeScript compilation and testing pipeline.

---

## Known Friction Points

These will need investigation during implementation:

1. **`tsconfig.lib.json` vs `tsconfig.json`** — FluentUI per-package builds typically use `tsc -p tsconfig.lib.json` (not just `tsc`). rage's TypeScript plugin infers `tsc` against `tsconfig.json`. Need to verify outputs are equivalent or adjust plugin config glob.

2. **Per-package `project.json` removal** — Need to confirm how many packages have `project.json` files and whether removing them causes any non-Nx scripts to break.

3. **`tools/workspace-plugin/` scope** — This plugin does more than Nx inference (generates version files, validates packaging). The Nx inference parts go away; other generators may remain as standalone scripts.

4. **`postinstall` script** — Root `package.json` has `"postinstall": "yarn patch-package && husky && node ./scripts/package-manager/src/postinstall.js"`. This is workspace-install postinstall, separate from per-package postinstalls. rage's postinstall cache handles per-package postinstalls; the root postinstall remains as-is.

5. **macOS 26 / Tahoe sandbox caveat** — rage's `DYLD_INSERT_LIBRARIES` approach is fragile on macOS 26+. Benchmark should note this and prefer Homebrew bash where needed.

---

## Implementation Steps

1. Fork `microsoft/fluentui` on GitHub → `rage-example-fluentui`
2. Pin `main` branch to the fork HEAD (no further changes to `main`)
3. Create `rage` branch from `main`
4. Remove Nx devDependencies from root `package.json`
5. Remove `just-scripts` from root `package.json`
6. Delete `nx.json`
7. Delete per-package `project.json` files (find and remove all)
8. Delete or stub `tools/workspace-plugin/` (preserve non-Nx parts as standalone scripts if needed)
9. Add `rage.json` at repo root
10. Update root `package.json` scripts
11. Run `yarn install` to update lockfile
12. Smoke test: `rage graph` (verify DAG is built correctly)
13. Smoke test: `rage run build --affected` (verify a subset builds cleanly)
14. Full run: `rage run build` (benchmark)
15. Cache run: `rage run build` again (verify cache hits)
16. Write `RAGE.md` at repo root documenting the migration and benchmark results

---

## Success Criteria

- [ ] `rage graph` produces a valid DOT graph with ~115 nodes
- [ ] `rage run build` completes without errors on the `rage` branch
- [ ] Second `rage run build` shows `(cached, two-phase)` for all unchanged packages
- [ ] `rage run typecheck` runs cleanly
- [ ] `git diff main rage` tells the complete, self-contained migration story
- [ ] Wall-clock time and cache hit rate documented in `RAGE.md`
