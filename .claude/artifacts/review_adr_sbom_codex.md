Codex model: `gpt-5.6-terra` (terra tier, cross-model default reviewer)
Review type: one-shot adversarial design review of `adr_sbom_attestations.md`, read-only sandbox, no fix loop, no code edits

---

VERDICT: 3 findings (3 Block, 0 Warn)

1. **ADR line ref**: `adr_sbom_attestations.md:581`, `adr_sbom_attestations.md:786`; `sigstore-0.14.0/src/bundle/verify/models.rs:475-495`
   - **Severity**: Block
   - **Defect**: The ADR requires `dsse:0.0.1` `payloadHash` to be SHA-256 over PAE, but the pinned `sigstore` 0.14 verifier checks it against the raw decoded payload before OCX's proposed hook runs. This also contradicts the ADR's own cited research, which describes `payloadHash` as the decoded payload.
   - **Failure scenario**: Rekor/cosign emits a normal DSSE entry with `payloadHash = sha256(decoded payload)`. Delegated verification succeeds, then `verify_tlog_binding` compares it to `sha256(PAE(...))` and rejects every valid external attestation.
   - **Suggested fix**: Define the v1 `dsse:0.0.1` check as raw-payload hashing, matching Rekor and the pinned crate; reserve PAE hashing for the separately deferred hashedrekord/Rekor-v2 path.

2. **ADR line ref**: `adr_sbom_attestations.md:312-330`, `adr_sbom_attestations.md:1200-1213`; `sigstore-0.14.0/src/bundle/verify/models.rs:264-280`, `sigstore-0.14.0/src/bundle/verify/verifier.rs:76-80`
   - **Severity**: Block
   - **Defect**: The claimed all-subject binding cannot run as specified after `verifier.verify`: `sigstore` 0.14 extracts only `subject[0]` and rejects it unless it matches the target before OCX's post-verification `verify_envelope` is reached.
   - **Failure scenario**: A valid external attestation lists a shared artifact as `subject[0]` and the requested OCX manifest as `subject[1]`. The normative all-subject rule would accept it, but `sigstore` rejects it first, so OCX cannot interoperate with that valid statement.
   - **Suggested fix**: Obtain or maintain an upstream/patchable verification path that supports any matching subject, or explicitly narrow the ADR's acceptance contract to first-subject-only before implementation.

3. **ADR line ref**: `adr_sbom_attestations.md:433-445`, `adr_sbom_attestations.md:1511`, `adr_sbom_attestations.md:1703`
   - **Severity**: Block
   - **Defect**: D-e says annotations must never exclude a candidate, yet `AttestationNotFound` and its required fixture explicitly make "annotation narrowing removed every candidate" produce NotFound. Those contracts authorize the omission attack D-e says is forbidden.
   - **Failure scenario**: A registry rewrites every referrer's unsigned predicate-type annotation to a different value. `sbom --type cyclonedx` narrows the set to zero and reports that no attestation exists even though a valid signed CycloneDX predicate is present.
   - **Suggested fix**: Never derive NotFound from annotation narrowing. Parse all artifact-type candidates within a bounded scan; if a cap prevents a complete scan, return an explicit incomplete/budget refusal rather than absence.

---

## Orchestrator corroboration notes (added by codex-adversary-sbom, not part of Codex's raw output)

All three findings were independently spot-checked against the actual pinned
`sigstore = "0.14.0"` source (found at
`/mnt/wslg/distro/home/mherwig/.cache/ocx-codex/sigcheck/sigstore-0.14.0/`,
a local research cache from this project's earlier `sigstore-rs` spike) and
the ADR text itself:

- **Finding 1 confirmed.** `models.rs:488` computes
  `expected_payload_hash: [u8; 32] = Sha256::digest(payload_bytes).into()`
  where `payload_bytes` is documented at `models.rs:183` as "Raw payload
  bytes from the DSSE envelope (used to verify `payloadHash`)" — i.e. the
  decoded payload, not the PAE. The ADR's own text at line ~583 states
  `payloadHash` — `sha256` over the PAE of the received envelope, a direct
  contradiction with the pinned crate's actual behavior.
- **Finding 2 confirmed.** `verifier.rs:71-80` shows the delegated DSSE
  verification arm does `let expected_hex = hex::encode(input_digest); if
  subject_sha256_digest != &expected_hex { return
  Err(SignatureErrorKind::Transparency); }` — a hard fail *inside* the
  crate's own `verifier.verify()` call, before OCX's `verify_envelope` layer
  ever runs. The ADR's D-d rationale (line ~330) already documents that
  `subject_sha256_digest()` reads `.first()` only, but frames the "all
  subjects" gap as something OCX's own post-hoc layer closes — it does not
  account for the fact that the delegated call rejects a non-`subject[0]`
  match outright.
- **Finding 3 confirmed as an internal-consistency defect.** The ADR's own
  text at line ~433 states "Annotations are ordering and pre-filter hints
  only — never an exclusion," while the error table at line ~1511
  (`AttestationNotFound`, exit 79, "after annotation narrowing") and the
  fixture at line ~1703 ("Annotation narrowing empties the candidate set...
  `AttestationNotFound` (79)") both make annotation narrowing produce an
  absence result — the exact class of exclusion D-e forbids.

No fourth finding was manufactured to pad the count; Codex's verdict line
matches its enumerated findings (3 findings, 3 Block, 0 Warn) and its own
prompt-following was disciplined (no code edits attempted, `write: false`
confirmed on the underlying job record).
