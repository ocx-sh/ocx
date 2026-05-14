// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sign pipeline — 15-step push state machine.
//!
//! Per
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md):
//! resolve identifier → acquire OIDC token → generate ephemeral key → Fulcio
//! CSR → signed cert → sign target digest → Rekor upload → bundle → push bundle
//! blob → push referrer manifest → emit [`SignResult`].
//!
//! The pipeline is a thin orchestrator: the cryptographic signing is
//! delegated to a [`Signer`] trait object (injected via
//! [`SignContext::signer`]), and the registry writes go through the
//! [`OciTransport`] also injected via the context.
//!
//! Phase 5c stub — bodies use `unimplemented!()`.

use url::Url;

use super::error::SignError;
use super::oidc::TokenProvider;
use super::signer::Signer;
use crate::oci::client::OciTransport;
use crate::oci::index::Index;
use crate::oci::{Descriptor, Identifier, Platform};

/// Context passed into [`SignPipeline::run`] — all external dependencies.
///
/// Every field is a `&` borrow so the context does not take ownership of the
/// injected components. The `fulcio_url` / `rekor_url` fields are the
/// C-S1-3 injection seams: production callers use the default Sigstore URLs,
/// tests inject `http://127.0.0.1:<port>` of the fake helpers.
pub struct SignContext<'a> {
    /// Target identifier (`registry/repo:tag[@digest]`).
    pub identifier: &'a Identifier,
    /// Platform selector for multi-platform manifests.
    pub platform: &'a Platform,
    /// Signer producing the cryptographic bundle.
    pub signer: &'a dyn Signer,
    /// OIDC token provider (used by the signer; surfaced here for diagnostics).
    pub token_provider: &'a dyn TokenProvider,
    /// When true, bypass the referrers-capability cache.
    pub no_cache: bool,
    /// Registry transport.
    pub transport: &'a dyn OciTransport,
    /// Index for resolving tag → digest.
    pub index: &'a Index,
    /// Fulcio URL (C-S1-3 injection seam).
    ///
    /// Default: `https://fulcio.sigstore.dev/api/v2/signingCert`.
    pub fulcio_url: &'a Url,
    /// Rekor URL (C-S1-3 injection seam).
    ///
    /// Default: `https://rekor.sigstore.dev`.
    pub rekor_url: &'a Url,
}

/// Result emitted by a successful sign pipeline run.
pub struct SignResult {
    /// Digest of the target manifest the signature was attached to.
    pub subject_digest: crate::oci::Digest,
    /// Digest of the pushed Sigstore bundle blob.
    pub bundle_digest: crate::oci::Digest,
    /// Typed digest of the pushed referrer manifest.
    ///
    /// This is the canonical typed field consumed by the CLI layer (e.g.,
    /// `SignatureReport::new`). The full [`Descriptor`] is retained in
    /// `referrer_descriptor` for cases that need size / media-type metadata.
    pub referrer_digest: crate::oci::Digest,
    /// Full OCI descriptor of the pushed referrer manifest (includes digest,
    /// size, and media-type). Kept for callers that need the complete descriptor;
    /// prefer `referrer_digest` when only the digest is required.
    pub referrer_descriptor: Descriptor,
    /// Cert SAN (identity) that signed the target.
    pub certificate_identity: String,
    /// Cert issuer (`--certificate-oidc-issuer` comparand).
    pub certificate_oidc_issuer: String,
}

/// Sign pipeline entry point.
pub struct SignPipeline;

impl SignPipeline {
    /// Run the 15-step sign pipeline.
    pub async fn run(_ctx: SignContext<'_>) -> Result<SignResult, SignError> {
        unimplemented!("SignPipeline::run — Phase 5 implements the 15-step push state machine")
    }
}

#[cfg(test)]
mod tests {
    use std::panic;

    use async_trait::async_trait;

    use super::*;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::index::{Index, IndexImpl, IndexOperation};

    /// Minimal no-op [`IndexImpl`] for stub tests.
    ///
    /// The pipeline panics before ever calling index methods, so this
    /// implementation just needs to satisfy the trait bounds.
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

    /// Assert that [`SignPipeline::run`] panics with the Phase-5c stub message.
    ///
    /// Protects against the stub being silently removed (which would turn
    /// "unimplemented" into "no-op success") before the real pipeline is wired.
    #[test]
    #[allow(clippy::result_large_err)] // test uses catch_unwind; large Err is expected and intentional
    fn pipeline_stub_is_unimplemented() {
        // Because `unimplemented!()` fires at the top of the async fn before
        // any await point, polling the Future once is sufficient.  Use
        // `catch_unwind` to assert the panic message.
        let result = panic::catch_unwind(|| {
            let data = StubTransportData::new();
            let transport = StubTransport::new(data);

            let identifier = Identifier::parse("registry.example/pkg:1.0").expect("parse");
            let platform: Platform = "linux/amd64".parse().expect("platform");
            let signer = super::super::signer::KeylessSigner::new();
            let token_provider = super::super::oidc::DispatchingTokenProvider::new(None, true);
            let fulcio_url = Url::parse("https://fulcio.sigstore.dev").expect("url");
            let rekor_url = Url::parse("https://rekor.sigstore.dev").expect("url");
            let index = Index::from_impl(NeverIndex);

            let ctx = SignContext {
                identifier: &identifier,
                platform: &platform,
                signer: &signer,
                token_provider: &token_provider,
                no_cache: false,
                transport: &transport,
                index: &index,
                fulcio_url: &fulcio_url,
                rekor_url: &rekor_url,
            };

            let rt = tokio::runtime::Builder::new_current_thread().build().expect("rt");
            rt.block_on(SignPipeline::run(ctx))
        });

        let panic_payload = match result {
            Ok(_) => panic!("SignPipeline::run must panic (Phase 5c stub) — but it returned Ok"),
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

    /// ADR S1-F structural test: no fallback-tag PUTs.
    ///
    /// OCX must never write `sha256-<hex>.sig` or `sha256-<hex>.att` tags on
    /// the push side — fallback-tag writes are banned per ADR S1-F. Registries
    /// that do not support the OCI Referrers API hard-fail with
    /// `SignErrorKind::ReferrersUnsupported` (exit 83) instead.
    ///
    /// This test is **ignored** until Phase 5c wires `SignPipeline::run`.
    /// When Phase 5c lands:
    /// 1. Remove the `#[ignore]` attribute.
    /// 2. Construct a `StubTransportData` with `capture_pushes = true`.
    /// 3. Run the pipeline against a pre-seeded registry state.
    /// 4. Walk `data.read().manifests` and assert no key matches
    ///    `^sha256-[0-9a-f]{64}\.(sig|att)$` (ADR S1-F invariant).
    #[test]
    #[ignore = "Phase 5c: SignPipeline::run is unimplemented — flip to active when pipeline is wired"]
    fn push_sequence_emits_no_fallback_tags() {
        // Phase 5c implementation sketch:
        //
        //   let data = StubTransportData::new();
        //   data.write().capture_pushes = true;
        //   // ... seed subject manifest + run pipeline successfully ...
        //   let inner = data.read();
        //   for key in inner.manifests.keys() {
        //       let is_fallback = key.starts_with("sha256-")
        //           && (key.ends_with(".sig") || key.ends_with(".att"))
        //           && key.len() == "sha256-".len() + 64 + ".sig".len();
        //       assert!(!is_fallback, "ADR S1-F violation: fallback tag in manifests: {key}");
        //   }
    }
}
