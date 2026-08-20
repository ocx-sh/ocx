# Discover: attestation architecture map (feat/sbom-attestations)

> hex-architect Discover phase (2026-08-20), worker: sonnet
> architecture-explorer. Persisted by the orchestrator (worker had no Write).
> Orchestrator correction applied to §1 (Signer trait — verified against
> signer.rs directly). Feeds `adr_sbom_attestations.md`.

## 1. Sign subsystem (`crates/ocx_lib/src/oci/sign/`)

**`signer.rs`** — exact trait (orchestrator-verified):

```rust
#[async_trait]
pub trait Signer: Send + Sync {
    async fn sign(&self, target_digest: &Digest, token: &OidcToken,
                  fulcio_url: &Url, rekor_url: &Url)
        -> Result<SignedBundle, SignErrorKind>;
    fn signer_kind(&self) -> &'static str;   // "keyless-fulcio"
}
```

Leaf-kind error return locked by adr_oci_referrers_signing_v1.md Amendment 7.

**`bundle.rs`** — Sigstore bundle v0.3 assembly via sigstore_protobuf_specs
(`Bundle` with verification_material incl. tlog entry + inclusion proof;
content: MessageSignature — never DsseEnvelope on this path). `SignedBundle
{ bytes, digest, certificate_identity, certificate_oidc_issuer }`.
`MAX_BUNDLE_SIZE_BYTES` = 512 KiB pre-parse cap on the read side.

**`rekor.rs`** — Rekor v1 client, `hashedrekord:0.0.1` proposal, SET
extraction; upload-failure classifier split for testability (RekorUnavailable
exit 83 vs RekorSetMalformed exit 65).

**`pipeline.rs`** — `SignPipeline::run`: resolve target → capability check →
OIDC token → Signer → push bundle blob → push referrer manifest.

**CLI `package_sign.rs`**: `PackageSign { platform, fulcio_url, rekor_url,
identity_token_file (conflicts stdin), identity_token_stdin, no_tty, no_cache,
identifier }`. execute(): resolve → validate_sigstore_url (SSRF, exit 64 on
fail) → offline check → OfflineSignRefused (77) BEFORE token resolution →
resolve_override_token (O_NOFOLLOW, uid check, mode & 0o077 rejection) →
SignOptions → `context.manager().sign_one(...)` → SignatureReport.

## 2. Verify subsystem (`crates/ocx_lib/src/oci/verify/`)

**THE gate for attestations — pipeline.rs:465-501 `BundleParts::from_bundle`:**

```rust
// A DSSE envelope is an attestation, not an artifact signature — v1 verify
// handles only message signatures (attestation verify is #198). ...
if !matches!(bundle.content.as_ref(), Some(bundle::Content::MessageSignature(_))) {
    return Err(VerifyErrorKind::NoUsableBundle);
}
```

Pinned by `from_bundle_rejects_dsse_envelope` (pipeline.rs:985-1000). Also
`from_bundle_requires_a_merkle_inclusion_proof` (1003-1017): promise-only
bundles refused (`RekorInclusionProofAbsent`) — the same discipline applies to
any DSSE path. leaf_der accepts X509CertificateChain OR single Certificate
content.

**`identity.rs`**: `FULCIO_ISSUER_OID 1.3.6.1.4.1.57264.1.8` (v2, DER
UTF8String; `.1.1` deliberately unsupported), `parse_certificate`,
`subject_identity`, `oidc_issuer`, `verify_policies(cert_der, &[CompiledPolicy])`
— ANY-of; IssuerMismatch when identity matched, else IdentityMismatch.
Reuse these; never reimplement OID parsing.

