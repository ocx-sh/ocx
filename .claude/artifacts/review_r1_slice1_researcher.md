# Review R1 — Slice 1 SOTA Gap

**Verdict:** PASS-WITH-ACTIONABLE (5 Actionable, 4 Deferred)
**Date:** 2026-04-19
**Reviewer:** worker-researcher (sonnet) — tracking 2025-Q4 through 2026-Q2 Sigstore ecosystem signals.

## Summary

The Slice 1 design is well-grounded in the Sigstore ecosystem **as it stood in late 2025**. Four actionable gaps exist against April 2026 reality. Most severe: the `ci-id` crate (ADR S1-C for ambient OIDC detection) was **archived on 2026-01-27** and is permanently read-only. Second: **Rekor v2 went GA in October 2025** and eliminates the Signed Entry Timestamp (SET), replacing it with RFC 3161 timestamps from a timestamp authority — sigstore-rs 0.13 has no Rekor v2 client, and no tracking issue for it exists. Third: **cosign 3.0.6 (April 6, 2026) patched CVE-2026-39395**; the interop test must pin this version. Fourth and fifth are correctable documentation drifts (Fulcio v2 URL in ADR step 6; inaccurate "upstream PR in flight" rationale for the DSSE deferral). OCI Distribution Spec 1.1.1 remains current with no v1.2 visible; Fulcio's underlying API usage in sigstore-rs is already v2-correct; GHA/GitLab/CircleCI have no breaking OIDC changes in 2026.

## Actionable Findings

### A1 — `ci-id` crate archived (CRITICAL)

