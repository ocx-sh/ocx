# Plan: setup-ocx GitHub Action & CI Export Command

## Overview

**Status:** In Progress
**Date:** 2026-03-14
**Related ADR:** N/A

## Objective

Create two GitHub Actions and a new `ocx ci export` CLI command that together enable OCX to manage tool installations in CI environments. This is the first dogfooding step — proving OCX works in production CI by migrating this repo's go-task installation away from third-party setup-* actions.

### Design Principles

1. **Stable CI, testable dev binary.** The CI *infrastructure* (installing go-task, running `task verify`) uses a released, stable OCX. The CI *tests* (acceptance tests) test the development binary. These are separate concerns and must never be conflated.

2. **Two actions, one responsibility each.** `setup-ocx` downloads a released OCX binary. `ocx-install` installs packages using whatever `ocx` is already on PATH. This separation solves the chicken-and-egg problem and enables reuse.

3. **TypeScript from day one.** Build a proper JS action with `@actions/toolkit` now, designed for extraction to `ocx-sh/setup-ocx` later. No throwaway composite action to rewrite.

4. **Dedicated CI command, not a shell variant.** CI export is not a shell — it's a file-append protocol. Keep `Shell` enum clean for real shells. A new `ocx ci export` command with auto-detection and `--flavor` override is the right abstraction.

## Scope

### In Scope

- `ocx ci export` command with `CiFlavor` enum (new Rust code)
- TypeScript action `setup-ocx` at `.github/actions/setup-ocx/`
- Lightweight action `ocx-install` at `.github/actions/ocx-install/`
- Migration of go-task installation in CI workflows

### Out of Scope

- Dedicated `ocx-sh/setup-ocx` repository (future extraction)
- Caching `$OCX_HOME` across runs with `@actions/tool-cache` (future optimization, but the TypeScript structure supports it)
- Migrating other tools beyond go-task (Phase 4 of dogfood initiative)
- Windows support in actions (no CI workflows use go-task on Windows today)
- GitLab CI / CircleCI flavors (only GitHub Actions for now, but the `CiFlavor` enum is extensible)

## Technical Approach

### Architecture

```
                    ┌─────────────────────────────────────┐
                    │         CI Workflow                  │
                    │                                     │
                    │  ┌───────────────┐                  │
                    │  │  setup-ocx    │ Downloads released│
                    │  │  (TypeScript) │ OCX from GitHub   │
                    │  │               │ Releases          │
                    │  └───────┬───────┘                  │
                    │          │ ocx now on PATH           │
                    │          ▼                           │
                    │  ┌───────────────┐                  │
                    │  │  ocx-install  │ Calls ocx install │
                    │  │  (composite)  │ + ocx ci export   │
                    │  │               │                   │
                    │  └───────┬───────┘                  │
                    │          │ packages installed,       │
                    │          │ env exported              │
                    │          ▼                           │
                    │  ┌───────────────┐                  │
                    │  │ Subsequent    │ Tools available   │
                    │  │ steps         │ via PATH + env    │
                    │  └───────────────┘                  │
                    └─────────────────────────────────────┘
```

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| TypeScript action, not composite | Enables `post:` hook for cache saving, `@actions/tool-cache` integration, proper outputs. Designed for extraction to standalone repo. |
| Two separate actions | `setup-ocx` handles OCX binary installation (one concern). `ocx-install` handles package installation (second concern). Decoupled: `ocx-install` works with any OCX on PATH — built from source, pre-installed, or from `setup-ocx`. |
| `ocx ci export` command, not `Shell::GitHubActions` | CI export is a file-append protocol, not a shell. Adding it to the `Shell` enum would require special-casing in `detect()`, `from_path()`, `profile_builder()`, and `try_into::<clap_complete::Shell>()`. A dedicated command with its own `CiFlavor` enum is a cleaner abstraction. |
| `ocx-install` is composite, not TypeScript | It just calls `ocx install` + `ocx ci export`. No platform detection, no downloads, no caching. Shell steps are sufficient. |
| Stable OCX for CI infrastructure | The released OCX installs go-task. The dev binary is only used in acceptance tests via `OCX_COMMAND`. If the dev OCX has a bug, CI infrastructure still works. |