**`tlog.rs`**: `SetPayload {body, integrated_time, log_index, logID}`
(hand-declared to avoid sigstore's `cosign` feature), `rekor_key(pem)`,
`verify_set(key, entry)`, `verify_inclusion(key, proof, canonicalized_body)`.
Content-agnostic over canonicalized_body → reusable for dsse entries as-is;
divergence is entirely in BundleParts + payload parsing.

**`trust_resolve.rs`**: `resolve_trust_root(explicit_override, sigstore,
home_trusted_root, state, rekor_cache_key, offline)` — six-rung ladder (flag ▸
env ▸ [trust.sigstore] ▸ $OCX_HOME/sigstore/trusted-root.json ▸ cache ▸
TUF public-good). Shared by package_verify AND auto_verify.

**`trust_cache.rs`**: `$OCX_HOME/state/trust_root/<rekor-slug>.json`.

**`error.rs` VerifyErrorKind** (confirmed subset): BundleParseFailed,
NoUsableBundle, RekorSetInvalid, RekorInclusionProofAbsent, CertChainInvalid,
IdentityMismatch, IssuerMismatch, InvalidEndpointUrl, NoIdentityProvided,
TrustPolicyInvalid, RekorUnavailable (kind_detail "rekor_unavailable").

## 3. Referrer subsystem (`oci/referrer/`)

`manifest.rs` ReferrerManifest::build(subject, artifact_type, payload) +
to_canonical_json. `media_types.rs`: SIGSTORE_BUNDLE_V03, EMPTY_CONFIG,
EMPTY_CONFIG_PAYLOAD (b"{}"), EMPTY_CONFIG_DIGEST sha256:44136fa3…,
EMPTY_CONFIG_SIZE 2. `capability.rs`: probe + cache
`$OCX_HOME/state/referrers/<registry>.json`, consumed by both pipelines.

## 4. Transport (`oci/client/native_transport.rs`) — confirmed

```rust
async fn push_referrer_manifest(&self, image, _subject_digest, manifest_bytes, media_type) -> Result<oci::Descriptor>  // :454-490, digest-addressed PUT
async fn list_referrers(&self, image, subject_digest, artifact_type: Option<&str>) -> Result<Vec<oci::Descriptor>>     // :492-515
```

- `list_referrers`: None from pull_referrers_native → `ClientError::ReferrersUnsupported`
  (never silently empty).
- `filter_and_convert_referrers` (:219-243): client-side artifact_type
  re-filter (server MAY ignore the query param); **`artifact_type` AND
  `annotations` survive into `oci::Descriptor`** — predicateType-annotation
  narrowing needs no second fetch and no transport change.
- 404/NAME_UNKNOWN/MANIFEST_UNKNOWN → ReferrersUnsupported; 5xx/rate-limit
  stay ClientError::Registry (test-pinned :833-899).

## 5. CLI / trust / envelope

**`package_verify.rs`**: `PackageVerify { platform, certificate_identity
(requires issuer), certificate_oidc_issuer (requires identity), rekor_url,
no_cache, trusted_root: Option<PathBuf>, identifier }`. execute(): resolve →
validate_sigstore_url → verify_client + is_offline → rekor_cache_key →
resolve_trust_root → resolve_policies (flag-pair overrides tiered policies;
empty → NoIdentityProvided 64) → VerifyOptions → manager().verify_one →
VerificationReport. `--offline` scopes to Sigstore services, not the artifact
registry; verify offline ≠ exit 81.

**`options/verify.rs`**: two structs — `Verify` (login ping) and
`SignatureVerify` (the auto-verify gate; OCX_NO_VERIFY-aware; flag wins;
POSIX last-wins).

**No `SignContext`/`VerifyContext` accessor on `Context`** — CLI builds
`SignOptions`/`VerifyOptions` → `manager().sign_one()/verify_one()`; the
manager constructs pipeline-internal contexts. An attest command mirrors this
(AttestOptions → manager().attest_one()).

**`auto_verify.rs`** (fully mapped): `AutoVerify` struct with memoized
`Arc<OnceCell<TrustRoot>>` + WARN-once `Arc<AtomicBool>`; fires in
`setup_impl` (every install surface) after resolve, before download;
five-step gate; lazily resolves trust root only when a policy matches;
hoisted one-time Rekor-key fetch re-applies the SSRF floor (test-pinned).

**`trust.rs`** (leaf module — MUST NOT depend on `oci`): TrustConfig
{policy: Vec<TrustPolicy>, sigstore: Option<SigstoreTrust>} (merge:
policy array-append, sigstore field-replace); TrustPolicy {scope,
identity: Option, identity_regexp: Option, oidc_issuer, #[serde(skip)]
system_locked}; compile() = identity XOR identity_regexp →
CompiledPolicy {identity: IdentityRule, issuer}; resolve(): system-locked
matches preempt; else longest-literal-prefix, ANY-of among equals;
resolve_tiered(): operator match is authoritative over project tier.
**No deny_unknown_fields anywhere (fleet tolerance, test-pinned);
system_locked cannot be set from TOML (test-pinned).**

**Field-consumer blast radius for the keyless nesting refactor: 3 files** —
trust.rs (definition), oci/verify/identity.rs (the one real consumer),
config/loader.rs (one test assertion). Schema: TrustConfig/SigstoreTrust
derive schemars::JsonSchema, reached transitively via Config + ProjectConfig
roots → both config/v1.json and project/v1.json regenerate
(task schema:generate).

**`error_envelope.rs`** (fully mapped): ENVELOPE_SCHEMA_VERSION 1;
`ErrorCategory` enum (serde snake_case) — **RekorUnavailable and
ReferrersUnsupported are their own top-level `kind` values** (exit 83/84 map
1:1); `from_exit_code` total map test-pinned at 15 rows
(`error_category_total_over_exit_codes` :524-569). `collect_detail` walks the
source chain for the first SignErrorKind/VerifyErrorKind → `kind_detail()`.
`sign_error_into_anyhow`/`verify_error_into_anyhow` unwrap to the bare error
so the envelope's context/identifier survives — an attest command needs the
same unwrapping.
**Slug-rename scope**: `rekor_unavailable` appears as (a) ErrorCategory
variant serde name (kind), (b) kind_detail() strings in Sign/VerifyErrorKind.

**DTOs** (`api/data/signature.rs`, `verification.rs`): SignatureReport
{identifier, subject_digest, bundle_digest, referrer_digest, platform,
signer, certificate_identity, certificate_oidc_issuer}; VerificationReport
{subject_digest, referrer_digest, certificate_identity,
certificate_oidc_issuer, signed_at}. Doc comment: "A future multi-signature
slice can add a signatures[] array without breaking these top-level fields."
Pattern: sibling DTO per verb (AttestationReport/SbomReport), CWE-150
sanitization on every field with per-attack-class corpora.

## 6. Push seam (`publisher.rs` — fully mapped)

`PushOutcome { manifest_digest, cascade_tags, canonical_tags, layer_counts }`.
- `manifest_digest` = pushed image-INDEX digest (last platform's merge on
  fan-out).
- **`canonical_tags` = digest-named `sha256.<hex>` tags, one per distinct
  per-platform manifest** — attest-after-push must target per-platform
  manifest digests (recoverable from canonical_tags), not the index digest
  alone.
- Per-platform pushes are sequential (index merge is read-modify-write).
- `PushOutcome` is a cross-tool contract (ocx-mirror parses the report) —
  extending it is wire-adjacent, not internal.

## 7. Exit codes (`cli/exit_code.rs`)

0/1/64/65/69/74/75/77/78/79/80/81/82/**83 RekorUnavailable**/**84
ReferrersUnsupported**. Next free: **85**. Values test-pinned against
research_exit_codes.md. Classifier pairs: ClassifyExitCode for
SignError/VerifyError; ClassifyErrorKind for SignErrorKind/VerifyErrorKind.

## 8. Acceptance harness (`test/`)

- compose `sigstore` profile (7 services): dex v2.45.1 (iss
  http://dex:5556/dex), TesseraCT posix CT log, fulcio v1.8.8 (fileca,
  committed test CA), Trillian (mysql + log-server + log-signer), rekor
  v1.4.2. Readiness via `sigstore/wait-for-stack.py` (distroless — no compose
  healthchecks). Ports: OCX_TEST_{DEX,FULCIO,REKOR,CT}_PORT.
- **Primary registry = zot v2.1.18 (native Referrers API + OCI-Subject
  header); `mirror-registry` (registry:2, port 5001) doubles as the permanent
  exit-84 negative fixture.** registry:3 verified to 404 on /referrers/.
- `helpers.mint_identity_token(target) -> Path` (dex token via
  sigstore/get-token.py; file-permission-hardened), `start_sigstore_stack()`.
- test_sign.py: 19 tests (offline-refused, exit 83/84, SSRF rejections,
  token precedence/permissions, idempotent re-sign, no-token-forwarding).
- test_verify.py: 15 tests incl. golden envelope shapes
  (test_verify_error_envelope_golden_shape /
  test_verify_success_envelope_golden_shape — THE pattern for new
  test_attest/test_sbom envelope pinning), tampered SET/signature, spliced
  bundle onto foreign subject, malformed-referrer-does-not-block-valid.
- test_cosign_interop.py: cosign invoked via subprocess (both directions:
  cosign verifies ocx bundle; ocx verifies cosign bundle).

## 9. Docs surfaces

- in-depth/signing.md sections: Trust Root, Referrers Capability Cache,
  Hard-Fail Policy, Bundle Format and Storage, cosign Interoperability,
  Identity Matching, Choosing a Sigstore Deployment, Signing from CI,
  **Slice Boundary, Current Limitations, Deferred to Future Work** (the
  SEC-32 surfaces an attestation feature must update), Offline Verification,
  Signing Flow Summary.
- self-hosted-sigstore.md: components / trusted-root / distribution / issuer
  matrix / identity_regexp authorization / verify-setup / coverage.
- reference/environment.md: OCX_SIGSTORE_TRUSTED_ROOT (:220), OCX_NO_VERIFY
  (:574, forwarded to children; TRUSTED_ROOT is not).
- reference/command-line.md: single exit-code table at `## Exit codes`
  (:295); commands from :428.
- reference/configuration.md: `[[trust.policy]]` (:555, array-append
  exception documented :668), `[trust.sigstore]` (:767).
- **Casts: general infra exists (Terminal.vue + recordings.taskfile.yml);
  no signing/verify cast recorded yet.** New docs with casts use that
  mechanism.

## 10. Prior ADRs (headings scanned)

- adr_oci_referrers_signing_v1.md — S1-A..S1-I (S1-D: DSSE not-in-v1,
  superseded rationale — sigstore-rs 0.13 gap, since dissolved), exit-code
  taxonomy, error-kind inventory, frozen envelope C-S1-1, referrer manifest
  shape, Not-Doing guardrails, Forward-Compat Hooks for v2, 9 amendments.
- adr_real_sigstore_stack_and_delegation.md — D1..D6 (real stack, delete the
  fake, cosign v3 interop #197, SSRF dial guard, negative tests a real stack
  cannot produce).
- adr_trust_policy.md — D1..D7 (leaf-module constraint, array-append,
  scope resolution, system_locked amendment, TWO ocx_schema additions).
- adr_offline_verify_trust_cache.md — offline semantics + trust cache;
  Amendment 2026-08-19 = six-rung ladder (most recent, authoritative).
- adr_oci_artifact_enrichment.md — parent ADR; Phase 4 SBOM deferred there;
  media-type registry; registry compatibility matrix.

## Synthesis (facts, no design)

The single load-bearing change point on verify is `BundleParts::from_bundle`
pipeline.rs:498 (test-pinned DSSE rejection). tlog.rs, identity.rs,
trust_resolve.rs, trust.rs are content-agnostic and reusable as-is. Transport
already preserves artifact_type + annotations through list_referrers.
PushOutcome already surfaces what attest-after-push needs (per-platform via
canonical_tags). Exit 85 is the next free slot. The envelope slug rename
touches ErrorCategory + two kind_detail() sites. DTOs extend by sibling type,
never by mutating VerificationReport.
