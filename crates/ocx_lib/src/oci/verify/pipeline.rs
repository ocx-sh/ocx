// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Verify pipeline — full keyless Sigstore verification state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md):
//! list referrers → pick v0.3 bundle → parse → verify cert chain vs TUF root
//! → verify Rekor SET → verify signature over subject digest → identity/issuer
//! match → emit [`VerifyResult`].
//!
//! The pipeline's `rekor_url` + `trust_root` injection seams (C-S1-3) let
//! unit tests and acceptance tests share the same test doubles.
//!
//! Phase 5c stub — bodies use `unimplemented!()`.

use url::Url;

use super::error::VerifyError;
use super::trust_root::TrustRoot;
use crate::oci::client::OciTransport;
use crate::oci::index::Index;
use crate::oci::{Identifier, Platform};

/// Context passed into [`VerifyPipeline::run`] — all external dependencies.
///
/// `trust_root` is a `&TrustRoot` per C-S1-3: the CLI layer resolves the
/// trust root (production → [`TrustRoot::load_embedded`]; tests →
/// [`TrustRoot::load_from_pem`] against the fake_fulcio root) and passes a
/// borrowed reference so unit and acceptance tests share the same seam.
///
/// `rekor_url` mirrors [`super::super::sign::pipeline::SignContext::rekor_url`]
/// so sign-then-verify tests can target the same fake helper.
pub struct VerifyContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Platform selector for multi-platform manifests.
    pub platform: &'a Platform,
    /// Expected certificate SAN (user flag).
    pub certificate_identity: &'a str,
    /// Expected certificate OIDC issuer (user flag).
    pub certificate_oidc_issuer: &'a str,
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// Registry transport.
    pub transport: &'a dyn OciTransport,
    /// Index for resolving tag → digest.
    pub index: &'a Index,
    /// Trust root (resolved by the CLI layer; C-S1-3 injection seam).
    pub trust_root: &'a TrustRoot,
    /// Rekor URL (C-S1-3 injection seam). Default: `https://rekor.sigstore.dev`.
    pub rekor_url: &'a Url,
}

/// Result emitted by a successful verify pipeline run.
pub struct VerifyResult {
    /// Digest of the subject manifest that was verified.
    pub subject_digest: crate::oci::Digest,
    /// Digest of the referrer manifest (the bundle referrer).
    pub referrer_digest: crate::oci::Digest,
    /// Cert SAN that signed the subject.
    pub certificate_identity: String,
    /// Cert OIDC issuer URL.
    pub certificate_oidc_issuer: String,
    /// Rekor integrated time (UTC epoch seconds) of the signature entry.
    pub signed_at: u64,
    /// True when the signing cert had expired by verify time but Rekor SET
    /// attests it was integrated pre-expiry (so verification still succeeds).
    pub cert_expired_but_tlog_valid: bool,
}

/// Verify pipeline entry point.
pub struct VerifyPipeline;

impl VerifyPipeline {
    /// Run the verify pipeline against a [`VerifyContext`].
    ///
    /// # Phase 5c canonicalized-body cross-checks
    ///
    /// The implementation MUST cross-check the following Sigstore bundle fields
    /// against the on-chain Rekor payload to close the GHSA-whqx-f9j3-ch6m class
    /// of signature-substitution attacks (cosign advisory, 2022):
    ///
    /// - `spec.signature.publicKey` → must equal the Fulcio cert subject public key.
    ///   A bundle carrying a cert/pubkey pair that does not match the SET entry
    ///   proves a different key signed the artifact, not the Fulcio-issued one.
    ///
    /// - `spec.data.hash.algorithm` + `spec.data.hash.value` → must equal the
    ///   subject manifest digest (`VerifyContext::subject_digest`). An attacker who
    ///   replaces a valid bundle alongside a different manifest image exploits the
    ///   gap between "cert verifies" and "cert verifies *this* digest". Exit 65
    ///   (`DataError`) on mismatch — tampered data, not a transient fault.
    ///
    /// Both checks must precede the final `VerifyResult` emission.
    pub async fn run(_ctx: VerifyContext<'_>) -> Result<VerifyResult, VerifyError> {
        unimplemented!("VerifyPipeline::run — Phase 5 implements the full verify state machine")
    }
}

#[cfg(test)]
mod tests {
    use std::panic;

    use async_trait::async_trait;

    use super::*;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::index::{Index, IndexImpl, IndexOperation};
    use crate::oci::verify::trust_root::TrustRoot;

    // A synthetic CERTIFICATE PEM block whose body is the smallest non-empty
    // DER SEQUENCE accepted by `validate_der_certificate`: `SEQUENCE { INTEGER 1 }`
    // = [0x30, 0x03, 0x02, 0x01, 0x01]. Mirrors the constant in trust_root tests.
    const MINIMAL_CERT_PEM: &[u8] = b"\
-----BEGIN CERTIFICATE-----\n\
MAMCAQE=\n\
-----END CERTIFICATE-----\n";

    /// Minimal no-op [`IndexImpl`] for stub tests.
    ///
    /// The pipeline panics before calling any index methods, so this
    /// implementation only needs to satisfy trait bounds.
    struct NeverIndex;

    #[async_trait]
    impl IndexImpl for NeverIndex {
        async fn list_repositories(&self, _registry: &str) -> crate::Result<Vec<String>> {
            unimplemented!("NeverIndex: not used in stub tests")
        }

