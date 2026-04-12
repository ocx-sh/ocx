// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;
use std::collections::HashMap;

use crate::{
    Result,
    file_structure::{BlobStore, TagStore},
    log, oci,
    package::tag::Tag,
    prelude::*,
};

use super::index_impl;

mod cache;
mod config;
mod tag_guard;
mod tag_lock;

pub use config::Config;

#[derive(Clone)]
pub struct LocalIndex {
    tag_store: TagStore,
    blob_store: BlobStore,
    cache: cache::SharedCache,
}

impl LocalIndex {
    pub fn new(config: Config) -> Self {
        Self {
            tag_store: config.tag_store,
            blob_store: config.blob_store,
            cache: cache::SharedCache::default(),
        }
    }

    /// Syncs the local index with `index` for the given identifier.
    ///
    /// When the identifier carries a tag (e.g. `cmake:3.28`), only that tag is
    /// refreshed — no remote tag listing. A bare identifier (`cmake`) triggers
    /// `list_tags` to discover every tag to sync. Both paths funnel into
    /// [`update_tags`](Self::update_tags) with a pre-computed tag set.
    pub async fn update(&self, index: &super::Index, identifier: &oci::Identifier) -> Result<()> {
        let tags = match identifier.tag() {
            Some(tag) => vec![tag.to_owned()],
            None => index.list_tags(identifier).await?.unwrap_or_default(),
        };
        self.update_tags(index, identifier, tags).await
    }

    /// Refreshes `tags` on `identifier` from `index` into the local tag log.
    ///
    /// Remote I/O (digest fetches and any missing manifest downloads) runs
    /// **outside** the per-repo exclusive lock so that a slow registry cannot
    /// stall other OCX processes touching the same repo. Only the final
    /// read-modify-write on the tag file is serialised, and only if at least
    /// one tag actually changed.
    async fn update_tags(&self, index: &super::Index, identifier: &oci::Identifier, tags: Vec<String>) -> Result<()> {
        // Seed from cache / disk to skip tags that already resolve to the
        // same digest. Safe to be stale — the merge under the exclusive lock
        // below is the authoritative read-modify-write.
        let seed = self.get_tags(identifier).await?.unwrap_or_default();

        let mut fetched: HashMap<String, oci::Digest> = HashMap::new();
        for tag in tags {
            let tagged = identifier.clone_with_tag(&tag);
            log::info!("Updating tag '{}' for identifier '{}'.", tag, identifier);

            let Some(digest) = index.fetch_manifest_digest(&tagged).await? else {
                log::debug!("Remote has no digest for tag '{}' — skipping.", tag);
                continue;
            };

            if seed.get(&tag) == Some(&digest) {
                log::debug!(
                    "Tag '{}' for identifier '{}' is up to date with digest '{}'.",
                    tag,
                    identifier,
                    digest
                );
                continue;
            }

            let manifest_path = self.blob_store.data(tagged.registry(), &digest);
            if !manifest_path.exists() {
                self.update_manifest(index, &tagged, &digest).await?;
            }

            fetched.insert(tag, digest);
        }

        if fetched.is_empty() {
            return Ok(());
        }

        // Per-repo exclusive lock guards the read-modify-write so concurrent
        // writers in other OCX processes don't clobber each other. Existing
        // disk-only entries are preserved; identical-tag races resolve
        // last-writer-wins.
        let tags_path = self.tag_store.tags(identifier);
        let guard = tag_guard::TagGuard::acquire_exclusive(tags_path).await?;
        let mut merged = guard.read_disk(identifier).await?;
        merged.extend(fetched);
        guard.write_disk(identifier, &merged).await?;

        let cache = self.cache.write().await;
        cache.set_tags(identifier.clone(), merged).await;

        Ok(())
    }

    async fn update_manifest(
        &self,
        index: &super::Index,
        identifier: &oci::Identifier,
        digest: &oci::Digest,
    ) -> Result<()> {
        let (_, manifest) = index
            .fetch_manifest(identifier)
            .await?
            .ok_or_else(|| super::error::Error::RemoteManifestNotFound(identifier.to_string()))?;
        let path = self.blob_store.data(identifier.registry(), digest);
        manifest.write_json(&path).await?;

        if let oci::Manifest::ImageIndex(image_index) = manifest {
            for manifest in image_index.manifests {
                let digest = manifest.digest.clone().try_into()?;
                let identifier = identifier.clone_with_digest(digest);
                let (digest, manifest) = index
                    .fetch_manifest(&identifier)
                    .await?
                    .ok_or_else(|| super::error::Error::RemoteManifestNotFound(identifier.to_string()))?;
                let path = self.blob_store.data(identifier.registry(), &digest);
                manifest.write_json(&path).await?;
            }
        }

        Ok(())
    }

    async fn get_tags(&self, identifier: &oci::Identifier) -> Result<Option<HashMap<String, oci::Digest>>> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get_tags(identifier).await {
                return Ok(Some(cached));
            }
        }

        let tags_path = self.tag_store.tags(identifier);

        // Shared (reader) lock on the tag file itself. `acquire_shared`
        // returns `Ok(None)` when the file doesn't exist, which is how we
        // distinguish "no tags known for this repo" from a real I/O error.
        let Some(guard) = tag_guard::TagGuard::acquire_shared(tags_path.clone()).await? else {
            log::debug!(
                "Tags file '{}' not found for identifier '{}'.",
                tags_path.display(),
                identifier
            );
            return Ok(None);
        };
        let tags = guard.read_disk(identifier).await?;
        drop(guard);

        {
            let cache = self.cache.write().await;
            cache.set_tags(identifier.clone(), tags.clone()).await;
        }

        Ok(Some(tags))
    }

    async fn get_manifest(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> Result<Option<oci::Manifest>> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get_manifest(identifier, digest).await {
                log::trace!(
                    "Manifest for identifier '{}' and digest '{}' found in cache.",
                    identifier,
                    digest
                );
                return Ok(Some(cached));
            }
        }

        let manifest_path = self.blob_store.data(identifier.registry(), digest);
        if !manifest_path.exists() {
            log::debug!(
                "Manifest file not found for identifier '{}' and digest '{}'.",
                identifier,
                digest
            );
            return Ok(None);
        }

        log::trace!(
            "Reading manifest for identifier '{}' and digest '{}' from path '{}'.",
            identifier,
            digest,
            manifest_path.display()
        );
        let manifest = oci::Manifest::read_json(manifest_path).await?;
        {
            log::trace!(
                "Caching manifest for identifier '{}' and digest '{}'.",
                identifier,
                digest
            );
            let cache = self.cache.write().await;
            cache
                .set_manifest(identifier.clone(), digest.clone(), manifest.clone())
                .await;
        }
        Ok(Some(manifest))
    }
}

#[async_trait]
impl index_impl::IndexImpl for LocalIndex {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        self.tag_store.list_repositories(registry).await
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
        Box::new(Self {
            tag_store: self.tag_store.clone(),
            blob_store: self.blob_store.clone(),
            cache: self.cache.clone(),
        })
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
            set.spawn(async move { index.update(&source, &id).await });
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
            set.spawn(async move { index.update(&source, &id).await });
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
