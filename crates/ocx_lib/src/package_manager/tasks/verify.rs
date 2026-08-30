// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `verify_one` — the single lib-level keyless-Sigstore verify entry point.
//!
//! Given a resolved OCI identifier, a platform, and the effective ANY-of trust
//! policy set, drive [`crate::oci::verify::VerifyPipeline::run`] (the #194
//! pipeline) with the #196 trust-root/offline material and wrap any
//! [`VerifyError`](crate::oci::verify::VerifyError) in a [`PackageError`] so the
//! caller sees per-package diagnosis and the exit code survives the batch
//! classifier (`PackageErrorKind::Internal` → `classify_error` →
//! `crate::Error::Verify` → `VerifyError::classify`).
//!
//! Consumed by the policy-gated auto-verify hook (see
//! [`super::auto_verify`]). The `ocx package verify` CLI command drives the
//! pipeline directly because it layers flag-vs-policy identity resolution and
//! flag-driven trust-root overrides on top; both ultimately run the same
//! pipeline.
//!
//! Per [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md)
//! and Spec A10 — the aggregator is `package_manager/tasks.rs`.

use url::Url;

use crate::file_structure::StateStore;
use crate::oci::sign::SignatureFormat;
use crate::oci::verify::pipeline::VerifyResult;
use crate::oci::verify::{TrustRoot, VerifyContentMode, VerifyContext, VerifyError, VerifyPipeline};
use crate::oci::{self};
use crate::package_manager::error::{PackageError, PackageErrorKind};
use crate::trust::CompiledPolicy;

use super::super::PackageManager;

/// External dependencies forwarded to [`PackageManager::verify_one`].
///
/// `client` is the **registry** client (present even under `--offline` — verify
/// inherently reads the artifact + its signature referrer from the registry);
/// the manager's own offline-gated client is not used here.
pub struct VerifyOptions<'a> {
    /// Resolved ANY-of policies the signing certificate must satisfy.
    pub policies: &'a [CompiledPolicy],
    /// Registry client (always available, unlike the manager's offline client).
    pub client: &'a oci::Client,
    /// Trust root (Fulcio CA + optional pinned Rekor key); #196 seam.
    pub trust_root: &'a TrustRoot,
    /// Rekor transparency-log endpoint (default public Rekor).
    pub rekor_url: &'a Url,
    /// When true, no Sigstore-trust-services network — the Rekor key must come
    /// from pinned/cached trust material.
    pub offline: bool,
    /// State store owning the referrers-capability and trust-root cache layouts.
    pub state: &'a StateStore,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// Which signed content to look for. [`VerifyContentMode::Signature`] is
    /// what every shipped caller passes and what `ocx package verify` does
    /// without `--attestation`; the attestation modes carry the `--type`
    /// narrowing. Not defaulted — a verify run states what it is verifying.
    pub content: VerifyContentMode,
    /// The cosign wire shape this run pins (`--signature-format`), or `None` to
    /// prefer a bundle and fall back to a simplesigning sidecar when the bundle
    /// shape is absent. A bundle that was fetched and refused is not an absence:
    /// it fails closed with its own exit code.
    pub signature_format: Option<SignatureFormat>,
    /// Accept a keyless simplesigning sidecar carrying no transparency-log
    /// evidence (`--allow-unlogged-signature`). Off is the contract; the
    /// opt-out is for air-gapped CI. See
    /// [`VerifyContext::allow_unlogged_signature`](crate::oci::verify::VerifyContext).
    pub allow_unlogged_signature: bool,
    /// Collect every verified candidate, not just the first.
    ///
    /// Q3: the commands that render `signatures[]` set it; the install-time
    /// auto-verify hook does not, because full crypto per candidate for output
    /// nobody reads is not a cost the install path pays. It widens the report,
    /// never the verdict.
    pub report_all: bool,
}

/// Success payload returned by [`PackageManager::verify_one`].
pub struct VerifyReport {
    /// Every signature that verified, in scan order, deduplicated.
    ///
    /// **Never empty**, and the first element is the verdict — the same
    /// candidate under either [`VerifyOptions::report_all`] setting, because
    /// widening the arity appends behind the answer rather than reordering it.
    /// One field rather than a `result` beside a list: two would be one
    /// assignment away from disagreeing about which signature was the answer.
    pub signatures: Vec<VerifyResult>,
}

