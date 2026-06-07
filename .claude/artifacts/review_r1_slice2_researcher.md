# Review R1 — Slice 2 SOTA Gap

**Verdict:** PASS-WITH-ACTIONABLE (6 Actionable, 6 Deferred)
**Date:** 2026-04-19
**Reviewer:** worker-researcher (sonnet) — tracking 2025-Q4 through 2026-Q2 OCI + SBOM ecosystem signals.

## Summary

The Slice 2 design is well-grounded in prior research. Consequential 2026-Q2 signals already baked in: GHCR still has no Referrers API (fallback is not temporary), cosign v3 ships dual-format (no legacy removal imminent), OCI Distribution Spec is stable at 1.1, SPDX 3.0 adoption is not a forcing function. Two material gaps not surfaced in any prior review artifact remain actionable: the **`.att` tag convention** used by `cosign attest` / `syft attest` on non-Referrers-API registries is completely unaddressed in the SBOM discovery algorithm, and **CycloneDX 1.7** (released October 2024, 18 months old) is absent from the documented limitation set. Four lower-priority findings round out the six actionable slots.

## Actionable Findings

### R1 — `.att` tag SBOM discovery path is undocumented and silently missed (HIGH)

- **Location:** ADR §Decision S2-C SBOM parsing algorithm steps 1–4; ADR §Not Doing; plan §Step 1.6.
- **2026 signal:** `cosign attest <image> --predicate sbom.cdx.json` (and its `syft attest` wrapper, the dominant GitHub Actions SBOM workflow) stores SBOMs as in-toto DSSE attestations under the tag `sha256-<subject-digest>.att` on registries without the Referrers API. This is the same suffixed naming convention as `.sig` for signatures, but uses `.att` suffix. cosign issue #4335 opened Aug 2025 documents this; still open April 2026.
- **Implication:** The ADR's SBOM discovery algorithm (step 1) calls `list_referrers(subject_digest)` which falls through to `manifest_walk_fallback_tags` on GHCR/Docker Hub. That walk probes `sha256-<digest>` (the referrer-index tag) but NOT `sha256-<digest>.att`. Result: any SBOM attached via `cosign attest` or `syft attest` on GHCR is silently invisible to `ocx sbom`; returns exit 79 (`sbom_no_referrers_found`) even though an SBOM exists.
- **Prior signal ignored:** `research_oci_referrers_2026.md §3` already flagged "Attestations pushed by `cosign attest` may still use the legacy `.att` tag convention" and cited cosign issue #4335. Never incorporated into ADR.
- **Required fix (choose one):**
  - **Option A (recommended):** Add `sha256-<digest>.att` probe to the SBOM discovery fallback path after the `sha256-<digest>` walk. The `.att` tag points to an in-toto DSSE manifest; classify the inner predicate type to detect CycloneDX/SPDX predicates. Add `"att-tag"` to the `method` field in the referrer-index cache entry schema. ~10 lines of Rust.
  - **Option B:** Add to ADR "Not Doing": *"cosign attest .att tag convention — not supported; use `cosign attach sbom` or a Referrers-API-capable registry"*, with user-guide callout.
- **Source:** https://github.com/sigstore/cosign/issues/4335 ; `research_oci_referrers_2026.md §3` (2026-04-19).

### R2 — CycloneDX 1.7 absent from documented limitations (MEDIUM)

