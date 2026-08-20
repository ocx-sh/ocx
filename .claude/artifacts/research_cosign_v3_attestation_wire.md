# Research: cosign v3 attestation wire format (OCI referrers, bundle mode)

> hex-architect research axis (2026-08-20), worker: sonnet researcher. Persisted
> by the orchestrator (worker toolset had no Write). Feeds
> `adr_sbom_attestations.md`. Cosign tag inspected: v3.1.3 (2026-08-06).

**Verdict:** the wire format is fully reproducible — OCI 1.1 referrer manifest
with artifactType/layer mediaType `application/vnd.dev.sigstore.bundle.v0.3+json`,
DSSE envelope inside a protobuf-JSON Sigstore bundle, three fixed annotations —
but the tlog layer is mid-migration (Rekor v1 `dsse`/`intoto` kinds → Rekor v2
`hashedrekord:0.0.2` over the DSSE PAE hash), so the verifier must handle both
regimes.

## 1. Referrer manifest cosign v3 pushes (`WriteAttestationNewBundleFormat`)

Source: `pkg/oci/remote/write.go` + `specs/BUNDLE_SPEC.md`.

```go
bundleMediaType, _ := sgbundle.MediaTypeString("0.3") // application/vnd.dev.sigstore.bundle.v0.3+json
layer := static.NewLayer(bundleBytes, types.MediaType(bundleMediaType))
annotations := map[string]string{
    "org.opencontainers.image.created": time.Now().UTC().Format(time.RFC3339),
    "dev.sigstore.bundle.content":      "dsse-envelope",
    BundlePredicateType:                predicateType, // "dev.sigstore.bundle.predicateType"
}
```

