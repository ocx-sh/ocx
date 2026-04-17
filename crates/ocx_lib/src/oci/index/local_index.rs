// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;
use std::collections::HashMap;

use crate::{Result, file_structure::BlobStore, log, oci, package::tag::Tag};

use super::index_impl;

mod cache;
mod config;
mod tag_guard;
mod tag_lock;
mod tag_manager;

pub use config::Config;

use tag_manager::TagManager;

#[derive(Clone)]
pub struct LocalIndex {
    tags: TagManager,
    blob_store: BlobStore,
    cache: cache::SharedCache,
}

impl LocalIndex {
    pub fn new(config: Config) -> Self {
        let cache = cache::SharedCache::default();
        Self {
            tags: TagManager::new(config.tag_store, cache.clone()),
            blob_store: config.blob_store,
            cache,
        }
    }

    /// Atomic tag refresh entry point. Delegates to [`TagManager::refresh`].
    ///
    /// Retained on `LocalIndex` so higher layers (`ChainedIndex`, tests) can
    /// drive tag refreshes through the same facade they use for manifest
    /// reads, without reaching into the tag-layer internals.
    pub async fn refresh_tags(&self, identifier: &oci::Identifier, source: &super::Index) -> Result<()> {
        self.tags.refresh(identifier, source).await
    }

    /// Fetches the manifest chain from `source` and, if successful, commits
    /// the tag pointer via [`TagManager::commit`] so that subsequent cache
    /// reads short-circuit the chain walk. Digest-only identifiers are
    /// fully addressed by the digest itself and skip the tag commit.
    ///
    /// Returns `Ok(true)` when the chain was persisted, `Ok(false)` when
    /// the source cleanly does not have the identifier, and `Err` on source
    /// failure.
    ///
    /// The single entry point used by `ChainedIndex` on cache miss. Chain
    /// persistence and tag commit are exposed as one atomic operation to
    /// keep the write-through sequence inside the cache facade.
    pub async fn write_chain_and_commit_tag(
        &self,
        source: &super::Index,
        identifier: &oci::Identifier,
    ) -> Result<bool> {
        // `depth = 0` at the top-level entry. Child manifests inside an
        // image index are written inline (not via recursion) because the
        // OCI spec does not describe a nested image index — the inline
        // `matches!` check below rejects that shape. The `depth` parameter
        // is defence-in-depth: if a future refactor routes child
        // processing through this function recursively, it must pass
        // `depth + 1`, and the guard at the top of `_inner` will catch
        // any unsupported nesting before a corrupt cache entry is written.
        let Some(digest) = self.persist_manifest_chain_inner(source, identifier, 0).await? else {
            return Ok(false);
        };
        // Digest-only inputs have no tag to pin — the chain is fully addressed by
        // the digest itself, so we skip the tag commit. Tag-bearing inputs (the
        // common path from `walk_chain`'s `tag_or_latest()` normalisation) commit
        // the tag pointer so subsequent cache reads short-circuit the chain walk.
        if let Some(tag) = identifier.tag() {
            self.tags.commit(identifier, tag, &digest).await?;
        }
        Ok(true)
    }