### Comparison with setup-uv / setup-node

| Aspect | setup-uv | setup-node | setup-ocx |
|--------|----------|------------|-----------|
| Type | JS (node24) | JS (node24) | JS (node24) |
| Build pipeline | TypeScript → ncc | TypeScript → ncc | TypeScript → ncc |
| Inputs | 27 | 14 | ~3 (version, github-token, checksum) |
| Post hook | Cache save | Cache save | Cache save (future) |
| Checksum | Version manifest | None | install.sh verifies SHA-256 |
| Installs packages? | No | No | No (separate `ocx-install` action) |
| Env export | Hardcoded UV_* vars | Just PATH | `ocx ci export` (dynamic per-package) |

The fundamental difference: setup-uv and setup-node **are** package managers rebuilt in TypeScript. setup-ocx delegates to OCX — the complexity lives in the Rust binary, not the action.

### Usage Patterns

**External repos:**
```yaml
- uses: ocx-sh/setup-ocx@v1
  with:
    version: '0.5.0'
- uses: ocx-sh/ocx-install@v1
  with:
    packages: 'cmake:3.28 go-task'
```

**OCX repo — stable CI infrastructure:**
```yaml
- uses: ./.github/actions/setup-ocx
  with:
    version: '0.5.0'          # pinned released version
- uses: ./.github/actions/ocx-install
  with:
    packages: 'go-task'
```

**OCX repo — testing dev binary (acceptance tests):**
```yaml
# Build step produces test/bin/ocx
# Acceptance tests use OCX_COMMAND=test/bin/ocx
# No setup-ocx needed — dev binary is not used for infrastructure
```

## Implementation Steps

### Phase 1: `ocx ci export` command

- [x] **Step 1.1:** Create `CiFlavor` enum in `ocx_lib`
  - Files: `crates/ocx_lib/src/ci.rs` (new module)
  - Details:
    ```rust
    pub enum CiFlavor {
        GitHubActions,
        // Future: GitLabCi, CircleCi, ...
    }
    ```
    Implement `export_path(key, value)` and `export_constant(key, value)` on `CiFlavor`:
    - **GitHub Actions** PATH-type where key is `PATH`:
      `echo "{value}" >> "$GITHUB_PATH"`
    - **GitHub Actions** PATH-type where key is not PATH:
      `echo "{KEY}={value}{sep}${KEY}" >> "$GITHUB_ENV"`
    - **GitHub Actions** constant-type:
      `echo "{KEY}={value}" >> "$GITHUB_ENV"`
    - Multiline values use the delimiter syntax:
      ```
      echo "KEY<<EOF" >> "$GITHUB_ENV"
      echo "{value}" >> "$GITHUB_ENV"
      echo "EOF" >> "$GITHUB_ENV"
      ```
    Implement `detect() -> Option<CiFlavor>`:
    - `$GITHUB_ACTIONS == "true"` → `Some(GitHubActions)`
    - else → `None`
    Implement `clap::ValueEnum` with `github-actions` as the CLI name.

- [x] **Step 1.2:** Create `ci export` CLI subcommand
  - Files: `crates/ocx_cli/src/command/ci.rs` (new), `crates/ocx_cli/src/command/ci_export.rs` (new)
  - Details: New `Ci` subcommand group (like `Shell` group) with `Export` subcommand. Structure:
    ```
    ocx ci export <PACKAGES...> [--flavor github-actions] [-p platform]
    ocx ci export <PACKAGES...> --candidate|--current [--flavor github-actions]
    ```
    - `--flavor` is optional; auto-detects from environment if omitted
    - Reuses the same package resolution logic as `shell_env.rs` (find_all / find_symlink_all — no auto-install)
    - Reuses `Exporter` from `package::metadata::env::exporter` for variable resolution
    - Output: prints the CI-specific export commands to stdout (the action pipes this to eval or runs it)
  - Register in main command enum.

