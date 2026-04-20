# Codex Cross-Model Plan Review — Slice 1

**Target:** `.claude/state/plans/plan_slice1_sign_and_verify.md`
**Reviewer:** Codex (via `codex-companion.mjs task --effort high`)
**Date:** 2026-04-19
**Verdict:** **BLOCK**
**Duration:** ~6m
**Codex session:** `019da7af-2ea6-7001-a8e7-79da51339eab`

Codex ran as a one-shot adversarial gate after the in-family Review-Fix Loop converged PASS. The plan is not ready for `/swarm-execute` — 5 actionable blockers. The 2026 ecosystem assumptions were spot-checked and mostly sound (cosign `v3.0.6` floor for `GHSA-w6c6-c85g-mmv6`, Rekor v1 maintenance / v2 TSA transition, `sigstore-rs 0.13.0` tip).

Summary: 5 Actionable · 1 Deferred · 2 Trivia dropped.

---

## Actionable findings

### C-S1-1: JSON Schema Is Frozen In Two Incompatible Shapes

Citations: `plan:249`, `plan:263`, `plan:361`, `plan:399`, `plan:647`, `adr:524`, `adr:591`, `prd:89`.

The plan locks three mutually incompatible JSON contracts at once:
- Steps 1.10/3.7 flatten `context` into top-level keys
- The ADR freezes `context` as a nested object
- Step 3.10 expects verify success as `verified: [...]`
- Acceptance table / ADR success shape expect `data.{...}`

Because Phase 3 says tests are written from ADR + PRD, this is not just doc drift; it makes Gate 3 non-deterministic. **Freeze one v1 schema now** and update plan, ADR, and PRD in one pass before execution.

### C-S1-2: `VerifyErrorKind` And Exit-Code Contracts Conflict Inside The Plan

Citations: `plan:186`, `plan:349`, `plan:374`, `plan:399`, `plan:648`, `adr:443`, `prd:330`.

- Step 1.5 says `VerifyErrorKind` follows ADR inventory, but Step 3.6 uses different variant names (`CertificateIdentityMismatch`, `CertificateOidcIssuerMismatch`, `MalformedBundle`, `RekorSetMissing`) than Step 3.9 and the ADR (`IdentityMismatch`, `IssuerMismatch`, `BundleParseFailed`, `RekorSetInvalid`).
- Exit-code contract also splits: Step 3.10 says identity mismatch exits `80`, later acceptance says `77`, ADR inventory says `77`.

A contract-first plan cannot have two incompatible enums and two incompatible exit codes for the same scenario. Collapse to one variant inventory and regenerate every test/table from it.

### C-S1-3: The Fake Fulcio/Rekor Strategy Is Not Runnable With The Planned Seams

Citations: `plan:174`, `plan:188`, `plan:308`, `plan:417`, `plan:456`, `plan:465`.

- Acceptance plan requires `fake_fulcio` / `fake_rekor` to drive real CLI tests, but the plan never introduces a service-URL injection seam for signing; verify only gets a trust root after the CLI layer.
- Phase 3 says `fake_fulcio` / `fake_rekor` start as empty skeletons, while Step 3.11 already depends on them returning 401/403/503 and issuing test certs.
- Internal contract split: `fixtures.toml` in Step 3.0 vs YAML tape in Step 3.11.

Gate 3 cannot be run end-to-end as written. Add explicit test-only endpoint and trust-root injection; fully specify one helper protocol before launch.

### C-S1-4: `--identity-token` Is A Bad Public One-Way Door

Citations: `plan:213`, `prd:153`.

Publishing a raw bearer token on argv is a security footgun: leaks through process listings, shell history, CI debug surfaces. This is exactly the kind of CLI surface that is hard to retract once documented. Keep the override, but change the shape before v1 ships: **env var, file, or stdin** are defensible; a public raw-token flag is not.

### C-S1-5: Canonical Exit-Code Alignment Is Not Actually Aligned Yet

Citations: `plan:695`, `plan:391`, `plan:648`, `quality-rust-exit_codes.md:80`.

The plan claims the canonical rule file was updated, but the current rule still describes `RekorUnavailable = 82` as "signing path only," while this plan uses `82` on verify too. The plan's verify-side use of `82` is correct; the blocker is that the supposed source of truth still disagrees. **Reconcile the rule file first**, then keep plan/ADR/PRD in lockstep.

---

## Deferred

### C-S1-D: `ambient-id` Dependency Note Is Stale

Citation: `plan:598`.

Dependency table says `ambient-id` is "latest 0.1.x" and implies first-class support across GHA/GitLab/CircleCI/Buildkite/GCP. Current public docs show `ambient-id` still in the `0.0.x` line and explicitly document GHA/GitLab/Buildkite support ([docs.rs](https://docs.rs/ambient-id)). Inline fallback is already planned, so not a blocker — but the dep note should be corrected before handoff so `deps` pass doesn't start from a false premise.

---

## Stated-convention / trivia dropped

2. The cosign `>=3.0.6` floor and the Rekor v2/TSA risk callout both look correct as written.