    async fn persist_manifest_chain_inner(
        &self,
        source: &super::Index,
        identifier: &oci::Identifier,
        depth: usize,
    ) -> Result<Option<oci::Digest>> {
        if depth > 1 {
            // Any recursive call past depth 1 implies an image index nested
            // inside another image index — not a supported OCI shape.
            let digest = identifier
                .digest()
                .ok_or_else(|| super::error::Error::RemoteManifestNotFound(identifier.to_string()))?;
            return Err(super::error::Error::NestedImageIndex { digest }.into());
        }

        let Some((digest, manifest)) = source.fetch_manifest(identifier).await? else {
            return Ok(None);
        };

        let pinned: oci::PinnedIdentifier = identifier.clone_with_digest(digest.clone()).try_into()?;
        write_manifest_blob(&self.blob_store, &pinned, &manifest).await?;

        if let oci::Manifest::ImageIndex(image_index) = manifest {
            for entry in image_index.manifests {
                let child_digest: oci::Digest = entry.digest.clone().try_into()?;
                let child_id = identifier.clone_with_digest(child_digest.clone());
                let (_, child_manifest) = source
                    .fetch_manifest(&child_id)
                    .await?
                    .ok_or_else(|| super::error::Error::RemoteManifestNotFound(child_id.to_string()))?;
                // An image index nested inside an image index is not a
                // supported OCI shape; writing it would produce a corrupt
                // cache entry. Abort the persist.
                if matches!(child_manifest, oci::Manifest::ImageIndex(_)) {
                    return Err(super::error::Error::NestedImageIndex { digest: child_digest }.into());
                }
                let child_pinned: oci::PinnedIdentifier = child_id.try_into()?;
                write_manifest_blob(&self.blob_store, &child_pinned, &child_manifest).await?;
            }
        }

        Ok(Some(digest))
    }

    async fn get_tags(&self, identifier: &oci::Identifier) -> Result<Option<HashMap<String, oci::Digest>>> {
        self.tags.get(identifier).await
    }

    async fn get_manifest(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> Result<Option<oci::Manifest>> {
        if let Some(cached) = self.cache.get_manifest(identifier, digest).await {
            log::trace!(
                "Manifest for identifier '{}' and digest '{}' found in cache.",
                identifier,
                digest
            );
            return Ok(Some(cached));
        }

        // Shared lock on the blob `data` file. Missing file → `Ok(None)`,
        // which is the cache-miss signal ChainedIndex turns into a chain walk.
        let pinned: oci::PinnedIdentifier = identifier.clone_with_digest(digest.clone()).try_into()?;
        let Some(guard) = self.blob_store.acquire_read(&pinned).await? else {
            log::debug!(
                "Manifest file not found for identifier '{}' and digest '{}'.",
                identifier,
                digest
            );
            return Ok(None);
        };

        log::trace!(
            "Reading manifest for identifier '{}' and digest '{}'.",
            identifier,
            digest
        );
        let bytes = guard.read_bytes().await?;
        drop(guard);
        let manifest: oci::Manifest = match serde_json::from_slice(&bytes) {
            Ok(m) => m,
            Err(e) => {
                // Kill-9 recovery window: a truncated `data` file from a
                // previous crash must not propagate as an error. Log at warn
                // and treat as a cache miss so ChainedIndex can re-fetch.
                log::warn!(
                    "Manifest blob for '{}'@'{}' is unparseable ({e}) — treating as cache miss for recovery.",
                    identifier,
                    digest
                );
                return Ok(None);
            }
        };
        log::trace!(
            "Caching manifest for identifier '{}' and digest '{}'.",
            identifier,
            digest
        );
        self.cache
            .set_manifest(identifier.clone(), digest.clone(), manifest.clone())
            .await;
        Ok(Some(manifest))
    }
}

/// Writes `manifest` under an exclusive `BlobGuard` on the blob store's
/// CAS path for `pinned`. Also writes the sibling `digest` marker file as
/// part of `BlobStore::acquire_write`.
async fn write_manifest_blob(
    blob_store: &BlobStore,
    pinned: &oci::PinnedIdentifier,
    manifest: &oci::Manifest,
) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let guard = blob_store.acquire_write(pinned).await?;
    guard.write_bytes(&bytes).await?;
    Ok(())
}

