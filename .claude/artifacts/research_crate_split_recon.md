# Research: ocx crate-split recon

## Metadata

**Date:** 2026-09-06
**Domain:** cli
**Triggered by:** /hex-discuss crate-split-and-verify-tiers
**Expires:** 2027-03-06

## Direct answer

`ocx_lib/src` is 34 top-level modules (278,777 LOC, 364 files), and the module graph is not layered: 63 of the ~120 populated edges are reciprocated (2-cycles), so almost every module both depends on and is depended on by several others. `cli` (the lib-internal `crate::cli`, distinct from the `ocx_cli` crate) is the busiest hub — highest combined in+out degree — followed by `utility`, `oci`, `env`, `file_structure`, `log`, `package`. `oci` alone is 87k LOC across 86 files (largest: `oci/verify/pipeline.rs` at 10,131 lines); `package_manager` is 43.6k LOC. There is one 33-variant crate-wide `Error` enum plus roughly 45 module-local `*Error` enums, only 3 of which have an explicit `From` into the top type. `task verify` runs lint (parallel) then build+test (sequential, cached by `sources:`/`generates:` where declared); the acceptance suite (2,412 tests across 158 files) is CI-measured at ~9–27 minutes depending on workflow, with no CLI-command→test-file mapping and only 4 files using `xdist_group` for cross-test coordination. There is standing owner doctrine (2026-07-16, `arch-principles.md`) to split `ocx_lib` long-term, and one concrete boundary already enforced by a test (`shell/` importing no `crate::project`) specifically to keep that split viable.

## Key findings

