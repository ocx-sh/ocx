// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `verify_one` — the single lib-level keyless-Sigstore verify entry point.
//!
//! See [`subsystem-package-manager.md`](../../../../../.claude/rules/subsystem-package-manager.md).

use url::Url;

use crate::error::{PackageError, PackageErrorKind};
use ocx_oci::{self};
use ocx_sign::sign::SignatureFormat;
use ocx_sign::verify::pipeline::VerifyResult;
use ocx_sign::verify::{TrustRoot, VerifyContentMode, VerifyContext, VerifyError, VerifyPipeline};
use ocx_store::file_structure::StateStore;
use ocx_trust::CompiledPolicy;

use super::super::PackageManager;

/// External dependencies forwarded to [`PackageManager::verify_one`].
pub struct VerifyOptions<'a> {
    /// Resolved ANY-of policies the signing certificate must satisfy.
    pub policies: &'a [CompiledPolicy],
    /// Registry client, present even under `--offline`: verify always reads the artifact and its referrers.
    pub client: &'a ocx_oci::Client,
    /// Trust root (Fulcio CA + optional pinned Rekor key).
    pub trust_root: &'a TrustRoot,
    /// Rekor transparency-log endpoint (default public Rekor).
    pub rekor_url: &'a Url,
    /// No Sigstore trust-service network; the Rekor key must come from pinned or cached trust material.
    pub offline: bool,
    /// State store owning the referrers-capability and trust-root cache layouts.
    pub state: &'a StateStore,
    /// Bypass the referrers-capability cache for this invocation.
    pub no_cache: bool,
    /// Which signed content to look for; not defaulted, so a verify run states what it verifies.
    pub content: VerifyContentMode,
    /// The pinned cosign wire shape, or `None` to prefer a bundle and fall back to a simplesigning sidecar;
    /// a fetched bundle that is refused fails closed rather than falling back.
    pub signature_format: Option<SignatureFormat>,
    /// Accept a keyless simplesigning sidecar with no transparency-log evidence (`--allow-unlogged-signature`).
    pub allow_unlogged_signature: bool,
    /// Collect every verified candidate, not just the first; widens the report, never the verdict.
    pub report_all: bool,
}

/// Success payload returned by [`PackageManager::verify_one`].
pub struct VerifyReport {
    /// Every signature that verified, in scan order, deduplicated.
    ///
    /// Never empty; the first element is the verdict under either [`VerifyOptions::report_all`] setting.
    pub signatures: Vec<VerifyResult>,
}

impl PackageManager {
    /// Verify `package` against `opts.trust_root`, requiring the signing certificate to satisfy one of
    /// `opts.policies`; a `Some` platform requires an index, `None` acts on whatever resolved.
    ///
    /// # Errors
    /// A [`PackageError`] tagged with `package`, classified via [`ocx_sign::verify::VerifyErrorKind`].
    pub async fn verify_one(
        &self,
        package: &ocx_oci::PackageRef,
        platform: Option<&ocx_oci::Platform>,
        opts: VerifyOptions<'_>,
    ) -> Result<VerifyReport, PackageError> {
        // Read-only view: verifying must never grow the permanent local index (`adr_index_indirection.md`).
        let mgr = self.read_only_view();
        let resolve = super::resolve_subject::verify_resolver(mgr.index());
        let state = ocx_sign::sign::state::SigningStatePaths::new(opts.state.root());
        let context = VerifyContext {
            identifier: package,
            platform,
            policies: opts.policies,
            no_cache: opts.no_cache,
            dial: mgr.index().dial_policy(package.registry()),
            resolve,
            trust_root: opts.trust_root,
            rekor_url: opts.rekor_url,
            state,
            offline: opts.offline,
            content: opts.content,
            signature_format: opts.signature_format,
            allow_unlogged_signature: opts.allow_unlogged_signature,
            report_all: opts.report_all,
            verification: ocx_sign::verify::VerificationMode::Demand,
        };
        let signatures = VerifyPipeline::run(opts.client, context)
            .await
            .map_err(|err| map_verify_error(package.clone(), err))?;
        Ok(VerifyReport { signatures })
    }
}