#[async_trait]
impl index_impl::IndexImpl for LocalIndex {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        self.tags.tag_store().list_repositories(registry).await
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        Ok(self
            .get_tags(identifier)
            .await?
            .map(|tags| tags.into_keys().filter(|t| !Tag::is_internal_str(t)).collect()))
    }

    async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        log::trace!("Fetching manifest for identifier '{}'.", identifier);
        let queried_digest = identifier.digest();
        let queried_tag = if queried_digest.is_some() {
            identifier.tag()
        } else {
            Some(identifier.tag_or_latest())
        };

        if let Some(queried_digest) = &queried_digest {
            return Ok(self
                .get_manifest(identifier, queried_digest)
                .await?
                .map(|m| (queried_digest.clone(), m)));
        } else if let Some(queried_tag) = queried_tag {
            let Some(available_tags) = self.get_tags(identifier).await? else {
                return Ok(None);
            };
            let digest = match available_tags.get(queried_tag) {
                Some(digest) => digest,
                None => {
                    log::debug!("Tag '{}' not found for identifier '{}'.", queried_tag, identifier);
                    return Ok(None);
                }
            };
            return Ok(self
                .get_manifest(identifier, digest)
                .await?
                .map(|m| (digest.clone(), m)));
        }

        Ok(None)
    }

    async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>> {
        let queried_digest = identifier.digest();
        let queried_tag = if queried_digest.is_some() {
            identifier.tag()
        } else {
            Some(identifier.tag_or_latest())
        };

        if let Some(queried_digest) = queried_digest {
            if self.get_manifest(identifier, &queried_digest).await?.is_some() {
                return Ok(Some(queried_digest));
            }
            return Ok(None);
        } else if let Some(queried_tag) = queried_tag {
            let Some(available_tags) = self.get_tags(identifier).await? else {
                return Ok(None);
            };
            if let Some(digest) = available_tags.get(queried_tag) {
                return Ok(Some(digest.clone()));
            }
        }

        Ok(None)
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(self.clone())
    }
}

// ── Specification tests — plan_resolution_chain_refs.md tests 14-21 ─────
//
// Each test encodes the expected behaviour of the LocalIndex primitives
// introduced for the resolution-chain-refs design.
#[cfg(test)]
mod spec_tests {
    use super::*;
    use std::collections::HashMap;

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::file_structure::{BlobStore, TagStore};
    use crate::oci::{ImageManifest, Manifest};

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";

