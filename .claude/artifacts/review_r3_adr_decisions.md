# Round-3 ADR Decision Log — OCI Referrers Signing v1

**Date:** 2026-05-14
**Trigger:** RCA Cluster B + D from `.claude/artifacts/review_r3_rca_oci_referrers.md`
**Source review:** `.claude/artifacts/review_r3_verdict_oci_referrers.md`
**Amended ADR:** `.claude/artifacts/adr_oci_referrers_signing_v1.md`
**Touched rules:** `.claude/rules/arch-principles.md`, `.claude/rules/subsystem-file-structure.md`

Append-only summary of each amendment, the chosen option, the rationale in one paragraph, and the Phase 5c impact. The ADR itself is the source of truth for full rationale and trade-off tables.

## Summary table

| # | Cluster | Topic | Chosen | Stakeholder needed? | Phase 5c code impact |
|---|---|---|---|---|---|
| 1 | B (B1) | `Client::transport()` accessor | **Option (3) — `SignPipeline::run(&Client, options)`** | Yes — Option (3) recommended, Option (2) acceptable fallback under schedule pressure | Delete `pub struct SignContext`; rename `Pipeline → SignPipeline`; demote `Client::transport()` to `pub(crate)` |
| 2 | B (F1) | `UrlRejection` cross-subsystem | **Lift to `oci::endpoint` peer module** | No | Move `endpoint.rs` from `oci/sign/` to `oci/`; update imports in both pipelines |
| 3 | B (G1) | `state/` fourth tier | **Document as legitimate fourth tier** | No | Add `StateStore` to `FileStructure`; thin wrapper module; no data migration |
| 4 | B (BLOCK-1) | Rekor v2 client tracking | **Track via upstream + internal GitHub issues + release-notes copy** | Yes — issues need filing; URLs are placeholders in ADR | No code change. Filing two tracking issues + release-notes update are non-code Phase 5c gates |
| 5 | B (BLOCK-2) | TUF rotation SLA | **Option (A) — embedded-only + 90-day SLA + nightly CI parity check** | No (v1); v2 may revisit Option (B) | Wire embedded asset (replace `EmbeddedAssetMissing` production state); add nightly `.github/workflows/sigstore-trust-root-drift.yml` |
| 6 | D (TTL drift) | Capability-cache TTL | **Flat 6h v1; CI/interactive split deferred to v2** | No | None to code (already 6h flat). Update PRD FR-17 text |
| 7 | D (G3) | `Signer::sign` return shape | **Kind-only `Result<SignedBundle, SignErrorKind>` (current code wins)** | No | None to code. Update ADR §"Signer trait abstraction" code block to match |
| 8 | B (F2, F3) | Token-provider boundary concerns (`AmbientProvider::detect()` static-trait, `DispatchingTokenProvider::new` narrowness) | **Defer to v2; v1 ships current shape; v2 seam = `AmbientProvider` instance dispatch + `DispatchingTokenProvider::builder()`** | No (explicit defer; round-1 reviewer asked for explicit defer record) | None to code. Plus: back-ref banner above §"Signer trait abstraction" code block pointing to Amendment 7 |

## Per-amendment narrative

### Amendment 1 — `Client::transport()` accessor pattern

**Chosen: Option (3) — `SignPipeline::run(&Client, options)`. Recommended; awaiting stakeholder confirmation before Phase 5c branch creation.**

Three options exist: (1) keep `Client::transport()` public and document invariants; (2) `SignContext::new_from_client(&Client)` with `transport()` private; (3) `SignPipeline::run(&Client, options)` taking `Client` directly, never exposing transport. Option (3) wins on boundary clarity (preserves `Client` facade), future-symmetry (`VerifyPipeline::run` follows the same shape), test mockability (pipelines accept the existing `Client`/`TestTransport` pair), and YAGNI (the `SignContext` struct exists only to bundle five fields the pipeline reads in sequence). Option (1) is **not recommended** — it permanently locks the trait shape into public surface. Option (2) is the acceptable fallback if Phase 5c schedule pressure trumps boundary purity.

**Open question:** Pick Option (3) for purity vs Option (2) for diff size. Stakeholder confirmation requested.

**Phase 5c impact:** Delete `pub struct SignContext`; rename `sign/pipeline.rs::Pipeline` to `SignPipeline` with the recommended signature; demote `Client::transport()` to `pub(crate)`.

### Amendment 2 — `UrlRejection` cross-subsystem import

**Chosen: lift `endpoint` module to `oci::endpoint` peer of `sign` and `verify`. Recommended.**

