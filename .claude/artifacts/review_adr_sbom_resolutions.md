# Panel resolutions — adr_sbom_attestations.md batch fix directive

Orchestrator triage of the full adversarial panel: `review_adr_sbom_spec.md`
(13 findings), `review_adr_sbom_security.md` (B1–B9, W1–W5, audits),
`review_adr_sbom_quality.md` (F1–F11 + addendum + reversibility), and
`review_adr_sbom_sota.md` (2 items). Every finding is RESOLVED below — the
architect applies these in one pass; no finding is left open. Where reviewers
converged, the resolution is stated once and cross-referenced. Read the three
review files for full evidence; this file is the *decision*, theirs is the
*argument*.

## R1 — Verify composition: delegate crypto, layer OCX checks on top
(sec B8, quality F1+F2, spec F4; resolves checklist rows 1/4/11/13)

- `verifier.verify(subject_bytes, bundle, &PolicyDeferredToOcx, true)`
  (pipeline.rs:382) runs UNCHANGED for **both** content modes. sigstore 0.14's
  DSSE branch does PAE verification, subject[0]-digest binding vs input digest,
  chain build to OCX's TrustRoot, SCT, cert-validity window, and dsse:0.0.1
  tlog consistency.
- Rewrite D-d L239's rejection cell: DELETE the "two trust roots" claim (false —
  `trust_root.rs:301` already implements `sigstore::trust::TrustRoot`; there is
  one Verifier holding OCX's root). Record the REAL delegation gaps (quality F1
  list, verified in sigstore 0.14 source): (1) no `_type` check on the bundle
  path, (2) no `payloadType` check, (3) subject[0] only, (4) tlog envelopeHash
  compared against a re-serialization, (5) `InTotoStatementV1`/`verify_bundle_content`
  are `pub(crate)` — the delegation-only option is unimplementable AND
  insufficient. Fix L241-245's "raw ECDSA `verify_prehash`" → sigstore's DSSE
  step is `CosignVerificationKey::verify_signature(Signature::Raw(sig), pae)`.
- `verify_envelope` becomes OCX's defence-in-depth layer running AFTER
  `verifier.verify`, over the RECEIVED bytes. New signature (drops
  `verifying_key` — crypto is delegated):
  `pub(super) fn verify_envelope(bundle: &Bundle, target_digest: &Digest, expected_predicate_type: Option<&PredicateType>) -> Result<VerifiedAttestation, VerifyErrorKind>`
  It enforces: `_type` ∈ {v1, v0.1}; `payloadType == application/vnd.in-toto+json`
  before parse; **ALL subjects** bound (closes sigstore's subject[0] gap —
  CVE-2026-31830); zero-subject refusal; sha256-only DigestSet; exactly one
  signature; predicateType from payload (CVE-2022-35929); caps.
- Row 13 (CVE-2024-55655): option (a) — OCX's OWN re-assertion
  `NotBefore <= integratedTime <= NotAfter` over `parts.integrated_time` and the
  leaf certificate's validity window, implemented once and run for BOTH modes
  (helper beside the tlog checks; correct row 13's site — `verify/tlog.rs` does
  not contain it today, quality F5/sec B9 proved it). Negative fixture:
  integratedTime outside window → specific error kind. Also correct row 13's
  "Existing coverage" cell — name the new test.

## R2 — Tlog binding + accepted kinds (sec B2, B3; quality F3/Reading A)

- `ACCEPTED_TLOG_KINDS = {dsse:0.0.1}` ONLY. Drop `intoto:0.0.2`: unsourced,
  and sigstore's `tlog_entry_for_dsse` hard-rejects any other kind, so it has
  no reachable green through R1's delegated path. Move to Not Doing with that
  as the stated reason.
- Row 12 rebind: verify-side `verify_tlog_binding` compares the canonicalized
  body's `payloadHash` (sha256 over PAE of the RECEIVED envelope) and
  `signatures[]` content against the received bundle envelope. `envelopeHash`
  is NOT recomputed verify-side (impossible from protobuf-JSON — sec B2);
  envelopeHash consistency is sign-side only. Record the deviation: sigstore's
  internal comparison runs over a re-serialization; OCX's own binding check on
  received bytes is the defence in depth.

## R3 — Predicate byte fidelity (sec B5, quality F6, F11; row 2)

