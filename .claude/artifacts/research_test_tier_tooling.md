# Research: Test-tier tooling — in-process CLI tests, Bazel test selection, provenance-free test builds

## Metadata

**Date:** 2026-09-22
**Domain:** testing
**Triggered by:** hex-architect ADR on test-suite speed tiers (axis: technology/tooling)
**Expires:** 2027-03-22

## Direct Answer

1. **In-process CLI testing.** A library entry point with the shape `run(argv, env, out, err) -> ExitCode` is documented practice ([rust-cli book](https://rust-cli.github.io/book/tutorial/testing.html)). No surveyed flagship CLI (cargo, uv, ruff, rustup) runs its *whole* behaviour suite in-process, though. Each pairs the library seam with subprocess or snapshot tooling (assert_cmd, insta_cmd, snapbox/trycmd) to avoid global-state hazards. Rust 2024 made `std::env::set_var`/`remove_var` `unsafe` because changing the process environment races other threads ([rust#124636](https://github.com/rust-lang/rust/pull/124636)).
2. **Bazel test selection and caching.** `rust_test` gets sharding, `size`→timeout and `--cache_test_results` from Bazel core; that is the mechanism behind `bazel:test:accept` executing 0 of 181 on an unchanged tree. bazel-diff and target-determinator precompute the affected targets *before* Bazel runs, which is a CI-sharding concern layered on top rather than a replacement.
3. **Test builds without provenance.** vergen `VERGEN_IDEMPOTENT` plus `SOURCE_DATE_EPOCH`, and Bazel's `--workspace_status_command` with its stable/volatile split under `--stamp`, express the same pattern: provenance stays out of the cache key for dev/test builds and is stamped in for releases. No alternative pattern surfaced, so treat divergence from it as a bug, not a design choice.
4. **Snapshot maintenance at scale.** insta has `cargo insta review` with bulk shortcuts and `--unreferenced` cleanup. Large, unscoped snapshot diffs get rubber-stamped; the documented incident is a regression that shipped inside a 300-line snapshot accept ([Kent C. Dodds](https://kentcdodds.com/blog/effective-snapshot-testing)). The mitigation is small snapshots with one concern each, plus review discipline; tooling alone does not provide it.

## Technology Landscape

### Trending (gaining momentum)

| Tool/Pattern | Adoption Signal | Key Benefit | Relevance |
|---|---|---|---|
| Declarative CLI scenarios in TOML (uv, [PR #7844](https://github.com/astral-sh/uv/pull/7844)) | uv | Authoring cases separately from Rust code | Cheap case authoring for agents after a port |
| bazel-diff / target-determinator | BazelCon 2025 talk; Tinder "saved years of compute" ([talk](https://www.youtube.com/watch?v=rCFc3tFcVVE)) | Selects affected targets before Bazel runs | Only needed once in-repo caching falls short |
| insta bulk review UX | insta.rs | Scales snapshot review | A bulk accept is also the failure mode |

### Established (proven, widely accepted)

| Tool/Pattern | Status | Notes |
|---|---|---|
| assert_cmd + predicates | Standard | Spawns the real binary ([docs](https://docs.rs/assert_cmd)) |
| trycmd / snapbox | Standard | rustup uses snapbox behind a `test` feature ([Cargo.toml](https://github.com/rust-lang/rustup/blob/main/Cargo.toml)) |
| insta / insta_cmd | Standard | ruff (nextest + insta), uv (`uv_snapshot!` over `TestContext`) |
| Bazel `size`/`timeout`/`shard_count`/`--cache_test_results` | Standard | [test encyclopedia](https://bazel.build/reference/test-encyclopedia), [user manual](https://bazel.build/docs/user-manual) |
| `--workspace_status_command` + `--stamp` | Standard | `STABLE_` keys invalidate the cache; volatile keys never do |
| vergen `VERGEN_IDEMPOTENT` + `SOURCE_DATE_EPOCH` | Standard | `SOURCE_DATE_EPOCH` wins when both are set ([docs](https://docs.rs/vergen/latest/vergen/)) |
| cargo `tests/testsuite` + `cargo-test-support` | Standard | Cargo **spawns itself per test** because its global state (env, cwd, registry cache) is unsafe to re-enter in-process ([docs](https://doc.rust-lang.org/beta/nightly-rustc/cargo_test_support/)). This is the strongest "against in-process" precedent, from the tool closest in shape to ocx. |

### Emerging (early but promising)

| Tool/Pattern | Signal | Worth Watching Because |
|---|---|---|
| A library `run(argv, env, out, err)` driven in-process | rust-cli book; small projects | No flagship CLI runs its whole suite this way |
| RAII test helpers that make env changes safe | Driven by Rust 2024 `unsafe` `set_var` ([edition guide](https://doc.rust-lang.org/edition-guide/rust-2024/newly-unsafe-functions.html)) | Still community crates only |

## Findings

### In-process testing: for and against

- **For:** logic in `lib.rs` with an injected `impl Write`, and `main` returning `ExitCode` instead of calling `process::exit`, lets tests inspect the result in-process ([ExitCode](https://doc.rust-lang.org/beta/std/process/struct.ExitCode.html)). Pure logic (parsing, resolution, formatting, exit classification) gets faster with no hazard, and no source disputes that.
- **Against:**
  - Changing the environment after threads start is a data race (Rust 2024 made it `unsafe`).
  - A Tokio runtime leaves process-global state (signals, `tokio::process`) that cannot be reset ([okou#35505](https://github.com/vm0-ai/okou/issues/35505)).
  - uv still fights ambient environment leakage in its subprocess tests ([uv#9873](https://github.com/astral-sh/uv/issues/9873)).
- **The mitigation that recurs:** never read ambient environment inside the library entry point; pass argv, env and writers in as explicit owned values.

### Bazel

- rules_rust tracks Bazel ≥ 7.4.1 ([releases](https://github.com/bazelbuild/rules_rust/releases)). `--cache_test_results=auto` re-runs a test only on a change to its dependencies or flags, or after a failure.
- bazel-diff hashes the graph (rule plus attributes plus file content), which is fast but can miss affected targets. target-determinator works through `cquery`, is correctness-first, and its cache is not portable across machines ([README](https://github.com/bazel-contrib/target-determinator)).
- **Implication for OCX (orchestrator note):** a `rust_test` that *spawns the binary* (assert_cmd) depends on the binary's bytes. Once the binary is in the graph, that test is invalidated by every Rust change, just like the acceptance `sh_test`s. Selecting by crate through `rdeps` stays precise only for tests of the in-process library.

### Provenance

- vergen idempotent mode turns volatile values into constants; release 10.x also falls back to `.cargo_vcs_info.json` when `.git` is absent.
- Bazel volatile status is refreshed on every run but never invalidates the cache; `STABLE_` keys do.
- Composition: dev/test builds unstamped or idempotent, release builds stamped.

### Snapshots

- `cargo insta review` works per hunk and offers bulk accept/reject/skip; `--unreferenced=auto` rejects orphaned snapshots in CI and deletes them locally ([insta.rs](https://insta.rs/)).
- Mitigation: one behaviour per snapshot, a cap on snapshot size, and never landing a migration wave as a single bulk-accept commit.

## Recommendation (researcher's opinion, advisory)

- Do not replace the acceptance tier wholesale with in-process tests. Cargo, the closest analogue, isolates with subprocesses, and ocx carries even more state (registry containers).
- Do move pure logic down (resolution, parsing, exit classification, flag validation) behind a `run(argv, env, out, err)` seam with explicit owned inputs.
- Bazel's existing `--cache_test_results` already does most of the speed-tier work; defer bazel-diff and target-determinator.
- Split provenance: idempotent / `SOURCE_DATE_EPOCH` for test builds, stamped for release.
- Budget for review discipline if cases move to snapshots.

## Sources

rust-cli book · assert_cmd · snapbox · trycmd · insta.rs · cargo tests/testsuite · cargo_test_support · rustup Cargo.toml · uv PR #7844, #9873, #5021 · ruff contributing · rust#124636 · Rust 2024 edition guide · okou#35505/#34596 · std ExitCode · Bazel test encyclopedia · Bazel user manual · rules_rust releases · Tinder/bazel-diff · bazel-contrib/target-determinator · BazelCon 2025 talk and recap · vergen docs · SOURCE_DATE_EPOCH spec (reproducible-builds.org) · Kent C. Dodds "Effective Snapshot Testing". URLs are inline above; all retrieved 2026-09-22.
