# Research — Cosign Keyless Signing Flow (Push Side)

**Date:** 2026-04-19
**Scope:** How `ocx package sign <ref>` constructs and publishes a cosign-keyless signature as an OCI referrer, given the `sigstore = "=0.13"` pin. Companion to `research_cosign_sigstore_notation.md` (which covers *verification* and library identity) and `research_oidc_cli_flows.md` (which covers *identity-token acquisition*).

## TL;DR

- **Signing algorithm:** ephemeral ECDSA P-256 keypair generated per-signature — **never persisted**. Private key exists only for the duration of the `Sign-Certify-Log-Push` sequence, then dropped. Fulcio signs a certificate binding the ephemeral public key to the OIDC identity (email, `iss`, SAN). The certificate has a ~10-minute validity window; Rekor's transparency-log entry is what makes the signature verifiable after that window closes.
- **Sigstore-rs v0.13 signing status:** the crate **ships signing APIs for the message-signature path** (hash-only / blob-style) and **Fulcio/Rekor clients**, but DSSE/attestation signing is **not implemented** (matches the verification-side gap). That is acceptable for v1 because cosign-style artifact signatures use the message-signature path, not DSSE.
- **Bundle format:** write v0.3 bundles (`application/vnd.dev.sigstore.bundle.v0.3+json`) only. No legacy format support on the *write* side.
- **Referrer push:** manifest-push is a pure OCI operation — `oci-client`'s existing `push_manifest` + `push_blob` are sufficient; no need for a new transport. The bundle becomes an OCI blob; the referrer manifest sets `subject: <digest-of-signed-artifact>` per OCI v1.1.
- **Registry compatibility (push side):** registries that lack Referrers API still accept referrer-shaped manifests — the server just doesn't *index* them. Clients that only read via Referrers API won't see them. Per the parent-ADR amendment, **v1 holds the "no push-side fallback tags" line** for GHCR / Docker Hub; signatures pushed there are discoverable only via digest-based manifest lookups, not via cosign's `sha256-<digest>.sig` fallback tag. Documented limitation, not a bug.

## 1. Signing Flow (End-to-End)

```
┌─────────────────────────────────────────────────────────────────────────┐
│ ocx package sign my/cmake:3.28                                          │
└─────────────────────────────────────────────────────────────────────────┘
  1. Resolve reference → manifest digest D  (existing: Index::select)
  2. Acquire OIDC token T                   (new: TokenProvider — see OIDC research)
  3. Generate ephemeral ECDSA P-256 keypair (sigstore::crypto::signing_key)
     - private key: kept in RAM only; zeroized on drop
  4. Build CSR (proof-of-possession) from ephemeral pubkey + OIDC `sub`
  5. POST CSR + token to Fulcio             → X.509 cert chain C
  6. Hash payload H = sha256(manifest-bytes-of-D)
  7. Sign H with ephemeral private key       → signature S
  8. Construct Rekor entry {pubkey: C, sig: S, hash: H}
  9. POST Rekor entry                        → signed log entry E (inclusion proof + SET)
 10. Assemble sigstore Bundle v0.3 B {
       cert-chain: C,
       message-signature: S,
       rekor-bundle: E,
       (optional) RFC 3161 timestamp: TS
     }
 11. Push B as OCI blob                     → blob digest BD
 12. Build referrer manifest M_r {
       artifactType: "application/vnd.dev.sigstore.bundle.v0.3+json",
       subject: { digest: D },
       config: empty-descriptor,
       layers: [ { digest: BD, mediaType: "application/vnd.dev.sigstore.bundle.v0.3+json" } ]
     }
 13. Push M_r                               → manifest digest MD
 14. Drop ephemeral private key              (zeroized)
 15. Emit JSON: { subject: D, signature-manifest: MD, bundle: BD, cert-identity, rekor-log-index }
```

