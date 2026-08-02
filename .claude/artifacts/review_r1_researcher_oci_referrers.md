# Research Review Round 1 — SOTA Gap Assessment for OCI Referrers Discovery (Issue #24)

## Summary

**Verdict: PASS-WITH-FIXES.** The ADR/plan is well-grounded in the research artifacts and correctly traces every major decision back to a research finding. 9 actionable findings — none are showstoppers, but 5 carry enough force to warrant a note or correction before the plan is handed to the builder. 4 deferred findings require human judgment on product or stakeholder grounds. 2 genuine 2026-vintage SOTA shifts worth flagging.

Counts: **9 actionable**, **4 deferred**.

---

## Actionable Findings

### 1. `research_verify_cli_patterns.md` §6 registry matrix contains erroneous GHCR/Docker Hub/registry:2 entries

Patterns-axis research §6 registry support matrix lists GHCR as "Supported" (WRONG — no Referrers API), Docker Hub as "Supported" (WRONG), and registry:2 as "No referrers API" (WRONG — registry:2 v2.8.3+ supports it). The ADR correctly cites the domain-axis research for registry compat (which is accurate), but the contradictory patterns-axis table could confuse future readers.

**Fix**: annotate the discrepancy in the ADR's Industry Context footnotes — "patterns-axis research §6 contains known-incorrect entries; authoritative source is `research_oci_referrers_2026.md` §2."

### 2. `adr_oci_referrers_discovery.md` §Decision B — sigstore-rs v0.13.0 release date error in research

`research_cosign_sigstore_notation.md` states v0.13.0 released October 2025. Actual release: October 2024. Still current, no 2026 release, still pre-1.0. Recommendation unchanged but the date error creates a false freshness signal.

**Fix**: correct the date in the research artifact to "October 2024."

### 3. `adr_oci_referrers_discovery.md` §Decision B — DSSE currency check needed

ADR relies on sigstore-rs README claim "DSSE not yet implemented." Verified true as of April 2026 (no 0.14 release; no DSSE PRs merged). Without explicit currency-check annotation, this risks being re-opened as a stale-research concern.

**Fix**: add a line to §Decision B — "Verified current April 2026: no 0.14 release; DSSE remains unimplemented."

### 4. `adr_oci_referrers_discovery.md` §Decision B — GitHub Attestations API not addressed as GHCR alternative

GitHub ships `GET /repos/{owner}/{repo}/attestations/{subject_digest}` (REST, not OCI) — versioned 2026-03-10. `actions/attest-build-provenance` writes here, NOT into OCI fallback tags. `ocx verify` against a GHA-attested GHCR image will return empty even though provenance exists.

**Fix**: add to §Consequences → Negative: "GitHub Attestations (`actions/attest-build-provenance`) store provenance in GHCR via GitHub's proprietary Attestations API, not in OCI fallback tags. `ocx verify` cannot discover this provenance. Users should call `gh attestation verify` separately for GHA-attested packages."

### 5. `plan_oci_referrers_discovery.md` §3.1 unit tests — DSSE classification drift vs ADR

Plan specifies test `classify_dsse_envelope`: `application/vnd.dsse.envelope.v1+json` → `Signature(SigFormat::Dsse)`. ADR §Decision B forward-compat table does NOT enumerate DSSE. Research says DSSE types exist but aren't verified. Drift between plan and ADR.

**Fix**: ADR §Decision B needs explicit row `application/vnd.dsse.envelope.v1+json` → "Classify as DSSE; discover-only; no verify." Plan test description to match.

### 6. `adr_oci_referrers_discovery.md` §Decision C — cosign bug #4641 upstream PR status

Bug #4641 still open as of April 2026. Upstream go-containerregistry PRs (#1931, #2068) unmerged. Defensive-parse decision still correct.

**Fix**: plan's `test_verify_bug_4641_defensive_classify` fixture docstring should note "upstream go-containerregistry PRs stalled 2026-04; remove defensive classify only after upstream confirms fix." Add TODO anchor in `referrer/fallback.rs`.

### 7. `adr_oci_referrers_discovery.md` §Decision B — CycloneDX 1.6 gap not surfaced

`cyclonedx-bom` v0.8.1 (March 2025, still latest) does not support CycloneDX 1.6 (released March 2024). Users encountering 1.6 SBOMs receive opaque pass-through only. EU CRA tooling landscape (BSI TR-03183-2) now requires CycloneDX 1.6+ or SPDX 3.0.1+ for full compliance.