- **Location:** ADR §Not Doing ("CycloneDX 1.6 → `sbom_unsupported_format`"); plan §Testing Strategy Unit Tests.
- **2026 signal:** CycloneDX 1.7 released October 21, 2024 — 18 months before this design. ADR is silent on 1.7. Both 1.6 and 1.7 are in the wild from Trivy, Grype, cdxgen. The `cyclonedx-bom` 0.8.1 crate tops at 1.5; issue #769 (1.6 support, opened Nov 2024) still open, no milestone. CycloneDX 1.7 is backward-compatible with 1.4–1.6 (same media type, additive fields).
- **Implication:** A `cyclonedx-bom` 1.5 parser applied to a 1.7 document will deserialize **successfully without error** but silently drop new fields — it will NOT emit `sbom_unsupported_format`. The ADR's stated behavior ("1.6 bytes → `sbom_unsupported_format`") requires an explicit `specVersion` check before parser dispatch, but the ADR never specifies that check. Without it, 1.6/1.7 documents silently parse as 1.5.
- **Required fix:** Implementation must check `bom["specVersion"]` before dispatching to `cyclonedx-bom`. ADR amendment: *"If `specVersion > "1.5"` (detected from JSON root before parse), emit `sbom_unsupported_format`. Affected versions: 1.6 and 1.7."* Add `cyclonedx_1_7_input_yields_unsupported_format` unit test alongside `cyclonedx_1_6_input`.
- **Source:** https://github.com/CycloneDX/specification/releases ; https://github.com/CycloneDX/cyclonedx-rust-cargo/issues/769 ; https://fossa.com/blog/whats-new-cyclone-dx-1-7/

### R3 — `spdx-rs` staleness quantified: last commit 2023-11-27; `serde-spdx` alternative (MEDIUM)

- **Location:** ADR §Decision S2-C rationale; ADR §Risks table; overlaps Slice 2 Architect F5.
- **2026 signal:** Confirmed last commit on `spdx-rs` was **November 27, 2023** — 17 months of zero activity. No open PRs. No fork on GitHub. 7 stars unchanged. Architect F5 already demands a vendor-or-fork contingency paragraph; this finding supplies concrete data plus alternative.
- **Required fix:** Update ADR risk row: *"Last known commit: 2023-11-27. No maintained fork exists. Contingency: (1) fork `spdx-rs` at `external/spdx-rs/` under the workspace patch mechanism (Apache-2.0, permitted); (2) if fork cost is prohibitive, evaluate `serde-spdx` 0.10 as a drop-in SPDX 2.3 deserializer."*
- **Source:** https://github.com/doubleopen-project/spdx-rs/commits/main ; https://docs.rs/crate/serde-spdx/latest

### R4 — CycloneDX 2.0 ("Transparency Exchange Language") announced for 2026 — forward-compat hook missing (LOW)

- **Location:** ADR §Forward-Compat Hooks for v3.
- **2026 signal:** CycloneDX 2.0 described on official specification GitHub issue #702 as coming "in 2026" — modular format unifying BOM with supply-chain blueprints, threat modeling, AI/ML capabilities, PQC. No release date. Explicitly superset, not replacement. Backward compat is stated goal.
- **Required fix:** Add one sentence to ADR §Forward-Compat Hooks: *"**CycloneDX 2.0** — announced for 2026 as superset 'Transparency Exchange Language'; no release date yet. If backward compat holds (stated goal), a `cyclonedx-bom` crate update suffices. If the JSON root schema changes, add `sbom/cyclonedx2.rs` alongside `cyclonedx.rs` and add `SbomFormat::CycloneDx2` — `#[non_exhaustive]` means no breaking change to `SbomSummary`."*
- **Source:** https://github.com/CycloneDX/specification/issues/702

### R5 — Cosign legacy `.sig` interop is indefinite, not "multi-year" (LOW, framing fix)

- **Location:** ADR §Decision S2-A rationale ("multi-year transition window"); ADR §Risks ("Legacy cosign bundle parse drift — Low").
- **2026 signal:** cosign v3 blog says *"soon we will start removing the old functionality for the initial release of Cosign v4"* but provides **no v4 timeline**. SIGNATURE_SPEC.md documents the legacy format as fully supported. No deprecation notice, sunset milestone, or removal date anywhere. Supporting legacy `.sig` buys **indefinite** interop, not ~1–2 years.
- **Required fix:** Update ADR §S2-A rationale from "multi-year transition window" to *"indefinite compatibility period — cosign v4 mentions removal of old functionality but no timeline exists; treat legacy format as permanent for Slice 2 and v3+ planning purposes."*
- **Source:** https://blog.sigstore.dev/cosign-3-0-available/ ; https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md

### R6 — GHCR fallback code is not dead within 12–24 months (LOW, risk-note fix)