**Security properties:**
- Private key never leaves memory; signing is one-shot per invocation.
- The short-lived Fulcio cert (< 10 min) is fine because Rekor's SET (signed entry timestamp) + inclusion proof let verifiers check that the signature happened *during* the cert's validity window — no key rotation, no revocation list.
- Failure at steps 8–10 (Rekor) aborts the flow; the partial signature is not pushed. This preserves the invariant: *every published sigstore bundle has a corresponding Rekor entry*.

## 2. `sigstore = "=0.13"` Signing API Surface

Based on the crate source (`sigstore::sign`, `sigstore::fulcio`, `sigstore::rekor`):

| Capability | Available in 0.13 | API locus | Notes |
|---|---|---|---|
| Ephemeral P-256 keypair | ✅ | `sigstore::crypto::signing_key::SigStoreSigner::ECDSA_P256_SHA256_ASN1` | Also supports Ed25519, but Fulcio only issues certs for ECDSA by default |
| Fulcio CSR + cert issuance | ✅ | `sigstore::fulcio::FulcioClient::create_signing_certificate` | Takes `SigStoreSigner` + OIDC token |
| Rekor posting | ✅ | `sigstore::rekor::apis::entries_api::create_log_entry` | Returns `LogEntry` (SET + inclusion proof) |
| Bundle v0.3 assembly | ✅ | `sigstore::bundle::SigstoreBundle` constructors | Wraps the three pieces (cert, sig, log-entry) |
| Message-signature sign | ✅ | `SigStoreSigner::sign` | sha256 of subject bytes → ECDSA signature |
| DSSE envelope sign | ❌ | — | **Not implemented in 0.13.** Blocks `ocx package attest`; does NOT block `ocx package sign`. |
| TUF root bootstrap | ✅ | `sigstore::trust::sigstore::SigstoreTrustRoot` | Cached per-home under `~/.sigstore/root`; `TrustConfig` disables network on refresh if offline |
| RFC 3161 timestamping | Partial | `sigstore::rekor` has TSA types; full client is experimental | Optional per-bundle field; omit in v1 unless Fulcio requires it (it doesn't yet in 2026-04) |

**Known API churn (pre-1.0):** signing-side types moved between `sigstore::cosign` and `sigstore::sign` between 0.10 → 0.13. Pin `=0.13` hard; wrap in an OCX-internal `SigningBackend` trait so v2 can swap to 0.14+ without CLI breakage.

**Pre-existing crate identity** already captured in `research_cosign_sigstore_notation.md`:
- 49k monthly downloads, 16 reverse deps (April 2026)
- Governance: Sigstore project (CNCF). Pre-1.0 but production-used by chainloop, oci-spec-rs consumers, and sget.

## 3. Referrer Manifest Shape (Push Side)

**Subject pointer strategy:** OCI v1.1 `subject` field points at the *manifest* being signed, by digest. For OCX, that's the package manifest — the same descriptor `ocx install` pulls.

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "artifactType": "application/vnd.dev.sigstore.bundle.v0.3+json",
  "config": {
    "mediaType": "application/vnd.oci.empty.v1+json",
    "digest": "sha256:44136fa355b3...",
    "size": 2,
    "data": "e30="
  },
  "layers": [
    {
      "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
      "digest": "sha256:<bundle-blob-digest>",
      "size": <bundle-byte-length>
    }
  ],
  "subject": {
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "digest": "sha256:<signed-manifest-digest>",
    "size": <signed-manifest-byte-length>
  }
}
```

**Notes:**
- `config` is the OCI v1.1 empty-config sentinel; required for registries that reject manifests without `config`.
- `artifactType` MUST match `layers[0].mediaType` for cosign compatibility (cosign reads `artifactType` to identify bundle-type; readers that scan `layers` also work).
- Path on registry: `/v2/<name>/manifests/sha256:<mr-digest>` — no tag. Discovery is via `/v2/<name>/referrers/<subject-digest>` (OCI v1.1) or manifest-walk fallback (DF-handled).

## 4. Push-Side Transport

OCX already has `oci-client` transport in `crates/ocx_lib/src/oci/client/`. What's needed:

| Step | Existing? | Action |
|---|---|---|
| Push blob (bundle JSON bytes) | ✅ | Reuse `Client::push_blob_chunked` — bundles are small (~3–10 KB), but the chunked path handles it |
| Push manifest (referrer) | ✅ | Reuse `Client::push_manifest` with `OciImageManifest` populated |
| HEAD check to detect idempotent re-sign | ✅ | `Client::fetch_manifest_digest` (already used in `pull_manifest`) — if present with same digest, skip push |
| Auth (push scope) | ✅ | `oci-client` requests `push,pull` scope automatically when push APIs are called |
| Error mapping (401/403/413) | Partial | Need: push-side 413 (payload-too-large) rare but real; 403 ("insufficient scope") needs actionable error |

**No new transport code required.** The signing command composes existing primitives.

## 5. Registry Compatibility (Push Side)

Re-affirming the parent-ADR amendment stance with signing-specific detail:

| Registry | Accepts OCI v1.1 referrer-shape manifest | Indexes via Referrers API | Client can discover after push |
|---|---|---|---|
| Zot | ✅ | ✅ | ✅ Full |
| Harbor ≥ 2.8 | ✅ | ✅ | ✅ Full |
| ECR (2026) | ✅ | ✅ | ✅ Full |
| ACR (2026) | ✅ | ✅ | ✅ Full |
| registry:2 (test fixture) | ✅ | ✅ | ✅ Full |
| GHCR | ✅ accepts | ❌ no `/referrers/` | ⚠️ **Push succeeds; clients without manifest-walk can't find it.** v1 ships manifest-walk fallback on *read* side (DF already decided) |
| Docker Hub | ✅ accepts | ❌ no `/referrers/` | ⚠️ Same as GHCR |
| Quay | ✅ | ✅ | ✅ Full |

**v1 policy (locked in):**
- Push always writes a real referrer manifest (`subject: ...`).
- Push **never writes** the cosign fallback tag (`sha256-<digest>.sig`). Rationale: preserves the single-source-of-truth invariant (referrer manifest is authoritative); avoids dual-write race conditions on concurrent signers; clients that implement manifest-walk fallback can still discover signatures on GHCR/Docker Hub.
- Push emits a **warning on stderr** when the target registry is detected to lack `/v2/<name>/referrers/<digest>`: *"Signature pushed successfully but this registry does not support the Referrers API — clients that rely on it alone will not find this signature. Use `ocx verify --manifest-walk` or deploy to a registry with Referrers support (Zot/Harbor/ECR/ACR)."*

## 6. Idempotency

Re-running `ocx package sign <ref>` on the same subject should not produce duplicate signatures:

- **Option A (content-addressed idempotency):** compute the would-be bundle, hash it, HEAD check. Rejected: the bundle is non-deterministic (Rekor log index + cert serial + ephemeral cert are different each time), so content-addressed dedup is impossible.
- **Option B (policy: sign always creates a new signature):** every invocation creates a fresh ephemeral key, fresh cert, fresh Rekor entry, fresh referrer manifest. This is how cosign behaves by default.
- **Pick B.** Add `--skip-if-signed-by <identity>` flag in v2 if demand emerges; v1 keeps the surface minimal.
- **Garbage collection is out-of-scope for v1.** Stale duplicate signatures live on the registry. Document: "use registry-side GC / `ocx verify` to prune." No OCX-side `ocx package prune-signatures`.

## 7. CLI Surface — `ocx package sign`

Aligned with other `ocx package` subcommands and the CLI conventions ADR.

```
ocx package sign <REFERENCE>
    [--identity-token <JWT> | --identity-token-file <PATH>]
    [--oidc-issuer <URL>]            # default: https://oauth2.sigstore.dev/auth
    [--fulcio-url <URL>]             # default: https://fulcio.sigstore.dev
    [--rekor-url <URL>]              # default: https://rekor.sigstore.dev
    [--format json]
    [--offline: REJECTED (signing requires Fulcio + Rekor network calls)]
