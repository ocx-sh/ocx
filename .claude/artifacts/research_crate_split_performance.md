# Research: Developer-loop performance baseline and crate-split projection

## Metadata

**Date:** 2026-09-06
**Domain:** cli
**Triggered by:** /hex-architect crate-split-and-verify-tiers
**Expires:** 2027-03-06

## Direct Answer

On this machine, right now, touching **any** file inside `ocx_lib` — a 91-line leaf
module or the single largest file in the workspace (`oci/client.rs`, 8,157 lines) —
costs the same **~7.0s** incremental rebuild, because the whole 278,777-LOC crate is
rustc's unit of front-end re-check and codegen-unit invalidation. Touching a file in
`ocx_cli` (already a separate crate) costs **1.4–1.5s** and never rebuilds `ocx_lib` at
all. A crate split's biggest win is therefore not "faster codegen" (the measured
codegen rate is ~17 µs/LOC — small numbers) but **restoring the today-CLI-only
isolation to the inside of `ocx_lib`**: a touch in a small, cleanly-layered foundation
crate would plausibly drop to ~2.5–3s (a 2.5–3x win); a touch in the still-entangled
`oci` hub (per the open ADR thread on inverting its edges) would only drop to
~4–5s (a 1.4–1.8x win) until that entanglement is resolved. The full 2,412-test
acceptance suite and the release-profile CI build remain the dominant cost regardless
(CI evidence: Build+Unit 1,054s, Acceptance 1,604s, Linux) — a crate split changes the
*edit-compile-test loop*, not the *CI wall clock*, unless paired with the scoped-gate
work the parent discussion also proposes.

## Environment

- Host: WSL2, 32 cores, 31G RAM / 32G swap, ~15G RAM free during this run.
- Build-slot guard (discovered, not documented in this repo):
  `/home/mherwig/.cache/hex/build-slot.sh` → `/home/mherwig/dev/ocx/.tmp/hex/build-slot.sh`,
  a 2-slot `flock -n -E 99` guard shared across every worktree on the host, each slot
  running with `CARGO_BUILD_JOBS=12` (env wins over this repo's `.cargo/config.toml`
  `jobs=4`), gated on ≥6GB free RAM, starting a shared `sccache` daemon outside the
  lock. Both slots were free for the whole session; every measurement below ran
  through this wrapper.
- `cargo-nextest 0.9.143` is installed and used for the workspace's own `rust:test:unit`
  task already.
- Linker actually in use: **LLD 22.1.2** (confirmed via `readelf -p .comment` on
  `target/debug/ocx`), not mold — `which mold` finds nothing and `.cargo/config.toml`
  sets no linker override, despite `product-tech-strategy.md` listing "Linker: Mold
  (dev)" as the golden path. Not in scope to fix here, but the golden-path doc and
  reality have drifted; worth a note back to whoever owns that doc. Dev-profile link
  time for the whole `ocx` binary is already fast (~1.3s), so this drift is not costing
  the loop measured here.

## Measurements

All commands run through the build-slot wrapper (`$BS = /home/mherwig/.cache/hex/build-slot.sh`); "wall" = `time` on the wrapper invocation.

