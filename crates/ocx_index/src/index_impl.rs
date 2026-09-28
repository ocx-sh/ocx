// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use super::error::Result;

use super::IndexOperation;

#[async_trait]
pub trait IndexImpl: Send + Sync {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>>;

    /// List all tags for the given identifier, reserved tags included; those are
    /// filtered once, in [`Index::list_tags`](super::Index::list_tags).
    async fn list_tags(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>>;

    /// Fetch the manifest for the given identifier.
    ///
    /// Pure-read callers must pass [`IndexOperation::Query`]; a `Resolve` from a
    /// read path silently writes into the local index.
    async fn fetch_manifest(
        &self,
        identifier: &ocx_oci::PackageRef,
        op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>>;
    /// Fetch the manifest digest for the given identifier.
    ///
    /// `op` carries the same contract as on [`Self::fetch_manifest`].
    async fn fetch_manifest_digest(
        &self,
        identifier: &ocx_oci::PackageRef,
        op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>>;

    /// Fetch the raw bytes of a content blob; `Ok(None)` is an unrecoverable miss.
    async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>>;

    /// Fetch the verbatim, digest-verified manifest bytes with the parsed manifest
    /// and its digest; `Ok(None)` = absent.
    ///
    /// The default re-serialises, which breaks digest verification on write: every
    /// source a persisting caller can reach must override it.
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        match self.fetch_manifest(identifier, IndexOperation::Resolve).await? {
            Some((digest, manifest)) => {
                let bytes = serde_json::to_vec(&manifest)?;
                Ok(Some((bytes, digest, manifest)))
            }
            None => Ok(None),
        }
    }

    /// Fetch a published root document's exact `p/<ns>/<pkg>.json` bytes with the
    /// parsed [`IndexRoot`](super::IndexRoot); `Ok(None)` for a derived source,
    /// never for "outside jurisdiction" (see [`super::Jurisdiction`]).
    async fn fetch_root_document(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, super::IndexRoot)>> {
        let _ = identifier;
        Ok(None)
    }

    /// The physical transport identifier a root's `repository` pointer rewrites
    /// `identifier` to; `Ok(None)` = no rewrite.
    ///
    /// Transport-only: never round-tripped into a storage path or lock.
    async fn physical_reference(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        let _ = identifier;
        Ok(None)
    }

    /// [`Self::physical_reference`] from local state alone, for callers downloading
    /// nothing; no source is ever contacted.
    async fn physical_reference_local(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<ocx_oci::OciIdentifier>> {
        let _ = identifier;
        Ok(None)
    }

    /// Record the routing pointer for `identifier` in local state; best-effort.
    ///
    /// Call from the resolve path only: a reader (`cascade check`, `verify`, `sign`)
    /// calling it would pin a physical address the next invocation routes by.
    async fn record_routing_pointer(&self, identifier: &ocx_oci::PackageRef) {
        let _ = identifier;
    }

    /// Whether this source answers for `identifier` and what its silence means,
    /// asked before fetching (no I/O).
    ///
    /// An [`Authoritative`](super::Jurisdiction::Authoritative) source's refusal and
    /// miss both stop the chain walk; falling through would bypass the refusal.
    fn jurisdiction(&self, identifier: &ocx_oci::PackageRef) -> super::Jurisdiction {
        let _ = identifier;
        super::Jurisdiction::FallThrough
    }

    /// This source's `[registries."<ns>"].trusted_hosts` SSRF exemption; empty guards every host.
    fn trusted_hosts(&self) -> &[String] {
        &[]
    }

    /// [`Self::trusted_hosts`] of the source that owns `registry`, keyed on
    /// ownership, never on per-name jurisdiction.
    fn trusted_hosts_for(&self, registry: &str) -> &[String] {
        let _ = registry;
        self.trusted_hosts()
    }

    /// Replaces the proxy-route rules this source's SSRF guard reads — a test
    /// seam, so a guard test never depends on the host's real `HTTPS_PROXY`.
    fn set_proxy_rules(&mut self, rules: std::sync::Arc<ocx_oci::ssrf::ProxyRules>) {
        let _ = rules;
    }

    /// The `OCX_INSECURE_REGISTRIES` authorities this index was built with, which
    /// [`Index::guard_physical_dial`](super::Index::guard_physical_dial) needs to pick the proxy scheme.
    fn insecure_hosts(&self) -> &[String] {
        &[]
    }

    /// Whether this source is the configured owner of `registry` (no I/O);
    /// ownership, not per-name jurisdiction, decides local-subtree layout.
    fn serves_registry(&self, registry: &str) -> bool {
        let _ = registry;
        false
    }

    /// The effective static-file base URL of a configured ocx-index, named by an
    /// authoritative terminal miss.
    fn index_base_url(&self) -> Option<&str> {
        None
    }

    /// The refusal for a name config assigns to an index with no source present
    /// (`--offline` builds none); `None` lets [`super::Index::route`] pass it through.
    fn refuse_unrouted(&self, _identifier: &ocx_oci::PackageRef) -> Option<super::error::Error> {
        None
    }

    /// The base URL of the configured index authoritative for `identifier`, if any.
    fn authoritative_index_base_url(&self, identifier: &ocx_oci::PackageRef) -> Option<&str> {
        match self.jurisdiction(identifier) {
            super::Jurisdiction::Authoritative => self.index_base_url(),
            super::Jurisdiction::FallThrough | super::Jurisdiction::Outside => None,
        }
    }

    /// This source's provenance, which decides the root-read catalog cross-check and
    /// root authorship on growth — never recovery routing.
    fn source_kind(&self) -> super::local_index::SourceKind {
        super::local_index::SourceKind::Derived
    }

    fn box_clone(&self) -> Box<dyn IndexImpl>;

    /// A view that resolves identically but writes nothing into the local index;
    /// blob-store writes still happen, since that store is a GC-able cache.
    fn read_only_view(&self) -> Box<dyn IndexImpl> {
        self.box_clone()
    }

    /// A view that reads live from the sources regardless of the ambient
    /// [`ChainMode`](super::ChainMode) and writes nothing, so it can never move a pin.
    fn remote_view(&self) -> Box<dyn IndexImpl> {
        self.box_clone()
    }
}