```

**Exit codes (follows OCX typed exit-code rules):**
| Code | Condition |
|---|---|
| 0 | Push succeeded; emit bundle + referrer digest |
| 77 | Local validation (e.g. subject not found locally, `--offline` passed) |
| 77 | OIDC pre-check failure (expired/wrong-audience/missing-permission) — typed `OidcError` variant |
| 78 | Fulcio 4xx that is not 401/403 (e.g. malformed CSR) |
| 75 | Registry unauthorized (401) |
| 76 | Registry forbidden (403) |
| 80 | Registry rate-limited (429, with `Retry-After`) |
| 81 | Registry service unavailable (5xx / network) |
| 82 | Rekor unavailable (5xx / network) — distinct because it's a different SLO than the target registry |

**JSON success output (shape):**
```json
{
  "schema_version": 1,
  "subject": {
    "reference": "my/cmake:3.28",
    "digest": "sha256:..."
  },
  "signature": {
    "bundle_digest": "sha256:...",
    "referrer_manifest_digest": "sha256:...",
    "bundle_media_type": "application/vnd.dev.sigstore.bundle.v0.3+json"
  },
  "identity": {
    "subject": "user@example.com",
    "issuer": "https://accounts.google.com",
    "source": "ambient:github-actions"
  },
  "transparency_log": {
    "log_index": 123456,
    "log_id": "sha256:..."
  },
  "registry_capability": "full" | "referrers-missing-manifest-walk-required"
}
```

**JSON error output:** piggybacks on the v1 CLI-level JSON error-reporting DTO added under the Codex R3 findings (DF-CODEX-1 artifact response); same envelope, populated variant differs.

## 8. Key Design Decisions (v1)

| # | Decision | Pick | Rationale |
|---|---|---|---|
| SIGN-1 | Signing algorithm | **ECDSA P-256 + SHA-256** (Fulcio default) | Only algo Fulcio issues certs for in 2026; Ed25519 cert support is pending |
| SIGN-2 | Bundle format to write | **Sigstore bundle v0.3 only** | Legacy `vnd.dev.cosign.artifact.sig.v1+json` is OCI-tag-based, not referrer-based; writing it would split discovery paths |
| SIGN-3 | DSSE attestation signing | **Not in v1 (defer to v2+)** | sigstore-rs 0.13 lacks DSSE sign API; this defers `ocx package attest` to v2 |
| SIGN-4 | Re-sign idempotency | **Each invocation creates a fresh signature** | Matches cosign; content-hash dedup impossible due to non-deterministic log fields |
| SIGN-5 | Offline signing | **Rejected** — `--offline` errors with exit 77 | Fulcio + Rekor network round-trips are mandatory for keyless |
| SIGN-6 | Fallback tag on push | **Never written** (enforce single source of truth) | Preserves referrer invariant; read-side manifest-walk covers GHCR/Docker Hub |
| SIGN-7 | RFC 3161 timestamping | **Not in v1** (omit TSA field from bundle) | Optional in bundle v0.3; Fulcio 2026 does not require it; adds complexity without user-visible benefit at v1 |
| SIGN-8 | Persistent signing config | **Not in v1**; all knobs via flags or env | Avoids creating yet another config surface; `--oidc-issuer` / `--fulcio-url` / `--rekor-url` as flags is cosign-compatible |

## 9. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| sigstore-rs 0.13 signing API has latent bugs (pre-1.0) | Hard-pin; OCX wraps behind `SigningBackend` trait; integration tests with mock Fulcio/Rekor; CI smoke test against real Sigstore staging (`https://oauth2.sigstore.dev/auth` on `fulcio.sigstage.dev`) gated behind `--feature ci-staging` |
| Fulcio / Rekor outage during CI | Typed exit codes 78/82 distinguish the two SLOs; retry with exponential backoff (max 3 attempts, jitter); docs include "if Rekor returns 5xx repeatedly, check status.sigstore.dev" |
| CircleCI audience misconfiguration | OIDC research artifact already handles: client-side audience pre-check + actionable error |
| Ephemeral private key leaked via panic → core dump | `SigStoreSigner` uses `zeroize::Zeroize` on drop; no path that formats or logs the private key exists; unit test asserts `Debug` impl never prints key material |
| GHCR user signs, then pulls, then fails to verify | Documented limitation: README table + CLI warning at sign time; read-side manifest-walk fallback bridges it |
| TUF root-cache corruption | sigstore-rs refreshes from CDN; OCX wraps `SigstoreTrustRoot::from_offline_mode` and falls back to bundled root on corruption |

