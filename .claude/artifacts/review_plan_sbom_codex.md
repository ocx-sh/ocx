# Codex Adversarial Review — SBOM/DSSE Attestations Plan

Reviewer: Codex terra (`gpt-5.6-terra`), invoked via `codex-companion.mjs task` in read-only mode (no `--write`, sandbox forced to `read-only`) — no fix loop, no edits.
Runs: two independent one-shot passes against the same prompt, target `.claude/state/plans/plan_sbom_attestations.md` cross-referenced against `.claude/artifacts/adr_sbom_attestations.md`; both completed (exit 0). The first run's background job was killed by a turn boundary before I could read it and was re-run in the foreground — both transcripts turned out to be complete and are included below since they corroborate each other independently.

## Raw Codex output — Run 1 (background, completed, 4 findings)

```
**Block — C-002 leaves the shipped signature path unowned.**

1. Plan: line 89, line 96.
2. Defect: C-002 requires signature referrers to gain `created` and `content: message-signature`, but no WP owns `oci/sign/pipeline.rs`; WP2 and WP7 both exclude it.
3. Failure scenario: agents implement their listed files literally. The live signature path still calls `ReferrerManifest::build(...)` without annotations at sign/pipeline.rs:232, so newly produced signature referrers violate C-002.
4. Suggested fix: add `oci/sign/pipeline.rs` to WP2 or WP7, explicitly require the two signature annotations, and add it to the shared-file register if WP2 also changes its constructor API.

**Block — WP2 cannot pass its own required build gate after C-021.**

1. Plan: line 89, lines 176-180.
2. Defect: WP2 adds `PushOutcome.platform_digests` and `#[non_exhaustive]`, but omits `crates/ocx_cli/src/api/data/push.rs`, which constructs that public `ocx_lib::publisher::PushOutcome` in its unit tests.
3. Failure scenario: after WP2, the struct literals at api/data/push.rs:111, 180, and 203 fail to compile: first for the missing field, and with `#[non_exhaustive]`, because an external crate may not construct it at all.
4. Suggested fix: include `api/data/push.rs` in WP2 and replace those test fixtures with a public constructor/helper, or move all of C-021 to a WP that owns both `publisher.rs` and this consumer.

**Warn — the stated critical path contradicts the DAG.**

1. Plan: line 144, lines 129-140.
2. Defect: the declared path begins at WP0, but WP4 also requires WP2 and WP-A, both gated by WP-S. Those predecessors are absent from the purported critical path.
3. Failure scenario: a coordinator prioritizes only WP0 as critical; a delayed spike/WP2/WP-A delays WP4 and every downstream WP despite not appearing in the scheduling-critical chain.
4. Suggested fix: state the joined path, e.g. `WP-S -> (WP0, WP2, WP-A) -> WP4 -> WP6 -> WP8b -> WP9b -> WP10a/WP10b`, noting equivalent joined predecessors.

**Suggest — WP11 is under-sized by a full class.**

1. Plan: line 104.
2. Defect: WP11 is labelled M despite spanning roughly sixteen surfaces: nine documentation pages/config entries, two cast scripts, and four rule/catalog files.
3. Failure scenario: an implementing agent is allocated an M-sized wave-6 slot and either defers documentation/rule updates or delays the branch's declared documentation-and-casts completion bar.
4. Suggested fix: label WP11 L, or split docs/casts from rule/catalog updates into separately reviewed WPs.

C-/S traceability: all C-001..C-021 and S-001..S-019 are covered by at least one row, including the plan's explicit ID ranges. The same-wave insertion-point claims do not expose a current merge conflict.

Finding count: 4
```

## Raw Codex output — Run 2 (foreground, completed, 3 findings)

```
### Block

