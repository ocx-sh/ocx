---
name: ci-workflows
description: Design, review, and optimize GitHub Actions CI/CD workflows. Use when creating, modifying, or auditing CI pipelines. Covers workflow structure, cost analysis, security, quality gates, and task runner integration.
---

# CI/CD Workflow Engineering

## Workflow

Follow these phases in order when creating or modifying CI workflows.

### Phase 1: Research

Before writing or modifying any workflow YAML:

1. **Read existing workflows** — `Glob` for `.github/workflows/*.yml` and understand current structure
2. **Read the project's Taskfile** — identify which checks already have task definitions
3. **Search the web** for current best practices:
   - GitHub Actions changelog for deprecations and new features
   - SHA-pinned versions of actions you plan to use (never use tag-only references)
   - Alternative actions — compare at least two options per capability
4. **Check action versions** — verify no deprecated v3 artifact/cache actions remain; confirm Node.js runtime compatibility (Node 24+ required for new actions as of 2026)

### Phase 2: Cost Analysis

Evaluate the cost impact of every structural decision:

| Factor | Cost Impact | Guidance |
|--------|------------|----------|
| **Job count** | Each job = 1 min minimum (rounded up) + ~8s startup | Combine short steps into fewer jobs; split only when parallelism saves wall-clock time |
| **Runner OS** | Linux $0.006/min, Windows $0.010/min, macOS $0.062/min | Run on Linux unless OS-specific testing is required; gate macOS/Windows behind Linux passing |
| **Artifact retention** | $0.008/GB/day; default 90 days | Set `retention-days: 1` for inter-job artifacts, `retention-days: 7` for debugging artifacts |
| **Cache storage** | 10 GB free/repo, $0.07/GiB/month overage | Use `Swatinem/rust-cache` for Rust; set `save-if: github.ref == 'refs/heads/main'` to limit cache writes |
| **Duplicate builds** | A full Rust release build costs 5-15 minutes | Share binaries via `upload-artifact`/`download-artifact` between jobs; never rebuild what another job already built |
| **Stale runs** | PR pushes queue duplicate runs | Always set `concurrency` with `cancel-in-progress: true` on non-main branches |
| **Matrix breadth** | N matrix entries = N x job cost | Use `fail-fast: false` only when you need all results; default `fail-fast: true` saves minutes |

**Cost estimation template** (per workflow run, Linux 2-core):

```
Lint steps:     ~1 min  = $0.006
Build (release): ~5 min = $0.030
Unit tests:     ~3 min  = $0.018
Acceptance:     ~5 min  = $0.030
                --------
Total:          ~14 min = $0.084/run
```

For Rust projects, set these environment variables to reduce build time:

```yaml
env:
  CARGO_INCREMENTAL: "0"           # No benefit in CI, adds overhead
  CARGO_PROFILE_TEST_DEBUG: "0"    # Smaller target dir, faster cache
```

### Phase 3: Design

Apply these principles in order of priority:

#### 1. Single Source of Truth — Taskfile Wrapping

**Every check that a developer might run locally MUST be defined as a Taskfile task.** The CI workflow calls the task, not the raw command. This eliminates drift between local and CI environments.

```yaml
# CORRECT — CI calls the task
- name: Clippy
  run: task clippy:check

# WRONG — duplicates the command, will drift from taskfile
- name: Clippy
  run: cargo clippy --workspace --locked -- -D warnings
```

**When to wrap in a task:**
- Lint checks (fmt, clippy, eslint, ruff)
- Build commands
- Test execution (unit, integration, acceptance)
- Any multi-step operation (build + copy + test)

**When raw commands are acceptable in CI:**
- CI-only infrastructure (`chmod +x`, artifact paths)
- GitHub-specific annotations and outputs (`echo "::error::"`)
- Steps that use CI-only variables (`${{ steps.*.outcome }}`)

**Merge rule:** When CI and Taskfile define the same check with different flags, always adopt the stricter flags in the Taskfile and have CI call the task. Example: if CI uses `--locked -- -D warnings` but the task uses just `--workspace`, update the task to include all flags.

#### 2. Minimize Code Duplication with Reusable Actions

**Repeated step sequences** across jobs or workflows MUST be extracted:

| Scope | Mechanism | When |
|-------|-----------|------|
| Steps within a job | Taskfile task | Same steps used locally and in CI |
| Steps across jobs in one workflow | Composite action (`.github/actions/*/action.yml`) | Setup sequences (checkout + toolchain + cache) |
| Jobs across workflows | Reusable workflow (`workflow_call`) | Shared CI pipeline patterns across repos |

Example composite action for repeated Rust setup:

```yaml
# .github/actions/setup-rust/action.yml
name: Setup Rust Toolchain
description: Checkout, install Rust, setup Task, cache
inputs:
  components:
    description: Rust components to install
    default: ''
runs:
  using: composite
  steps:
    - uses: actions/checkout@SHA  # v4
      with:
        submodules: true
    - uses: actions-rust-lang/setup-rust-toolchain@SHA  # v1
      with:
        toolchain: stable
        components: ${{ inputs.components }}
        matcher: true
    - uses: go-task/setup-task@SHA  # v1
```

Then in workflows:

```yaml
steps:
  - uses: ./.github/actions/setup-rust
    with:
      components: clippy,rustfmt
```

#### 3. Quality Gates — Never Block Test Results

Lint failures MUST NOT prevent tests from running. Test results are always more valuable than lint results because they reveal correctness issues.

**Pattern: continue-on-error + final gate**

```yaml
- name: Check formatting
  id: fmt
  run: task format:check
  continue-on-error: true

- name: Clippy
  id: clippy
  run: task clippy:check
  continue-on-error: true

- name: Build
  run: task build

- name: Test
  run: task test:unit -- --profile ci

# Always publish results, even on failure
- name: Publish Test Results
  if: ${{ !cancelled() }}
  uses: EnricoMi/publish-unit-test-result-action@SHA
  with:
    files: target/nextest/ci/junit.xml

# Fail the job at the end if linters failed
- name: Check lint results
  if: ${{ !cancelled() }}
  run: |
    if [[ "${{ steps.fmt.outcome }}" == "failure" || "${{ steps.clippy.outcome }}" == "failure" ]]; then
      echo "::error::Lint checks failed (fmt=${{ steps.fmt.outcome }}, clippy=${{ steps.clippy.outcome }})"
      exit 1
    fi
```

**Key rules:**
- Linters get `continue-on-error: true` + an `id`
- Build and test steps run normally (failure here should stop the job)
- Test result publishing uses `if: ${{ !cancelled() }}`
- A final gate step checks all `continue-on-error` outcomes and fails the job if any linter failed

#### 4. Annotations and Reporting

Use GitHub's native annotation mechanisms to surface results inline on PRs:

| Mechanism | Source | Setup |
|-----------|--------|-------|
| **Problem matchers** | `actions-rust-lang/setup-rust-toolchain` with `matcher: true` | Automatic for cargo, clippy, rustfmt output |
| **JUnit test results** | `EnricoMi/publish-unit-test-result-action` | Requires `checks: write` + `pull-requests: write` permissions |
| **SARIF** (optional) | `clippy-sarif` → `github/codeql-action/upload-sarif` | For GitHub Security tab integration |

**Test report requirements:**
- Unit tests: use nextest `--profile ci` (configured in `.config/nextest.toml` with JUnit output)
- Acceptance tests: pass `--junit-xml=results/junit.xml` to pytest
- Always upload reports with `if: ${{ !cancelled() }}` so they're available even when tests fail

#### 5. Job Structure — Share Artifacts, Don't Rebuild

```
┌─────────────────────────────────────────┐
│ smoke job                               │
│  fmt ──► clippy ──► build ──► test      │
│                       │                 │
│                  upload-artifact         │
│                       │                 │
│            publish test results          │
│            check lint results            │
└─────────────┬───────────────────────────┘
              │ needs: smoke
┌─────────────▼───────────────────────────┐
│ acceptance-tests job                    │
│  download-artifact ──► task test        │
│                                         │
│            publish test results          │
└─────────────────────────────────────────┘
```

- Build the binary **once** in the smoke job, upload with `compression-level: 0` and `retention-days: 1`
- Downstream jobs download the artifact instead of rebuilding
- Use the `CI` environment variable or Taskfile's `SKIP_BUILD` to skip compilation in downstream jobs

#### 6. Security

All workflows MUST follow these rules:

- **SHA-pin every action** — `uses: owner/action@<full-sha>  # vX.Y.Z` with human-readable version in comment
- **Minimal permissions** — declare `permissions` at workflow level with minimum scope; elevate per-job only where needed
- **No secrets in run steps** — use `env:` intermediary to prevent script injection from GitHub context values
- **OIDC for cloud auth** — replace static credentials with `id-token: write` + cloud provider OIDC
- **No self-hosted runners for public repos** — fork PRs can execute arbitrary code

Configure Dependabot to auto-update pinned action SHAs:

```yaml
# .github/dependabot.yml
version: 2
updates:
  - package-ecosystem: github-actions
    directory: "/"
    schedule:
      interval: weekly
    groups:
      github-actions:
        patterns: ["*"]
```

#### 7. Concurrency Control