## 10. Test Fixture Strategy (Push-Side Addendum)

Complements the existing test-fixture research for read-side.

**Local signing loop (CI-safe, no live Sigstore):**
- Use Sigstore *staging* endpoints: `fulcio.sigstage.dev` + `rekor.sigstage.dev` + OIDC via ambient GHA token. Staging is cost-free and rate-limited permissively.
- Against local `registry:2`: staging Fulcio will issue certs bound to GHA identity (`https://github.com/<org>/<repo>/.github/workflows/...`); the referrer manifest is pushed to `localhost:5000`. End-to-end round-trip without touching production Sigstore.

**Deterministic fixtures (for unit-test parity with verify-side):**
- Pre-generate a signed bundle once against staging; commit the bundle + referrer manifest bytes under `test/fixtures/signing/`.
- Unit tests verify that `ocx package sign` *composes* the referrer manifest correctly (structure, `artifactType`, `subject`, `layers[0].mediaType`) — without actually calling Fulcio/Rekor.
- The Fulcio/Rekor client calls are mocked via `wiremock` (already in the dev-dependency set per the existing transport tests).

**Happy-path acceptance test (pytest):**
```python
def test_sign_then_verify_roundtrip(registry, gha_oidc):
    # requires CI environment with GHA OIDC available, or skip
    ref = push_test_package(registry, "test/pkg:1.0")
    result = ocx("package", "sign", ref, env=gha_oidc)
    assert result.returncode == 0
    out = json.loads(result.stdout)
    assert out["signature"]["referrer_manifest_digest"].startswith("sha256:")
    # now verify
    verify = ocx("verify", ref, "--certificate-oidc-issuer", "https://token.actions.githubusercontent.com",
                 "--certificate-identity", out["identity"]["subject"])
    assert verify.returncode == 0
```

