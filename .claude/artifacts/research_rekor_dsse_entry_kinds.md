# Research: Rekor v1 DSSE entry kinds (targeting rekor-server v1.4.2)

> hex-architect research axis (2026-08-20), worker: sonnet researcher. Persisted
> by the orchestrator (worker toolset had no Write). Feeds
> `adr_sbom_attestations.md`.

## 1. Entry kinds for DSSE on Rekor v1 — `dsse` vs `intoto`

Both exist in rekor v1.4.2 (the tag OCX's docker-compose stack pins):

- **`dsse`** — only version `0.0.1`. Wrapper schema `oneOf`s a single ref:
  `v0.0.1/dsse_v0_0_1_schema.json`. Introduced in **rekor v1.2.0** ("add dsse
  type", sigstore/rekor#1487) — present in v1.4.2 (no removal since).
- **`intoto`** — versions `0.0.1` and `0.0.2`. `0.0.2` landed in v0.12.0
  (sigstore/rekor#973); v1.2.0 later relaxed 0.0.1's PayloadHash requirement.

**Proposed-entry schema, `dsse:0.0.1`**:
- Input (`proposedContent`, write-only): `envelope` (stringified JSON DSSE
  envelope), `verifiers` (array, >=1, base64 verification-material items —
  cert PEM or raw pubkey bytes).
- Output (read-only, `oneOf` with input): `signatures[]` (`signature` b64,
  `verifier` b64 — the verifier/cert travels with the persisted record, unlike
  hashedrekord), `envelopeHash{algorithm:"sha256",value}`,
  `payloadHash{algorithm:"sha256",value}`.
- No `attestation` field anywhere in this schema (see §4).

**Proposed-entry schema, `intoto:0.0.2`**: input
`content.envelope{payload (b64, write-only), payloadType, signatures[]{keyid?,
sig, publicKey}}`; output `hash{sha256}`, `payloadHash{sha256}`.

## 2. What cosign v3 uploads for `cosign attest`

Cosign v3.1.2. `pkg/cosign/tlog.go` builds proposed entries via `dsseEntry()`:

```go
func dsseEntry(ctx context.Context, signature, pubKey []byte) (models.ProposedEntry, error) {
    ...
    return types.NewProposedEntry(ctx, dsse.KIND, dsse_v001.APIVERSION, types.ArtifactProperties{
        ArtifactBytes:  signature,
        PublicKeyBytes: pubKeyBytes,
    })
}
```

i.e. kind `dsse`, version `0.0.1`.

**Caveat:** the `proposedEntries()` pairing read in `tlog.go` is reachable from
`FindTLogEntry` — verification-side reconstruction (search the log by trying
both candidate bodies), not a first-party quote of the literal upload call
site. Secondary confirmation (Rekor v2 blog, cosign issue history) but flag
for the implementation spike: our own upload against rekor v1.4.2 settles it
empirically.

## 3. What the `dsse:0.0.1` canonicalized body commits to

Exact source, `pkg/types/dsse/v0.0.1/entry.go` `Canonicalize()`:

```go
canonicalEntry := models.DSSEV001Schema{
    Signatures:      v.DSSEObj.Signatures,   // sorted lexicographically by signature
    EnvelopeHash:    v.DSSEObj.EnvelopeHash,
    PayloadHash:     v.DSSEObj.PayloadHash,
    ProposedContent: nil,
}
itObj := models.DSSE{APIVersion: &APIVERSION, Spec: &canonicalEntry}
return json.Marshal(&itObj)
```

Canonicalized `body` = `{apiVersion:"0.0.1", spec:{signatures:[...sorted...],
envelopeHash, payloadHash}}` — the raw envelope is explicitly excluded ("we
don't want to canonicalize the envelope", source comment). To cross-check
against a held envelope: recompute `sha256(envelope_json_bytes)` and
`sha256(decoded_payload_bytes)`, match `envelopeHash`/`payloadHash`.

**SET mechanics are kind-agnostic**: SET signs `{logID, logIndex, body,
integratedTime}` regardless of kind. The server hands back
`canonicalizedBody` verbatim, so a verifier checks SET/Merkle over those bytes
and separately cross-checks the hashes inside against the envelope it holds.

## 4. `intoto:0.0.2` vs `dsse:0.0.1` — attestation storage

`intoto:0.0.2` implements `AttestationKeyValue()` gated by server
`max_attestation_size` (persists the decoded payload server-side).
`dsse:0.0.1` has no such method at all — source comment: "AttestationKey and
AttestationKeyValue are not implemented so the envelopes will not be persisted
in Rekor."

Why cosign moved intoto→dsse: **Rekor v2 GA drops every type except
`hashedrekord` and `dsse`** — attestation storage in the log was discontinued
("persist attestations alongside artifacts rather than storing them in the
log"). dsse's no-persistence design is the v2 forward shape; the migration
tracks a storage-model decision, not cosmetics.

Wire note: proposed-entry submission uploads the full envelope regardless of
persistence; cosign#3599 reports a 130MB attestation rejected by the public
instance (limit exists; exact number unconfirmed — likely ingress body cap,
not `max_attestation_size`).

## 5. sigstore-rs 0.14 DSSE support — more capable than assumed

`sigstore::bundle::Bundle.content: Option<Content>` where `Content` is
`MessageSignature | DsseEnvelope` — DSSE is first-class.

`Verifier::verify_bundle_content()` (verifier.rs:56-85) branches on content:
- `MessageSignature`: `verify_prehash(sig, input_digest)`.
- **`DsseEnvelope` (66-83): verifies the DSSE signature over PAE bytes, then
  compares the in-toto subject digest against the caller-supplied
  `input_digest`.**

`verify_digest`/`verify` (117-255) run the same 7-step pipeline for both
kinds (cert chain → SCT → policy → signature → Rekor log-entry consistency →
cert-validity-at-signing-time).

**Consequence for OCX:** `CosignVerificationKey` SET check,
`InclusionProof::verify`, and `SigstoreTrustRoot` are generic over entry kind —
they work unchanged against a `dsse` entry, with OCX reconstructing the dsse
canonicalized body per §3. Separately, sigstore-rs's own `Verifier` has an
independent complete DSSE path (PAE + subject-digest) worth delegating to
where it fits.

(sigstore-rs#393 is about private trust roots for GitHub's attestation
service, not a DSSE gap — do not misread.)

## 6. `POST /api/v1/log/entries` with a `dsse` proposed entry

- **201 Created** — full entry, `ETag` + `Location`.
- **400** `BadContent`; **409 Conflict** — already logged, `Location` points at
  the existing entry (standard semantics, not dsse-specific); default 5xx.
- Fulcio-cert verifier: `proposedContent.envelope` (stringified envelope JSON)
  + `proposedContent.verifiers = [base64(cert PEM)]` (one-element array in
  cosign's usage).

## 7. Public-good Rekor restrictions

No primary-source byte cap confirmed ("~100KB" is folklore). Solid:
`max_attestation_size` is structurally irrelevant to `dsse` (never persists);
the upload wire size has some ingress-level cap (cosign#3599).

## Sources

- rekor v1.4.2: releases page; `pkg/types/dsse/v0.0.1/{entry.go,schema}`;
  `pkg/types/intoto/v0.0.2/entry.go`; `openapi.yaml`; CHANGELOG
- Rekor v2 GA blog (kind list, intoto removal, attestation-storage
  discontinuation)
- cosign `pkg/cosign/tlog.go` (v3.1.2), cosign#3599
- sigstore-rs 0.14: `bundle::Bundle`, `bundle::verify::Verifier` + verifier.rs
  source; docs.sigstore.dev logging overview

**Flagged weak points:** (a) cosign v3 literal upload call site unquoted
(verify-side reconstruction only) — settle in the implementation spike;
(b) public-Rekor size cap unsourced.