1. **Plan line 89** — WP2's file set omits required existing consumers of both C-002 and C-021. `ReferrerManifest::build` is called by the shipped signature writer at pipeline.rs:232, yet C-002 requires signature referrers to write `created` and `content: message-signature`. C-021 also mandates `PushOutcome #[non_exhaustive]`; push.rs:112 constructs that cross-crate type literally.
   Failure scenario: following WP2 literally edits only its listed files. Signature referrers retain no required annotations. Adding `#[non_exhaustive]` makes the CLI test's `PushOutcome { ... }` illegal outside `ocx_lib` (and adding only the field also leaves that literal missing a field), so Wave 1 cannot pass its required verification.
   Suggested fix: make WP2 at least M and include `oci/sign/pipeline.rs`, `oci/verify/pipeline.rs`'s existing manifest fixture call, and `ocx_cli/src/api/data/push.rs`. Specify a replacement construction seam for CLI tests before marking `PushOutcome` non-exhaustive.

2. **Plan line 92** — WP9-pre creates `command/package_sign_common.rs` but omits its required parent-module declaration, `crates/ocx_cli/src/command.rs`. The current parent explicitly declares every sibling command module at command.rs:38.
   Failure scenario: the implementing agent moves the resolver out of `package_sign.rs` into the new file and imports `super::package_sign_common`; Rust cannot resolve that module because no `pub mod package_sign_common;` exists. Wave 2 fails compilation. The same omission recurs for the new `package_attest.rs` and `package_sbom.rs` files in WP9a/WP9b.
   Suggested fix: add `command.rs` to WP9-pre, WP9a, and WP9b; assign each its own module row and add it to the shared-file register as a declared same-wave exception for WP9a/WP9b.

### Warn

3. **Plan line 144** — The stated critical path contradicts the plan's own graph. WP4 is also blocked by WP-A, and WP-A is blocked by WP-S (lines 129-130); the listed path begins at WP0 and omits the Wave 0 predecessor chain.
   Failure scenario: an execution coordinator uses the stated critical path for staffing or milestone risk. WP0 completing does not unblock WP4 unless the WP-S -> WP-A branch has also completed, so the forecast understates the actual gating chain.
   Suggested fix: state `WP-S -> WP-A -> WP4 -> WP6 -> WP8b -> WP9b -> WP10a/WP10b` as the critical path, with WP0/WP2/WP5/WP1 noted as co-prerequisites where applicable.

3 findings.
```

## Corroboration (independently verified against the real repository tree)

All three checks below were run directly against `/home/mherwig/dev/ocx-soraka/.agents/worktrees/sbom-milestone`, not taken on Codex's word.

### Finding A — WP2 omits `oci/sign/pipeline.rs` (both runs, same defect)

**CONFIRMED, high confidence.** `crates/ocx_lib/src/oci/sign/pipeline.rs:232` reads exactly:
```rust
let manifest = ReferrerManifest::build(subject_descriptor, SIGSTORE_BUNDLE_V03, bundle_descriptor);
```
— a 3-argument call with no annotation parameter, confirming this is the live, shipped signature-referrer construction path. Plan line 89 (WP2's Expected files: `oci/referrer/manifest.rs`, `oci/referrer/media_types.rs`, `publisher.rs`, `Cargo.toml`) and plan line 96 (WP7's Expected files: `oci/sign/{signer,rekor,bundle}.rs`) both checked — neither lists `pipeline.rs`. C-002 (plan line 37) explicitly requires "sign referrers gain `created` + `content: message-signature`." No WP in the table touches the one file that would need to pass those annotations through. Real gap.

### Finding B — WP2 omits `api/data/push.rs`, breaks under `#[non_exhaustive]` (both runs, same defect)

