# Research: sigstore dependency surface in ocx

## Metadata

**Date:** 2026-11-23
**Domain:** security
**Triggered by:** discussion `.agents/discussions/rekor-v2-sigstore-rust.md` — sizing a Rekor v2 migration (ocx-sh/ocx#107) and a possible swap of the `sigstore` crate for `sigstore-rust`
**Expires:** 2027-02-23 (code recon at branch `qui` @ `27e79b0a`; re-run after any `ocx_sign` restructuring)

## Direct Answer

The `sigstore` crate is a dependency of `ocx_sign` and `ocx_trust` only. OCX
uses it for bundle verification, crypto key types, Rekor *models* and the
trust-root trait. OCX hand-rolls the Fulcio client, the Rekor v1 write client,
ambient OIDC, keyless signing, simplesigning read/write, DSSE/in-toto statements
and SBOM referrers. No `SigningConfig` handling exists anywhere.

## Findings

### 1. API use (93 `sigstore::` hits in 17 files, ~60 `sigstore_protobuf_specs` import lines)

- **Bundle and protobuf types (~25 sites):** `sigstore::bundle::Bundle`
  (`crates/ocx_sign/src/sign/bundle.rs:6`); `sigstore_protobuf_specs` `Bundle`,
  `VerificationMaterial`, `TransparencyLogEntry`, `InclusionPromise`,
  `InclusionProof`, `Checkpoint`, `X509Certificate(Chain)`, `MessageSignature`,
  in-toto `Envelope`/`Signature` — `sign/bundle.rs:7-12`,
  `verify/pipeline.rs:48-49`, `verify/attestation_sidecar.rs:12-15`,
  `verify/simplesigning_read.rs:13-15`, `verify/dsse.rs:16`,
  `verify/candidates.rs:6`, `sign/referrers.rs:6`.
- **Verification (~35 sites):** `sigstore::bundle::verify::{Verifier,
  VerificationError, policy::{PolicyResult, VerificationPolicy}}` —
  `verify/pipeline.rs:9-10,916,1376,2234`, `simplesigning_read.rs:11`,
  `attestation_sidecar.rs:535`. `Verifier::new(RekorConfiguration::default(),
  trust_root)` at `pipeline.rs:916`. `sigstore::crypto::{CosignVerificationKey,
  Signature, SigningScheme}` — `verify/tlog.rs:10-11`, `verify/identity.rs:159,274`,
  `crates/ocx_trust/src/lib.rs:707,758,778,838-839`,
  `crates/ocx_trust/src/publishers.rs:197-199`.
- **Rekor models (~8 sites):** `rekor::models::{InclusionProof, Hashedrekord,
  hashedrekord, log_entry::RekorInclusionProof}` — `sign/rekor.rs:13`,
  `tlog.rs:12-13`, `simplesigning_read.rs:12,410,477`, `bundle.rs:58`.
- **TUF / TrustedRoot (6 sites):** `sigstore::trust::{TrustRoot,
  sigstore::SigstoreTrustRoot}` in `verify/trust_root.rs:13-14,204-216`; the
  `TrustRoot` trait is implemented by hand.
- **Key signing (~5 sites):** `SigStoreKeyPair::from_encrypted_pem`
  (`sign/key_backend.rs:152,403`); `SigstoreError` at `key_backend.rs:350`,
  `ocx_trust/src/publishers.rs:199`.
- **Hand-rolled, not via sigstore:** Fulcio (`sign/fulcio.rs`), Rekor v1 POST
  (`sign/rekor.rs`), ambient OIDC (`sign/oidc_ambient_inline.rs`), keyless
  signing (`sign/key_signer.rs:6` bypasses `sigstore::bundle::sign`),
  simplesigning write/read, DSSE/in-toto statements (`attest/*`), SBOM referrers.

### 2. The fork

- `.gitmodules:11-15`: `ocx-sh/sigstore-rs`, branch `ocx/integration`, based on
  tag `v0.14.0`. Root `Cargo.toml:26` patches `sigstore` to `external/sigstore-rs`.
- Features: workspace `bundle`, `rustls-tls`, `default-features=false`
  (`Cargo.toml:195`); `ocx_sign` adds `sign`, `verify`, `fulcio`, `rekor`,
  `sigstore-trust-root` (`crates/ocx_sign/Cargo.toml:38-44`).
- Recorded delta: only `SigstoreTrustRoot::new_with_client`, so the TUF fetch
  uses the shared reqwest client (`.claude/rules/subsystem-deps.md:117`).
- `Cargo.toml:138`: sigstore's own Fulcio/Rekor clients were rejected because
  they require an SCT and use a different wire shape than the offline fake stack.

### 3. Rekor today

- `DEFAULT_REKOR_URL = "https://rekor.sigstore.dev"` hardcoded
  (`crates/ocx_oci/src/endpoint.rs:19`); used at
  `crates/ocx_cli/src/app/context.rs:1834`,
  `command/package_sign_common.rs:207,774`. Fulcio default also hardcoded
  (`package_sign_common.rs:183`). Both overridable by `--rekor-url` /
  `--fulcio-url`.
- The trusted root supplies the Rekor key and Fulcio CA only.
- `sign/rekor.rs:4-6`: "Rekor v1 transparency-log client … Rekor v2 is not
  supported: ocx-sh/ocx#107".
- Entry kinds written: `hashedrekord` (`rekor.rs:134`), `dsse:0.0.1`
  (`rekor.rs:153,266`).
- Verification offline (SET + Merkle inclusion proof against a checkpoint, from
  the bundle — `verify/tlog.rs:4,46,65`) and online (Rekor public-key fetch,
  trust-on-first-use — `pipeline.rs:1712,1767`).
- `pipeline.rs:2637`: a v2 bundle this build cannot read is ranked unparseable.

### 4. Persisted and wire formats (`crates/ocx_oci/src/referrer/media_types.rs`)

- `SIGSTORE_BUNDLE_V03 = "application/vnd.dev.sigstore.bundle.v0.3+json"` (:9)
- `ANNOTATION_BUNDLE_CONTENT` (:49), `ANNOTATION_BUNDLE_PREDICATE_TYPE` (:51),
  `BUNDLE_CONTENT_DSSE = "dsse-envelope"` (:54)
- `SIMPLESIGNING_MEDIA_TYPE = "application/vnd.dev.cosign.simplesigning.v1+json"` (:58)
- `COSIGN_SIG_ARTIFACT_TYPE` (:62), `COSIGN_SBOM_ARTIFACT_TYPE` (:66),
  `DSSE_ENVELOPE_MEDIA_TYPE` (:70)
- `DSSE_PAYLOAD_TYPE = "application/vnd.in-toto+json"`
  (`crates/ocx_sign/src/attest.rs:34`); cosign-compat sidecar tags in
  `simplesigning_read.rs`.

### 5. Safety net

- Rust: ~542 test attributes in `ocx_sign` across 32 files
  (`verify/pipeline.rs` 118, `attest/pipeline.rs` 38, `identity.rs` 31).
- Python acceptance: ~288 `test_` functions in 19 sign/verify/attest/SBOM/trust/
  cosign modules; cosign interop matrix (`test_cosign_matrix_*`,
  `test_cosign_interop`).
- Real local Sigstore stack via `test/tests/fixtures/sigstore_stack.py`.

### 6. Coupling

- `ocx_trust` exposes `sigstore::crypto::CosignVerificationKey` in its own
  public signatures (`crates/ocx_trust/src/lib.rs:707,758,778,838`).
- `ocx_sign::verify` re-exports an ocx-owned `TrustRoot` implementing the
  sigstore trait (`verify.rs:41`). Errors are wrapped in `VerifyError` / `SignError`.

### 7. Transitive weight

- rustls only; `deny.toml:72-75` bans `openssl`, `openssl-sys`, `native-tls`.
- `sigstore-trust-root` pulls `tough`. `rsa` (RUSTSEC-2023-0071) arrives via
  sigstore → openidconnect and cannot be feature-gated out (`deny.toml:16-23`).
- Workspace pins RustCrypto crates (`p256`, `x509-cert`, `ed25519-dalek`,
  `sha2`) to sigstore 0.14's versions (`Cargo.toml:190-211`).

## negative

- No `SigningConfig` handling anywhere; no Rekor v2 code.
- `sigstore` not used in `ocx_oci`, `ocx_cli`, `ocx_package`.
- `.claude/rules/subsystem-oci.md` still lists old `oci/sign/*` paths.
- Not established: real-vs-fake stack split in acceptance tests; aws-lc-rs vs
  ring; every public `ocx_sign` signature; the `ocx-mirror` satellite; the
  fork's actual git log (submodule not checked out in this worktree).

## leads

- Fork archaeology: `git submodule status` plus upstream diff for fork-only commits.
- `crates/ocx_trust/src/lib.rs:700-840` — the main public-API coupling.
- `verify/pipeline.rs:1376,4497` — key mode bypasses `sigstore::bundle::verify`.
- `ocx-mirror` satellite consumption of `ocx_sign` / `ocx_trust`.
