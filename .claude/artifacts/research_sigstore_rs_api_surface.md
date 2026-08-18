# Research: sigstore-rs 0.14 API surface — what ocx can delete

> Ground truth is the vendored crate at
> `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sigstore-0.14.0/src`, read directly.
> docs.rs rendering and training-data recall are both explicitly distrusted here. Every
> `file:line` below was produced by a line-exact scan of that tree on 2026-08-18 and is
> relative to that `src` directory.
>
> Companion artifacts: `research_sigstore_rs_spike.md` (the 2026-07-09 in-workspace probe,
> still accurate), `research_sigstore_current_architecture.md` (what ocx hand-rolls today),
> `research_sigstore_selfhost_stack.md` §8 (the stack decision that unblocks all of this).

## Headline

**Milestone 5 is mostly a deletion.** #207 and #208 are wholly implemented inside one
sigstore-rs function and become pure deletions. #209 is the exception and needs care: the
crate ships every primitive but does **not** call two of them from its own top-level
verifier. The reason ocx hand-rolls today is that the Python fake stack cannot satisfy the
crate's mandatory SCT check — remove the fake, and most of the hand-rolled code loses its
justification.

> **Correction, 2026-08-18.** An earlier revision of this file claimed #209 was a pure
> deletion because `crypto/merkle/` and the checkpoint requirement in
> `bundle/verify/models.rs` exist. Reading `bundle/verify/verifier.rs` end to end refutes
> that: lines 196–202 are literal `TODO(tnytown) ... sigstore-rs#285` comments where the
> Merkle-inclusion and SET checks should be. The primitives are real and usable; the
> top-level `Verifier::verify_digest` simply does not invoke them. Calling
> `Verifier::verify()` and deleting ocx's code would therefore **silently drop both checks**
> — the exact failure mode #209 exists to fix. Credit: the verification-semantics research
> worker flagged this against the pinned tag before I had opened the file.

| Issue | Asks for | sigstore-rs 0.14 | Verdict |
|---|---|---|---|
| #207 | Fulcio chain walk + temporal validity | `crypto/certificate_pool.rs` — webpki `EndEntityCert`/`TrustAnchor`, code-signing EKU, `verify_cert_with_time` | **delete ocx code** |
| #208 | SCT / CT-log verification | `crypto/transparency.rs` (398 lines) — embedded + detached SCT, precertificate handling | **delete ocx code** |
| #209 Merkle half | Inclusion proof + checkpoint | `LogEntry::verify_inclusion` (`rekor/models/log_entry.rs:144`), `InclusionProof::verify`, `checkpoint.rs` (524 lines, Signed Note) | **wire one call** — see §2 |
| #209 SET half | Rekor Signed Entry Timestamp | **no primitive exists** — `signed_entry_timestamp` is a bare `String` field | **compose**, see §2 |
| #210 | TUF-distributed trust root | `trust/sigstore/` (feature `sigstore-trust-root`, tough 0.22) | **wire, don't build** |
| #206 | Real X.509 parsing | already done on this branch via `x509-cert` | **close as done** |
| #107 | Rekor v2 | absent from 0.14 | **stays open**, gated upstream |

## 1. Most of the verification path is one function

`bundle/verify/verifier.rs` covers most of ocx's `verify/pipeline.rs` crypto, done properly —
with two documented gaps at steps 5 and 6, enumerated in the coverage table below:

| Line | What it does | Replaces in ocx |
|---|---|---|
| 105 | `CertificatePool::from_certificates(trust_repo.fulcio_certs()?, [])` — pool built straight from the `TrustRoot` trait | `TrustRoot::der_certs()` loop |
| 160 | `.verify_cert_with_time(&ee_cert, UnixTime::since_unix_epoch(issued_at))` — full chain walk **at the Rekor integrated time** | `verify_cert_chain` (single-hop, `verify/pipeline.rs:441-468`) **and** the hardcoded `cert_expired_but_tlog_valid: false` at `:330` |
| 168 | `verify_sct(&sct_context, &self.ctfe_keyring)` | nothing — ocx has zero SCT handling in 1583 lines |