`oci::verify::error` currently imports `UrlRejection` from `oci::sign::endpoint`, creating verify → sign coupling on a shared primitive that is conceptually neither sign-specific nor verify-specific. Both pipelines validate Fulcio / Rekor / TSA URLs identically. Lifting to `oci::endpoint` removes the cross-subsystem dependency, hosts the validator in a single source of truth, and pre-empts the same leak when Slice 2 adds TSA endpoint validation. Documenting the dependency direction instead (alternative B) was rejected — it documents the structural defect rather than fixing it.

**Phase 5c impact:** Create `crates/ocx_lib/src/oci/endpoint.rs` (named module file per OCX convention), move `UrlRejection`, `validate_sigstore_url`, URL constants from `oci/sign/endpoint.rs`, update imports across `oci/sign/*` and `oci/verify/*`, delete `oci/sign/endpoint.rs`.

### Amendment 3 — `state/` tier in three-store architecture

**Chosen: document `state/` as legitimate fourth tier. Update `arch-principles.md` + `subsystem-file-structure.md`. Recommended (lowest churn).**

Capability cache is not content-addressed, not GC-relevant, and not user-visible — it has none of the properties that justify residence in `blobs/`, `layers/`, `packages/`, or `tags/`. The code already writes to `~/.ocx/state/referrers/<registry>.json` (per Codex finding #3 fix in same review pass); the ADR and rules simply need to acknowledge what exists. Relocating under `blobs/.cache/` (alternative) was rejected because it stretches `blobs/` semantics and requires GC code to learn an exception. The fourth-tier contract is: ephemeral runtime state, TTL-bound, not GC-walked, per-subsystem JSON schema.

**Phase 5c impact:** Add a thin `StateStore` wrapper at `crates/ocx_lib/src/file_structure/state_store.rs`, add the `state` field to `FileStructure`, update both rules with the doc-cross-ref text already inserted in this round. No data migration needed (nothing released yet). Rule updates already landed in this commit pass.

### Amendment 4 — Rekor v2 client tracking

**Chosen: track upstream and internal via dedicated GitHub issues; record URLs in ADR. Required before Phase 5c finalize.**

sigstore-rs Rekor v2 client support is not released as of 2026-05-14 (sigstore-rs 0.13.0, October 2024, is the latest; no 0.14 in progress; no Rekor v2 client work visible on the issue tracker). OCX v1 cannot verify Rekor-v2-only bundles. The existing `VerifyErrorKind::RekorSetAbsentTsaPresent → ExitCode::RekorUnavailable = 82` mapping is the operator's signal. The amendment adds concrete tracking: (a) file an upstream issue on `sigstore/sigstore-rs` so OCX appears on the v2 client-upgrade gate; (b) file an internal OCX issue referencing the upstream; (c) update release-notes template to state "OCX v1 verifies Rekor v1 bundles only." Both issue URLs are placeholders in the ADR pending stakeholder action.

**Open action:** File the two GitHub issues and record URLs in ADR Amendment 4 placeholders before Phase 5c `/finalize`.

**Phase 5c impact:** None to code. Two non-code gates (file issues, update release-notes template) on Phase 5c finalize checklist.

### Amendment 5 — TUF rotation SLA

**Chosen: Option (A) — embedded-only trust root + 90-day forced-upgrade window + nightly CI parity check. Recommended for v1. Document v2 path to Option (B).**

Two options: (A) embed the TUF root statically, document a 90-day SLA for OCX releases following any upstream root rotation, and run a nightly CI job that diffs the embedded asset against latest upstream and files a tracking issue on drift; (B) embedded as bootstrap plus `from_tuf()` runtime refresh under `state/trust_root/`. (A) wins because it preserves the offline-first principle (Product Principle 4) at verify time, keeps failure modes observable (`CertChainInvalid` exit 65 with "TUF root out of date" remediation, never silent acceptance), and makes the 90-day SLA enforceable via the CI parity check rather than aspirational. (B) is the v2 migration path; the `Signer` trait split already accommodates it. The 90-day window shortens to 7 days if upstream publishes a key rotation as a security advisory.

**Critical Phase 5c gate:** `EmbeddedAssetMissing` is the current production state per reviewer's evidence. Phase 5c must wire the embedded asset (`sigstore-trust-root = "=0.6.4"`) — replace the runtime check with a build-time compile-time include.

**Phase 5c impact:** (a) wire embedded asset; (b) add `.github/workflows/sigstore-trust-root-drift.yml` nightly cron with hidden subcommand `ocx internal print-embedded-trust-root --json`; (c) update release notes template with 90-day SLA line.

### Amendment 6 — Capability-cache TTL drift

**Chosen: reconcile to code reality. Flat 6h TTL for v1. CI / interactive split deferred to v2.**

Code: 6h flat. ADR / FR-17: 24h CI / 1h interactive. The split was specified pre-implementation and never built; the discrepancy has been Warn-tier across two review rounds. 6h flat is a reasonable middle (survives back-to-back CI runs in the same Actions runner; reflects registry capability flips within the same business day); the 1h interactive ceiling was over-engineered because `--no-cache` already covers the "fresh probe per session" use case. Deferring the split (vs deleting the v2 option) keeps the door open if telemetry later shows benefit.

**Phase 5c impact:** None to code. Update PRD `prd_oci_referrers_signing_v1.md` FR-17 and any acceptance-criteria mentions of the 24h/1h split to "flat 6h TTL; CI/interactive split deferred to v2."

### Amendment 7 — `Signer::sign` return shape

**Chosen: kind-only `Result<SignedBundle, SignErrorKind>` — current code wins.**

ADR: `SignError`. Code: `SignErrorKind`. Two options: (A) update ADR to match code (kind-only); (B) refactor code to match ADR (full error). (A) wins on YAGNI and convention: OCX's existing leaf traits (`IndexImpl`, `OciTransport`) return kind enums and let the surrounding facade compose into the three-layer error. Forcing `Signer` to be the exception complicates every future `Signer` impl with identifier-context plumbing it doesn't need. No information is lost — composition into `SignError` happens unconditionally one level up, where the pipeline already has the identifier. v2 multi-signer dispatch can wrap signers if pre-composed errors are ever needed at trait boundary.

**Phase 5c impact:** None to code. Update the §"Signer trait abstraction" code block in the ADR to match (`SignErrorKind` return); add a doc-comment on `Signer::sign` pointing to `quality-rust-errors.md`'s three-layer error pattern and this amendment.

### Amendment 8 — Token-provider boundary concerns (F2, F3): defer to v2

**Chosen: defer both F2 and F3 to v2. v1 token-provider machinery ships unchanged. v2 seam named: `AmbientProvider` instance dispatch + `DispatchingTokenProvider::builder()` pattern with provider injection points.**

Round-1 architect review flagged two boundary defects re-surfaced by round-3 as unaddressed by Amendments 1–7: (F2) `AmbientProvider::detect()` is a static-trait associated function, which blocks `Vec<Box<dyn AmbientProvider>>` heterogeneous dispatch and forces callers to name each concrete impl; (F3) `DispatchingTokenProvider::new(override_token, no_tty)` is a narrow positional constructor for what the docs frame as a four-path dispatch state machine, growing positionally with every future input. Both concerns are real and acknowledged. v1 has only two ambient backends with a fixed dispatch order and only two constructor inputs, so neither defect is actively blocking a v1 contract; both refactors are non-trivial trait-surface / API-shape changes whose blast radius would multiply Phase 5c's change set without unlocking a v1 deliverable; concrete second callers haven't surfaced. YAGNI argues against speculating on the abstraction shape without a real second consumer. The amendment converts what round-3 called a "silent defer" into a documented, dated defer with a named v2 seam.

**Phase 5c impact:** none. Current `DispatchingTokenProvider::new(override_token, no_tty)` and `AmbientProvider::detect()` continue to work for v1. v2 design work tracked separately and re-opened as a v2 ADR amendment when a concrete second backend or extension materialises.

## Cross-impact on Phase 5c plan

| Phase 5c work item | Affected by amendment(s) |
|---|---|
| Sign-pipeline implementation | 1 (delete `SignContext`), 2 (import path change), 7 (no change, just doc) |
| Verify-pipeline implementation | 1 (mirror with `VerifyPipeline::run`), 2 (import path change) |
| `Client` API surface | 1 (`transport()` demoted to `pub(crate)`) |
| Trust-root loading | 5 (wire embedded asset; remove `EmbeddedAssetMissing` from production path) |
| Capability cache | 3 (moves under `StateStore`), 6 (TTL stays 6h flat; PRD FR-17 update) |
| File structure module | 3 (new `state_store.rs`; `state` field on `FileStructure`) |
| CI workflows | 5 (new `sigstore-trust-root-drift.yml` nightly) |
| GitHub issue hygiene | 4 (two tracking issues, URLs to record in ADR) |
| Release notes template | 4 (Rekor v1 note), 5 (90-day SLA line) |
| PRD update | 6 (FR-17 text) |
| ADR self-reference | 7 (update `Signer::sign` signature in code block) |

## Open stakeholder questions

1. **Amendment 1 — Option (3) vs Option (2)?** Recommendation is Option (3) on architectural grounds. Option (2) is acceptable if schedule pressure on Phase 5c is real.
2. **Amendment 4 — Who files the two GitHub issues, and when?** OCX core maintainer owns; should land before Phase 5c `/finalize`.

## Memory / positioning impact

None of these amendments reframe OCX's product positioning. Product principle "Supply-chain-ready out of the box" (differentiator #9 in `product-context.md`) is unchanged — the amendments harden the implementation (boundary clarity, trust-root SLA, fourth-tier acknowledgement) without changing the user-facing supply-chain story. No `MEMORY.md` insight update needed.
