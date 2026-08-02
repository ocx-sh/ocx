# Plan — issue #194 sigstore sign/verify pipeline (slice 2)

## Status

- **Plan:** plan_issue194_sigstore_pipeline
- **Active phase:** 3 — Acceptance validation
- **Step:** /swarm-execute → implementation (Rust core committed 263a729d; acceptance suite green: 26 passed)
- **Last update:** 2026-07-09 (acceptance run against fake stack surfaced + fixed 3 positive-path bugs; test_sign.py + test_verify.py = 26 passed, 0 failed)

## Acceptance-validation fixes (positive path was never run green before)

The build converged but the sign→verify round-trip had never been exercised. Un-skipping the specs surfaced three real bugs, each fixed at root cause:

1. **Empty-config blob never pushed** (`sign/pipeline.rs`, `referrer/media_types.rs`) — the referrer manifest's `config` descriptor points at the OCI empty-config blob (`{}`, `sha256:44136fa3…`), but the pipeline pushed only the bundle blob. zot rejected the manifest with `MANIFEST_INVALID`. Fix: push the empty-config blob before the manifest (`push_blob` HEADs first, so it is a no-op after the first sign).
2. **Manifest URL compared to a digest** (`client/native_transport.rs`) — `push_referrer_manifest` compared `push_manifest_raw`'s return (the pullable manifest **URL**) against the expected digest, so the check always failed once the manifest push succeeded. The push is digest-addressed over the exact hashed bytes, so integrity is already guaranteed; dropped the bogus comparison.
3. **`artifactType` query not percent-encoded** (fork `external/rust-oci-client` `to_v2_referrers_url`) — the `+` in `…bundle.v0.3+json` was sent raw, which the registry decodes as a space, so the server-side referrers filter matched nothing → verify saw zero referrers → exit 79. Fix: build the query via `Url::query_pairs_mut().append_pair` (encodes `+`→`%2B`). Folded into the fork commit.

Also corrected one self-contradictory acceptance assertion: `test_sign_does_not_forward_identity_token_to_children` set a garbage non-JWT probe token then expected sign to return 0 — the fake Fulcio (correctly) rejects it (exit 80). Rewritten to drive the valid fixture token and assert it never surfaces on stdout/stderr.

## Deviations from the scaffold/ADR (decided during build)

