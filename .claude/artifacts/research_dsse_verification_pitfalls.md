# Research: DSSE/in-toto attestation verification pitfalls (OCI referrers)

> hex-architect research axis (2026-08-20), worker: sonnet researcher. Persisted
> by the orchestrator (worker toolset had no Write). Feeds
> `adr_sbom_attestations.md`. Every claim carries a primary spec quote or a
> named CVE/advisory.

## 1. PAE byte construction

Exact spec (secure-systems-lab/dsse `protocol.md`):

```
PAE(type, body) = "DSSEv1" + SP + LEN(type) + SP + type + SP + LEN(body) + SP + body
SIGNATURE = Sign(PAE(UTF8(PAYLOAD_TYPE), SERIALIZED_BODY))
```

- `SP` = single ASCII space (0x20); `LEN(s)` = ASCII decimal byte length, no
  leading zeros.
- `SERIALIZED_BODY` = the **raw decoded payload bytes** — never the base64
  transport text.
- Classic bugs: (a) signing the base64 string; (b) signing payload without
  payloadType; (c) re-deriving PAE from a re-serialized payload instead of the
  exact received bytes (§9).
- The DSSE spec has **no Security Considerations section** — it is a
  wire-format contract, not a threat model; the attack surface below comes
  from in-toto's spec and Sigstore's CVE history.

## 2. Subject binding

- in-toto Statement v1: subjects match **purely by digest**; `name` is
  informational (policy add-on at most).
- **The OCI referrers subject linkage is unsigned registry metadata.** The
  referrer manifest's `subject` descriptor is outside the DSSE signature; a
  hostile registry can serve a validly-signed attestation for artifact A as a
  referrer of artifact B.
- Real instance: **CVE-2026-31830** (sigstore-ruby ≤0.2.2) — `verify_in_toto`'s
  digest-mismatch failure was discarded, so "an attacker who possesses a valid
  signed DSSE bundle containing an in-toto attestation for artifact A can
  present it as a valid attestation for a different artifact B."
- Rule: the only authoritative binding is `statement.subject[].digest.sha256`
  inside the verified payload, compared against the target digest **computed
  by OCX itself**.

## 3. predicateType binding

- **CVE-2022-35929 / GHSA-vjxv-45g9-9296** (cosign <1.10.1):
  `verify-attestation --type spdx` passed against an image carrying only a
  `vuln` attestation — the check was "any attestation validly signed", not
  "the attestation whose SIGNED predicateType matches".
- Manifest `artifactType`/annotations are discovery hints only; the value
  gating parse/policy is `predicateType` inside the verified payload.

## 4. payloadType binding

- `payloadType` is PAE-covered. DSSE rationale: "two different applications
  could use the same encoding (e.g. JSON) but interpret the payload
  differently."
- Verifier obligation: check `payloadType == "application/vnd.in-toto+json"`
  explicitly before parsing as a Statement — never infer type from a
  successful JSON parse.

## 5. Multi-signature envelopes / threshold

- DSSE itself is `(t,n)`-threshold-capable.
- **Sigstore Bundle v0.3 constrains the wire shape**: DsseEnvelope is
  "restricted to exactly one signature, requiring verifiers to validate
  payload type and reject envelopes with multiple signatures"
  (sigstore_bundle.proto).
- For the bundle path OCX targets: hard-check `len(signatures) == 1`,
  **reject** otherwise — never "verify [0], ignore rest" and never
  "accept if any verifies" (signature-stuffing).
- Raw multi-signer DSSE outside the bundle path would need explicit
  configured `(t,n)`; not in v1 scope.

## 6. keyid and algorithm selection

- DSSE: `keyid` is an "optional... unauthenticated hint" and "MUST NOT be
  used for security decisions". Lookup optimization only.
- No `alg` field exists in DSSE (structurally safer than JWT); the discipline
  generalizes: the crypto primitive comes from the pinned trust root's
  key-type record, never from any envelope field. (JWT alg-confusion analogy —
  reasoned, not a DSSE CVE.)

## 7. Tlog binding for DSSE entries

- Rekor dsse:0.0.1 stores two hashes: `PayloadHash` (decoded payload) and
  `EnvelopeHash` (raw envelope JSON).
- Payload-hash-only checking proves "this content was logged by someone" —
  not that the presented signature/cert/keyid was logged. Envelope-hash
  binds payload + signatures + keyid + payloadType. Generalized form of
  **GHSA-8gw7-4j42-w388** (cosign <1.12.0, verify-blob rekorBundle splicing).
- SET = signature over canonical JSON of `{canonicalized_body, log_index,
  log_id, integrated_time}` — the one place RFC-8785-style canonical JSON
  matters (vs PAE, which never canonicalizes).
- **CVE-2024-55655** (sigstore-java 2.0.0): the `integratedTime ∈
  [NotBefore, NotAfter]` check was silently regressed — an attacker with a
  stolen short-lived key could sign indefinitely. OCX must assert this check
  itself, not assume a library default.
- **CVE-2024-54140 / GHSA-jp26-88mw-89qr** (sigstore-java <1.2.0): checkpoint
  signature not verified — "a bundle may provide an inclusion proof that
  doesn't actually correspond to the log in question." Inclusion-proof
  recomputation is meaningless without authenticating the checkpoint against
  the pinned Rekor key.

## 8. Size/DoS

- cosign#3599: 130 MB attestation rejected by Rekor ingest — both dsse and
  intoto proposed entries upload the full payload.
