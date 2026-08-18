# sigstore-rs 0.14 — capability matrix and the visibility wall

> **Read together with `research_sigstore_rs_api_surface.md`.** That file was being written
> concurrently by another worker; this one was split off to avoid clobbering it. The two
> overlap on the Fulcio/Rekor/TUF findings and **disagree on three verdicts**. Where they
> disagree, the evidence here is `cargo check` output and it wins — see §9.
>
> Ground truth: the vendored crate at
> `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sigstore-0.14.0/src`, plus probe
> crates compiled against it. **docs.rs is unusable for this question** — it renders
> `--all-features` and does not surface `pub(crate)`, which is the axis that decides
> nearly half the answers below.
>
> Probes: `~/.cache/ocx-claude/m25/probe` (features `["bundle","rustls-tls"]` — the
> workspace's current pin) and `~/.cache/ocx-claude/m25/probe2` (`+ sigstore-trust-root`).

## Verdict: can ocx delete its hand-rolled stack?

**PARTIAL, and the split is not where it looks.**

Delegate: the Fulcio client, the Rekor v1 client, Merkle-inclusion + checkpoint
verification, cert-chain building + temporal validity, the TUF trust root, and
self-hosted/offline **verification**.

Cannot delegate: bundle-v0.3 **writing**, keyless **signing** against a self-hosted Fulcio,
standalone SCT verification, Rekor v1 SET verification inside the bundle path, and ambient
CI OIDC. Rekor v2 is absent entirely.

**0.14.0 is the latest release (2026-05-22) and `main` is not staged ahead of it, so no
upgrade relieves any of this.**

## 1. Capability matrix

| # | Capability | Verdict |
|---|---|---|
| 1 | Write bundle **v0.3** | **PARTIAL** — media-type constant is in a private module; the writer hardcodes v0.2 |
| 2 | Read + verify a bundle end-to-end | **PARTIAL** — reads v0.3 correctly; the verifier **skips SET and Merkle** |
| 3 | Fulcio client — CSR, issuance, full chain | **YES** |
| 4 | Rekor v1 client — upload, retrieve | **YES** |
| 5 | Rekor v1 **canonical SET** verification | **PARTIAL** — real algorithm present but `pub(crate)`; only a cosign-v1-shaped wrapper is public |
| 6 | Merkle inclusion proof + checkpoint/STH | **YES** — public, but not wired into the bundle verifier |
| 7 | SCT extraction + verification | **NO** — module is `pub(crate)`, no re-export |
| 8 | Cert chain build/validate + temporal validity | **PARTIAL** — happens *inside* the verifier; primitives are `pub(crate)` |
| 9 | TUF trust root + embedded production root | **YES** — and it does embed the production root |
| 10 | `ManualTrustRoot` for self-hosted/test | **YES for verify · NO for sign** |
| 11 | Offline verification | **YES** |
| 12 | OIDC — ambient/CI and interactive | **PARTIAL** — interactive PKCE yes; **no ambient/CI detection at all** |
| 13 | Rekor **v2** | **NO** — zero surface |

## 2. The visibility wall

`crypto/mod.rs`, verbatim:

```
 22: pub(crate) mod merkle;
131: pub(crate) mod certificate;
133: pub(crate) mod certificate_pool;
135: pub(crate) use certificate_pool::CertificatePool;
137: pub(crate) mod keyring;
139: pub mod verification_key;
147: pub mod signing_key;
150: pub(crate) mod transparency;
```

A probe importing each produces **six `E0603: module is private` errors**. A sweep for
re-exports (`pub use` of `Keyring`, `CertificatePool`, `verify_sct`, `merkle`,
`transparency`) returns **none**.

So `verify_sct`, `CertificateEmbeddedSCT`, `CertificatePool::verify_cert_with_time`,
`MerkleProofVerifier` and `Rfc6269Default` are **real, complete, well-tested, and
unreachable from ocx**. They run only as internal side effects of
`bundle::verify::Verifier`.

> This is the trap: reading the source and finding a 398-line `crypto/transparency.rs` or a
> 905-line `crypto/merkle/proof_verification.rs` naturally reads as "the capability exists,
> delete our copy". It does exist. It is not callable. Check the `mod` line before every
> such verdict.

**What compiles under the current pin `["bundle","rustls-tls"]`** (probe, exit 0):

```rust
sigstore::bundle::Bundle
sigstore::bundle::sign::SigningContext              // nameable; ctor unusable — §3
sigstore::bundle::verify::{Verifier, VerificationPolicy, policy}
sigstore::trust::{TrustRoot, ManualTrustRoot}
sigstore::fulcio::{FulcioClient, TokenProvider, FULCIO_ROOT}
sigstore::rekor::apis::configuration::Configuration
sigstore::rekor::apis::entries_api::create_log_entry
sigstore::rekor::models::log_entry::{LogEntry, RekorInclusionProof}
sigstore::rekor::models::{InclusionProof, ConsistencyProof}
sigstore::rekor::models::checkpoint::SignedCheckpoint
sigstore::crypto::{CosignVerificationKey, Signature}
sigstore::oauth::IdentityToken
```

## 3. Signing against a self-hosted Fulcio is impossible through the public API

`bundle/sign.rs`:

```rust
pub fn new(fulcio: FulcioClient, rekor_config: RekorConfiguration, ctfe_keyring: Keyring) -> Self
#[cfg(feature = "sigstore-trust-root")] pub async fn async_production() -> SigstoreResult<Self>
#[cfg(feature = "sigstore-trust-root")] pub fn production() -> SigstoreResult<Self>
```

`Keyring` is in `pub(crate) mod keyring` (`crypto/mod.rs:137`), is not re-exported, and no
public function returns one. Probe, forcing the compiler to name it:

```
error[E0308]: mismatched types: expected `Keyring`, found `()`
error[E0603]: module `keyring` is private
```

`production()` hardcodes public-good Sigstore, and on the workspace's **current** feature
pin does not exist at all:

```
error[E0599]: no function or associated item named `production` found for struct `SigningContext`
error[E0599]: no function or associated item named `production` found for struct `Verifier`
```

Both appear only with `sigstore-trust-root` (probe2, exit 0).

**Upstream confirms**: [sigstore-rs#562](https://github.com/sigstore/sigstore-rs/issues/562)
(open, 2026-04-26) — *"`Keyring` is `pub(crate)` and not re-exported in the public module
surface"*, *"the only escape is to vendor the `Keyring` construction code into our own
crate"*.

Compounding it: `SigningSession::sign_digest` verifies an SCT **unconditionally**
(`bundle/sign.rs:140,143`) — detached from the Fulcio response header, else reconstructed
from the certificate extension. No flag skips it. So a self-hosted stack must run a real CT
log regardless.

> **Consequence:** replacing the Python fake with a real Fulcio + Rekor + CT log is
> necessary but **not sufficient**. The blocker is an upstream Rust visibility decision.
> ocx keeps an ocx-owned signing orchestration built from public pieces:
> `FulcioClient::request_cert_v2` + `p256`/`x509-cert` + `create_log_entry` + a
> hand-assembled `Bundle`. **None of that is hand-rolled crypto or a hand-rolled wire
> format**, which is what the non-negotiable forbids.

## 4. The verifier skips two of its own seven steps

`bundle/verify/verifier.rs:197-203`, verbatim:

```rust
// 5) Verify the inclusion proof supplied by Rekor for this artifact,
//    if we're doing online verification.
// TODO(tnytown): Merkle inclusion; sigstore-rs#285

// 6) Verify the Signed Entry Timestamp (SET) supplied by Rekor for this
//    artifact.
// TODO(tnytown) SET verification; sigstore-rs#285
```

| Step | Done? | Evidence |
|---|:-:|---|
| 1. cert chains to trusted root, valid at signing time | yes | `cert_pool.verify_cert_with_time(&ee_cert, UnixTime::since_unix_epoch(issued_at))` |
| 1b. SCT verified | yes | `verify_sct(&sct_context, &self.ctfe_keyring)` |
| 2. identity policy | yes | `policy.verify(&materials.certificate)?` |
| 3. artifact signature | yes | `verify_bundle_content(...)` |
| 4. entry consistent with materials (CVE-2022-36056) | yes | `materials.tlog_entry(offline, &input_digest)` |
| **5. Merkle inclusion proof** | **NO** | `TODO` |
| **6. Signed Entry Timestamp** | **NO** | `TODO` |
| 7. `integrated_time` within cert validity | yes | explicit `not_before`/`not_after` compare |

**Adopting `Verifier` wholesale therefore gives a *weaker* transparency guarantee than
ocx's current hand-rolled SET check**, unless ocx also calls the two public routines in §5
and §6. The bundle *profile* check does require a proof and checkpoint to be **present** —
it just never verifies them.

Public API:

```rust
pub fn new<R: TrustRoot>(rekor_config: RekorConfiguration, trust_repo: R) -> SigstoreResult<Self>
pub async fn verify_digest<P: VerificationPolicy>(&self, input_digest: Sha256, bundle: Bundle,
                                                  policy: &P, offline: bool) -> VerificationResult
pub async fn verify<R: AsyncRead + Unpin + Send, P: VerificationPolicy>(&self, input: R,
                                                  bundle: Bundle, policy: &P, offline: bool) -> VerificationResult
```

`bundle::verify::blocking::Verifier` also exists. `bundle::verify::policy` is public — the
right home for `[[trust.policy]]` (#98).

## 5. Rekor SET — real algorithm, private door, public workaround

`cosign/bundle.rs:80-105` implements the true wire format:

```rust
pub(crate) fn verify_bundle(bundle: &Bundle, rekor_pub_keys: &BTreeMap<String, CosignVerificationKey>) -> Result<()> {
    let buf = serde_json_canonicalizer::to_vec(&bundle.payload)?;  // {body, integratedTime, logIndex, logID}
    let rekor_pub_key = rekor_pub_keys.get(&bundle.payload.log_id)...;
    rekor_pub_key.verify_signature(Signature::Base64Encoded(bundle.signed_entry_timestamp.as_bytes()), &buf)?;
    Ok(())
}
```

`verify_bundle` and `Bundle::new_verified` are `pub(crate)`. The only public door,
`SignedArtifactBundle::new_verified(raw: &str, rekor_pub_keys: &BTreeMap<..>)`
(`cosign/bundle.rs:48`), wants the whole **cosign-v1** artifact-bundle JSON
(`{base64Signature, cert, rekorBundle}`) — not a Sigstore bundle v0.3 — and sits behind the
`cosign` feature, which drags `oci-client`, `regex` and `registry`. A second OCI client in a
workspace that patches `oci-client` to a fork is not acceptable.

**Recommended, and not hand-rolling** — both halves are already public (probe-compiled):

```rust
use sigstore::crypto::{CosignVerificationKey, Signature};
let buf = serde_json_canonicalizer::to_vec(&payload)?;   // {body, integratedTime, logIndex, logID}
rekor_key.verify_signature(Signature::Base64Encoded(set_b64.as_bytes()), &buf)?;
```

Signature verification is sigstore's; canonicalization is `serde_json_canonicalizer`'s (an
RFC 8785 implementation, already a transitive dep and the crate of record); the payload is a
four-field serde struct. **This replaces the custom `ocx-rekor-set-v1` payload with the real
wire format** without adopting the `cosign` feature.

## 6. Merkle — arithmetic private, entry point public

`crypto/merkle` is `pub(crate)`; `MerkleProofVerifier` and `Rfc6269HasherTrait` are
`pub(crate) trait`. But the Rekor-model entry points are public —
`rekor/models/log_entry.rs:144`:

```rust
pub fn verify_inclusion(&self, rekor_key: &CosignVerificationKey) -> Result<(), SigstoreError>
```

delegating to `rekor/models/inclusion_proof.rs:61`:

```rust
pub fn verify(&self, entry: &[u8], rekor_key: &CosignVerificationKey) -> Result<(), SigstoreError> {
    let checkpoint = self.checkpoint.as_ref().ok_or(...)?;                   // checkpoint REQUIRED
    checkpoint.verify_signature(rekor_key)?;                                 // STH signature
    checkpoint.is_valid_for_proof(&self.root_hash.into(), self.tree_size)?;  // STH binds to proof
    let entry_hash = Rfc6269Default::hash_leaf(entry);
    Rfc6269Default::verify_inclusion(self.log_index as u64, &entry_hash,
                                     self.tree_size, &proof_hashes, &self.root_hash.into())
}
```

Full RFC 6962: signed-note checkpoint signature, checkpoint↔proof binding, leaf hash,
inclusion path. `SignedCheckpoint::verify_signature` is separately public
(`rekor/models/checkpoint.rs:109`).

The split is deliberate —
[sigstore-rs#283](https://github.com/sigstore/sigstore-rs/issues/283) planned *"a basic
implementation in the `crypto` module that is **not** part of the public API"* plus
*"methods on the related Rekor data structures — this would be part of the public API"*.

**#209's Merkle half is wiring, not implementation.** One caveat: `verify_inclusion` takes a
`rekor::models::LogEntry` (REST shape), not the bundle's protobuf `TransparencyLogEntry`.
Upstream ships only the forward conversion (`impl TryFrom<RekorLogEntry> for
TransparencyLogEntry`, `bundle/models.rs:72`); the reverse adapter is unwritten work of
unmeasured size.

## 7. Bundle v0.3 write, Fulcio, Rekor, trust roots, OIDC

**Bundle v0.3 (PARTIAL).** `bundle/models.rs:17-41` defines
`Version::Bundle0_3 => "application/vnd.dev.sigstore.bundle.v0.3+json"`, but
`bundle/mod.rs:22` declares `mod models;` — private. Probe: `E0603`. And
`SigningArtifact::to_bundle()` hardcodes `media_type: Version::Bundle0_2.to_string()` with a
leaf-only chain. ocx must set the media type itself. `sigstore::bundle::Bundle` is a
re-export of the `sigstore_protobuf_specs` struct with a public `media_type: String`, so this
is one string literal over a library-owned serde type — not a hand-rolled format. Keep one
ocx constant citing `bundle/models.rs:29`, with a test asserting sigstore's own reader
accepts it. **It does** — `bundle/verify/models.rs:363-373` accepts v0.3 and handles its
single-`Content::Certificate` form.

**Fulcio (YES).** `fulcio/mod.rs`: `FulcioClient::new(root_url, token_provider)`,
`request_cert_v2(CertReq, &IdentityToken) -> CertificateResponse { cert, chain, detached_sct }`
— full chain with intermediates, both SCT response shapes, errors if `certs.len() < 2`.
`TokenProvider::Static` takes a raw token — the CI-friendly door. Use `request_cert_v2`;
`request_cert` is the older v1 flow and takes `self` by value.

> **Constraint for the ADR:** `request_cert_v2` builds `reqwest::Client::new()` internally —
> no timeouts, no injectable DNS resolver. That is a PKG-13/PKG-14/SEC-16/SEC-18 violation
> ocx **cannot fix from outside**; the SSRF-guarded resolver cannot reach Fulcio traffic.
> Upstream: [sigstore-rs#176](https://github.com/sigstore/sigstore-rs/issues/176) (open).
> **This bears directly on the mission's dial-site SSRF item — delegating Fulcio *removes*
> ocx's ability to guard that dial.**

**Rekor (YES), with the opposite property.** `create_log_entry`, `get_log_entry_by_index`,
`get_log_entry_by_uuid`, `search_log_query`, `get_log_info`, `get_log_proof`,
`get_public_key`; `hashedrekord` 0.0.1 present. `Configuration`
(`rekor/apis/configuration.rs`) has **every field public including
`pub client: reqwest::Client`** — ocx can inject a fully hardened client. **Rekor traffic can
satisfy the house rules; Fulcio traffic cannot.** Exploit the asymmetry rather than papering
over it.

**Trust roots (YES).** `trust/mod.rs` — `pub trait TrustRoot { fulcio_certs, rekor_keys,
ctfe_keys }` and `pub struct ManualTrustRoot` with all three fields `pub`.
`Verifier::new(cfg, ManualTrustRoot{..})` compiles on the current pin — **verify against a
self-hosted stack works today**, and offline verification (#196) is a `ManualTrustRoot` built
from a cached file.

`trust/sigstore/constants.rs` **embeds the production root**:

```rust
pub(crate) const SIGSTORE_METADATA_BASE: &str = "https://tuf-repo-cdn.sigstore.dev";
impl_static_resource! { "root.json", "trusted_root.json", }
// => include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/trust_root/prod/", $name))
```

**This alone retires the `TrustRoot::load_embedded` stub (#210).** The constants are
`pub(crate)`, so `SigstoreTrustRoot::new()` cannot be redirected at a self-hosted TUF repo;
the public escape hatches are `from_trusted_root_json_unchecked(&[u8])` (a `TrustedRoot`
JSON) and `from_client_trust_config(&PathBuf)` (a `ClientTrustConfig` JSON), plus
`impl TryFrom<ClientTrustConfig>`. `new()` uses `tough` with
`ExpirationEnforcement::Safe` (**expiry enforced**) and resolves targets **disk cache →
embedded → remote**, sha256-checked against TUF targets metadata with write-back —
self-healing. Note `new()` **still needs network**: `cache_dir` caches targets, not metadata.
`_unchecked` performs no validation by its own doc — treat as operator-supplied trusted
input. Temporal handling is deliberate: `fulcio_certs()` passes `allow_expired = true`
(*"they may have been active when the certificate was used to sign"*) while tlog keys must be
currently valid.

**OIDC (PARTIAL).** `IdentityToken` is public with `TryFrom<&str>` (raw JWT),
`From<CoreIdToken>`, `unverified_claims()`, `in_validity_period()`. Interactive PKCE is
complete (`oauth/openidflow.rs`, `fulcio::oauth::OauthTokenProvider`,
`DEFAULT_REDIRECT_PORT = 8080`; it sets `redirect::Policy::none()` with the comment
*"Following redirects opens the client up to SSRF vulnerabilities"*).

**Ambient/CI detection: none.**

```
grep -rniE 'ACTIONS_ID_TOKEN|CI_JOB_JWT|ambient|detect_credential' src/   →  no hits
```

The only `GITHUB_ACTIONS` hits are a hardcoded issuer URL in `cosign/mod.rs` used for
verification constraints, not acquisition. No equivalent of sigstore-python's `id` or
sigstore-go's ambient detection. **#194's CI story is ocx-owned:** read
`ACTIONS_ID_TOKEN_REQUEST_URL` + `ACTIONS_ID_TOKEN_REQUEST_TOKEN` (or GitLab `id_tokens`),
exchange over HTTPS, then `IdentityToken::try_from(jwt)`. ~30 lines of ordinary HTTP against
a documented endpoint — no crypto, no wire format, and no maintained Rust crate exists to
delegate it to.

**Rekor v2: NO.**
`grep -rniE 'rekor.*v2|api/v2/log|tile|dev\.sigstore\.rekor\.v2' src/rekor/` → empty.
[sigstore-rs#513](https://github.com/sigstore/sigstore-rs/issues/513) *"Add Merkle tree and
Note format foundation for Rekor v2"* is **closed** — that foundation is the `crypto::merkle`
+ checkpoint code in 0.14 — but no v2 **client** shipped. #107 stays open, gated upstream.

## 8. Feature flags, TLS, MSRV, releases

| Capability | Required features |
|---|---|
| Bundle type, read, verify, policy | `bundle` (= `sign` + `verify`) |
| Fulcio client, `IdentityToken`, OAuth | `bundle` (drags `fulcio` → `oauth`) |
| Rekor client, `verify_inclusion`, checkpoint | `bundle` (drags `rekor`) |
| `ManualTrustRoot`, `TrustRoot` trait | **none** — `pub mod trust;` is ungated |
| `SigstoreTrustRoot`, TUF, embedded root, `production()` | **`sigstore-trust-root`** |
| cosign-v1 SET door | `cosign` — **not recommended**, see §5 |

**The current pin `["bundle","rustls-tls"]` already reaches** the Fulcio client, Rekor
client, `Verifier`, `LogEntry::verify_inclusion` and `ManualTrustRoot` (probe, exit 0). The
one missing flag is **`sigstore-trust-root`** — which is exactly why #210 has a stub.

**TLS posture is clean.** `cargo tree -e normal -i` over
`["bundle","sigstore-trust-root","rustls-tls"]`:

| Crate | Present? |
|---|---|
| `openssl`, `openssl-sys`, `native-tls` | **ABSENT** |
| `ring` | **ABSENT** (`nothing to print`) |
| `aws-lc-rs` | `v1.18.0` |
| `rustls` | `v0.23.43` |

Exactly one crypto provider, no banned crate — satisfies EVO-10 and SEC-14. `native-tls`
**is** in sigstore's `default` set, so `default-features = false` is load-bearing and must
not be "simplified" away. `aws-lc-rs` is an unconditional sigstore dependency on both
`cfg(target_arch)` arms, so it enters regardless of the TLS feature — consistent with the
pinned decision in `current-apis.md`.

`oauth` is **not optional**: `bundle → fulcio → oauth → openidconnect`, which pulls `rsa`
(RUSTSEC-2023-0071, already covered by a documented `deny.toml` ignore — ocx signs ECDSA
P-256 only and holds no RSA private key; re-confirm the ignore carries a machine-checkable
removal condition per DEP-08). `fulcio` also pulls `webbrowser` unconditionally — an
interactive-browser dependency inside a backend CLI, not disableable without giving up
`bundle`.

**MSRV: undeclared.** No `rust-version` in `Cargo.toml` or `Cargo.toml.orig`;
`edition = "2024"` sets the effective floor at **Rust ≥ 1.85**. No upstream MSRV promise —
keep the pinned channel authoritative (EVO-15).

**No newer release.** crates.io: `sigstore` `max_version = 0.14.0`,
`updated_at = 2026-05-22`. GitHub: v0.14.0 (2026-05-22) latest, v0.13.0 was 2025-10-16, and
`main`'s `Cargo.toml` still reads `version = "0.14.0"`. `sigstore_protobuf_specs`
`max_version = 0.5.1` (2026-04-06) — the workspace pin is current.

**Alternative crates for the NO/PARTIAL rows** (crates.io liveness checked): `x509-cert`
0.3.0 (2026-07-09) — already in-graph, has an `sct` feature sigstore itself enables;
`tls_codec` 0.5.0 (2026-07-13) — already in-graph, for `DigitallySigned`; `rustls-webpki`
stable 0.103.14 — what sigstore uses for path building (note crates.io `max_version` reads
`0.104.0-alpha.7`, a prerelease, do not pin); `tough` 0.24.0 (2026-07-10) — sigstore pins
0.22, ocx should not depend on it directly. **Avoid** the `sct` crate (last release
2023-10-24 — DEP-02 disqualifies it regardless of downloads) and `ct-codecs` (a base64/hex
codec, not Certificate Transparency, despite the name).

## 9. Corrections

**To `research_sigstore_rs_api_surface.md`** (concurrent sibling document). Its headline
*"Milestone 5 is a deletion, not an implementation"* holds for verify and is **wrong for
sign** (§3). Three of its verdict-table rows cite `crypto` modules that are `pub(crate)`:

| Its row | Correction |
|---|---|
| #208 SCT → "delete ocx code", citing `crypto/transparency.rs` | Module is `pub(crate)` (`crypto/mod.rs:150`). Deletion holds **only via `Verifier`**; no standalone primitive exists. |
| #209 SET+Merkle → "delete ocx code", citing `crypto/merkle/proof_verification.rs` | Module is `pub(crate)` (`crypto/mod.rs:22`). The reachable route is `LogEntry::verify_inclusion` (§6), and **`Verifier` performs neither check** (§4). Verdict is PARTIAL. |
| #207 chain walk → "delete ocx code", citing `crypto/certificate_pool.rs` | Module is `pub(crate)` (`crypto/mod.rs:133`). Deletion holds **only via `Verifier`**. |

Its ocx-side findings are correct and not duplicated here — in particular that ocx emits
`inclusion_proof: None` (`oci/sign/bundle.rs:66`) and never reads it back, so ocx currently
produces bundles sigstore-rs's own verifier and cosign v3 would **reject** for a v0.3 media
type. That is a correctness finding and the concrete mechanism behind #197.

**To `research_sigstore_rs_spike.md` (2026-07-09):**

1. *"Keyless sign (Fulcio) — YES, `SigningContext::{new,production}`"* is **false** (§3).
   Its conclusion (ocx hand-rolls the sign flow) is right, for a stronger reason than it
   gives: the blocker is upstream visibility, so replacing the Python fake will not fix it.
2. *"Bundle v0.3 write — YES"* overstates; `bundle::models` is private (§7). PARTIAL.

Everything else in the spike holds.

## 10. Interop risk for #197

[sigstore-rs#608](https://github.com/sigstore/sigstore-rs/issues/608) (open, 2026-07-29,
against 0.14.0 and `main`): **DSSE bundle verification rejects valid `cosign attest-blob
--new-bundle-format` v0.3 bundles.** `tlog_entry_for_dsse` recomputes
`sha256(serde_json::to_vec(&dsse))`, but Rekor hashed cosign's Go `encoding/json` bytes —
field order `payloadType, payload, signatures` with `keyid` always present, versus
sigstore-rs's prost struct omitting `keyid`. Different bytes, different hash, rejected. The
issue carries a logged fixture (`logIndex 2086326142`) and notes sigstore-python does not
perform this check at all.

Message-signature bundles are unaffected. **If #197's interop suite covers `cosign
attest-blob`, expect a red that is an upstream bug, not an ocx defect.**

## 11. UNCONFIRMED

- **`cosign trusted-root create` output shape.** Confirmed from source which JSON type each
  constructor takes (`trust/sigstore/mod.rs:138-171` and the `TryFrom<ClientTrustConfig>`
  impl); **not** byte-compared against actual `cosign` output. Verify empirically before
  relying on it for #210.
- **Concrete `bundle::verify::policy` variants.** The module and `VerificationPolicy` trait
  are confirmed public (probe-compiled); the variant list was not enumerated against
  `[[trust.policy]]`'s needs. Read `bundle/verify/policy.rs`.
- **`TransparencyLogEntry` → `rekor::models::LogEntry` adapter size** (§6). Only the forward
  conversion exists upstream.
- **Method gap:** Context7 MCP and docs.rs WebFetch were **not** used. Compiler probes
  against the vendored source were used instead — strictly stronger for the visibility
  question, which is what decides this ADR, but stated here rather than left implicit.

## Sources

- Vendored source: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sigstore-0.14.0/src/**`
- Compiler probes: `~/.cache/ocx-claude/m25/probe`, `~/.cache/ocx-claude/m25/probe2`
- [crates.io API](https://crates.io/api/v1/crates/sigstore) — versions/dates for sigstore, sigstore_protobuf_specs, tough, x509-cert, x509-verify, rustls-webpki, der, tls_codec, sct, ct-codecs
- [sigstore-rs#562](https://github.com/sigstore/sigstore-rs/issues/562) — `Keyring` not re-exported, no `staging()`
- [sigstore-rs#283](https://github.com/sigstore/sigstore-rs/issues/283) — deliberate private-crypto / public-Rekor-methods split
- [sigstore-rs#608](https://github.com/sigstore/sigstore-rs/issues/608) — DSSE v0.3 `envelopeHash` mismatch
- [sigstore-rs#176](https://github.com/sigstore/sigstore-rs/issues/176) — Fulcio/Rekor clients not independent of reqwest
- [sigstore-rs#513](https://github.com/sigstore/sigstore-rs/issues/513) (closed) — Merkle/Note foundation, no v2 client
- [sigstore-rs releases](https://github.com/sigstore/sigstore-rs/releases) — v0.14.0 (2026-05-22) latest
