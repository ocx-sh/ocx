# Research: two Bazel mechanisms the adoption plan is sized on

**Axis:** technology verification
**Date:** 2026-09-21
**For:** [`plan_bazel_build_adoption.md`](./plan_bazel_build_adoption.md), which decomposes
[`adr_bazel_build_adoption.md`](./adr_bazel_build_adoption.md)
**Status:** primary-source verified; two ADR mechanisms **refuted**, one confirmed

The ADR's § Stage 2 ruling 5 and its § Stage 1 BUILD-generation section each rest on a
mechanism nobody had measured. Both were measured here. **Both fail.** The third check
(version compatibility) holds exactly as the ADR states.

This is the same shape as the ADR's own recorded lesson — "a README describing a config is
not the config" — one level up: *a design naming a mechanism is not the mechanism*.

---

## Q1 — Can a rules_rust `rust_test` write per-case JUnit XML to `$XML_OUTPUT_FILE`?

**Verdict: NO, not on a stable 1.95.0 pin.** The ADR's primary design is unreachable and
its named fallback is the live path.

| # | Finding | Source |
|---|---|---|
| a | libtest's JSON/JUnit output is **still unstable**. Tracking issue open; `-Z unstable-options` is nightly-gated by design. | [rust-lang/rust#49359](https://github.com/rust-lang/rust/issues/49359) (open), [rust-lang/rust#109044](https://github.com/rust-lang/rust/pull/109044) |
| b | **rules_rust does not write `$XML_OUTPUT_FILE`** on any released version. | [rules_rust#1303](https://github.com/bazelbuild/rules_rust/issues/1303) (open): "Currently `rust_test` does not support generating junit xml report out-of-the-box" |
| b | The fix is **open and unmerged** — not in 0.74.0. Its own description states today's behaviour: "`rust_test` targets don't currently write `$XML_OUTPUT_FILE`, so Bazel falls back to synthesising a single generic `<testcase>` for the whole test binary." | [rules_rust#4181](https://github.com/bazelbuild/rules_rust/pull/4181), `mergedAt: null` |
| c | Bazel's synthesised `test.xml` **wraps the test log** — one `<testcase>` per *target*. | [bazel.build/reference/test-encyclopedia](https://bazel.build/reference/test-encyclopedia) § Initial Conditions |
| d | **No maintained third-party path.** No BCR module, no maintained cargo-nextest↔Bazel bridge, no libtest-json→JUnit converter. | BCR catalogue + GitHub search; negative result |

### What this refutes

The ADR § Stage 2 ruling 5 says `$XML_OUTPUT_FILE` "removes the hand grammar entirely"
and dissolves WP-1b's second falsifier, on the ground that "the mechanism exists, is
already in the repository". **It does not.** What exists in the repository is
*cargo-nextest's* JUnit emitter (`.config/nextest.toml` `[profile.default.junit]`), and a
Bazel `rust_test` does not run nextest — it runs the libtest harness directly.

The ADR's own falsifier 1 described this exact state as the thing to guard against: a
reader that cannot tell the synthesised form from a real one "would report ~34 against a
floor of 8213 and pass nothing while looking like a count". That is now the **expected**
state, not the failure mode.

### What it changes

`quality-core.md` § "Don't Own Non-Domain Code" escalates hand-owned parsing of an
external format to Block, with one escape: "no library implements the requirement,
**verified by searching, not assumed**." Row (d) is that search, and it came back empty.
**The bar is met** — a repo-owned reader of libtest's `test result:` line is now the
sanctioned answer rather than the thing the rule forbids.

Three routes, ranked; see the plan's ruling **P1**:

1. **BEP `TestResult` + `test.log` + libtest's `test result: ok. N passed; M failed; K ignored`
   line** — budgeted. The repository already owns a reader of exactly this shape
   (`rust:test:ceiling` parses nextest's `Summary` line, `taskfiles/rust.taskfile.yml:616-649`).
2. Vendor [#4181](https://github.com/bazelbuild/rules_rust/pull/4181)'s wrapper — the
   optimisation, and the upstream approach, so not a reinvention.
3. Accept target-level granularity and keep the nextest floor/ceiling on the `smoke` job
   indefinitely — which is what the ADR already mandates as the interim state, and is a
   **successful** terminal state for stage 2, not a failure.

Route 1 needs no nightly toolchain. Routes needing `-Z unstable-options` contradict the
pinned stable 1.95.0 and are out.

---

## Q2 — gazelle_rust 0.1.0, and `crate.from_cargo` against `[patch.crates-io]` path deps

**Verdict: gazelle_rust is out of wave 1. And `crate.from_cargo` has a measured upstream
failure against this repository's exact shape.**

### gazelle_rust

| # | Finding | Source |
|---|---|---|
| a | One published version, `0.1.0`. | [bcr.bazel.build/modules/gazelle_rust/metadata.json](https://bcr.bazel.build/modules/gazelle_rust/metadata.json) |
| b | At tag `v0.1.0` its own `.bazelversion` is **8.4.2** and it depends on **rules_rust 0.67.0**. Its CI runs one OS, no version matrix — so **Bazel 9.2.0 / rules_rust 0.74.0 is exercised nowhere**. | [Calsign/gazelle_rust](https://github.com/Calsign/gazelle_rust) at `v0.1.0` |
| c | **#5 closed, but not as the ADR reads it.** The resolution is an opt-in experimental `# gazelle:rust_mode generate_from_cargo`; a downstream user records that it still leaves two sources of truth and abandoned the tool for that reason. | [#5](https://github.com/Calsign/gazelle_rust/issues/5) |
| c | **#15 open** — `# gazelle:rust_cargo_lockfile` makes gazelle read *all* `Cargo.lock` packages including transitive ones, so it can emit a `deps` entry for a crate with no top-level target. | [#15](https://github.com/Calsign/gazelle_rust/issues/15) |
| c | **#29 closed** (Bazel 9 `sh_test`), fixed in `main` at `d1ae032` — `UNVERIFIED` whether that commit is inside the published `v0.1.0`. | [#29](https://github.com/Calsign/gazelle_rust/issues/29) |
| d | **Zero handling of `[patch.crates-io]`, path dependencies, or excluded members** — no issue, no PR, no code hit. The absence *is* the finding. | repo issue + code search; negative result |
| e | Expected first failure: default mode names every crate's target `lib` regardless of package name, so cross-package `deps` resolution picks the wrong target the moment two members exist. This workspace has 20. | maintainer comment in [#5](https://github.com/Calsign/gazelle_rust/issues/5) |

### `crate.from_cargo` + a `path =` patch — the one that bites hardest

> **[rules_rust#1631](https://github.com/bazelbuild/rules_rust/issues/1631) (OPEN)** —
> patching a crate to a local path fails with
> `Error: The package '...' has no source info so no annotation can be made`
> (`crate_universe/src/metadata/metadata_annotation.rs:234`), because a path package has
> `Package.source == None` and the annotator requires a source.
> **The `git = "..."` patch form does work.**

This repository's root `Cargo.toml` carries exactly three such patches
(`oci-client`, `docker_credential`, `sigstore` → `external/` submodule paths, all outside
`members = ["crates/*"]`). Related open issues confirm the class rather than a one-off:
[#1905](https://github.com/bazelbuild/rules_rust/issues/1905),
[#3682](https://github.com/bazelbuild/rules_rust/issues/3682),
[#3732](https://github.com/bazelbuild/rules_rust/issues/3732).

The ADR's § Stage 1 "Mode: `crate.from_cargo` … Not `crates_vendor`" is therefore taking
the mode whose patched-path handling is a known open defect, and its note that "#3732 is
`crates_vendor`-only" is true but incomplete — **#1631 is not `crates_vendor`-scoped.**

See the plan's ruling **P2** for the route taken.

---

## Q3 — rules_rust 0.74.0 sanity check

**Verdict: holds exactly as the ADR states.** No resizing.

| Claim | Measured | Source |
|---|---|---|
| Release date | 2026-08-28 | [releases/tag/0.74.0](https://github.com/bazelbuild/rules_rust/releases/tag/0.74.0) |
| Minimum Bazel | **7.4.1** (`.bazelci/presubmit.yml`); no `bazel_compatibility` declared | tag `0.74.0` |
| Default Rust toolchain | **1.98.0** (`DEFAULT_RUST_VERSION`, `rust/private/common.bzl:34`) | tag `0.74.0` |

The 9.2.0 pin clears the 7.4.1 floor with margin. This repository's 1.95.0 is three
releases *behind* the ruleset default, so `rust.toolchain(versions = ["1.95.0"])` is
mandatory and the gap widens silently on every `rules_rust` bump — which is why the plan
carries C-026, the `rust-toolchain.toml` ↔ `MODULE.bazel` drift check.

---

## Net effect on planning

| Question | ADR's design | Measured | Plan's response |
|---|---|---|---|
| Q1 `$XML_OUTPUT_FILE` | primary; BEP+libtest is the fallback | **primary unreachable on stable** | ruling **P1** — polarity inverts; BEP+libtest budgeted, #4181 the optimisation, target-level granularity a valid terminal state |
| Q2 gazelle_rust | Experimental; hand-written already budgeted | **never exercised on the pin pair; zero `[patch]` story** | ruling **P3** — dropped from wave 1 entirely, docketed with a named reopen condition |
| Q2 `crate.from_cargo` | chosen mode, "#3732 is `crates_vendor`-only" | **#1631 hits `from_cargo` on `path =` patches** | ruling **P2** — `external/` crates become first-party Bazel targets, not `crate_universe` entries; WP-11 is the gating spike |
| Q3 versions | 9.2.0 / 0.74.0 / 1.98.0 default | **confirmed** | unchanged |

**Wave-1 sizing that assumed automatic per-test JUnit reporting and clean generated BUILD
files is optimistic on both counts.** Only the version-compatibility assumption is solid.