| # | Command | Scenario | Wall time | Notes |
|---|---|---|---|---|
| 1 | `$BS cargo build -p ocx` | Session warm-up (many transitive deps first-compile: aws-lc-rs, rustls stack, sigstore, oci-client fork, then `ocx_lib`+`ocx`) | 71.7s | Not a clean incremental baseline — deps were cold |
| 2 | `$BS cargo build -p ocx` | Fully warm, no-op | 0.64s / 0.82s | True do-nothing baseline |
| 3 | `$BS cargo build -p ocx --timings` | **Leaf touch**: `crates/ocx_lib/src/media_type.rs` (91 LOC, +1 blank line, restored byte-identical after) | 6.93s / 7.01s | `ocx_lib` recompiles (4.65s codegen), `ocx` relinks (2.35s + 1.26s, two units: lib + bin) |
| 4 | `$BS cargo build -p ocx --timings` | **Hub touch**: `crates/ocx_lib/src/oci/client.rs` (8,157 LOC, largest file in the repo, +1 blank line, restored) | 7.01s / 7.09s | Identical shape to the leaf touch: `ocx_lib` 4.68s + `ocx` 2.53s+1.21s — **leaf vs. hub make no difference today** |
| 5 | `$BS cargo build -p ocx --timings` | **CLI touch**: `crates/ocx_cli/src/main.rs` (77 LOC, +1 blank line) | 6.65s / 6.73s | Contaminated: still shows `ocx_lib` recompiling. Root cause (isolated below): restoring `client.rs` via `sed -i` bumped its mtime *after* build #4 fingerprinted it, so this build re-saw `client.rs` as dirty. Cargo's fingerprinting here is mtime-based, not content-hash — a restored-to-identical file still invalidates the crate's fingerprint until one more build settles it. This is a methodology artifact of restore-after-measure ordering, not a real "any touch anywhere rebuilds everything" phenomenon (see #6). |
| 6 | `$BS cargo build -p ocx` | CLI-touch settle build (= a clean, isolated `main.rs`-only mtime change, no prior contamination) | 1.44s / 1.51s | Only the `ocx` (CLI) crate recompiles; `ocx_lib` is untouched. **Proves `ocx_cli`-only edits are already isolated from `ocx_lib` today** — the crate boundary that exists already does its job. |
| 7 | `$BS cargo check -p ocx_lib` | Leaf touch, 1st run (check-profile artifact cache cold for `schemafy`/`starlark`/`starlark_syntax`) | 27.47s | Not representative — one-time cache warm-up for the `check` profile, separate from `build`'s cache |
| 8 | `$BS cargo check -p ocx_lib` | Leaf touch, 2nd run (check-profile warm) | 2.59s / 2.65s | True incremental `check` cost |
| 9 | `$BS cargo clippy -p ocx_lib` | Leaf touch, 1st run (clippy's own artifact cache cold) | 17.09s / 17.17s | Clippy has a third, separate artifact cache from `build`/`check` |
| 10 | `$BS cargo clippy -p ocx_lib` | Leaf touch, 2nd run (clippy cache warm) | 5.51s / 5.59s | True incremental `clippy` cost |
| 11 | `$BS cargo test -p ocx_lib --no-run` | 1st run (`test`-profile cache cold — a 4th separate cache) | 1m38.8s | One-time cache warm-up |
| 12 | `$BS cargo test -p ocx_lib --no-run` | 2nd run, warm no-op | 0.28s / 0.35s | |
| 13 | `$BS cargo test -p ocx_lib media_type::` | Targeted module test run | 0.21s build + <0.01s run | 3 tests, instant |
| 14 | `$BS cargo nextest run -p ocx_lib` | Full `ocx_lib` unit suite, dev/debug profile | 111.2s (nextest) / 111.9s wall | 5,711 tests run, 5,711 passed, 8 skipped. Not directly comparable to the CI figure (1,054s) below — CI runs `--release`; this was dev/debug. |

**CI reference (from the recon artifact, feature-branch runs, 2026-09-06 — no `main` baseline exists in the last 15 runs):** "Verify Deep" Build & Unit Test Linux 1,054s (release profile), Acceptance (Linux) 1,604s. "Basic Verification" Acceptance Tests 558s.

**Interpretation of #3–#6 together:** in the current 4-crate layout, *every* single-file edit inside `ocx_lib` — regardless of whether it lands in a 91-LOC leaf or an 8,157-LOC hub file — forces the same whole-crate front-end pass and the same ~3.6s `ocx` relink/build-script tail, for a uniform ~7s. The one file that already gets cheap, isolated treatment is anything in `ocx_cli`, because it is *already* a separate crate. This is the empirical version of the discussion's own framing: the win isn't "less LOC to compile," it's "isolation that currently only exists at the `ocx_lib`/`ocx_cli` boundary, moved to more boundaries."

**cargo has (at least) four separate artifact caches per profile** (`build`/dev, `check`, `clippy`, `test`) that do not share compiled output with each other — confirmed empirically by #7 and #9 each needing a first, slower run to populate `schemafy`/`starlark`-family and clippy-specific artifacts respectively, even though `ocx_lib` itself had already been built via `cargo build` moments earlier. This matters for the scoped-gate design in the parent discussion: a WP gate that runs `clippy` then `nextest` cold, back to back, pays for two separate cache warm-ups the first time, not one.

## Projection: crate split (illustrative map, not the ADR's map)

**Assumption A1 (linear codegen proxy):** using the leaf-touch measurement, `ocx_lib`'s
whole-crate codegen cost is 4.65s for 278,777 LOC ⇒ **~16.7 µs/LOC**. This is a crude
linear stand-in for an order-of-magnitude estimate only — real incremental compilation
reuses codegen units untouched by the change, so actual per-touch cost inside a smaller
crate is likely *sub-linear* relative to this proxy, not linear. Stated, not proven.

**Assumption A2 (fixed relink/build-script tail):** the ~3.6–3.7s "`ocx` relink +
`vergen-gix` build-script rerun" tail observed in #3/#4 is treated as roughly constant
per build regardless of which upstream crate changed, based on the dev-profile link
step alone costing ~1.3s (from the `--timings` JSON) and the build script re-running on
every build in this repo (its `rerun-if-changed` set appears sensitive to `.git`
state changes, plausibly from Git index-refresh side effects of unrelated `git status`/
`git diff` calls made between builds — not root-caused further, out of scope here).

**Assumption A3 (illustrative crate map):** the ADR has explicitly not chosen crate
boundaries (`Threads` → "open — seam order" in the discussion artifact). The map below
is built directly from the recon's own classification (OCI generic/specific/mixed,
per-hub sub-module tables) purely to make the projection concrete — treat every crate
name and boundary as illustrative, not a recommendation:

| Illustrative crate | Contents | LOC (approx.) |
|---|---|---|
| `ocx_foundation` | `media_type`, `log`, `error` (leaf), generic `utility` minus `fs/assemble.rs` | ~6,660 |
| `ocx_cli_types` | `ExitCode`/`ClassifyExitCode`/`ClassifyErrorKind` + presentation primitives (`Printer`, `ProgressManager`, `DataInterface`, `UserInterface`, `Theme`) | ~4,754 |
| `ocx_oci_generic` | 13 OCI/cosign-spec-generic sub-modules (`annotations`, `digest`, `endpoint`, `manifest`, `manifest_builder`, `pinned_identifier`, `referrer`, `repository`, `resolve_target`, `simplesigning`, `ssrf`, `transport_policy`, `file_storage`) | ~7,275 |
| `ocx_oci_index` | `oci::index` (OCX's own resolution-index protocol) | ~22,139 |
| `ocx_trust` | `trust` (signer-identity policy engine) | ~3,458 |
| `ocx_oci_mixed` | `oci::{client, attest, copy, host_capabilities, identifier, platform, sign, verify}` — still entangled with `trust`/`config`/`env`/`project`/`announce` per the open ADR thread | ~57,481 |
| `ocx_config` | `config`, `managed_config`, `env` | ~21,684 |
| `ocx_store` | `file_structure`, `symlink`, `reference_manager`, `archive`, `hardlink`, `shim`, `utility::fs::assemble` (relocated per the recon's own recommendation) | ~16,851 |
| `ocx_package` | `package` (incl. `metadata`) | ~26,829 |
| `ocx_package_manager` | `package_manager` | ~43,574 |
| `ocx_workflow` | `project`, `shell`, `setup`, `record`, `forge`, `publisher`, `announce`, `patch`, `auth`, `ci`, `launch`, `activation`, `lazy`, `codesign`, `compression`, `sbom` | ~65,019 |

11 crates + the unchanged `ocx_cli` binary crate — inside the 8–15 range asked for.
Rows sum to ~275,700 of the 278,777 LOC total; the ~3,000-LOC residual is top-level
module files not individually re-attributed (rounding, not a hidden crate).

### Touch projection

| Touch | Crate(s) rebuilt today | Crate(s) rebuilt after split (illustrative) | Est. time (today → after) |
|---|---|---|---|
| Leaf (`media_type.rs`) | `ocx_lib` (278,777 LOC) + `ocx` relink | `ocx_foundation` (own compile) + recheck/relink of its 3 real dependents (`ocx_config`, `ocx_oci_generic`/`ocx_oci_mixed`, `ocx_package_manager`, per today's `media_type` in-degree) + final `ocx` relink | ~7.0s → **~2.5–3s** (≈2.5–3x) |
| Hub (`oci/client.rs`) | `ocx_lib` (278,777 LOC) + `ocx` relink | `ocx_oci_mixed` (own compile, ~57K LOC) + real (not just metadata) recompile pressure on `ocx_package_manager` (today's heaviest single edge, 317 refs into `oci`) + `ocx_package`/`ocx_cli_types` recheck + final relink | ~7.0s → **~4–5s** (≈1.4–1.8x) |
| CLI (`ocx_cli/main.rs`) | `ocx` only (already isolated, #6) | Unchanged — `ocx_cli` was already its own crate | ~1.5s → **~1.5s** (no change) |

The hub number is the weaker win **on purpose**: it reflects the open ADR thread that
`oci`'s 8 "mixed" sub-modules (`client`, `verify`, `sign`, …) are still genuinely
coupled to `package_manager`/`trust`/`config` by real code, not just by module
placement — the recon's own finding that `package_manager -> oci` is 317 references,
the single heaviest edge in the graph. Splitting the *file* into a new crate without
inverting those edges first (the discussion's stated sequencing: "layer in place, then
extract") would not fully capture the leaf-touch-sized win. If the ADR does invert
those edges, the hub number should converge toward the leaf number over time.

## Key findings

1. **Crate splitting is the standard first lever for Rust compile times, with concrete before/after numbers from a comparable "large generated codebase → many small crates" case**: Feldera cut a ~1,100-crate split from 25–45 min to 2m10s wall, explicitly because incremental compilation could then skip identical sibling crates by content hash — but they also report the parallel speedup was ~7x worse than theoretical linear scaling even at full CPU utilization, i.e. the win is real but sublinear in crate count. [Cutting Down Rust Compile Times From 30 to 2 Minutes With One Thousand Crates](https://www.feldera.com/blog/cutting-down-rust-compile-times-from-30-to-2-minutes-with-one-thousand-crates) (2025-04-15, ~17 months old — near the edge of the 18-month freshness window, flagged).
2. **The canonical framing — "the crate is rustc's unit of compilation, caching, and parallelism" — is matklad's, and it is old (2021) but still the reference point every later source restates**, including the guidance that splitting a monolithic crate is the single highest-leverage lever available. [Fast Rust Builds — matklad](https://matklad.github.io/2021/09/04/fast-rust-builds.html) (2021-09-04, **flagged: ~5 years old**, cited only because it remains the load-bearing canonical source every 2025/2026 post below still points back to).
3. **A parallel *frontend* (not just parallel codegen units) is close to real, and would blunt some of the "any touch rechecks the whole crate" cost this report measured** — Kobzol's most recent status report describes the rustc-rayon-based parallel front-end as past its major deadlock/ICE issues, enabled in Rust's own bootstrap, with surrounding-tooling support (rustc-perf, Cargo) now the open item before wider stabilization. Available today only via nightly `-Z threads=N`. [Sovereign Tech Fellowship for Rust maintenance (June–July 2026 report) — Kobzol](https://kobzol.github.io/rust/2026/08/03/stf-june-july-2026.html) (2026-08-03, fresh); background: [Faster compilation with the parallel front-end in nightly](https://blog.rust-lang.org/2023/11/09/parallel-rustc/) (2023-11-09, **flagged: ~2.8 years old**, superseded in currency by the 2026 report but still the correct technical description of the mechanism).
4. **`split-debuginfo = "unpacked"` is a same-machine, same-crate-count lever this project is not using, and it targets exactly the Linux dev-profile debuginfo-linking cost this report's link-time measurement touched.** Default is `packed` on Linux (full debuginfo folded into the binary via the linker); `unpacked` keeps `.o`-adjacent debug sections and skips that fold on every incremental link. Not free — worth a controlled A/B, not a blind flip. [Disable debuginfo to improve Rust compile times — Kobzol](https://kobzol.github.io/rust/rustc/2025/05/20/disable-debuginfo-to-improve-rust-compile-times.html) (2025-05-20, 16 months old, fresh).
5. **This host already runs a shared `sccache` daemon across worktrees (`build-slot.sh`), which is exactly the setup a February 2026 write-up found broken by default**: sccache's cache key includes the working directory, so naive cross-worktree sharing cache-misses every time, and `--remap-path-prefix` isn't recognized by sccache (making it worse, not better, since the unrecognized flag itself becomes part of the key). The author's working fix was a hardlink-based dependency-artifact copier (immutable deps hardlinked, mutable workspace artifacts left alone), cutting worktree setup from 2m19s (plain copy) to <1s. **Action-relevant**: worth checking whether this repo's `sccache` daemon is actually getting hits across worktrees today, or silently missing on every one exactly as described. [Sharing Rust Build Cache — howardjohn](https://blog.howardjohn.info/posts/shared-rust-build/) (2026-02-18, fresh).
6. **cargo-nextest's archive feature (already installed here, v0.9.143) lets a CI job build test binaries once and reuse them in downstream jobs/machines**, including build-script `OUT_DIR`s (one level deep) and dynamic libraries test binaries link against — directly relevant to the parent discussion's "scoped WP gate ≤5 min" goal, since it removes redundant test-binary builds between a scoped tier and a later full tier in the same CI run. [Archiving and reusing builds — cargo-nextest](https://nexte.st/docs/ci-features/archiving/) (evergreen docs, undated; tool version confirmed current on this host).
7. **Bevy is the closest real precedent for "split a monolith into ~10+ crates for a CLI/engine-shaped Rust project," and its own experience report adds a lever this project hasn't considered**: `bevy_dylib` forces dynamic linking in dev builds specifically because static linking means every change anywhere requires re-linking the *entire* engine — the same "final relink is a fixed, non-trivial tail" pattern this report measured (~1.3–3.6s) for `ocx`. A `cdylib`/dylib dev-only escape hatch is a smaller, faster-to-ship change than the full crate split and could be evaluated independently. [Crate Organization — Bevy (DeepWiki)](https://deepwiki.com/bevyengine/bevy/1.2-crate-organization) (evergreen, undated).
8. **cargo maintains separate, non-shared artifact caches per profile** (`dev`/`build`, `check`, `clippy`, `test` each populate their own directory under `target/`) — not found via a single canonical citation, but directly reproduced in this session (measurements #7 and #9 above): the very first `cargo check` and `cargo clippy` runs each paid a one-time cost to compile check/clippy-specific artifacts for `schemafy`/`starlark`-family crates, *after* `cargo build` had already succeeded moments earlier on the same source tree. Relevant to the scoped-gate design: running `clippy` then `nextest` back-to-back in a cold CI cache pays two separate warm-ups, not one.

## Recommendation

Proceed with the crate split as scoped by the ADR, but calibrate expectations: it is a
**2.5–3x** win for touches that land in genuinely decoupled foundation code, degrading
toward **1.4–1.8x** for touches inside `oci`'s still-entangled "mixed" sub-modules until
their edges are inverted — it will not make `ocx_lib` touches as cheap as `ocx_cli`
touches are today unless the edge-inversion work (already sequenced first in the
discussion's Decisions) actually lands before extraction. Two independent, much
cheaper levers are worth a short spike before or alongside the split, because neither
requires the crate-boundary work to pay off:

- **Verify the shared `sccache` daemon (`build-slot.sh`) is actually hitting across
  worktrees**, given the February 2026 report that naive sccache-across-worktrees
  silently cache-misses on working-directory-sensitive keys. If it's missing, the
  hardlink-based dependency-artifact approach from that report is a same-week fix,
  independent of any crate work.
- **A/B `split-debuginfo = "unpacked"`** in this repo's dev profile — it targets the
  same "relink tail" this report measured as a large, roughly-fixed cost per
  incremental build (~1.3–3.6s), and costs one profile-key line to try.

Both are additive to the crate split, not substitutes for its stated primary goals
(predictable change surface, reusability) — but they're cheaper to validate and would
compound with it.

## Self-check

- Tree left clean: `/usr/sbin/git status --short` shows only the pre-existing
  `.agents/memory/hex.md` modification (present before this session started, not
  touched by this work) plus untracked artifacts from other concurrent research agents
  in this session (not created by this agent). `git diff --stat` on all three files
  touched during measurement (`media_type.rs`, `oci/client.rs`, `main.rs`) is empty —
  confirmed byte-identical restoration.
- Every external claim above carries a URL and a publication date where the source is
  dated; two sources (matklad 2021, the Nov-2023 parallel-rustc announcement) are
  flagged as older than 18 months and kept only because later, fresher sources
  (Kobzol 2026-08, Feldera 2025-04) still point back to them as the canonical framing.
- Stated assumptions: A1 (linear LOC→codegen-time proxy, likely an overestimate of the
  win since real incremental reuse is sub-linear), A2 (fixed ~3.6s relink/build-script
  tail independent of which upstream crate changed), A3 (the 11-crate map is
  illustrative only — the ADR has not chosen crate boundaries; built directly from the
  recon artifact's own OCI generic/specific/mixed classification and per-hub sub-module
  tables, not invented here).
- Known gaps: did not measure a cold, from-scratch full workspace build (budget
  trade-off — the incremental/touch-based numbers are what the parent decision needs);
  did not root-cause why the `ocx_cli` build script appears to rerun on every build
  (plausible Git-index-refresh side effect of `git status`/`git diff` calls between
  measurements, not confirmed); did not test the nightly parallel-frontend
  (`-Z threads=N`) directly, since this project pins a non-nightly toolchain.