Every workflow MUST include concurrency settings:

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}
```

This cancels stale PR runs (saving minutes) but never cancels main branch runs.

### Phase 4: Review Checklist

Before finalizing any CI workflow change, verify:

- [ ] **Taskfile wrapping** — every check callable via `task <name>` locally
- [ ] **No command duplication** — CI calls tasks, not raw commands (except CI-only glue)
- [ ] **Composite actions** — repeated setup sequences extracted to `.github/actions/`
- [ ] **SHA pinning** — every `uses:` has a full commit SHA with version comment
- [ ] **Permissions** — explicit, minimal `permissions:` block at workflow and/or job level
- [ ] **Concurrency** — `concurrency:` group with `cancel-in-progress` on non-main branches
- [ ] **Cost check** — estimated per-run cost documented; no unnecessary `--release` builds; artifacts have short retention
- [ ] **Annotations** — problem matchers enabled; test results published with `if: !cancelled()`
- [ ] **Lint doesn't block tests** — `continue-on-error: true` on linters; final gate step checks outcomes
- [ ] **Artifact sharing** — binary built once and shared; downstream jobs skip rebuild
- [ ] **Caching** — `Swatinem/rust-cache` or equivalent; `CARGO_INCREMENTAL=0` set
- [ ] **No deprecated actions** — no v3 artifact/cache actions; no `actions-rs/*`

## Recommended Actions Reference

### Core

| Action | Current | Purpose |
|--------|---------|---------|
| `actions/checkout` | v4 | Repository checkout |
| `actions/upload-artifact` | v4 | Share files between jobs |
| `actions/download-artifact` | v4 | Receive shared files |
| `actions/cache` | v4 | Dependency caching |

### Rust

| Action | Current | Purpose |
|--------|---------|---------|
| `actions-rust-lang/setup-rust-toolchain` | v1 | Toolchain + cache + matchers |
| `Swatinem/rust-cache` | v2 | Cargo/target caching (standalone) |
| `taiki-e/install-action` | v2 | Install cargo tools (nextest, llvm-cov) |

### Quality

| Action | Current | Purpose |
|--------|---------|---------|
| `EnricoMi/publish-unit-test-result-action` | v2 | JUnit → PR annotations + summary |
| `github/codeql-action/upload-sarif` | v3 | SARIF → Security tab |
| `codecov/codecov-action` | v5 | Coverage reporting |

### Infrastructure

| Action | Current | Purpose |
|--------|---------|---------|
| `go-task/setup-task` | v1 | Install Taskfile runner |
| `astral-sh/setup-uv` | v6 | Install uv (Python) |
| `softprops/action-gh-release` | v2 | Create GitHub Releases |
| `docker/build-push-action` | v6 | Build + push container images |

### Security

| Action | Current | Purpose |
|--------|---------|---------|
| `aquasecurity/trivy-action` | v0.33 | Vulnerability scanning |
| `actions/attest-build-provenance` | v1 | SLSA Build Level 2 attestation |

## Caching Strategy (Rust)

### Option A: Integrated (recommended for most projects)

`actions-rust-lang/setup-rust-toolchain` includes `Swatinem/rust-cache` by default. No extra step needed.

### Option B: Standalone (when you need fine control)

```yaml
- uses: Swatinem/rust-cache@SHA  # v2
  with:
    cache-targets: true
    cache-on-failure: false
    save-if: ${{ github.ref == 'refs/heads/main' }}
```

### Option C: sccache (large workspaces, 10GB+ target dirs)

```yaml
env:
  RUSTC_WRAPPER: sccache
  SCCACHE_GHA_ENABLED: "true"
steps:
  - uses: mozilla-actions/sccache-action@SHA
```

## Anti-Patterns

| Anti-Pattern | Fix |
|-------------|-----|
| Raw `cargo` commands in CI that duplicate Taskfile tasks | Call `task <name>` instead |
| Repeated setup steps across jobs without extraction | Create composite action in `.github/actions/` |
| `@v4` tag references without SHA | Pin by full commit SHA, add version comment |
| Lint failure blocks test execution | `continue-on-error: true` + final gate |
| Binary rebuilt in every job | `upload-artifact` from build job, `download-artifact` in consumers |
| Default 90-day artifact retention | `retention-days: 1` for inter-job, `7` for debugging |
| No concurrency control | Add `concurrency:` with `cancel-in-progress` |
| `fetch-depth: 0` when full history not needed | Default `fetch-depth: 1` |
| `pull_request_target` + checkout of PR head | Use `pull_request` trigger for untrusted code |
| `cargo test` instead of `cargo nextest run` | nextest is ~40% faster in CI |
| `--release` builds for unit tests | Use debug builds; reserve `--release` for published binaries |
| Saving cache on failed builds | `cache-on-failure: false` |
