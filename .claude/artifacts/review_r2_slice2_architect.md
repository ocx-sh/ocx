# Review R2 — Slice 2 Architecture (Re-review)

**Verdict:** PASS-WITH-ACTIONABLE
**Date:** 2026-04-19
**Scope:** Verification of R1 findings F1–F8 only. No new findings outside R1 scope.

## Summary

Six of eight R1 findings are fully addressed. F1 is PARTIAL (the Step 3.1 unit-test enumeration still lists only seven tests; the eighth only appears in the downstream test matrix). **F6 is UNRESOLVED** — the PR-FAQ still contains the exact incorrect "exit 77 when the Fulcio cert chain or Rekor SET fails" sentence that R1 flagged. F6 must be fixed before landing because the PR-FAQ is the customer-facing exit-code contract.

---

## Per-finding verdicts

### F1 — `MAX_REFERRER_WALK = 50` bound — PARTIAL

- **ADR constant + truncation semantics:** ADDRESSED. ADR L120: *"The manifest walk is hard-capped at `MAX_REFERRER_WALK = 50` descriptors per subject … OCX takes the first 50 in list order and emits a warning via `tracing::warn!` (not an error)."*
- **Plan Step 3.1 eighth test:** PARTIAL. Step 3.1 (plan L569–578) enumerates seven tests only: `branch_1_fresh_cache_hit…`, `branch_2…`, `branch_3…`, `branch_4a…`, `branch_4b…`, `offline_plus_cache_miss_returns_exit_81`, `no_cache_bypasses_both_caches`. The required `manifest_walk_truncates_at_max_referrers` appears only in the Test Matrix (L989) and Acceptance Test Matrix (L1014), not as an explicit Step 3.1 test bullet. R1 required it as "the eighth test" in the Step 3.1 enumeration. Action: add the explicit `manifest_walk_truncates_at_max_referrers` bullet under Step 3.1 so the implementer who works off the step list does not miss it.

### F2 — CI TTL false-positive on containerised dev — ADDRESSED

- ADR §S2-B rationale L136–141 adds an explicit "CI TTL false-positive note (Architect F2)" block covering self-hosted-runner persistence and local `CI=true` leakage, with the `--no-cache` override documented in both `ocx sbom --help` and `ocx verify --help`. The user-guide snippet is called out at L138 ("Doc this in the user guide's 'CI caveats' section"). The disclosure R1 required is present on all three surfaces.
- Note: R1 specifically asked for a *Risks-table row* in addition to the narrative; the note lives in the §S2-B rationale rather than the Risks table. Functionally equivalent — the disclosure is visible where readers looking at cache TTL land first — and the Risks table at L519–535 already covers the related "registry adds Referrers API within 24 h window" case. Accepted as ADDRESSED.

### F3 — SBOM trust gap disclosure (three surfaces) — ADDRESSED

All three surfaces carry the disclosure with traceable `Architect F3 mention #N of 3` markers:
1. ADR §Context L59 ("mention #1 of 3") — full trust-gap narrative.
2. ADR §`ocx sbom` CLI surface L411–412 ("mention #2 of 3") — help-text preamble quoted verbatim.
3. ADR §Risks table L529 ("mention #3 of 3") — High-severity row with v3+ `ocx sbom --verify` tracked.

PR-FAQ L181 ("What is `ocx sbom`?") also includes *"it does not verify signatures on the SBOM itself (that's `ocx verify`'s job)"*. The wording is lighter than R1's suggested template but discloses the gap.

### F4 — S2-E precedence when multiple formats valid — ADDRESSED

ADR §Decision S2-E L266–274 adds a numbered normative block "Precedence when multiple valid signature formats exist (Architect F4)" stating: run full pipeline against *every* candidate (no short-circuit), SUCCESS if *any* passes, JSON emits all successful candidates under `signatures[]` in referrer-list order, plain text shows first-match with v0.3-before-legacy *display* order, and "format is not a ranking axis for correctness — only for display order when multiple equally-valid signatures exist." This matches R1's required answer shape exactly.

### F5 — spdx-rs vendor/fork contingency — ADDRESSED

ADR §S2-C L181–187 adds a three-tier escape-hatch ladder: (1) `serde-spdx` as pre-selected drop-in; (2) vendor the 2023-11-27 snapshot under `external/spdx-rs/`; (3) write a ~200-line minimal deserialiser. Risks table L530 mirrors the contingency as a standalone Medium-severity row. The merge with Researcher R3 (commit-hash pin at 2023-11-27) is reflected in the amendment log.

### F6 — PR-FAQ exit-code error — UNRESOLVED

PR-FAQ L254 still reads: *"exits non-zero with a typed BSD sysexits.h code describing the cause — for example **exit 77 when the Fulcio cert chain or Rekor SET fails**, exit 80 for `--certificate-identity` mismatch, exit 81 for `OCX_OFFLINE` with cold cache, exit 82 for transient Rekor unavailability."* This is the exact wrong sentence R1 flagged. Cert-chain failure is exit 65 (`DataError`); Rekor unavailability is exit 82, not 77. L207 separately states "Slice 2 adds zero new exit-code variants … (Architect F6)" — that inline aside notes the finding but does *not* fix the wrong customer-facing sentence at L254. **Required fix before merge:** replace L254 with the R1-suggested sentence ("exit 65 … exit 80 … exit 82 … exit 79 …").

### F7 — Remove `SbomSummaryReport.signature_format` — ADDRESSED

Plan Step 1.11 L492–507 now shows `SbomSummaryReport` with `signature_format` absent and an inline comment L496–498: *"`signature_format` removed (Architect F7 + Spec F8). `ocx sbom` does not verify signatures, so reporting a signature format would mislead consumers."* Test matrix L997 re-asserts **"no `signature_format` field"**. ADR L264 scope-notes the field as `ocx verify`-only. Merge with Spec F8 is explicit.

### F8 — Cache algorithm branch reconciliation — ADDRESSED

ADR §Cache algorithm L293–338 reshapes the pseudocode so the `OfflineBlocked` guard is inlined in each network-crossing branch (Branch 2, Branch 3, Branch 4). L340 adds the "Branch-label reconciliation (Architect F8)" paragraph: *"The original draft double-labelled the offline-cache-miss as both 'Branch 4 guard' and 'Branch 5 return', which was unreachable-by-construction. This rewrite folds the `OfflineBlocked` guard into each reachable branch … no dead code paths remain."* Branch inventory L342–352 enumerates nine reachable test cases mapping 1:1 to reachable paths; the former "Branch 5" is now test case 8 labelled "equivalent to the deleted 'Branch 5'". Exit-code table note at L475 also aligns.

---

## Verdict

**PASS-WITH-ACTIONABLE.** Six findings fully addressed. F1 PARTIAL (add the missing eighth bullet under Step 3.1; the matrix coverage is informative but not actionable for an implementer reading the step list). **F6 UNRESOLVED and must be fixed** — the PR-FAQ's customer-facing exit-code sentence is still wrong. Both are small edits; no architectural rework required. No findings outside R1 scope.

## Required actions before merge

1. PR-FAQ L254 — replace the `exit 77 when the Fulcio cert chain or Rekor SET fails` sentence with the R1-prescribed 65/80/82/79 mapping.
2. Plan L569 Step 3.1 — add explicit test bullet `manifest_walk_truncates_at_max_referrers` (the behaviour is already specified at L989/L1014).