- [x] **Step 1.3:** Write unit tests for `CiFlavor`
  - Files: `crates/ocx_lib/src/ci.rs` (test module)
  - Tests: PATH export → `$GITHUB_PATH`, constant export → `$GITHUB_ENV`, path-type non-PATH var → `$GITHUB_ENV` with separator, multiline value escaping, `detect()` with/without `$GITHUB_ACTIONS`.

- [x] **Step 1.4:** Write acceptance test for `ocx ci export`
  - Files: `test/tests/test_ci_export.py` (new)
  - Tests:
    - `ocx ci export <pkg> --flavor github-actions` produces correct `echo ... >> "$GITHUB_PATH"` / `"$GITHUB_ENV"` output
    - Auto-detection works when `GITHUB_ACTIONS=true` is set
    - Error when no flavor specified and no CI environment detected

### Phase 2: `setup-ocx` TypeScript action

- [x] **Step 2.1:** Initialize TypeScript project
  - Files: `.github/actions/setup-ocx/`
  - Details: Create project structure:
    ```
    .github/actions/setup-ocx/
    ├── src/
    │   ├── setup.ts        # main: download, verify, extract, addPath
    │   ├── download.ts     # platform detect, URL construction, SHA256
    │   ├── cache-save.ts   # post hook (stub for now)
    │   └── constants.ts    # repo, archive naming patterns
    ├── action.yml
    ├── package.json        # @actions/core, @actions/tool-cache, @actions/exec
    ├── tsconfig.json
    └── README.md           # minimal usage docs
    ```
  - Dependencies: `@actions/core`, `@actions/tool-cache`, `@actions/exec`, `@actions/http-client`, `@vercel/ncc` (dev)
  - Build: `ncc build src/setup.ts -o dist/setup && ncc build src/cache-save.ts -o dist/cache-save`
  - The `dist/` directory is committed (standard for GitHub Actions).

- [x] **Step 2.2:** Create action.yml
  - Files: `.github/actions/setup-ocx/action.yml`
  - Details:
    ```yaml
    name: 'Setup OCX'
    description: 'Install the OCX package manager'
    inputs:
      version:
        description: 'OCX version to install (exact version, not "latest")'
        required: true
      github-token:
        description: 'GitHub token for API requests and release downloads'
        default: ${{ github.token }}
    outputs:
      version:
        description: 'The installed OCX version'
      ocx-path:
        description: 'Path to the OCX binary'
    runs:
      using: node24
      main: dist/setup/index.js
      post: dist/cache-save/index.js
      post-if: success()
    ```
  - Note: `version` is required with no default. No `latest` resolution — pin your version. This is a deliberate constraint for reproducibility.

- [x] **Step 2.3:** Implement `setup.ts`
  - Details: Core flow:
    1. Read `version` and `github-token` inputs
    2. Detect platform (`process.platform` → `linux`/`darwin`) and arch (`process.arch` → `x64`/`arm64`)
    3. Map to cargo-dist archive name (e.g., `ocx-x86_64-unknown-linux-gnu.tar.xz`)
    4. Check `tc.find('ocx', version, arch)` — if cached, skip download
    5. Download archive from `https://github.com/ocx-sh/ocx/releases/download/v{version}/{archive}`
    6. Download `sha256sum.txt` from same release, verify checksum
    7. Extract archive (`tc.extractTar` with xz flag)
    8. Cache with `tc.cacheDir(extractedPath, 'ocx', version, arch)`
    9. `core.addPath(binPath)` — makes `ocx` available in subsequent steps
    10. Set outputs: `version`, `ocx-path`

- [x] **Step 2.4:** Implement `cache-save.ts` (stub)
  - Details: For now, just log "cache save not yet implemented". The tool-cache (`tc.cacheDir`) already provides within-job caching. Cross-run caching via `actions/cache` is a future optimization.

### Phase 3: `ocx-install` composite action

