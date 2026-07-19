// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use crate::{Result, oci};

use super::IndexOperation;

#[async_trait]
pub trait IndexImpl: Send + Sync {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>>;

    /// List all user-visible tags for the given identifier.
    ///
    /// Internal tags ([`Tag::Internal`](crate::package::tag::Tag::Internal)) must be
    /// filtered out by every implementation.
    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>>;

    /// Fetch the manifest for the given identifier.
    ///
    /// Pure-read callers must pass [`IndexOperation::Query`]; install/pull
    /// callers pass [`IndexOperation::Resolve`]. The trait does not validate
    /// this — misuse silently leaks writes through query paths. The
    /// [`IndexOperation`] enum exists to make the choice unmissable at every
    /// call site.
    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>>;
    /// Fetch the manifest digest for the given identifier.
    ///
    /// `op` carries the same contract as on [`Self::fetch_manifest`].
    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<oci::Digest>>;

    /// Fetch the raw bytes of a content blob.
    ///
    /// `blob_ref` carries `(registry, repo)` for the OCI blob endpoint and
    /// the blob's own digest for content addressing. `Ok(None)` = unrecoverable
    /// miss (e.g. local-only mode + absent).
    async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>>;

    /// Fetch the verbatim manifest bytes alongside the parsed manifest and its
    /// digest.
    ///
    /// A registry-backed source ([`super::OciIndex`]) returns the exact
    /// bytes the registry served — digest recompute-verified — so the index
    /// store can persist them without re-serialisation
    /// (`adr_index_indirection.md` A3). `Ok(None)` = tag/manifest absent.
    ///
    /// The default derives the bytes by re-serialising the parsed manifest.
    /// That fallback is correct only for sources that do not retain the wire
    /// bytes (test fakes that never persist); every source that a persisting
    /// caller can reach overrides it — registry sources, `OcxIndex`, and
    /// `ChainedIndex` itself (which walks its own source chain rather than
    /// falling back to a re-serialisation of its cache-first `fetch_manifest`
    /// read). The persist path ([`super::LocalIndex::persist_dispatch`]) and
    /// chain-blob staging ([`crate::package_manager::tasks::common::stage_and_link_chain_blobs`])
    /// are always driven with a registry-backed source in production, so the
    /// re-serialising default never reaches a verifying write.
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        match self.fetch_manifest(identifier, IndexOperation::Resolve).await? {
            Some((digest, manifest)) => {
                let bytes = serde_json::to_vec(&manifest)?;
                Ok(Some((bytes, digest, manifest)))
            }
            None => Ok(None),
        }
    }

    /// Fetch a published index root document verbatim: the exact
    /// `p/<ns>/<pkg>.json` bytes the index site served, alongside the parsed
    /// [`IndexRoot`](super::IndexRoot).
    ///
    /// A **published** ocx-index source ([`super::OcxIndex`]) serves the
    /// verbatim root so `LocalIndex::persist_published_root` can grow the local
    /// copy byte-for-byte (copy-a-mirror, `adr_index_indirection.md` A2). A
    /// **derived** (plain OCI-registry) source publishes no index of its own, so
    /// the default returns `None` — its root is OCX-authored field-wise instead
    /// (`LocalIndex::commit_root_tag`, A2/H). `Ok(None)` = this source serves no
    /// verbatim root for `identifier`.
    async fn fetch_root_document(&self, identifier: &oci::Identifier) -> Result<Option<(Vec<u8>, super::IndexRoot)>> {
        let _ = identifier;
        Ok(None)
    }

    /// The physical transport identifier for `identifier`, when this source
    /// rewrites a logical reference to a distinct physical location
    /// (`index.ocx.sh`'s `repository` pointer). `Ok(None)` = no rewrite
    /// (registry sources: physical == logical).
    ///
    /// The returned reference is **transport-only** (Decision C2) — used to
    /// fetch layer/manifest content from the registry the index points at, and
    /// never round-tripped into a storage path or lock. The default returns
    /// `None`; only [`super::OcxIndex`] (and `ChainedIndex`, which delegates)
    /// override it.
    async fn physical_reference(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        let _ = identifier;
        Ok(None)
    }

    /// Whether this source is the authoritative resolver for `identifier`'s
    /// namespace.
    ///
    /// An authoritative source's **refusal** (a yanked tag without opt-in, an
    /// observation-object tamper, a fail-closed format mismatch) must stop the
    /// chain walk — it must never fall through to a lower source that could
    /// answer the same name and both bypass the refusal and leak the
    /// induced-error traffic to that source. The default returns `false`; only
    /// [`super::OcxIndex`] overrides it (true for its own namespace).
    fn is_authoritative_for(&self, identifier: &oci::Identifier) -> bool {
        let _ = identifier;
        false
    }

    /// This source's provenance (`adr_index_indirection.md` A2/H — the "two
    /// ifs" that distinguish a published copy from a derived one).
    ///
    /// A cheap, synchronous, no-I/O classification — `ChainedIndex` calls it to
    /// pick [`super::local_index::SourceKind`] for local dispatch-object reads
    /// and AbsentLeaf recovery routing, without needing to contact the source.
    /// The default is [`super::local_index::SourceKind::Derived`] (an OCI
    /// registry publishes no index of its own); only [`super::OcxIndex`]
    /// overrides it (`Published`).
    fn source_kind(&self) -> super::local_index::SourceKind {
        super::local_index::SourceKind::Derived
    }

    fn box_clone(&self) -> Box<dyn IndexImpl>;
}
