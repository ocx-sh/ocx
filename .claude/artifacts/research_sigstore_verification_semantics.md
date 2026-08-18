# Research: Sigstore Verification Semantics & cosign v3 Interop

Axis for ADR (milestone 5, `feat/signing-and-trust`, PR #203). Covers issues
#206 (X.509 parsing), #207 (Fulcio chain + temporal validity), #208 (SCT/CT-log),
#209 (Rekor SET + Merkle inclusion), #210 (TUF trust root), #197 (cosign v3 interop).

Status: COMPLETE. Appended incrementally during research; sections landed in this file
order: §2, §5, §1, §3, §4, §6/7, §8, §9 (not 1–9 sequential) — use the numbered headings
to navigate, not document order. The single highest-priority finding for the ADR is in
**§3d**: `sigstore-rs` 0.14 (the pinned version) does not itself call Merkle-inclusion or
SET verification from its top-level `Verifier::verify()`, despite shipping the
primitives needed for both — "just call sigstore-rs's verify()" is not sufficient
for #209 on its own.

---

## 2. Bundle format v0.3 — protobuf schema and what cosign 3.x requires

Source: [`sigstore/protobuf-specs` — `protos/sigstore_bundle.proto`](https://github.com/sigstore/protobuf-specs/blob/main/protos/sigstore_bundle.proto)
(raw fetch of `main`), cross-checked against [docs.sigstore.dev/about/bundle](https://docs.sigstore.dev/about/bundle/).

```proto
message Bundle {
    string media_type = 1;
    VerificationMaterial verification_material = 2 [(google.api.field_behavior) = REQUIRED];
    oneof content {
        dev.sigstore.common.v1.MessageSignature message_signature = 3 [(google.api.field_behavior) = REQUIRED];
        io.intoto.Envelope dsse_envelope = 4 [(google.api.field_behavior) = REQUIRED];
    }
    reserved 5 to 50;
}

message VerificationMaterial {
    oneof content {
        dev.sigstore.common.v1.PublicKeyIdentifier public_key = 1 [(google.api.field_behavior) = REQUIRED];
        dev.sigstore.common.v1.X509CertificateChain x509_certificate_chain = 2 [(google.api.field_behavior) = REQUIRED];
        dev.sigstore.common.v1.X509Certificate certificate = 5 [(google.api.field_behavior) = REQUIRED];
    }
    repeated dev.sigstore.rekor.v1.TransparencyLogEntry tlog_entries = 3;
    TimestampVerificationData timestamp_verification_data = 4;
}

message TimestampVerificationData {
    repeated dev.sigstore.common.v1.RFC3161SignedTimestamp rfc3161_timestamps = 1;
}
```

**What changed v0.2 → v0.3 (this is the load-bearing fact for #209/#197):** `media_type`
MUST be `application/vnd.dev.sigstore.bundle.v0.3+json` when JSON-encoded, and a
conformant reader MUST still accept the older `v0.1`/`v0.2` media types for back-compat.
The `VerificationMaterial.content` oneof has three forms:

1. `public_key` — a `PublicKeyIdentifier` (non-keyless / self-managed key signing)
2. `x509_certificate_chain` — the **full chain**, leaf through root
3. `certificate` — a **single leaf certificate only** (field 5, added later)

Per-version normative requirement pulled from the proto's own comments: **for keyless
signing against the Public Good Instance, v0.1/v0.2 bundles MUST use form (2)** (the
full chain embedded in the bundle), while **v0.3 bundles MUST use form (3)** (single
leaf cert only — intermediates are *not* re-embedded because the client is expected to
already hold Fulcio's intermediate/root from the TUF trust root, not from the bundle).
Form (1) (`public_key`) MUST NOT be used for the Public Good Instance keyless path.

**Practical consequence for ocx's verifier (#206/#207):** a real cosign-3.x-produced
v0.3 bundle will hand the verify pipeline exactly **one** DER certificate, not a chain.
The intermediate-and-root half of the chain the pipeline needs to walk (#207) must come
from the **trust root** (Fulcio's `certificate_authorities` in the TUF-distributed
`trusted_root.json`, #210), never from the bundle's `verification_material` itself in
v0.3. A verifier that assumes "the chain is always in the bundle" (true pre-v0.3, and
still true if talking to an older producer) will misparse a v0.3-form-(3) bundle if it
requires `x509_certificate_chain` unconditionally — the pipeline must handle both oneof
arms.

`TransparencyLogEntry` (field 3, repeated) is the Rekor SET/inclusion-proof carrier
researched in detail in §3. `timestamp_verification_data` (field 4) carries RFC 3161
TSA responses as an alternative/supplement to the Rekor timestamp route from client-spec
step 1.1 above. Per the bundle-format page: "bundles must include at least one
transparency log's signed entry timestamp or an RFC3161 timestamp to provide proof that
signing occurred during the certificate's validity window" — i.e. **at least one of
`tlog_entries` or `timestamp_verification_data` is mandatory**, not both, but never
neither.

---

## 5. Fulcio certificate profile

Source: [`sigstore/fulcio` — `docs/certificate-specification.md`](https://github.com/sigstore/fulcio/blob/main/docs/certificate-specification.md)
and [`docs/oid-info.md`](https://github.com/sigstore/fulcio/blob/main/docs/oid-info.md) (raw
fetches of `main`), cross-checked with [docs.sigstore.dev/certificate_authority/oidc-in-fulcio](https://docs.sigstore.dev/certificate_authority/oidc-in-fulcio/).

### Certificate chain shape and validity

| Cert | Key usage | Basic constraints | Validity | Notes |
|---|---|---|---|---|
| Root | Certificate Sign, CRL Sign | `CA:TRUE` | ~10 years (recommended) | Subject Key Identifier required |
| Intermediate | Certificate Sign, CRL Sign | `CA:TRUE`, `pathlen:0` recommended | ~3 years, ≤ parent | AKI must equal parent's SKI — this is the chain-walk anchor for #207 |
| Leaf (issued) | Digital Signature only | — | ephemeral (Fulcio issues ~10-minute-lifetime certs in practice; the spec only says "cannot exceed parent lifetime, ephemeral keys recommended") | exactly one SAN, critical extension |

The 10-minute figure is the well-known operational value for the public-good Fulcio
instance (short-lived cert + Rekor timestamp = the "hybrid trust model" in §1) — the
spec itself does not hardcode a number, so a self-hosted Fulcio can choose differently;
ocx's verifier must not assume a fixed lifetime, only that `notBefore`/`notAfter` bound
a short window that the Rekor `integratedTime` must fall inside.

### OIDs (full table, `.1.1`–`.1.6` deprecated GitHub-only, `.1.7`+ current provider-generic)

| OID (`1.3.6.1.4.1.57264.1.`) | Status | Meaning |
|---|---|---|
| `.1` | **deprecated** | OIDC token issuer (issuer v1) — still emitted for back-compat, do not pin against it |
| `.2`–`.6` | deprecated | GitHub-specific: event trigger, commit SHA, workflow name, repository, git ref |
| `.7` | current | OtherName SAN (username identity, RFC 5280 `otherName`) |
| `.8` | **current — "Issuer (V2)"** | OIDC token issuer, DER-encoded string. **This is the OID `[[trust.policy]]` (#98) should pin against per the client spec's "SHOULD check the Issuer extension (OID 1.3.6.1.4.1.57264.1.8) at a minimum."** |
| `.9` | current | Build Signer URI |
| `.10` | current | Build Signer Digest |
| `.11` | current | Runner Environment (hosted vs self-hosted) |
| `.12` | current | Source Repository URI |
| `.13` | current | Source Repository Digest |
| `.14` | current | Source Repository Ref (branch/tag) |
| `.15`–`.17` | current | Repository identifier, owner URL, owner identifier |
| `.18` | current | Build Config URI |
| `.19` | current | Build Config Digest |
| `.20` | current | Build Trigger (e.g. `push`) |
| `.21` | current | Run Invocation URI |
| `.22` | current | Repository visibility at signing time |
| `.23` | current | Deployment environment |
| `.24` | current | Token Subject (raw OIDC `sub` claim) |

### SAN by identity type

- **GitHub Actions**: SAN is the `job_workflow_ref` claim, formatted as a URI:
  `https://github.com/<owner>/<repo>/.github/workflows/<workflow>.yml@<ref>` (e.g.
  `@refs/heads/main`). Chosen specifically so a *reusable* workflow's callers all share
  one SAN, centralizing policy — `[[trust.policy]]` pinned to a `job_workflow_ref` covers
  every caller of that reusable workflow, not just one repo.
- **GitLab CI**: SAN is `ci_config_ref_uri` (the path to `.gitlab-ci.yml`, analogous role
  to `job_workflow_ref`). Additional claims (`namespace_path`, `project_path`,
  `pipeline_id`, `job_id`, `ref`, `runner_environment`, `sha`, `pipeline_source`) map onto
  the generic `.1.9`–`.1.24` OID set the same way GitHub's claims do — GitLab is a
  provider-generic OIDC issuer, not a special-cased Fulcio code path.
- **Email/interactive OIDC**: SAN is an RFC 822 email address (the token's verified
  `email` claim); "exactly one" SAN is a **critical** extension either way.

### Issuer v1 vs v2 — practical note for #98/#206

The v1 issuer OID (`.1.1`) is **deprecated but still physically present** in certs
Fulcio issues today (both are stamped for back-compat). A parser must be able to read
both — rejecting a cert for lacking `.1.1` would break nothing since `.1.8` is what
policy should check, but a parser that *only* knows `.1.1` and never learns about `.1.8`
silently mis-reads the issuer on every newly-issued cert. #206's real-X.509-parsing work
should extract both and let policy consult `.1.8`.

---

## 1. The Sigstore client spec — mandatory verification steps

Source: [`sigstore/architecture-docs` — `client-spec.md`](https://github.com/sigstore/architecture-docs/blob/main/client-spec.md)
(fetched from `raw.githubusercontent.com/sigstore/architecture-docs/main/client-spec.md`).
This is the checklist ocx's verify pipeline (`crates/ocx_lib/src/oci/verify/pipeline.rs`)
should be held against issue by issue.

**Framing.** "If any step fails, abort verification unless otherwise specified." The
spec is explicitly a hybrid-trust model: a short-lived Fulcio cert is trusted only if a
timestamp exists proving the signature was made *while the cert was valid* — that
timestamp comes from either an RFC 3161 TSA or a Rekor V1 log entry's `integratedTime`.

### Step 1 — Establish the signature timestamp (do this FIRST, before cert path validation)

Two routes, at least one MUST be used:

- **1.1 Timestamping Service (RFC 3161) route**: "The Verifier MUST verify the
  timestamping response using the Timestamping Service root key material." Output: a
  Unix timestamp. "If verification or timestamp parsing fails, the Verifier MUST abort."
- **1.2 Rekor V1 Transparency Service route**: verify the signature on the `LogEntry`
  against the pre-distributed Rekor root key, then parse `integratedTime` as the Unix
  timestamp. Same abort-on-failure rule.

This timestamp becomes the "current time" fed into every subsequent temporal check —
**not** wall-clock `now()`. This is the answer to research question 8 below.

### Step 2 — Certificate chain verification

- **2.1 Path validation (RFC 5280 §6)**: "The Verifier MUST perform certification path
  validation … of the certificate chain with the pre-distributed Fulcio root
  certificate(s) as a trust anchor," checking validity **against the Step-1 timestamp**,
  not against real-world now. This is exactly issue #207's "leaf → intermediate →
  root" chain-walk requirement plus its temporal-validity requirement — the spec treats
  them as one inseparable step, not two.
- **2.2 SCT verification** (unless doing online CT-log verification): "the Verifier
  MUST extract the `SignedCertificateTimestamp` embedded in the leaf certificate, and
  verify it as in RFC 6962 §3.2, using the verification key from the Certificate
  Transparency Log." Mandatory for offline verification — this is issue #208.
- **2.3 Policy checks**: "The Verifier MUST then check the certificate against the
  verification policy" — "SHOULD check the `Issuer` X.509 extension (OID
  `1.3.6.1.4.1.57264.1.8`) at a minimum, and will in most cases check the
  `SubjectAlternativeName` as well." This is ocx's `[[trust.policy]]` identity pinning
  (#98) — confirms the OID to pin against is the **v2** issuer OID, not v1's
  `.1.1` (deprecated, still present for back-compat but not the one to check).

### Step 3 — Transparency log entry validation (Rekor)

- **3.1 Parse `body`**: base64-encoded JSON with `apiVersion`/`kind`. "The Verifier
  implementation contains a list of supported Transparency Service formats." Minimum:
  "the Verifier MUST define a set of types it supports and at a minimum SHOULD support
  verifying `hashedrekord` (V1 and V2) and `dsse` (V1 only) entries."
- **3.2 Three invariants** the parsed body must satisfy against the *out-of-band*
  material the verifier already has:
  1. the signature in the parsed body == the provided signature
  2. the key/cert in the parsed body == the input certificate
  3. the "subject" of the parsed body matches the artifact being verified
- **3.3 DSSE handling**: artifact hash = `Hash(PAE(payloadType, payload))`; the entry's
  `signature.content` must equal `dsse_envelope.signatures[0].sig` byte-for-byte.

This is the answer to research question 3's "how a client reconstructs the signed
payload" at the Rekor-entry level — the SET/Merkle-proof wire format itself is a
separate layer, researched below in §3.

### Step 4 — Artifact signature verification

- **4.1** Construct the signed payload per the metadata format: raw bytes, a digest, or
  a DSSE envelope (`payloadType` MUST be `application/vnd.in-toto+json`; "Verifier MUST
  ensure that the artifact's digest/algorithm tuple is present in the list of subjects
  in the in-toto statement").
- **4.2** "The Verifier MUST verify the provided signature for the constructed payload
  against the key in the leaf of the certificate chain."

### Trust roots required (the full set a conformant verifier must hold)

Fulcio root cert(s); CT log public key; Timestamping Service root cert; Transparency
Service (Rekor) root key material; a verification policy. All five are what
`TrustRoot` (`oci/verify/trust_root.rs`) needs to model — today it holds a subset;
`load_embedded` stubs out the production values (issue #210).

### Permitted deviations (MAY)

- Omit TSA verification if using Rekor's timestamp instead, and vice versa.
- Perform **online** CT-log verification instead of offline SCT verification.
- Require threshold verification across multiple logs (not applicable to a single
  public-good Rekor instance, but relevant if ocx ever supports Rekor v2's multiple
  shards/tiles).

---

## 3. Rekor v1 SET wire format, checkpoint format, and Merkle inclusion proof

### 3a. The SET canonical wire format

Source: [`sigstore/rekor`](https://github.com/sigstore/rekor) `pkg/verify/verify.go`
(WebFetch summary of raw source; **UNCONFIRMED at field-name/line-number precision —
re-read the actual `.go` file before implementing**, this fetch degraded on the exact
struct name).

The SET is **not** a signature over the raw `LogEntryAnon` JSON as Go's `encoding/json`
would serialize it. It is a signature over a **canonicalized** subset:

1. Build a payload struct with exactly four fields: `body` (the base64 entry body,
   unchanged), `integratedTime` (int64), `logIndex` (int64 — "noted as virtual index" in
   the fetched summary, meaning the tree-relative index, not necessarily monotonic
   across shards), `logID` (string, the SHA-256 of the log's public key per RFC 6962-style
   log ID convention).
2. `json.Marshal()` that struct.
3. Run the result through **`github.com/cyberphone/json-canonicalization`**'s Go port
   (`jsoncanonicalizer.Transform`) — this is the reference implementation of what became
   **RFC 8785 (JSON Canonicalization Scheme, JCS)**.
4. The SET signature (ECDSA, Rekor's log key — public-good Rekor uses P-256/SHA-256) is
   verified over the canonicalized bytes from step 3.

**Direct, load-bearing implication for #209**: this canonicalization step is RFC 8785,
and the OCX crates-of-record rule (`.claude/rules/rust-cargo/crates-of-record.md`) already
names **`serde_json_canonicalizer`** as the canonical-JSON crate of record ("Canonicalisation
is a spec, not a sort"). `serde_json_canonicalizer` documents itself as RFC 8785-compatible
and aims for "100% compatibility with the RFC to be a suitable implementation in a
multi-language environment" — i.e. it is designed to be byte-identical with Go's
`cyberphone/json-canonicalization` on the same input, because both implement the same
spec. **Verifying a Rekor v1 SET is therefore composable from existing/pinned
primitives — `serde_json_canonicalizer` for the JCS step, plus a standard ECDSA
verify already available through `sigstore-rs`'s own crypto/`CosignVerificationKey`
type — not a wire-format parser that needs hand-rolling.** This satisfies the
non-negotiable ("no hand-rolled crypto / wire-format parsing") while still replacing
ocx's current non-standard `ocx-rekor-set-v1` payload.

### 3b. Inclusion promise vs inclusion proof (terminology, confirms client-spec §3)

- **Inclusion promise** = the SET itself. An *immediate* commitment: "the log promises
  to include this entry" (analogous to an RFC 6962 SCT, per Rekor's own docs — a
  timestamped promise made before the entry is actually merged into the tree).
- **Inclusion proof** = the actual Merkle audit path (§3c) proving the entry **is**
  present in a specific tree state, checked against a checkpoint. This is issued
  separately (and can be requested any time after the entry is merged) and is what
  issue #209 calls out as "never read" in ocx's current pipeline
  (`verify/pipeline.rs:289-294`).

A verifier holding only a SET has cryptographic proof the log *committed* to inclusion,
not that inclusion actually happened — the spec's hybrid model treats the SET as
sufficient for temporal-validity purposes (§1 step 1.2) but the **inclusion proof is
what makes an entry auditable/tamper-evident** against the public log's actual state.

### 3c. Merkle inclusion proof and checkpoint (signed tree head) verification

Source: RFC 6962 §2.1 (Merkle Tree Hash), fetched from `rfc-editor.org/rfc/rfc6962.html`;
Rekor's `pkg/util/checkpoint.go` (raw fetch, `main`).

**MTH (Merkle Tree Hash), RFC 6962 §2.1** — this is the exact algorithm a Rust
implementation (or sigstore-rs's own primitive, see below) must reproduce:

```text
MTH({})           = SHA-256()                                    (empty tree)
MTH({d(0)})       = SHA-256(0x00 || d(0))                        (single leaf)
MTH(D[n]), n > 1  = SHA-256(0x01 || MTH(D[0:k]) || MTH(D[k:n]))  (k = largest power of 2 < n)
```

The `0x00`/`0x01` prefix bytes are **domain separation** — without them, a leaf hash and
an internal-node hash could collide (second-preimage attack: an attacker crafts an
internal node's hash to equal some other leaf's hash). Any from-scratch Merkle
implementation that omits these prefixes is not RFC 6962-compliant and is a security
bug, not a style choice.

**Checkpoint / signed tree head (STH) format** — Rekor uses the `transparency-dev`
"signed note" convention (a.k.a. C2SP `tlog-checkpoint`), the same format Go's
`sumdb/note` package and `golang.org/x/mod` use. The signed text is:

```text
<origin>\n<size (decimal)>\n<hash (base64, standard encoding)>\n[<optional extra lines>]\n
```

produced via `fmt.Fprintf(&b, "%s\n%d\n%s\n", origin, size, base64.StdEncoding.EncodeToString(hash))`.
This is wrapped in a `SignedNote`: the text block, a blank line, then one or more
signature lines of the form `— <name> <base64(4-byte-keyhash || raw-signature)>`. A
client verifies a checkpoint by recomputing this exact text, then verifying the
signature line(s) against the log's known public key(s) — **not** against a JSON
encoding of the checkpoint; the note format is deliberately plain text for
human-diffability and is itself the canonicalization (no separate JCS step needed here,
unlike the SET).

**Inclusion proof verification (client side, general algorithm, matches RFC 6962 §2.1.1
plus the audit-path construction)**: take the entry's leaf hash (`SHA-256(0x00 ||
canonically-encoded entry)`), walk the supplied ordered list of sibling hashes
("audit path") up the tree recomputing parent hashes with the `0x01` prefix rule at each
level, and assert the final recomputed root equals the checkpoint's root hash — and
separately verify the checkpoint's own signature (above) against the log key. Both
checks are required; a proof that recomputes correctly against a checkpoint whose
signature was never checked proves nothing about the *log's* commitment to that state.

### 3d. sigstore-rs 0.14 — what it already has vs. what ocx must still wire (critical finding for #209)

`sigstore = "0.14"` is already pinned (`Cargo.toml`); crates.io confirms 0.14.0 is
genuinely the latest version (`updated_at: 2026-05-22`, DEP-01-compliant check against
the crates.io API). Two independent WebFetches of the pinned `v0.14.0` tag's
`src/bundle/verify/verifier.rs` (`raw.githubusercontent.com/.../v0.14.0/...` and the
GitHub blob view, cross-confirming) show the crate's **top-level `Verifier::verify()`**
performs, in order: cert chain validation, SCT validation, policy conformance, artifact
signature check, "Rekor consistency" (the CVE-2022-36056 mitigation, §9), and temporal
validity — but contains these two literal, unresolved comments immediately before the
method returns:

```text
// 5) Verify the inclusion proof supplied by Rekor for this artifact,
// if we're doing online verification.
// TODO(tnytown): Merkle inclusion; sigstore-rs#285

// 6) Verify the Signed Entry Timestamp (SET) supplied by Rekor for this
// artifact.
// TODO(tnytown) SET verification; sigstore-rs#285
```

**Neither Merkle inclusion proof verification nor SET verification is called from the
crate's own top-level `verify()`/`verify_digest()` entry point at the pinned version.**
This is exactly the gap issue #209 exists to close, and it means "just call
`sigstore::bundle::verify::Verifier::verify()` and delete our hand-rolled code" is
**not** a complete answer — that call alone would silently skip both checks issue #209
is about, even after the migration.

However — and this is the actionable half — PR [#285](https://github.com/sigstore/sigstore-rs/pull/285)
(merged, shipped in 0.14.0 per the release notes: "Merkle tree proof implementation")
**did** add real, usable **library primitives** for both proof types, confirmed by
fetching `src/rekor/models/inclusion_proof.rs` at the `v0.14.0` tag:

```rust
pub struct InclusionProof {
    log_index: i64,
    root_hash: [u8; 32],
    tree_size: TreeSize,
    hashes: Vec<[u8; 32]>,
    checkpoint: Option<SignedCheckpoint>,
}

/// Verify that the canonically encoded `entry` is included in the log,
/// and the included checkpoint was signed by the log.
pub fn verify(&self, entry: &[u8], rekor_key: &CosignVerificationKey) -> Result<(), SigstoreError>
```

— which internally calls an RFC-6962-conformant `verify_inclusion` (confirmed present:
`Rfc6269Default::hash_leaf`, `Rfc6269Default::verify_inclusion`, using the `0x00`/`0x01`
domain-separation prefixes from §3c). Consistency-proof and `SignedCheckpoint`/`Checkpoint`
types (matching Go's `sumdb/note` naming) were added in the same PR. Per the PR author's
own scope note: **"I have not implemented the logic to verify that Checkpoints and the
corresponding consistency/inclusion proof are sound together"** — i.e. the primitive
verifies one proof against one supplied root hash correctly, but the caller
(ocx) is responsible for the orchestration: fetching/trusting the checkpoint,
picking which root hash to verify against, and (per PR scope) any consistency-proof
chaining across log states.

**What sigstore-rs still has *no* code for at all, per the PR author and the release
notes**: **SET verification remains "separate future work"** — no canonicalize-then-verify
primitive exists anywhere in the crate for the SET specifically (distinct from the
generic Merkle-proof primitive above). This is where §3a's `serde_json_canonicalizer`
composition is the concrete answer: ocx implements the *four-field JCS canonicalize, then
verify with sigstore-rs's own `CosignVerificationKey`* sequence itself, using existing
crates for both halves, rather than waiting on upstream or hand-rolling a bespoke
canonicalizer.

**ADR-relevant conclusion for #209**: the correct shape is (a) call
`InclusionProof::verify()` directly (sigstore-rs primitive, not sigstore-rs's top-level
`Verifier`) for the Merkle/checkpoint half, and (b) compose `serde_json_canonicalizer` +
sigstore-rs's crypto verify primitive for the SET half — both as an *orchestration layer
ocx owns*, not a hand-rolled parser/canonicalizer/Merkle-math implementation. Consider
filing the "wire this into `Verifier::verify()`" gap upstream as a sigstore-rs issue
(non-blocking — ocx cannot wait on it) since #209's acceptance criteria ("emitting
real-format bundles unblocks #197") only require ocx's *own* pipeline to do this
correctly, not upstream's.

---

## 4. SCT / CT-log verification

Source: RFC 6962 §3.2/§3.3 (`rfc-editor.org/rfc/rfc6962.html`), Fulcio's
certificate-specification.md (§5 above), and sigstore-rs's confirmed-implemented SCT
check (v0.10.0 changelog: "Signed Certificate Timestamp verification", PR #326 —
already present at the pinned 0.14.0, per §3d's trace through `verifier.rs`'s ordered
check list).

### The SCT structure (RFC 6962 §3.2)

```text
struct {
    Version sct_version;
    LogID id;
    uint64 timestamp;
    CtExtensions extensions;
    digitally-signed struct {
        Version sct_version;
        SignatureType signature_type = certificate_timestamp;
        uint64 timestamp;
        LogEntryType entry_type;
        select(entry_type) {
            case x509_entry:   ASN.1Cert;
            case precert_entry: PreCert;
        } signed_entry;
        CtExtensions extensions;
    };
} SignedCertificateTimestamp;
```

`PreCert` (the case Fulcio always uses, since Fulcio submits a *precertificate* to get
an SCT to embed before issuing the final cert) is:

```text
struct {
    opaque issuer_key_hash[32];   // SHA-256 of the ISSUING CA's SubjectPublicKeyInfo (DER)
    TBSCertificate tbs_certificate; // DER TBSCertificate, poison ext + signature stripped
} PreCert;
```

### Embedded vs detached SCT — Fulcio uses embedded only

Fulcio's certificate-specification.md confirms the **SCT extension OID is
`1.3.6.1.4.1.11129.2.4.2`** (the well-known CT "embedded SCT list" OID from RFC 6962),
embedded directly in the issued (final, non-precert) certificate's extensions. The
**poison extension OID is `1.3.6.1.4.1.11129.2.4.3`**, present only in the
*precertificate* sent to the CT log to request the SCT, never in the final cert. Fulcio
does not use the detached/`SCTFE`-response-header route the research question flagged as
an alternative — that route exists in the CT ecosystem generally (some CAs return the
SCT out-of-band at issuance time rather than embedding it), but Fulcio's own spec is
embedded-only.

### The precertificate TBS-reconstruction difficulty, precisely

This is the genuinely hard part of #208, confirmed from RFC 6962 text directly: to
verify an *embedded* SCT, a verifier must reconstruct the exact bytes that were
originally signed — which is the **precertificate's** `TBSCertificate`, not the final
certificate's. The final certificate differs from the precertificate it was derived from
in exactly two ways: (1) it carries the embedded-SCT-list extension the precert didn't
have, and (2) the precert may have been signed by a distinct "precertificate signing
certificate" whose issuer differs from the final CA (not Fulcio's case, since Fulcio
signs precerts with the same intermediate — but a general-purpose verifier must handle
both). Per RFC 6962 §3.2: "It is also possible to reconstruct this TBSCertificate from
the final certificate by extracting the TBSCertificate from it and deleting the SCT
extension" — i.e. **the reconstruction is: take the final cert's TBSCertificate DER,
remove the embedded-SCT-list extension (OID `.2.4.2`), and (if the precert issuer
differed) rewrite the issuer field to the precert-signing CA's identity.** For Fulcio
specifically, step 2 is a no-op (same issuer both times), which simplifies ocx's
implementation versus a fully general CT verifier — but the code should not silently
assume this holds for every future trust root; it is a Fulcio-profile fact, not a CT
protocol fact, and should be checked/asserted rather than hardcoded blindly.

### CT log key source

Per client-spec §2.2 (already quoted in §1): "verify it … using the verification key
from the Certificate Transparency Log." That key is **NOT** the Fulcio root/intermediate
— it is a **separate CT log operator key**, sourced from the TUF-distributed trust
root's CT-log key material (issue #210's `trusted_root.json` — Sigstore's trust root
format carries a distinct `ctlogs` section alongside `certificate_authorities` and
`tlogs`/Rekor keys). Acceptance criterion "CT log key comes from the trust root (no
hardcoded key)" (issue #208) maps directly onto this — and structurally *depends* on
#210's TUF trust-root work landing the `ctlogs` section, not only the Fulcio/Rekor
sections, or the SCT check has no key to verify against for a self-hosted/rotated log.

**Confirmed** (separate WebSearch against `sigstore-trust-root`'s Rust docs and
`sigstore/root-signing`'s checked-in `targets/trusted_root.json`): the trust-root schema
has exactly four top-level sections — `tlogs` (Rekor), `certificate_authorities`
(Fulcio + intermediates), `ctlogs` (CT log keys, explicitly documented as "used to
validate entries received from certificateAuthorities … properly recorded in the
Certificate Transparency Log"), `timestamp_authorities` (RFC 3161) — under
`mediaType: application/vnd.dev.sigstore.trustedroot+json;version=0.1`. `ctlogs` is a
first-class sibling section, not nested under `certificate_authorities`; #210's TUF
client and #208's SCT verifier both read from this same fetched/cached document but from
different top-level keys.

Also relevant to #210 specifically (not one of the nine numbered questions, but
load-bearing for sequencing #206–#210 against each other): the `sigstore-trust-root`
Rust crate already exposes `TrustedRoot::from_tuf()` / `from_tuf_staging()` behind a
`tuf` Cargo feature, doing real TUF metadata fetch + signature verification against an
embedded root of trust, with built-in production/staging/GitHub trust anchors. This is
the same "primitive exists, orchestration/wiring is ocx's job" pattern as §3d — #210 is
plausibly a wiring task (call `from_tuf()`, cache/refresh per the client-spec's
`load_embedded` contract) rather than a from-scratch TUF client, pending direct
confirmation against the pinned crate version (**UNCONFIRMED — not verified against
sigstore-rs 0.14 specifically, only the `sigstore-trust-root` sub-crate's latest docs**).

---

## 6/7. cosign v3 interop — what a producer must emit, what a verifier must tolerate

Sources: [`sigstore/cosign` `specs/SIGNATURE_SPEC.md`](https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md)
(tag-based spec only — **does not cover OCI 1.1 referrers**, confirmed by direct fetch);
cosign issue [#3577](https://github.com/sigstore/cosign/issues/3577) (the original
referrers-as-OCI-artifact design); cosign source `pkg/oci/remote/write.go` (`main`,
`WriteReferrer`); [cosign releases](https://github.com/sigstore/cosign/releases) (v3
behavior summary via WebSearch).

**Caveat on this section**: `SIGNATURE_SPEC.md` is stale/incomplete for v3 — it documents
only the pre-1.1 tag-based (`sha256-<digest>.sig`) scheme. The actual v3 referrers
behavior had to be reconstructed from source + issue history, not a single normative
spec doc; treat the field/annotation names below as **UNCONFIRMED against the exact
pinned cosign version** until validated against `cosign version` output in ocx's own CI
harness (per issue #197 step 2's "dogfood `ocx install cosign` pin >= 3.0").

### What a v3 producer emits (from `pkg/oci/remote/write.go`'s `WriteReferrer`)

- **Config descriptor**: empty config, `mediaType: application/vnd.oci.empty.v1+json`
  (the OCI 1.1 "artifact manifest with no meaningful config" convention).
- **`artifactType`** on the manifest: the bundle's own media type string, generated via
  `sgbundle.MediaTypeString("0.3")` — i.e. **`application/vnd.dev.sigstore.bundle.v0.3+json`**,
  matching exactly the `Bundle.media_type` field's own required value from §2. The
  artifact-type and the bundle's self-declared media type are the same string by
  construction — a verifier can trust one implies the other.
- **Layer**: the serialized bundle bytes, same media type.
- **`subject`**: an OCI `Descriptor{MediaType, Digest, Size}` pointing at the **signed
  artifact's own manifest** — this is the binding that makes the referrer discoverable
  via `GET /v2/<repo>/referrers/<digest>` and is what ties the signature to *this specific*
  artifact digest (not a tag, which can move).
- **Annotations** (confirmed field names from source, semantics from the 2023 proposal
  issue for the *predicate-type* annotation's purpose): `org.opencontainers.image.created`
  (RFC 3339 timestamp), `dev.sigstore.bundle.content` (e.g. `"dsse-envelope"` — signals
  what's inside the bundle's `content` oneof from §2 without parsing it), and
  `dev.sigstore.bundle.predicateType` (for DSSE/in-toto attestations, the predicate URI —
  e.g. `https://slsa.dev/provenance/v1` — letting a scanner filter referrers by
  attestation type without fetching every blob).

### What `ocx package sign` must therefore produce for cosign 3.x to accept it

A referrer manifest with `artifactType` = the bundle's own v0.3 media type string,
`subject` bound to the target artifact's descriptor, one layer carrying the serialized
protobuf bundle (§2's `Bundle` message, form-3 `certificate` — single leaf, not chain),
and the `dev.sigstore.bundle.content` annotation set correctly for message-signature vs
DSSE-envelope content. Referrers-only: per issue #197's "Decision (final)", **no `.sig`
tag fallback is to be emitted** — cosign ≥3.0 defaults to referrers-only discovery and a
tag-based signature is invisible to it by design, not a bug to route around.

### What `ocx package verify` must tolerate from a cosign-produced bundle (bidirectional, #197 item 3)

- A v0.3-form-(3) bundle carrying a **single leaf certificate**, not a chain (§2) — the
  verifier must source intermediates/roots from its own trust root, not assume the
  bundle is self-contained.
- `tlog_entries` populated with a **real, canonical-format Rekor v1 SET** (§3a) — not
  ocx's own `ocx-rekor-set-v1` shape; this is precisely why #209 gates #197.
- A **real embedded SCT** in the leaf cert (§4), signed by a CT log ocx's trust root
  must actually enumerate — a fake-stack-only CT key will not validate a real Fulcio
  cert's SCT.
- Possibly `timestamp_verification_data` (RFC 3161) instead of, or alongside,
  `tlog_entries` (§2's "at least one of the two, never neither").
- DSSE-envelope content for in-toto attestations (cosign attaches SBOMs/provenance this
  way) vs plain `message_signature` for a bare artifact signature — the `content` oneof
  from §2, both arms must be handled.

---

## 8. Temporal validity semantics — verifying an expired-by-design cert

This is directly answered by client-spec §1 (already quoted in full in §1 above) and is
worth restating as its own conclusion since it is easy to get backwards:

**The verifier never checks the leaf certificate's validity window against real
wall-clock `now()`.** By design, a Fulcio leaf's short lifetime (operationally ~10
minutes on the public-good instance) means it is *expected* to be long expired by the
time anyone verifies it later. The check is: **cert validity window MUST contain the
timestamp established in step 1** (either the Rekor `integratedTime` from the SET/log
entry, or an RFC 3161 TSA timestamp) — never the verifier's clock. This is the "hybrid
trust model" the spec names explicitly: short-lived key material + a trusted third-party
timestamp standing in for "was this key valid at signing time," instead of a long-lived
key with its own long-lived, revocation-checkable validity window.

**Correct behavior when a bundle carries only an inclusion promise (SET, no separate
inclusion proof yet fetched)**: per §3b, the SET *alone* is sufficient for the temporal
check — the promise's own `integratedTime` field is trustworthy for this purpose the
moment the SET signature verifies, independent of whether the Merkle inclusion proof has
also been checked. The inclusion proof answers a *different* question ("is this entry
really in the tree, tamper-evident, auditable") from the temporal question ("was the
signing timestamp legitimate"). **Both are still separately mandatory per §1's abort-on-
failure framing and #209's acceptance criteria** — a verifier is not exempt from checking
inclusion just because the SET alone was enough for temporal validity. sigstore-rs 0.14
already implements the temporal-validity-against-integrated-time check (§3d's trace
confirms step "7) Temporal validity" is implemented, not a TODO) — only the inclusion
proof and the SET signature itself remain gaps ocx must close per §3d.

---

## 9. Known pitfalls — verifiers that skipped X, exploitable by Y

Every entry below is a real, named CVE/advisory (not a hypothetical), each demonstrating
a concrete skip-this-check → exploitable-that-way pattern directly relevant to ocx's
own verify pipeline design. Sourced via WebSearch + WebFetch against GitHub Security
Advisories / CVE trackers; **treat exact CVSS/version numbers as UNCONFIRMED pending a
direct read of the GHSA page for each** — the fetch tool's summaries are consistent
across independent queries but were not cross-verified against the raw advisory JSON.

| CVE / Advisory | Project | Skipped check | Exploitable by |
|---|---|---|---|
| **CVE-2022-36056** ([GHSA-8gw7-4j42-w388](https://github.com/advisories/GHSA-8gw7-4j42-w388)) | cosign (pre-1.12.0) | Four related bypasses in `verify-blob`: (1) a bundle's embedded `rekorBundle` was never checked to actually *reference* the given signature; (2) email/issuer identity flags were checked against nothing when a Rekor bundle was present — **GitHub Actions identity was never checked at all**; (3) an invalid Rekor bundle without an experimental flag still verified; (4) an invalid transparency-log entry produced immediate success. | Forging or reusing an unrelated valid signature + an unrelated valid log entry together, or supplying a garbage log entry, all pass. This is the exact "Rekor consistency" check sigstore-rs's `verify()` already implements per §3d's trace — **ocx must confirm this check survives the migration, not just that a Rekor entry exists**. |
| **CVE-2024-53267** ([sigstore-java](https://www.miggo.io/vulnerability-database/cve/CVE-2024-53267)) | sigstore-java (`KeylessVerifier`, pre-1.1.0) | The transparency log entry was cryptographically valid and time-correct but **never checked to correspond to the artifact being verified** — a "mismatched bundle" (valid sig + valid-but-unrelated log entry) passed. | Presenting any validly-signed artifact alongside any other valid, unrelated log entry. This is client-spec §3.2's "three invariants" (§1 above) — **invariant 3, "the subject of the parsed body matches the artifact," is the one this CVE is precisely about; a verifier implementing invariants 1–2 but skipping 3 looks complete and is not.** |
| **CVE-2026-48815** ([GHSA-52v5-jr5w-gjxr](https://github.com/advisories/GHSA-52v5-jr5w-gjxr), CVSS 7.5) | `sigstore` (npm, ≤4.1.0) | Caller-supplied `certificateOIDs` policy constraints (e.g. "this cert MUST carry OID X with value Y") were accepted by the public API but **silently discarded** before verification — the policy-construction path copied only SAN/issuer settings. | Any caller relying on `certificateOIDs` to scope trust (e.g. "only accept certs with a specific build-config OID") gets **no enforcement at all**, with no error — this is the exact shape of failure ocx's `[[trust.policy]]` (#98) must not have: a policy field that parses and is silently unused downstream. Direct code-review implication: after wiring policy checks, write a test asserting a policy violation is actually *rejected*, not just that the policy struct round-trips. |
| **CVE-2026-54787** ([sigstore-go](https://cvereports.com/reports/CVE-2026-54787), CVSS low, CWE-324) | sigstore-go | Bundle verification for **self-managed long-lived public keys** (non-Fulcio, `ExpiringKey`) never cross-referenced the signing timestamp against the key's own validity window. | An attacker holding an expired or rotated long-lived key can produce bundles that still verify. Relevant to ocx if/when non-keyless (BYO public key) signing is supported alongside the Fulcio/keyless path — the temporal check in §8 is not automatically inherited by a different key-material shape; each verification *mode* needs its own temporal check, not one shared assumption. |
| **CVE-2026-39984** ([timestamp-authority](https://advisories.gitlab.com/golang/github.com/sigstore/timestamp-authority/v2/CVE-2026-39984/)) | sigstore/timestamp-authority | `VerifyTimestampResponse` correctly verified the certificate **chain**, but then read authorization-relevant fields from the **first non-CA cert in the PKCS#7 bag** instead of the actual leaf certificate the chain validation had just confirmed. | Prepending a forged certificate to the PKCS#7 bag while signing with a legitimately-authorized key: signature validates against one cert, authorization checks run against a *different* one silently substituted in. General lesson for #206/#207: **"which certificate did the chain-walk actually validate" and "which certificate do policy checks read fields from" must be structurally the same value**, not two separately-indexed lookups into the same cert list that can drift apart. |
| **CVE-2024-55655** (sigstore-java) | sigstore-java | Missing verification of `integratedTime` against the Fulcio certificate's validity window — i.e. §8's core check, omitted. | A signature made with a cert-Rekor-timestamp pair where the timestamp falls *outside* the cert's validity window still verified. This is the single most direct precedent for #207's stated acceptance criterion ("a leaf whose validity window excludes the Rekor integrated time is rejected") — it is not a hypothetical edge case, a major sigstore client actually shipped without it. |

### Cross-cutting lesson for the ADR

Five of six entries above are **omission** bugs (a check that should run, does not),
not cryptographic breaks — none involves broken crypto; every one is "the verifier
computed something true about the wrong object, or didn't compute it at all." This maps
directly onto why the mission's non-negotiable is *delegation*, not merely *correctness
review*: sigstore-rs's own crypto primitives (ECDSA verify, Merkle hash, SCT signature
check) are not where the historical bugs lived — the **orchestration** (which fields get
compared against which, whether a check silently no-ops, whether a policy constraint is
actually threaded through) is where they lived, in libraries with far more scrutiny and
users than ocx's own pipeline. §3d's finding (sigstore-rs's own `verify()` skips Merkle
inclusion + SET) is exactly this same class of bug, currently live in the very library
ocx is migrating to — which is the strongest argument for treating "we call
`sigstore-rs`" as necessary, not sufficient, and building #209's explicit test matrix
(a tampered entry MUST be rejected; a SET from real Rekor MUST verify) as the acceptance
gate rather than trusting the delegation alone.

---