- [x] **Step 3.1:** Create action.yml
  - Files: `.github/actions/ocx-install/action.yml`
  - Details:
    ```yaml
    name: 'OCX Install'
    description: 'Install packages and export environment using OCX'
    inputs:
      packages:
        description: 'Space-separated list of packages to install'
        required: true
    runs:
      using: composite
      steps:
        - name: Install packages
          shell: bash
          run: ocx install ${{ inputs.packages }}
        - name: Export environment
          shell: bash
          run: ocx ci export ${{ inputs.packages }}
    ```
  - Note: This action assumes `ocx` is already on PATH (from `setup-ocx` or a build step). It's intentionally minimal.

### Phase 4: Migrate go-task in CI workflows

Migrate one workflow first, verify in a PR, then migrate the rest.

- [ ] **Step 4.1:** Replace `go-task/setup-task` in verify-basic.yml
  - Files: `.github/workflows/verify-basic.yml`
  - Details: Replace both jobs' `go-task/setup-task@v1.0.0` steps with:
    ```yaml
    - uses: ./.github/actions/setup-ocx
      with:
        version: '0.5.0'  # pin to current release
    - uses: ./.github/actions/ocx-install
      with:
        packages: go-task
    ```
  - **Gate:** Verify this workflow passes in a PR before proceeding.

- [ ] **Step 4.2:** Replace in verify-deep.yml
  - Files: `.github/workflows/verify-deep.yml`

- [ ] **Step 4.3:** Replace in deploy-website.yml
  - Files: `.github/workflows/deploy-website.yml`

- [ ] **Step 4.4:** Replace `arduino/setup-task` in verify-licenses.yml
  - Files: `.github/workflows/verify-licenses.yml`
  - Details: Also fixes the inconsistency of using a different action (`arduino/setup-task` vs `go-task/setup-task`).

### Phase 5: Verification