Line 160 is the one that matters most: it resolves #207's chain walk **and** its temporal
validity in a single call, and it uses the transparency-log timestamp rather than wall-clock
`now()`, which is the semantics a ~10-minute Fulcio certificate requires.

`crypto/certificate_pool.rs:17` imports `pki_types::{CertificateDer, TrustAnchor, UnixTime}`
and `webpki::{EndEntityCert, KeyUsage, VerifiedPath}`, with `ID_KP_CODE_SIGNING` from
`const_oid::db::rfc5280` — so path building, EKU enforcement and expiry are webpki's, not
hand-written. `verify_cert_with_time` takes `verification_time: Option<UnixTime>`
(`:62`, `:85`, `:102`), falling back to now only when unset.

### What `verify_digest` actually runs, in order

Transcribed from `bundle/verify/verifier.rs:127-223`. This list is the ADR's coverage
statement — state it, do not imply it.

| Step | Lines | Check | Covers |
|---|---|---|---|
| 1 | 158-161 | `cert_pool.verify_cert_with_time(&ee_cert, UnixTime::since_unix_epoch(issued_at))` — webpki chain walk to a trusted root | #207 chain |
| — | 165-168 | `CertificateEmbeddedSCT::new_with_verified_path(...)` then `verify_sct(&sct_context, &self.ctfe_keyring)` | #208 |
| 2 | 172 | `policy.verify(&materials.certificate)` — identity/issuer policy | overlaps #98 |
| 3 | 176-185 | `verify_bundle_content(...)` — signature over the input digest by the leaf's SPKI | signature |
| 4 | 191-193 | `materials.tlog_entry(offline, &input_digest)` — Rekor entry consistent with the other materials (**CVE-2022-36056** mitigation) | body binding |
| 5 | **196-198** | **`TODO(tnytown): Merkle inclusion; sigstore-rs#285` — not performed** | #209 gap |
| 6 | **200-202** | **`TODO(tnytown) SET verification; sigstore-rs#285` — not performed** | #209 gap |
| 7 | 204-219 | `integrated_time < not_before \|\| integrated_time > not_after` → `CertificateErrorKind::Expired` | #207 temporal |

Step 7 is precisely what ocx hardcodes as `cert_expired_but_tlog_valid: false`
(`verify/pipeline.rs:330`), and it compares against the Rekor **integrated time**, not
wall-clock now — the semantics a ~10-minute Fulcio certificate requires.

Steps 5 and 6 are the whole of #209, and they are the reason the ADR must specify an
orchestration layer rather than a bare `Verifier::verify()` call:

- **Merkle half — wire, do not build.** `rekor/models/log_entry.rs:144` exposes
  `pub fn verify_inclusion(&self, rekor_key: &CosignVerificationKey)`, which canonicalizes
  the entry body with `serde_json_canonicalizer::to_vec(&self.body)` (`:152`) and delegates
  to `InclusionProof::verify`. That in turn enforces a checkpoint is present, calls
  `checkpoint.verify_signature(rekor_key)`, checks `checkpoint.is_valid_for_proof(root_hash,
  tree_size)`, and runs `Rfc6269Default::hash_leaf` + `verify_inclusion`. `checkpoint.rs` is
  a complete 524-line Signed Note implementation. One call site, no arithmetic of ours.
- **SET half — compose, do not hand-roll.** No SET verification primitive exists anywhere in
  the crate; `signed_entry_timestamp` is a bare `String` field (`log_entry.rs:109`). ocx must
  canonicalize the four SET-bound fields and verify with `CosignVerificationKey`. Both halves
  are existing library calls — `serde_json_canonicalizer` is *already in the dependency graph
  via sigstore itself*, so this adds no dependency. Canonicalisation is a spec, and we use the
  crate that implements it; this is orchestration over primitives, not owning non-domain code.
  Worth filing upstream as "wire steps 5 and 6 into `Verifier::verify()`", non-blocking.