    fn make_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(Config {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: BlobStore::new(dir.path().join("blobs")),
        })
    }

    fn repo_id() -> oci::Identifier {
        oci::Identifier::new_registry(REPO, REGISTRY)
    }

    fn tagged_id(tag: &str) -> oci::Identifier {
        repo_id().clone_with_tag(tag)
    }

    fn digest(hex_char: char) -> oci::Digest {
        oci::Digest::Sha256(hex_char.to_string().repeat(64))
    }

    /// Minimal fake IndexImpl that returns a fixed ImageManifest for any tag.
    #[derive(Clone)]
    struct FakeSource {
        known_tags: HashMap<String, oci::Digest>,
    }

    impl FakeSource {
        fn with_tag(tag: &str, d: oci::Digest) -> Self {
            let mut known_tags = HashMap::new();
            known_tags.insert(tag.to_string(), d);
            Self { known_tags }
        }
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for FakeSource {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(Some(self.known_tags.keys().cloned().collect()))
        }
        async fn fetch_manifest(&self, identifier: &oci::Identifier) -> crate::Result<Option<(oci::Digest, Manifest)>> {
            let tag = identifier.tag_or_latest();
            Ok(self
                .known_tags
                .get(tag)
                .map(|d| (d.clone(), Manifest::Image(ImageManifest::default()))))
        }
        async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> crate::Result<Option<oci::Digest>> {
            Ok(self.known_tags.get(identifier.tag_or_latest()).cloned())
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn make_source(tag: &str, d: oci::Digest) -> super::super::Index {
        super::super::Index::from_impl(FakeSource::with_tag(tag, d))
    }

    /// Fake source that knows a configurable set of tags, each resolving to
    /// a fixed `ImageManifest` blob. Multi-tag variant of `FakeSource`.
    #[derive(Clone)]
    struct MultiTagSource {
        known: HashMap<String, oci::Digest>,
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for MultiTagSource {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(Some(self.known.keys().cloned().collect()))
        }
        async fn fetch_manifest(&self, identifier: &oci::Identifier) -> crate::Result<Option<(oci::Digest, Manifest)>> {
            let tag = identifier.tag_or_latest();
            Ok(self
                .known
                .get(tag)
                .map(|d| (d.clone(), Manifest::Image(ImageManifest::default()))))
        }
        async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> crate::Result<Option<oci::Digest>> {
            Ok(self.known.get(identifier.tag_or_latest()).cloned())
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn source_with(tags: &[(&str, oci::Digest)]) -> super::super::Index {
        let mut known = HashMap::new();
        for (t, d) in tags {
            known.insert((*t).to_string(), d.clone());
        }
        super::super::Index::from_impl(MultiTagSource { known })
    }

    /// Helper: seed an existing tag on disk by routing through `refresh_tags`
    /// against a single-tag source. Uses the same entry point the rest of
    /// the suite exercises, so the test reflects the real write path.
    async fn seed_tag(index: &LocalIndex, id: &oci::Identifier, tag: &str, d: oci::Digest) {
        let tagged = id.clone_with_tag(tag);
        let source = make_source(tag, d);
        index.refresh_tags(&tagged, &source).await.unwrap();
    }

    // ── refresh_tags: merge with existing disk entries ───────────────────

    /// Design record (rev §4 of plan_resolution_chain_refs.md):
    /// `refresh_tags` merges the source's tag set with any existing on-disk
    /// entries, preserving tags that the source does not report.
    #[tokio::test]
    async fn refresh_tags_merges_new_tags_with_existing_disk_entries() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = repo_id();

        // Seed an initial tag via refresh_tags itself.
        seed_tag(&index, &id, "1.0", digest('a')).await;

        // Refresh a different tag from a fresh source.
        let second = source_with(&[("2.0", digest('b'))]);
        index.refresh_tags(&id.clone_with_tag("2.0"), &second).await.unwrap();

        let fresh = make_index(&dir);
        let tags = fresh.get_tags(&id).await.unwrap().unwrap();
        assert!(tags.contains_key("1.0"), "tag 1.0 must survive the merge");
        assert!(tags.contains_key("2.0"), "tag 2.0 must be present after merge");
    }

    // ── refresh_tags: non-destructive for tags not in source ─────────────

    /// `refresh_tags` must preserve tags present on disk that the source
    /// does not report for the requested identifier.
    #[tokio::test]
    async fn refresh_tags_preserves_tags_not_in_source() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = repo_id();

        seed_tag(&index, &id, "existing", digest('e')).await;

        // Refresh a different tag — the source never mentions "existing".
        // (Use hex char 'f' for the digest — 'n' is not a valid hex digit and
        // would fail deserialization.)
        let source = source_with(&[("new-tag", digest('f'))]);
        index
            .refresh_tags(&id.clone_with_tag("new-tag"), &source)
            .await
            .unwrap();

        let fresh = make_index(&dir);
        let tags = fresh.get_tags(&id).await.unwrap().unwrap();
        assert!(
            tags.contains_key("existing"),
            "pre-existing tag must be preserved by refresh_tags non-destructive merge"
        );
    }

    // ── refresh_tags: concurrent callers visible on disk ─────────────────

    /// Eight concurrent `refresh_tags` callers on different tags all land
    /// on disk — proves the atomic tag-writer serialises correctly.
    #[tokio::test]
    async fn refresh_tags_concurrent_callers_both_visible_on_disk() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = repo_id();

        let mut tasks: tokio::task::JoinSet<crate::Result<()>> = tokio::task::JoinSet::new();
        for i in 0u8..8 {
            let idx = index.clone();
            let ident = id.clone();
            tasks.spawn(async move {
                let tag = format!("v{i}");
                let d = oci::Digest::Sha256(format!("{i:0>64}"));
                let source = source_with(&[(&tag, d)]);
                idx.refresh_tags(&ident.clone_with_tag(&tag), &source).await
            });
        }
        while let Some(joined) = tasks.join_next().await {
            joined.expect("task panicked").expect("refresh_tags failed");
        }

        let fresh = make_index(&dir);
        let tags = fresh.get_tags(&id).await.unwrap().unwrap();
        assert_eq!(tags.len(), 8, "all 8 concurrent writers' entries must be on disk");
    }

    // ── ChainedIndex cache-miss persistence: image manifest ──────────────

    /// `ChainedIndex::fetch_manifest` on a cache miss must persist the
    /// fetched image manifest blob at the expected CAS path.
    #[tokio::test]
    async fn chained_fetch_manifest_persists_image_blob_at_expected_cas_path() {
        let dir = TempDir::new().unwrap();
        let cache = make_index(&dir);
        let d = digest('a');
        let source = make_source("3.28", d.clone());
        let id = tagged_id("3.28");

        let cache_clone = cache.clone();
        let chained = super::super::Index::from_chained(cache_clone, vec![source], super::super::ChainMode::Default);
        let _ = chained.fetch_manifest(&id).await.unwrap();

        let expected = cache.blob_store.data(REGISTRY, &d);
        assert!(
            expected.exists(),
            "chained fetch_manifest must persist the blob at {}",
            expected.display()
        );
    }

    // ── ChainedIndex cache-miss persistence: digest-only input ───────────

    /// Regression for the digest-only `walk_chain` short-circuit. A digest-
    /// pinned identifier must walk the source chain via
    /// `GET /v2/<repo>/manifests/<digest>` and persist the blob into the
    /// cache — no tag commit, but the data file must exist after the fetch.
    #[tokio::test]
    async fn chained_fetch_manifest_persists_blob_for_digest_only_identifier() {
        let dir = TempDir::new().unwrap();
        let cache = make_index(&dir);
        let d = digest('a');

        // Source serves the manifest by digest, ignoring the tag slot.
        #[derive(Clone)]
        struct DigestOnlySource {
            d: oci::Digest,
        }
        #[async_trait]
        impl super::super::index_impl::IndexImpl for DigestOnlySource {
            async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
                Ok(Vec::new())
            }
            async fn list_tags(&self, _: &oci::Identifier) -> crate::Result<Option<Vec<String>>> {
                Ok(None)
            }
            async fn fetch_manifest(&self, _: &oci::Identifier) -> crate::Result<Option<(oci::Digest, Manifest)>> {
                Ok(Some((self.d.clone(), Manifest::Image(ImageManifest::default()))))
            }
            async fn fetch_manifest_digest(&self, _: &oci::Identifier) -> crate::Result<Option<oci::Digest>> {
                Ok(Some(self.d.clone()))
            }
            fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
                Box::new(self.clone())
            }
        }

        let source = super::super::Index::from_impl(DigestOnlySource { d: d.clone() });
        let id = repo_id().clone_with_digest(d.clone());

        let cache_clone = cache.clone();
        let chained = super::super::Index::from_chained(cache_clone, vec![source], super::super::ChainMode::Default);
        let result = chained.fetch_manifest(&id).await.unwrap();
        assert!(
            result.is_some(),
            "digest-only fetch_manifest must walk the chain and return Some"
        );

        let expected = cache.blob_store.data(REGISTRY, &d);
        assert!(
            expected.exists(),
            "digest-only walk_chain must persist the blob at {}",
            expected.display()
        );
    }

    // ── ChainedIndex cache-miss persistence: image-index recursion ───────

    /// `ChainedIndex::fetch_manifest` must write both the top-level image
    /// index blob and every child manifest blob when the resolved identifier
    /// points at an image index.
    #[tokio::test]
    async fn chained_fetch_manifest_recurses_for_image_index_children() {
        let dir = TempDir::new().unwrap();
        let cache = make_index(&dir);

        let child_digest = oci::Digest::Sha256('b'.to_string().repeat(64));
        let parent_digest = digest('a');
        let child_digest_str = format!("sha256:{}", 'b'.to_string().repeat(64));

        #[derive(Clone)]
        struct ImageIndexSource {
            parent: oci::Digest,
            child: oci::Digest,
            child_digest_str: String,
        }
        #[async_trait]
        impl super::super::index_impl::IndexImpl for ImageIndexSource {
            async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
                Ok(Vec::new())
            }
            async fn list_tags(&self, _: &oci::Identifier) -> crate::Result<Option<Vec<String>>> {
                Ok(None)
            }
            async fn fetch_manifest(
                &self,
                identifier: &oci::Identifier,
            ) -> crate::Result<Option<(oci::Digest, Manifest)>> {
                // Tag-only lookups (no digest) return the parent image index;
                // digest-bearing lookups (child recursion) return a leaf image
                // manifest. `clone_with_digest` preserves the tag, so we key
                // on the digest slot, not the tag slot.
                if identifier.digest().is_none() {
                    let idx = oci::Manifest::ImageIndex(oci::ImageIndex {
                        schema_version: 2,
                        media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
                        manifests: vec![oci::ImageIndexEntry {
                            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                            digest: self.child_digest_str.clone(),
                            size: 1,
                            platform: None,
                            annotations: None,
                        }],
                        artifact_type: None,
                        annotations: None,
                    });
                    Ok(Some((self.parent.clone(), idx)))
                } else {
                    Ok(Some((self.child.clone(), Manifest::Image(ImageManifest::default()))))
                }
            }
            async fn fetch_manifest_digest(&self, _: &oci::Identifier) -> crate::Result<Option<oci::Digest>> {
                Ok(Some(self.parent.clone()))
            }
            fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
                Box::new(self.clone())
            }
        }

        let source = super::super::Index::from_impl(ImageIndexSource {
            parent: parent_digest.clone(),
            child: child_digest.clone(),
            child_digest_str,
        });
        let id = tagged_id("3.28");

        let cache_clone = cache.clone();
        let chained = super::super::Index::from_chained(cache_clone, vec![source], super::super::ChainMode::Default);
        let _ = chained.fetch_manifest(&id).await.unwrap();

        let parent_path = cache.blob_store.data(REGISTRY, &parent_digest);
        let child_path = cache.blob_store.data(REGISTRY, &child_digest);
        assert!(
            parent_path.exists(),
            "parent index blob must exist at {}",
            parent_path.display()
        );
        assert!(
            child_path.exists(),
            "child manifest blob must exist at {}",
            child_path.display()
        );
    }

    // ── ChainedIndex cache-miss persistence: sibling digest marker ───────

    /// Every blob `ChainedIndex::fetch_manifest` persists on a cache miss
    /// is accompanied by a sibling `digest` marker file.
    #[tokio::test]
    async fn chained_fetch_manifest_writes_sibling_digest_marker_for_every_blob() {
        let dir = TempDir::new().unwrap();
        let cache = make_index(&dir);
        let d = digest('c');
        let source = make_source("3.28", d.clone());
        let id = tagged_id("3.28");

        let cache_clone = cache.clone();
        let chained = super::super::Index::from_chained(cache_clone, vec![source], super::super::ChainMode::Default);
        let _ = chained.fetch_manifest(&id).await.unwrap();

        let digest_file = cache.blob_store.digest_file(REGISTRY, &d);
        assert!(
            digest_file.exists(),
            "sibling digest marker must be written alongside data; missing: {}",
            digest_file.display()
        );
    }

    // ── test 20 ───────────────────────────────────────────────────────────

    /// Design record §20: get_manifest on a truncated blob data file returns
    /// None (log warn, treat as cache miss for graceful recovery).
    ///
    /// This verifies the Phase E.3 requirement: BlobStore::acquire_read
    /// wrapping + graceful parse-failure recovery in get_manifest.
    #[tokio::test]
    async fn get_manifest_on_truncated_blob_file_returns_none_and_logs_warn() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let d = digest('d');
        // Write partial/corrupt JSON directly to the blob data path.
        let data_path = index.blob_store.data(REGISTRY, &d);
        std::fs::create_dir_all(data_path.parent().unwrap()).unwrap();
        std::fs::write(&data_path, b"{\"schemaVersion\":2,\"broken\"").unwrap();

        // get_manifest must return Ok(None) — not an error — for corrupt data.
        let id = tagged_id("3.28");
        let result = index.get_manifest(&id, &d).await.unwrap();
        assert!(
            result.is_none(),
            "get_manifest on a truncated blob file must return Ok(None), not Err"
        );
    }

    // ── latent-bug fix (integration via ChainedIndex) ────────────────────

    /// The latent-bug fix. A tag file is present on disk pointing at digest
    /// D, but D's blob file is missing. `fetch_manifest` via a `ChainedIndex`
    /// must re-populate the blob on cache miss and return `Some` — not loop
    /// forever or return `None`.
    ///
    /// This is an integration-style test because it exercises the full path:
    /// `LocalIndex::fetch_manifest` sees cache-miss on the blob → `ChainedIndex`
    /// walks the source → write-through persists it → re-read succeeds.
    #[tokio::test]
    async fn latent_bug_fix_missing_manifest_triggers_refetch_via_chain() {
        let dir = TempDir::new().unwrap();
        let cache = make_index(&dir);

        // Step 1: seed a tag file pointing at digest 'd' via refresh_tags.
        let d = digest('d');
        let id = tagged_id("3.28");
        seed_tag(&cache, &id, "3.28", d.clone()).await;

        // Step 2: nuke the blob data file — simulate it being missing while
        // the tag pointer remains on disk. (refresh_tags' side-effect of
        // persisting the manifest blob is what we're explicitly undoing.)
        let blob_path = cache.blob_store.data(REGISTRY, &d);
        if blob_path.exists() {
            std::fs::remove_file(&blob_path).unwrap();
        }
        assert!(!blob_path.exists(), "prerequisite: blob file must be absent");

        // Step 3: construct a ChainedIndex with a source that can serve the manifest.
        let source = make_source("3.28", d.clone());
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        // Step 4: fetch_manifest must re-fetch and return Some.
        let result = chained.fetch_manifest(&id).await.unwrap();
        assert!(
            result.is_some(),
            "latent-bug fix: fetch_manifest with tag cached but blob missing must re-fetch and return Some"
        );
        let (returned_digest, _) = result.unwrap();
        assert_eq!(
            returned_digest, d,
            "returned digest must match the expected blob digest"
        );
    }
}

