# Research: sigstore-rust capability mapping, sign and attest side

## Metadata

**Date:** 2026-11-23
**Domain:** security
**Triggered by:** discussion `.agents/discussions/rekor-v2-sigstore-rust.md`
**Expires:** 2027-02-23 (sigstore-rust at 7fbe058a; three breaking 0.x releases in the prior 30 days)

Paths into B are relative to the clone root and prefixed `sigstore-rust:`. X paths are relative to the ocx repo root
(`crates/ocx_sign/src/` abbreviated `S/`). B = sigstore-rust 0.17.0 (edition 2021, MSRV 1.86, reqwest 0.13, aws-lc-rs default).
Nothing was built or run; every statement is from reading source. Facts only, no recommendation, no ranking.

## Direct Answer

- B has a one-call keyless signer (`sigstore-sign` `Signer`): ephemeral P-256 key, Fulcio v2 `signingCert`, Rekor v1 or v2
  write, optional RFC 3161 timestamp, bundle v0.3, for both message-signature and DSSE/in-toto content.
- B's Fulcio, Rekor, TSA, OIDC and TUF clients each take a caller-supplied `reqwest::Client`. B does not inject it into
  ambient-credential detection (`Detector::new()` builds its own).
- B has no: encrypted-PEM key loading, KMS, cosign simplesigning, OCI/referrers/sidecar code, Rekor v1 write with a bare
  public key, intoto write, DSSE v2 entry kind, SBOM code, predicate-type vocabulary, device flow.