- `AttestOptions.predicate: Vec<u8>` — raw file bytes, validated by a parse
  whose `Value` is discarded. Statement built with the predicate embedded via
  `serde_json::value::RawValue` (verbatim splice). Read side parses the payload
  with `predicate: Box<RawValue>`, so `sbom --output` writes the exact sub-slice.
  `VerifiedAttestation` gains `predicate: Box<RawValue>` (parsed from the
  verbatim payload at construction; `payload: Vec<u8>` stays the full Statement).
  Workspace `serde_json` gains the `raw_value` feature — Affected Surfaces row
  (root `Cargo.toml`).
- Correct L202-206: `preserve_order` is ON workspace-wide
  (`crates/ocx_lib/Cargo.toml:50`, IndexMap-backed) — the BTreeMap claim is
  false. State the real contract: determinism is per-input via RawValue verbatim
  embedding, not canonicalization. (`serde_json_canonicalizer` stays unused here;
  hashing is over received/embedded bytes, never re-serialized.)
- Row-2 round-trip fixture MUST be a pretty-printed CycloneDX file;
  red-before-green demonstrated (assert byte-identity fails when the embed path
  re-serializes).
- `MAX_PREDICATE_FILE_BYTES = 15 MiB` — below `MAX_STATEMENT_PAYLOAD_BYTES`
  (16 MiB) by a named wrapper reserve; comment names it; boundary fixture at the
  cap exactly (quality F11).
- `sbom --output -` REFUSES when stdout is a TTY (typed error; message: raw
  predicate bytes are unsanitized, redirect to a file or pipe). File/pipe:
  verbatim bytes (sec W2). CLI-09/CLI-07 precedent.

## R4 — Annotations: write all three; read as hints only
(quality F4+addendum+F5, sec B4; reversibility)

- OCX writes cosign's full annotation set on attestation referrers:
  `org.opencontainers.image.created` (RFC 3339 Z; `SOURCE_DATE_EPOCH` when set,
  else now — note: bundle blobs are per-run unique anyway (ephemeral cert +
  fresh Rekor entry, S1-I append-only), so referrer digests never converge
  regardless; `created` adds no *new* nondeterminism — record this),
  `dev.sigstore.bundle.content: dsse-envelope`,
  `dev.sigstore.bundle.predicateType: <uri>`. Reconcile ALL FOUR sites
  (L77, L551-554, L757-758, L1108) to three; constants gain
  `ANNOTATION_CREATED`; L1049 count → 5.
- `ReferrerManifest` gains
  `#[serde(skip_serializing_if = "Option::is_none")] annotations: Option<BTreeMap<String, String>>`
  — `skip_serializing_if` is load-bearing (existing signature-manifest bytes
  unchanged when None). New Affected Surfaces row for
  `crates/ocx_lib/src/oci/referrer/manifest.rs`.
- Signature referrers ALSO start writing `created` +
  `dev.sigstore.bundle.content: message-signature` (cosign parity; unreleased
  surface — say it out loud; signature golden fixtures updated).
- Verify side (sec B4): annotations are ORDERING/PRE-FILTER HINTS only.
  Authoritative discrimination is the parsed bundle content oneof. A candidate
  lacking annotations is still fetched and parsed — a registry stripping
  annotations cannot hide an attestation. Golden fixture: an OCX-written
  attestation referrer is selected (quality F5 item 4).
- Reversibility: the annotation SET joins the Part V MAY-NOT-adjust list (the
  sharpest one-way door — pushed bytes are immutable identity).

## R5 — Discovery filter stays (spec F6)

Keep `list_referrers(..., Some(SIGSTORE_BUNDLE_V03))` — the client-side
re-filter is the one reliable typed filter (native_transport.rs:224-243 says so
verbatim). Rewrite D-e: annotation narrowing is layered ON TOP as a hint, not in
place of the artifactType filter.

## R6 — Caps by mode + candidate accounting (sec B6, W5)

- D-d states: per-candidate size cap, candidate-count cap, cross-candidate
  budget are selected from the REQUESTED `VerifyContentMode` before the first
  fetch. Signature mode keeps 512 KiB / 8 / 4 MiB unchanged.
  Red-before-green pair: a 1 MiB bundle rejected in Signature mode, accepted in
  Attestation mode.
- Mode-mismatched candidates (discriminated after fetch+parse, R4) are skipped
  WITHOUT consuming the mode's candidate count; the cross-candidate byte budget
  bounds total fetch work; a hard listing-iteration cap backstops. Fixture: one
  signature + nine attestation referrers on one subject → `ocx package verify`
  (signature mode) still succeeds (sec W5).

