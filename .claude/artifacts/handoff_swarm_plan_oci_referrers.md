# Handoff — `/swarm-plan max 24` — OCI Referrers Discovery

**Issue:** [ocx-sh/ocx#24 — feat: OCI referrers API for signature and SBOM discovery](https://github.com/ocx-sh/ocx/issues/24)
**Tier:** `max` (explicit)
**Date:** 2026-04-19
**Status:** Planning complete. Two independent slices ready for `/swarm-execute`.

This supersedes the earlier single-slice handoff. Scope was expanded at planning time (architect + user pushback on read-only MVI at 2026-04-19) from read-only discovery to a full keyless sign + verify loop for OCX, plus external-artifact read-side. The two slices are independent and can execute in either order.

---

## Scope decision — two slices

| Slice | Scope | Ships |
|---|---|---|
| **Slice 1** | `ocx package sign` + `ocx verify <OCX bundle>` | cosign keyless, Fulcio v2, Rekor v1, Sigstore Bundle v0.3 |
| **Slice 2** | `ocx verify <external ref>` + `ocx sbom <ref>` | OCI v1.1 Referrers API + `sha256-<digest>.sig` / `.att` fallback tag probe, CycloneDX 1.3/1.4/1.5 + SPDX 2.3 |

Both slices are deliverable features — each ships a user-facing capability on its own.

---

## Execute these plans next

```sh
/swarm-execute .claude/state/plans/plan_slice1_sign_and_verify.md
/swarm-execute .claude/state/plans/plan_slice2_external_discovery.md
```

Order flexible. Slice 1 first is the better dogfood story (OCX signs its own releases; enables FR-15 cosign interop); Slice 2 first is the better supply-chain consumer story.

---

## Artifacts produced

### Research (Phase 2 — 3 parallel workers, mandatory at max)

- `research_cosign_sigstore_notation.md` — cosign 2026 state, Notation+Rust, sigstore-rs 0.13 pin rationale, rekor-rs, TUF
- `research_verify_cli_patterns.md` — peer UX survey (cosign/notation/oras/crane), trust-policy patterns, JSON envelope conventions
- `research_oci_referrers_2026.md` — OCI Distribution Spec v1.1 referrers endpoint, fallback tag schemes, 2026 registry compat matrix
- `research_oidc_cli_flows.md` — OIDC ambient detection (ambient-id), laptop TTY flow, token-override patterns

### Design (Phase 3 — opus architect, max-tier mandatory PRD + PR-FAQ)

Slice 1:
- `adr_oci_referrers_signing_v1.md`
- `prd_oci_referrers_signing_v1.md`
- `pr_faq_oci_referrers_signing_v1.md`
- `.claude/state/plans/plan_slice1_sign_and_verify.md` — **execute this**

Slice 2:
- `adr_oci_referrers_discovery_v2.md`
- `prd_oci_referrers_discovery.md`
- `pr_faq_oci_referrers_discovery.md`
- `.claude/state/plans/plan_slice2_external_discovery.md` — **execute this**

### Reviews (Phase 4 in-family + Phase 5 Codex cross-model)

Round 1 (3 reviewers/slice):
- `review_r1_slice1_{architect,spec_compliance,researcher}.md`
- `review_r1_slice2_{architect,spec_compliance,researcher}.md`

Round 2:
- `review_r2_slice1_{architect,spec_compliance,researcher}.md`
- `review_r2_slice2_{architect,spec_compliance,researcher}.md`

Phase 5 Codex cross-model (BLOCK on first pass; fixed + spec-recheck PASS):
- `codex_review_slice1_plan.md`
- `codex_review_slice2_plan.md`

### Rule updates

- `.claude/rules/quality-rust-exit_codes.md` — `RekorUnavailable = 82` docstring now covers both sign path (Rekor upload failure) AND verify path (SET absent + TSA absent, or Rekor 5xx/timeout). `ReferrersUnsupported = 83` variant added for registries lacking OCI Distribution Spec v1.1 Referrers API.

---

## Review loop outcome

**Converged. Both plans PASS after a Codex-driven fix pass + targeted spec-compliance recheck.**

- **Phase 4 (in-family):** 2 rounds per slice, 6 reviewers per round. All actionable items resolved; residuals cleared in a targeted cleanup pass.
- **Phase 5 (Codex, max-tier mandatory):** returned **BLOCK** on both slices — 10 substantive contract inconsistencies. All 10 applied in a single architect fix sweep. Spec-compliance re-run on both slices returned PASS-WITH-ACTIONABLE; 2 small residuals (PRD exit-code drift in Slice 1, ADR §Context-injection signature in Slice 2) fixed inline.
- **Codex adapter note:** `codex-companion 1.0.3` dropped the `plan-artifact` scope on `adversarial-review`. Substituted `task --effort high --fresh` mode with an adversarial prompt. If a future Codex version restores `plan-artifact`, prefer it — semantically equivalent outcome here, just via a different subcommand.

### Codex fixes applied

| ID | Finding | Slice | Resolution |
|---|---|---|---|
| C-S1-1 | JSON schema frozen in 3 incompatible shapes | 1 | Canonical nested-envelope `{schema_version, data\|error}` across plan/ADR/PRD; `data.signatures[]` success shape |
| C-S1-2 | `VerifyErrorKind` variant names + 77-vs-80 exit conflict | 1 | ADR inventory canonical; identity mismatch exits 77 everywhere (PRD S10+FR-10+S7+FR-2 fixed inline) |
| C-S1-3 | `fake_fulcio`/`fake_rekor` not runnable with planned seams | 1 | Added `fulcio_url`/`rekor_url`/`trust_root` injection to `SignContext`/`VerifyContext`; single `fake_sigstore.toml` protocol |
| C-S1-4 | `--identity-token` raw-argv security footgun | 1 | Replaced with `--identity-token-file`, `--identity-token-stdin`, `OCX_IDENTITY_TOKEN` env var |
| C-S1-5 | Canonical exit-code rule stale | 1 | `quality-rust-exit_codes.md:80` updated for dual sign+verify use of `RekorUnavailable = 82` |
| C-S2-1 | Offline-cache contract self-contradictory | 2 | `sbom_context()` returns `Option<&Client>`, never errors on `remote_client=None`; cold cache exits 81 `OfflineBlocked` (ADR §Context-injection fixed inline) |
| C-S2-2 | Legacy `.sig`/`.att` tag probe specified 3 ways | 2 | Frozen: `sha256-<digest>.{sig,att}` for BOTH tag- and digest-addressed subjects; `AttTag` method in `ReferrerDiscoveryMethod` |
| C-S2-3 | Exit-83 + cache-corruption contract not locked by tests | 2 | Added `test_referrers_unsupported_registry_exits_83` (verify + sbom); cache corruption is debug-only in release |
| C-S2-4 | SBOM parser not executable (CycloneDX fallback, SPDX "TBD", in-toto unbounded) | 2 | `CycloneDxSpecVersion` typed enum; `spdx-rs = "=0.5.5"`; `MAX_INTOTO_DEPTH = 4`; NIST SPDX fixture |
| C-S2-5 | Mixed-format `VerifyResult` missing `signatures[]` | 2 | `VerifyResult = { signature_count, signatures: Vec<VerifyCandidate> }`; scalar fields removed |

---

## Deferred findings (handoff only — not blockers)

### Slice 1

- **D-R2-S1-1 — Rekor v2 TUF distribution timing.** Rekor v2 log public key is in TUF as of Q4 2025, but log-URL distribution via TUF was "a couple months away" in Oct 2025 with no April 2026 confirmation. Mitigation (pin `sigstore = "=0.13"`, warn on `RekorSetAbsentTsaPresent`) is correct. **Recommended:** pre-release smoke test against the v2 log before cutting v1.
- **D-R2-S1-2 — cosign v4 cleanup announced.** cosign 3.0.6 is current tip; few v3 releases before v4 removes deprecated flags. `>= 3.0.6` pin survives through v4 as long as `cosign verify` flag surface stays stable.
- **D-Codex-S1-1 — `ambient-id` 0.0.x.** Plan dep table says "latest 0.1.x"; Codex notes it's still `0.0.x`. Inline fallback is planned regardless; `/swarm-execute`'s deps pass should re-pin at execute time.

### Slice 2

- **D-R2-S2-1 — CycloneDX 2.0 milestone 2026-06-30.** OWASP CycloneDX public roadmap shows 2.0 ("Transparency Exchange Language") scheduled for 2026-06-30 — earlier than the ADR assumed. `CycloneDxSpecVersion` pre-parse guard means the code change is a one-line constant edit when it ships. **Recommended:** post-v1 follow-up ticket to add 2.0 to the accepted set.
- **D-R2-S2-2 — CVE-2026-24122 cosign expired intermediate bypass (< 3.0.7).** Not directly exploitable against OCX (sigstore-rs 0.13 checks intermediate `NotAfter`), but the FR-15 cosign-interop test uses the cosign binary — **must bump conftest `cosign_binary` pin to `>= 3.0.7`** before the interop test lands in `/swarm-execute`.

---

## Three issue-open-questions answered in the ADRs

| # | Question | Resolution | ADR section |
|---|---|---|---|
| 1 | Should `ocx install` auto-verify signatures if a policy is configured? | **Deferred to Slice 3.** v1 ships `verify` as an explicit command; auto-verify-on-install is a follow-up. Current composition: `ocx verify && ocx install`. | Slice 2 S2-F |
| 2 | Which signature formats ship first — cosign keyless, notation, both? | **cosign keyless only for v1.** Notation has no Rust lib; `Signer` trait in Slice 1 keeps the door open. | Slice 1 S1-D |
| 3 | Registries without Referrers API — revisit "no fallback" stance? | **Split:** read-side fallback YES (`sha256-<digest>.{sig,att}` probe); write-side fallback NO (signing exits 83). | Slice 2 S2-A + Slice 1 S1-F |

---

## Cost

| Dimension | Value |
|---|---|
| Peak parallel workers | 4 (Discover phase) |
| Total worker launches | ~19 across all phases |
| Heaviest calls | Opus architect during Design (synthesis across 4 research + 2 prior ADRs) and the Codex-fix architect (10 blockers, single sweep) |
| Codex presence | 2 one-shot adversarial plan reviews (one per slice); 2 spec-recheck re-runs |
| Deferred findings | 5 total (3 Slice 1, 2 Slice 2) — all handoff-only, none block execute |

---

## Not done (explicitly out of scope)

- No implementation, tests, or source edits — `/swarm-plan` only plans. Source changes come in `/swarm-execute`.
- No commits, PRs, pushes.
- No trust-policy DSL implementation (Slice 3 or later).
- No auto-verify-on-install integration (Slice 3 or later).
- No signing of non-OCX artifacts (third-party `cosign sign` remains the only way to add signatures OCX didn't emit).
- No Notation / SPIFFE / X.509 PKI signatures (v1 is keyless-only).
- No DSSE signing (sigstore-rs 0.13 does not support it; no upstream PR in flight).
