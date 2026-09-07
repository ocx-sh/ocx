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

### 7. Lint workflows with actionlint

Workflow YAML is linted by [actionlint](https://github.com/rhysd/actionlint) (project toolchain via `ocx.toml`). Local + CI gate = `task ci:actionlint`; the `workflow-lint` job in `verify-basic.yml` runs it on push + PR via `ocx exec -- task ci:actionlint` (dogfoods `setup-ocx` + the composed toolchain). actionlint catches invalid contexts (e.g. `matrix` on a step's `shell:` key), unpinned/typo'd expressions, and runs shellcheck over `run:` scripts. Conventions:

- Embedded shellcheck severity floor = `--severity=warning` (`SHELLCHECK_OPTS` in the task), matching `shell:shellcheck`. Info/style findings are not gated.
- The cargo-dist-generated `release.yml` is excluded via `.github/actionlint.yaml` (`paths:` ignore-all) — never hand-edited, drift policed by `verify-release-ci.yml`.

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
| `actions/checkout` | v6 | Repository checkout |
| `actions/upload-artifact` | v7 | Share files between jobs |
| `actions/download-artifact` | v8 | Receive shared files |
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
| `go-task/setup-task@v2` | Install Taskfile runner |
| `astral-sh/setup-uv@v8` | Install uv (Python) |
| `softprops/action-gh-release@v2` | Create GitHub Releases |
| `aquasecurity/trivy-action@v0.33` | Vulnerability scanning |
| `actions/attest-build-provenance@v2` | SLSA Build Level 2 attestation |

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

Cross-builds the `ocx_shim` binary for `x86_64-pc-windows-gnullvm` and `aarch64-pc-windows-gnullvm` on Linux runners via `cargo-zigbuild` — Zig bundles its own clang/lld/libc, so no Microsoft SDK manifest is fetched. Design authority: `.claude/artifacts/adr_shim_hermetic_zigbuild.md`, addendum 2.

### Structure

| Job | Runner | Purpose |
|-----|--------|---------|
| `gate` | `ubuntu-latest` | Host-runnable shim + launcher spec tests (`cargo nextest run -p ocx_shim -p ocx_lib --locked`; nextest comes from `taiki-e/install-action`, the toolchain action does not ship it). Both other jobs carry `needs: gate`. |
| `build` (matrix) | `ubuntu-latest` | Per-arch `task rust:shim:build TARGET=${{ matrix.target }}`, then blob validation, SLSA attestation, artifact upload. `fail-fast: false` so one arch's status never masks the other's. |
| `acceptance-windows` | `windows-latest` | Native `cargo build -p ocx_shim --locked`, then `uv run pytest tests/test_windows_shim.py` with `OCX_SHIM_BINARY` set. Registry-independent (`OCX_TESTS_NO_REGISTRY=1`) — the `registry:2` fixture is Linux-only. Without this job the module is green-by-skip. |

### Key design points

- **The gate is provenance, not byte-equality.** The "Validate committed shim blob (provenance model)" step asserts three things about the committed blob under `crates/ocx_lib/src/shims/`: MZ magic (valid PE), size within the `SHIM_SIZE_BUDGET` it greps out of `crates/ocx_lib/src/shim.rs`, and `sha256` equal to that file's per-arch `SHIM_SHA256`. Nothing compares bytes against the fresh build — the gnullvm PE link embeds a per-link build id, so two runs of one pinned toolchain differ (ADR addendum 2). Drift control comes from pairing the SHA canary with the job's `paths:` filter (`crates/ocx_shim/**`, `Cargo.lock`, `rust-toolchain.toml`, `taskfiles/rust.taskfile.yml`, this workflow): a source change without a blob refresh reds the canary.
- **SLSA attestation** via `actions/attest-build-provenance` signs the *committed* blob (Sigstore/Rekor). The freshly built binary uploads as `ocx-shim-fresh-<target>` **before** the validation step and `if: !cancelled()`, because that artifact is the refresh source.
- **The whole toolchain is pinned.** `rust-toolchain.toml` pins rustc 1.95.0 and both gnullvm targets; `install:cargo-zigbuild` pins cargo-zigbuild 0.22.3; the workflow pins `ZIG_VERSION: 0.16.0` with a `ZIG_SHA256` the download is checked against. Zig's version changes the emitted bytes, so bump it deliberately in a blob-refresh PR, never incidentally. The `shim` profile (`opt-level="z"`, `lto`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`) is a size control, not a reproducibility one.
- **`RUSTFLAGS` goes on the command line, never in task `env:`.** go-task's task-level `env:` loses to an inherited variable and `setup-rust-toolchain` exports `RUSTFLAGS`, which left the former `-Clink-arg=/Brepro` inert in CI for every blob ever shipped while breaking clean dev shells (`zig cc` rejects the MSVC flag). `shim:build` prefixes `RUSTFLAGS=` on the `cargo zigbuild` line, carrying the `--remap-path-prefix` that keeps a dev box's rust-src `$HOME` paths out of the blob.
- **Gate-before-matrix** is the Cost Factors rule above ("gate macOS/Windows behind Linux passing"), not §"Never let lint block test results".
- **Signing is Phase 2.** The workflow explicitly does NOT sign. Authenticode signing via SignPath Foundation is a documented follow-on (ADR §Out of Implementation Scope plus the workflow's header and trailing comments). Do not add a signing step without the Phase-2 prerequisites.

### Refresh flow (updating the committed blob)

Canonical procedure: the `crates/ocx_lib/src/shim.rs` module docs. In a dedicated PR:

1. `task rust:shim:build TARGET=x86_64-pc-windows-gnullvm` (and `aarch64-pc-windows-gnullvm`) — or download the `ocx-shim-fresh-<target>` artifact from a CI run, which exists because a dev box need not have a usable Zig.
2. Copy each `target/<triple>/shim/ocx-shim.exe` to `crates/ocx_lib/src/shims/ocx-shim-{x86_64,aarch64}.exe`.
3. Record each blob's `sha256sum` in the matching per-arch `SHIM_SHA256` in `crates/ocx_lib/src/shim.rs` — blob and constant must move together, or the canary reds.

## Windows & macOS Coverage — what each gate compiles

`cfg(windows)` and `cfg(target_os = "macos")` code is invisible to a host (Linux) build, so the coverage boundary is easy to overstate. What holds today:

| Gate | Platform scope |
|---|---|
| `task verify` (root) | **No Windows or macOS leg.** `.verify:lint` runs `rust:format:check` + `rust:clippy:check` directly, not `rust:verify`. |
| `task rust:verify` → `check:windows-cfg` | `cargo check -p ocx_shim --locked --all-targets --target {x86_64,aarch64}-pc-windows-msvc` — **`ocx_shim` only**, deliberately scoped in `taskfiles/rust.taskfile.yml`. It type-checks none of `ocx_lib`'s `cfg(windows)` arms (`env.rs::pathext`, `setup/session_path/windows.rs`, `package_manager/launcher/generate.rs`, `package_manager/tasks/render_toolchain.rs`). No equivalent check exists for `cfg(target_os = "macos")`. |
| `task rust:check:windows` | Whole-workspace `cargo xwin check` for both MSVC targets in Docker — covers those Windows arms, but is opt-in: no task chain and no workflow invokes it. No macOS counterpart. |
| `verify-basic.yml` → `smoke-windows` (`windows-latest`, `needs: smoke`, push + PR) | `cargo nextest run --workspace --locked --profile ci`. The only gate on every push and PR that compiles *and runs* `ocx_lib`'s `cfg(windows)` code, `#[cfg(windows)]` unit tests included. |
| `verify-basic.yml` → `smoke-macos` (`macos-latest`, `needs: smoke`, push + PR) | Byte-identical `cargo nextest run --workspace --locked --profile ci`. The macOS twin of `smoke-windows`, added so the LaunchAgent session-PATH writer (`setup/session_path/macos.rs`) is compiled and executed on every push and PR instead of only in `verify-deep.yml`'s weekly/dispatch leg. |
| `verify-deep.yml` → `build` Windows leg / `cross-compile` | Native `cargo nextest run --workspace --target=x86_64-pc-windows-msvc --profile ci --locked`, and `cargo xwin build --release --target=aarch64-pc-windows-msvc` for arm64 (the only Windows-arm64 workspace build in CI). Both are `workflow_dispatch` / `workflow_call` only — never on push or PR. |
| `verify-deep.yml` → `build` macOS leg (matrix) | Native `cargo nextest run --workspace --target=aarch64-apple-darwin --profile ci --locked` on `macos-latest`. `workflow_dispatch` / `workflow_call` only (the weekly `schedule` trigger skips this matrix unless `inputs.full`) — never on push or PR, which is why `smoke-macos` exists as the per-PR gate. |

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
| Invalid context in a workflow (e.g. `matrix` on a step's `shell:` key) | Run `task ci:actionlint`; for matrixed shells use job-level `defaults.run.shell` |
| `fetch-depth: 0` when not needed | Default `fetch-depth: 1` |
| `pull_request_target` + checkout of PR head | Use `pull_request` trigger for untrusted code |
| `cargo test` instead of `cargo nextest` | nextest is ~40% faster in CI |
| `--release` for unit tests | Debug; reserve release for published binaries |
| Saving cache on failed builds | `cache-on-failure: false` |
| Editing generated workflows directly | Edit the config (`dist-workspace.toml`) and re-run `dist generate-ci`. `release.yml` is generated by cargo-dist — manual edits get overwritten. Hand-written workflows (`verify-version.yml`) can be edited directly. |