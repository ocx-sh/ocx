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
| `taskfiles/bazel.taskfile.yml` | Bazel adoption | `bazel:` -- `bootstrap` (one-time crate_universe repin; `Cargo.bazel.lock.json` is gitignored per DX-23), `pin:check` (`.bazelversion` against the resolved binary + the `rust-toolchain.toml` / `MODULE.bazel` twin), `build:nobuild` (loading + analysis of `//...`, the `.bazelignore` check, and the BEP this tree's only build telemetry reads), `build:drift` (Cargo / `BUILD.bazel` dep-set comparison), `tag:guard`, `lint` (buildifier over every `BUILD`/`.bzl` file), `mod:check` (`bazel mod deps --lockfile_mode=error`), `test:unit` (`bazel test //crates/...`, run and floor read off one build event stream; wired into both `task verify` phase 2 and `verify-basic.yml`'s `smoke` job, replacing the old `rust:test:floor` / `rust:test:unit` / `rust:test:ceiling` cargo-nextest trio there), `test:accept` (`bazel test //test:all --local_test_jobs=$ACCEPT_JOBS`, the acceptance suite as one `sh_test` target per `test/tests/test_*.py` module (172 today — check the live count, it only ever falls as modules move to `test/lint/` or get ported down) **with results cached**; wired into `task verify` phase 2 and `verify-deep.yml`'s acceptance job in place of the direct `task test:parallel` call, and it carries `test/SUITE_FLOOR` plus the skip/xfail ceilings, read off the per-case JUnit reports it copies out of `bazel-testlogs` into `target/bazel/accept/`. Every module on `test/bazel.bzl`'s `UNCACHED_MODULES` carries `external` and re-runs regardless of the cache (one named residual today: `tests/test_windows_shim.py`, `subsystem-tests.md` § Verification tiers). A failed run during a work-package merge (`MERGE_HEAD` present) appends escape records via `scripts/scoped_gate.py --record-escapes` — one line per module the merged tip's own `verify:scoped` runs never selected — and pushes `ocx.gate=full` / `ocx.gate.escape` telemetry through `task telemetry:accept`; both are observation-only and never change this step's own exit status. The targets run concurrently against the one compose stack, at xdist parity, behind the runner's host locks (`test/bazel.bzl` § Concurrency). `ACCEPT_JOBS` defaults to `min(8, nproc)` and `ACCEPT_JOBS=<n>` overrides it; `--local_test_jobs` lives on that command line and in no rc file -- an rc line is per-command, so it would also throttle the 35 Rust test targets; `NOCACHE=1` adds `--nocache_test_results`, and it is NOT `--force` -- `task verify --force` would then never hit the cache this lane exists for), and the two A7 entry points `test` (full suite) and `test:scoped` (change-selected) -- neither wired into a lane: WP-30's NO-GO was about the *full-suite* `bazel test //...` costing more than the lane's budget, not about `bazel test` generally, and the two narrower scopes `//crates/...` and `//test:all` are what the lanes run today. Every call goes through `ocx exec bazel --`; deliberately **no** `verify` aggregator, because the plan splits the eight gates across both verify phases |
| `taskfiles/scripts.taskfile.yml` | gate tooling | `scripts:` -- `self-test` runs the gate scripts' pytest suite, `scripts/tests/` (parallel, 20 s budget; `.github/actions/*/selftest.sh` runs from it too). Discovery is the list: `test_completeness.py` reds a gate script with no `test_<script>.py`. `verify` is ruff over `scripts/` then that suite |
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

1. **Phase 1 (parallel `deps:`)**: `rust:format:check`, `shell:verify`, `ci:verify`, `claude:verify`, `scripts:verify`, `test:lint` (ruff over the acceptance suite), `test:lint:structure` (the T0 lint tier — `test/lint/`, uncached, `test/LINT_FLOOR` and a 30 s budget), `test:rows:check` (the scoped gate's coverage guard over `test/scoped_rows.toml` and the `command` markers), `website:lint:links`, `bazel:pin:check`
2. **Phase 2 (sequential `cmds:`)**: `rust:license:check`, `rust:license:deps`, `rust:license:notice:check`, `scripts:suite-census`, `scripts:dead-path-sweep`, `rust:clippy:check`, `rust:doc:ratchet`, `rust:build`, `bazel:build:nobuild`, `bazel:build:drift`, `bazel:tag:guard`, `bazel:lint`, `bazel:mod:check`, `bazel:test:unit`, `bazel:test:accept`

`taskfile.yml`'s `verify` summary is the authority these two lists paraphrase, and `.claude/tests/test_ai_config.py::TestVerifySummary` holds the summary and the phases together in both directions. The Bazel members sit where they do for a reason: `bazel:pin:check` needs no graph (`bazel --version` starts no server), while the other six read one and are ordered behind `bazel:build:nobuild`, which is what proves the graph loads at all. `bazel:test:unit` sits last of those six but one — it is both the workspace's unit-test run and its floor — doctests included, one `rust_doc_test` per library crate, which is why no `rust:test:doc` step exists — replacing the `rust:test:floor` / `rust:test:unit` / `rust:test:ceiling` cargo-nextest trio that used to follow the Bazel gates; only `rust:test:ceiling:self-test` (the ceiling parser's own fixtures, still a cargo-side check) survives next to it. `bazel:lint` is the exception that is measured rather than assumed: with `Cargo.bazel.lock.json` moved aside `bazel run //:buildifier.check` still exits 0 while `bazel mod deps` exits 2, so `bazel:lint` alone needs no `bazel:bootstrap`. `bazel:test:accept` closes the phase, where `test:parallel` used to: the acceptance suite's `sh_test` targets (one per `test/tests/test_*.py` module, 172 today — `ACCEPTANCE_MODULE_TARGETS` in `scripts/bazel_gate_proofs.py`) had existed since WP-36 with nothing running them, because this phase and `verify-deep.yml` both called pytest directly. It is the one lane whose results are **cached** (`adr_bazel_build_adoption.md` § Stage 4, amended) -- `no-sandbox` alone, neither `exclusive` (AM-9: the targets run concurrently) nor `external` nor `local`, because `local`'s `no-remote` half suppresses the disk-cache hit on the fresh server every CI runner is -- and of the two compensating controls for that, exactly one is live in a lane. `bazel:tag:guard` is: it runs here and in `verify-basic.yml`, and it refuses an acceptance target that stops declaring the binary or the compose definition, one that acquires a cache-suppressing tag (`external`, `local`, `no-cache`, `no-remote`, `no-remote-cache`), one that sweeps its sibling modules' source without declaring them, and a `//test:suite_inputs` whose own glob has narrowed. The other — `scripts/bazel_accept_proofs.py --check-s015`, the binary-digest half, which requires a binary-swap run in which nothing cached — is invoked by **no lane and no task**: it needs a rebuilt binary and a warm run, so it is a command a person runs, with the tag table from `task -d test bazel:tags`. Do not read it anywhere as running on its own. `task test:parallel` survives as the local xdist convenience and as `verify:scoped`'s leg for a touched `test/tests/test_*.py`; the scoped arm's acceptance leg is `test:smoke` + `test:scoped` over the plan's `acceptance_globs`.

## Verify Tiers and the Verify Mark

Three gates (ADR `adr_crate_split_workspace.md` § "Verification tiers"): `task` (fast check), `task verify:scoped` (per task / review-fix iteration, local only), `task verify` (full — mechanically enforced at the work-package merge by the commit gate, and required at finalize and in CI). `task satellite:verify` is a fourth, deep-tier-only gate.

| Task | What it does |
|---|---|
| `verify:scoped` | `scripts/scoped_gate.py --plan` decides, reading `test/scoped_rows.toml` (`plan_test_speed_tiers.md` C-014): **escalate** to `task verify` — a path under a `[security]` glob (checked first), a command file a `[verbs]` entry names or that no `command` marker selects, any other path under `crates/ocx_cli/` (permit-list default — `ocx` left `TABLE_ESCALATES` under ADR D3), a hub crate (≥ 4 reverse dependents), an ecosystem crate, a crate whose `[crates]` row reads `escalate` (today just `ocx_test_support`), a path with no route, or the gate tooling under `scripts/**` changed (its self-tests are routed too, and the full run's `.verify:lint` runs them); **routed** — the T0 lint tier (`test:lint:structure`) and coverage guard (`test:rows:check`) run on every routed and scoped arm (an escalate reaches both through `verify`), then only the routed checks: ruff for `test/lint/**`, `claude:tests` for `.claude/**`/`.agents/**`/`CLAUDE.md`, actionlint (+ `test_workflows.py`) for `.github/**`, `test:parallel` over the touched `test/tests/test_*.py`, `scripts:self-test` for `scripts/**`, the workspace-structure guards for a changed `crates/<c>/Cargo.toml`/README; or **scoped** — the routed checks, `cargo check --workspace`, per changed crate clippy, one `bazel:test:unit` (cached — executes the changed crates' reverse dependents, serves the rest; its `rust_doc_test` targets are the doctests), the workspace `rust:doc:ratchet`, then `test:smoke` and `test:scoped` over the plan's `acceptance_globs` — run as concurrent lanes (`.verify:scoped:lanes`): the lint tier and `bazel:test:unit` then the doc ratchet (both at `--jobs=4`, sequential within the lane — and queued on the Bazel server's lock behind the cargo lane's own `bazel build` of the acceptance binaries in `test:build`) beside one cargo lane that builds the acceptance binaries first, then runs the two pytest legs beside check and clippy — at most one cargo invocation compiles at a time, so the arm peaks at cargo's 12 jobs plus Bazel's 4, not three 12-job compilers; same steps, none dropped, and `.taskrc.yml`'s `failfast` stops the rest on the first red. Run it as `task verify:scoped --force`: the sub-tasks keep their `sources:` cache, so without `--force` an unchanged step is skipped and the mark still gets written — nothing refuses the bare form, which is why the flag is the documented spelling. On a non-escalate decision the run's selection is appended to the shared scoped-run log (`--log-run`, C-018) under the common git dir, which a T2 failure during a WP merge reads to attribute escapes (`subsystem-tests.md` § Verification tiers). |
| `verify:mark` | Hand-write a *scoped* mark — the allowed escape hatch for any commit before finalize: run the checks the change needs, mark, name the deferral in the commit body. Also after a passing verify when only merge context changed. Never `full`. |
| `.verify:mark` | Writes `.claude/hooks/.state/commit-verified` as JSON `{"timestamp", "head", "scope": "full"\|"scoped", "crates": [...], "toplevel", "tree", "nocache"}` (a scoped mark carries `full_head` forward). `tree` is the *proven* tree: `git write-tree` of the index, recorded only when `--mark-precheck` at `task verify`'s start snapshot the same tree and the working tree equalled the index at both ends — otherwise `null`, which no merge commit accepts. `nocache` is true when `NOCACHE=1` reached the run. `scripts/commit_gate.py`, which git runs through the `commit-msg` hook, reads it with a 300 s TTL, fail-closed: a bare integer, a missing scope, another HEAD, or a `toplevel` naming a different working tree is *not verified*; a `release:` commit, a commit on `main` and `task release:prepare` need a `full` mark, which only `task verify` writes — and a `release:` commit one with `nocache` true (`task verify NOCACHE=1`, which `release:prepare` runs). **While `MERGE_HEAD` exists**, only a `full` mark whose `tree` equals the tree being committed admits the commit (C-017/D4 — `workflow-git.md` "Work-Package Merges"); `task verify` refuses to start with a working tree that differs from its index during a merge, and refuses to write the mark if the merged tree changed mid-run (AM-8). |
| `git:hooks` | Idempotent (`status:`): unsets `core.hooksPath`, then `git lfs install --local --force`, `prek install` and a copy of `scripts/pre-push.sh` — three owners sharing git's own hooks directory, in that order. Called by `default` and `verify` — there is no `task setup`, and those are what everyone working here runs; public rather than `internal:` so a fresh clone can arm itself from `task --list`. Measured, and each fact load-bearing: `git lfs install` **honours** an existing `core.hooksPath` instead of reclaiming it (git-lfs 3.7.1), which is why the unset comes first; `--git-common-dir` answers the same absolute path from a linked worktree as from the main one, so one run arms every worktree regardless of which branch it sits on; the last `status:` line is `cmp`, because a copy goes stale the moment the tracked script changes. `prek install` is never `--force` — that deletes a `<hook>.legacy` file, which is the only reason prek can share a directory at all. Why `commit-msg` is a prek hook and `pre-push` is not: prek hands a declared hook no stdin and skips the pre-push run outright for a branch delete and for a `--all` that only creates refs, so the trunk gate would stop seeing pushes it must refuse. `git lfs` keeps `post-checkout`/`post-commit`/`post-merge` as its own hooks again; `scripts/pre-push.sh` runs the gate first, then replays the same stdin to `git lfs pre-push`. |
| `rust:build` / `test/taskfile.yml`'s `.build-binaries` | `.build-binaries` runs `bazel build //crates/ocx_cli:ocx //crates/ocx_shim:ocx_shim //crates/ocx_schema:ocx_schema_bin` and copies the outputs into `test/bin/` for the pytest entry points; `rust:build` only delegates to it (with `SKIP_BUILD` off), so nothing compiles the acceptance binary twice and cargo compiles it not at all. The Bazel acceptance targets run the same `//crates/ocx_cli:ocx` straight from their runfiles, and `bazel:test:accept` builds it itself. The `__testing` provenance placeholders come from `crates/ocx_cli/BUILD.bazel` (`_TESTING_PROVENANCE`), not `build.rs`, which Bazel never runs; `task release:provenance:proof` reds if they stop reaching the binary. `.claude/tests/test_ai_config.py` (`TestAcceptanceBinaryOneBuild`) holds the one-producer shape. |
| `test:smoke` | `-m smoke -n auto --dist loadgroup`: one marked happy path per top-level verb, no Sigstore stack; fails above the 90 s wall-clock budget. |
| `test:scoped -- <glob>...` | Takes acceptance-module globs, not crate names — the modules `verify:scoped` selected (`acceptance_globs` from `test/scoped_rows.toml` rows and `command` markers, moved off `test/taskfile.yml`'s old `SCOPED_ROWS`, which no longer exists); a glob collecting nothing fails. |
| `rust:doc:ratchet` | One cached Bazel pass, `rust:clippy:check`'s shape: `.bazelrc`'s bare `build` registers `rustdoc_diagnostics_aspect` (`rustdoc_diagnostics.bzl`, over the private `rustdoc_compile_action`) beside clippy's, running rustdoc on every non-test crate with `cargo doc --no-deps`'s flags, JSON error format (its own `//:rustdoc_error_format` setting) and `--cap-lints=warn`, stderr to `<name>.rustdoc.diagnostics`. The task builds `//crates/... --output_groups=rustdoc_diagnostics` with the same BEP handling, then `scripts/lint_ratchet.py --by-file --suffix .rustdoc.diagnostics --allow-codes 'rustdoc::'` against `rustdoc-warn-baseline.json`; keys each entry `<file>::<code>`, fails on any count above baseline, `-- --update` commits a decrease. Per-target coverage is scoped per suffix: every configured non-test Rust target must leave a file. `BAZEL_ARGS` carries the scoped Bazel lane's `--jobs=4`. Linux only. Replaced `rust:doc:check`, whose `-D rustdoc::broken_intra_doc_links` could not go green against a 400-entry backlog and which ran on the *scoped* arm only, so every escalated work package skipped the one check that reads intra-doc links. |
| `rust:deps:direction` | The `deps_direction` guards of `crates/ocx_test_support/tests/workspace_structure.rs`: every `ocx_*` edge in the `cargo metadata` graph is in `scripts/crate_map.toml`; reds naming `from -> to`. |
| `rust:test:loop-guard` | The `inert` guards of `crates/ocx_test_support/tests/workspace_structure.rs`: no loop inside test code has a body that can neither fail nor be observed — no call, macro, assignment, `.await`, `?` or wildcard-free `match`. Reds naming file, line and keyword. Its witness fixture must still yield `for`, `while` and `loop`, so a blind scanner reds instead of reporting a clean workspace. Gates nothing on its own (`test:unit` and the `verify:scoped` manifest route already reach it); it is the per-batch entry point. |
| `rust:clippy:check` | One cached Bazel pass: `.bazelrc`'s bare `build` registers `rust_clippy_aspect` with `clippy_output_diagnostics`, JSON error format and `clippy_flag=-Wunreachable_pub` (BZL-RUST-23; settings on the rc, never the task line, so no invocation switch drops the analysis cache), and the task builds `//crates/... --output_groups=clippy_output` with `--build_event_json_file` in a 0700 `mktemp -d` (0600 stream, deferred removal — `bazel:test:unit`'s handling). `scripts/lint_ratchet.py --by-file --bep` reads the `*.clippy.diagnostics` files that stream names (never a `bazel-bin` glob), attributes each to its target's `crates/<member>` package, and fails on any `<file>::<code>` above `clippy-warn-baseline.json` — an absent key is 0, so one compare is both the `-D warnings` gate and the backlog ratchet; this deliberately supersedes rust-cargo.md LINT-15's two invocations. Refuses an unfinished stream, a member with no diagnostics file, a `[lints]` entry Bazel does not read (C-017), and a baseline code other than `unreachable_pub` even under `--allow-regression` (C-018). A bazel failure exits with bazel's status and the ratchet is not run. `-- --update` commits a decrease. Linux only. Lints nothing without a Bazel target (`ocx_cli/build.rs`, `linux_self_contained.rs`) and only the `__testing` feature set — [ocx-sh/ocx#533](https://github.com/ocx-sh/ocx/issues/533). `rust:clippy:fix` stays on cargo. |
| `rust:test:floor` / `rust:test:ceiling` | Bracket `rust:test:unit`: nextest must list at least `crates/NEXTEST_FLOOR` tests, and the run's Summary line must skip at most `crates/NEXTEST_SKIP_CEILING`. Both `cmds:`, never `preconditions:` (`--force` skips preconditions). Linux only. |
| `rust:test:duration` | The 1.000 s per-test budget over the same run log `rust:test:ceiling` reads (`scripts/unit_test_duration_gate.py`): any test at or above it that no line of `crates/NEXTEST_SLOW_ALLOWLIST` covers (exact id or `fnmatch` glob) fails. Refuses a log holding fewer timed result lines than `crates/NEXTEST_FLOOR` — a reader that read a subset run prints the same green as a fast suite. `scripts/tests/test_unit_test_duration_gate.py` holds both reds and the green across all three log spellings. Linux only, so the claim is *every unit test that runs in the Linux unit suite*: `verify-deep.yml`'s matrix runs raw `cargo nextest` on three OSes and is not judged by this. |
| `satellite:verify` | `git worktree add` of the ocx-mirror checkout (`OCX_MIRROR_DIR`, default `../ocx-mirror`; missing = fail, never pass) under `.tmp/satellite-verify/`, rsync this tree over its `external/ocx`, `cargo build --workspace` (persistent `CARGO_TARGET_DIR` under the same dir), then the forward-closure check: `cargo tree -i <crate>` answers `did not match any packages` (asserted on the text, never the exit code) for every operations-tier crate, `ocx_store`'s direct dependents are a non-empty subset of `{ocx_index, ocx_package}` (the ecosystem-tier crates with a direct `ocx_store` edge in `scripts/crate_map.toml`; unlinked is accepted until one links it), and no mirror source or manifest names one. Worktree removed via `defer:` and again explicitly. Runs in `verify-deep.yml` only (`continue-on-error: true` until the mirror re-points; never a required check while set). |

## AI Quality Gate Pattern

Each subsystem rule has a Quality Gate section, and every one of them now reads the same sentence: per task / review-fix iteration, `task verify:scoped --force`; full `task verify` runs at WP merge (enforced by the commit gate), at finalize, and whenever `verify:scoped` escalates (it then runs `task verify` itself). Stops expensive acceptance tests running every iteration.

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
sbom:generate:
  cmds: [uv run scripts/sbom-to-markdown.py --input {{.IN}} --output {{.OUT}}]
  sources: ['{{.IN}}', scripts/sbom-to-markdown.py]
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

- **Generation tasks** never fingerprint Rust source lists. A generator that is a Bazel target is a Bazel action: `schema:generate` builds the `//crates/ocx_schema:schemas` genrule (`ocx exec bazel -- bazel build`, `test:.build-binaries`' `CACHE_RC`), reads its outputs with `cquery --output=files`, refuses anything but the seven expected paths before writing one, and carries no `sources:` — Bazel is the cache. It calls `bazel:bootstrap` as a nested `task -d <root>`, because `website/schema.taskfile.yml` is included at two depths and go-task resolves a leading `:` one namespace up, not at the root. A cargo-built generator (`recordings:*`) depends on the compiled binary via `deps: [build]` + `sources: [<binary>]`; cargo already tracks source deps.

## Sources

- [Taskfile schema reference](https://taskfile.dev/docs/reference/schema)
- [Taskfile usage](https://taskfile.dev/usage/) -- sources/status/method, run modes, env, preconditions
- [Taskfile templating](https://taskfile.dev/docs/reference/templating) -- special vars
- [go-task #2181](https://github.com/go-task/task/issues/2181) -- `generates:` globs not fingerprinted
- [go-task #852](https://github.com/go-task/task/issues/852) -- `run: once` cross-include dedup broken