## 2. Inclusion proof and checkpoint are mandatory for bundle v0.3

`bundle/verify/models.rs:290-340` reads both `inclusion_promise` and `inclusion_proof` off
the tlog entry and branches by bundle version. Verbatim from the source:

- `:293` — "`inclusion_proof` is a required field in the current protobuf spec"
- `:310` — "0.1 bundle contains inclusion proof without checkpoint"
- `:328` — `error!("bundle must contain checkpoint")`
- `:337` — "Bundle v0.3 requires a full inclusion proof with checkpoint, same as v0.2."
- `:340` — "MUST include a full inclusion proof with checkpoint."

`:391` and `:410` gate on `!offline && self.tlog_entry.inclusion_proof.is_none()` — i.e. when
online and the proof is missing, the crate fetches it; offline, it works from what the bundle
carries. That is exactly the offline/air-gapped behaviour #196 needs, already implemented.

Against this, ocx today populates `inclusion_proof: None` on the sign side
(`oci/sign/bundle.rs:66`) and never reads it on the verify side — the field appears in
production code nowhere, only in a `#[cfg(test)]` fixture. So ocx currently emits bundles
that sigstore-rs's own verifier — and therefore cosign v3 — would **reject** for a v0.3
media type. This is a correctness finding, not only a fidelity gap, and it is the concrete
mechanism behind #197 (cosign interop).

The Merkle arithmetic itself is `crypto/merkle/proof_verification.rs` (905 lines):
`verify_inclusion`, `root_from_inclusion_proof`, `verify_consistency`, over an RFC 6269
hasher in `crypto/merkle/rfc6962.rs`. Note the feature gate at the top of that file —
`sign`, `sigstore-trust-root`, `rekor`, `verify` — none of which the workspace currently
enables (`default-features = false, features = ["bundle", "rustls-tls"]`), which is why none
of this is reachable from ocx today.

## 3. SCT is why the fake stack forced the hand-rolling

`crypto/transparency.rs` imports `CT_PRECERT_SCTS` and `CT_PRECERT_SIGNING_CERT` from
`const_oid::db::rfc6962`, uses `tls_codec` for the RFC 6962 wire encoding, and carries
`cert_is_preissuer` and `find_issuer_cert` helpers — i.e. it handles the hard case, the
precertificate SCT whose signed payload must be reconstructed with the poison extension
removed. `verify_sct` is the entry point at `:296`, with round-trip tests at `:380` and
`:396`.

On the signing side `bundle/sign.rs:140` and `:143` call it unconditionally:

```rust
use crate::crypto::transparency::{CertificateEmbeddedSCT, verify_sct};
// ...
verify_sct(detached_sct, &self.context.ctfe_keyring)?;   // :140
verify_sct(&sct, &self.context.ctfe_keyring)?;           // :143
```

Both spellings are covered — a detached SCT from the Fulcio response header, and one embedded
as an X.509 extension in the issued certificate. There is no flag to skip either. That is the
single reason `oci/sign/fulcio.rs` exists: the Python fake mints no SCT, so this call can only
fail, so ocx bypassed the crate entirely. **With a real Fulcio + CT log the call succeeds and
the bypass has no justification left.**

## 4. `ManualTrustRoot` is the seam, and it is trivially small

`trust/mod.rs` is 60 lines total. The whole contract:

```rust
pub trait TrustRoot {
    fn fulcio_certs(&self) -> crate::errors::Result<Vec<CertificateDer<'_>>>;
    fn rekor_keys(&self)   -> crate::errors::Result<BTreeMap<String, &[u8]>>;
    fn ctfe_keys(&self)    -> crate::errors::Result<BTreeMap<String, &[u8]>>;
}

pub struct ManualTrustRoot<'a> {
    pub fulcio_certs: Vec<CertificateDer<'a>>,
    pub rekor_keys:   BTreeMap<String, Vec<u8>>,
    pub ctfe_keys:    BTreeMap<String, Vec<u8>>,
}
```

