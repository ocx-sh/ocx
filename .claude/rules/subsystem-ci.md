---
paths:
  - .github/workflows/**
  - .github/actions/**
  - renovate.json
---

# CI Subsystem

GitHub Actions workflows for OCX build, test, lint, license, release.

## Verification tiers

Two workflows, one gate each (plan_crate_split_workspace.md C-015 – C-017, C-023):

| Tier | Workflow | Fires on | Runs |
|---|---|---|---|
| **basic** | `verify-basic.yml` | every push to `main`, every PR (drafts included) and the merge queue (`merge_group`, listed so a basic check later marked required cannot stall a queue) | **Linux only** (D5): `workflow-lint` (`ci:actionlint` plus `claude:tests`, the structural tests of `.claude/` — the only job that runs them), `conventional-commits` (PRs), `smoke` (fmt, clippy, `rust:deps:inventory`, `rust:lint:ratchet`, build, `rust:test:floor` → `rust:test:unit -- --profile ci` → `rust:test:ceiling` → `rust:test:ceiling:self-test`), `smoke-acceptance` (`task test:smoke`, registry only). **No Windows or macOS runner, no full acceptance job, no Sigstore stack.** |
| **deep** | `verify-deep.yml` | every non-draft PR (`pull_request` with `ready_for_review` in `types`), the merge queue (`merge_group`), every push to `main` (merges are rebase-only and there is no queue, so this is the one full run over the tree that actually landed — DX-16), `workflow_dispatch` / `workflow_call` (release readiness), weekly cron (drift check only) | 3-OS build + unit matrix, cross-compile, the full acceptance suite with the Sigstore stack, `satellite-verify` |

- **Draft guard**: every deep job carries `if: github.event_name != 'pull_request' || github.event.pull_request.draft == false` (composed as `(existing) && (guard)`); the event-name half keeps `merge_group` and `push` runs alive, which have no `pull_request` object. Both tiers carry `merge_group`, and neither cancels a `gh-readonly-queue/` ref — a cancelled queue run is a failed check and drops the entry. `.claude/tests/test_workflows.py` asserts all of this — the merge-queue pair over both workflows — plus deep's `pull_request` branch filter and glob liveness (every `paths:` / `paths-ignore:` entry matches a tracked file); `test/tests/test_smoke_coverage.py` (c) asserts the triggers.
- **Compile cache (2026-09-20)**: every Rust job runs `sccache` in front of the shared Garage S3 store at `sccache.ocx.sh` (server side: [herwig-systems/server-hetzner1#2](https://github.com/herwig-systems/server-hetzner1/issues/2)), beside `rust-cache`, which keeps `target/` for the link outputs sccache never caches. Wiring is job-level `env:` (`SCCACHE_ENABLED`, `RUSTC_WRAPPER`, `SCCACHE_*`, `AWS_*` from the org secrets `SCCACHE_AWS_ACCESS_KEY_ID` / `SCCACHE_AWS_SECRET_ACCESS_KEY`) plus a `mozilla-actions/sccache-action` step gated on `env.SCCACHE_ENABLED` — a fork PR has no secrets, gets an empty `RUSTC_WRAPPER` and a skipped step, never a build that fails on a missing cache. Deep's `build` matrix also sets `CARGO_BUILD_TARGET` so `task schema:generate`'s bare `cargo run` lands in the same target dir as the `--target=` build instead of paying a second full compile, and its acceptance job runs `task test:parallel` (the serial `task test` took 21:41 for 3819 tests at a 0.1 s median). The bucket is `cache`, not `sccache`: a bucket named like the endpoint's first host label is parsed as virtual-host style by the S3 client and the endpoint is rewritten. Per-run hit/miss stats land in the job summary from the action's post step.
- **Tier partition and cost (D5)**: basic is what a draft pays for, so it runs on Linux only; the Windows and macOS unit legs are deep's `build` matrix (the same `cargo nextest run --workspace --profile ci`), which every non-draft PR gets anyway — a basic leg on either OS would run that suite twice per push. `test_workflows.py` pins both halves (`test_basic_runs_on_linux_only`, `test_deep_build_matrix_covers_the_three_oses`). Per push, in runner-minutes from runs [35032266887](https://github.com/ocx-sh/ocx/actions/runs/35032266887) / [35003182482](https://github.com/ocx-sh/ocx/actions/runs/35003182482): basic ≈ 18 (`Smoke (Linux)` 14, `Workflow Lint` 1, `Smoke Acceptance` ≈ 3 — estimated from its 90 s budget plus bring-up; the reference run's 7-minute acceptance job was the full suite WP-05 replaced); deep ≈ 113 (`Build & Unit Test` Linux 23 / macOS 23 / Windows 32, `Cross-compile` 12, `Acceptance` 23) plus `satellite-verify` (unmeasured until it first runs). A draft push pays ≈ 18, a non-draft push or a push to `main` ≈ 130. The deleted legs were 8 Windows + 10 macOS minutes per push, ≈ $0.70 at the Cost Factors rates.
- **`announce/**` branches (D10)**: basic listens on PRs into `announce/**`, deep on `main` only — a PR into an `announce/**` branch gets the smoke tier and nothing more, by design; deep covers that work when the `announce/**` branch itself PRs into `main`.
- **`satellite-verify`** (`task satellite:verify --force`, ocx-mirror built against this tree) is **non-blocking**: job-level `continue-on-error: true` until the mirror re-points at the split crates (DEC-4; the flip is deleting that line, WP-39's spec). **Never make it a required check while the line is set** — the job's own check run still concludes `failure` (only the run and `needs.*.result` read `success`), so a required check on it blocks every merge until the mirror re-points; and a gate GitHub read as green would gate nothing. Skipped on the weekly cron like `build`.
- **Observing the draft guard (S-008)**: open a PR as draft — basic runs, every deep job shows *skipped*; mark it ready — a new deep run appears (event `pull_request`, activity `ready_for_review`) with the jobs running. `gh run list --workflow=verify-deep.yml --event=pull_request` lists both.

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

A workflow with a `merge_group` trigger adds `!startsWith(github.ref, 'refs/heads/gh-readonly-queue/') &&` in front (see `verify-deep.yml`): the queue treats a cancelled run as a failed check.

### 7. Lint workflows with actionlint

Workflow YAML is linted by [actionlint](https://github.com/rhysd/actionlint) (project toolchain via `ocx.toml`). Local + CI gate = `task ci:actionlint`; the `workflow-lint` job in `verify-basic.yml` runs it on push + PR via `ocx exec -- task ci:actionlint` (dogfoods `setup-ocx` + the composed toolchain). actionlint catches invalid contexts (e.g. `matrix` on a step's `shell:` key), unpinned/typo'd expressions, and runs shellcheck over `run:` scripts. Conventions:

- Embedded shellcheck severity floor = `--severity=warning` (`SHELLCHECK_OPTS` in the task), matching `shell:shellcheck`. Info/style findings are not gated.
- The cargo-dist-generated `release.yml` is excluded via `.github/actionlint.yaml` (`paths:` ignore-all) — never hand-edited, drift policed by `verify-release-ci.yml`.

## Test telemetry

Every unit and acceptance run — in CI and on a developer machine — pushes its JUnit report to `https://otel.ocx.sh` as OTLP traces, so Grafana can name the tests that accumulate the most wall clock. One converter, `junit2otlp` 0.1.2, on both routes: the composite action `.github/actions/test-telemetry` in CI, the `telemetry:push` task locally.

**What is pushed.** Resource `service.name=ocx-tests`. Span attributes, on every span: `ocx.suite` (`unit` / `acceptance` / `smoke`), `ocx.source` (`ci` / `local`), `ocx.os`, `ocx.git.sha`, `ocx.git.branch`, `ocx.ci.run_id`, `ocx.ci.job` (CI only), `host.name` (the runner name, or the hostname locally). The tool adds `tests.case.duration` and `tests.suite.duration` in **milliseconds**, plus `code.function`, `tests.case.classname` and `tests.case.status`. `OTEL_RESOURCE_ATTRIBUTES` is not read, so none of these can be moved onto the resource without patching the tool.

**Three properties that decide how this is read and wired**, all measured against a local collector rather than assumed:

- **The span's own duration is not the test's.** Each test span is created and ended in the same instant (under a microsecond wide); the real figure is the `tests.case.duration` attribute. A dashboard aggregating span duration reports nothing.
- **The exit status says nothing.** Export errors go through the OTel error handler, so a push that lands nowhere still exits 0 — three `traces export: ... connection error` lines and `$? == 0`. Both routes therefore grep the tool's own output for `traces export:` and warn; neither trusts the status.
- **The endpoint must carry a scheme.** The tool builds `otlptracegrpc` and has no HTTP exporter, and the Go SDK reads the endpoint as a URL: `https://otel.ocx.sh:443` selects TLS, `http://` selects plaintext, and a bare `host:port` fails with `dns resolver: missing address`. It also always builds a metric exporter (`OTEL_METRICS_EXPORTER` is not read); with no metrics receiver that logs one `Unimplemented` per push, which is non-retryable and costs no measurable time.

**Two defaults would each have lost data quietly, so neither route feeds the tool a report as written.** Both are handled in the same few lines of both routes, and both were found by counting spans rather than by reading the tool's output, which reported success in both cases.

- **A 1 MB-per-line stdin scanner, against producers that emit one line.** The acceptance report is a single line of 545,175 bytes; past the ceiling junit2otlp exits 1 with `bufio.Scanner: token too long` and pushes nothing at all. Both routes therefore re-emit the document with `xml.dom.minidom.toprettyxml` first, which drops the longest line under a kilobyte whatever the suite grows to. Whitespace between elements is ignorable in XML, so what gets parsed is the same document: measured, the 3,819-case acceptance report yields exactly 3,821 spans (one run, one suite, one per `<testcase>`) and the 8,215-case unit report exactly 8,250.
- **A 2,048-span export queue, against reports an order of magnitude larger.** junit2otlp builds a `BatchSpanProcessor` and never sets a queue size, so the OTel SDK default applies — and a span enqueued above it is dropped with no log line, no error and exit 0. At the default the unit report landed **2,560 of its 8,250 spans**; with `OTEL_BSP_MAX_QUEUE_SIZE` set it landed all 8,250. Both routes size it from the report they are about to push (`cases + 1024`), so no suite can outgrow it. `--batch-size 512` is set beside it: the default of 10 makes the unit report 825 gRPC round trips instead of 17, and one run at the default lost a contiguous batch of exactly ten spans mid-report, unreproducibly and with nothing logged.

Nextest's report needs neither on its own — it is already 8,286 lines with a 242-byte maximum — but it takes both, because the routes do not branch on which producer wrote the file.

**There is no in-band way to notice the next such loss, and the obvious one does not exist.** `tests.suite.total` reads like the count a query could check a run against; in v0.1.2 it is a metric name passed once to `createIntCounter` and never placed on a span, and the ingested trace confirms it — the only `tests.suite.*` keys on any span are `duration`, `suitename`, `systemerr` and `systemout`. Nothing a span carries declares how many were meant to arrive, so a short run is a smaller believable number and not a gap. Checking a count means holding Tempo's span count against the `<testcase>` count in the report, from outside.

**Auth** is nginx basic auth. The header value (`Basic <base64 of ci:password>`) lives in the org secret `OTEL_OTLP_AUTH` and reaches the tool as `OTEL_EXPORTER_OTLP_HEADERS`. It is lifted into job `env:` rather than read from `secrets` in the step, for the reason the sccache credentials are: `secrets` is not a context a composite action can read, and the guard `env.OTEL_OTLP_AUTH != ''` is what makes a fork PR — which sees no secrets — skip the push instead of failing it. Every push step is also `continue-on-error: true` with `timeout-minutes: 5`: telemetry never decides a job. They run under `if: ${{ !cancelled() && … }}`, so a red suite pushes its timings too.

Wired after the JUnit-producing step of `verify-deep.yml`'s `build` matrix (`target/nextest/ci/junit.xml`, all three OSes) and its `acceptance-tests` job (`test/results/junit.xml`), and of `verify-basic.yml`'s `smoke` and `smoke-acceptance` jobs.

**Enabling it locally** (off by default, and silently — no endpoint and no binary means no output and no failure):

1. Install `junit2otlp` 0.1.2 onto `PATH` from the [release tarball](https://github.com/mdelapenya/junit2otlp/releases/tag/v0.1.2) for your platform.
2. Write `~/.config/ocx-telemetry/env`, **mode 600** — it holds a password — with `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_EXPORTER_OTLP_HEADERS`. `taskfiles/telemetry.taskfile.yml` sources it itself, so it works from a shell that has not loaded it.

`task verify` then pushes: `rust:test:unit` reads `target/nextest/default/junit.xml` (the `[profile.default.junit]` entry in `.config/nextest.toml`), and the pytest legs read `test/results/junit.xml` / `junit-smoke.xml`, which they write on every run rather than only when CI passes the flag — a report produced only under a CI-supplied argument is one no local run has.

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
| `gate` | `ubuntu-latest` | Host-runnable shim + launcher spec tests (`cargo nextest run -p ocx_shim -p ocx_store -p ocx_package_manager --locked` — `ocx_store` owns the committed blob, `ocx_package_manager` owns the launcher spec; nextest comes from `taiki-e/install-action`, the toolchain action does not ship it). Both other jobs carry `needs: gate`. |
| `build` (matrix) | `ubuntu-latest` | Per-arch `task rust:shim:build TARGET=${{ matrix.target }}`, then blob validation, SLSA attestation, artifact upload. `fail-fast: false` so one arch's status never masks the other's. |
| `acceptance-windows` | `windows-latest` | Native `cargo build -p ocx_shim --locked`, then `uv run pytest tests/test_windows_shim.py` with `OCX_SHIM_BINARY` set. Registry-independent (`OCX_TESTS_NO_REGISTRY=1`) — the `registry:2` fixture is Linux-only. Without this job the module is green-by-skip. |

### Key design points

- **The gate is provenance, not byte-equality.** The "Validate committed shim blob (provenance model)" step asserts three things about the committed blob under `crates/ocx_store/src/shims/`: MZ magic (valid PE), size within the `SHIM_SIZE_BUDGET` it greps out of `crates/ocx_store/src/shim.rs`, and `sha256` equal to that file's per-arch `SHIM_SHA256`. Nothing compares bytes against the fresh build — the gnullvm PE link embeds a per-link build id, so two runs of one pinned toolchain differ (ADR addendum 2). Drift control comes from pairing the SHA canary with the job's `paths:` filter (`crates/ocx_shim/**`, `Cargo.lock`, `rust-toolchain.toml`, `taskfiles/rust.taskfile.yml`, this workflow): a source change without a blob refresh reds the canary.
- **SLSA attestation** via `actions/attest-build-provenance` signs the *committed* blob (Sigstore/Rekor). The freshly built binary uploads as `ocx-shim-fresh-<target>` **before** the validation step and `if: !cancelled()`, because that artifact is the refresh source.
- **The whole toolchain is pinned.** `rust-toolchain.toml` pins rustc 1.95.0 and both gnullvm targets; `install:cargo-zigbuild` pins cargo-zigbuild 0.22.3; the workflow pins `ZIG_VERSION: 0.16.0` with a `ZIG_SHA256` the download is checked against. Zig's version changes the emitted bytes, so bump it deliberately in a blob-refresh PR, never incidentally. The `shim` profile (`opt-level="z"`, `lto`, `codegen-units=1`, `panic="abort"`, `strip="symbols"`) is a size control, not a reproducibility one.
- **`RUSTFLAGS` goes on the command line, never in task `env:`.** go-task's task-level `env:` loses to an inherited variable and `setup-rust-toolchain` exports `RUSTFLAGS`, which left the former `-Clink-arg=/Brepro` inert in CI for every blob ever shipped while breaking clean dev shells (`zig cc` rejects the MSVC flag). `shim:build` prefixes `RUSTFLAGS=` on the `cargo zigbuild` line, carrying the `--remap-path-prefix` that keeps a dev box's rust-src `$HOME` paths out of the blob.
- **Gate-before-matrix** is the Cost Factors rule above ("gate macOS/Windows behind Linux passing"), not §"Never let lint block test results".
- **Signing is Phase 2.** The workflow explicitly does NOT sign. Authenticode signing via SignPath Foundation is a documented follow-on (ADR §Out of Implementation Scope plus the workflow's header and trailing comments). Do not add a signing step without the Phase-2 prerequisites.

### Refresh flow (updating the committed blob)

Canonical procedure: the `crates/ocx_store/src/shim.rs` module docs. In a dedicated PR:

1. `task rust:shim:build TARGET=x86_64-pc-windows-gnullvm` (and `aarch64-pc-windows-gnullvm`) — or download the `ocx-shim-fresh-<target>` artifact from a CI run, which exists because a dev box need not have a usable Zig.
2. Copy each `target/<triple>/shim/ocx-shim.exe` to `crates/ocx_store/src/shims/ocx-shim-{x86_64,aarch64}.exe`.
3. Record each blob's `sha256sum` in the matching per-arch `SHIM_SHA256` in `crates/ocx_store/src/shim.rs` — blob and constant must move together, or the canary reds.

## Windows & macOS Coverage — what each gate compiles

`cfg(windows)` and `cfg(target_os = "macos")` code is invisible to a host (Linux) build, so the coverage boundary is easy to overstate. What holds today:

| Gate | Platform scope |
|---|---|
| `task verify` (root) | **No Windows or macOS leg.** `.verify:lint` runs `rust:format:check` + `rust:clippy:check` directly, not `rust:verify`. |
| `task rust:verify` → `check:windows-cfg` | `cargo check -p ocx_shim --locked --all-targets --target {x86_64,aarch64}-pc-windows-msvc` — **`ocx_shim` only**, deliberately scoped in `taskfiles/rust.taskfile.yml`. It type-checks none of `ocx_lib`'s `cfg(windows)` arms (`env.rs::pathext`, `setup/session_path/windows.rs`, `package_manager/launcher/generate.rs`, `package_manager/tasks/render_toolchain.rs`). No equivalent check exists for `cfg(target_os = "macos")`. |
| `task rust:check:windows` | Whole-workspace `cargo xwin check` for both MSVC targets in Docker — covers those Windows arms, but is opt-in: no task chain and no workflow invokes it. No macOS counterpart. |
| `verify-basic.yml` | **No Windows or macOS leg** (D5, pinned by `test_basic_runs_on_linux_only`): a draft pull request compiles none of the `cfg(windows)` / `cfg(target_os = "macos")` arms, and a pull request into an `announce/**` branch compiles none of them at any point — deep's `pull_request.branches` is `[main]` (D10), so marking it ready changes nothing. |
| `verify-deep.yml` → `build` Windows leg / `cross-compile` | Native `cargo nextest run --workspace --target=x86_64-pc-windows-msvc --profile ci --locked`, and `cargo xwin build --release --target=aarch64-pc-windows-msvc` for arm64 (the only Windows-arm64 workspace build in CI). Non-draft PRs into `main`, the merge queue, pushes to `main`, `workflow_dispatch` / `workflow_call` — never a draft, a PR push before it is ready, or a PR into `announce/**`. The only CI gate that compiles *and runs* `ocx_lib`'s `cfg(windows)` code, `#[cfg(windows)]` unit tests included. |
| `verify-deep.yml` → `build` macOS leg (matrix) | Native `cargo nextest run --workspace --target=aarch64-apple-darwin --profile ci --locked` on `macos-latest`. Non-draft PRs into `main`, the merge queue, `workflow_dispatch` / `workflow_call` (the weekly `schedule` trigger skips this matrix unless `inputs.full`), pushes to `main` — never a draft, a PR push before it is ready, or a PR into `announce/**`. The only CI gate that compiles and runs the LaunchAgent session-PATH writer (`setup/session_path/macos.rs`). |

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