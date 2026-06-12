---
paths:
  - .github/workflows/**
  - .github/actions/**
  - renovate.json
---

# CI Subsystem

GitHub Actions workflows for OCX build, test, lint, license, release.

## Design Principles

### 1. Taskfile is the single source of truth

Every check dev run locally MUST be Taskfile task. CI call task, not raw command. Kill drift between local + CI.

```yaml
# CORRECT
- name: Clippy
  run: task rust:clippy:check

# WRONG — will drift from taskfile
- name: Clippy
  run: cargo clippy --workspace --locked -- -D warnings
```

Raw commands in CI OK only for CI-only glue (artifact paths, GitHub annotations, `${{ steps.*.outcome }}`).

**Merge rule**: CI + Taskfile diverge → adopt stricter flags in Taskfile, CI call task.

### 2. Minimize duplication via composite actions

| Scope | Mechanism | When |
|-------|-----------|------|
| Steps within a job | Taskfile task | Same steps used locally and in CI |
| Steps across jobs in one workflow | Composite action (`.github/actions/*/action.yml`) | Setup sequences (checkout + toolchain + cache) |
| Jobs across workflows | Reusable workflow (`workflow_call`) | Shared CI pipeline patterns |

### 3. Never let lint block test results

Test results > lint results. Pattern: `continue-on-error: true` on linters + final gate step.

```yaml
- name: Check formatting
  id: fmt
  run: task rust:format:check
  continue-on-error: true

- name: Clippy
  id: clippy
  run: task rust:clippy:check
  continue-on-error: true

- name: Build
  run: task rust:build

- name: Test
  run: task rust:test:unit -- --profile ci

- name: Publish Test Results
  if: ${{ !cancelled() }}
  uses: EnricoMi/publish-unit-test-result-action@SHA
  with:
    files: target/nextest/ci/junit.xml

- name: Check lint results
  if: ${{ !cancelled() }}
  run: |
    if [[ "${{ steps.fmt.outcome }}" == "failure" || "${{ steps.clippy.outcome }}" == "failure" ]]; then
      echo "::error::Lint checks failed"
      exit 1
    fi
```

### 4. Share artifacts, don't rebuild

Build binary **once** in smoke job, upload with `compression-level: 0` and `retention-days: 1`. Downstream jobs `download-artifact` not rebuild.

### 5. Security

- **SHA-pin every action** — `uses: owner/action@<full-sha>  # vX.Y.Z`. Includes first-party actions (`ocx-sh/setup-ocx`): no floating-major carve-out. Exception: `release.yml` is cargo-dist-generated and floats by design (ignored by the bumper).
- **Keep pins fresh automatically** — dependency updates run via **Renovate** (`renovate.json`, replaces Dependabot). (The ocx-mirror pipeline templates and their customManager moved to the ocx-sh/ocx-mirror repo.)
- **Minimal permissions** — declare at workflow level, elevate per-job
- **No secrets in `run:` steps** — use `env:` intermediary to block script injection
- **OIDC for cloud auth** — not static credentials
- **No self-hosted runners for public repos**

### 6. Concurrency

Every workflow:

```yaml
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}
```

## Cost Factors

| Factor | Impact | Guidance |
|--------|--------|----------|
| **Job count** | 1 min minimum per job + ~8s startup | Combine short steps; split only when parallelism saves wall-clock |
| **Runner OS** | Linux $0.006, Windows $0.010, macOS $0.062 per min | Linux default; gate macOS/Windows behind Linux passing |
| **Artifact retention** | $0.008/GB/day; default 90 days | `retention-days: 1` inter-job, `7` debugging |
| **Cache** | 10 GB free, $0.07/GiB/month overage | `Swatinem/rust-cache` + `save-if: github.ref == 'refs/heads/main'` |
| **Duplicate builds** | A full Rust release build = 5-15 min | Share binaries via upload/download-artifact |
| **Matrix breadth** | N entries = N x job cost | `fail-fast: true` default |

Rust env to cut build time:

```yaml
env:
  CARGO_INCREMENTAL: "0"
  CARGO_PROFILE_TEST_DEBUG: "0"
```

## Recommended Actions (2026)

### Core

| Action | Version | Purpose |
|--------|---------|---------|
| `actions/checkout` | v4 | Repository checkout |
| `actions/upload-artifact` | v4 | Share files between jobs |
| `actions/download-artifact` | v4 | Receive shared files |
| `actions/cache` | v4 | Dependency caching |

### Rust

| Action | Version | Purpose |
|--------|---------|---------|
| `actions-rust-lang/setup-rust-toolchain` | v1 | Toolchain + cache + matchers |
| `Swatinem/rust-cache` | v2 | Cargo/target caching (standalone) |
| `taiki-e/install-action` | v2 | Install cargo tools (nextest, llvm-cov) |

### Quality + Infra + Security

| Action | Purpose |
|--------|---------|
| `EnricoMi/publish-unit-test-result-action@v2` | JUnit → PR annotations |
| `github/codeql-action/upload-sarif@v3` | SARIF → Security tab |
| `codecov/codecov-action@v5` | Coverage reporting |
| `go-task/setup-task@v1` | Install Taskfile runner |
| `astral-sh/setup-uv@v6` | Install uv (Python) |
| `softprops/action-gh-release@v2` | Create GitHub Releases |
| `aquasecurity/trivy-action@v0.33` | Vulnerability scanning |
| `actions/attest-build-provenance@v1` | SLSA Build Level 2 attestation |

No deprecated v3 artifact/cache actions. No `actions-rs/*`. Require Node 24+ runtime.

## Review Checklist