All three fields are `pub`, so populating it from a `cosign trusted-root create`
`trusted_root.json` is field mapping over the `sigstore_protobuf_specs` `TrustedRoot` type
ocx already depends on — no crypto, no parsing beyond what serde does. This is what lets one
code path serve the public-good instance, a self-hosted stack, and the acceptance suite,
differing only in which trust root is loaded. It is also what preserves #196's offline story
unchanged: an offline verify is a `ManualTrustRoot` built from the cached file.

`trust/sigstore/` (`mod.rs`, `constants.rs`, `transport.rs`) is the TUF-backed
`SigstoreTrustRoot` for #210, behind feature `sigstore-trust-root`, using tough 0.22.
**Open**: whether it can be pointed at a self-hosted TUF repo or hardcodes the production CDN
— `constants.rs` is the file to read. Assigned to the `res-sigstore-rs` worker; do not
assume either answer.

## 5. What stays hand-written, and why that is acceptable

- **Bundle v0.3 assembly.** `SigningArtifact::to_bundle()` emits v0.2 (per the July spike), so
  ocx keeps constructing the `Bundle` struct and setting the v0.3 media type itself. This is
  struct assembly over `sigstore_protobuf_specs` types — no cryptography, no wire-format
  parsing — and is therefore outside the "don't own non-domain code" ban. It must be stated
  as such in the ADR rather than left to a reviewer to re-litigate.
- **OIDC token acquisition** for the acceptance suite, if the crate exposes no non-interactive
  path. Injecting an out-of-band token is configuration, not crypto.
- **The OCI referrers transport.** ocx has its own; the crate's `registry`/`cached-client`
  features stay off, which is also what keeps `oci-client` from colliding.

## 6. Feature flags

Workspace today: `default-features = false, features = ["bundle", "rustls-tls"]` — which
reaches the protobuf structs and nothing else. The July spike built cleanly with
`["sign", "verify", "bundle", "fulcio", "rekor", "sigstore-trust-root", "rustls-tls"]`, and
that is the set the deletion needs. `native-tls` is a **default** feature, so
`default-features = false` is load-bearing and must not be "simplified" away.

Known cost, already paid once: `sigstore -> openidconnect -> rsa 0.9.10` trips
RUSTSEC-2023-0071 (Marvin timing sidechannel), resolved by a documented `deny.toml` ignore —
ocx signs with ECDSA P-256 only and holds no RSA private key. Re-confirm the ignore still
carries a machine-checkable removal condition per DEP-08.

## 7. Rekor v2 (#107)

No v2 client surface in 0.14 — only `rekor::apis::entries_api::create_log_entry` with
`hashedrekord:0.0.1`. #107 stays open and stays gated on an upstream release. This is the
reason the stack decision landed on classic Rekor v1 rather than the lighter `rekor-tiles`
(`research_sigstore_selfhost_stack.md` §8b).

## Open items

1. Can `SigstoreTrustRoot` target a self-hosted TUF repo, or is the production CDN hardcoded?
   Read `trust/sigstore/constants.rs` and `transport.rs`.
2. Does 0.14 expose any non-interactive OIDC token path, or must ocx inject a token minted
   out-of-band? Read `fulcio/oauth.rs` and `oauth/`.
3. Does a sigstore-rs release newer than 0.14 change any of the above — in particular, has
   Rekor v2 landed?
4. Exactly which verification steps `bundle/verify/verifier.rs` performs end to end for a
   `MessageSignature` bundle, in order, so the ADR can state coverage rather than imply it.

## Answers to open items

All four answered below from the vendored source. **One claim in §1 above needs correcting
and one in §3 needs narrowing — see "Corrections" at the end of this section.** Method note:
these answers were cross-checked by compiling probe crates against the real crate
(`~/.cache/ocx-claude/m25/probe` = `["bundle","rustls-tls"]`, `probe2` = `+
sigstore-trust-root`), because docs.rs renders `--all-features` and does not show `pub(crate)`
— which is the axis half of this section turns on. Fuller capability matrix and the
per-module visibility evidence:
[`research_sigstore_rs_visibility_and_capabilities.md`](research_sigstore_rs_visibility_and_capabilities.md).