- **Topic:** OIDC ambient-detection dependency
- **2026 signal:** `jku/ci-id` (v0.3.0, December 2024) was **archived 2026-01-27**, read-only with 3 open issues + 1 open PR that will never be merged. Covers only GHA, GitLab, CircleCI, Buildkite — missing GCP Cloud Build. No maintainer path for future CVE fixes.
- **Implication for Slice 1:** ADR decision **S1-C** selects `ci-id = "^0.3"`. Depending on an archived crate in a security-sensitive path (OIDC token acquisition) is a non-starter.
- **Required amendment:** Evaluate `ambient-id` (active as of April 2026, Fedora packaging review underway RHBZ#2396331) as the replacement. If insufficient, inline the ambient detection (~80 lines of env-var inspection: `ACTIONS_ID_TOKEN_REQUEST_URL`, `CI_JOB_ID` + `SIGSTORE_ID_TOKEN`, `CIRCLE_OIDC_TOKEN_V2`, `BUILDKITE_AGENT_ACCESS_TOKEN`). Update `plan_slice1_sign_and_verify.md` dependency table; update `research_oidc_cli_flows.md` decision D-OIDC-1; flag in deps-skill pass.
- **Source:** https://github.com/jku/ci-id (archived 2026-01-27)

### A2 — Rekor v2 eliminates SET; sigstore-rs 0.13 has no v2 client (HIGH)

- **Topic:** Rekor entry format + verify-side SET validation
- **2026 signal:** Rekor v2 went GA October 2025 (tiled-log architecture). **No SET in log entries** — clients must obtain RFC 3161 signed timestamps from `timestamp.sigstore.dev` instead. `TransparencyLogEntry.integrated_time` is always 0 and MUST be ignored. v2 log instance: `log2025-1.rekor.sigstore.dev`. Sigstore-rs issue #539 (Dec 2025) discusses TSA-based verification but no Rekor v2 tracking issue for sigstore-rs exists. Rekor-tiles/issues/#272 (closed) tracked sigstore-go / java / python — **sigstore-rs is absent**.
- **Implication for Slice 1:** The verify pipeline (`oci/verify/pipeline.rs`, step 4: "Validates Rekor SET against Rekor public key") is valid only against v1 log. If Sigstore distributes the v2 log URL through TUF before OCX ships, newly-signed bundles contain no SET — verification fails on the "SET present and valid" invariant the plan mandates. No branching logic exists in the plan.
- **Required amendment:** Add explicit Risks-table item: *"Rekor v2 TUF distribution imminent (Q1 2026 deadline has passed); verify pipeline must handle SET-absent bundles."* For v1 scope: verify SET when present; emit warning (not hard error) when absent if RFC 3161 TSA timestamp is present. Reserve distinct variant `VerifyErrorKind::RekorSetAbsentTsaPresent` (exit code mapped under existing `Unavailable = 69` OR a new reserved slot — architect to decide). Pin `sigstore = "=0.13"` and note in risk table: 0.13 cannot verify Rekor v2 entries; full v2 sign/verify loop deferred until sigstore-rs gains v2 client support.
- **Sources:**
  - https://blog.sigstore.dev/rekor-v2-ga/
  - https://github.com/sigstore/rekor-tiles/blob/main/CLIENTS.md
  - https://github.com/sigstore/sigstore-rs/issues/539

### A3 — cosign interop test must pin ≥3.0.6 due to CVE-2026-39395 (MEDIUM)

- **Topic:** Cosign version in interop test, threat-model documentation
- **2026 signal:** cosign v3.0.6 (April 6, 2026) fixed **CVE-2026-39395** (`GHSA-w6c6-c85g-mmv6`) — `cosign verify-blob-attestation` false-positive "Verified OK" for attestations with malformed payloads / mismatched predicate types in all versions < 2.6.3 and 3.0.0–3.0.5. Also fixed: DSSE predicate check bypass.
- **Implication for Slice 1:** Slice 1 ships `ocx verify`, not `verify-attestation` — so OCX's own verification path is unaffected. But the interop test (`test_sign_verify_interop.py`) installs `cosign` via `ocx install cosign:2`, which could resolve to a vulnerable version. Installing a known-vulnerable tool in CI is bad hygiene; also, CI authors copying the interop-test pattern into their pipelines would pick up the vuln.
- **Required amendment:** Update plan step 3.10 + the `conftest.py` `cosign_binary` fixture: install `cosign:3` pinned `>= 3.0.6`. Note in threat model: "Interop test dependencies MUST pin non-vulnerable cosign; see GHSA-w6c6-c85g-mmv6."
- **Sources:**
  - https://github.com/sigstore/cosign/releases/tag/v3.0.6
  - https://github.com/sigstore/cosign/security/advisories/GHSA-w6c6-c85g-mmv6

### A4 — ADR step 6 lists stale Fulcio v1 URL (LOW, doc-only)

- **Topic:** Fulcio API endpoint in ADR documentation
- **2026 signal:** Fulcio v1beta is deprecated but not removed. sigstore-rs 0.13 internally uses `request_cert_v2()` against `/api/v2/signingCert`. No 2026 Fulcio breaking change announced.
- **Implication for Slice 1:** The ADR push sequence step 6 lists `https://fulcio.sigstore.dev/api/v1/signingCert` — the v1 URL. sigstore-rs routes to v2 correctly at runtime, but the ADR will mislead the builder in Phase 4.
- **Required amendment:** Correct ADR step 6 to read: *"POST to Fulcio (`https://fulcio.sigstore.dev/api/v2/signingCert` via sigstore-rs `FulcioClient::request_cert_v2`)"*. No code change required.
- **Source:** https://docs.rs/sigstore/latest/sigstore/fulcio/struct.FulcioClient.html

### A5 — DSSE deferral rationale cites non-existent PR (LOW, doc-only)

- **Topic:** S1-D rationale accuracy
- **2026 signal:** sigstore-rs latest release is 0.13.0 (October 2024). **No 0.14 in progress**, no DSSE signing issue or PR on the tracker. Current ADR rationale for S1-D states "upstream PR already in flight" — factually incorrect.
- **Implication for Slice 1:** The deferral decision is correct; only the justification is wrong. Leaving it creates false expectations for v2 planning.
- **Required amendment:** Remove "upstream PR already in flight" from S1-D rationale. Replace: *"DSSE signing is not implemented in sigstore-rs 0.13; no upstream tracking issue as of 2026-04-19. v2 must re-evaluate when / if sigstore-rs 0.14+ ships signing support."*
- **Sources:**
  - https://github.com/sigstore/sigstore-rs/releases
  - https://github.com/sigstore/sigstore-rs/issues

## Deferred Findings

- **D1 — Rekor v2 TUF distribution deadline passed.** Sigstore planned to distribute the v2 log URL via TUF "by end of 2025 / early 2026." April 2026 and distribution has not yet been confirmed pushed. If it happens during OCX v1 development, all newly-signed bundles will use v2 format (no SET). **Human decision:** ship v1 with documented "Rekor v1 log only" limitation, or delay until sigstore-rs adds v2 client support? Resolver: owner + parent-ADR author.
- **D2 — `ci-id` replacement maturity.** `ambient-id` is active (Fedora packaging review in progress as of April 2026) but pre-ecosystem-integration. Inline detection is ~80 lines of stable Rust. Trade-off: `ambient-id` may diverge from Sigstore conventions; inline means maintaining a CI platform compatibility list ourselves. Resolver: architect + deps skill pass.
- **D3 — SLSA provenance customer pull.** SLSA Level 2 is becoming the recommended baseline for production software; GitHub Actions natively generates SLSA provenance via `actions/attest-build-provenance`. OCX's signature-only v1 may be perceived as incomplete supply-chain tooling by enterprise buyers relative to tools that ship SLSA attestations. Not a v1 blocker; should accelerate v2 sequencing (DSSE attestation beats SBOM discovery in enterprise pull). Resolver: product owner.
- **D4 — sigstore timestamp-authority CVE-2026-39984.** Medium-severity cert bag prepend attack in the Go TSA verifier, fixed in v2.0.6. Not in OCX v1 scope (no TSA verification). Relevant only if A2 mitigation grows into full RFC 3161 TSA verification. Resolver: track for v2.

## Citations

| URL | Date | Claim |
|-----|------|-------|
| https://github.com/jku/ci-id | 2026-01-27 | ci-id repository archived; v0.3.0 is final |
| https://blog.sigstore.dev/rekor-v2-ga/ | 2025-10-10 | Rekor v2 GA; SET replaced by RFC 3161 TSA; `/api/v2/log/entries` endpoint |
| https://github.com/sigstore/rekor-tiles/blob/main/CLIENTS.md | 2025-Q4 | Rekor v2 client requirements; sigstore-rs absent from tracking |
| https://github.com/sigstore/rekor-tiles/issues/272 | 2025 | Client support tracking: sigstore-go/java/python covered; sigstore-rs absent |
| https://github.com/sigstore/sigstore-rs/issues/539 | 2025-12-21 | TSA-based verification feature request; no Rekor v2 tracking issue open |
| https://github.com/sigstore/cosign/releases/tag/v3.0.6 | 2026-04-06 | Release notes: fixes CVE-2026-39395 and GHSA-w6c6-c85g-mmv6 |
| https://github.com/sigstore/cosign/security/advisories/GHSA-w6c6-c85g-mmv6 | 2026-04 | DSSE predicate check bypass advisory; fix in 3.0.6 |
| https://docs.rs/sigstore/latest/sigstore/fulcio/struct.FulcioClient.html | current | sigstore-rs uses `request_cert_v2()` — confirms v2 API |
| https://github.com/sigstore/sigstore-rs/releases | 2024-10-16 | Latest release 0.13.0; no 0.14 |
| https://blog.sigstore.dev/cosign-3-0-available/ | 2025 | cosign v4 will remove ~half of CLI flags; identity flags mandatory in v3 |
| https://github.blog/changelog/2026-03-12-actions-oidc-tokens-now-support-repository-custom-properties/ | 2026-03-12 | GHA OIDC adds custom-properties claim; additive-only |
| https://advisories.gitlab.com/pkg/golang/github.com/sigstore/timestamp-authority/v2/CVE-2026-39984 | 2026-03 | TSA cert bag prepend attack; fixed v2.0.6; Go-only |
| https://crates.io/crates/ambient-id | 2026-Q2 | Active alternative to ci-id |