- B has no SCT check on the signing path (X's recorded reason 1 does not apply). B parses the Fulcio response as either
  embedded or detached SCT.
- B offers `SigningServices::new(fulcio, rekor)` (v1, no TSA), so a stack with Fulcio and Rekor v1 and no TSA is
  addressable without a SigningConfig. `from_signing_config` requires a TSA entry.
- X non-test lines in the listed scope: 4837 physical (3555 excluding blank and `//` lines). Under the counting rules
  below: (a) 195 call sigstore today, (b) 924 are X-own with a B counterpart (798 of those need a non-trivial adapter),
  (c) 3718 have no B counterpart or are X-specific.

## Findings

### 1. OIDC token acquisition

X (`S/sign/oidc.rs:1-108`, `S/sign/oidc_ambient_inline.rs:1-169`, `S/sign/oidc_ambient.rs:1-28`, `S/sign/oidc_browser.rs:1-35`):
- Caller-supplied token: `DispatchingTokenProvider.override_token` short-circuits (`oidc.rs:81-83`). Opaque string, no JWT parse.
- Ambient, own code, env inspection (`oidc_ambient_inline.rs:48-68`): GitLab `SIGSTORE_ID_TOKEN`, CircleCI
  `CIRCLE_OIDC_TOKEN_V2` (read directly), GitHub Actions (`ACTIONS_ID_TOKEN_REQUEST_URL/TOKEN`, GET with bearer, `&audience=`
  appended, response `value`). Order gitlab, circle, gha is documented as contract (`:47-49`). Audience passed is `"sigstore"`
  (`S/sign/pipeline.rs:312`).
- GHA exchange goes through `guarded_request_url` (`:72-85`: scheme gate + SSRF resolve with `trusted_hosts`), the shared
  client, `read_body_capped`, and maps transient vs rejected failures to distinct exit kinds.
- `oidc_ambient.rs:15-27`: seam for the `ambient-id` crate, `detect` always returns `None`.
- Browser flow: `oidc_browser.rs:27-34` always returns `OidcPreCheckFailed{browser_flow_unavailable}`. No device flow.
- `no_tty` refuses instead of hanging (`oidc.rs:99-103`).

B (`sigstore-rust:crates/sigstore-oidc/src/`):
- Ambient: `IdentityToken::detect_ambient()` (`token.rs:161-170`) calls `ambient_id::Detector::new().detect("sigstore")`.
  The source comment names "GitHub Actions, GitLab CI, Buildkite, etc." (`token.rs:159-160`). The provider list lives in the
  external crate `ambient-id` 0.0.11 (`sigstore-rust:Cargo.toml:58`), not in the clone. docs.rs page for 0.0.11 lists four:
  GitHub Actions, GitLab CI, Buildkite, CircleCI (fetched via WebFetch, one source, not read from code).
- Interactive browser: `OAuthClient::authenticate` / `authenticate_with_options` (`oauth.rs:~260-300`): authorization code
  with PKCE S256, localhost redirect listener, opens browser under feature `browser`; falls back to out-of-band code paste
  (`AuthMode::{BrowserRedirect, OutOfBand}`, `oauth.rs:111-118`). `OAuthConfig::dex(issuer)` (`:60`).
- Device flow: none found (grep "device" matches only HTML viewport meta).
- Caller-supplied: `IdentityToken::from_jwt` (`token.rs:135`). Requires three dot-separated parts and claims `iss`, `sub`,
  `aud`, `exp` deserializable (`TokenClaims`, `:68-90`); signature not checked.
- Proof-of-possession subject: `identity()` = `email` else `sub` (`token.rs:213-215`); an empty-string email is not
  skipped (X skips empty, `S/sign/signer.rs:299-306`).

Verdict: ambient detection and browser flow: **B covers more than X (X code could shrink)** in provider breadth (4 vs 3
providers; X has no working browser flow). Against: B's `detect_ambient()` takes no client or URL gate, so X's guarded GHA
exchange and capped body read cannot be reproduced through that call (see 11). X's dispatcher/override/no_tty policy
(`oidc.rs`) is **X-specific, not a library concern**.

X non-test lines: `oidc.rs` 108, `oidc_ambient_inline.rs` 169, `oidc_ambient.rs` 28, `oidc_browser.rs` 35. All X own code.

### 2. Fulcio

X (`S/sign/fulcio.rs`, 201 non-test lines, own code, no `sigstore` call; module doc `:4-5` records why):
- `POST {url}/api/v2/signingCert` (`:105`). Body JSON `credentials.oidcIdentityToken`, `publicKeyRequest.publicKey{algorithm:"ECDSA",
  content:<PEM verbatim>}`, `proofOfPossession` (`:26-52`, `:86-99`). PoP = ECDSA over sha256(email-or-sub)
  (`S/sign/signer.rs:245-262`).
- Response: `signedCertificateEmbeddedSct` or `signedCertificateDetachedSct`, `.or()` between them (`:132-139`). SCT is not
  checked at the sign side; detached SCT value is discarded. Requires chain length >= 2 (`:141-145`).
- Custom URL: `--fulcio-url`, SSRF pre-flight `guard_dialed_endpoints` (`S/sign/pipeline.rs:188-212`).
- Error classification by status and body substring (`:164-196`), body capped, log line sanitized.

B (`sigstore-rust:crates/sigstore-fulcio/src/client.rs`):
- `FulcioClient::create_signing_certificate(&IdentityToken, &KeyPair)` (`:80-139`) posts `{url}/api/v2/signingCert`; v2
  JSON; same field names; `algorithm` sent as `""` (`:~100`), PoP = `key_pair.sign(identity)`. v1 `/api/v1/signingCert` not
  present. `get_configuration` (`/api/v2/configuration`) and `get_trust_bundle` (`/api/v2/trustBundle`) also present.
- SCT: `SigningCertificate::{EmbeddedSct{chain}, DetachedSct{chain, signed_certificate_timestamp}}` (`:372-385`); both or
  neither is `InvalidResponse` (`:387-410`). No SCT verification anywhere in `sigstore-sign` (grep "sct" in
  `sigstore-sign/src`: no hits). Chain length >= 1 required (`:350-355`); X requires >= 2.
- Custom URL: `FulcioClient::builder(url)` / `SigningServices::new(fulcio_url, ..)`; no scheme restriction found.
- Errors: `Error::Status{status, message}` with body text; transport as `Error::Http(String)`. Body read with `.bytes()` /
  `.text()` uncapped.

Verdict: **replaceable with adapter**. Same wire endpoint and JSON; X would keep `classify_rejection`/`detail_snippet`
(33 lines) and its pre-flight; B hides response bodies from size capping. Unverified: whether a local Fulcio accepts
`algorithm:""` (B tests against public-good and staging per its conformance crate; not run here).

### 3. Rekor v1 write

X (`S/sign/rekor.rs`, 323 non-test lines, own code; only borrowed item is `sigstore::rekor::models::log_entry::RekorInclusionProof`, `:13`):
- `POST {url}/api/v1/log/entries` (`:190`). `hashedrekord` `0.0.1` (`:134`), `dsse` `0.0.1` via `TLOG_KIND_WRITTEN` (`:266`, `S/attest.rs:46-49`).
  Both carry the leaf PEM base64-encoded; DSSE uses `proposedContent{envelope:<string>, verifiers:[b64 PEM]}`. No `intoto` write
  (`S/attest.rs:42-44`: "Never add intoto:0.0.1").
- Response: map keyed by uuid, first value; decodes body, SET, logIndex, integratedTime, logID, optional inclusionProof
  (`:299-323`), all failures `RekorSetMalformed`.
- Beyond a client: retry ladder on transient status (`:169-186`), 409-on-retry recovery by fetching `Location` entry id
  (`:223-250`), status classification (`:288-297`), capped body, key-mode upload with a public key `verifier`
  (`S/sign/key_signer.rs:74-84, 143-146`).

B (`sigstore-rust:crates/sigstore-rekor/`):
- `RekorClient::create_entry(HashedRekord)` / `create_dsse_entry(DsseEntry)` (`src/client.rs:~140-175`) POST
  `{url}/api/v1/log/entries`; `get_entry_by_uuid`, `get_entry_by_index`, `search_index`, `search_by_hash`.
- `HashedRekord::new(hash, signature, certificate)` kind `hashedrekord` / `0.0.1` (`src/entry.rs:383-411`); `DsseEntry::new(envelope,
  certificate)` kind `dsse` / `0.0.1` `proposedContent` (`:262-303`). Both certificate-only; no public-key constructor on v1.
- Entry body parsing (read side) covers `HashedRekordV001/V002`, `DsseV001/V002`, `IntotoV002` (`src/body.rs:22-32`); no write path for intoto or DSSE v0.0.2.
- Response: `single_entry` takes first map value; `LogEntry::to_bundle_entry(KindVersion)` (`src/entry.rs:79-111`) converts SET and
  inclusion proof (checkpoint text parsed, `CheckpointData::new`); inclusion proof optional (X refuses a bundle without one,
  `S/sign/bundle.rs:99-110`).
- No retry, no 409 handling (`Error::Status{status,message}` carries no headers), no body cap, no `Retry-After`.
- Custom URL: `RekorClient::builder(url)`.

Verdict: **replaceable with adapter** for certificate-mode hashedrekord and dsse. **Gap in B** for key-mode v1 upload (public
key verifier) and for `intoto` writes (X does not write intoto). Retry/409/status classification stay X's.

### 4. Rekor v2 write

X: none. `S/sign/rekor.rs:4-6` records "Rekor v2 is not supported: ocx-sh/ocx#107". No TSA code in `S/sign`, `S/attest*` (grep).

B:
- `RekorV2Client::create_entry(HashedRekordV2) -> TransparencyLogEntry` (`sigstore-rust:crates/sigstore-rekor/src/client.rs:~200-230`),
  `POST {url}/api/v2/log/entries`; response is deserialized straight into the bundle's `TransparencyLogEntry`.
- Entry kind: only `hashedrekord` `0.0.2` (`HashedRekordV2::new_with_certificate` / `new_with_public_key`, `src/entry.rs:424-520`).
  Key details enum includes `PKIX_ECDSA_P256_SHA_256` (`:453`). No `dsse` v0.0.2 request type; `sigstore-sign` signs DSSE under
  v2 as a hashedrekord v0.0.2 over sha256(PAE) (`sigstore-rust:crates/sigstore-sign/src/sign.rs:~813-824`; enum doc
  `src/entry.rs:20-23` "hashedrekord 0.0.2 for both artifacts and DSSE envelopes").
- Response validation `validate_v2_entry` (`client.rs:~300-350`): kind is hashedrekord/0.0.2; body matches the submitted
  digest, signature, verifier; no `integratedTime`; no inclusion promise; inclusion proof present; checkpoint non-empty.
- Read helpers: `get_checkpoint`, `get_tile`, `get_entry_bundle` (tlog-tiles paths, `client.rs:~230-290`).
- RFC 3161: `sigstore-sign` `validate_configuration` (`sign.rs:~300-314`) returns `TimestampAuthorityRequired` when Rekor is not v1
  and no TSA URL is set. `TimestampClient::timestamp_signature(&SignatureBytes)` POSTs `application/timestamp-query`
  (`sigstore-rust:crates/sigstore-tsa/src/client.rs:~85-135`); validates message imprint and nonce on the reply
  (`:~137-175`). Timestamp token attached via `BundleV03::with_rfc3161_timestamp`.

Verdict: **B covers more than X** (X has no v2 or TSA code). Not a capability of X's to replace.

### 5. Endpoint discovery / SigningConfig

X: none. `SigningConfig` not referenced anywhere in `crates/` (grep). URLs are `--fulcio-url`, `--rekor-url`
(hardcoded defaults in `crates/ocx_oci/src/endpoint.rs:19`, `package_sign_common.rs:183`). Prior recon confirms no SigningConfig handling.

B (`sigstore-rust:crates/sigstore-trust-root/src/signing_config.rs`, `crates/sigstore-sign/src/sign.rs`):
- `SigningConfig::from_json`, `from_file(path)` (`:~180-195`), media-type check (`:68`); embedded snapshots for production,
  staging, GitHub (`:42-55`); TUF fetch `SigningConfig::from_tuf(TufConfig)` (`src/tuf.rs:~578-618`, feature `tuf`); custom TUF
  repo via `TufConfig::custom(url, bootstrap)` (`:167`) and `custom_from_file` (`:199`); offline from cache (`:246`).
- Selection: `rekor_url(force_version)` sorts eligible Rekor endpoints by major version descending and takes the first;
  supported Rekor majors `[1,2]`, TSA `[1]`, Fulcio `[1]` (`:59-65`, `:~200-290`). Fulcio and TSA take the first eligible.
- Caller can force v1 or v2: `SigningServices::from_signing_config_with_rekor_version(cfg, Some(version))`
  (`sign.rs:~230-270`); errors `MissingRekorVersion` when none matches. Also `SigningServices::with_rekor(url, version)`
  and `SigningServices::new(fulcio, rekor)` (v1) for explicit URLs without a config.
- Several versions listed: highest preferred; `ServiceSelector` ANY and EXACT count 1 accepted; ALL accepted only if the
  eligible endpoints share one operator; other selectors rejected (`check_single_service_requirement`, `sign.rs:~325-360`).
- `from_signing_config` requires a TSA endpoint (`MissingService(Tsa)`) and a Fulcio endpoint; Rekor required when the
  config lists any or has `rekorTlogConfig`; a config with neither (GitHub) signs without tlog.

Verdict: **B covers more than X**. Not replacement of X code.

### 6. Bundle assembly

X (`S/sign/bundle.rs`, 279 non-test lines): bundle v0.3 only, media type `application/vnd.dev.sigstore.bundle.v0.3+json`
(`:29`). Built from `sigstore_protobuf_specs` generated types (`Bundle`, `VerificationMaterial`, `TransparencyLogEntry`,
`InclusionPromise`, `InclusionProof`, `Checkpoint`, `X509Certificate`, in-toto `Envelope`) and serialized with `serde_json`
(`:7-12, 72-154`). Content: DSSE envelope only (`build_dsse_bundle`, `:200-227`); the keyless blob path goes to a cosign
simplesigning sidecar, not a message-signature bundle. Verification material: `certificate` (single leaf) or
`publicKey{hint}` (key mode) (`:156-180`). Mandatory inclusion proof (`:99-110`). Binds the Rekor body hashes back to the
uploaded envelope (`assert_body_records_our_envelope`, `:229-247`). Also computes `SignedBundle` side data (identity from the
leaf, sidecar annotation material, `:33-56`, `:130-152`).

B (`sigstore-rust:crates/sigstore-bundle/src/builder.rs`, `crates/sigstore-types/src/bundle.rs`):
- `BundleV03` builder: `with_certificate_and_signature`, `with_certificate_and_dsse`, `with_tlog_entry`,
  `with_rfc3161_timestamp`, `into_bundle()` (`builder.rs:47-115`). `VerificationMaterialV03::{Certificate, PublicKey{hint}}` (`:14-28`).
  `TlogEntryBuilder` (`:138-210`).
- `MediaType` enum 0.1, 0.2, 0.3 (`bundle.rs:31-95`); `Bundle::from_json`, `to_json`, `to_json_pretty` (`:126-136`). Serde JSON on hand-written
  types; protobuf-generated crate used only by the test-only `sigstore-protobuf-compat` (`lib.rs:1-11`).
- `validate_bundle(_with_options)` (`sigstore-bundle/src/validation.rs:83,120`) with optional require-inclusion-proof / timestamp.
- KeyId empty is skipped on serialize (`sigstore-types/src/dsse.rs:33-37`), same as X's `skip_serializing_if` (`S/attest/dsse.rs:120-123`).

Verdict: **replaceable with adapter**. X's `SignedBundle` side data, hash binding and mandatory-proof policy stay. X's v0.3
DSSE certificate/public-key shapes both exist in B. Whether B's JSON is byte-identical to the protobuf-JSON X emits is covered by B's
protobuf-compat tests (not run).

### 7. End-to-end sign API

B: `SigningContext::{new, production, staging, for_instance, from_tuf, from_embedded}` then `.signer(IdentityToken)` or
`.authenticate()` returning `Signer` (`sigstore-rust:crates/sigstore-sign/src/sign.rs:~410-520`).
- `Signer::sign(artifact)`, `sign_reader`, `sign_async_reader`: ephemeral key, Fulcio cert, sign sha256, Rekor (v1 or v2) unless none,
  TSA when configured, `BundleV03`, returns `Bundle` (`:~560-720`).
- `Signer::sign_attestation(Attestation)` and `sign_raw_statement(&[u8])`: DSSE with payloadType `application/vnd.in-toto+json`,
  validates statement JSON before spending a Fulcio cert, Rekor DSSE entry, bundle (`:~735-840`).
- Stages usable separately: yes. `FulcioClient`, `RekorClient`/`RekorV2Client`, `TimestampClient`, `BundleV03`, `KeyPair`
  are all public and re-exported from `sigstore-sign` (`lib.rs:29-41`). `Signer`'s own stages are private methods.
- `Signer` accepts only an `IdentityToken` and an ephemeral P-256 key; no entry point takes an existing key, a
  `KeyBackend`-style trait, or a hook between stages. Signing scheme is fixed to ECDSA P-256 SHA-256 (`sign.rs` `SigningServices::signing_scheme`).

X beyond that (`S/sign/signer.rs`, `key_signer.rs`, `pipeline.rs`):
- `Signer` trait with a keyless and a key implementation, selected at run time (`signer.rs:69-120`; `key_signer.rs:32-153`).
- `identity()` pre-flight that spends one Fulcio cert and no Rekor entry to learn the SAN before deciding whether the
  signature already exists (`signer.rs:138-160`, `S/sign/referrers.rs:97-125`).
- 16 MiB statement cap before any network (`signer.rs:204-212`), `SignedBlob` carrying the cosign offline-bundle annotation
  material (`signer.rs:24-40`, `simplesigning_write.rs:359-398`).
- Whole OCI pipeline: subject resolve, referrers capability probe and cache, bundle referrer and cosign sidecar legs,
  per-leg best-effort results, SSRF pre-flights (`pipeline.rs:49-552`).

Verdict: DSSE keyless path **replaceable with adapter**; keyless `sign_blob` for simplesigning needs raw signature, cert
PEM and the Rekor entry, available only by using B's stages (not `Signer::sign`); key mode and the OCI pipeline are
**X-specific, not a library concern** / **gap in B** (key mode has no Rekor v1 public-key upload).

### 8. Key signing (non-keyless)

X (`S/sign/key_backend.rs`, 236 non-test lines; `key_signer.rs` 180): `KeyBackend` async trait, "KMS-shaped"
(`:17-35`); `PemKeyBackend::from_encrypted_pem` calls `sigstore::crypto::signing_key::SigStoreKeyPair::from_encrypted_pem`
(`:152-153`) for cosign `ENCRYPTED SIGSTORE PRIVATE KEY`, then P-256 only (`:165-185`). Hint = base64(sha256(SPKI))
(`:39-41`). Only the PEM backend exists ("reserved for the first remote backend, nothing constructs it yet", `:46-49`).
`env://` and file sources.

B (`sigstore-rust:crates/sigstore-crypto/src/signing.rs`): `KeyPair::generate(KeyAlgorithm)`, `generate_ecdsa_p256`,
`from_pkcs8_der` (`:225-241`), `sign`, `sign_prehashed`, `sign_digest`, `public_key_der`. `SigningScheme` covers ECDSA
P-256/P-384/P-521, Ed25519, RSA PSS/PKCS1, ML-DSA 44/65/87 (`:17-45`). Searched B for `scrypt`, `secretbox`,
`ENCRYPTED SIGSTORE`, `kms`: no match. No encrypted-PEM loader and no KMS in B; bundle material `PublicKey{hint}` exists.

Verdict: encrypted-PEM load: **gap in B**. Key algorithm breadth: B covers more than X (X is P-256 only). KMS: **gap in B**
(and unimplemented in X; the trait is X's own).

### 9. DSSE and in-toto

X (`S/attest/dsse.rs` 154, `statement.rs` 201, `predicate.rs` 195, `S/attest.rs` 57):
- PAE (`dsse.rs:13-26`); envelope wire type with single-signature enforcement, 16 MiB payload ceiling, payloadType must
  equal `application/vnd.in-toto+json` (`:40-90`); `envelope_hashes` for the Rekor `dsse` body (`:132-154`).
- Statement `_type` v1 written, v1 and v0.1 accepted; predicate kept as `RawValue` so bytes are preserved
  (`statement.rs:1-110`); `binds_subject` sha256-only binding with capped diagnostics (`:146-201`).
- Predicate vocabulary: cosign `--type` alias table (cyclonedx, spdx, spdxjson, slsaprovenance[02|1], link, vuln, openvex,
  custom) -> URI, cosign `custom` wrapper `{Data, Timestamp}`, SLSA v0.2/v1 builder-id accessor, SBOM artifactType mapping
  (`predicate.rs:19-190`).
- SBOM: CycloneDX 1.5-1.7 summary reader (`S/sbom/cyclonedx.rs`, 70 non-test lines; `S/sbom.rs`, 58).

B:
- `sigstore-types` `DsseEnvelope` (single signature enforced at deserialization), `DsseSignature`, `pae()` (`src/dsse.rs:14-112`);
  `Statement` (`type_`, `subject`, `predicate_type`, `predicate: serde_json::Value`), `Subject`, `Digest`, `Statement::TYPE_V1`,
  `matches_sha256/512` (`src/intoto.rs:20-200`).
- `Attestation::new(predicate_type, Value).add_subject(...)` builds a v1 statement (`sigstore-sign/src/sign.rs:~870-960`).
  `sign_raw_statement` takes caller bytes verbatim, which avoids the `Value` re-serialization for the signed payload.
- Predicate types: B has no predicate-type enum, alias table, or builder-id accessor; only the doc/example mention
  `https://slsa.dev/provenance/v1` (`intoto.rs:30, 261, 316`). Searched for `cyclonedx|spdx|slsa` across B: only those
  hits and `sigstore-verify` fixtures. No SBOM code.
- No payload size cap, no payload-type choice (hardcoded in `sign_raw_statement`).

Verdict: PAE and envelope type: **replaceable with adapter** (X's parse adds caps and `VerifyErrorKind` mapping). Statement type:
**replaceable with adapter** (B's `Value` predicate vs X's `RawValue`). Predicate vocabulary, SBOM reader, subject-binding
diagnostics: **X-specific, not a library concern** / none found in B.

### 10. Cosign simplesigning write, referrers, sidecar tags

X: `S/sign/simplesigning_write.rs` (398 non-test lines) writes `sha256-<hex>.sig` / `.att` sidecar manifests, appends layers
under a bounded compare-and-set loop (`:28-31, 124-205`), annotations `dev.cosign.signature`, certificate, chain, bundle
(`:44-98`), media types `application/vnd.dev.cosign.simplesigning.v1+json` and the DSSE envelope type (`:19-22`);
`S/sign/referrers.rs` (291) probes the OCI Referrers capability with a cache, finds existing signature referrers, attaches
the bundle referrer (`:28-95, 145-250`). `S/simplesigning.rs` (74, outside the listed scope) is the claim type.

B: searched for `simplesigning`, `referrers`, `ENCRYPTED SIGSTORE`, `oci`, `kms`, `cosign` across all files in the clone.
No `simplesigning`, `referrers`, OCI registry, sidecar or layer code exists. `cosign` appears only in verify tests
(cosign v3 blob bundle fixture) and a doc comment. No `oci` dependency in any `Cargo.toml`.

Verdict: **gap in B**; also **X-specific, not a library concern** (registry layout and cosign sidecar compatibility).

### 11. HTTP transport

X: one process-wide client `ocx_oci::endpoint::sigstore_http_client()` (`crates/ocx_oci/src/endpoint.rs:56-70`): custom TLS roots
(`ocx_util::tls::sigstore_roots`), proxy rules (`ssrf::proxy_rules`), read timeout, redirects refused
(`refuse_redirects`, `:72-82`). Every X Fulcio/Rekor/GHA request uses it; endpoint pre-flight `resolve_sigstore_url` and
`validate_sigstore_url`; bodies via `read_body_capped`.

B, per client:
- Fulcio: `FulcioClientBuilder::with_http_client(reqwest::Client)` (`sigstore-fulcio/src/client.rs`, builder `:~215-245`).
  Default: `reqwest::Client::builder().timeout(30s).user_agent(..)` with the crate's `rustls` feature.
- Rekor v1/v2: `RekorClientBuilder` / `RekorV2ClientBuilder::with_http_client` (`sigstore-rekor/src/client.rs:~330-420`).
- TSA: `TimestampClientBuilder::with_http_client` (`sigstore-tsa/src/client.rs:~25-50`).
- OIDC OAuth: `OAuthClient::with_http_client(config, client)` (`sigstore-oidc/src/oauth.rs:~228`).
- TUF/signing config: `TufConfig::with_http_client` (`sigstore-trust-root/src/tuf.rs:218`); `sigstore-tuf` also exposes a
  `Repository` trait (`sigstore-tuf/src/transport.rs`) but `TufConfig` only takes a reqwest client.
- Whole signer: `SigningContext::with_http_client(client)` pushes one client to Fulcio, Rekor, TSA and the OAuth client
  (`sigstore-sign/src/sign.rs:~485-495, 525-565`).
- Ambient detection: `Detector::new()` inside `detect_ambient()`; no client parameter (`sigstore-oidc/src/token.rs:162`).
- Type compatibility: B pins `reqwest = 0.13` (`sigstore-rust:Cargo.toml:63`), X uses `reqwest 0.13` (root `Cargo.toml`), so
  the same `reqwest::Client` type resolves if both unify onto one 0.13.x.
- Not available: a trait-based transport for Fulcio/Rekor/TSA; body-size caps (all use `.bytes()`/`.text()`);
  pre-flight URL scheme or SSRF checks (no `https_only`/scheme check found); redirect policy of an injected client is the
  caller's.
- TLS/provider: default features `rustls` + `aws-lc-rs`; `rustls-no-provider` and `native-tls` features exist
  (`sigstore-sign/Cargo.toml`). `ambient-id` supports only `rustls` or `native-tls` (README `:186`).

Verdict: **replaceable with adapter** for Fulcio, Rekor, TSA, OIDC OAuth, TUF; **gap in B** for ambient detection transport
and for response-size caps.

### 12. X's recorded reasons for its own Fulcio/Rekor clients, applied to B

Recorded in: root `Cargo.toml:130-134` (sigstore's own clients "require an SCT + a different wire shape than the offline fake
stack"), `.claude/artifacts/research_sigstore_rs_spike.md` (SCT mandatory; `request_cert_v2` builds its own client), 
`adr_real_sigstore_stack_and_delegation.md` (Context and D4: unguarded timeout-free client; SCT; ocx controls both ends),
`S/sign/fulcio.rs:4-5` ("builds an unguarded, timeout-free `reqwest::Client`"), `.claude/artifacts/research_sigstore_rs_visibility_and_capabilities.md`
(`request_cert_v2` fixed client; SCT module `pub(crate)`; no Rekor v2).

| # | Reason recorded for sigstore-rs 0.14 | Applies to B? | Evidence |
|---|---|---|---|
| 1 | `sign_digest` unconditionally verifies the SCT (`bundle/sign.rs:140,143`), so it fails against a fake with no SCT | Not on signing path | no SCT verification in `sigstore-sign/src` (grep "sct": no hits); Fulcio response accepted with embedded or detached SCT (`sigstore-fulcio/src/client.rs:387-410`). X's test stack is now real Fulcio + CT (`sigstore_stack.py`), so the premise is already gone on X's side |
| 2 | "a different wire shape than the offline fake" (Python fake) | Not applicable to the real stack | B uses `/api/v2/signingCert`, same as X; fake was replaced by a real stack (ADR D1) |
| 3 | Fulcio/Rekor client builds an unguarded, timeout-free `reqwest::Client`, no injection | No: injection exists | `with_http_client` on every B client; default has a 30 s timeout (`DEFAULT_TIMEOUT`) |
| 4 | Ambient-token or token-fetch dials not under the SSRF guard | Applies to ambient detection only | `Detector::new()` not injectable (`token.rs:162`) |
| 5 | `SigningContext` constructors need an unconstructable `Keyring` or hardcode `FULCIO_ROOT`; usable only against public-good (ADR 2026-08-18 changelog) | No | `SigningServices::new(fulcio, rekor)`, `from_signing_config`, custom `TufConfig` (`sign.rs`, `tuf.rs:167`) |
| 6 | `production()` `block_on`s its own runtime (ASYNC-08 panic risk) | No (async `production().await`) | `sign.rs:~420-440` |
| 7 | No Rekor v2 | No | `RekorV2Client` exists |
| 8 | `SigningArtifact::to_bundle()` emitted v0.2; no `Content::PublicKey` arm | No for v0.3; public key hint supported in `BundleV03` | `builder.rs:14-28, 59-115`; v0.3 media type `bundle.rs:46` |
| 9 | `rsa` RUSTSEC-2023-0071 via `openidconnect`, non-optional | Not in B's manifests read (`sigstore-oidc` depends on `ambient-id`, `open`, `reqwest`; no `openidconnect`) | `sigstore-oidc/Cargo.toml`; transitive deps of `ambient-id` not inspected |
| 10 | RustCrypto versions pinned to sigstore 0.14's | Different stack: B uses `aws-lc-rs` default for crypto (`sigstore-crypto/Cargo.toml`) | not otherwise evaluated |
| 11 | Retry, 409 recovery, body caps, status-to-exit-code mapping, key-mode Rekor upload, identity pre-flight | These are X additions, not sigstore-rs reasons; none exist in B | see 3, 7 |

### 13. X's test stack versus B's clients

`test/tests/fixtures/sigstore_stack.py`: URLs `fulcio` `localhost:5555`, `rekor` `localhost:3000`, CT `localhost:6962/ocx-test`;
identity token from a local dex, `--identity-token-file`; trusted root from `SIGSTORE_DIR/trusted_root.json` carrying Fulcio CA,
CT log key, pinned Rekor key. ADR "Empirical validation" table: real Fulcio `--ca fileca` + CT returns
`signedCertificateEmbeddedSct`, chain length 2, issuer-v2 OID 57264.1.8 present; Rekor **v1** (Trillian) returns SET and
inclusion proof with checkpoint inline; no TSA, no TUF, no SigningConfig in the stack. dex token carries `iss`, `aud`, `email`.

Mapping to B, by reading (not executed):
- Fulcio: B's `/api/v2/signingCert` request, embedded-SCT response: matches. No SCT check, so a CT log is not needed to sign.
- Rekor v1: `SigningServices::new(fulcio_url, rekor_url)` yields v1, no TSA; `validate_configuration` passes
  (`sign.rs:~300-314`); `create_entry`/`create_dsse_entry` and `to_bundle_entry` handle SET and checkpointed inclusion proof.
- TSA: none present; B does not need one with Rekor v1. A Rekor v2 or TSA-requiring run is not possible against this stack.
- SigningConfig/TUF: absent in the stack; `from_signing_config` would not work (requires a TSA entry), `SigningServices::new` does.
- OIDC: `IdentityToken::from_jwt` needs `iss`, `sub`, `aud`, `exp` claims (`token.rs:68-90`); dex-minted tokens carry them per
  the ADR table (`email`, `aud`, `iss`); `sub` presence not recorded in the ADR table.
- Mismatches named: (i) `algorithm:""` in B's Fulcio request versus X's `"ECDSA"` (unverified against local Fulcio);
  (ii) X's tests assert behaviour B lacks (retry on 429/5xx, 409 recovery, capped bodies, exit-code mapping, identity
  pre-flight, key-mode Rekor upload, simplesigning sidecar, OCI referrers); (iii) the stack's issuer URL is in-network
  `http://dex:5556/dex` while the token is fetched over localhost: B does not dial the issuer in `from_jwt`, only
  `OAuthClient` (browser flow) does.

Verdict: the Fulcio + Rekor v1 + bundle signing stages are addressable against this stack by reading; the OCI, sidecar and
key-mode tests are not B's scope.

## Line-count summary

How counted: X non-test physical lines = lines before the first `#[cfg(test)]` in each file (every test module in these files
sits at end of file; `key_backend.rs` first cfg(test) is `StubKeyBackend`, `:237`). Includes comments and blanks. A second
count excludes blank lines and lines starting with `//` (so `///`, `//!`). Files: `sign.rs`, `sign/*.rs`, `attest.rs`, `attest/*.rs`,
`sbom.rs`, `sbom/cyclonedx.rs`. Not included: `simplesigning.rs` (74 non-test lines, X-specific claim type, outside the
listed scope), `verify*`. Category (a) = module/range that calls `sigstore`/`sigstore_protobuf_specs` today and would be
rewritten against B's types; (b) = X-own code with a B counterpart; the "(b) needing adapter" column marks those where a B call
would not replace the block without X-owned glue; (c) = no B counterpart, or X-specific. Straddling functions assigned by
dominant purpose; ranges below are by function, so figures are +/- a handful of lines.

| File (X) | Non-test lines | (a) | (b) | of (b): needs adapter | (c) |
|---|---:|---:|---:|---:|---:|
| `sign.rs` | 34 | 0 | 0 | 0 | 34 |
| `sign/error.rs` | 362 | 0 | 0 | 0 | 362 |
| `sign/format.rs` | 81 | 0 | 0 | 0 | 81 |
| `sign/state.rs` | 53 | 0 | 0 | 0 | 53 |
| `sign/oidc.rs` | 108 | 0 | 0 | 0 | 108 |
| `sign/oidc_ambient_inline.rs` | 169 | 0 | 152 | 152 | 17 |
| `sign/oidc_ambient.rs` | 28 | 0 | 28 | 0 | 0 |
| `sign/oidc_browser.rs` | 35 | 0 | 35 | 0 | 0 |
| `sign/fulcio.rs` | 201 | 0 | 168 | 168 | 33 |
| `sign/rekor.rs` | 323 | 0 | 171 | 171 | 152 |
| `sign/bundle.rs` | 279 | 195 | 0 | 0 | 84 |
| `sign/signer.rs` | 310 | 0 | 189 | 189 | 121 |
| `sign/key_signer.rs` | 180 | 0 | 0 | 0 | 180 |
| `sign/key_backend.rs` | 236 | 0 | 0 | 0 | 236 |
| `sign/pipeline.rs` | 551 | 0 | 0 | 0 | 551 |
| `sign/referrers.rs` | 291 | 0 | 0 | 0 | 291 |
| `sign/simplesigning_write.rs` | 398 | 0 | 0 | 0 | 398 |
| `attest.rs` | 57 | 0 | 0 | 0 | 57 |
| `attest/dsse.rs` | 154 | 0 | 63 | 0 | 91 |
| `attest/statement.rs` | 201 | 0 | 118 | 118 | 83 |
| `attest/predicate.rs` | 195 | 0 | 0 | 0 | 195 |
| `attest/pipeline.rs` | 463 | 0 | 0 | 0 | 463 |
| `sbom.rs` | 58 | 0 | 0 | 0 | 58 |
| `sbom/cyclonedx.rs` | 70 | 0 | 0 | 0 | 70 |
| **Total** | **4837** | **195** | **924** | **798** | **3718** |

Code-only total (no blank, no `//` lines): 3555 of the 4837.

Assignment notes:
- (a) `sign/bundle.rs`: imports `1-27`, `proto_inclusion_proof 57-70`, `assemble 72-154`, `SigningMaterial 156-180`,
  `build_dsse_bundle 200-228`, `parse_bundle 266-279`. These use `sigstore::bundle::Bundle`, `sigstore::rekor::models::...`,
  `sigstore_protobuf_specs`. (c) there: `SignedBundle` 33-56, `SignedEnvelope` 182-198, hash binding 229-265.
- (a) elsewhere is 0 by this rule: `sign/rekor.rs:13` and `:111` use one `sigstore` type (`RekorInclusionProof`); the Rekor client itself is X's.
  `sign/key_backend.rs:152-153` calls `sigstore` (`from_encrypted_pem`, about 50 lines in `144-193`) but B has no equivalent, so
  those lines are counted in (c) and would stay on the `sigstore` 0.14 dependency or become X-owned.
- (b) with adapter: ambient inline (all but `guarded_request_url`), Fulcio client (all but classification/log snippet),
  Rekor proposals/response parsing (not retry, 409, status classification), `KeylessSigner` and ephemeral-cert code in
  `signer.rs:122-310`, Statement type/build/parse in `statement.rs`. `oidc_ambient.rs`, `oidc_browser.rs` are stubs B replaces outright.
- `attest/dsse.rs` (b) 63: PAE, envelope type, Serialize, wire shape; (c) 91: parse with caps and `envelope_hashes`.
- The two ocx crates outside this scope that call `sigstore` (`verify/*`, `ocx_trust`) are not counted here.

## negative

- Searched B for `scrypt`, `secretbox`, `ENCRYPTED SIGSTORE`, `simplesigning`, `referrers`, `kms`, `device` flow, `oci`,
  `openidconnect`, SBOM/`cyclonedx`/`spdx` types: no hits. No Rekor v1 public-key entry constructor. No `intoto` or `dsse` 0.0.2 write.
- B Fulcio: no v1 `/api/v1/signingCert` call; no SCT verification in the sign path.
- Contradicting an easy reading: B does not need a TSA when Rekor v1 is used (`sign.rs:~300-314`); B's own provider list
  for ambient detection is not in the clone (external crate `ambient-id` 0.0.11); the four-provider list is from one docs.rs
  fetch, not read from code. Individual providers' env variables and audience handling are not established.
- Not established: whether a local Fulcio accepts `algorithm:""`; byte-identity of B's v0.3 JSON versus X's protobuf-JSON output;
  whether B's Rekor v1 request bodies are byte-equal to X's (field names match by reading); `ambient-id` transitive deps
  (including `rsa`); whether X and B unify onto one reqwest 0.13.x patch and one rustls provider (X uses `rustls`, B default
  `aws-lc-rs`; X's `deny.toml` bans `openssl`/`native-tls`; B's default features avoid them but `ring`/`aws-lc-rs` choice was not compared);
  `sub` claim in the dex token; which line numbers in B are exact: ranges prefixed `~` were read in context but not
  re-counted.
- X's `#[cfg(test)]` cut-off assumes no non-test code after the first test module; spot-checked in `key_backend.rs` and
  `pipeline.rs`, assumed elsewhere (only one `#[cfg(test)]` in most files).
- Nothing was executed; no test or build was run against either side.

## leads

- `sigstore-rust:crates/sigstore-conformance/` and `.github/workflows/interop.yml`: what B's own CI signs against (staging) and
  whether a local-stack path exists.
- `sigstore-rust:crates/sigstore-rekor/tests/v2_client.rs`: v2 request/response fixtures.
- External crate `ambient-id` 0.0.11: provider list, env variables, whether it can accept a client.
- `S/sign/rekor.rs:169-253` retry and 409 logic versus anything B's `Error::Status` would need to expose (headers, `Location`).
- X `verify/*` and `crates/ocx_trust` still call `sigstore` 0.14; sign-side swap alone would leave that dependency in place.
- `.agents/discussions/rekor-v2-sigstore-rust.md` for the question these answers feed.