- **Verify JSON `data` = flat** (`subject_digest`, `referrer_digest`, `certificate_identity`, `certificate_oidc_issuer`, `signed_at`) — the shipped `VerificationReport` used the ADR `signatures[]` shape but `test_verify.py::test_verify_success_envelope_golden_shape` pins the flat shape. Test wins; `verification.rs` rewritten.
- **Trust-root seam = Fulcio CA PEM** (not a Sigstore `TrustedRoot` JSON) — the scaffold's `TrustRoot::load_from_pem` already parses PEM and the fake writes `fulcio-root.pem`. `--trust-root`/`OCX_SIGSTORE_TRUST_ROOT` load a Fulcio CA PEM. Rekor pubkey is fetched online from `--rekor-url/api/v1/log/publicKey` (documented v1 simplification; trust-root Rekor key deferred to #196).
- **Hand-rolled HTTP** for Fulcio + Rekor (reqwest) — sigstore-rs's `SigningSession` mandates an SCT and its clients use a different wire shape than the offline fake. `sigstore` crate used for the `Bundle` type; `sigstore_protobuf_specs` for the rest. Features trimmed to `["bundle","rustls-tls"]` (dropped `sigstore-trust-root` → removed `tough`/`typed_path`, which was breaking `Cow<str>::as_ref` inference).
- **Fork change (needs upstream PR):** `external/rust-oci-client` gained `Client::pull_referrers` + `OciReferrersIndex`/`OciReferrerDescriptor` (referrers API). Submodule left dirty; not recorded in commit 263a729d.
- **Fake SET format:** `ocx-rekor-set-v1\n<logIndex>\n<integratedTime>\n<logID>\n<base64(body)>` (Ed25519), reproduced by `sign/rekor.rs::set_signing_payload`. NOT the public-good Rekor wire format (real-network verify is a network-gated `#[ignore]` path, out of #194).

## Locked design decisions (from Step-0 spike — see research_sigstore_rs_spike.md)

1. **Add `sigstore = 0.14`** (`default-features=false`, `["sign","verify","bundle","fulcio","rekor","sigstore-trust-root","rustls-tls"]`). `rsa` advisory RUSTSEC-2023-0071 ignored in `deny.toml` (documented; ECDSA-only, no RSA private key).
2. **Hand-roll sign + verify crypto** — sigstore's `SigningSession`/`bundle::verify` mandate SCT + real Rekor SET which the fake stack cannot produce. Reuse: `FulcioClient::request_cert_v2` (custom URL, no SCT check), `rekor::create_log_entry` (custom `base_path`), `sigstore_protobuf_specs::...::Bundle` (v0.3, serde), `p256`/`ecdsa`/`x509-cert`/`ed25519-dalek` (transitive → declare direct).
3. **Bundle** = `Bundle` struct built by hand, `media_type = Version::Bundle0_3` = `application/vnd.dev.sigstore.bundle.v0.3+json`. Carries leaf-cert-only chain, `MessageSignature{message_digest, signature}`, `tlog_entries[TransparencyLogEntry{ inclusion_promise=SET, integrated_time, log_index, log_id, canonicalized_body }]`.
4. **Trust-root seam** = `--trust-root <path>` flag on `ocx package verify` + `OCX_SIGSTORE_TRUST_ROOT` env. Loads a Sigstore `TrustedRoot` JSON (in-tree `sigstore_protobuf_specs` `TrustedRoot` type) → extract Fulcio CA cert(s). Production default (no override) = embedded/TUF (network-gated, documented, `TrustRootLoadReason::EmbeddedAssetMissing` if unavailable). Precedence: flag > env > default.
5. **Rekor public key for SET verify** = fetched online from `--rekor-url/api/v1/log/publicKey` (matches the fake's served endpoint + makes the 503→exit-83 path reachable). Documented v1 simplification; trust-root Rekor key deferred to #196.
6. **`--format json`** = the GLOBAL flag; sign/verify report through `context.api()`; success via `render_success_envelope`, error via `render_error_envelope` (both already implemented). No subcommand `--format`/`--json`.
7. **Error taxonomy REUSE** — SignErrorKind / VerifyErrorKind already shipped + classified + envelope-wired. No new exit codes (64/65/77/78/79/80/83/84 in use). Delete `SignErrorKind::PipelinePending` usage once pipeline lands (keep variant).

## Verify data shape (test-authoritative, flat — overrides ADR `signatures[]`)

`data = { subject_digest, referrer_digest, certificate_identity, certificate_oidc_issuer }` (all `sha256:`/string). Sign `data = { subject_digest, bundle_digest, referrer_digest, ... }`.

## Module fill order (dependency-ordered)

1. **`oci/endpoint.rs`** — lift `UrlRejection` + `validate_sigstore_url` + URL consts from `oci/sign/endpoint.rs` (ADR Am.2). Update imports in sign/* + verify/*. (Low risk; do first.)
2. **`oci/referrer/manifest.rs`** — `ReferrerManifest::build` + `to_canonical_json` (shape already defined; empty config consts present).
3. **`oci/client/native_transport.rs`** — `list_referrers` + `push_referrer_manifest` real HTTP (OCI 1.1 Referrers API + `?artifactType=` filter). **[delegatable]**
4. **`oci/sign/signer.rs` + `fulcio.rs` + `rekor.rs` + `bundle.rs`** — keygen/CSR/Fulcio POST, Rekor POST, bundle build.
5. **`oci/sign/oidc.rs`** (+ ambient/browser) — token dispatch: override(file/stdin/env) → ambient → browser → OidcPreCheckFailed(77). `--no-tty` suppresses browser.
6. **`oci/sign/pipeline.rs`** — 15-step push state machine; wire capability cache (`no_cache` seam) → ReferrersUnsupported(84).
7. **`oci/verify/trust_root.rs`** — load TrustedRoot JSON from `--trust-root`/env; extract Fulcio CA.
8. **`oci/verify/identity.rs`** — SAN + issuer-OID (`1.3.6.1.4.1.57264.1.1`) extraction + exact match.
9. **`oci/verify/pipeline.rs`** — 7-step verify; capability cache; Rekor SET check; classify failures.
10. **CLI**: `command/package_sign.rs` (wire real pipeline), `command/verify.rs` + `options/verify.rs` (add `--trust-root`), `api/data/signature.rs` + verification report.

## Test wiring (un-skip + align fake stack) — `test/`

- `fake_sigstore.py`: emit a `TrustedRoot` JSON (`trust_root_json_path()`); wire `set_invalid_chain` (leaf signed by a non-advertised key), `set_tampered_set` (bad SET at sign time), `set_failure_mode(HttpStatus(503))` (503 on GET publicKey).
- `test_sign.py`/`test_verify.py`: remove `xfail`; verify tests set `OCX_SIGSTORE_TRUST_ROOT` in env. Fix `test_sign_referrers_unsupported_exits_84` to target `legacy_registry` (registry:2), not zot (#195 finding).
- Acceptance registry: run on alt port (5000 squatted) via harness env override.
- Stale-binary trap: rebuild `--features ocx/__testing` + copy to `test/bin/ocx` before pytest.

## Honesty gates

- Positive path only against `fake_sigstore.py`. Real-network Fulcio/Rekor/TUF stays `#[ignore]`, documented, never claimed green.
- `#[ignore]` structural tests in `sign/pipeline.rs` (no-fallback-tag) flipped + real.