**Fix**: PRD FR-13 to add "CycloneDX 1.3–1.5 only (1.6 unsupported by `cyclonedx-bom` v0.8.1)." `ocx sbom --help` + user-guide section to state this explicitly.

### 8. `plan_oci_referrers_discovery.md` §3.2 — `registry_without_referrers_api` fixture underspecified

Plan offers fallback option `--distribution-spec v1.1-referrers-tag` if registry:2 can't be configured to return 404. This bypasses the auto-probe path — several acceptance tests (`test_verify_auto_probe_falls_back_on_404`) depend on the probe flow.

**Fix**: remove the `--distribution-spec` fallback option; specify `distribution:v3.0.0-beta.1` OR a NGINX proxy that returns 404 on the referrers path.

### 9. `adr_oci_referrers_discovery.md` §Risks — ECR bug #2783 not mentioned

Domain research flagged ECR bug #2783 (405 on `oras copy -r` with OCI 1.1 referrer manifests, March 2026). Affects push/copy path, NOT read path. Out of v1 scope but should be documented for when write-side lands.

**Fix**: add Risks row — "ECR push bug #2783 affects cross-registry referrer copy operations (not read path); OCX read path confirmed unaffected; note for documentation when write-side lands."

---

## Deferred Findings

### 1. PRD open questions Q-PRD-1/5/6/7 require stakeholder sign-off

Q-PRD-1 (short digest prefix), Q-PRD-5 (CPU variant in `--platform`), Q-PRD-6 (TTY vs `CI=true`), Q-PRD-7 (exit 69 vs 65 when `--require-referrers` + registry 500). Research cannot resolve; Q-PRD-7 in particular affects which acceptance tests pass.

### 2. ADR §Decision D — v2 GitHub issue placeholder

ADR defers auto-verify to v2. Trust-policy `level = "strict"` error message links to `<TBD>` issue. Requires human to create the v2 tracker issue first before landing; otherwise error message directs users to a non-existent tracker.

### 3. product-context.md update candidate

ADR §Step 4.12 flags "Cross-registry supply-chain discovery (OCI 1.1 + fallback)" as a new differentiator row. Needs panel consensus before committing.

### 4. Kyverno/Sigstore Policy Controller v2 compat

Kyverno 1.17 (Feb 2026) ships cosign support with ClusterImagePolicy CRD policy shape. Future v2 trust-policy design must weigh translation cost for users with existing Kyverno/Policy Controller policies. Not a v1 concern.

---

## SOTA Shifts Since Phase 2 Research

- **sigstore-rs release-date error in tech research** — v0.13.0 is October 2024, not October 2025 as stated. Correctable nit.
- **GitHub Attestations API versioned 2026-03-10** — GHCR-native REST alternative to OCI Referrers. Divergent ecosystem path unaddressed by current ADR.
- **CycloneDX 1.6 still unsupported by `cyclonedx-bom` v0.8.1** — status unchanged since research; EU CRA tooling (BSI TR-03183-2) now explicitly requires 1.6+ or SPDX 3.0.1+. Gap mildly more significant than at research time.

---

## Verdict

**PASS-WITH-FIXES.** 9 actionable findings (none are blockers; findings 1, 4, 5, 7, 8 have the highest practical consequence). 4 deferred. No fundamental design decisions need revisiting.

## Sources

- [sigstore-rs releases](https://github.com/sigstore/sigstore-rs/releases) — v0.13.0 still latest, no DSSE in 2026
- [cosign issue #4641](https://github.com/sigstore/cosign/issues/4641) — still open, upstream PRs stalled
- [GHCR community discussion #163029](https://github.com/orgs/community/discussions/163029) — no Referrers API, no roadmap
- [GitHub REST API for attestations](https://docs.github.com/en/rest/repos/attestations) — GitHub-proprietary alternative confirmed active
- [cyclonedx-rust-cargo releases](https://github.com/CycloneDX/cyclonedx-rust-cargo/releases) — v0.8.1 still latest, 1.6 unsupported
- [Notary Project GitHub](https://github.com/notaryproject) — all repos Go-only, no Rust library in 2026
- [Kyverno 1.17 announcement](https://kyverno.io/blog/2026/02/02/announcing-kyverno-release-1.17/) — cosign support added with CRD policy shape
