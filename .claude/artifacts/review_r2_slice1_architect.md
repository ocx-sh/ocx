# Review R2 — Slice 1 Architecture (Verification Pass)

**Verdict:** PASS-WITH-ACTIONABLE
**Date:** 2026-04-19
**Round:** 2 (verifies R1 findings F1–F8 against batch-fix pass)

---

## Summary

Six of eight R1 findings are fully addressed. Two remain partial: F2 has a residual contradictory sentence in the Forward-Compat section, and F5 is inconsistently propagated to plan Steps 2.1 and 3.8, which still reference the pre-collapse `signing_context` / `verify_context` names. No architectural regressions.

---

## Per-Finding Verification

### F1 — Fallback-tag asymmetry (ADR §S1-F) — **ADDRESSED**

ADR line 179 now calls out hard-error exit 83 for GHCR: "hard error: `ReferrersUnsupported` → exit 83." Line 183 steelmans and rejects write-side fallback: "we need the hard-error path to force registries to adopt Referrers API rather than papering over the gap." The Slice-2-read-≠-Slice-3-write distinction is stated: "Slice 2 will still *read* legacy tag-based signatures other tools have written; `ocx package sign` only ever produces v0.3 referrers." Enforcement added via test-tape assertion (lines 175, 183). All three R1 required sub-points present.

### F2 — TokenProvider / Signer seam — **PARTIAL**

`Signer` trait introduced in ADR §"`Signer` trait abstraction (Architect F2)" (lines 311–347). `TokenProvider` correctly narrowed (lines 341–345). The R1-offending sentence "KMS signers plug in at the Fulcio step as a sibling abstraction" is **still present verbatim at line 722** in the §Forward-Compat Hooks for v2 bullet. This directly contradicts the new `Signer` trait paragraph and was explicitly named for removal in R1.

**Still required:** Rewrite ADR line 722 to reference the `Signer` trait — e.g., "HSM/KMS signers implement the `Signer` trait directly; the v1 `TokenProvider` abstraction is Fulcio-keyless-specific and is not reused by non-OIDC signers."

### F3 — CI-hostile happy-path test — **ADDRESSED**

Plan Step 3.10 (line 385) gates the live-Sigstore happy path behind `OCX_TEST_SIGSTORE_STAGING=1` with graceful skip, and Gate 3 (line 403) exempts only the "staging tier" from no-skip rule. Step 3.11 (lines 388–395) adds fake_registry / fake_fulcio / fake_rekor helpers with tape-based assertions including S1-F referrer-shape enforcement ("no fallback tag write occurred"). This satisfies R1 Option A (fake services) plus the fixture-driven shape check in Option B. A renamed `test_sign_referrer_shape_from_fixture` does not exist under that literal name, but Step 3.11's tape-assertion mechanism fulfills the intent. Acceptable.

### F4 — `ReferrersUnsupported = 83` — **ADDRESSED**

Canonical enum at `.claude/rules/quality-rust-exit_codes.md` lines 85–88 adds `ReferrersUnsupported = 83` with accurate semantics (registry capability gap, hard error). Script case-statement updated at line 201. ADR exit-code table row 83 (line 247) is present with full remediation text. `ClientError::ReferrersUnsupported` variant at ADR line 388; classifier impl at lines 504–517 routes `SignErrorKind::ReferrersUnsupported` → `ExitCode::ReferrersUnsupported`. Plan Step 3.9 (lines 357, 369) asserts the mapping for both sign and verify kinds. Full coverage.

### F5 — Collapse of `signing_context()` + `verify_context()` — **PARTIAL**

ADR Architecture section (lines 350–362) correctly collapses to a single `online_context()` accessor with explicit YAGNI rationale. Plan Step 4.13 (line 448) and Step 1.7 (lines 184–186) use `online_context`. File map line 103 and table line 535 cite `online_context`.

However, **plan Step 2.1 line 276** still lists the review checklist item as "`signing_context` and `verify_context` return both `&Index` and `&Client`?", and **plan Step 3.8 lines 346–348** still names the unit test as "`Context::signing_context` + `verify_context`" with a case "signing_context in offline mode is rejected." These are the pre-collapse names and will confuse Phase 2/3 reviewers.

**Still required:** Rename plan Step 2.1 bullet and Step 3.8 header/body to `online_context`.

### F6 — SignErrorKind / VerifyErrorKind justification — **ADDRESSED**

ADR §"`SignErrorKind` and `VerifyErrorKind` — variant inventory & justification" (lines 394–491) now states the verb-specificity rationale explicitly: "Every new kind below is justified by a distinct user-facing remediation *and* a distinct exit code. Variants that would map to identical remediation + exit code are merged." The "Mergers rejected" block at line 491 names the two kept-separate pairs with reasons (IdentityMismatch vs IssuerMismatch; RekorSetInvalid vs RekorSetAbsentTsaPresent). Plan Step 3.9 (lines 350–373) makes variant enumeration explicit with per-variant assertions and a structural test iterating `ALL_SIGN_ERROR_KINDS` / `ALL_VERIFY_ERROR_KINDS` slices. Full coverage.

### F7 — ErrorEnvelope / ClassifyErrorKind trait in ocx_lib — **ADDRESSED**

ADR §"`ClassifyErrorKind` trait (Architect F7)" (lines 493–522) defines the trait at `crates/ocx_lib/src/cli/classify.rs` with exhaustive `match` impls for both kind enums. The rationale ("unit tests assert every kind has a mapping... adding a new kind forces the match to be updated (exhaustive match compile error)") directly addresses the R1 compile-time-enforcement requirement. Plan Step 3.9 (line 351) routes tests to `crates/ocx_lib/src/oci/sign/error.rs` and `verify/error.rs` for the trait tests; Step 4.16 wires the envelope. `error_envelope.rs` stays in `ocx_cli` but now calls the trait rather than pattern-matching raw variants. Full coverage.

### F8 — sigstore-rs 0.14 bundle regression — **ADDRESSED**

ADR Risks table adds row at line 714: "**sigstore-rs 0.14 upgrade path (Architect F8)**" with pin discipline (`# pinned — see adr_oci_referrers_signing_v1.md Risks` comment), re-evaluation protocol, and reference to the `Signer` trait seam. Plan Step 3.12 (lines 397–399) requires the fixture runbook to include `cosign verify` as a validation step. The golden-fixture regeneration gate is present. Full coverage.

---

## Residual Actionable Items

1. **F2 cleanup** — delete or rewrite ADR line 722 to reference the `Signer` trait, not a non-existent "sibling abstraction at the Fulcio step."
2. **F5 propagation** — rename `signing_context` / `verify_context` to `online_context` in plan Step 2.1 line 276 and Step 3.8 lines 346–348.

Both are text-only edits, no architectural change.

---

## No New Findings

This was a verification pass. No fresh review surface area was examined.