**CONFIRMED, high confidence, more precise than either raw transcript.** `crates/ocx_cli/src/publisher.rs` (correction: `crates/ocx_lib/src/publisher.rs`) currently defines `PushOutcome` with `#[derive(Debug)]` and **no** `#[non_exhaustive]`. `crates/ocx_cli/src/api/data/push.rs` — a different crate — imports `ocx_lib::publisher::PushOutcome` and constructs it via bare struct-literal syntax inside `#[cfg(test)] mod tests` (confirmed at line 112, a helper `fn outcome(...) -> PushOutcome { PushOutcome { manifest_digest: ..., cascade_tags, canonical_tags, layer_counts: Default::default() } }`; Codex's other two cited lines, 180 and 203, are additional literals further down the same test module). `#[non_exhaustive]` blocks struct-literal construction from outside the defining crate **unconditionally, test code included** — this is standard Rust semantics, not a maybe. WP2's row (C-002, C-021) does not list `api/data/push.rs`. Concrete failure: `task verify --force` at the Wave 1 merge gate runs `ocx_cli`'s test suite, which will not compile once WP2 lands `#[non_exhaustive]` + the new `platform_digests` field with no matching update to this helper.

### Finding C — WP9-pre/WP9a/WP9b omit `crates/ocx_cli/src/command.rs` (Run 2 only)

**CONFIRMED and sharper than Codex stated it — this is the most valuable finding in the pass.** `crates/ocx_cli/src/command.rs` is a flat file of 34 `pub mod <name>;` lines, one per existing command file (`pub mod package_sign;` at line 49, `pub mod package_verify;` at line 51, etc. — confirmed by direct read). `crates/ocx_cli/src/command/package.rs` is a *different* file: the `#[derive(Subcommand)] pub enum Package` that lists CLI subcommand variants (`Sign(super::package_sign::PackageSign)`, etc.) via `super::` paths back into `command.rs`'s declarations. WP9a and WP9b's rows already list `command/package.rs` (for the new enum variants `Attest(...)`/`Sbom(...)`) — but that is **not** the same file as the one that declares the module itself. None of WP9-pre (`package_sign_common.rs`, new), WP9a (`package_attest.rs`, new), or WP9b (`package_sbom.rs`, new) lists `command.rs` in their Expected files. Given the consistent, unbroken pattern across all 34 existing sibling files, all three new files need a `pub mod` line added there or the module will not resolve. Codex's raw finding named only WP9-pre explicitly and WP9a/WP9b in passing prose without the `command/package.rs` vs `command.rs` distinction; I confirmed the distinction is real and the gap applies to all three WPs, not one.

### Finding D — stated critical path omits the WP-S -> WP2/WP-A predecessor chain (both runs)

**CONFIRMED as literally true, Warn-tier as both runs classified it.** Cross-checked against the plan's own Depends column and mermaid graph: WP4's Depends is `WP0 WP2 WP-A`; WP2's Depends is `WP-S`; WP-A's Depends is `WP-S`; WP0's Depends is `—`. The prose critical-path line (plan line 144: "WP0 -> WP4 -> WP6 -> WP8b -> WP9b -> WP10a/WP10b") is the longest single-predecessor chain by wave count but is not the deepest true dependency chain, which runs through WP-S (wave 0) via WP2 or WP-A before reaching WP4. Real, but it is a scheduling/reporting inaccuracy in one summary bullet, not an execution blocker — the actual Merge plan bullet two lines below it and the Wave/Depends columns that gate real execution are correct. The two runs' "fixed" paths differ slightly (`WP-S -> (WP0, WP2, WP-A) -> WP4` vs `WP-S -> WP-A -> WP4`, the latter dropping WP0/WP2); neither is a clean drop-in replacement, so any fix should restate all three of WP4's real predecessors rather than adopt either verbatim.

### Finding E — WP11 undersized (Run 1 only, Suggest-tier)

**Plausible but soft; not independently corroborated to Block/Warn confidence.** File-counted WP11's Expected files column myself: 16 distinct files/surfaces (3 in-depth docs, 3 reference docs, 2 new user-guide pages, 1 user-guide overview, 1 VitePress config, 2 cast scripts, 3 AI-config rules, 1 rules catalog) — Codex's count checks out. Labelled M against WP0's L for roughly 9 Rust/error-taxonomy files. The comparison is not apples-to-apples (docs edits are typically lower per-file complexity than cross-cutting Rust error-taxonomy work), so I'd surface this as worth a second look rather than a confirmed mis-sizing.

### Not corroborated further / accepted as-is

Both runs' claim that "all C-001..C-021 and S-001..S-019 are covered by at least one WP row" was spot-checked against the Component contracts and User-experience scenarios tables against the Parallelization table's Scope column — no gap found on my own pass either. Not re-litigating ADR-level design decisions per the review brief; no such finding was raised by either run.