- Subject descriptor = whatever the target is (per-platform manifest OR index;
  BUNDLE_SPEC's example uses an index).
- **Empty config blob is actively pushed on every referrer write**
  (`writeEmptyConfigLayer`, digest sha256:44136fa3…, size 2,
  `application/vnd.oci.empty.v1+json`) — never assumed registry-resident.
  (OCX already holds these constants in `oci/referrer/media_types.rs`.)
- Resulting manifest: schemaVersion 2, mediaType oci.image.manifest.v1+json,
  **top-level** artifactType = bundle media type, config = empty descriptor,
  layers = [one bundle blob, mediaType = bundle media type], subject, the three
  annotations above.
- Medium-confidence caveat: one summarizer pass placed artifactType on the
  config descriptor; go-containerregistry `v1.Manifest` has a real top-level
  `ArtifactType` field and BUNDLE_SPEC's JSON shows top-level — treat top-level
  as correct, but **verify against a live cosign attach in the interop test**.
- Signature vs attestation referrers share the SAME artifactType; discriminated
  only by `dev.sigstore.bundle.content` (`message-signature` vs
  `dsse-envelope`).

**Cosign's read path discovers referrers with an EMPTY artifactType filter**
(`ociremote.Referrers(digest, "")`) and discriminates client-side by parsing
each bundle and type-switching on the content oneof. The annotations are for
other tooling's filtering, not cosign's own.

## 2. Sigstore bundle JSON shape (attestation)

Sources: `sigstore_bundle.proto`, `sigstore_rekor.proto` (proto3 JSON →
camelCase).

```jsonc
{
  "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
  "verificationMaterial": {
    "certificate": { "rawBytes": "<b64 DER leaf>" },   // PGI keyless: form (3) single cert MUST be used, not x509CertificateChain
    "tlogEntries": [{
      "logIndex", "logId", "kindVersion": {"kind","version"}, "integratedTime",
      "inclusionPromise": {"signedEntryTimestamp"},     // Rekor v1 SET — optional
      "inclusionProof": {"logIndex","rootHash","treeSize","hashes","checkpoint"}, // required
      "canonicalizedBody"
    }],
    "timestampVerificationData": { "rfc3161Timestamps": [] }
  },
  "dsseEnvelope": {
    "payload": "<b64 in-toto Statement JSON>",
    "payloadType": "application/vnd.in-toto+json",
    "signatures": [{ "sig": "<b64>", "keyid": "" }]
  }
}
```

`content` oneof = `messageSignature` | `dsseEnvelope`, exactly one. Spec:
"DSSE envelopes in a bundle MUST have exactly one signature."

## 3. What `cosign verify-attestation` requires

- Leaf cert against trusted material + SCT (unless IgnoreSCT).
- **Tlog verification ON by default**; disabling needs
  `--insecure-ignore-tlog`. `--offline` + missing material fails closed.
- v3 default branch: `verifyImageAttestationsSigstoreBundle` → sigstore-go
  `VerifyNewBundle` (generic verifier, same for OCI/file inputs).
- **predicateType matching is the CLI layer's job** via `co.ClaimVerifier`,
  reading the SIGNED payload's predicateType — the manifest annotation is
  discovery-only. (Matches pitfalls research row 7.)
- Advisory context: GHSA-whqx-f9j3-ch6m — bundles verified although the Rekor
  entry did not reference the artifact's digest/signature; a verifier must
  assert the tlog entry's canonicalizedBody commits to THIS
  signature/digest/key. (Matches pitfalls checklist row 12/20.)

## 4. In-toto Statement cosign builds + predicate URIs

Source: `pkg/cosign/attestation/attestation.go`, `cmd/cosign/cli/attest/attest.go`,
cross-checked against in-toto-golang.

```go
Type: in_toto.StatementInTotoV01,  // "https://in-toto.io/Statement/v0.1"  ← cosign still writes v0.1!
Subject: [{ Name: digest.Repository.String(),           // bare repo path, no tag/digest
            Digest: {"sha256": h.Hex} }]                // hex, no "sha256:" prefix
```

**⚠ Interop tension with the pitfalls research:** cosign v3 still WRITES
Statement v0.1; the security checklist recommends rejecting non-v1 on verify.
The ADR must decide: what OCX writes (v1 vs cosign-matching v0.1) and what OCX
verify accepts (v1-only vs {v0.1, v1}) — settle empirically in the cosign
interop test whether cosign verify-attestation accepts a v1 Statement.

| `--type` | predicateType URI |
|---|---|
| `slsaprovenance`, `slsaprovenance02` | `https://slsa.dev/provenance/v0.2` |
| `slsaprovenance1` | `https://slsa.dev/provenance/v1` |
| `spdx`, `spdxjson` | `https://spdx.dev/Document` |
| `cyclonedx` | `https://cyclonedx.org/bom` |
| `link` | `https://in-toto.io/Link/v1` |
| `vuln` | `https://cosign.sigstore.dev/attestation/vuln/v1` |
| `openvex` | `https://openvex.dev/ns` |
| `custom` / default | `https://cosign.sigstore.dev/attestation/v1` |
| full URI passed to `--type` | used verbatim, raw predicate, no wrapper |

**⚠ note:** cosign's bare `slsaprovenance` = v0.2, not v1 — issue #102 wants
>= v1.0 validation. ADR must reconcile the vocabulary (e.g. OCX `slsaprovenance`
→ v1 with `slsaprovenance02` rejected, or cosign-identical mapping + policy
validation elsewhere).

## 5. CosignPredicate wrapper — correction

The `{Data, Timestamp}` wrapper removal for spdx/spdxjson/cyclonedx happened in
**PR #2718, v1.14.x (2023-02)** — not a v3 change. `custom`/default STILL wraps
in CosignPredicate. SBOM predicates are the raw BOM document. No v3-specific
change to design around.

## 6. Rekor entry kind — the sharpest interop edge

Which regime governs depends on the signing config's Rekor target, not a cosign
constant:

- **Rekor v1** (OCX compose stack, self-hosted default): `pkg/cosign/tlog.go`
  builds `kind: "dsse", version: "0.0.1"` (or intoto) proposed entries.
- **Rekor v2** (public-good default as of cosign v3.0.5+): `dsse`/`intoto`
  kinds REMOVED; only `hashedrekord:0.0.2`, where the hashed value =
  `Hash(PAE(payloadType, payload))` — the DSSE PAE hash. `kindVersion` says
  hashedrekord even for attestations; attestation-ness is inferred from the
  bundle content oneof. Rekor v2 drops online SET/proof APIs; C2SP checkpoint
  embedded at signing time.
- v3.0.5 changelog: "Automatically require signed timestamp with Rekor v2
  entries."
- **Trust-root consequence:** a shipped trusted root should be able to carry
  both v1 and v2 log keys; a verifier learns the regime only from
  `tlogEntries[].kindVersion`. For OCX v1 scope: write dsse:0.0.1 (Rekor v1);
  accepted-kind set on verify is an ADR decision (#107 tracks the v2 delta).

## 7. Registry mechanics (OCI 1.1)

- Push with `subject`: referrers-capable registry MUST answer with
  `OCI-Subject: <digest>` header (push-side capability signal).
- Referrers 404 → cosign falls back to the `sha256-<hex>` tag schema (OCI-spec
  fallback). **OCX hard-fails exit 84 instead (ADR S1-F) — unchanged.**
- Empty config blob pushed every time; dedupe is the registry's business.

## Sources

cosign v3.1.3: pkg/oci/remote/write.go, specs/BUNDLE_SPEC.md,
pkg/cosign/verify.go, pkg/cosign/tlog.go, pkg/cosign/attestation/attestation.go,
cmd/cosign/cli/attest/attest.go, PR #2718, CHANGELOG · sigstore/protobuf-specs
sigstore_bundle.proto + sigstore_rekor.proto · Rekor v2 GA blog + rekor-tiles
CLIENTS.md · in-toto-golang attestations.go + slsa_provenance/{v0.2,v1} · OCI
distribution-spec + image-spec manifest.md · GHSA-whqx-f9j3-ch6m

**Watch item:** v3.1.2 notes call it "potentially the last v3.1 release ahead
of v4" — keep the interop surface pinned to documented BUNDLE_SPEC behavior,
not incidental v3 internals.

## Spike results (2026-08-20)

WP-S (plan `plan_sbom_attestations.md`, wave 0). Empirical run against the
already-running compose `sigstore` profile: cosign v3.1.1 container
(`ghcr.io/sigstore/cosign/cosign:v3.1.1`, pinned per `test/tests/fixtures/cosign.py`)
against the local zot registry on `localhost:5000` (the `registry` /
`test-registry-1` fixture). `--key`-based signing throughout, per the task's
"avoid Fulcio" instruction — a freshly generated cosign key pair
(`cosign generate-key-pair`, `COSIGN_PASSWORD=` empty), never committed. A
minimal hand-pushed OCI manifest (`spike/subject`, empty config + one 22-byte
layer) served as the subject; a second subject (`spike/subject-v1type`) kept
the `_type:v1` acceptance test unambiguous from cosign's own v0.1-typed output.

**Verdict on all three assigned items: the ADR's existing design is confirmed
byte-for-byte. No divergence found; no ADR amendment needed.**

### Item 1 — `artifactType` position

**Confirmed: top-level**, exactly as the ADR's "treat top-level as correct"
call. The pushed attestation referrer manifest (golden fixture below) carries
`artifactType: "application/vnd.dev.sigstore.bundle.v0.3+json"` as a sibling of
`schemaVersion`/`config`/`layers`/`subject` — the field the OCI 1.1 Referrers
API filters on, which is what D-e's filter decision assumed. The referrers-index
entry returned by `GET /v2/spike/subject/referrers/<digest>` echoes the same
value in its own descriptor's `artifactType`, confirming server-side indexing
picked it up correctly.

**New, non-blocking finding not previously called out:** cosign *also* stamps
`config.artifactType` with the identical value, redundantly:

```json
"config": {
  "mediaType": "application/vnd.oci.empty.v1+json",
  "size": 2,
  "digest": "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
  "artifactType": "application/vnd.dev.sigstore.bundle.v0.3+json"
}
```

The plan's C-002/WP2 scope adds only `annotations` to `ReferrerManifest` — no
`config.artifactType` field. This is **not a spike-mandated change**: cosign's
own read path discriminates by parsing bundle content (§1 above), never by
`config.artifactType`, so an OCX-written referrer omitting it does not break
`cosign verify-attestation` reading OCX's output — the interop criterion D1
cares about. Flagged for WP2's Implement phase to decide with full context,
not something this spike can or should settle unilaterally.

### Item 2 — `_type` acceptance

**cosign v3.1.1 writes `_type: "https://in-toto.io/Statement/v0.1"`** — confirms
the research's original finding and the ADR's D-b premise exactly. Decoded the
real DSSE envelope's `payload` (base64) from the pushed bundle; the full
Statement is in the golden fixture.

**`cosign verify-attestation`/`verify-blob-attestation` ACCEPT a hand-crafted
`_type: "https://in-toto.io/Statement/v1"` Statement.** Since `cosign attest`
gives no CLI way to override `_type` (confirmed by reading `attest --help` in
full — no such flag, and `--statement` turned out **not** to be a full-statement
override; see "Tooling notes" below), the v1-typed Statement was hand-built and
signed by reusing cosign's own ECDSA-P256 key through `cosign sign-blob` over
the DSSE PAE bytes computed in Python (`DSSEv1 SP LEN(type) SP type SP LEN(body)
SP body`) — the "pragmatism over purity" path the task pre-approved. The
resulting signature was assembled into a bundle v0.3 JSON with a `dsseEnvelope`
content oneof and checked with:

```
cosign verify-blob-attestation --bundle crafted_v1_bundle.json --key cosign.pub \
  --type cyclonedx --insecure-ignore-tlog=true \
  --digest <subject-digest> --digestAlg sha256
```

Result: **`Verified OK`**, exit 0. Two negative controls prove the green
result was not vacuous (Unchecked Green discipline, `quality-core.md`):

- Wrong subject digest → `Error: failed to verify signature: provided artifact
  digest does not match any digest in statement`, exit 1.
- Wrong `--type` (`spdxjson` against a `cyclonedx`-typed statement) →
  `Error: invalid predicate type, expected spdxjson got https://cyclonedx.org/bom`,
  exit 1.

This empirically validates D-b's chosen design (`STATEMENT_TYPE_WRITTEN = v1`,
`ACCEPTED_STATEMENT_TYPES = {v1, v0.1}`) end to end: OCX can write v1 and
interop with cosign's verifier without incident, exactly as the deviation
paragraph in `signing.md` will claim. **No amendment to D-b or the constants
table.**

### Item 5 — Annotation literal values

**Confirmed exact match** to the Constants block already in the ADR:

| Key | Observed value |
|---|---|
| `org.opencontainers.image.created` | `2026-08-20T01:33:28Z` — RFC 3339, explicit `Z` |
| `dev.sigstore.bundle.content` | `dsse-envelope` |
| `dev.sigstore.bundle.predicateType` | `https://cyclonedx.org/bom` |

All three present on every attestation referrer cosign wrote in this run; no
fourth key, no missing key. `BUNDLE_CONTENT_DSSE`/`ANNOTATION_*` constants in
the ADR's Constants block need no change.

### Bonus finding (not a spike-assigned item, but load-bearing for D-g)

The task's item 5 ("Rekor payloadHash sanity") asked only for a hash
computation, but the local run actually reached a **real, live Rekor entry** —
against `rekor.sigstore.dev` (the public production log), not the local
compose Rekor — because `--key`-based `cosign attest` with no `--signing-config`
still defaults `--use-signing-config=true` and logs by default; there is no
`--tlog-upload=false` flag on `attest` in this cosign version. That gave a real
`dsse:0.0.1` `canonicalizedBody` to check D-g's binding claim against, not just
the researched schema:

```
canonicalizedBody.spec.payloadHash.value == sha256(base64-decode(dsseEnvelope.payload))
```

**Confirmed true, byte-for-byte**: `684f6eace089c8d6f8102ba5fbb9f6645c9550a45c0f930c6f3995d725efba76`
on both sides. This is the exact claim `verify_tlog_binding` (D-g, Part III row
12) is built on — cosign's `dsse:0.0.1` entries in Rekor v1 hash the **decoded**
payload bytes, never the PAE bytes and never a re-serialization. Confirms row
12's design with a real log entry rather than the researched source alone.

### Tooling notes for WP10a/WP7 (process corrections, not ADR-relevant)

- **`--new-bundle-format` is not a real flag on cosign v3.1.1.** Absent from
  `attest --help`, `sign-blob --help`, and `verify-attestation --help` in full.
  cosign v3 always writes bundle v0.3 for OCI referrer attestations — there is
  nothing to opt into. The string surfaces only inside two unrelated error
  messages ("must specify --bundle with --new-bundle-format") triggered when
  `--no-upload`/local blob-signing is requested without a `--bundle <path>` —
  supplying `--bundle` resolves it; passing a literal `--new-bundle-format=true`
  flag to any subcommand is a parse error. Any planned `test_cosign_interop.py`
  invocation using this flag name should drop it.
- **`--statement` on `cosign attest` is not a full-statement override.** Passing
  it alone fails with `predicate cannot be empty`; passing it alongside
  `--predicate`/`--type` silently loses to `--predicate` ("Using payload from:
  predicate.json" is printed regardless). cosign's CLI has no supported path to
  override `_type`/`subject` construction — the hand-signed PAE route above is
  the only way to test a non-standard Statement shape against cosign's own
  verifier.
- **Key-based `attest`/`sign-blob` still reach the network by default.** Worth
  knowing before wiring a fully offline interop test: `--allow-http-registry=true`
  is required for the plain-HTTP zot registry, and absent an explicit
  `--signing-config` disabling defaults (the `signing_config()` helper already
  in `cosign.py`), the tool will try live TUF + live Rekor even for `--key`
  signing.

### Golden fixtures added

- `test/tests/fixtures/spike_cosign_attestation_referrer.json` — the real
  attestation referrer manifest cosign pushed (OCI image manifest, three
  annotations, `subject`, empty-config + bundle-layer descriptors).
- `test/tests/fixtures/spike_cosign_bundle.json` — the real Sigstore bundle
  v0.3 blob referenced by that manifest's one layer (verificationMaterial with
  a live `rekor.sigstore.dev` `dsse:0.0.1` tlog entry, plus the `dsseEnvelope`
  whose decoded payload is the `_type:v0.1` Statement).

Neither fixture required hand-editing after capture — both are the registry's
own bytes, fetched by digest and pretty-printed for diff-friendliness.
