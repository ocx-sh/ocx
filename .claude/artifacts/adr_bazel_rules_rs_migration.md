# ADR: Move the Bazel Rust graph to rules_rs and hermetic LLVM, and cross-build from Linux

## Metadata

**Status:** Accepted
**Date:** 2026-10-21
**Deciders:** Michael Herwig
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
- [ ] OR deviation justified in Rationale section
**Domain Tags:** devops | infrastructure
**Supersedes:** part of [`adr_bazel_build_adoption.md`](./adr_bazel_build_adoption.md) (the rules_rust + crate_universe toolchain choice)
**Superseded By:** N/A

## Context

[`adr_bazel_build_adoption.md`](./adr_bazel_build_adoption.md) put Bazel under the Taskfile on `rules_rust` 0.74 with `crate_universe`. That setup carried three standing costs:

- A gitignored `Cargo.bazel.lock.json` that every fresh checkout had to regenerate (`task bazel:bootstrap`, `CARGO_BAZEL_REPIN=1`), plus a step that hid the generated `BUILD.bazel` files `crate_universe` wrote into the three `external/` forks.
- A host C++ toolchain: Rust link actions needed the system `gcc` and an unversioned `libstdc++.so`, so `bazel:doctor` carried a libstdc++ check.
- Native CI legs. macOS and Windows each compiled and tested the workspace themselves with `cargo nextest`, a second build of what Linux had already built.