1. **34 top-level modules, no clean layering.** `crates/ocx_lib/src/lib.rs:33-67` declares them (`config`, `media_type` private; 32 `pub mod`). 63 pairs are mutually coupled (both directions reference each other via `crate::<mod>`) — see Module graph below. Only `media_type` and `log` have zero internal-module fan-out (true leaves); `script` has zero in-degree (nothing else depends on it).
2. **`oci` is the largest and most-referenced subsystem.** 87,307 LOC / 86 files (`crates/ocx_lib/src/oci/**`), in-degree 22 (22 other modules call into it), out-degree 18. Its own largest files: `oci/verify/pipeline.rs` (10,131 lines), `oci/client.rs` (8,157), `oci/index/chained_index.rs` (6,385).
3. **`package_manager` is the densest single edge.** `package_manager -> oci` is 317 references (by far the heaviest edge in the whole graph), and `package_manager -> file_structure` is 165. `package_manager/tasks/resolve.rs` is 8,123 lines, `package_manager/composer.rs` is 6,604.
4. **The lib-internal `cli` module (`crates/ocx_lib/src/cli/`) is the top hub**, not `oci`: in-degree 25, out-degree 23 (touches 23 of the other 33 modules). This is a distinct module from the `ocx_cli` crate (`crates/ocx_cli/src/**`) — a naming collision worth flagging on its own.
5. **One crate-wide `Error`, many module-local ones.** `crates/ocx_lib/src/error.rs:9` declares 33 variants in a single `pub enum Error`. Only 4 `impl From` blocks exist there (`package::error::Error`, `patch::PatchError`, `package_manager::error::PackageErrorKind`, plus `Error -> ArcError`). Separately, ~45 other `pub enum *Error`/`pub enum Error` types exist scattered across modules (e.g. `oci/client/error.rs`, `oci/sign/error.rs`, `package/error.rs`, `project/error.rs`, `config/error.rs`, `managed_config/persistence.rs` — 3 in one file). Most do **not** funnel through an explicit `From` into the top `Error` — construction-site counts (caveat: text-match on `Error::`, so a module's own local `Error` enum is indistinguishable from the crate-wide one in this count) run from 1 (`media_type`) to 1,018 (`oci`).
6. **Test volume: 5,192 inline unit tests in `ocx_lib/src`, 954 in `ocx_cli/src`, plus 4 integration test files (682 lines) in `crates/ocx_lib/tests/` and 2 (99 lines) in `crates/ocx_cli/tests/`.** `oci` alone carries 1,377 of the lib's inline tests; `package` 808; `package_manager` 372; `shell` 282.
7. **`task verify` is two phases plus a mark step**, defined at `taskfile.yml:76-108`: `.verify:lint` (parallel: rust format/clippy, `shell:verify`, `ci:verify` (actionlint), `claude:verify`, `test:lint` (ruff)) then `.verify:build-test` (sequential: license check/deps/notice, `rust:build`, `rust:test:unit` = `cargo nextest run --workspace --release`, `test:parallel` = pytest `-n auto --dist loadgroup`), then `.verify:mark` writes `.claude/hooks/.state/commit-verified` with a Unix timestamp. The pre-commit hook (`.claude/hooks/pre_commit_verification.py`) hard-denies any `git commit` in this repo unless that timestamp is under 300 seconds old (`hook_utils.py:289`).
8. **Coverage, benchmarks, doc-script drift, and the index-conformance-drift check are explicitly outside `task verify`.** `coverage.taskfile.yml` (`cargo llvm-cov`), `test/taskfile.yml`'s `bench:*` family (needs Docker + hyperfine, separately timed 1–10 min per tier), `test:doc-scripts:drift` (a named subset re-runner, not a separate gate — same tests `test:parallel` already collects), and `index-conformance-drift` (network-bound, run only on `verify-deep.yml`'s weekly schedule) are all opt-in tasks, not deps of `verify`.
9. **No CLI-command→test-file mapping exists.** `test/pyproject.toml` defines only two pytest markers (`requires_tty`, `divergence`, neither about subsystem scoping) and no env-var-based subsetting convention. The only cross-test coordination is `xdist_group`, used in exactly 4 files (`test_doc_scripts.py`, `test_frozen.py` x3 blocks, `test_patches.py`) to serialize a shared global patch-descriptor slot under `-n auto`.
10. **14 AI-config rule files declare a `paths:` glob naming `crates/`**, of which 2 are workspace-wide (`arch-principles.md`: `crates/**/*.rs`; `workflow-bugfix.md`/`workflow-refactor.md`: `crates/**`) and the rest are scoped to one module or sub-tree (`subsystem-oci.md`, `subsystem-package.md`, `subsystem-package-manager.md`, `subsystem-file-structure.md`, `subsystem-script.md`, `subsystem-cli.md`, `subsystem-cli-api.md`, `subsystem-cli-commands.md`, `subsystem-metadata-schema.md`, `subsystem-deps.md`, `quality-cli-help.md`). `.claude/tests/test_ai_config.py` has no hard-coded module-shape path assertions beyond one regression test (`test_package_manager_glob_not_too_broad`, line 328) guarding against an overly broad `*.rs` glob — not a structural dependency on the current module layout. No nested `CLAUDE.md` exists anywhere under `crates/`.
11. **There is standing owner doctrine to split `ocx_lib`, and one enforced boundary already built for it.** `arch-principles.md:20-25` ("Core vs Plugin Boundary," dated 2026-07-16): "Long-term: split `ocx_lib` into smaller, cleanly layered crates (future `ocx-lib` repo); plugins link foundation crates, drive operations via CLI." Concretely, `arch-principles.md:157,169` and `.claude/artifacts/adr_shell_env_addenda.md:1984-1999` (resolution **A-45**, closing part of [ocx-sh/ocx#343](https://github.com/ocx-sh/ocx/issues/343)) record a real module-placement decision made *because of* the future split: per-prompt shell sequencing lives at the crate root (`crate::activation`) rather than under `shell/`, specifically because `project::consent` reads `shell`, and a `shell/`→`project` dependency would be a `use` cycle that "does not compile across a crate boundary." This is enforced today by a real test, `shell_does_not_import_project` at `crates/ocx_lib/src/activation.rs:1159`.
12. **A prior, related split already happened one repo up.** `1476192c refactor!: move ocx-mirror to its own repository` and `.claude/artifacts/adr_cli_repo_split_template.md` document `ocx-mirror` being extracted from this mono-repo into `ocx-sh/ocx-mirror`, vendoring `ocx` as a submodule with `ocx_lib` as a path dependency — an accepted, reusable template ("CLI Repo-Split Template") explicitly earmarked for future satellites (`ocx-mcp`, `ocx-dist`), not for splitting `ocx_lib` itself.
13. **CI timing (most recent successful runs, feature branch, no evidence exists for `main` in the last 15 runs — see negative below).** "Basic Verification" (workflow id 34045468912, 2026-09-06): Smoke (Linux) 1068s, Smoke (Windows) 426s, Acceptance Tests 558s. "Verify Deep" (34042282820, same day): Build & Unit Test Linux 1054s, macOS 1295s, Windows 1918s; Acceptance (Linux) 1604s; Cross-compile Windows ARM64 581s; Index Conformance Drift 12s.

## Module graph

Top-level modules = every `pub mod X;` / `mod X;` in `crates/ocx_lib/src/lib.rs` (33 pub + `config`/`media_type` private = 34 counted; `error`/`log` counted too). LOC = combined `X.rs` + `X/**/*.rs` where both exist (many modules are `X.rs` + `X/` submodule dir — legal old-style Rust, not a duplicate).

### LOC per top-level module (descending)

| Module | LOC |
|---|---|
| oci | 87,307 |
| package_manager | 43,574 |
| package | 26,829 |
| project | 17,575 |
| config | 14,404 |
| shell | 11,542 |
| utility | 9,928 |
| file_structure | 8,535 |
| setup | 6,691 |
| record | 5,505 |
| cli | 4,754 |
| forge | 4,583 |
| env | 4,398 |
| publisher | 3,752 |
| announce | 3,718 |
| trust | 3,458 |
| script | 3,013 |
| managed_config | 2,882 |
| patch | 2,451 |
| auth | 1,855 |
| ci | 1,673 |
| launch | 1,546 |
| archive | 1,441 |
| activation | 1,372 |
| reference_manager | 1,123 |
| symlink | 972 |
| lazy | 815 |
| codesign | 610 |
| shim | 489 |
| hardlink | 472 |
| compression | 461 |
| error | 456 |
| sbom | 414 |
| media_type | 91 |
| log | 4 |

Total `ocx_lib/src`: 278,777 LOC, 364 files. Five largest files workspace-wide: `oci/verify/pipeline.rs` (10,131), `oci/client.rs` (8,157), `package_manager/tasks/resolve.rs` (8,123), `package_manager/composer.rs` (6,604), `oci/index/chained_index.rs` (6,385).

### Adjacency (module → depends-on, edge = distinct `crate::<target>` reference count within the source module's own files)

```
config          -> auth:2 cli:14 env:35 file_structure:12 log:5 managed_config:4 oci:45 package_manager:1 project:5 record:4 trust:20 utility:4
media_type      -> (none)
activation      -> cli:3 env:4 file_structure:1 oci:1 package:2 package_manager:2 project:17 reference_manager:1 shell:3 trust:1
announce        -> cli:10 forge:10 oci:20 package:1 publisher:2
archive         -> cli:4 compression:1 oci:1 symlink:14 utility:2
auth            -> cli:2 env:1 file_structure:2 oci:6 utility:1
ci              -> cli:2 env:18 log:2 package:10 utility:5
cli             -> config:7 activation:1 announce:7 archive:2 auth:1 ci:1 compression:1 env:2 error:1 file_structure:2 forge:3 launch:3 log:8 managed_config:11 oci:71 package:13 package_manager:4 patch:2 project:3 publisher:3 record:4 setup:7 utility:9
codesign        -> env:1 hardlink:1
compression     -> cli:2
env             -> config:28 auth:1 cli:13 lazy:2 package:48 record:6 shell:4 utility:3
error           -> config:1 archive:1 auth:1 ci:1 cli:2 compression:1 file_structure:1 managed_config:2 oci:14 package:3 package_manager:12 patch:3 project:3 record:2 shell:1 utility:4
file_structure  -> config:4 cli:3 env:2 error:26 hardlink:1 log:7 managed_config:1 oci:72 package:1 project:5 reference_manager:4 shim:21 symlink:1 utility:40
forge           -> announce:1 cli:1 oci:4 utility:1
hardlink        -> symlink:2
launch          -> cli:1 env:4 file_structure:1 log:6 oci:1 package:2 package_manager:1 record:7 utility:3
lazy            -> cli:1 env:9 log:3
log             -> (none)
managed_config  -> config:15 media_type:11 cli:22 file_structure:11 log:5 oci:27 package:4 publisher:2 trust:6 utility:4
oci             -> config:32 media_type:6 auth:2 cli:113 codesign:1 env:3 error:24 file_structure:34 log:17 managed_config:1 package:26 package_manager:1 patch:9 project:1 publisher:16 sbom:1 trust:69 utility:32
package         -> archive:1 cli:28 env:8 error:5 file_structure:2 log:1 oci:62 package_manager:2 publisher:3 utility:11
package_manager -> config:20 media_type:2 archive:2 cli:26 env:3 error:24 file_structure:165 hardlink:2 lazy:1 log:22 managed_config:16 oci:317 package:96 patch:48 project:9 publisher:6 sbom:2 shim:5 symlink:15 trust:12 utility:20
patch           -> cli:3 error:1 oci:15 package_manager:4
project         -> config:19 cli:36 env:14 file_structure:16 lazy:1 log:13 oci:48 package:10 package_manager:2 reference_manager:4 shell:2 symlink:7 trust:4 utility:10
publisher       -> cli:18 oci:23 package:11 package_manager:1 utility:4
record          -> config:5 cli:2 env:8 file_structure:4 launch:2 log:1 oci:6 package:6 package_manager:2 utility:1
reference_manager -> file_structure:8 oci:12 symlink:17
sbom            -> trust:1
script          -> auth:1 env:27 oci:12 symlink:5 utility:5
setup           -> config:37 cli:8 env:2 error:6 file_structure:2 log:4 managed_config:20 oci:37 package:2 package_manager:6 shell:1 utility:9
shell           -> config:8 activation:5 cli:4 env:27 file_structure:2 log:1 oci:1 package:12 symlink:1 utility:18
shim            -> hardlink:1 symlink:1 utility:1
symlink         -> archive:3 reference_manager:5 utility:1
trust           -> config:5 cli:3 log:1 managed_config:1 oci:28 utility:5
utility         -> config:1 archive:4 cli:10 env:7 error:57 file_structure:3 hardlink:1 launch:1 log:6 shell:8 symlink:31
```

### In-degree ranking (hubs — most other modules depend on it)

cli 25, utility 23, oci 22, env 18, file_structure 16, log 16, package 16, config 13, package_manager 12, symlink 10, error 8, managed_config 8, project 7, trust 7, archive 6, auth 6, publisher 6, shell 6, hardlink 5, record 5, patch 4, reference_manager 4, media_type 3, compression 3, launch 3, lazy 3, activation 2, announce 2, ci 2, forge 2, sbom 2, shim 2, codesign 1, setup 1, script 0.

### Out-degree ranking (how many other modules it reaches into)

cli 23, package_manager 21, oci 18, error 16, file_structure 14, project 14, config 12, setup 12, utility 11, activation 10, managed_config 10, package 10, record 10, shell 10, launch 9, env 8, trust 6, announce 5, archive 5, auth 5, ci 5, publisher 5, script 5, forge 4, patch 4, lazy 3, reference_manager 3, shim 3, symlink 3, codesign 2, compression 1, hardlink 1, sbom 1, media_type 0, log 0.

### Leaves and near-leaves

- **True leaves (no internal top-level dependency at all):** `media_type`, `log`.
- **Zero in-degree (nothing depends on it):** `script` — a candidate for extraction if it stays isolated, though it still has out-degree 5 (`env`, `oci`, `symlink`, `auth`, `utility`).
- **Lightest hubs by both directions:** `codesign` (in 1 / out 2), `sbom` (in 2 / out 1), `shim` (in 2 / out 3), `compression` (in 3 / out 1), `hardlink` (in 5 / out 1) — these look like plausible low-risk extraction candidates on edge count alone.

### Cycles

63 reciprocated (2-cycle) module pairs exist — i.e. most of the graph. Listing the full pairwise reciprocation table is not useful signal on its own (see the adjacency block above, which is symmetric-heavy by inspection); the notable pattern is that **`cli`, `oci`, `config`, `env`, `file_structure`, `package_manager`, and `utility` are each in a 2-cycle with more than half the other modules**, meaning any crate boundary drawn to isolate one of them from the rest requires resolving that module's back-edges into the isolated crate, not just its forward edges. No 3+ node cycles were specifically enumerated beyond the 2-cycles (not checked; see negative).

## Verify pipeline

`task verify` (`taskfile.yml:76-88`) = `.verify:lint` → `.verify:build-test` → `.verify:mark`, fail-fast, no parallelism *between* the two verify phases (parallelism is only within `.verify:lint`'s own `deps:` list, which go-task runs concurrently: `rust:format:check`, `rust:clippy:check`, `shell:verify`, `ci:verify`, `claude:verify`, `test:lint`).

- **`.verify:lint`** (parallel deps): `rust:format:check` (`cargo fmt --check`, cached via `sources: *rust-sources`), `rust:clippy:check` (`cargo clippy --workspace --locked --all-targets -- -D warnings`, same cache key), `shell:verify` (shellcheck + shfmt, no cache — deliberately, per `test/taskfile.yml:110-117`'s comment on `lint`), `ci:verify` (actionlint), `claude:verify` (`.claude/tests/test_ai_config.py` via pytest + lychee link check, cached on a broad `.claude/**` + `CLAUDE.md`/`AGENTS.md`/`taskfile.yml`/`taskfiles/**` glob), `test:lint` (ruff, no cache — same "cached-green looks like never-ran" rationale).
- **`.verify:build-test`** (sequential): `rust:license:check` (hawkeye headers), `rust:license:deps` (cargo-deny), `rust:license:notice:check` (cargo-about diff, deliberately uncached), `rust:build` (`cargo build --release -p ocx --features ocx/__testing --locked`, cached via a `status:` double-check — binary exists AND nothing newer than it — not just `sources:`), `rust:test:unit` (`cargo nextest run --workspace --release --locked`, cached), `test:parallel` (pytest acceptance suite, `-n auto --dist loadgroup`, cached on `tests/**/*.py`, `src/**/*.py`, `conftest.py`, `pyproject.toml`, `doc_scripts/**/*.sh`, and the release binary itself).
- **`.verify:mark`**: writes a Unix timestamp to `.claude/hooks/.state/commit-verified`, consumed by the pre-commit hook.
- **`task` (default, fast check):** `rust:format:check` + `rust:clippy:check` + `check` (bare `cargo check`) — no build, no tests, no license, no lint of shell/CI/claude/python.
- **Outside `verify` entirely:** `coverage:*` (`cargo llvm-cov`), `test:bench:*` (needs Docker + hyperfine; tiers run <1 min / <4 min / <10 min), `test:doc-scripts:drift` (named pytest subset — same tests `test:parallel` already collects, just scoped for iteration), `index-conformance-drift` (network-bound, weekly schedule on `verify-deep.yml` only), `test:shells` (Docker shell-zoo matrix), `test:shell-latency` (NFR latency gate with fault injection).
- **Pre-commit hook** (`.claude/hooks/pre_commit_verification.py` + `hook_utils.py:289`): PreToolUse hook on `Bash`, fires when the command contains `git commit` and the commit's resolved git root matches this project. Hard-denies (not a warning) unless `commit-verified` is younger than **300 seconds**. Scoped correctly to avoid false-firing on cross-repo commits via `cd other-repo && git commit`.
- **Acceptance test build + registry:** `test/taskfile.yml:36-50` builds `ocx` + `ocx_shim` with `--features ocx/__testing` and copies both into `test/bin/`. The registry is *not* started by the taskfile directly — `test/conftest.py`'s `pytest_sessionstart` + session-scoped fixtures (`registry`, `mirror_registry`, `target_registry`, `legacy_registry`) start it via `docker compose` against `test/docker-compose.yml` (zot for the primary/Referrers-capable registry, `registry:2` for the legacy/mirror one). `test:parallel`/`test:quick` add `-n auto --dist loadgroup` (pytest-xdist), with `--dist loadgroup` specifically to let `xdist_group`-marked tests serialize on a shared resource (the global patch-descriptor slot) while everything else parallelizes freely.

## Tests

- **`ocx_lib/src` inline tests:** 5,192 (`#[test]` + `#[tokio::test]`), heaviest in `oci` (1,377), `package` (808), `package_manager` (372), `shell` (282), `config` (361), `utility` (225).
- **`ocx_cli/src` inline tests:** 954, spread thin across `command/` (413) and `api/` (296, mostly `api/data/**`), `options/` (119), `app/` (59).
- **`crates/ocx_lib/tests/`** (integration, 4 files, 682 lines): `dispatch_conformance.rs`, `index_wire_conformance.rs`, `live_index_wire.rs`, `tag_verdicts.rs`, plus a `fixtures/` tree (21 files, 184K: `index_wire/{dispatch,config,catalog,root,cpython}`, `live_index_ocx_sh/`). There is also a `crates/ocx_lib/test/` (singular) directory of shared test helpers (`data.rs`, `env.rs`, `manifest_source.rs`, `mod.rs`) wired in via `#[path = "../test/mod.rs"]` in `lib.rs` — not cargo's `tests/` convention, a hand-rolled shared-fixture module for the inline unit tests.
- **`crates/ocx_cli/tests/`**: 2 files, 99 lines (`linux_self_contained.rs`, `macos_self_contained.rs`).
- **Acceptance suite (`test/tests/`):** 158 files, ~2,412 test functions (rough grep count, includes async defs and nested class methods). No CLI-command→file mapping exists in config; grouping below is by filename convention only (approximate, some files span >1 group):

| Group (approx.) | File count |
|---|---|
| package (create/inspect/lifecycle/metadata/schema/entrypoints) | 26 |
| OCI/registry/referrers/mirror/push-pull | 14 |
| env/shell/direnv/toolchain | 11 |
| project (init/add/remove/lifecycle/hooks/crash-recovery) | 11 |
| cosign/sign/attest/trust | 12 |
| install/self-setup/self-update/uninstall | 9 |
| config/managed-config/frozen | 9 |
| doc-scripts drift gate | 8 |
| exec/run/launcher | 8 |
| index/catalog/describe | 7 |
| state/select/status/which | 5 |
| lazy/offline/warm-resolve | 5 |
| clean/purge | 3 |
| patch/lock | 2 |
| misc infra (exit codes, color, completion, plugin dispatch, bench smoke, scenarios smoke, golden fixtures, verify, inspect) | 17 |
| sbom, login | 2 |
| helper modules (not tests: `announce_helpers.py`, `fake_forge.py`, `fake_gitlab.py`, `fake_registry.py`, `conftest.py`) | 5 |

- **Markers:** only `requires_tty` and `divergence` (`test/pyproject.toml`) — neither is a subsystem/subsetting mechanism, both are behavioral (skip-on-no-tty, pin-a-known-disagreement).
- **`xdist_group` usage:** 4 files total (`test_doc_scripts.py`, 3 blocks in `test_frozen.py`, `test_patches.py`) — all serializing the same shared global patch-descriptor slot.
- **Session-scoped fixtures** (`test/conftest.py`): `registry`, `mirror_registry`, `target_registry`, `legacy_registry` (all real Docker containers via `pytest_sessionstart`), `ocx_binary`. Per-test fixtures (`ocx_home`, `ocx`, `fake_forge`, `html_mirror`, `forward_proxy`, `mock_credential_helper`) are function-scoped.

## AI-config path coupling

14 rule files declare a `paths:` glob naming something under `crates/`:

| Rule | `crates/` globs |
|---|---|
| `arch-principles.md` | `crates/**/*.rs` (+ `external/**/*.rs`) |
| `workflow-bugfix.md` | `crates/**` (+ `test/**`, `website/**`) |
| `workflow-refactor.md` | `crates/**` |
| `subsystem-oci.md` | `crates/ocx_lib/src/oci/**` (+ `external/rust-oci-client/**`) |
| `subsystem-package.md` | `crates/ocx_lib/src/package/**`, `crates/ocx_lib/src/package.rs` |
| `subsystem-package-manager.md` | `crates/ocx_lib/src/package_manager/**`, `crates/ocx_lib/src/package_manager.rs` |
| `subsystem-file-structure.md` | `crates/ocx_lib/src/file_structure/**`, `crates/ocx_lib/src/file_structure.rs`, `crates/ocx_lib/src/reference_manager.rs` |
| `subsystem-script.md` | `crates/ocx_lib/src/script/**` |
| `subsystem-metadata-schema.md` | `crates/ocx_lib/src/package/metadata/**`, `crates/ocx_schema/**` |
| `subsystem-cli.md` | `crates/ocx_cli/src/**` |
| `subsystem-cli-api.md` | `crates/ocx_cli/src/api/**`, `crates/ocx_cli/src/command/**` |
| `subsystem-cli-commands.md` | `crates/ocx_cli/src/command/**` |
| `quality-cli-help.md` | `crates/ocx_cli/src/**` |
| `subsystem-deps.md` | `Cargo.toml`, `crates/*/Cargo.toml` |

`.claude/tests/test_ai_config.py` has exactly one hard-coded `crates/` path literal (line 334, inside `test_package_manager_glob_not_too_broad`'s docstring — a regression comment, not an assertion body) and a generic dead-glob detector (`glob.glob` against every rule's declared `paths:`) that would catch a glob broken by a crate-boundary move but does not itself encode the current module layout. No nested `CLAUDE.md` exists under `crates/` (checked, none found).

## Archaeology

- **Owner doctrine exists and is current.** `arch-principles.md` "Core vs Plugin Boundary (owner doctrine, 2026-07-16)": *"Long-term: split `ocx_lib` into smaller, cleanly layered crates (future `ocx-lib` repo); plugins link foundation crates, drive operations via CLI."* Also: *"Known drift: `ocx-mirror` reaches into operational internals — migration target is CLI for operations (pending refactor)."*
- **One concrete module-placement decision already made because of the planned split**, tracked as resolution **A-45** in `.claude/artifacts/adr_shell_env_addenda.md:1984-1999`, closing the placement half of [ocx-sh/ocx#343](https://github.com/ocx-sh/ocx/issues/343): per-prompt shell-env sequencing sits at `crate::activation` (crate root), not under `shell/`, specifically because `shell/` must stay import-free of `crate::project` to avoid a `use` cycle that "does not compile across a crate boundary and blocks the `ocx_lib` split." Enforced by `shell_does_not_import_project` (a directory-walk test) at `crates/ocx_lib/src/activation.rs:1159`.
- **No ADR titled for an `ocx_lib` crate split exists.** Search of `.claude/artifacts/` for `ocx_core`/`ocx_oci`/"crate split"/"split.*crate" found no dedicated ADR — only the one arch-principles.md doctrine paragraph and the one addenda resolution above.
- **A related but distinct split already happened and is documented as a reusable template:** `.claude/artifacts/adr_cli_repo_split_template.md` ("CLI Repo-Split Template," Accepted, 2026-06-12) records moving `ocx-mirror` out of this mono-repo into `ocx-sh/ocx-mirror` (commit `1476192c refactor!: move ocx-mirror to its own repository`), vendoring `ocx` as a git submodule with `ocx_lib` as a path dependency. This is explicitly a *repo* split template for satellite CLIs (`ocx-mcp`, `ocx-dist` are named as future candidates), not a plan to split `ocx_lib` itself into multiple crates.
- **No ADR found about the test-tier structure or the verify gate's shape.** The verify pipeline's design rationale lives only in taskfile comments (e.g. the `rust:build` `status:` double-check comment, the `test:lint`/`shell:verify` "cached green looks like never-ran" comment), not in a `.claude/artifacts/adr_*` file.
- `d94f8ae3 refactor(cli): expose the ocx crate as a library target` — the commit that gave `ocx_cli` its `lib.rs` (used today by `ocx_schema`) — is the closest prior "crate-shape" commit in git log, but it is about exposing an existing crate as a library target, not a module/crate split.

## negative:

- Could not enumerate cycles beyond pairwise (2-cycle) reciprocation — a 3+-node cycle detector was not built; given how dense the 2-cycle set already is (63 of ~120 populated pairs), a full cycle basis would likely be dominated by 2-cycles anyway, but this was not verified.
- `Error::` construction counts in Key finding 5 are a text-match on the token `Error::`, which cannot distinguish the crate-wide `crate::error::Error` from a same-named module-local `pub enum Error` (at least 15 modules define one). Directional (who constructs the *crate-wide* type specifically) was not separately measured; only the total token count and the fact that only 4 explicit `From` impls exist in `error.rs` are solid.
- No local timing evidence exists: `.tmp/` is empty, `test/bench/results/` is empty (no baseline or dashboard has been generated on this machine/branch).
- The last 15 `gh run list` entries contained no `Basic Verification` or `Verify Deep` run on `main` — all recent CI activity is on `feat/index-claim-command`. Timing evidence in this report is therefore from the feature branch's latest runs, not a `main` baseline; branch-to-branch variance was not checked.
- The acceptance-suite grouping table is approximate (filename-convention judgment call), not derived from an actual command→test mapping, because no such mapping exists in the repo to read.
- Did not check `crates/ocx_schema/` or `crates/ocx_shim/` module-internal structure — out of scope per the task's focus on `ocx_lib`/`ocx_cli`.
- A sibling research file, `.agents/research/research_crate_split_prior_art.md`, already exists in this directory from a parallel lane; its contents were not read (out of scope for this recon, which was to gather ocx-local evidence only).

## leads:

- **Hub decomposition:** `cli` (lib-internal), `oci`, and `package_manager` are the three modules whose removal from a hypothetical "core" crate would cut the most edges — worth a dedicated pass on what each actually needs from the other two versus what's incidental coupling (e.g. logging, error conversion).
- **Error-type consolidation:** the ~45 module-local `*Error` enums with only 4 `From` impls into the crate-wide `Error` suggests either a lot of `anyhow`-style context-wrapping bypassing the typed error, or dead/underused module-local types — worth an actual audit of how those local types reach callers before any crate boundary is drawn through them.
- **Verify-gate cost breakdown by phase, on `main`:** current timing evidence is branch-only; a same-day `main` run (post-merge) would give a cleaner baseline for any "is verify too slow" argument.
- **Acceptance-suite parallelism ceiling:** with only 4 `xdist_group`-marked files, `-n auto --dist loadgroup` parallelism is close to fully unconstrained already — if a crate split changes what needs rebuilding before acceptance tests run, the binary-build step (not the pytest run) is likely the bigger lever.

## Responsibility map (follow-up)

Follow-up recon, same read-only constraints. Covers the 7 directory-backed hubs (`oci`, `cli`, `package_manager`, `utility`, `config`, `file_structure`, `package`). `env` has no sub-directory — it is one 4,398-line file with a single internal `pub mod keys { … }` (env.rs:14) plus its own `#[cfg(test)] mod tests` (env.rs:1844); it is not decomposable one level down the way the other seven are, so it is omitted from the per-sub-module table below.

Sub-module grouping follows the same file+dir convention as the top-level report (`X.rs` + `X/` = one sub-module, when both exist). LOC = combined. Responsibility text is taken verbatim (truncated) from each file's leading `//!`/`///` doc comment where one exists; where none exists, it is inferred from the `pub struct`/`pub enum`/`pub trait` names actually declared in the file (marked *[from type names]*).

### Per-hub sub-module tables (all 8 hubs)

The task asked for every hub's sub-modules with LOC and a one-line responsibility. `oci` is below in its own subsection (it also gets the deeper classification). The other six directory-backed hubs follow immediately; `env` (no sub-directory) is noted separately.

#### `cli` (lib-internal, `crates/ocx_lib/src/cli/`)

| Sub-module | LOC | Responsibility |
|---|---|---|
| clap | 47 | Clap parse boundary — drives `try_get_matches`, maps every clap failure to an `ExitCode` |
| classify | 1,339 | Error → `ExitCode` classification shared by all OCX binaries, via the `ClassifyExitCode` trait |
| data_interface | 503 | *[from types]* `Annotation`/`Column`/`Cell`/`DataInterface`, trait `TreeItem` — tabular/tree data-rendering model |
| error | 197 | Typed CLI errors shared across OCX binaries (`UsageError`, `MetadataResolutionError`) |
| error_category | 169 | Coarse error categories for the structured JSON error envelope (`error.kind` wire contract) |
| exit_code | 219 | Process exit codes shared by all OCX binaries, aligned to BSD `sysexits.h` |
| human | 104 | Human-readable byte-size formatting for plain-text reports |
| log_level | 53 | Log level for controlling logging verbosity |
| log_settings | 194 | *[from types]* `LogSettings` |
| options | 213 | Shared CLI option types for OCX binaries (incl. `options/color_mode.rs`, `options/progress_mode.rs`) |
| printer | 393 | *[from types]* `Printer`/`Style`/`Line`/`Alignment` — low-level text rendering |
| progress | 604 | *[from types]* `ProgressManager`/`Spinner`/`BytesBar`/`LogWriter` — progress-bar and log-interleaving |
| styles | 23 | clap help/usage styling |
| theme | 512 | Central, swappable colour theme — every stdout data-rendering style lives here |
| user_interface | 141 | *[from types]* `UserInterface` — status/warn/success/prompt helpers |

#### `package_manager`

| Sub-module | LOC | Responsibility |
|---|---|---|
| composer | 6,604 | Two-env composition: flat iteration over each root's pre-built transitive closure with cross-root dedup |
| concurrency | 104 | Concurrency cap for parallel package operations (shared `tokio::sync::Semaphore`) |
| error | 607 | *[from types]* `Error`/`PackageError`/`OfflineManifestMissing`/`ShimClaim`/`PackageErrorKind`/`DependencyError` |
| launcher | 1,299 | On-disk launcher scripts that wrap `ocx exec` per entrypoint |
| tasks | 34,251 | No module-level doc comment; the task-dispatch layer (largest sub-module in the crate) — includes `tasks/resolve.rs` (8,123 LOC), `tasks/inspect.rs`, `tasks/patch_discovery.rs`, `tasks/common.rs` |

Two non-Rust, git-tracked files also sit in this directory: `composer.rs.all` / `composer.rs.fns` (function/test-name dumps) — excluded from LOC, see negative.

#### `utility`

| Sub-module | LOC | Responsibility |
|---|---|---|
| boolean_string | 103 | *[from types]* `BooleanString` enum |
| child_process | 135 | Generic boundary between OCX and spawned child-process exit status |
| fs | 8,201 | Filesystem primitives (12 files — see grab-bag analysis below) |
| list | 183 | Move-to-back deduplication for separator-joined option-list values |
| path | 506 | Move-to-front dedup + segment removal for `PATH`-style **environment** values (not filesystem paths) |
| result_ext | 12 | *[from types]* trait `ResultExt` |
| schema | 34 | Hand-built JSON Schema fragments `schemars` cannot infer |
| serde_ext | 42 | *[from types]* trait `SerdeExt` |
| singleflight | 545 | Watch-based singleflight for async work deduplication |
| string_ext | 63 | *[from types]* trait `StringExt` |
| tls | 28 | Shared TLS root-seeding for hand-rolled `reqwest::Client` builders |
| vec_ext | 61 | *[from types]* trait `VecExt` |

#### `config`

| Sub-module | LOC | Responsibility |
|---|---|---|
| error | 149 | *[from types]* `ConfigSource`, `Error` |
| insecure | 478 | The one answer to "may this registry be contacted over plain HTTP?" (union of `insecure=true` + `OCX_INSECURE_REGISTRIES`) |
| loader | 5,846 | Configuration discovery and loading (largest file in `config`) |
| managed | 1,391 | Corporate-managed configuration tier (`[managed]`) |
| mirror | 2,124 | Per-traffic-host mirror configuration (`[mirrors."<host>"]`) |
| patch | 1,283 | Site-tier patch registry configuration (`[patches]`) — the execution-env twin of the mirror tier |
| registry | 442 | Per-registry configuration (`[registries.<name>]`), including the `index` field selecting the resolution protocol |
| shell | 1,790 | The `[shell]` config section: enablement toggles + activation consent whitelist |

#### `file_structure`

| Sub-module | LOC | Responsibility |
|---|---|---|
| blob_store | 677 | *[from types]* `BlobDir`/`BlobStore` |
| cas_path | 384 | *[from types]* `CasTier` enum |
| error | 77 | *[from types]* `Error` |
| index_store | 3,074 | Self-contained local index store — hosted `index.ocx.sh` served-tree layout |
| layer_store | 253 | *[from types]* `LayerDir`/`LayerStore` |
| package_store | 983 | No module doc comment (assembled-package store) |
| shim_bin_store | 725 | Content-addressed store for the embedded `ocx-shim` executable blob |
| shim_store | 556 | Identity-keyed store for generated shim directories (deferred-tool on-disk form) |
| state_store | 681 | No module doc comment |
| symlink_store | 214 | No module doc comment |
| temp_store | 607 | No module doc comment |

#### `package`

| Sub-module | LOC | Responsibility |
|---|---|---|
| bin_scan | 1,174 | Create-time interface-binaries auto-scan (fills/verifies the `binaries` claim) |
| bundle | 298 | No module doc comment |
| cascade | 7,100 | Cascade algebra and platform-aware push orchestration (rolling-tag updates) |
| dependency_pinning | 777 | Index-driven dependency pin resolution for `ocx package create` |
| description | 382 | No module doc comment (the package README/icon description claim) |
| error | 217 | *[from types]* package `Error` |
| info | 15 | *[from types]* `Info` |
| install_info | 218 | No module doc comment |
| install_status | 80 | *[from types]* `InstallStatus` |
| libc_lint | 1,569 | Create-time libc lint — checks packaged binaries' actual libc demand vs. declared platform |
| metadata | 12,188 | No module-level doc comment; the package-metadata subsystem (second-largest sub-module in `package`) |
| resolved_package | 755 | No module doc comment |
| tag | 886 | *[from types]* `InternalTag`/`Tag` |
| version | 1,151 | No module doc comment |

#### `env` (no sub-directory)

`env.rs` is a single 4,398-line file with one internal `pub mod keys { … }` (env.rs:14, environment-variable name constants) and its own `#[cfg(test)] mod tests` (env.rs:1844). It has no file/dir decomposition one level down, so it does not fit the sub-module table shape the other seven hubs use — flagged as its own data point: a 4.4k-line flat file is itself a finding about `env`'s internal structure (or lack of one).

### `oci` sub-modules (23)

| Sub-module | LOC | Responsibility |
|---|---|---|
| annotations | 25 | OCI Image Specification pre-defined annotation keys |
| attest | 3,794 | In-toto attestations in DSSE envelopes, attached as OCI referrers (cosign-compatible) |
| client | 14,892 | *[from type names]* `Client`/`ClientBuilder` — the registry HTTP client (`ClientError`, `MirrorMap`, `ReferrersListing`) |
| copy | 2,022 | Registry-to-registry transfer of an already-published package (leaf platform manifest + its blobs + referrers, copied verbatim) |
| digest | 551 | *[from type names]* `Algorithm`/`Digest`/`DigestError` — OCI content-addressed digest type |
| endpoint | 1,418 | SSRF-hardened URL validation for Sigstore endpoints (`--fulcio-url`/`--rekor-url`) |
| file_storage | 4 | Near-empty stub |
| host_capabilities | 2,451 | Host libc detection for platform-aware OCI index resolution (maps host libc families to `os.features` tag values) |
| identifier | 1,291 | *[from type names]* `Identifier`/`IdentifierError` — OCI reference-string (`name:tag@digest`) parsing |
| index | 22,139 | Self-contained local index store — hosted `index.ocx.sh` served-tree layout (`config.json`, `c/index.json` catalog, per-repo roots, digest-verified dispatch CAS) |
| layer_layout | 287 | Per-layer placement config carried in manifest layer-descriptor annotations (`sh.ocx.layer.*`) |
| manifest | 275 | OCI manifest utilities — image-index admission and platform membership |
| manifest_builder | 473 | Pure assembly of OCI image manifests, deliberately decoupled from any OCX domain type |
| pinned_identifier | 311 | Pinned reference/digest identifier (no doc comment; small, self-contained) |
| platform | 3,274 | OCI Image Index platform-variant object and the D1 compatibility relation used by fresh-resolve/lock-read/authoring-pinning |
| referrer | 1,310 | OCI 1.1 referrer artifacts (signatures, SBOMs, attestations) attached by digest via the Referrers API |
| repository | 178 | No doc comment, no distinctive types found — thin repository-name helper |
| resolve_target | 289 | The `--platform` optionality rule shared by sign/attest/verify — a pure decision over an OCI Image Index resolution outcome, no I/O |
| sign | 9,182 | Cosign-compatible keyless signing (Sigstore bundle v0.3 → OCI referrer) |
| simplesigning | 253 | The cosign *simplesigning* claim wire format — a `sha256-<hex>.sig` sidecar payload shape, byte-verified against real cosign output |
| ssrf | 1,166 | Default-on SSRF guard for remote-controlled registry hosts (an index root's `repository` pointer can name any host) |
| transport_policy | 1,022 | Retry and timeout policy for HTTP transports — three value objects + one driver, all transport-agnostic |
| verify | 20,575 | Full keyless Sigstore verification — Fulcio cert chain, Rekor SET, signature, `--certificate-identity`/`--certificate-oidc-issuer` checks |

### `oci` classification — OCI-spec-generic vs. OCX-specific

Classified by (a) real out-edges to other top-level modules (unambiguous `crate::X` grep — trustworthy) and (b) precise-token checks for genuine OCX concepts (`crate::oci::index`/`IndexImpl`/`LocalIndex`, literal `ocx.lock`/`Lockfile`, literal `ocx.toml`, `crate::project`/`crate::config`/`crate::env`, `announce`), each spot-checked against source to rule out false positives from overloaded English words. Two classes of false positive were found and excluded: (1) generic "index" hits that are the OCI-spec term **Image Index** (a manifest list — e.g. `resolve_target`, `platform`'s "OCI Image Index" spec sense), not OCX's own resolution index type; (2) "Claim" hits that are cosign's own `SimpleSigningClaim` wire-format term, not an OCX metadata `*Claim` type. Both are noted per-row.

| Sub-module | Classification | Evidence | Out-edges (`crate::X` count) |
|---|---|---|---|
| annotations | OCI-generic | zero OCX-token hits; pure image-spec annotation-key constants | (none) |
| attest | mixed | real: `crate::oci::index::{Index,IndexImpl,IndexOperation}` (8, genuine — builds an `IndirectingIndex` test double); "Publish" hits are generic prose, not real | cli:2, file_structure:1 |
| client | mixed, heavily | real: `announce` (7), package "description" feature (9), `config::` (10), `project::`(1); this is the single largest, most cross-cutting file (14,892 LOC) | config:8, media_type:3, auth:1, cli:32, codesign:1, file_structure:1, log:4, managed_config:1, package:3, patch:9, publisher:16 |
| copy | mostly OCI-generic | one real, light hit: literal `ocx.lock` (V2/V3 physical-address pin, per `adr_lock_records_physical_address.md`) | media_type:1, cli:2, log:1, package:6 |
| digest | OCI-generic | zero real OCX-token hits (earlier raw "lock"/"desc" hits were `spawn_blocking`/JSON-schema "description" — false positives, verified) | cli:2 |
| endpoint | OCI-generic (doc-only mention) | the two `ocx.toml` hits are threat-model prose in doc comments ("a hostile `ocx.toml`…"), not a real code dependency — no config/project out-edge exists | cli:12, utility:1 |
| file_storage | OCI-generic | trivial stub | (none) |
| host_capabilities | mixed | real: `crate::env` (10) for host-derived os.features | file_structure:4, utility:2 |
| identifier | mixed, light | real: `crate::project`(2), `crate::env`(2), one literal `ocx.toml` | cli:2, project:1 |
| index | OCX-specific, unambiguous | this module *is* OCX's own resolution-index protocol, not part of the OCI distribution spec at all | config:21, cli:40, error:24, file_structure:22, log:9, package:2, package_manager:1, utility:19 |
| layer_layout | OCI-generic | one incidental "Publish" prose hit, no real edge | cli:3, utility:2 |
| manifest | OCI-generic | zero OCX-token hits | (none) |
| manifest_builder | OCI-generic, by design | doc comment states it explicitly: "intentionally decoupled from any OCX domain type" | package:1 |
| pinned_identifier | OCI-generic | zero OCX-token hits | cli:2 |
| platform | mixed | real, and load-bearing: doc comment itself says "every `ocx.lock` / dependency-pin map key"; 7 literal `ocx.lock`/lock-read/dependency-pin hits, `crate::env`(4) | cli:2 |
| referrer | mostly OCI-generic | one incidental `crate::env` hit | file_structure:1, log:1, sbom:1, utility:2 |
| repository | OCI-generic | zero OCX-token hits | (none) |
| resolve_target | OCI-generic | zero real OCX-token hits after excluding the "Image Index" (OCI-spec) false positive | (none) |
| sign | mixed | real: `crate::oci::index`(14, genuine — `Index::select`), `crate::env`(14), `crate::config`(2); "Claim" hits are cosign's `SimpleSigningClaim`, excluded | config:2, media_type:1, cli:9, env:3, file_structure:2, log:2, trust:2, utility:3 |
| simplesigning | OCI/cosign-spec-generic | `Claim` = `SimpleSigningClaim`, cosign's own wire term (verified: `pub struct SimpleSigningClaim`), not an OCX metadata concept | (none) |
| ssrf | OCI/transport-generic | the guard mechanism itself has zero OCX-specific module references; it is consumed *by* the OCX index elsewhere, but is domain-agnostic itself | cli:1 |
| transport_policy | OCI-generic | doc comment: "transport-agnostic"; the one raw "index" hit was prose, not code | (none) |
| verify | mixed, heavily | real, and the largest single OCX-specific entanglement in `oci`: `crate::trust`(67) — OCX's own trust-policy (signer allow/deny) grafted onto Sigstore verification; `crate::oci::index`(11, genuine); `crate::package`(14); "Claim" hits again are cosign's, excluded | config:1, media_type:1, auth:1, cli:4, file_structure:3, package:14, trust:67, utility:3 |

**Summary for the ADR:** of 23 sub-modules, roughly 12 are OCI/cosign-spec-generic with no real OCX coupling (`annotations`, `digest`, `endpoint`, `file_storage`, `manifest`, `manifest_builder`, `pinned_identifier`, `referrer`, `repository`, `resolve_target`, `simplesigning`, `ssrf`, `transport_policy` — 13 actually), `index` is unambiguously OCX-specific and huge (22,139 LOC), and the rest (`attest`, `client`, `copy`, `host_capabilities`, `identifier`, `platform`, `sign`, `verify`) are genuinely mixed — spec-generic mechanism wired to real, load-bearing OCX policy (`trust`, `config`, `env`, `project`, `announce`, lock/pin resolution). `verify`'s dependency on `trust` (67 references) and `client`'s breadth (16 out-edges, touching `announce`/`publisher`/`patch`/`managed_config`) are the two heaviest entanglements a crate boundary through `oci` would have to cross.

### The lib-internal `cli` module — traits, types, and real dependencies

**Traits defined in `cli`:** `ClassifyExitCode` (`classify.rs:44`), `ClassifyErrorKind` (`classify.rs:64`), `TreeItem` (`data_interface.rs:131`), `StyledInk` (`theme.rs:273`).
**Key concrete types defined in `cli`:** `ExitCode` (`exit_code.rs`), `DataInterface`/`Annotation`/`Column`/`Cell` (`data_interface.rs`), `Printer`/`Style`/`Line`/`Alignment` (`printer.rs`), `ProgressManager`/`Spinner`/`BytesBar`/`LogWriter` (`progress.rs`), `Theme`/`UnknownTheme` (`theme.rs`), `UserInterface` (`user_interface.rs`), `LogLevel`, `LogSettings`, `ErrorCategory`, `UsageError`/`MetadataResolutionError` (`error.rs`).

**`impl <trait> for <Type>` blocks OUTSIDE `cli/`, by top-level module:**

| Trait | Total impls outside `cli/` | Modules (impl count) |
|---|---|---|
| `ClassifyExitCode` | 56 | oci:11, package:7, utility:5, managed_config:4, config:4, package_manager:3, publisher:3, project:3, env:3, launch:1, activation:1, patch:1, error:1, forge:1, archive:1, announce:1, compression:1, record:1, setup:1, auth:1, file_structure:1, ci:1 |
| `ClassifyErrorKind` | 3 | oci:2 (verify, sign), publisher:1 (copy) |
| `TreeItem` | 0 | — |
| `StyledInk` | 0 outside `cli/` (2 total, both inside `cli/theme.rs`, for `Digest` and `Identifier`) | — |

`ClassifyExitCode` is implemented in 21 of the other 33 top-level modules — this is the single mechanism that most explains `cli`'s in-degree of 25. Per its own doc comment (`classify.rs:44-48`): "Classification is distributed across error types via the `ClassifyExitCode` trait. Each error type owns the mapping from its own variants to a process exit code, **placing the knowledge next to the type definition**" — so these 56 impls physically live in each subsystem's own error module, not in `cli/`.

**What `cli` itself imports from other modules — real dependency vs. dispatcher plumbing:** Checked every `cli/*.rs` file individually for `crate::X::` references outside `cli`. Result: **all 22 of `cli`'s out-edges to subsystem modules (oci, package, managed_config, utility, setup, config, announce, record, package_manager, publisher, project, launch, forge, patch, file_structure, env, archive, error, compression, ci, auth, activation — 166 total reference occurrences) are concentrated in one function**, `try_classify` in `classify.rs:104-145` (plus its own `#[cfg(test)]` block below it) — a `downcast_ref` ladder (`try_downcast!` macro) that must name every classifiable concrete error type once so `classify_error` can route it to the right `ClassifyExitCode::classify` impl. 155 of the 166 occurrences are in `classify.rs` alone.

The only real, non-dispatcher subsystem coupling found in `cli`'s presentation-layer files:
- `theme.rs` — `impl StyledInk for Digest` and `impl StyledInk for Identifier` (both `crate::oci` types), plus a match on `crate::package::metadata::visibility::Visibility` — genuine, but tiny (needs 3 concrete types for color-coding).
- `log_settings.rs`, `progress.rs`, `user_interface.rs` — each has one `crate::log::` reference (log-level/log-writer interop).
- `data_interface.rs` — one **doc-comment-only** intra-doc link (`[Printable]: crate::api::Printable`) pointing at a type that does not exist in `ocx_lib` at all (likely meant as `ocx_cli::api::Printable`); not a compiled dependency.

**Bottom line for the ADR:** `cli`'s reputation as the top hub (in-degree 25, out-degree 23) is real in edge-count terms but almost entirely attributable to two narrow, structurally simple mechanisms — the `ClassifyExitCode` trait (impls live with each error type) and the `try_classify` downcast ladder (one function, one file) — rather than to the presentation layer (`Printer`, `Theme`, `ProgressManager`, `DataInterface`, `UserInterface`) genuinely needing subsystem business logic. A crate boundary that kept `ExitCode`/`ClassifyExitCode`/`ClassifyErrorKind` in a small leaf crate (implemented by each subsystem crate for its own error types, exactly as today) would not need to touch the presentation types at all, except for the `theme.rs` → `Digest`/`Identifier`/`Visibility` link.

### `utility` — grab-bag or coherent module?

**Categories present, by sub-module:**

| Category | Sub-modules |
|---|---|
| Generic extension traits | `result_ext` (`ResultExt`), `serde_ext` (`SerdeExt`), `string_ext` (`StringExt`), `vec_ext` (`VecExt`) |
| Generic collection/string helpers | `list` (move-to-back dedup for separator-joined values), `path` (move-to-front dedup for `PATH`-style env values — **not** filesystem paths) |
| Generic async primitive | `singleflight` (watch-based dedup for concurrent async work) |
| Network/TLS helper | `tls` (shared TLS root-seeding for hand-rolled `reqwest::Client` builders) |
| Schema helper | `schema` (hand-built JSON Schema fragments `schemars` cannot infer) |
| Process boundary | `child_process` (generic boundary to child-process exit status) |
| Parsing helper | `boolean_string` (`BooleanString` enum) |
| Filesystem primitives (`fs/`, 12 files) | `bounded_read`, `dir_walker`, `drop_file`, `file_lock`, `locked_file`, `same_dir`, `same_filesystem`, `scoped_lock`, `symlink_walk`, `fs/path` (lexical containment checks — **a second, unrelated "path" module**, name-colliding with top-level `utility::path`) — these are genuinely generic |
| **Outlier: domain logic misfiled as utility** | `fs/assemble.rs` (3,819 LOC, the single largest file in `utility`) — "Layer assembly walker": mirrors a layer's `content/` tree into `packages/{P}/content/` via hardlinks/symlinks. This is package-installation business logic (the actual materialization step), not a generic filesystem helper — it has 29 real references to `crate::symlink` and 25 to `crate::error`, i.e. it is wired into OCX's own symlink/CAS model, not merely using `std::fs`. |

**Naming collision confirmed:** `utility/path.rs` ("Move-to-front deduplication… for `PATH`-style environment values") and `utility/fs/path.rs` ("Lexical path helpers shared across archive extraction, symlink validation, and the layer assembly walker" — pure containment-check logic, no filesystem access) are two unrelated modules sharing the name "path" one level apart.

**`utility`'s own out-edges** (`crate::X` from `utility`'s own files): `error`:57 (mostly from `assemble.rs` and the `fs/` error types), `symlink`:31 (almost entirely `assemble.rs`'s hardlink/symlink walker), `cli`:10 (all `ClassifyExitCode` impls for `utility`'s own error types — `SymlinkWalkError`, `PathEscapeError`, `EmptyOrAbsentError`, `SameFilesystemError`, `singleflight::Error`), `shell`:8 (all from `path.rs` alone — the PATH-dedup logic apparently serves shell export generation), `env`:7, `log`:6, `archive`:4, `prelude`:3, `file_structure`:3, `hardlink`:1, `config`:1.

**Bottom line for the ADR:** `utility` is a genuine grab-bag by composition (generic ext-traits + generic fs primitives + one async primitive + one TLS helper + one schema helper, no shared theme), **except** for `fs/assemble.rs`, which is domain logic (package/layer materialization) that happens to live under a "generic utility" module and carries real, non-generic coupling to `crate::symlink`. A crate split that tried to carve `utility` out as a pure leaf/foundation crate would have to relocate `assemble.rs` first — everything else in `utility` is a plausible foundation-crate candidate as-is.

### negative: (follow-up)

- Did not build the same responsibility/classification depth for `package_manager`, `config`, `file_structure`, or `package` beyond the sub-module LOC + one-line-responsibility table above (the per-hub table) — the two deep-dive classifications the task asked for by name were `oci` and `cli`/`utility`; the other four hubs' sub-module tables are included above but not further classified generic-vs-specific or dependency-audited.
- `oci`'s classification is a token-grep-plus-spot-check method, not an exhaustive line-by-line read of all 87k LOC — spot checks corrected two systematic false-positive classes (the "Image Index" OCI-spec sense of "index," and cosign's `SimpleSigningClaim`), but a third undetected false-positive class is possible for sub-modules not individually spot-checked (`referrer`, `host_capabilities`, `identifier` were trusted on the precise-token pass alone, not manually read end to end).
- Two extraneous, git-tracked non-Rust files exist at `crates/ocx_lib/src/package_manager/composer.rs.all` and `composer.rs.fns` (function/test-name dumps, last touched by commit `26926b28`) — excluded from all LOC/module counts in this report since they are not `.rs` source, but flagged as repo noise worth a cleanup pass independent of any crate-split decision.
- `theme.rs`'s `crate::package::` reference (`Visibility`) was confirmed to exist via `use`, but whether it backs a third `StyledInk`-style impl or only a `match` arm was not individually confirmed (only the two `Digest`/`Identifier` `impl StyledInk for` blocks were grepped and confirmed directly).