- [ ] **Step 5.1:** Run `task verify` locally
- [ ] **Step 5.2:** Test the action in a real workflow run (PR to main)
- [ ] **Step 5.3:** Verify `task --version` works in subsequent steps across all migrated workflows

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/ci.rs` | Create | `CiFlavor` enum with GitHub Actions export logic + auto-detection |
| `crates/ocx_lib/src/lib.rs` | Modify | Add `pub mod ci;` |
| `crates/ocx_cli/src/command/ci.rs` | Create | `Ci` subcommand group |
| `crates/ocx_cli/src/command/ci_export.rs` | Create | `CiExport` subcommand implementation |
| `crates/ocx_cli/src/command.rs` | Modify | Register `Ci` subcommand |
| `test/tests/test_ci_export.py` | Create | Acceptance tests for `ocx ci export` |
| `.github/actions/setup-ocx/` | Create | TypeScript action (src/, dist/, action.yml, package.json, tsconfig.json) |
| `.github/actions/ocx-install/action.yml` | Create | Composite action for package installation |
| `.github/workflows/verify-basic.yml` | Modify | Replace go-task/setup-task with setup-ocx + ocx-install |
| `.github/workflows/verify-deep.yml` | Modify | Replace go-task/setup-task with setup-ocx + ocx-install |
| `.github/workflows/deploy-website.yml` | Modify | Replace go-task/setup-task with setup-ocx + ocx-install |
| `.github/workflows/verify-licenses.yml` | Modify | Replace arduino/setup-task with setup-ocx + ocx-install |

## Dependencies

### Code Dependencies

| Package | Version | Purpose |
|---------|---------|---------|
| `@actions/core` | latest | GitHub Actions core utilities (inputs, outputs, PATH, env) |
| `@actions/tool-cache` | latest | Download, extract, cache binaries |
| `@actions/exec` | latest | Run commands |
| `@actions/http-client` | latest | HTTP requests for release downloads |
| `@vercel/ncc` | latest (dev) | Bundle TypeScript to single JS file |

### Service Dependencies

| Service | Status | Notes |
|---------|--------|-------|
| GitHub Releases | Available | OCX must have at least one published release |
| `ocx.sh` registry | Available | go-task must be published at `ocx.sh/go-task` |

## Testing Strategy

### Unit Tests

| Component | Test Cases |
|-----------|------------|
| `CiFlavor::GitHubActions` | PATH export → `$GITHUB_PATH`, constant export → `$GITHUB_ENV`, path-type non-PATH var → `$GITHUB_ENV` with separator, multiline value escaping |
| `CiFlavor::detect()` | Returns `GitHubActions` when `$GITHUB_ACTIONS=true`, returns `None` otherwise |

### Acceptance Tests

| Scenario | Expected Outcome |
|----------|------------------|
| `ocx ci export <pkg> --flavor github-actions` | Correct `echo ... >> "$GITHUB_PATH"` / `"$GITHUB_ENV"` output |
| `ocx ci export <pkg>` with `GITHUB_ACTIONS=true` | Auto-detects flavor, same output |
| `ocx ci export <pkg>` without CI env vars | Error: "Could not detect CI environment. Use --flavor to specify." |

### Integration Tests

| Scenario | Expected Outcome |
|----------|------------------|
| CI workflow using `setup-ocx` + `ocx-install` with `packages: go-task` | `task --version` works in subsequent steps |

### Manual Testing

- [ ] Run `setup-ocx` action locally with `act` (if available) or in a test workflow
- [ ] Verify `ocx ci export` output is valid when piped to `eval` in a bash step

## Rollback Plan

1. Revert workflow changes to use `go-task/setup-task` again (single commit revert)
2. The `ocx ci export` command and actions remain in the repo but are unused — no harm
3. No data migration needed — CI runners are ephemeral

## Risks

| Risk | Mitigation |
|------|------------|
| install.sh download fails in CI (rate limits) | `github-token` input passed via `GITHUB_TOKEN` env var; tool-cache avoids re-downloading on cache hit |
| OCX release doesn't exist for a platform | Only use on platforms with published releases; `setup-ocx` fails clearly if archive not found |
| `ocx ci export` output format breaks GitHub Actions parsing | Multiline value handling with EOF delimiter; unit test all edge cases |
| Workflow migration breaks CI | Migrate verify-basic.yml first, verify in PR, then migrate the rest |
| `ocx.sh` registry down → go-task install fails | CI now depends on registry availability. Accepted risk for Phase 1. Future: bundle index snapshot for offline resilience. |
| Action requires a published OCX release | Cannot dogfood before first release. This is fine — OCX has existing releases. Document the dependency. |
| TypeScript build adds maintenance burden | Minimal code (~200 LOC). ncc bundles everything. No runtime deps. Build is a single `bun run build` command. |

## Notes

- **install.sh already has CI detection:** When `$GITHUB_PATH` is set, it appends the OCX bin directory. The bootstrap command uses `ocx --remote install --select ocx.sh/ocx:VERSION`, so the `current` symlink exists and the PATH entry resolves correctly.
- **go-task is published** at `ocx.sh/go-task` (production registry). The mirror config is at `mirrors/mirror-go-task.yml`.
- **Version pinning is mandatory.** The `setup-ocx` action requires an exact version — no `latest` default. This is a deliberate constraint for reproducibility. Dependabot can bump the version.
- **Extraction path:** When ready for external consumption, move `.github/actions/setup-ocx/` to `ocx-sh/setup-ocx` repo and `.github/actions/ocx-install/` to `ocx-sh/ocx-install`. The `uses:` references change; the action code stays the same.
- **Future `ocx ci` subcommands:** The `ci` command group can later host `ocx ci cache` (cache hints), `ocx ci summary` (GitHub step summaries), `ocx ci detect` (print detected CI environment).
- **`ocx ci export` vs `ocx shell env`:** Both resolve package environment variables. `shell env` prints `export KEY=value` for eval in interactive shells. `ci export` prints `echo KEY=value >> "$GITHUB_ENV"` for CI file-append protocols. Different output mechanisms, same underlying env resolution.

---

## Progress Log

| Date | Update |
|------|--------|
| 2026-03-14 | Initial draft |
| 2026-03-14 | Revised: TypeScript action, two-action split, `ocx ci export` command, stable/dev binary separation |