impl PackageManager {
    /// Verify `package` for `platform` against `opts.trust_root`, requiring the
    /// signing certificate to satisfy one of `opts.policies` (ANY-of).
    ///
    /// `platform` is `None` when the caller narrowed into nothing: the run acts
    /// on whatever the reference resolved to, index or bare manifest (C-010).
    /// `Some(..)` narrows into an index and is an error when the resolved
    /// object is not one.
    ///
    /// The pipeline is: resolve target → list referrers (capability cache) →
    /// pick the v0.3 bundle → verify cert chain vs trust root → bind signature
    /// to subject digest → verify signature → verify Rekor SET → identity/issuer
    /// match → emit [`VerifyReport`].
    ///
    /// # Errors
    /// Returns [`PackageError`] tagged with `package` on any failure —
    /// exit-code classification routes via
    /// [`crate::oci::verify::VerifyErrorKind`].
    pub async fn verify_one(
        &self,
        package: &oci::Identifier,
        platform: Option<&oci::Platform>,
        opts: VerifyOptions<'_>,
    ) -> Result<VerifyReport, PackageError> {
        // Read-only: verifying a package must never grow the permanent local
        // index (same rationale as `inspect` — `adr_index_indirection.md`).
        // Resolve through a read-only view: content warms the GC-able blob
        // cache, the index stays untouched.
        let mgr = self.read_only_view();
        let context = VerifyContext {
            identifier: package,
            platform,
            policies: opts.policies,
            no_cache: opts.no_cache,
            index: mgr.index(),
            trust_root: opts.trust_root,
            rekor_url: opts.rekor_url,
            state: opts.state,
            offline: opts.offline,
            // Not provable here — the mode's only observable effects (caps
            // selection, the attestation arm of `finish_scan`) sit past the
            // trust-root gate. Closed by S-009/S-016 (WP10b), once
            // `--attestation` lands (WP9b).
            content: opts.content,
            signature_format: opts.signature_format,
            allow_unlogged_signature: opts.allow_unlogged_signature,
            report_all: opts.report_all,
            // `ocx package verify` exists to verify: there is no permissive
            // form of it, and the pipeline's ANY-of entry point never reads
            // this field.
            verification: crate::oci::verify::VerificationMode::Demand,
        };
        let signatures = VerifyPipeline::run(opts.client, context)
            .await
            .map_err(|err| map_verify_error(package.clone(), err))?;
        Ok(VerifyReport { signatures })
    }
}

/// Wrap a [`VerifyError`] in a [`PackageError`] tagged with `identifier`,
/// preserving the verify exit code through `PackageErrorKind::Internal`.
pub(super) fn map_verify_error(identifier: oci::Identifier, err: VerifyError) -> PackageError {
    PackageError::new(
        identifier,
        PackageErrorKind::Internal(crate::Error::Verify(Box::new(err))),
    )
}

// `pub(crate)` under `cfg(test)`: the sibling `sbom` task verifies the same
// read-only routing against the same single-tag index fixture, and a second
// copy of it would be a second thing to keep in step.
#[cfg(test)]
pub(crate) mod tests {
    use async_trait::async_trait;
    use tempfile::TempDir;
    use url::Url;

    use super::*;
    use crate::file_structure::{FileStructure, StateStore};
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::index::{ChainMode, Index, IndexImpl, IndexOperation, LocalConfig, LocalIndex, SelectResult};
    use crate::oci::verify::VerifyErrorKind;

    pub(crate) const REGISTRY: &str = "example.com";
    pub(crate) const REPO: &str = "widget";
    const TAG: &str = "1.0";

    pub(crate) fn tagged_id() -> oci::Identifier {
        oci::Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }

    // Single-child image index — dispatch-shaped (never a bare leaf manifest),
    // matching the local index's "o/ never holds a leaf manifest" contract
    // (`adr_oci_index_only_dispatch.md`) so a writable resolve can actually
    // persist it. Reuses the fixture shape from
    // `oci::index::chained_index::chain_refs_tests`.
    fn index_bytes() -> &'static [u8] {
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2,"platform":{"os":"linux","architecture":"amd64"}}]}"#
    }
    pub(crate) fn index_digest() -> oci::Digest {
        oci::Algorithm::Sha256.hash(index_bytes())
    }
    fn index_manifest() -> oci::Manifest {
        serde_json::from_slice(index_bytes()).unwrap()
    }

    /// A remote source serving exactly one tag → one dispatch-shaped manifest.
    #[derive(Clone)]
    pub(crate) struct SingleTagSource;

    #[async_trait]
    impl IndexImpl for SingleTagSource {
        async fn list_repositories(&self, _registry: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _identifier: &oci::Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            identifier: &oci::Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(oci::Digest, oci::Manifest)>> {
            Ok((identifier.tag_or_latest() == TAG).then(|| (index_digest(), index_manifest())))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &oci::Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<oci::Digest>> {
            Ok((identifier.tag_or_latest() == TAG).then(index_digest))
        }
        async fn fetch_blob(&self, _blob_ref: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &oci::Identifier,
        ) -> crate::Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
            Ok((identifier.tag_or_latest() == TAG).then(|| (index_bytes().to_vec(), index_digest(), index_manifest())))
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A transport standing in for a registry with **no** OCI 1.1 Referrers API
    /// and no fallback referrers tag: `list_referrers` raises
    /// `ClientError::ReferrersUnsupported`, and the tag-schema read that
    /// `list_referrers_with_fallback` falls back to finds nothing
    /// (`pull_manifest_raw` → `ManifestNotFound` → empty index).
    ///
    /// It used to seed a cached "unsupported" capability record instead, so the
    /// pipeline stopped at `ensure_referrers_supported` before touching the
    /// transport at all. D-1 deleted that gate, so the refusal has to come from
    /// the transport now — and the truthful outcome is `NoSignaturesFound` (79),
    /// not exit 84 (C-002: 84 is write-path only).
    pub(crate) fn transport_without_referrers() -> StubTransport {
        let data = StubTransportData::new();
        data.write().referrers_unsupported = true;
        StubTransport::new(data)
    }

    /// `verify_one` must resolve through the read-only view: a bare-tag verify
    /// leaves the committed local index untouched, even though the identical
    /// resolve against the (writable) chained index backing this test WOULD
    /// commit a tag pointer + dispatch object — proven by the positive control
    /// at the end of this test, which is exactly the routing `verify_one` used
    /// before this fix.
    #[tokio::test(flavor = "multi_thread")]
    async fn verify_one_answers_no_signatures_found_and_never_grows_the_local_index() {
        let root = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(root.path().to_path_buf());
        let index_store = file_structure.index.clone();
        let local_index = LocalIndex::new(LocalConfig {
            index_store: index_store.clone(),
        });
        let source = Index::from_impl(SingleTagSource);
        let index = Index::from_chained(local_index, vec![source], ChainMode::Default);
        let manager = PackageManager::new(file_structure, index, None, REGISTRY);

        let state = StateStore::new(root.path().join("state"));
        let client = oci::Client::with_transport(Box::new(transport_without_referrers()));
        // Empty material: this test never reaches signature verification.
        let trust_root = TrustRoot::default();
        // Loopback rather than a `.test` name: the verify pipeline resolves the
        // endpoint before use (dial-time SSRF guard), and a reserved-TLD name does
        // not resolve -- which would make this unit test depend on DNS.
        let rekor_url = Url::parse("http://127.0.0.1:3000").unwrap();
        let platform: oci::Platform = "linux/amd64".parse().unwrap();

        let opts = VerifyOptions {
            policies: &[],
            client: &client,
            trust_root: &trust_root,
            rekor_url: &rekor_url,
            offline: false,
            state: &state,
            no_cache: false,
            content: VerifyContentMode::Signature,
            signature_format: None,
            allow_unlogged_signature: false,
            report_all: false,
        };

        // `VerifyReport`/`VerifyResult` (the `Ok` payload) do not implement
        // `Debug` (owned by `oci::verify::pipeline`, out of scope here), so
        // match explicitly rather than `.expect_err(..)`.
        match manager.verify_one(&tagged_id(), Some(&platform), opts).await {
            Err(err) => match err.kind {
                PackageErrorKind::Internal(crate::Error::Verify(verify_err)) => assert!(
                    matches!(verify_err.kind, VerifyErrorKind::NoSignaturesFound),
                    "expected NoSignaturesFound, got {:?}",
                    verify_err.kind
                ),
                other => panic!("expected Internal(Verify(NoSignaturesFound)), got {other:?}"),
            },
            Ok(_) => panic!("nothing is signed here; verify_one must fail closed"),
        }

        // Discriminating assertion: the committed local index must be
        // untouched — no dispatch object, no tag pointer — despite the
        // successful resolve inside `verify_one`.
        let dispatch = index_store.dispatch_object_path(REGISTRY, REPO, &index_digest());
        assert!(
            !dispatch.exists(),
            "verify_one must not persist a dispatch object into the local index"
        );
        let offline_probe = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: index_store.clone(),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        let tag_pointer = offline_probe
            .fetch_manifest(&tagged_id(), IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            tag_pointer.is_none(),
            "verify_one must not commit a tag pointer; an offline probe found one"
        );

        // Positive control: the SAME resolve, run directly against the
        // writable index `verify_one` routed through before this fix, DOES
        // grow the local index — proving the read-only routing above is
        // load-bearing, not a no-op change.
        let select_result = manager
            .index()
            .select(&tagged_id(), &platform, IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            matches!(select_result, SelectResult::Found(_)),
            "writable resolve must find the single-platform candidate"
        );
        assert!(
            dispatch.exists(),
            "the writable index (verify_one's pre-fix routing) must persist a dispatch object"
        );
        let tag_pointer_after = offline_probe
            .fetch_manifest(&tagged_id(), IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            tag_pointer_after.is_some(),
            "the writable index (verify_one's pre-fix routing) must commit a tag pointer"
        );
    }
}