## 11. What This Research Does NOT Cover

- **DSSE / in-toto / `ocx package attest`** — blocked on sigstore-rs 0.13 lacking DSSE signing; re-scope when 0.14+ ships signing support
- **Notation signing** — no Rust library (settled in `research_cosign_sigstore_notation.md`)
- **HSM / hardware-key signing** — orthogonal feature; not in v1 scope
- **OCSP / CRL checking at verify-time** — Fulcio's certs are short-lived, so OCSP is neither implemented nor needed; verification relies on Rekor SET + cert validity window
- **Key rotation UX for legacy cosign key-pair flow** — OCX skips key-based signing entirely per Decision B1

## Sources

- Sigstore Bundle Format v0.3.2 — https://docs.sigstore.dev/about/bundle/
- Cosign signing spec — https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md
- Sigstore Rust crate source — https://github.com/sigstore/sigstore-rs (v0.13 tag)
- sigstore-rs `sign` module docs — https://docs.rs/sigstore/0.13.0/sigstore/sign/
- Fulcio API spec — https://github.com/sigstore/fulcio/blob/main/openapi.yaml
- Rekor API — https://docs.rs/sigstore/0.13.0/sigstore/rekor/
- OCI Image Manifest spec (referrer/subject) — https://github.com/opencontainers/image-spec/blob/main/manifest.md
- OCI Distribution Spec Referrers API — https://github.com/opencontainers/distribution-spec/blob/main/spec.md#listing-referrers
- Chainguard "OCI v1.1 in cosign" — https://www.chainguard.dev/unchained/building-towards-oci-v1-1-support-in-cosign
- Sigstore staging endpoints — https://docs.sigstore.dev/about/security/#sigstore-staging
- `zeroize` crate — https://docs.rs/zeroize/ (defense-in-depth for ephemeral key material)