        async fn list_tags(&self, _id: &crate::oci::Identifier) -> crate::Result<Option<Vec<String>>> {
            unimplemented!("NeverIndex: not used in stub tests")
        }

        async fn fetch_manifest(
            &self,
            _id: &crate::oci::Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(crate::oci::Digest, crate::oci::Manifest)>> {
            unimplemented!("NeverIndex: not used in stub tests")
        }

        async fn fetch_manifest_digest(
            &self,
            _id: &crate::oci::Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<crate::oci::Digest>> {
            unimplemented!("NeverIndex: not used in stub tests")
        }

        async fn fetch_blob(&self, _blob_ref: &crate::oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            unimplemented!("NeverIndex: not used in stub tests")
        }

        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(NeverIndex)
        }
    }

    /// Assert that [`VerifyPipeline::run`] panics with the Phase-5c stub message.
    ///
    /// Mirrors `pipeline_stub_is_unimplemented` on the sign side. Protects
    /// against the stub being silently removed before the real pipeline is wired.
    #[test]
    #[allow(clippy::result_large_err)]
    fn verify_pipeline_stub_is_unimplemented() {
        // `unimplemented!()` fires at the top of the async fn before any await
        // point, so polling the Future once via `block_on` is sufficient.
        let result = panic::catch_unwind(|| {
            let data = StubTransportData::new();
            let transport = StubTransport::new(data);
            let identifier = Identifier::parse("registry.example/pkg:1.0").expect("parse");
            let platform: Platform = "linux/amd64".parse().expect("platform");
            let trust_root = TrustRoot::load_from_pem(MINIMAL_CERT_PEM).expect("trust root");
            let rekor_url = Url::parse("https://rekor.sigstore.dev").expect("url");
            let index = Index::from_impl(NeverIndex);

            let ctx = VerifyContext {
                identifier: &identifier,
                platform: &platform,
                certificate_identity: "test@example.com",
                certificate_oidc_issuer: "https://accounts.example.com",
                no_cache: false,
                transport: &transport,
                index: &index,
                trust_root: &trust_root,
                rekor_url: &rekor_url,
            };

            let rt = tokio::runtime::Builder::new_current_thread().build().expect("rt");
            rt.block_on(VerifyPipeline::run(ctx))
        });

        let panic_payload = match result {
            Ok(_) => panic!("VerifyPipeline::run must panic (Phase 5c stub) — but it returned Ok"),
            Err(payload) => payload,
        };
        let msg = panic_payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic_payload.downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic>");
        assert!(
            msg.contains("Phase 5"),
            "expected Phase-5 stub panic message, got: {msg}"
        );
    }

    /// Bundle whose signature targets a different digest than the subject must be rejected.
    ///
    /// Guards against the signature-substitution attack class (CWE-345): an
    /// attacker presents a valid Sigstore bundle for digest A alongside a manifest
    /// at digest B. Phase 5c must cross-check `spec.data.hash` == subject digest.
    ///
    /// Ignored until Phase 5c wires `VerifyPipeline::run`. When Phase 5c lands:
    /// 1. Remove `#[ignore]`.
    /// 2. Construct a `VerifyContext` whose trust root + Rekor stub accept the bundle.
    /// 3. Seed the transport with a subject manifest at `subject_digest`.
    /// 4. Forge a bundle whose `spec.data.hash.value` differs from `subject_digest`.
    /// 5. Assert the pipeline returns `Err(VerifyErrorKind::SignatureInvalid)` or
    ///    `Err(VerifyErrorKind::RekorSetInvalid)` — NOT `Ok(VerifyResult)`.
    #[test]
    #[ignore = "phase 5c — VerifyPipeline::run is unimplemented; flip active when pipeline is wired"]
    fn bundle_message_signature_must_bind_to_subject_digest() {
        // Phase 5c implementation sketch:
        //
        //   let subject_digest = "sha256:aaaaaa...".parse().expect("digest");
        //   let forged_bundle_digest = "sha256:bbbbbb..."; // different digest
        //   // Build a StubTransport seeded with:
        //   //   - subject manifest at subject_digest
        //   //   - referrer manifest pointing to a bundle blob whose
        //   //     spec.data.hash.value == forged_bundle_digest
        //   // Run VerifyPipeline::run(ctx).await and assert Err(SignatureInvalid
        //   // | RekorSetInvalid) — the pipeline must reject digest mismatch.
    }

    /// ADR S1-F: no fallback-tag reads must occur during verify.
    ///
    /// Verify must list referrers via the OCI Referrers API only (no
    /// `sha256-<hex>.sig` / `.att` tag reads). Ignored until Phase 5c wires the
    /// pipeline — flip active and assert no tag-list calls match the fallback
    /// pattern in `data.read().calls`.
    #[test]
    #[ignore = "phase 5c — VerifyPipeline::run is unimplemented; flip active when pipeline is wired"]
    fn verify_sequence_reads_no_fallback_tags() {
        // Phase 5c implementation sketch:
        //
        //   let data = StubTransportData::new();
        //   // ... seed referrer manifest + run pipeline ...
        //   let inner = data.read();
        //   for call in &inner.calls {
        //       let is_fallback = call.contains("sha256-")
        //           && (call.contains(".sig") || call.contains(".att"));
        //       assert!(!is_fallback, "ADR S1-F violation: fallback tag read: {call}");
        //   }
    }
}
