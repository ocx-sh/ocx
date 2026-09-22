---
paths:
  - taskfile.yml
  - taskfiles/**/*.yml
  - "**/taskfile.yml"
---

# Taskfiles Subsystem

[Taskfile](https://taskfile.dev) v3 = primary task runner for OCX. Root taskfile includes per-subsystem taskfiles + reusable templates under `taskfiles/`. Each subsystem own pipeline, expose contract back to root.

## File Layout

| Path | Owner | Loaded as |
|---|---|---|
| `taskfile.yml` | root | entry point -- `task verify`, `task default`, etc. |
| `.taskrc.yml` | root | project config -- `failfast: true` |
| `taskfiles/git.taskfile.yml` | cross-cutting | `git:` -- merge (fast-forward current branch onto main), hooks (install the commit and push hooks into git's own hooks directory) |
| `taskfiles/infra.taskfile.yml` | cross-cutting | `infra:` -- Cloudflare API plumbing (zones, DNS) via `~/.config/ocx-infra/cf.env` |
| `taskfiles/rust.taskfile.yml` | Rust subsystem | `rust:` -- format, clippy, license, build, test:unit, the crate-split structural gates (below) |
| `taskfiles/satellite.taskfile.yml` | cross-cutting | `satellite:` -- build ocx-mirror against this tree in a disposable worktree; deep tier only |
| `taskfiles/shell.taskfile.yml` | Shell subsystem | `shell:` -- shellcheck + shfmt called directly off the project toolchain |
| `taskfiles/ci.taskfile.yml` | CI subsystem | `ci:` -- actionlint (workflow lint) off the project toolchain |
| `taskfiles/bazel.taskfile.yml` | Bazel adoption | `bazel:` -- `bootstrap` (one-time crate_universe repin; `Cargo.bazel.lock.json` is gitignored per DX-23), `pin:check` (`.bazelversion` against the resolved binary + the `rust-toolchain.toml` / `MODULE.bazel` twin), `build:nobuild` (loading + analysis of `//...`, the `.bazelignore` check, and the BEP this tree's only build telemetry reads), `build:drift` (Cargo / `BUILD.bazel` dep-set comparison), `tag:guard`, `lint` (buildifier over every `BUILD`/`.bzl` file), `mod:check` (`bazel mod deps --lockfile_mode=error`), `test:unit` (`bazel test //crates/...`, run and floor read off one build event stream; wired into both `task verify` phase 2 and `verify-basic.yml`'s `smoke` job, replacing the old `rust:test:floor` / `rust:test:unit` / `rust:test:ceiling` cargo-nextest trio there), `test:accept` (`bazel test //test:all --local_test_jobs=1`, the acceptance suite as 181 `sh_test` targets **with results cached**; wired into `task verify` phase 2 and `verify-deep.yml`'s acceptance job in place of the direct `task test:parallel` call, and it carries `test/SUITE_FLOOR` plus the skip/xfail ceilings, read off the 181 per-case JUnit reports it copies out of `bazel-testlogs` into `target/bazel/accept/`. `--local_test_jobs=1` lives on that command line and in no rc file -- one compose stack on fixed ports, and an rc line would also serialise the 34 Rust test targets; `NOCACHE=1` adds `--nocache_test_results`, and it is NOT `--force` -- `task verify --force` would then never hit the cache this lane exists for), and the two A7 entry points `test` (full suite) and `test:scoped` (change-selected) -- neither wired into a lane: WP-30's NO-GO was about the *full-suite* `bazel test //...` costing more than the lane's budget, not about `bazel test` generally, and the two narrower scopes `//crates/...` and `//test:all` are what the lanes run today. Every call goes through `ocx exec bazel --`; deliberately **no** `verify` aggregator, because the plan splits the eight gates across both verify phases |
| `taskfiles/scripts.taskfile.yml` | gate tooling | `scripts:` -- `self-test` runs the `--self-test` of every gate script under `scripts/`, plus `.github/actions/*/selftest.sh`; `self-test:complete` is the reader that asserts the list names them all, so a self-test nobody invokes reds. `verify` is ruff over `scripts/` then that list |
| `taskfiles/xwin.taskfile.yml` | Rust subsystem | Windows MSVC cross-compile template; included with `(TARGET, SUBCOMMAND)` vars |
| `taskfiles/coverage.taskfile.yml` | cross-cutting | `coverage:` |
| `taskfiles/duplo.taskfile.yml` | cross-cutting | `duplo:` |
| `taskfiles/release.taskfile.yml` | cross-cutting | `release:` |
| `taskfiles/telemetry.taskfile.yml` | cross-cutting | `telemetry:` -- `push` sends a JUnit report to otel.ocx.sh as OTLP traces (tail of `rust:test:unit` and the pytest legs); `bazel` sends a Bazel BEP the same way through `scripts/bep_to_otlp.py` (tail of `bazel:build:nobuild`). Both are a silent no-op unless the machine is configured for it, and neither can fail a build (subsystem-ci.md § Test telemetry) |
| `.claude/taskfile.yml` | `.claude/` subsystem | `claude:` |
| `test/taskfile.yml` | acceptance tests | `test:` |
| `website/taskfile.yml` | website | `website:` (includes schema, sbom, recordings internally) |
| `website/schema.taskfile.yml` | website | `website:schema:` -- JSON schema generation |
| `website/sbom.taskfile.yml` | website | `website:sbom:` -- SBOM generation |
| `website/recordings.taskfile.yml` | website | `website:recordings:` -- terminal recordings |

## Two-Phase Verify

`task verify` run as two-phase pipeline via internal `.verify:lint` + `.verify:build-test` tasks, then writes a full verify mark:

1. **Phase 1 (parallel `deps:`)**: `rust:format:check`, `rust:clippy:check`, `shell:verify`, `ci:verify`, `claude:verify`, `scripts:verify`, `test:lint`, `website:lint:links`, `bazel:pin:check`
2. **Phase 2 (sequential `cmds:`)**: `rust:license:check`, `rust:license:deps`, `rust:license:notice:check`, `scripts:suite-census`, `scripts:dead-path-sweep`, `rust:lint:ratchet`, `rust:doc:ratchet`, `rust:build`, `bazel:build:nobuild`, `bazel:build:drift`, `bazel:tag:guard`, `bazel:lint`, `bazel:mod:check`, `bazel:test:unit`, `rust:test:ceiling:self-test`, `rust:test:duration`, `rust:test:duration:self-test`, `rust:test:doc`, `bazel:test:accept`

`taskfile.yml`'s `verify` summary is the authority these two lists paraphrase, and `.claude/tests/test_ai_config.py::TestVerifySummary` holds the summary and the phases together in both directions. The Bazel members sit where they do for a reason: `bazel:pin:check` needs no graph (`bazel --version` starts no server), while the other six read one and are ordered behind `bazel:build:nobuild`, which is what proves the graph loads at all. `bazel:test:unit` sits last of those six — it is both the workspace's unit-test run and its floor, replacing the `rust:test:floor` / `rust:test:unit` / `rust:test:ceiling` cargo-nextest trio that used to follow the Bazel gates; only `rust:test:ceiling:self-test` (the ceiling parser's own fixtures, still a cargo-side check) survives next to it. `bazel:lint` is the exception that is measured rather than assumed: with `Cargo.bazel.lock.json` moved aside `bazel run //:buildifier.check` still exits 0 while `bazel mod deps` exits 2, so `bazel:lint` alone needs no `bazel:bootstrap`. `bazel:test:accept` closes the phase, where `test:parallel` used to: the acceptance suite's 181 `sh_test` targets had existed since WP-36 with nothing running them, because this phase and `verify-deep.yml` both called pytest directly. It is the one lane whose results are **cached** (`adr_bazel_build_adoption.md` § Stage 4, amended) -- `no-sandbox` + `exclusive` and neither `external` nor `local`, because `local`'s `no-remote` half suppresses the disk-cache hit on the fresh server every CI runner is -- and of the two compensating controls for that, exactly one is live in a lane. `bazel:tag:guard` is: it runs here and in `verify-basic.yml`, and it refuses an acceptance target that stops declaring the binary or the compose definition, one that acquires a cache-suppressing tag (`external`, `local`, `no-cache`, `no-remote`, `no-remote-cache`), one that sweeps its sibling modules' source without declaring them, and a `//test:suite_inputs` whose own glob has narrowed. The other — `scripts/bazel_accept_proofs.py --check-s015`, the binary-digest half, which requires a binary-swap run in which nothing cached — is invoked by **no lane and no task**: it needs a rebuilt binary and a warm run, so it is a command a person runs, with the tag table from `task -d test bazel:tags`. Do not read it anywhere as running on its own. `task test:parallel` survives as the local xdist convenience and as `verify:scoped`'s acceptance leg.

## Verify Tiers and the Verify Mark

Three gates (ADR `adr_crate_split_workspace.md` § "Verification tiers"): `task` (fast check), `task verify:scoped` (per work package, local only), `task verify` (full; review, finalize, CI). `task satellite:verify` is a fourth, deep-tier-only gate.

| Task | What it does |
|---|---|
| `verify:scoped` | `scripts/scoped_gate.py --plan` decides from the diff since the last full mark's head (else `merge-base origin/main`): **escalate** to `task verify` (a hub crate with >= 4 reverse dependents, an ecosystem crate, a crate in `TABLE_ESCALATES` -- `ocx_lib`, `ocx_test_support` and, for phase 1, `ocx`: the rows of `test:scoped` that read `escalate`, mirrored in `scripts/scoped_gate.py` and kept equal by its `--self-test`, until WP-37 retires the phase-1 tooling -- an unrouted path, or the gate tooling under `scripts/**` changed -- its self-tests are routed too, and the full run's `.verify:lint` runs them); **routed** checks only (`claude:tests` for `.claude/**`, `.agents/**` and `CLAUDE.md`, actionlint, `test:parallel` over touched test files, `scripts:self-test`); or **scoped** -- routed checks, `cargo check --workspace`, per changed crate clippy / nextest / doc tests, the workspace `rust:doc:ratchet`, then `test:smoke` + `test:scoped`. Always `--force` -- the task refuses to run without it (`{{.CLI_FORCE}}`): a cached sub-task is a skipped step, and the mark must not certify one. |
| `verify:mark` | Hand-write a *scoped* mark after a passing verify when only merge context changed. Never `full`. |
| `.verify:mark` | Writes `.claude/hooks/.state/commit-verified` as JSON `{"timestamp", "head", "scope": "full"\|"scoped", "crates": [...], "toplevel"}` (a scoped mark carries `full_head` forward). `scripts/commit_gate.py`, which git runs through the `commit-msg` hook, reads it with a 300 s TTL, fail-closed: a bare integer, a missing scope, another HEAD, or a `toplevel` naming a different working tree is *not verified*; a `release:` commit, a commit on `main` and `task release:prepare` need a `full` mark, which only `task verify` writes. |
| `git:hooks` | Idempotent (`status:`): unsets `core.hooksPath`, then `git lfs install --local --force`, `prek install` and a copy of `scripts/pre-push.sh` — three owners sharing git's own hooks directory, in that order. Called by `default` and `verify` — there is no `task setup`, and those are what everyone working here runs; public rather than `internal:` so a fresh clone can arm itself from `task --list`. Measured, and each fact load-bearing: `git lfs install` **honours** an existing `core.hooksPath` instead of reclaiming it (git-lfs 3.7.1), which is why the unset comes first; `--git-common-dir` answers the same absolute path from a linked worktree as from the main one, so one run arms every worktree regardless of which branch it sits on; the last `status:` line is `cmp`, because a copy goes stale the moment the tracked script changes. `prek install` is never `--force` — that deletes a `<hook>.legacy` file, which is the only reason prek can share a directory at all. Why `commit-msg` is a prek hook and `pre-push` is not: prek hands a declared hook no stdin and skips the pre-push run outright for a branch delete and for a `--all` that only creates refs, so the trunk gate would stop seeing pushes it must refuse. `git lfs` keeps `post-checkout`/`post-commit`/`post-merge` as its own hooks again; `scripts/pre-push.sh` runs the gate first, then replays the same stdin to `git lfs pre-push`. |
| `test:smoke` | `-m smoke -n auto --dist loadgroup`: one marked happy path per top-level verb, no Sigstore stack; fails above the 90 s wall-clock budget. |
| `test:scoped -- <crate>...` | The acceptance subset the named crates own (`SCOPED_ROWS` in `test/taskfile.yml`); a crate without a row, an `escalate` row, or a glob collecting nothing fails. |
| `rust:doc:ratchet` | `cargo doc --workspace --no-deps --message-format=json` (`RUSTDOCFLAGS=--cap-lints warn`, own `target/docratchet`) through `scripts/lint_ratchet.py --by-file` against `rustdoc-warn-baseline.json`; keys each entry `<file>::<code>`, fails on any count above baseline, `-- --update` commits a decrease. Linux only. Replaced `rust:doc:check`, whose `-D rustdoc::broken_intra_doc_links` could not go green against a 400-entry backlog and which ran on the *scoped* arm only, so every escalated work package skipped the one check that reads intra-doc links. |
| `rust:deps:direction` | The `deps_direction` guards of `crates/ocx_test_support/tests/workspace_structure.rs`: every `ocx_*` edge in the `cargo metadata` graph is in `scripts/crate_map.toml`; reds naming `from -> to`. |
| `rust:test:loop-guard` | The `inert` guards of `crates/ocx_test_support/tests/workspace_structure.rs`: no loop inside test code has a body that can neither fail nor be observed — no call, macro, assignment, `.await`, `?` or wildcard-free `match`. Reds naming file, line and keyword. Its witness fixture must still yield `for`, `while` and `loop`, so a blind scanner reds instead of reporting a clean workspace. Gates nothing on its own (`test:unit` and the `verify:scoped` manifest route already reach it); it is the per-batch entry point. |
| `rust:lint:ratchet` | Second clippy run for lints with a backlog (`-W unreachable_pub`, `--cap-lints warn`) grouped by `scripts/lint_ratchet.py --by-file` against `clippy-warn-baseline.json`, one entry per `<file>::<code>`; fails on any key above baseline, `-- --update` commits a decrease. Linux only. |
| `rust:test:doc` | `cargo test --doc --workspace --locked`, on the **full** route. nextest runs no doctest, so `rust:test:unit` and its floor/ceiling bracket never execute one; `verify:scoped`'s per-crate `cargo test --doc -p <crate>` runs under `for: {var: CRATES}`, which is empty on an escalate — and every extraction escalates (`ocx_lib` ∈ `TABLE_ESCALATES`). Same defect `rust:doc:ratchet` was moved here for. Dev profile, matching the per-crate step so both arms judge the same thing. |
| `rust:test:floor` / `rust:test:ceiling` | Bracket `rust:test:unit`: nextest must list at least `crates/NEXTEST_FLOOR` tests, and the run's Summary line must skip at most `crates/NEXTEST_SKIP_CEILING`. Both `cmds:`, never `preconditions:` (`--force` skips preconditions). Linux only. |
| `rust:test:duration` | The 1.000 s per-test budget over the same run log `rust:test:ceiling` reads (`scripts/unit_test_duration_gate.py`): any test at or above it that no line of `crates/NEXTEST_SLOW_ALLOWLIST` covers (exact id or `fnmatch` glob) fails. Refuses a log holding fewer timed result lines than `crates/NEXTEST_FLOOR` — a reader that read a subset run prints the same green as a fast suite. `rust:test:duration:self-test` holds both reds and the green across all three log spellings. Linux only, so the claim is *every unit test that runs in the Linux unit suite*: `verify-deep.yml`'s matrix runs raw `cargo nextest` on three OSes and is not judged by this. |
| `satellite:verify` | `git worktree add` of the ocx-mirror checkout (`OCX_MIRROR_DIR`, default `../ocx-mirror`; missing = fail, never pass) under `.tmp/satellite-verify/`, rsync this tree over its `external/ocx`, `cargo build --workspace` (persistent `CARGO_TARGET_DIR` under the same dir), then the forward-closure check: `cargo tree -i <crate>` answers `did not match any packages` (asserted on the text, never the exit code) for every operations-tier crate, `ocx_store`'s direct dependents are a non-empty subset of `{ocx_index, ocx_package}` (the ecosystem-tier crates with a direct `ocx_store` edge in `scripts/crate_map.toml`; unlinked is accepted until one links it), and no mirror source or manifest names one. Worktree removed via `defer:` and again explicitly. Runs in `verify-deep.yml` only (`continue-on-error: true` until the mirror re-points; never a required check while set). |

## AI Quality Gate Pattern

Each subsystem rule has Quality Gate section specifying which verify task to run during review-fix loops. Full `task verify` runs only as final gate before commit. Stop expensive acceptance tests running every iteration.

## Taskfile Features in Use

| Feature | Purpose |
|---|---|
| `output: group` + `error_only: true` | Clean verify output; only show failures |
| `.taskrc.yml` | Project-wide `failfast: true` (abort on first failure) |
| Include-with-vars templates | `xwin.taskfile.yml` included multiple times with `(TARGET, SUBCOMMAND)` vars; uses `requires:` for mandatory vars |
| `internal: true` on includes | Template includes hidden from `task --list` |
| `:` prefix cross-references | Child taskfiles call root helpers like `:.ensure-cargo-tool` |
| `--force` | Bypass all caching for one run (replaces `rm -rf .task/`) |
| `--status` | Check freshness without running |
| `--summary` | Show task detail (e.g., `task --summary verify`) |
| `aliases:` | Backward compat after renames |
| `for:` loops | Iteration in cmds/deps |
| `defer:` | Cleanup that runs on failure too (`satellite:verify` removes its worktree). A failing deferred command does **not** fail the task -- repeat load-bearing cleanup as the last `cmds:` entry |

## OCX Conventions

1. **`verify` is a subsystem contract.** Every subsystem taskfile expose `verify` task. Root `verify` calls `<subsystem>:verify` instead of inlining subtasks.
2. **Composite tasks aggregate subtasks.** Each linter/formatter has own subtask so independently runnable. `verify` references composite tasks only -- one entry per concern.
3. **Helpers use dot-prefix + `internal: true`.** Internal tasks named with dot prefix (e.g., `.ensure-cargo-tool`, `.verify:lint`) -- like GitLab CI hidden jobs. Visually distinct + hidden from `task --list`.
4. **Tool installs use `status:` for idempotency.** `status: - cargo nextest --version` skips install when tool already present. Cross-include dedup pattern -- `run: once` does **not** work reliably across namespaced includes (go-task issue #852).
5. **Subsystem env overrides go on task, not include.** Project toolchain env (OCX_HOME, etc.) comes from direnv (`.envrc` → `ocx direnv export`) locally and from the `setup-ocx` action in CI -- taskfiles do not set these, and the root taskfile carries no `env:` block. This repository keeps **no committed index copy**: `ocx.lock` pins every tool's per-platform digest, so a task-invoked `ocx` needs no index, and a cold pull routes the logical `ocx.sh/<ns>/<pkg>` name through `index.ocx.sh` -- the same path `ocx exec` already takes in CI before `task` starts. Index redirection (`--index` / `OCX_INDEX`) stays a CLI feature; it is simply not wired into this repository's pipeline. When a task genuinely needs a different value, set it with per-task `env:`.

## Caching Contract

### `sources:` + `status:` + `method:`

| Field | Purpose | Recommendation |
|---|---|---|
| `sources:` | Files that force re-run when changed | Broad include + targeted excludes. Survive new file types. |
| `status:` | Exit 0 = up-to-date, skip | **Load-bearing for cache.** Use for output file checks. |
| `generates:` | Output declaration (docs only) | **Not load-bearing** (go-task #2181). Declare for discoverability; add `status:` if skip-on-present needed. |
| `method:` | `checksum` (default), `timestamp`, `none` | Only specify `timestamp` for tools with own incremental build (e.g. `cargo build`). |

**Sources relative to task's `dir:`.** Set `dir: '{{.TASKFILE_DIR}}'` or rely on include's `dir:` so globs use plain `**/*`.

**Two canonical patterns:**

```yaml
# Pattern A -- skip when output exists, re-run when sources change
schema:generate:
  cmds: [cargo run -p ocx_schema --release -- metadata > {{.OUT}}]
  sources: [crates/ocx_schema/src/**/*.rs, Cargo.lock]
  status: [test -f {{.OUT}}]

# Pattern B -- wrap a tool with its own incremental build
build:
  cmd: cargo build --release -p ocx --locked
  sources: [crates/**/*.rs, Cargo.lock]
  status: [test target/release/ocx -nt Cargo.lock]
  method: timestamp
```

### Sharing source lists: `x-shared:` anchors

Multiple tasks validate same input set → define source list once as YAML anchor under `x-shared:` (JSON-Schema extension convention), reference with `*alias`. Do not anchor under consuming task -- implies ownership data structure does not have.

```yaml
x-shared:
  claude-sources: &claude-sources
    - '**/*'
    - '../CLAUDE.md'
    - exclude: 'artifacts/**'
    - exclude: '**/__pycache__/**'

tasks:
  tests:
    sources: *claude-sources
  lint:links:
    sources: *claude-sources
```

### `.task/` cache directory

Task stores fingerprint state in `.task/`. **Add to `.gitignore`** before introducing any `sources:`/`method:` task.

## Reports and Artifacts (`REPORTS_DIR`)

Tasks that validate or check should emit machine-readable reports.

- **Location**: `target/reports/<subsystem>/` (already gitignored under `target/`).
- **Root var**: `REPORTS_ROOT: '{{.ROOT_DIR}}/target/reports'` in root taskfile.
- **Subsystem var**: Each subsystem resolves own `REPORTS_DIR` with `{{if/else}}` so standalone invocation works:

  ```yaml
  vars:
    REPORTS_DIR: '{{if .REPORTS_ROOT}}{{.REPORTS_ROOT}}/claude{{else}}{{.TASKFILE_DIR}}/reports{{end}}'
  ```

  Parent + child vars must have **different names** (`REPORTS_ROOT` vs `REPORTS_DIR`) to avoid double-suffixing.
- **Format**: JUnit XML (preferred) or JSON. Create dir with `mkdir -p {{.REPORTS_DIR}}` as first command.
- **Declare `generates:`** for docs even though not load-bearing.

## Includes and `dir:` Scoping

| Variable | Resolves to |
|---|---|
| `{{.ROOT_DIR}}` | Directory of entry-point (root) taskfile |
| `{{.TASKFILE_DIR}}` | Directory of currently executing taskfile |
| `{{.USER_WORKING_DIR}}` | Directory from which `task` was invoked |

Set `dir:` on include block when all tasks should run relative to sub-taskfile's directory (`.claude/`, `test/`, `website/`). Override per-task only when needed.

**Gotchas:** `${{.TASKFILE_DIR}}` wrong (becomes shell var). When root sets global `env:`, included tasks inherit -- override per-task if needed.

## Preconditions vs Status

| Field | Use | Semantics |
|---|---|---|
| `preconditions:` + `msg:` | Guard -- abort if not ready | Failure aborts; downstream tasks don't run. **`--force` skips preconditions** -- a gate step is a `cmds:` entry (`verify:scoped`, `rust:test:floor`, `satellite:verify` all do this) |
| `status:` | Cache -- skip if done | Exit 0 = up-to-date, skip silently |

## OCX-Specific Task Contracts

- **Generation tasks** (`schema:generate`, `recordings:*`) should depend on compiled binary via `deps: [build]` + `sources: [target/release/ocx_schema]`, NOT on Rust source file lists. Cargo already tracks source deps — duplicating in Taskfile = maintenance overhead.

## Sources

- [Taskfile schema reference](https://taskfile.dev/docs/reference/schema)
- [Taskfile usage](https://taskfile.dev/usage/) -- sources/status/method, run modes, env, preconditions
- [Taskfile templating](https://taskfile.dev/docs/reference/templating) -- special vars
- [go-task #2181](https://github.com/go-task/task/issues/2181) -- `generates:` globs not fingerprinted
- [go-task #852](https://github.com/go-task/task/issues/852) -- `run: once` cross-include dedup broken