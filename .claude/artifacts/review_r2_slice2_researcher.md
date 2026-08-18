# Review R2 — Slice 2 SOTA-Gap Re-Review

**Verdict: PASS-WITH-ACTIONABLE (2 new deferred findings)**
**Date:** 2026-04-19
**Reviewer:** worker-researcher (sonnet)

---

## Per-Finding Verdicts (R1 actionables)

### R1 — Referrers API registry compat matrix — ADDRESSED

- Registry compat table in ADR (lines 612–636) enumerates GHCR, Docker Hub, ECR, ACR, Harbor 2.10+, Zot, registry:2 with Referrers-API and tag-fallback support columns, citation links, and last-observed dates.
- Tag-fallback rows call out the `sha256-<digest>.sig` / `sha256-<digest>.att` conventions per referrer type.
- Plan Step 2.3 (line 231) names the `RegistryCaps` probe that is cached per-(registry, repo) tuple and degrades to fallback silently.

### R2 — `.att` tag convention for SBOM-on-registries-without-referrers — ADDRESSED

- ADR S2-C (line 311): "`.att` tag convention (cosign attest / syft attest) is the de facto SBOM fallback on non-Referrers-API registries."
- Plan line 573: `cache_method` enumeration now includes `"att-tag"` alongside `"referrers-api"` and `"sig-tag"`.
- Step 3.1 test matrix (line 579): new 8th bullet `manifest_walk_truncates_at_max_referrers` confirms the defensive `MAX_REFERRER_WALK = 50` cap.

### R3 — CycloneDX specVersion pre-parse guard — ADDRESSED

- ADR line 344: "CycloneDX specVersion is pre-parsed from JSON before handing to `serde-cyclonedx`. Only 1.3, 1.4, 1.5 accepted in v1." Explicit rationale: avoids opaque serde errors and makes the 1.7+ upgrade a one-line constant change.
- Plan Step 3.5 lists `cyclonedx_version_rejects_1_7` test case.

### R4 — spdx-rs unmaintained contingency — ADDRESSED

- ADR risks table row: "`spdx-rs` unmaintained since 2023-11-27" with mitigation "v1 ships with `spdx-rs` pinned at last known-good; contingency: `serde-spdx` 0.10 (maintained, API-compatible enough for our read-only subset)."
- Plan Step 3.6 references both crates; the fallback path is spelled out in a code comment sketch.

### R5 — Referrer walk DOS defense — ADDRESSED

- ADR Risks row: "Maliciously populated referrer chains → walk explosion" | mitigation: `const MAX_REFERRER_WALK: usize = 50;` with exit `ReferrersUnsupported = 83` on truncation, surfaced as a warning in human mode and an error-kind in JSON mode.
- Plan Step 3.1 test `manifest_walk_truncates_at_max_referrers` confirms coverage.

### R6 — CycloneDX 2.0 forward-compatibility hook — ADDRESSED

- ADR line 352: "`CycloneDxSpecVersion` accepts `1.3 | 1.4 | 1.5` today; when 2.0 ships (upstream milestone June 30, 2026) the accepted set becomes a single constant edit."
- PR-FAQ (line 254): "What about CycloneDX 2.0?" answer confirms forward plan and exit-code stability.

---

## New Deferred Findings (2026-Q2 trend scouting)

### ND-1 — CycloneDX 2.0 "Transparency Exchange Language" milestone due June 30, 2026

OWASP CycloneDX public roadmap shows the 2.0 release (rebranded "Transparency Exchange Language") scheduled for **June 30, 2026**. This is ~10 weeks earlier than the prior ADR assumed (previously characterized as "late 2026"). The specVersion pre-parse guard (R3) means the code change is trivial — add `"2.0"` to the accepted set — but the *product* implication is worth flagging: SBOM tooling (Syft, Grype, Trivy) is already advertising 2.0 support targets in their 2026 roadmaps, which means SBOMs with `"specVersion": "2.0"` may appear in the wild before OCX v1 ships. Handoff-only — do not alter Slice 2 scope. Recommend: post-v1 follow-up ticket to add 2.0 to the accepted set as soon as the spec stabilizes.

### ND-2 — CVE-2026-24122 cosign expired intermediate cert bypass

A new CVE surfaced 2026-04-02 (CVSS 7.1) against cosign < 3.0.7 where an expired Fulcio intermediate cert can be presented as valid if the verifier doesn't check `NotAfter` on the intermediate (only on the leaf). OCX's verify path uses sigstore-rs 0.13 — which *does* check intermediate expiry — so this CVE is not directly exploitable against OCX. However, the `cosign verify` *interop* test (FR-15) uses the cosign binary, so the conftest `cosign_binary` pin must be bumped to `>= 3.0.7` before the interop test lands. Handoff-only — file as a Slice 2 pre-execute task.

Sources:
- [OWASP CycloneDX 2.0 roadmap](https://cyclonedx.org/roadmap/) — 2.0 milestone June 30, 2026
- [GHSA-q8qw-8m2q-6h24](https://github.com/sigstore/cosign/security/advisories/) — CVE-2026-24122 cosign intermediate expiry bypass
- [Syft 1.22 release notes](https://github.com/anchore/syft/releases) — advertises CycloneDX 2.0 target
- [Trivy 0.60 roadmap](https://github.com/aquasecurity/trivy/discussions) — CycloneDX 2.0 as Q3 2026 feature

---

## Summary

All six R1 findings fully addressed. Two new handoff-only deferred findings from 2026-Q2 trend scouting — neither requires Slice 2 scope changes; both are pre-execute reminders. Plan is consistent with April 2026 SBOM and referrers ecosystem state.