/// Wrap a [`VerifyError`] in a [`PackageError`] tagged with `identifier`,
/// preserving the verify exit code through `PackageErrorKind::Internal`.
pub(super) fn map_verify_error(identifier: ocx_oci::PackageRef, err: VerifyError) -> PackageError {
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
    use ocx_index::{ChainMode, Index, IndexImpl, IndexOperation, LocalConfig, LocalIndex, SelectResult};
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use ocx_sign::verify::VerifyErrorKind;
    use ocx_store::file_structure::{FileStructure, StateStore};

    pub(crate) const REGISTRY: &str = "example.com";
    pub(crate) const REPO: &str = "widget";
    const TAG: &str = "1.0";

    pub(crate) fn tagged_id() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }

    // Single-child image index — dispatch-shaped (never a bare leaf manifest),
    // matching the local index's "o/ never holds a leaf manifest" contract
    // (`adr_oci_index_only_dispatch.md`) so a writable resolve can actually
    // persist it. Reuses the fixture shape from
    // `ocx_index::chained_index::chain_refs_tests`.
    fn index_bytes() -> &'static [u8] {
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2,"platform":{"os":"linux","architecture":"amd64"}}]}"#
    }
    pub(crate) fn index_digest() -> ocx_oci::Digest {
        ocx_oci::Algorithm::Sha256.hash(index_bytes())
    }
    fn index_manifest() -> ocx_oci::Manifest {
        serde_json::from_slice(index_bytes()).unwrap()
    }

    /// A remote source serving exactly one tag → one dispatch-shaped manifest.
    #[derive(Clone)]
    pub(crate) struct SingleTagSource;

    #[async_trait]
    impl IndexImpl for SingleTagSource {
        async fn list_repositories(&self, _registry: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _identifier: &ocx_oci::PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            identifier: &ocx_oci::PackageRef,
            _op: IndexOperation,
        ) -> ocx_index::error::Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
            Ok((identifier.tag_or_latest() == TAG).then(|| (index_digest(), index_manifest())))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &ocx_oci::PackageRef,
            _op: IndexOperation,
        ) -> ocx_index::error::Result<Option<ocx_oci::Digest>> {
            Ok((identifier.tag_or_latest() == TAG).then(index_digest))
        }
        async fn fetch_blob(&self, _blob_ref: &ocx_oci::PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &ocx_oci::PackageRef,
        ) -> ocx_index::error::Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
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
    /// transport at all. That gate was deleted, so the refusal has to come from
    /// the transport now — and the truthful outcome is `NoSignaturesFound` (79),
    /// not exit 82 (82 is write-path only).
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
        let index_store = ocx_index::IndexStore::machine_local(&file_structure);
        let local_index = LocalIndex::new(LocalConfig {
            index_store: index_store.clone(),
        });
        let source = Index::from_impl(SingleTagSource);
        let index = Index::from_chained(local_index, vec![source], ChainMode::Default);
        let manager = PackageManager::new(file_structure, index, None, REGISTRY);

        let state = StateStore::new(root.path().join("state"));
        let client = ocx_oci::Client::with_transport(Box::new(transport_without_referrers()));
        // Empty material: this test never reaches signature verification.
        let trust_root = TrustRoot::default();
        // Loopback rather than a `.test` name: the verify pipeline resolves the
        // endpoint before use (dial-time SSRF guard), and a reserved-TLD name does
        // not resolve -- which would make this unit test depend on DNS.
        let rekor_url = Url::parse("http://127.0.0.1:3000").unwrap();
        let platform: ocx_oci::Platform = "linux/amd64".parse().unwrap();

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
