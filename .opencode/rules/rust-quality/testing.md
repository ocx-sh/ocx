# Testing

How the OCX-family Rust crates are tested and what a test has to prove before
it counts. Covers placement, determinism, CLI black-box contracts, structural
guards, property testing, fuzzing, coverage, the flaky-test policy, the
filesystem strategy, and Cargo-feature test seams.

Contents: [Placement and Seams](#placement-and-seams) ·
[Determinism and Isolation](#determinism-and-isolation) ·
[CLI Contract](#cli-contract) · [Test Quality](#test-quality) ·
[Property, Fuzz, Mutation](#property-fuzz-mutation) ·
[Tooling and CI](#tooling-and-ci) · [Filesystem Strategy](#filesystem-strategy) ·
[Features, Seams and Fixtures](#features-seams-and-fixtures) ·
[What Agents Get Wrong](#what-agents-get-wrong-here) · [Sources](#sources)

Two layers:

- **The mechanism** — unit tests inline, contracts in `tests/`, no shared
  mutable environment, no network by default, seams instead of mocks — is
  general Rust practice.
- **The crate set** is a *pinned decision*: `rstest`, `assert_cmd` +
  `predicates`, `wiremock`, `proptest`, plus `cap-std` as a *production*
  dependency (TEST-28), `cargo-hack` as a CI tool (TEST-37), and `fail` at
  CONSIDER (TEST-32). Deliberately *not* adopted: `insta`, `trycmd`,
  `serial_test`, `quickcheck`, `cargo-semver-checks`, `loom`, `shuttle`,
  `turmoil`, `madsim`, `rsfs`, `vfs`, `mockall`, `faux`, `httpmock`. Adding one
  needs a filed bug it would have caught, not a preference.

The pyramid is inverted on purpose: the bulk is inline `#[cfg(test)]` units,
and every user-visible CLI contract — exit code, stderr wording, file written
— additionally gets a black-box test, because none of that is reachable from
a unit test.

## Placement and Seams

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-01 | Implementation-detail assertions go in an inline `#[cfg(test)] mod tests`; public-contract assertions go in `tests/`. Never widen an item's visibility so an integration test can reach it — the widening outlives the test. | A diff adding `pub(crate)`/`pub` alongside a new `tests/*.rs` file is a finding | MUST |
| TEST-02 | Shared integration-test helpers live at `tests/<name>/mod.rs`, never `tests/<name>.rs` — a bare helper file compiles as its own test binary and reports a spurious "running 0 tests". | `rg --files-without-match -g '**/tests/*.rs' -e '#\[test\]' -e '#\[tokio::test\]' .` empty | MUST |
| TEST-03 | Test-only methods go in one dedicated `#[cfg(test)] impl Foo {}` block before `mod tests`, or behind a `__testing` feature — never as scattered `#[cfg(test)]` attributes inside the production `impl`. Choosing the feature form binds TEST-34 and TEST-35. | A `#[cfg(test)]` directly on an `fn` inside a non-test `impl` is the violation | MUST |
| TEST-04 | Three or more variations of one assertion use `#[rstest]` `#[case(...)]`, not a `for` loop over an array inside one `#[test]` — a loop aborts at the first failure and reports one opaque name. | `rg -n --type rust 'for .* in \[' .` inside `#[cfg(test)]` regions | SHOULD |

A trait seam is earned by a **second production implementation** — the bar the
existing `OciTransport` / `OciAccess` / `CredentialStore` traits already meet.
HTTP clears it. **The filesystem does not**: there is no `FileSystem`/`Vfs`
trait here and none is coming (TEST-27). What replaces it is a real temp
directory (TEST-06), a sans-I/O split of decide-from-write (TEST-31),
`cap-std` where the path came from an attacker (TEST-28), and fail-point
injection where the failure mode is durability (TEST-32).

## Determinism and Isolation

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-05 | No test calls `std::env::set_var`/`remove_var`. Code under test takes configuration as a parameter; a test that must vary the environment does so via `Command::env()` on a child process — env mutation is `unsafe` because concurrent env access is an OS-unprotected data race. | `rg -n --type rust -e 'env::set_var' -e 'env::remove_var' .` returns no call sites | MUST |
| TEST-06 | A real filesystem under a per-test `tempfile::TempDir` is *the* strategy for filesystem-touching tests, not a fallback (TEST-27): bind the guard to a `let` and pass `dir.path()` — never a fixed path, never the inline `tempdir()?.path()` form, which drops the guard before the callee runs, and never a fake, which cannot reach `EXDEV`, `ENOSPC`, permission denial, case folding or Windows locking. | `rg -n --type rust 'tempdir\(\)[?.]*(unwrap\(\))?\.path\(\)' .` and `rg -n --type rust -e '"/tmp/' -e '"\./scratch' .` empty | MUST |
| TEST-07 | No test in the default profile opens a socket. HTTP behaviour is tested against `wiremock` on a local random port, or against captured production bytes committed with a `PROVENANCE.md`; anything needing the real network is `#[ignore = "network"]`. | Default test job passes with egress blocked | MUST |
| TEST-08 | Never assert path equality against a POSIX-absolute literal; canonicalize both sides with `dunce::canonicalize` first, and pair every `!contains(path)` assertion with a positive assertion on a known-present canonical path — `/tmp` is a symlink on macOS, Windows returns `\\?\` verbatim paths, and `base.join("/root/bin")` yields `C:/root/bin`, so a green negative proves nothing. | `rg -n --type rust 'assert.*"/[a-z]' .` — every hit is a candidate | MUST |
| TEST-09 | Sort any `HashMap`/`HashSet`-derived sequence before asserting on order, and never assert a wall-clock instant — inject a clock or assert a range. The default hasher is randomized per process. | `rg -n --type rust 'assert_eq!\(.*\.iter\(\)\.collect' .` | MUST |

## CLI Contract

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-10 | Every exit code the CLI can produce, and every stderr message a user is expected to act on, has an `assert_cmd` test asserting `.code(n)` and the stream contents *separately* — asserting on combined output cannot tell a code change from a wording change. | One test per `ExitCode` variant: `rg -n --type rust -g '**/tests/*.rs' '\.code\(' .` vs the variant count | MUST |

## Test Quality

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-11 | Extract a behavioural seam first. A source-text ("structural") guard is permitted only when the property under test is the *absence* of behaviour with nothing to call — and then it must strip comments before scanning, assert its needle matches at least once, scan each call site rather than compare counts, and be scoped to where the defect can occur, not to the function whose name matches the contract. | `rg -n --type rust -e 'include_str!' -e 'file!\(\)' .` — every hit in a test shows a non-zero match-count assertion | MUST |
| TEST-12 | Before claiming a check works, demonstrate it red on a controlled input. A mutation that fails to turn it red means another guard exists, not that the check is weak. Reporting "verified" without citing the red run is a Block-tier finding. | The PR body or commit message cites the failing invocation, not only the passing one | MUST |
| TEST-13 | Do not write a test whose only failure mode is "someone refactored the internals": if swapping in a different implementation with identical observable behaviour would break the test, it tests implementation. | Reviewer heuristic — a test naming a private helper in its own name is the tell | SHOULD |

Five ways a structural guard silently passes, all observed:

1. **The needle matched nothing.** Zero hits reports green. Assert a minimum match count.
2. **Comments counted as code.** A banned pattern quoted in a doc comment satisfies or defeats the scan. Strip comments before scanning.
3. **Counting instead of inspecting.** An occurrence total stays constant when one site moves and one is added. Scan each call site.
4. **Scoped by name, not by risk.** The guard reads the function whose name matches the contract; the defect lives in the three callers it never opens.
5. **Formatting drift.** `cargo fmt` rewraps the line, the needle stops matching, and nothing turns red.

## Property, Fuzz, Mutation

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-14 | Use `proptest`, never `quickcheck`, for new property tests. | `rg -n quickcheck -g Cargo.toml .` empty | MUST |
| TEST-15 | Every parser/serializer pair — manifest, lockfile, OCI reference, version requirement, digest encoding, path normalization — has a round-trip property that generates the *structured* value and asserts `parse(x.to_string()) == x`, so the test never reimplements the parser. | For each `impl FromStr` + `impl Display` pair, a `proptest!` block naming the type | MUST |
| TEST-16 | Commit `proptest-regressions/*.txt`; never gitignore that directory — it is the only record of the minimized failing case. | `git check-ignore -v proptest-regressions` exits non-zero | MUST |
| TEST-17 | Any subsystem with read-modify-write state across calls — lockfile writer, install/uninstall sequences, cache eviction — gets a `proptest-state-machine` test before it gets more single-shot unit tests. Sequential unit tests cannot discover order-dependent corruption. | A module doing multi-step on-disk mutation with no `prop_state_machine!` in the crate | SHOULD |
| TEST-18 | Hand-rolled format parsers (tar headers, OCI manifest JSON, reference strings) get a `cargo-fuzz` target whose `fuzz_target!` argument is a `#[derive(Arbitrary)]` struct, not `&[u8]` — byte soup dies at the first validity check. Targets **build** on every PR and **run** for hours nightly against a persisted corpus. | `cargo fuzz build` in the PR workflow; `rg -n max_total_time .github/workflows/` hits only the scheduled one | SHOULD |
| TEST-19 | Every bug found by a fuzzer or property test also gets a plain `#[test]` with the minimized input hardcoded — fuzz infrastructure gets skipped by a fast profile or deleted by a refactor; a literal test survives. | Each fixed-bug entry in `proptest-regressions/` and `fuzz/corpus/` has a named `#[test]` | MUST |
| TEST-20 | Run `cargo mutants --in-diff` on the PR diff and an unscoped pass nightly — mutation catches what coverage cannot see, such as a test asserting `Ok(_)` came back but never that the file was written. | `--in-diff` present in the PR job, absent in the scheduled job | CONSIDER |
| TEST-21 | Run `cargo miri test` on a schedule, scoped with `-p`/`--lib` to the pure parser/resolver slice — never at the workspace root, never as a PR gate. Miri cannot execute FFI, networking, or most filesystem syscalls. | The Miri step names a crate/target; a bare workspace-root `cargo miri test` is the violation | CONSIDER |

## Tooling and CI

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-22 | CI runs `cargo nextest run --profile ci` **and** a separate `cargo test --doc` step. Neither substitutes for the other — nextest does not execute doc tests at all. | `rg -n 'cargo test.*--doc' .github/ taskfiles/` hits alongside every `cargo nextest run` | MUST |
| TEST-23 | A flaky result is a failing state. Retries may exist in the CI profile, but a test reported flaky blocks the merge until fixed or quarantined with a tracking issue — "retried and eventually green" is not green, and an agent handed a retried-green suite never sees the bug. | CI parses `target/nextest/ci/junit.xml` for `flakyFailure` and fails on a non-zero count | MUST |
| TEST-24 | Coverage is measured with `cargo llvm-cov`, never `cargo-tarpaulin` (Linux-x86_64-only ptrace), and CI enforces a ratchet: line coverage may not fall below the last merged value. No aspirational fixed floor — it gets gamed. Report line/region only; `--branch` is still unstable. | `rg -n --hidden -g '!.git' tarpaulin .` empty; `rg -n 'fail-under-lines' .github/ taskfiles/` present | MUST |
| TEST-25 | Do not add `loom`, `shuttle`, `turmoil`, or `madsim` without a filed bug describing a concrete race the tool would have caught. Their target is hand-written lock-free code; correct use of `tokio::sync` is not it. | `rg -n -g Cargo.toml -e loom -e shuttle -e turmoil -e madsim .` empty or citing an issue | MUST |
| TEST-26 | A crate that is actually published marks new public structs/enums `#[non_exhaustive]` at first publish — retrofitting it is itself breaking — and gates release on `cargo semver-checks`. A clean run is *not* proof no breaking change occurred. | Applies only where `publish = false` is absent from `Cargo.toml` | SHOULD |

## Filesystem Strategy

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-27 | Do not introduce a `FileSystem`/`Vfs` trait to make filesystem code testable — cargo, rustup, sccache, jj and uv all test against a real temp directory instead, and an in-memory fake structurally cannot produce `EXDEV`, `ENOSPC`, permission denial, symlink-escape races, case folding or Windows locking. | `rg -n --type rust -e 'trait .*FileSystem' -e 'trait .*Vfs' .` empty, or the one hit names its second production implementor in the same PR | MUST NOT |
| TEST-28 | Any path derived from attacker-controlled data — OCI layer paths, tar entry names, manifest-supplied filenames — is opened through a `cap_std::fs::Dir` scoped to the destination root, never `std::fs` on a string-joined `Path`: `Dir::open` rejects `../` and symlink escapes at open time instead of leaving a check-then-use TOCTOU window. | In the materializer/cache-write modules, every `Path::join`/`File::open` fed from a manifest or tar entry is a violation; `cargo tree -i cap-std` confirms adoption | MUST |
| TEST-29 | Every `fs::rename`/`tokio::fs::rename` that moves an artifact into place has a documented same-filesystem invariant or an `ErrorKind::CrossesDevices` fallback (copy + fsync + delete), tested across two real mounts and never against a canned `Err` — a cache directory on a different device from `$TMPDIR` is the ordinary case. | `rg -n --type rust 'fs::rename' .` — each hit shows a fallback branch or an invariant comment | MUST |
| TEST-30 | A filesystem-touching change is not green until the filesystem job has passed on Linux, macOS *and* Windows, with the symlink/junction containment tests actually executing on Windows rather than compiled out by `#[cfg(unix)]`. | The job matrix lists `windows-latest` and `macos-latest`; `rg -n -B2 --type rust -e 'fn .*symlink' -e 'fn .*escape' -e 'fn .*junction' .` shows no `#[cfg(unix)]` above any hit | MUST |
| TEST-31 | A function must not both decide and touch the disk: parsing, validation and policy take already-read bytes or already-listed paths, and `std::fs`/`tokio::fs` calls live in thin I/O-only wrappers — the only lever with a measured effect on suite time. | Reviewer heuristic on touched functions — a body holding both a parse/validate branch and an `fs` call is a split candidate | SHOULD |
| TEST-32 | Durability paths (lockfile commit, cache manifest write) that need a partial-failure test get `fail::fail_point!` behind a `failpoints` feature, driven by `FailScenario`/`fail::cfg` — not a fake filesystem, and never by setting `FAILPOINTS` in-process, which is `set_var` and banned by TEST-05. | `rg -n --type rust 'fail_point!' .` appears only in persistence/cache-write modules; `failpoints` absent from the shipped binary's resolved feature set | CONSIDER |
| TEST-33 | Do not add `rsfs` — no release since 2017, error injection removed. If an in-memory filesystem is used at all, the test name or an adjacent comment states that it covers branching logic only; no PR claims a `MemoryFS` test covers permissions, fsync, `EXDEV`, or symlink escape. | `cargo tree -i rsfs` returns nothing; every fake-filesystem test carries the scope comment | MUST NOT |

## Features, Seams and Fixtures

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TEST-34 | A helper that an integration test or a sibling crate must reach is gated `#[cfg(any(test, feature = "test-util"))]` — never `cfg(test)` alone, and never "fixed" by widening the item to `pub`. `cfg(test)` is set on one compilation unit, so a `tests/*.rs` binary links the library as a plain rlib and the item does not exist for it. | `rg -n --type rust -A1 '#\[cfg\(test\)\]' .` — a hit whose next line is not `mod tests` and that gates an item named from `tests/*.rs` or another crate is the violation | MUST |
| TEST-35 | A `test-util`/`__testing` feature is enabled only through a `[dev-dependencies]` edge — never `[dependencies]`, including forwarding entries like `__testing = ["lib/__testing"]` sitting on a normal dependency: resolver v2/v3 protects the dev edge only, and a normal edge is graph-wide unification in every resolver version. | `rg -n -g Cargo.toml -A3 '^\[dependencies\]' .` names no `test-util`, `__testing` or `testsupport` edge; `cargo tree -e normal -p <shipped binary>` does not reach the feature | MUST |
| TEST-36 | A virtual workspace (a `[workspace]` table with no `[package]`) sets `resolver = "2"` or `"3"` explicitly — cargo does not infer it from member editions, and the silent v1 fallback reopens exactly the leak TEST-35 depends on being closed. | `rg -A2 '^\[workspace\]' Cargo.toml` shows `resolver = "2"` or `"3"` at every virtual-workspace root | MUST |
| TEST-37 | `--all-features` is not the feature-combination gate — it forces every mutually exclusive pairing in the graph on at once, and turns on the test-only features too. CI runs `cargo hack check --each-feature` plus a pruned `--feature-powerset` (`--depth`, `--group-features`, `--exclude-features`), and any known-exclusive pair carries a `compile_error!` guard with a job asserting the combination *fails*. | `rg -n 'all-features' .github/ taskfiles/` — each hit removed, or paired with a comment establishing there is no exclusive pair and no test-only feature in the graph | MUST NOT |
| TEST-38 | Tier tests declaratively: `required-features` on any `[[test]]`/`[[bin]]`/`[[example]]` target needing network, a real registry or another external resource, plus nextest filtersets and test-groups for selection and shared-resource serialization. A `std::env::var(...).is_ok()` early-return is not a tier — it is invisible to `cargo test --list` and to every selector. | `cargo nextest list` with no extra features lists no test that immediately errors for lack of network; no test body contains an env-var tier gate | MUST |
| TEST-39 | Default to a hand-written fake behind a narrow trait; reach for `mockall` only when the trait already exists for a production reason and the fake would be pure boilerplate. Never expose a mock to another crate, and never set an expectation on a static method — those are process-global and unsynchronized, so they race nextest's per-test parallelism. | `rg -n --type rust -e automock -e 'expect_' .` — a `Mock*` named from `tests/*.rs` must be behind a feature (TEST-34); any static-method expectation is a finding | MUST |
| TEST-40 | Once a second crate needs the same fixtures or fakes, extract a `-testsupport` crate consumed only via `[dev-dependencies]` rather than growing a shared `test-util` feature — a crate absent from every `[dependencies]` section is structurally absent from the shipped graph. A single-crate workspace keeps the feature. | `cargo tree -e normal -p <shipped binary>` lists no `-testsupport` crate | SHOULD |
| TEST-41 | Locate fixtures and golden files with `concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/…")`, never a bare relative path, and give every golden file an explicit regeneration command — cwd-equals-package-root is a `cargo test` launcher guarantee that neither a directly invoked binary nor rustdoc (workspace root) keeps. | `rg -n --type rust -e '"\./' -e '"tests/' .` — every hit without `CARGO_MANIFEST_DIR` is a candidate | MUST |

## What Agents Get Wrong Here

1. **Claiming green without ever seeing red.** A guard scoped to the wrong function, a needle broken by `cargo fmt`, a `#[cfg(unix)]`-only escape test for a Windows property — all look identical to passing. Highest frequency, highest consequence, because the security properties are exactly the ones with no behavioural seam.
2. **Reaching for a `FileSystem` trait and a fake the moment "make this testable" is asked.** The textbook OOP answer, it makes the diff look thorough, and no comparable Rust project does it. Tell: a new trait whose only two implementors are `std::fs`-backed and test-only. Mechanical reject: TEST-27.
3. **`cfg(test)` on a helper another compilation unit needs, then "fixing" the unresolved item by making it `pub`.** Compiles, ships test-only code — in iroh's case, code that disabled TLS verification. Putting the feature-enabling edge in `[dependencies]` is the second half of the same mistake. Both are silent: the build succeeds.
4. **Owned `TempDir` dropped before use.** `Command::new(bin).current_dir(tempfile::tempdir()?.path())` compiles, deletes the directory at the end of the statement, then fails ENOENT intermittently.
5. **Writing the disk-full test against a fake and reporting it done.** `MemoryFS` has no error-injection hook, so a green `ENOSPC` test against a fake is fabricated coverage — same for permissions, fsync, and symlink escape.
6. **`rename()`-into-place with no `EXDEV` fallback.** Requires knowing the cache and `$TMPDIR` can sit on different mounts — an OS fact invisible in the diff, which is why TEST-29 is a grep and not a judgement call.
7. **Reaching for `--all-features` because it sounds maximal**, forcing on every mutually exclusive pairing and, here, the test-only escape hatch.
8. **Assuming `cargo nextest run` covers doc tests.** Writes a nextest-only CI file, sometimes with a comment asserting it runs everything.
9. **Hand-rolled `for`-loop test tables.** Reflexive, compiles, hides every case after the first failure behind one opaque name.
10. **`std::env::set_var` as a bare call**, or wrapped in a blanket `unsafe {}` to make it compile — which satisfies the compiler and does nothing about the race.
11. **A "property" that reimplements the function under test as its own oracle.** `prop_assert_eq!(parse(s), inline_copy_of_parse(s))` can never fail. Watch for an oracle with the same branches in the same order as the code.
12. **`tests/common.rs` instead of `tests/common/mod.rs`.** Half-works; the phantom "running 0 tests" binary reads as normal.
13. **Fuzz targets taking `&[u8]` with hand-written offset math.** Looks structure-aware, rejects almost every input before reaching parser logic.
14. **Adopting a heavyweight verification tool speculatively** — `loom`, `madsim`, OSS-Fuzz — because "concurrent code should be tested this way", with no bug motivating it.
15. **Treating a clean `cargo semver-checks` as proof of no breaking change**, missing auto-trait loss, bound tightening, and RPIT lifetime capture.

## Sources

- [Rust Book §11.3 — Test Organization](https://doc.rust-lang.org/book/ch11-03-test-organization.html) — the unit/integration split and the `tests/common/mod.rs` rule
- [`std::env::set_var`](https://doc.rust-lang.org/std/env/fn.set_var.html) — the safety contract that makes env mutation in tests a data race
- [nexte.st — Running tests](https://nexte.st/docs/running/) and [nextest retries](https://github.com/nextest-rs/nextest/blob/main/site/src/docs/features/retries.md) — process-per-test, the doc-test gap, JUnit `<flakyFailure>`
- [assert_cmd](https://github.com/assert-rs/assert_cmd) — `Command::cargo_bin`, separate exit-code and stream assertions
- [tempfile](https://docs.rs/tempfile/latest/tempfile/) — drop-based cleanup and the owned-guard pitfall
- [matklad — How to Test](https://matklad.github.io/2021/05/31/how-to-test.html) — the Neural Network Test
- [proptest getting-started](https://proptest-rs.github.io/proptest/proptest/getting-started.html) and [vs quickcheck](https://proptest-rs.github.io/proptest/proptest/vs-quickcheck.html) — round-trip patterns, strategy values vs type generators
- [rust-fuzz book — structure-aware fuzzing](https://rust-fuzz.github.io/book/cargo-fuzz/structure-aware-fuzzing.html) and [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov) — `#[derive(Arbitrary)]` targets; cross-platform coverage and `--branch` status
- [uv-fs](https://github.com/astral-sh/uv/blob/main/crates/uv-fs/src/lib.rs), [cap-std](https://github.com/bytecodealliance/cap-std), [std::fs::rename](https://doc.rust-lang.org/std/fs/fn.rename.html), [fail-rs](https://github.com/tikv/fail-rs) — free functions over `std::fs` with no trait; capability-scoped `Dir`; cross-mount failure; fault injection at the real call site
- [Cargo Book — Resolver](https://doc.rust-lang.org/cargo/reference/resolver.html), [Features](https://doc.rust-lang.org/cargo/reference/features.html), [Cargo Targets](https://doc.rust-lang.org/cargo/reference/cargo-targets.html) — dev-edge feature unification, the virtual-workspace resolver, additive-only discipline, `required-features` scope
- [Rust Reference — conditional compilation](https://doc.rust-lang.org/reference/conditional-compilation.html) and [cargo-hack](https://github.com/taiki-e/cargo-hack) — why `cfg(test)` cannot cross a compilation unit; `--each-feature` in place of `--all-features`