### Item 1 — `SigstoreTrustRoot` retargetability (#210)

**The production CDN is hardcoded in `new()`. It cannot be pointed at a self-hosted TUF
repository.** Retargeting means abandoning TUF, not reconfiguring it.

`trust/sigstore/constants.rs`:

```rust
pub(crate) const SIGSTORE_METADATA_BASE: &str = "https://tuf-repo-cdn.sigstore.dev";
pub(crate) const SIGSTORE_TARGET_BASE:   &str = "https://tuf-repo-cdn.sigstore.dev/targets";
impl_static_resource! { "root.json", "trusted_root.json", }
// => include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/trust_root/prod/", $name))
```

Both constants are `pub(crate)`. The three public constructors (`trust/sigstore/mod.rs`):

```rust
pub async fn new(cache_dir: Option<&Path>) -> Result<Self>;             // :69
pub fn from_trusted_root_json_unchecked(data: &[u8]) -> Result<Self>;   // :99
pub fn from_client_trust_config(pki_file: &PathBuf) -> Result<Self>;    // :118
impl TryFrom<ClientTrustConfig> for SigstoreTrustRoot
```

**`new()` takes only a cache directory — there is no URL, mirror or root.json parameter.**
`:77-83` builds the loader against the baked constants:

```rust
let client = reqwest::Client::new();                       // :74
let repository = tough::RepositoryLoader::new(...)         // :77
    .expiration_enforcement(tough::ExpirationEnforcement::Safe)   // :83
```

So, plainly:

| Property | Answer |
|---|---|
| Self-hosted TUF repo | **No** — CDN + embedded `root.json` are compile-time constants |
| Expiry enforced | **Yes** — `ExpirationEnforcement::Safe` (`:83`) |
| Signature verification / refresh | **Yes** — delegated to `tough 0.22`; test `trust_root_outdated` (`:377`) writes a stale `trusted_root.json` into the cache and asserts it is refreshed |
| Stale cached root + no network | **Hard failure.** `new()` unconditionally runs `RepositoryLoader::load().await`, which fetches *metadata*. `cache_dir` caches **targets**, not metadata — there is no offline path through `new()` |
| Client injectable | **No** — `reqwest::Client::new()` at `:74`, same limitation as Fulcio ([#176](https://github.com/sigstore/sigstore-rs/issues/176)) |

`fetch_target` (`:191-210`) resolves **disk cache → embedded resource → remote**, then
sha256-compares against the TUF targets metadata and re-fetches + writes back on mismatch —
self-healing and hash-checked, but only *after* the metadata load has already succeeded over
the network.

`trust/sigstore/transport.rs` does implement a `file://` scheme branch (`ReqwestTransport`,
tests `file_found_on_disk` `:145`, `unsupported_scheme` `:123`), but the type is
`pub(crate)` and `new()` accepts no transport, so it is not an escape hatch.

**Recommendation for #210, in two parts, because they are different guarantees:**

1. **Public-good verification** — use `SigstoreTrustRoot::new(Some(cache_dir))`. Real TUF:
   signature verification, rotation, expiry. Retires the `TrustRoot::load_embedded` stub
   outright, since `root.json` and `trusted_root.json` ship inside the crate binary.
2. **Self-hosted / air-gapped** — use `from_trusted_root_json_unchecked` or
   `from_client_trust_config`. **State plainly in the ADR that these bypass TUF entirely**:
   no signature verification, no refresh, no expiry enforcement. The name is not decoration
   — its own doc says *"The caller must ensure that the data is trustworthy."* The file
   becomes operator-supplied trusted input, integrity-checked by ocx's own means.

There is no third option in 0.14. A self-hosted TUF repo would need an upstream change.

### Item 2 — Non-interactive OIDC

**Yes, a pre-minted token can be injected, at two points. There is no ambient/CI detection
and no device flow.**

The injection point the acceptance suite needs — `oauth/token.rs`, re-exported at
`oauth/mod.rs:20`:

```rust
pub struct IdentityToken;
impl TryFrom<&str> for IdentityToken;      // <-- raw JWT string, no browser
impl From<CoreIdToken> for IdentityToken;
impl IdentityToken {
    pub fn unverified_claims(&self) -> &UnverifiedClaims;
    pub fn in_validity_period(&self) -> bool;
}
```

`IdentityToken` is exactly what `FulcioClient::request_cert_v2(request, identity)` takes, so
`IdentityToken::try_from(jwt_str)` is a complete non-interactive path — drive dex's
`mockCallback`, take the JWT, convert, sign. No browser, no listener, no port binding.

The second point, for the `TokenProvider` seam (`fulcio/mod.rs`):

```rust
pub enum TokenProvider {
    Static((CoreIdToken, String)),   // <-- pre-minted token + challenge claim
    Oauth(OauthTokenProvider),
}
```

`TokenProvider::Static` takes an already-obtained `CoreIdToken` and the claim string. Use
this when constructing `FulcioClient` directly.

**What exists only interactively:** `oauth/openidflow.rs` —
`OpenIDAuthorize::new(client_id, client_secret, issuer, redirect_url)`, `auth_url()` /
`auth_url_async()` with `PkceCodeChallenge::new_random_sha256()`, then
`RedirectListener::new(...)`; wrapped by `fulcio::oauth::OauthTokenProvider` with
`DEFAULT_REDIRECT_PORT = 8080` and a `webbrowser` launch. (Worth noting given the Fulcio
SSRF caveat in §5: this flow deliberately sets `redirect::Policy::none()` with the comment
*"Following redirects opens the client up to SSRF vulnerabilities"*.)

**What does not exist at all — ambient/CI detection:**

```
grep -rniE 'ACTIONS_ID_TOKEN|CI_JOB_JWT|ambient|detect_credential' src/   →  no hits
```

The only `GITHUB_ACTIONS` hits are a hardcoded **issuer URL** in `cosign/mod.rs` used as a
verification constraint, not for acquisition. There is no equivalent of sigstore-python's
`id` or sigstore-go's ambient credential detection.

**So #194's CI story is ocx-owned:** read `ACTIONS_ID_TOKEN_REQUEST_URL` +
`ACTIONS_ID_TOKEN_REQUEST_TOKEN` (or GitLab's `id_tokens` JWT), exchange over HTTPS, then
`IdentityToken::try_from(jwt)`. ~30 lines of ordinary HTTP against a documented endpoint —
no crypto, no wire format, and no maintained Rust crate exists to delegate it to.

### Item 3 — End-to-end verification order, and what is skipped

Execution order for a `MessageSignature` bundle, read top-to-bottom from
`bundle/verify/verifier.rs:130-224`. The crate documents seven steps at `:130-145`; **it
implements five.**

| # | Check | Line | Status |
|---|---|---|:-:|
| 1 | Cert chain to trusted root, at `issued_at`, code-signing EKU (webpki) | `:160` | **yes** |
| 1b | SCT verified against the CTFE keyring | `:168` | **yes** |
| 2 | Identity policy — `policy.verify(&materials.certificate)?` | `:172` | **yes** |
| 3 | Artifact signature over the digest — `verify_bundle_content` | `:180` | **yes** |
| 4 | Rekor entry consistent with signing materials (CVE-2022-36056) | `:189` | **yes** |
| 5 | **Merkle inclusion proof** | `:196` | **NO — comment only** |
| 6 | **Signed Entry Timestamp (SET)** | `:200` | **NO — comment only** |
| 7 | `integrated_time` within cert `notBefore`/`notAfter` | `:206-217` | **yes** |

Verbatim, `:196-203` — there is no call between the two comments:

```rust
// 5) Verify the inclusion proof supplied by Rekor for this artifact,
//    if we're doing online verification.
// TODO(tnytown): Merkle inclusion; sigstore-rs#285

// 6) Verify the Signed Entry Timestamp (SET) supplied by Rekor for this
//    artifact.
// TODO(tnytown) SET verification; sigstore-rs#285
```

Subject-digest binding for DSSE is additionally checked at `:76` (in-toto statement subject
vs artifact hash); for `MessageSignature` the binding *is* step 3.

**Mapping to the issues — this changes two of the three verdicts in §1:**

| Issue | Verdict |
|---|---|
| #207 cert chain + temporal validity | **deletion** — steps 1 and 7 cover it |
| #208 SCT | **deletion, but only via `Verifier`** — `crypto::transparency` is `pub(crate)` (`crypto/mod.rs:150`), so there is no standalone `verify_sct` for ocx to call |
| #209 SET + Merkle | **NOT a deletion — `Verifier` performs neither** |

For #209 the two halves need different treatment, and neither is hand-rolled crypto:

- **Merkle: wiring, not implementation.** `crypto/merkle` is `pub(crate)`, but the entry
  point is public — `rekor/models/log_entry.rs:144`
  `pub fn verify_inclusion(&self, rekor_key: &CosignVerificationKey)`, delegating to
  `rekor/models/inclusion_proof.rs:61`, which requires a checkpoint, verifies its signature,
  binds it to the proof's root hash and tree size, then runs the RFC 6962 inclusion path.
  `SignedCheckpoint::verify_signature` is separately public (`checkpoint.rs:109`). One
  caveat to budget: it takes a `rekor::models::LogEntry` (REST shape), not the bundle's
  protobuf `TransparencyLogEntry`; upstream ships only the forward conversion
  (`bundle/models.rs:72`), so a reverse adapter is unwritten work.
- **SET: use the public key API with the real payload.** The correct routine exists at
  `cosign/bundle.rs:80-105` but is `pub(crate)`, and its only public door
  (`SignedArtifactBundle::new_verified`, `:48`) wants a cosign-v1 artifact bundle and the
  `cosign` feature, which drags in `oci-client` — unacceptable against a patched fork. Both
  halves needed are already public:

  ```rust
  use sigstore::crypto::{CosignVerificationKey, Signature};
  let buf = serde_json_canonicalizer::to_vec(&payload)?;  // {body, integratedTime, logIndex, logID}
  rekor_key.verify_signature(Signature::Base64Encoded(set_b64.as_bytes()), &buf)?;
  ```

  Signature verification is sigstore's, canonicalization is `serde_json_canonicalizer`'s (RFC
  8785, already transitive and the crate of record), the payload is a four-field serde
  struct. **This replaces ocx's custom `ocx-rekor-set-v1` payload with the real wire
  format** — a net improvement over both today's ocx and today's `Verifier`.

The upstream split is deliberate, not an oversight —
[sigstore-rs#283](https://github.com/sigstore/sigstore-rs/issues/283) planned *"a basic
implementation in the `crypto` module that is **not** part of the public API"* plus
*"methods on the related Rekor data structures — this would be part of the public API"*.

**Where `policy.rs` fits, and the #98 conflict risk.** `bundle/verify/policy.rs` is a
complete identity-pinning layer and it is fully public:

```rust
pub trait VerificationPolicy { fn verify(&self, cert: &x509_cert::Certificate) -> PolicyResult; }
pub struct Identity { identity: String, issuer: OIDCIssuer }
impl Identity { pub fn new<A, B>(identity: A, issuer: B) -> Self }   // :251
pub struct AnyOf<'a>;  pub struct AllOf<'a>;                          // :170, :201
// single-extension policies, each pinned to its Fulcio OID:
OIDCIssuer, GitHubWorkflowTrigger, GitHubWorkflowSHA,
GitHubWorkflowName, GitHubWorkflowRepository, GitHubWorkflowRef      // :126-161
```

`Identity::new(identity, issuer)` is **exactly** ocx's `--certificate-identity` +
`--certificate-oidc-issuer` pair: it verifies the OIDC issuer extension, then matches the
identity against the SAN (emails, URIs, and Sigstore "other names"). `AnyOf`/`AllOf` compose
them.

> **They must not double up.** `verify_digest` calls `policy.verify(...)` at `:172` as step
> 2 — the policy is a *parameter*, so ocx's `[[trust.policy]]` should **compile down to a
> `VerificationPolicy`** and be passed in, not be evaluated a second time afterwards in
> ocx's own pipeline. Implementing `VerificationPolicy` for ocx's config type, or mapping it
> onto `Identity` + `AnyOf`, gets #98 with no duplicated matching logic and no risk of the
> two disagreeing. The six GitHub OID policies are a free superset of what #98 currently
> specifies.

### Item 4 — Newer release

**None. 0.14.0 is the latest, and Rekor v2 has not landed.**

crates.io JSON API (`https://crates.io/api/v1/crates/sigstore`): `max_version = "0.14.0"`,
`updated_at = "2026-05-22"`. GitHub releases: v0.14.0 (2026-05-22) is latest; v0.13.0 was
2025-10-16; v0.12.1 2025-05-28. The default branch's own `Cargo.toml` still reads
`version = "0.14.0"`, so `main` is not staged ahead of the release either.

`sigstore_protobuf_specs`: `max_version = "0.5.1"` (2026-04-06) — the workspace pin is
already current.

**Upgrade cost: zero, because there is nothing to upgrade to.** Every finding in this
document is stable against the latest published crate and against `main`. Rekor v2 confirmed
absent (`grep -rniE 'rekor.*v2|api/v2/log|tile|dev\.sigstore\.rekor\.v2' src/rekor/` →
empty); [#513](https://github.com/sigstore/sigstore-rs/issues/513) *"Merkle tree and Note
format foundation for Rekor v2"* is **closed**, but that shipped the `crypto::merkle` +
checkpoint foundation only — no v2 client. #107 stays open and stays gated upstream.

### Corrections to earlier sections

1. **§1 line 160 — the chain is walked at the certificate's own `notBefore`, not at the
   Rekor integrated time.** `verifier.rs:148`:

   ```rust
   let issued_at = tbs_certificate.validity.not_before.to_unix_duration();
   ```

   The Rekor `integrated_time` is used separately, at `:206-217`, for a *range containment*
   check (`integrated_time` must fall within `not_before`..`not_after`). Both behaviours are
   correct and together they cover #207 — but they are two distinct checks, and the ADR
   should not claim the chain walk happens at the log timestamp.

2. **§3's premise is right, its scope is not.** `crypto/transparency.rs` is real, complete,
   and the reason ocx bypassed the crate against the SCT-less fake — all confirmed. But
   `crypto/mod.rs:150` declares `pub(crate) mod transparency;` with no re-export, so ocx
   gets SCT verification **only** as an internal effect of `Verifier::verify_digest`, never
   as a callable primitive. Same for `crypto/certificate_pool.rs` (`:133`) and
   `crypto/merkle` (`:22`). A probe importing any of them yields `E0603: module is private`.
   #207 and #208 remain deletions *through `Verifier`*; #209 does not (Item 3 above).

3. **Not in scope of the open items, but load-bearing for milestone 5:** `SigningContext::new`
   is public yet **uncallable** — its third parameter is `Keyring`, declared in
   `pub(crate) mod keyring` (`crypto/mod.rs:137`) with no re-export and no public
   constructor. A probe yields `E0308: expected 'Keyring', found '()'` alongside `E0603`.
   `production()`/`async_production()` exist only under `sigstore-trust-root` and hardcode
   public-good Sigstore. Upstream:
   [#562](https://github.com/sigstore/sigstore-rs/issues/562) — *"`Keyring` is `pub(crate)`
   and not re-exported"*, *"the only escape is to vendor the `Keyring` construction code"*.

   **A real Fulcio + CTFE therefore does not unblock the high-level signing path.** The
   blocker is upstream visibility, not the fake stack. ocx keeps owning sign orchestration,
   assembled from public pieces (`request_cert_v2` + `p256`/`x509-cert` + `create_log_entry`
   + a hand-built `Bundle`) — none of which is hand-rolled crypto or a hand-rolled wire
   format.