- OCX: attestation ingestion is a **new artifact class** under the existing
  PKG-04..07 bounded-ingestion discipline — its own named constants, not a
  silent reuse of the 512 KiB signature-bundle cap:
  - `MAX_ATTESTATION_ENVELOPE_BYTES` (proposed **32 MiB**) via bounded read on
    raw fetched bytes, before base64 decode or JSON parse; never trust
    Content-Length/descriptor size alone.
  - Separate decoded-payload cap (base64 expansion is a fixed ~4/3 ratio —
    checkable before allocating the decode buffer).
  - Count cap on referrers fetched per subject (attacker-controlled registry
    can return an unbounded list).

## 9. Canonicalization — two layers, never blended

- **PAE: no canonicalization.** Signs the exact raw bytes received. DSSE spec:
  "implementations MUST NOT re-parse the envelope after verification to pull
  out the payload" — never unmarshal → re-marshal → hash; a struct round-trip
  is not byte-identical. Pass forward the original decoded bytes.
- **Rekor SET/checkpoint: canonical JSON (RFC 8785-flavored)** — required so
  the log's signature is reproducible across implementations. Different layer,
  different reason.

## 10. In-toto Statement validation minimums

- `_type`: accept exactly `https://in-toto.io/Statement/v1`; v0.1 is legacy.
  No normative MUST-reject exists — **explicit design decision to fail closed
  on any other `_type`** (recommended; flag in ADR as a decision, not
  spec-forced).
- Reject a Statement with **zero subjects** or no subject matching the target
  digest (the empty case degrades to CVE-2026-31830 by omission).
- `digest` is a DigestSet map: hardcode sha256 (matches OCI digest grammar /
  DATA-DIG rules); never accept a match on a weaker co-present algorithm
  (md5/sha1 entries must not satisfy the check).
- Multiple subjects legal: require the target digest to be present among
  them, not to be the only one.

## 11. Offline verify — provable vs not

Provable offline from a complete bundle: DSSE signature (PAE + leaf cert),
cert chain to pinned Fulcio root + identity policy, integratedTime within
cert validity, Merkle inclusion against the bundled checkpoint, checkpoint
signature against the pinned Rekor key (the CVE-2024-54140 check).

Not provable offline: log consistency/non-equivocation (split-view needs
online consistency proofs or witnesses), and **attestation freshness** —
DSSE+Rekor prove "validly signed and logged at T" forever; they say nothing
about a newer superseding SBOM. OCX's digest-pinned lockfile closes classic
artifact rollback independently; attestation staleness is a policy-layer
concern. Per SEC-32: docs must not imply freshness/rollback protection that
is not built.

## Compliant-verifier checklist (lift into the ADR)

| # | A compliant OCX DSSE/attestation verifier MUST/SHOULD | Cites |
|---|---|---|
| 1 | Recompute PAE over the decoded payload bytes (never the base64 string, never a re-serialized struct); reject on mismatch | §1 DSSE spec |
| 2 | Never re-parse/re-serialize the verified payload before downstream use — pass the original bytes | §1 §9 DSSE spec |
| 3 | Check `payloadType == "application/vnd.in-toto+json"` before Statement parse | §4 DSSE spec |
| 4 | Compare `statement.subject[].digest.sha256` against the locally computed target digest; never trust referrers linkage or annotations as binding | §2 CVE-2026-31830 |
| 5 | Reject zero-subject Statements and Statements with no subject matching the target | §10 |
| 6 | Match on hardcoded sha256 only within DigestSet; weaker co-present algorithms never satisfy | §10 |
| 7 | Read policy-relevant predicateType from the verified payload, never from unsigned annotations | §3 CVE-2022-35929 |
| 8 | Bundle path: hard-reject `len(signatures) != 1` | §5 Bundle v0.3 |
| 9 | (Non-bundle multi-signer: explicit configured (t,n) — out of v1 scope) | §5 |
| 10 | `keyid` = lookup hint only | §6 DSSE spec |
| 11 | Verification algorithm from the pinned trust root's key-type record, never an envelope field | §6 |
| 12 | Tlog binding via EnvelopeHash (or both hashes), never PayloadHash alone | §7 GHSA-8gw7-4j42-w388 |
| 13 | Assert `NotBefore <= integratedTime <= NotAfter` as OCX's own check | §7 CVE-2024-55655 |
| 14 | Verify the checkpoint signature against the pinned Rekor key before trusting any inclusion proof | §7 §11 GHSA-jp26-88mw-89qr |
| 15 | `MAX_ATTESTATION_ENVELOPE_BYTES` (proposed 32 MiB) bounded read on raw bytes, separate from the 512 KiB signature cap | §8 cosign#3599 PKG-04..07 |
| 16 | Separate decoded-payload cap (fixed ~4/3 base64 expansion) | §8 |
| 17 | Cap referrer/attestation count fetched per subject | §8 |
| 18 | Reject any `_type` ≠ `https://in-toto.io/Statement/v1` (explicit decision) | §10 |
| 19 | Per SEC-32: document that verification proves authenticity+integrity at signing time, not freshness; imply no rollback protection unless built | §11 |
| 20 | Fail closed on every verification-path exception; never fall through to success on incomplete input | §7 GHSA-8gw7-4j42-w388 |

## Sources

DSSE protocol.md · in-toto Statement v1 + ResourceDescriptor + v0.1.0 README ·
CVE-2026-31830 (sigstore-ruby) · GHSA-vjxv-45g9-9296 / CVE-2022-35929 (cosign)
· GHSA-8gw7-4j42-w388 (cosign) · CVE-2024-55655 + GHSA-jp26-88mw-89qr
(sigstore-java) · sigstore protobuf-specs sigstore_bundle.proto · rekor
pkg/types/dsse/v0.0.1/entry.go · cosign#3599 · RFC 8785 · PortSwigger JWT
alg-confusion (analogy) · Chainguard Academy Rekor intro · Sigstore
trusted-time blog