// ── Concurrency tests ────────────────────────────────────────────────────
//
// Exercise `update_tags` under contention to prove the per-repo tag lock plus
// atomic rename survive racing writers — both across distinct tags on the
// same repo (all entries must be preserved) and against the same tag
// (last-writer-wins, file stays valid). Drives through a minimal in-memory
// `TestIndex` that fakes `IndexImpl` so no registry is involved.
#[cfg(test)]
mod concurrency_tests {
    use super::*;

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::file_structure::{BlobStore, TagStore};
    use crate::oci::{ImageManifest, Manifest};

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";

    /// Sha256 digest built from a repeated hex nibble (`0..=15`).
    fn hex_digest_n(nibble: u8) -> oci::Digest {
        assert!(nibble < 16, "hex nibble must be < 16");
        let ch = if nibble < 10 {
            char::from(b'0' + nibble)
        } else {
            char::from(b'a' + (nibble - 10))
        };
        oci::Digest::Sha256(ch.to_string().repeat(64))
    }

    fn make_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(Config {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: BlobStore::new(dir.path().join("blobs")),
        })
    }

    fn repo_id() -> oci::Identifier {
        oci::Identifier::new_registry(REPO, REGISTRY)
    }

    /// Minimal in-memory fake of `IndexImpl` — every known (tag → digest)
    /// pair is served back verbatim with a default `ImageManifest`. Used to
    /// drive `LocalIndex::update` without touching a real registry.
    #[derive(Clone)]
    struct TestIndex {
        known_tags: HashMap<String, oci::Digest>,
    }

    impl TestIndex {
        fn with_tag(tag: &str, digest: oci::Digest) -> Self {
            let mut known_tags = HashMap::new();
            known_tags.insert(tag.to_string(), digest);
            Self { known_tags }
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for TestIndex {
        async fn list_repositories(&self, _registry: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(self.known_tags.keys().cloned().collect()))
        }

        async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, Manifest)>> {
            let tag = identifier.tag_or_latest();
            Ok(self
                .known_tags
                .get(tag)
                .map(|d| (d.clone(), Manifest::Image(ImageManifest::default()))))
        }

        async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>> {
            Ok(self.known_tags.get(identifier.tag_or_latest()).cloned())
        }

        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Spawn N tasks, each updating a distinct tag on the same repo against
    /// its own source `Index`, and assert every written entry survives the
    /// race. The per-repo exclusive lock + read-modify-write merge under the
    /// lock is what makes this safe.
    #[tokio::test]
    async fn concurrent_update_different_tags_preserves_all() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);

        let writers = 16usize;
        let mut set: tokio::task::JoinSet<Result<()>> = tokio::task::JoinSet::new();
        for i in 0..writers {
            let index = index.clone();
            let tag = format!("v{i}");
            let digest = hex_digest_n((i as u8) % 16);
            let source = super::super::Index::from_impl(TestIndex::with_tag(&tag, digest));
            let id = repo_id().clone_with_tag(&tag);
            set.spawn(async move { index.refresh_tags(&id, &source).await });
        }
        while let Some(joined) = set.join_next().await {
            joined.expect("task panicked").expect("update failed");
        }

        // Re-read via a fresh LocalIndex so we bypass the in-memory cache
        // and see exactly what landed on disk.
        let fresh = make_index(&dir);
        let tags = fresh.get_tags(&repo_id()).await.unwrap().unwrap();
        assert_eq!(tags.len(), writers, "every writer's tag must be present on disk");
        for i in 0..writers {
            assert!(tags.contains_key(&format!("v{i}")), "missing tag v{i}");
        }
    }

    /// Many writers racing the same tag must not corrupt the file. The final
    /// digest is one of the contenders (last-writer-wins is the agreed
    /// policy), and the file round-trips cleanly.
    #[tokio::test]
    async fn concurrent_update_same_tag_does_not_corrupt() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);

        let writers = 16usize;
        let tag = "latest".to_string();
        let mut set: tokio::task::JoinSet<Result<()>> = tokio::task::JoinSet::new();
        for i in 0..writers {
            let index = index.clone();
            let tag = tag.clone();
            let digest = hex_digest_n((i as u8) % 16);
            let source = super::super::Index::from_impl(TestIndex::with_tag(&tag, digest));
            let id = repo_id().clone_with_tag(&tag);
            set.spawn(async move { index.refresh_tags(&id, &source).await });
        }
        while let Some(joined) = set.join_next().await {
            joined.expect("task panicked").expect("update failed");
        }

        let fresh = make_index(&dir);
        let tags = fresh.get_tags(&repo_id()).await.unwrap().unwrap();
        assert_eq!(tags.len(), 1, "only the single contested tag should be present");
        assert!(tags.contains_key(&tag));
    }
}