- **Location:** ADR §Decision Drivers D1; ADR §Risks ("Two caches diverge when registry adds Referrers API within 24h window — Low").
- **2026 signal:** GHCR Referrers API community discussion #163029 (opened June 2025) marked **dormant** by GitHub automation on October 17, 2025. No GitHub staff comment. No roadmap. Docker Hub's "Referrers API on the horizon" blog is from October 2022 — 3.5 years old, no follow-up.
- **Required fix:** Update ADR §Risks row "Two caches diverge": *"GHCR: discussion #163029 dormant Oct 2025, no roadmap. Docker Hub: silent since Oct 2022. Fallback code not expected to be obsolete in Slice 2 or v3 timeframes."*
- **Source:** https://github.com/orgs/community/discussions/163029 ; https://www.docker.com/blog/announcing-docker-hub-oci-artifacts-support/

## Deferred Findings

- **D1 — SPDX 3.0 not accelerating.** SPDX 3.0 finalized April 2024; still no major Rust parser; major tooling still emits 2.3; SPDX Tooling Mini Summit 2025 showed transition underway but unfinished. Deferral correct.
- **D2 — OCI Distribution Spec stable at 1.1.** No 1.2/1.3 draft. Pagination, `OCI-Filters-Applied` header, fallback-tag algorithm unchanged.
- **D3 — Policy-as-code not replacing flag-based trust for CLI tools in 2026.** Kyverno 1.17 (Feb 2026) / Ratify operate on K8s admission control, not standalone CLIs. OCX's flag-based stance is correct for 2026 binary PMs.
- **D4 — SBOM-of-SBOM signing not an emerging practice.** OpenSSF "Beyond the SBOM" (Mar 2025) recommends cosign attest — exactly what OCX's `.att` handling (if R1 adopted) would cover. No second-order standard emerging.
- **D5 — No competitors shipping verify + sbom.** mise, proto, pixi, uv have no supply-chain verification or SBOM features. ORAS is raw OCI. Trivy covers containers only. Slice 2 differentiation confirmed.
- **D6 — CVE-2026-31830 is sigstore-ruby only.** Does not affect sigstore-rs (which lacks DSSE verification entirely — documented gap).

## Citations

| URL | Date | Claim |
|-----|------|-------|
| https://github.com/sigstore/cosign/issues/4335 | 2025-08→open | `cosign attest` still writes `.att` tags on non-API registries |
| https://github.com/CycloneDX/specification/releases | 2024-10-21 | CycloneDX 1.7 released |
| https://github.com/CycloneDX/cyclonedx-rust-cargo/issues/769 | 2024-11→open | CycloneDX 1.6 support open with no milestone |
| https://github.com/CycloneDX/specification/issues/702 | 2025-2026 | CycloneDX 2.0 (Transparency Exchange Language) target 2026 |
| https://fossa.com/blog/whats-new-cyclone-dx-1-7/ | 2024 | 1.7 backward-compat confirmed for 1.4–1.6 |
| https://github.com/doubleopen-project/spdx-rs/commits/main | 2023-11-27 | `spdx-rs` last commit |
| https://docs.rs/crate/serde-spdx/latest | 2026-Q2 | `serde-spdx` 0.10 alternative |
| https://blog.sigstore.dev/cosign-3-0-available/ | 2025 | cosign v4 mentioned without timeline |
| https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md | current | Legacy signature format still documented |
| https://github.com/orgs/community/discussions/163029 | 2025-10-17 | GHCR Referrers API discussion dormant |
| https://www.docker.com/blog/announcing-docker-hub-oci-artifacts-support/ | 2022-10 | "On the horizon" since 2022; no 2026 update |
| https://spdx.dev/unpacking-the-spdx-3-0-tooling-mini-summit-a-new-era-of-compliance-and-security/ | 2025 | SPDX 3.0 transition not complete |
| https://kyverno.io/blog/2026/02/02/announcing-kyverno-release-1.17/ | 2026-02 | Policy-as-code in K8s admission, not CLI |
| https://openssf.org/blog/2025/03/25/beyond-the-software-bill-of-materials-sbom-ensuring-integrity-with-attestations-event-recap/ | 2025-03 | SBOM signing via cosign attest |
| https://advisories.gitlab.com/pkg/gem/sigstore/CVE-2026-31830/ | 2026 | Ruby-only DSSE verification bypass |