- [ ] Taskfile wrapping — every check callable via `task <name>`
- [ ] No command duplication — CI calls tasks, not raw commands
- [ ] Composite actions — repeated setup sequences extracted
- [ ] SHA pinning — every `uses:` has full commit SHA + version comment
- [ ] Permissions — explicit, minimal, at workflow and/or job level
- [ ] Concurrency — group + `cancel-in-progress` on non-main
- [ ] Cost check — no unnecessary `--release`; short artifact retention
- [ ] Annotations — problem matchers on; test results with `if: !cancelled()`
- [ ] Lint doesn't block tests — `continue-on-error` + final gate
- [ ] Artifact sharing — binary built once; downstream jobs skip rebuild
- [ ] Caching — `Swatinem/rust-cache` or equivalent; `CARGO_INCREMENTAL=0`
- [ ] No deprecated actions

## Authoring Workflow

Creating, modifying, auditing GitHub Actions workflow → follow sequence:

1. **Research** — Glob `.github/workflows/*.yml`, read existing structure. Read Taskfile to find which checks already have task definitions. Check action versions for deprecation; verify Node 24+ runtime.
2. **Cost analysis** — Estimate per-run cost via Cost Factors table above. Document estimate in PR description.
3. **Design** — Apply Design Principles (Taskfile wrapping, composite actions, lint doesn't block tests, artifact sharing, SHA pinning, minimal permissions, concurrency).
4. **Review** — Walk Review Checklist before merge.

### Tool Preferences

- **GitHub MCP** — `mcp__github__list_workflow_runs`, `get_workflow_run`, `get_workflow_run_logs` for structured run inspection. Fallback: `gh run list`, `gh run view`, `gh run view --log`.
- **`gh workflow run <name>`** — manual dispatch (no MCP parity yet).
- **WebFetch** — GitHub Actions changelog and deprecation notices.

### Handoffs

- To Builder — Taskfile changes needed by CI (Taskfile = source of truth)
- To Security Auditor — supply chain or permission review on new workflows

## Windows Shim Build Workflow (`build-windows-shims.yml`)

The `build-windows-shims.yml` workflow cross-compiles the `ocx_shim` binary for `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` on Linux runners using `cargo-xwin`. No Windows runner is needed for the build.

### Structure

| Job | Runner | Purpose |
|-----|--------|---------|
| `gate` | `ubuntu-latest` | Runs host-runnable shim + launcher spec tests (`cargo nextest run -p ocx_shim -p ocx_lib --locked`). Cheap Linux gate; matrix legs only proceed if this passes. |
| `build` (matrix) | `ubuntu-latest` | Cross-compiles both arches via `task rust:shim:build TARGET=<arch>-pc-windows-msvc`. Byte-equality check + SLSA attestation per arch. |

### Key design points

- **Byte-equality = real drift control.** The "Verify committed blob is byte-identical to a fresh rebuild" step is the authoritative guard against `crates/ocx_shim` source drifting from the committed blob in `crates/ocx_lib/src/shims/`. The in-tree `SHIM_SHA256` constant is only a corruption canary (self-consistent include_bytes! check), not a provenance control.
- **SLSA attestation** via `actions/attest-build-provenance` provides build provenance on the committed blob (Sigstore/Rekor). Refresh-PR flow: `gh attestation verify` is part of the contributor checklist when re-cutting the blob.
- **Reproducibility flags.** The `shim` Cargo profile uses `opt-level="z"`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`. The `rust-toolchain.toml` pins the exact toolchain version for the build; both are required for byte-identical output across machines.
- **Gate-before-matrix pattern** matches `subsystem-ci.md` §"Never let lint block test results" — the spec-test gate is cheap Linux, the expensive cross-build jobs are gated behind it.
- **No Windows runner.** The cross-compile is hermetic (`cargo-xwin` + MSVC CRT headers); a Windows runner is required only for acceptance testing (`test_windows_shim.py`), which is a separate CI concern.
- **Signing is Phase 2.** The workflow explicitly does NOT sign. Authenticode signing via SignPath Foundation is a documented follow-on (see ADR §Out of Implementation Scope and the inline workflow comment). Do not add a signing step without the Phase-2 prerequisites.

### Refresh flow (updating the committed blob)

When `crates/ocx_shim` source changes:
1. Run `task rust:shim:build TARGET=x86_64-pc-windows-msvc` (and `aarch64-pc-windows-msvc`).
2. Copy the stripped output to `crates/ocx_lib/src/shims/ocx-shim-{x86_64,aarch64}.exe`.
3. Record the new SHA-256 in `crates/ocx_lib/src/shim.rs` `SHIM_SHA256` constant.
4. Open a dedicated refresh PR so the byte-equality CI gate passes.

## Anti-Patterns

| Anti-Pattern | Fix |
|--------------|-----|
| Raw `cargo` in CI duplicating Taskfile | Call `task <name>` |
| Repeated setup across jobs | Composite action in `.github/actions/` |
| `@v4` tag without SHA | Pin by full commit SHA |
| Lint failure blocks test execution | `continue-on-error` + final gate |
| Binary rebuilt in every job | `upload-artifact` + `download-artifact` |
| Default 90-day artifact retention | `retention-days: 1` inter-job |
| No concurrency control | Add `concurrency:` block |
| `fetch-depth: 0` when not needed | Default `fetch-depth: 1` |
| `pull_request_target` + checkout of PR head | Use `pull_request` trigger for untrusted code |
| `cargo test` instead of `cargo nextest` | nextest is ~40% faster in CI |
| `--release` for unit tests | Debug; reserve release for published binaries |
| Saving cache on failed builds | `cache-on-failure: false` |
| Editing generated workflows directly | Edit the config (`dist-workspace.toml`) and re-run `dist generate-ci`. `release.yml` is generated by cargo-dist — manual edits get overwritten. Hand-written workflows (`verify-version.yml`) can be edited directly. |