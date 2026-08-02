# Spike — sigstore-rs capability probe (issue #194, Step 0)

> Empirical probe of `sigstore` crate v0.14.0 in the OCX workspace (2026-07-09).
> Method: `cargo add` + resolve + `cargo build -p ocx_lib` (exit 0) + `cargo deny
> check` + direct reading of the vendored crate source at
> `~/.cargo/registry/.../sigstore-0.14.0/src`. Gates #107 (Rekor v2 delta) and
> #198 (DSSE attestation engine). Supersedes the 0.13-era assumptions in
> `adr_oci_referrers_signing_v1.md` (Amendments 4/5 name 0.13).

## Version & build facts

- **Latest release: `sigstore = 0.14.0`** (not 0.13 as the ADR assumes). `native-tls`
  is a DEFAULT feature — MUST use `default-features = false` + `rustls-tls` (workspace
  is rustls-only).
- Compiles cleanly in the workspace (`cargo build -p ocx_lib` exit 0). **No
  `oci-client`/`oci-distribution` conflict** as long as the `registry` and
  `cached-client` features stay OFF (we have our own OCI transport).
- Feature set used for the probe: `["sign","verify","bundle","fulcio","rekor","sigstore-trust-root","rustls-tls"]`, `default-features = false`.
- Transitively provides the crypto we need: `p256 0.13`, `ecdsa 0.16`,
  `ed25519-dalek 2.2`, `x509-cert 0.2`, `sigstore_protobuf_specs 0.5.1` (Bundle v0.3
  types), `tough 0.22` (TUF), `openidconnect 4.0.1`.

### License / advisory

- `cargo deny check licenses` — **green** (one benign `no-license-field` warning on
  `sigstore-protobuf-specs-derive 0.0.1`, still resolves via its license file).
- `cargo deny check advisories` — **failed** on `rsa 0.9.10` (RUSTSEC-2023-0071,
  Marvin timing sidechannel) pulled transitively via `sigstore → openidconnect`.
  `openidconnect` is a NON-optional dep of sigstore (every feature pulls it); `rsa`
  has no safe upgrade. **Resolution: documented ignore in `deny.toml`** — OCX signs
  and verifies with ECDSA P-256 only (ADR S1-A), holds no RSA private key, and the
  attack targets private-key decryption timing, not the public JWT-verify path OCX
  exercises. `cargo deny check` is green after the ignore.

## Verdicts (the questions Step 0 asked)

| Capability | Verdict | Detail |
|---|---|---|
| **Keyless sign (Fulcio)** | YES, but **unusable against the fake stack** | `SigningContext::{new,production}` + `SigningSession::sign`. `sign_digest` **mandatorily calls `verify_sct()`** against a CT-log keyring. The fake Fulcio embeds no SCT and there is no CT log → `SigningSession` rejects the fake cert. **We hand-roll the sign flow** reusing the lower-level `FulcioClient::request_cert_v2` (no SCT check at that layer) + `p256`/`x509-cert`. |
| **Bundle v0.3 write** | YES | `Version::Bundle0_3 → "application/vnd.dev.sigstore.bundle.v0.3+json"` (exact ADR media type). `sigstore_protobuf_specs::dev::sigstore::bundle::v1::Bundle` is serde-`Serialize`/`Deserialize`. NOTE: `SigningArtifact::to_bundle()` emits **v0.2** — so we build the `Bundle` struct by hand and set `media_type = Bundle0_3`. |
| **TUF TrustedRoot fetch** | YES (network) | `SigstoreTrustRoot::new(None)` fetches from the TUF CDN (feature `sigstore-trust-root`). Network-only → stays behind the default (non-test) path, documented as network-gated. |
| **Offline / custom trust-root override** | YES — **this is the seam enabler** | `sigstore::trust::ManualTrustRoot<'a> { fulcio_certs: Vec<CertificateDer>, rekor_keys: BTreeMap<String,Vec<u8>>, ctfe_keys: BTreeMap<String,Vec<u8>> }` implements the `TrustRoot` trait with out-of-band material — no network. Lets the acceptance suite inject a fake Fulcio CA. |
| **Rekor v1** | YES | `rekor::apis::entries_api::create_log_entry(&Configuration, ProposedEntry)`; `Configuration.base_path` overridable to the fake URL. `hashedrekord:0.0.1` proposal type present. |
| **Rekor v2** | **NO** | No v2 client surface in 0.14 (v2 GA'd 2025-10; Go/cosign have it, Rust does not). **#107 stays as the Rekor-v2 delta**, gated on a future sigstore-rs release. Do NOT close #107 into #194. |
| **DSSE / in-toto attestation VERIFY** | **YES (SOTA correction)** | 0.14 added a `bundle::verify` module. `bundle/verify/models.rs` handles `bundle::Content::DsseEnvelope`: computes DSSE PAE (`compute_pae`), verifies the envelope signature, and parses `InTotoStatementV1`; `bundle/intoto.rs` exposes `validate_cosign_v1` + `subject_sha256_digest`. **The ADR/plan claim "sigstore-rs does not verify attestations" is a 0.13-era fact and is now OUTDATED.** |

## Implications for downstream issues

- **#107 (Rekor v2):** keep as a real delta. No day-one v2 support; the pipeline lands
  on Rekor v1 + custom trust root. Record "v2 deferred pending sigstore-rs support" in
  `rekor.rs`.
- **#198 (DSSE attestation engine):** **re-scope.** The split plan (action-table item
  10) costs #198 as "sigstore-rs does not verify attestations → verify side
  hand-rolled/second crate." That is no longer true at 0.14. #198 should first spike
  whether `sigstore::bundle::verify` can be driven for DSSE/in-toto verification
  (PAE + `InTotoStatementV1` already implemented) before committing to a hand-rolled
  engine. Signing DSSE is still likely absent (the `bundle::sign` path only emits
  `MessageSignature`), so attach may still be partly hand-rolled — but VERIFY is much
  cheaper than the plan assumes.

## Design consequence for #194 (why we hand-roll)

The fake Sigstore stack (`test/tests/fixtures/fake_sigstore.py`) deliberately produces
NO CT-log SCT and a non-standard Ed25519 Rekor SET (over a canonical JSON of
`{body, integratedTime, logID, logIndex}`). sigstore-rs's high-level `SigningSession`
(SCT-mandatory) and `bundle::verify` (real Rekor SET / SCT) therefore cannot round-trip
the fake. Because OCX controls both ends of the round-trip (sign + verify) and the fake,
#194 hand-rolls:

- **sign:** `p256` keygen → `x509-cert` CSR → `FulcioClient::request_cert_v2` (custom
  URL, reused) → ECDSA-P256 sign of the subject digest → `create_log_entry` (custom
  URL, reused) → build `Bundle` v0.3 by hand.
- **verify:** parse `Bundle` → validate leaf cert against the injected Fulcio CA (trust
  root) → SAN/issuer-OID identity match → ECDSA-P256 signature check over the subject
  digest → Ed25519 Rekor-SET check.
- **trust-root seam:** `--trust-root <path>` flag + `OCX_SIGSTORE_TRUST_ROOT` env,
  loading a Sigstore `TrustedRoot` JSON (via the in-tree `sigstore_protobuf_specs`
  `TrustedRoot` type). Production default = embedded/TUF root (network-gated, documented).

Any real-network Sigstore integration (public-good Fulcio/Rekor/TUF) stays behind an
`#[ignore]` test and is never claimed green without evidence.