Release stays on cargo-dist (MSVC zips, auditable, CycloneDX, `scan-binaries`). Only the CI build and test path changes. ocx-mirror is out of scope: it consumes ocx crates by path (rules_rs issues #181/#273) and its MSVC release needs its own rules_rust Windows patches.

## Decision Drivers

- Delete the repin / lockfile / BUILD-hiding machinery.
- A hermetic C toolchain, so a host's `gcc` and `libstdc++` stop being inputs.
- One Linux job builds every target; native runners only execute.
- Hold every existing gate: clippy and rustdoc ratchets, doctests by exit status, `SUITE_FLOOR`.

## Industry Context & Research

**Research artifact:** the spike ran on the local branch `spike/rules-rs` (HEAD 47f2a2bb5), which was never pushed and left no evidence file in the tree. The summary below is the surviving record.
**Trending approaches:** [rules_rs](https://github.com/hermeticbuild/rules_rs) resolves crates from `Cargo.lock` inside Bazel (no generated lockfile) and bundles a [hermeticbuild fork of rules_rust](https://github.com/hermeticbuild/rules_rust); [hermetic-llvm](https://github.com/hermeticbuild/hermetic-llvm) (BCR module `llvm`) is a self-contained cross C/C++ toolchain that rules_rs already depends on.
**Key insight:** the rule names (`rust_library`, `rust_test`, `rust_doc_test`, the clippy aspect) survive, so the BUILD files and the lint plumbing port mechanically.

## Considered Options

### Option 1: Stay on rules_rust 0.74 + crate_universe

| Pros | Cons |
|------|------|
| No change | Repin machinery and host C++ toolchain stay; no cross-build story; native legs keep compiling |

### Option 2: rules_rs 0.1.3 + `llvm` 0.8.25 (chosen)

| Pros | Cons |
|------|------|
| No lockfile beyond `MODULE.bazel.lock`; hermetic linker; cross-compiles every triple | Pre-1.0, weekly releases (exact pins, bump rules_rs and llvm together); doctests gated by exit status, not counted; four Bazel-only crate features |

### Option 3: rules_rs crate resolution with the system C toolchain

| Pros | Cons |
|------|------|
| Smaller step | Keeps the libstdc++ prerequisite; no cross-build. Held as the fallback if criterion 1 had failed |

## Decision Outcome

**Chosen Option:** Option 2. `rules_rs` 0.1.3 and `llvm` 0.8.25, exact pins. The direct `rules_rust` dependency is dropped in favour of rules_rs's bundled fork, so there is one copy.

### Spike evidence

Branch `spike/rules-rs`, WSL2 x86_64, Bazel 9.2.0, Rust 1.95.0. Baseline: `3485207c1` (rules_rust 0.74 + crate_universe).

| # | Criterion | Result | Numbers |
|---|---|---|---|
| 1 | C-heavy crates and `ocx_shim` link with `@llvm` | Pass (local host) | `ocx` links only libc/libm/libpthread/libdl/librt; max GLIBC symbol 2.28; no libstdc++/libgcc_s (#137 absent). `ubuntu-latest` not run in the spike; CI has run it since |
| 2 | `bazel mod deps --lockfile_mode=error` | Pass | warm and cold (fresh `--output_base`) exit 0 |
| 3 | 24 members + 3 forks resolve, no BUILD-hiding | Pass | `bazel build //crates/...`: 99 targets, exit 0; no `Cargo.bazel.lock.json`; no `external/*/BUILD.bazel` |
| 4 | `package =` renames | Pass | `rustls-pki-types`, `sha2_oid` compile as-is |
| 5 | Feature parity vs `cargo metadata` | Partial | 388 of 392 third-party crates identical; 4 Bazel-only extras (below) |
| 6 | Clippy / rustdoc ratchets | Pass | clippy 169 diagnostics over 41 keys, identical to baseline; rustdoc 63 over 53; seeded warning red (+1), reverted green, for both |
| 7 | Tests and doctests | Pass for unit tests (partial in the spike) | 65/65 test targets, 9350 cases in 43 targets, identical to baseline; 22 doctest targets pass, 39 doctests with the stdout patch the spike then used (since dropped, Finding A); seeded `assert_eq!(1, 2)` goes red. Acceptance was not run in the spike; it runs in CI (below) |
| 8 | Cold build <= 1.3x | Pass | 176 s (3221 actions) against 218 s (2066 actions, includes the repin) = 0.81x; fresh output base on a warm disk cache 20 s, 2066/2066 hits |

### Feature parity: 388/392

The four Bazel-only extras are `getrandom 0.4.3` (`std`, `sys_rng`), `portable-atomic` (`require-cas`), `simdutf8` (`std`) and `zip` (`flate2`). **Accepted.** None changes which crates are selected, and the unit tests are green. The baseline already had the `getrandom std`, `simdutf8 std` and `tokio test-util` (#261 dev-feature leak) extras. Five more raw `cargo metadata` differences are non-Linux `cfg` features and host-only graphs reported by cargo's side. `build:drift` keeps the comparison honest.

### Findings

- **A. Doctests print nothing, and that is accepted.** `rust_doc_test` in the fork runs the doctest as a build action and passes `--discard-stdout-on-success`, so a passing target writes an empty `test.log` and no per-doctest count can be read. The spike patched the flag out; the patch is dropped. hermeticbuild/rules_rust added the flag on purpose (completion-order output changed the replay cache key), so removing it upstream would be rejected and carrying the patch means re-checking it on every rules_rs bump. **The rule:** the 22 `rust_doc_test` targets are gated by exit status alone. `bazel_test_floor.py` reds on a doctest target that failed or is missing from the run, `bazel:build:drift` reds on a library with no doctest target or one whose `crate_features` differ, and `crates/TEST_TARGET_MAP.toml` holds no doctest row. A doctest turned into an `ignore` fence or deleted is no longer a red; review catches it.
- **B. `all_crate_deps` already returns workspace path deps** as `//crates/X`; a hand-written duplicate fails analysis ("Label ... is duplicated"). It also has no `proc_macro*` arguments.
- **C. Port-script hazard.** A scripted BUILD port stripped `//crates/ocx_test_support:data` from `data` lists and broke four tests; only bare or self-named labels may be removed.
- **D. Fresh worktrees** need `task website:schema:default` before `bazel build //...` (the `//test:test_schema` input), unrelated to rules_rs.
- **E. BUILD `version =` attributes** are stale against the workspace version (pre-existing).
- **F. No lockfile or repin** is needed (the baseline repin took 26 s).
- **G. rules_rs 0.1.3 depends on `llvm` 0.8.18**; the root pin 0.8.25 resolves without conflict.

### Cross-build matrix

Eight triples build green from one Linux host: `{x86_64,aarch64}-unknown-linux-{gnu,musl}`, `{x86_64,aarch64}-apple-darwin`, `{x86_64,aarch64}-pc-windows-gnullvm` (`task cross:build TARGET=<triple>`; `TARGETS` in `taskfiles/cross.taskfile.yml` equals `platform_triples` in `MODULE.bazel`).

- `-c opt` is required: a fastbuild (opt-level 0) Windows binary overflows the 1 MiB main-thread stack at startup.
- macOS needs an `osx.frameworks` tag. The tag replaces the llvm module's default sysroot set, so the defaults are restated and `AppKit`, `IOKit` and `OpenDirectory` are added for the dependency graph.

### CI shape

Superseded by the Amendment 2026-10-24 below. The list records the shape as decided on 2026-10-21.

- **One Linux `build` job** runs the Bazel unit tests (skipped on a push to `main`, where `verify-basic.yml` just ran them), then cross-builds every triple and packs the macOS/Windows unit-test bundles and the acceptance `test-bin` layouts.
- **macOS and Windows download and execute only**: no Bazel, cargo or Rust toolchain on the runner. Unit tests run from the bundle (`task cross:test:run`, `scripts/test_bundle.py`); acceptance runs the smoke tier on native `zot` (`OCX_TEST_NATIVE_SERVICES=1`).
- **Linux acceptance** runs in 4 shards (`task test:accept:native SHARD_COUNT=4 SHARD_INDEX=i`, `i` 0-based) on native `zot`; an `acceptance-merge` job applies `SUITE_FLOOR`, the ceilings and the conformance floor over all shards' JUnit files together.
- **Release is unchanged**: cargo-dist builds MSVC zips.

### Consequences

**Positive:**
- `bazel:bootstrap`, `Cargo.bazel.lock.json`, `CARGO_BAZEL_REPIN`, the BUILD-hiding step and the doctor's libstdc++ check are gone.
- One cross-compiling Linux job replaces three native build legs.
- Acceptance shards and the merged floor cut wall-clock time without weakening the floor.

**Negative:**
- **gnullvm is the CI-tested Windows target while release ships MSVC.** No gate executes the shipped artifact's linker output or CRT. Recorded gap; moving the release to the Bazel cross-build is a separate decision.
- `rust:test:bazel-skipped` testcases (those `--skip`ped in `rust_test` targets) no longer run on macOS or Windows; the native legs execute only the Linux-built bundle, so that step is their one run, on Linux.
- Doctests stay Linux-only (build actions).
- macOS and Windows run the smoke tier on native zot, not the full acceptance suite (the `registry:2` mirror has no native build). Full-suite parity there is a follow-up.

**Risks:**
- rules_rs is pre-1.0 with roughly weekly releases. Exact pins; Renovate bumps `rules_rs` and `llvm` together; `build:drift` guards feature drift.

## Question -> research -> decision: "acceptance runs twice"

**Question.** The goal reported that acceptance tests appear to run twice across `verify-deep.yml` and `verify-basic.yml`.

**Research.** Run [39456163958](https://github.com/ocx-sh/ocx/actions/runs/39456163958): `Acceptance (Linux)` executed 179/179 targets once; there is no in-job double run. The duplication was elsewhere: (a) the acceptance job rebuilt `ocx` and the schema through Bazel (385 s) after waiting on the 3-OS build matrix, and (b) Linux unit tests ran in both `verify-basic` `smoke` and `verify-deep` `build` on every push to `main`.

**Decision.** The acceptance shards consume the Linux jobs' artifacts: `ocx` and its acceptance binaries come from the per-triple `cross` matrix (8 parallel legs; one serial job took 2h+ cold), the schema from `build`. Deep's `build` skips `bazel:test:unit` on `push` events.

## Amendment 2026-10-24 (CI shape after the PR 637 round-2 review)

The decision stands: rules_rs, hermetic llvm and a Linux cross-build. Only the CI shape above and the "acceptance runs twice" decision are replaced.

| # | Change |
|---|---|
| AM-1 | CI has two tiers. Basic runs on every pull request, push, merge group and dispatch: `verify-basic.yml` plus `basic-repo.yml`. Deep runs nightly, on dispatch and on the `run-deep` label: `verify-deep.yml` plus `deep-repo.yml`. |
| AM-2 | The full Linux acceptance suite is a basic job. `basic-repo.yml` calls `acceptance.yml`, which rebuilds `ocx` from the warm cache, runs the suite in shards (`--shard-count N --shard-index I`, 0-based) with Docker and Sigstore, and judges the floors once over all shards. Linux unit tests run as Bazel targets in `ci:test`. |
| AM-3 | Cross-builds are a deep job. `deep-repo.yml` calls `cross.yml` for every triple. macOS and Windows then download the artifacts and run the unit tests (`unit-native`) and the full acceptance suite (`acceptance-native`, four shards) on native zot without Docker. Each OS is judged against its own `test/limits/<os>`. This replaces the single Linux `build` job and the smoke-tier-only native acceptance. |
| AM-4 | Each pytest session starts its own zot registries on free ports, so `OCX_TEST_NATIVE_SERVICES` is gone. See `adr_test_speed_tiers.md` AM-13. |
| AM-5 | Every Bazel lane writes the remote cache with `BAZEL_CACHE_WRITE_AUTH`. The separate CI read credential is retired. A developer's read header in `~/.bazelrc` stays. |
| AM-6 | `release-readiness.yml` (dispatch only) builds the cargo-dist bytes at the exact SHA and runs the full suite on them per platform against `test/limits/<os>-release`. `task release:tag` refuses without a green run. Release still ships MSVC zips, so the gnullvm gap stands. |
| AM-7 | The rules_rust doctest-stdout patch is deleted. The doctest floor rests on the exit code alone. |

## Validation

- `task verify` and `verify-deep.yml` (build, unit-native, sharded acceptance with merged floor, acceptance-native) green on the migration branch.
- Each rewritten gate (`bazel_build_drift.py`, `bazel_pin_check.py`, `bazel_tag_guard.py`, `bazel_gate_proofs.py`, `bazel_doctor.py`) is shown red and green in `scripts/bazel_gate_proofs.py`.
- `scripts/tests/test_accept_shard.py` and `test_accept_floor.py` show the shard selector and merged floor red and green.

## Links

- [`adr_bazel_build_adoption.md`](./adr_bazel_build_adoption.md)
- [`.claude/rules/subsystem-ci.md`](../rules/subsystem-ci.md) § Windows & macOS Coverage
- [`.claude/rules/subsystem-taskfiles.md`](../rules/subsystem-taskfiles.md)

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-10-21 | Michael Herwig | Accepted |
| 2026-10-24 | Michael Herwig | Amendment: two CI tiers, Linux acceptance in basic, cross-builds and native legs in deep, write credential on every lane, readiness on the exact SHA; spike evidence pointer dropped |
