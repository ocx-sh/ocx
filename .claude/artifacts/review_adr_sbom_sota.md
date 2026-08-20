# SOTA gap check — adr_sbom_attestations (2026-08-20)

> hex-architect Review panel, SOTA researcher (sonnet). Persisted by the
> orchestrator. Verdict: **no ADR decision invalidated**; 2 actionable
> low-effort additions, 5 axes reinforcing.

## Actionable

1. **in-toto spec v1.2 broadened `payloadType`** to allow
   `application/vnd.in-toto.<predicate>+json`. The ADR hardcodes exact-match
   `application/vnd.in-toto+json` (Part III row 3). cosign v3.1.3 still emits
   only the generic form — nothing breaks — but the exact-match is now an
   undocumented narrowing, same shape as D-b's `_type` allowlist deviation.
   Fix: one sentence in `signing.md` recording the deviation
   (in-toto CHANGELOG, fetched raw, verified twice).
2. **CVE-2026-39395 / GHSA-w6c6-c85g-mmv6** (cosign, Apr 2026):
   `verify-blob-attestation` false-positive "Verified OK" with (a) valid
   signature + syntactically malformed payload, (b) predicate-type mismatch
   bypass on new-format bundles. (b) is covered (`PredicateTypeMismatch`
   fixture). **(a) has no dedicated fixture in the ADR's Testing Strategy** —
   add: correctly-signed envelope whose payload is corrupt JSON → specific
   error kind (statement parse failure), the sharpest concrete test of
   checklist row 20.

## Reinforcing / no-gap

- **cosign v4**: scoped to CLI simplification; bundle stays v0.3. D1 stands.
- **OCI**: no distribution-spec 1.2; artifactType + empty-descriptor + 
  OCI-Filters-Applied unchanged. D-e stands.
- **in-toto v1.1** DigestSet generalization: orthogonal to row 6 (OCX policy).
- **sigstore-rs**: 0.14.0 still latest (15 months, liveness flag — DEP-01
  recheck at spike time, not a design blocker). D-a/D-d stand.
- **Rekor v1 sunset**: public-good keeps v1 default "for the foreseeable
  future" (rekor-evolution post) — de-risks D7/D-g's dsse:0.0.1 timing; worth
  a one-line Risks addition.
- **SLSA v1.2** (Nov 2025): Source track promoted; buildDefinition/runDetails
  unchanged; predicateType stable across minors. D-c stands.
- **Cross-ecosystem**: npm provenance, Homebrew (beta), PyPI PEP 740 GA all
  verify through the same cosign-bundle path (cosign 2.4.0+ one code path) —
  independent convergence on the ADR's exact shape; citation material for D1.
- **CVE-2026-22703 / GHSA-whqx-f9j3-ch6m** (Jan 2026): confirmed REGRESSION
  of GHSA-8gw7-4j42-w388 — live proof row 12's EnvelopeHashMismatch fixture
  must stay red-before-green locked. No new action beyond Part III.
- **GHSA-fx35-mq7g-6g98** (Aug 2026, High 7.4): legacy LocalSignedPayload
  JSON bundles only; "Not Affected: Modern Sigstore protobuf bundle format" —
  does not reach this design.

Sources: cosign BUNDLE_SPEC.md · sigstore_bundle.proto · distribution-spec ·
in-toto spec/v1/CHANGELOG.md · sigstore-rs releases · blog.sigstore.dev
rekor-evolution + cosign-verify-bundles + pypi-attestations-ga · SLSA v1.2
build-provenance · GitHub advisories GHSA-whqx-f9j3-ch6m,
GHSA-w6c6-c85g-mmv6, GHSA-fx35-mq7g-6g98