## R7 — Attestation arity: collect-all (sec B7, spec F2)

- Attestation mode collects ALL verified candidates (bounded by the caps), not
  first-match. One pipeline, two entry points sharing `verify_one_referrer`:
  the signature path keeps today's ANY-of `run`; attestation path
  `run_attestations -> Vec<AttestationMatch>`.
- `pub struct AttestationMatch { pub verify: VerifyResult, pub attestation: VerifiedAttestation }`
  — carries the four DTO fields spec F2 found sourceless (referrer_digest,
  certificate_identity, certificate_oidc_issuer, signed_at; RFC-3339 conversion
  reuses `VerificationReport`'s existing path).
  `SbomReport { pub attestations: Vec<AttestationMatch> }`. DROP
  `VerifyResult.attestation: Option<VerifiedAttestation>` — one arity.
- `sbom --output` with >1 verified match of the requested type: typed refusal
  `MultipleAttestations { predicate_type, referrer_digests }` (exit 65) naming
  the digests. Not Doing row: a per-referrer selection flag is deferred.
- `--json` reports every match; default listing shows all.

## R8 — Provenance version floor at attach (spec F9 → option b; owner issue #102)

- `AttestPipeline` rejects a resolved provenance predicateType below v1.0:
  `SignErrorKind::ProvenanceVersionUnsupported`, slug
  `provenance_version_unsupported`, exit 64 (UsageError — the message names
  `--type slsaprovenance1` as the fix). D-c's alias table stays a pure lookup
  (cosign parity verbatim); the floor lives in the pipeline. One Part III row,
  one negative fixture. Not Doing row states verify still ACCEPTS v0.2
  provenance from external producers (cosign interop).

## R9 — builder matching: version dispatch, fail closed (sec W3, quality F3)

- `fn builder_id(predicate_type: &PredicateType, predicate: &Value) -> Option<&str>`:
  `predicate.runDetails.builder.id` for slsa v1, `predicate.builder.id` for
  v0.2 (verify accepts both; attach only produces v1 per R8).
- A policy carrying a `builder` pin against a predicate whose builder field is
  absent or unparseable is a REFUSAL (`BuilderMismatch { expected, found: Option<String> }`),
  never a skip.
- D-j states: `builder` is ANDed within a policy, ORed across the ANY-of set —
  an equal-scope policy without it weakens the set; `system_locked` (R10) is
  the operator's containment. Fixtures: one per SLSA shape.

## R10 — `system_locked` survives (sec B1)

The nested `TrustPolicy` carries `#[serde(skip)] system_locked: bool` verbatim,
same preemption semantics in `resolve()`. State it in the D-h/D-j contract
block.

## R11 — Offline attest refusal (sec W4, quality F10)

`AttestContext.offline: bool`; the S1-E gate runs BEFORE token resolution;
`SignErrorKind::OfflineAttestRefused`, slug `offline_attest_refused`, exit 77.
The refusal helper moves into `package_sign_common.rs` WITH the token resolver
(forking it is what the extraction exists to prevent). Fixture mirrors
`test_sign_offline_refused`. Correct L826 "mirrors … exactly".

## R12 — `--predicate` open hardening (sec W1)

Open with `O_NOFOLLOW`; `ELOOP` → refusal naming CWE-367; read via the opened
handle. NO mode/ownership checks — the predicate is public data destined for
publication, not a secret; 0644 SBOMs in CI are the normal case (state this
rationale so the asymmetry with `--identity-token-file` reads as decided).

## R13 — Renames: full site table + identifiers (spec F8; quality reversibility)

- Rename the Rust identifiers WITH the slug: `ExitCode::RekorUnavailable` →
  `ExitCode::TransparencyLogUnavailable` (83 unchanged);
  `VerifyErrorKind::RekorUnavailable` / `SignErrorKind::RekorUnavailable` →
  `TransparencyLogUnavailable`.
- Replace the 3-row rename table with the full site list: verify/error.rs:395 +
  :666, sign/error.rs:210 + :387, `ocx_cli/error_envelope.rs:307` (serde-name
  pinning test), `cli/exit_code.rs:77` + `cli/classify.rs`, and the THREE
  `website/.../command-line.md` rows (:3770, :3778, :3934) — add those to
  Documentation Surfaces. Note the two acceptance tests
  (`test_verify.py:446`, `test_sign.py:737`).
- Owner ruling stands: nothing is released → rename in place, no compat. The
  commit SUBJECT states the rename (subjects are the changelog).

## R14 — Distinct subject-failure variants (spec F7)

Mint `StatementSubjectMismatch { expected, actual }`
(`statement_subject_mismatch`), `StatementSubjectAbsent`
(`statement_subject_absent`), `StatementSubjectWeakAlgorithm`
(`statement_subject_weak_algorithm`) — all exit 65. Existing transport-integrity
`SubjectDigestMismatch` is UNTOUCHED. Fix the counts (L1047: 13 new verify
variants → recount after R7/R14; no mutated rows beyond the R13 rename).

## R15 — Plumbing contracts (spec F3, F4)

- `VerifyOptions` gains `pub content: VerifyContentMode`; `SbomOptions` gains
  `pub predicate_type: Option<PredicateType>`. Part IV blocks for both; add
  `crates/ocx_lib/src/package_manager/tasks/verify.rs` to Affected Surfaces.
- State the changed gate signature:
  `fn from_bundle(bundle: &Bundle, mode: &VerifyContentMode) -> Result<Self, VerifyErrorKind>`,
  threaded from `verify_one_referrer` (`ctx.content`).

## R16 — Path + contract corrections (spec F5, F10; quality F8)

- `oci/sign/keyless.rs` → `oci/sign/signer.rs`; `oci/publish/…` →
  `crates/ocx_lib/src/publisher.rs`.
- `PushOutcome` is NOT a wire type (derives Debug only); the parsed cross-tool
  contract is `PushReport` (`ocx_cli/src/api/data/push.rs`), which is
  unchanged — rewrite D-f's compat sentence. Mark `PushOutcome`
  `#[non_exhaustive]` in the same change (path-dep compile-compat).
- `platform_digests: BTreeMap<String, Digest>` keyed by `Platform::Display`
  canonical form — `Platform` has no `Ord` and MUST NOT gain one (lockfile
  map-key ordering would become observable; spec F1). State this in D-f.
- push `--sbom` failure contract: `PushReport` gains
  `attestation: Option<AttestationOutcome>` (succeeded | failed + slug envelope,
  CLI-04); exit = attest error's classified code, combined in the push command
  handler. Add `api/data/push.rs` to Affected Surfaces (spec F10).

## R17 — Re-attest semantics wording (quality F9)

Replace "convergent": re-running is *idempotent in outcome, additive in state*
(S1-I append-only — each run adds a referrer). State the retry consequence
(a flaky CI retry loop accumulates referrers toward `MAX_ATTESTATION_CANDIDATES`)
and the user action at the cap (prune stale referrers with registry tooling;
the constant is deliberately not configurable).

## R18 — Test-plan additions (spec F11, F13; sec audit nits; SOTA)

Negative fixtures: `AttestationBudgetExhausted`; `AttestationNotFound`
(reached via `aggregate_failure` when narrowing empties the candidate set —
state it); `PredicateNotJson`; `PredicateTooLarge`; hostile `keyid` still
verifies (row 10); PAE-over-base64 mutation added to red-before-green (row 1);
CVE-2026-39395: valid signature over a payload that does not parse as a
Statement → specific error, never success (SOTA). Golden envelopes:
`test_attest_*_envelope_golden_shape` + `test_sbom_*_envelope_golden_shape`
built on the `test_verify.py:245/:304` pair (spec F13).

## R19 — Part V corrections (spec F12; quality reversibility)

Row 1 (artifactType position) gains the discovery consequence and requires
re-confirming R5's filter decision with it. Row 2 (`STATEMENT_TYPE_WRITTEN`
flip) gains "and amend D-b + the signing.md deviation paragraph". Add R1/R2/R4
decisions to the MAY-NOT list (verify composition, accepted kinds, annotation
set). The five findings sec flagged as outside Part V (B2/B3/B4/B7/B8) are
resolved HERE, by this directive — the spike inherits them as settled.

## R20 — One-liners

- D-h: `TrustPolicyError::NoBackend` inherits exit 78 slug
  `trust_policy_invalid` via the existing `#[from]` chain (sec N4 — one sentence).
- CycloneDX summary: probe `specVersion` first, dispatch, then parse
  (DATA-FMT-02 shape); `unbounded_depth` stays off (sec N2).
- `signing.md` surface row gains the payloadType-deviation sentence (SOTA).
- Docs surface: `signing.md` Current Limitations already forced (sec N8) — keep.

