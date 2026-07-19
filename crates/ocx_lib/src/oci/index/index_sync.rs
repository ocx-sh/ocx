// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Separately-consumable local-index refresh seam (`adr_index_indirection.md`
//! Decision H).
//!
//! [`IndexSync`] is a thin wrapper around [`LocalIndex`]'s two write paths —
//! per-package tag refresh and per-source catalog digest-diff — named as its
//! own component so a future consumer (a search/TUI, `ocx-mcp`, an alternate
//! index root) can take [`LocalIndex`] + `IndexSync` directly for read-only
//! browsing and bulk refresh, without dragging in the remote-resolution
//! chain ([`super::chained_index::ChainedIndex`]). No daemon, no scheduling
//! policy lives here — auto-refresh is a policy seam layered on top,
//! deliberately not built yet.

use crate::{Result, oci};

use super::{CatalogSyncOutcome, Index, LocalIndex, OcxIndex};

/// Keeps one source's local index copy current.
///
/// Wraps [`LocalIndex::refresh_tags`] (per-package dispatch objects + root
/// document) and [`LocalIndex::sync_catalog`] (per-source catalog
/// conditional-GET digest-diff) — the two write paths `ocx index update`
/// piggybacks together. Carries no policy of its own: callers decide when
/// and what to refresh.
#[derive(Clone)]
pub struct IndexSync {
    local_index: LocalIndex,
}

impl IndexSync {
    pub fn new(local_index: LocalIndex) -> Self {
        Self { local_index }
    }

    /// Refreshes one package's local copy against `source` — writes its
    /// dispatch object(s) plus the tag -> digest root document
    /// (`adr_index_indirection.md` A2/A3). See [`LocalIndex::refresh_tags`]
    /// for the published-vs-derived write order.
    ///
    /// # Errors
    ///
    /// Propagates `source`'s fetch failures and any local-write failure.
    pub async fn refresh_package(&self, identifier: &oci::Identifier, source: &Index) -> Result<()> {
        self.local_index.refresh_tags(identifier, source).await
    }

    /// Conditional-GET-diffs `source`'s catalog against the persisted copy
    /// and re-snapshots only the packages whose root moved
    /// (`adr_index_indirection.md` F2). See [`LocalIndex::sync_catalog`] for
    /// the full read-diff-reconcile contract.
    ///
    /// # Errors
    ///
    /// Propagates transport failures reaching the catalog and any local
    /// write/lock failure while reconciling the catalog + ETag.
    pub async fn sync_catalog(&self, source: &OcxIndex) -> Result<CatalogSyncOutcome> {
        self.local_index.sync_catalog(source).await
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tempfile::TempDir;

    use super::*;
    use crate::file_structure::IndexStore;
    use crate::oci::index::{IndexImpl, IndexOperation, LocalConfig};
    use crate::oci::{Algorithm, ImageManifest, Manifest};

    const REGISTRY: &str = "registry.example";
    const REPOSITORY: &str = "repo";

    fn store(dir: &TempDir) -> IndexStore {
        IndexStore::new(dir.path().join("index"))
    }

    fn make_sync(dir: &TempDir) -> IndexSync {
        IndexSync::new(LocalIndex::new(LocalConfig {
            snapshot_store: store(dir),
        }))
    }

    /// Minimal fake serving one tag as a flat image manifest — just enough
    /// for `refresh_package` to reach `LocalIndex::refresh_tags`'s write
    /// path. Kept local to this module (DAMP) rather than reusing
    /// `local_index`'s own richer fixture.
    #[derive(Clone)]
    struct FakeSource;

    #[async_trait]
    impl IndexImpl for FakeSource {
        async fn list_repositories(&self, _registry: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            _identifier: &oci::Identifier,
            _op: IndexOperation,
        ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
            let manifest = Manifest::Image(ImageManifest::default());
            let bytes = serde_json::to_vec(&manifest)?;
            let digest = Algorithm::Sha256.hash(&bytes);
            Ok(Some((digest, manifest)))
        }
        async fn fetch_manifest_digest(
            &self,
            _identifier: &oci::Identifier,
            _op: IndexOperation,
        ) -> Result<Option<oci::Digest>> {
            let manifest = Manifest::Image(ImageManifest::default());
            let bytes = serde_json::to_vec(&manifest)?;
            Ok(Some(Algorithm::Sha256.hash(&bytes)))
        }
        async fn fetch_blob(&self, _blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_package_delegates_to_local_index_write_path() {
        // Trivial-delegation check (ponytail: the wrapper is a one-line
        // passthrough): proves `IndexSync::refresh_package` actually reaches
        // `LocalIndex::refresh_tags`'s write path rather than being a no-op —
        // the deep write-order/merge semantics are `local_index`'s own tests.
        let dir = TempDir::new().unwrap();
        let sync = make_sync(&dir);
        let source = Index::from_impl(FakeSource);
        let identifier = oci::Identifier::new_registry(REPOSITORY, REGISTRY).clone_with_tag("1.0");

        sync.refresh_package(&identifier, &source).await.unwrap();

        assert!(
            store(&dir).root_document_path(REGISTRY, REPOSITORY).exists(),
            "refresh_package must persist a root document via LocalIndex::refresh_tags"
        );
    }
}
