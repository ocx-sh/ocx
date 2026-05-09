// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use crate::{Result, file_structure::TagStore, log, oci};

use super::{cache, tag_guard::TagGuard};

/// Self-contained manager for the local tag store.
///
/// Owns the disk tag files, the per-repo exclusive lock discipline, and the
/// in-memory tag cache. Exposes a small interface — `refresh`, `commit`,
/// `get` — that captures the legitimate tag-layer operations without leaking
/// the raw lock/merge machinery to `LocalIndex` or higher layers.
#[derive(Clone)]
pub struct TagManager {
    tag_store: TagStore,
    cache: cache::SharedCache,
}

impl TagManager {
    pub fn new(tag_store: TagStore, cache: cache::SharedCache) -> Self {
        Self { tag_store, cache }
    }

    /// Returns the underlying tag store. Used by `LocalIndex` for
    /// repository enumeration, which is a store-level operation unrelated
    /// to tag read/write.
    pub fn tag_store(&self) -> &TagStore {
        &self.tag_store
    }

    /// Atomic tag refresh entry point. Fetches the requested tag set from
    /// `source` and writes it into the on-disk tag file under an exclusive
    /// `TagGuard`, merging non-destructively with any existing entries.
    ///
    /// A tagged identifier (e.g. `cmake:3.28`) refreshes only that tag. A
    /// bare identifier (`cmake`) first calls `list_tags` on the source to
    /// discover every tag to sync. Either path funnels into the same atomic
    /// read-modify-write on the on-disk tag file.
    ///
    /// This method does NOT short-circuit when a tag's seed digest matches
    /// the source — the downstream correctness invariant ("tag cached AND
    /// manifest on disk") is enforced by `ChainedIndex`'s walk path, not
    /// here. `refresh` is strictly about tag pointers; manifest persistence
    /// is a separate concern.
    pub async fn refresh(&self, identifier: &oci::Identifier, source: &super::super::Index) -> Result<()> {
        let tags = match identifier.tag() {
            Some(tag) => vec![tag.to_owned()],
            None => source.list_tags(identifier).await?.unwrap_or_default(),
        };

        let mut fetched: HashMap<String, oci::Digest> = HashMap::new();
        for tag in tags {
            let tagged = identifier.clone_with_tag(&tag);
            log::info!("Refreshing tag '{}' for identifier '{}'.", tag, identifier);
            let Some(digest) = source
                .fetch_manifest_digest(&tagged, super::super::IndexOperation::Resolve)
                .await?
            else {
                log::debug!("Source has no digest for tag '{}' — skipping.", tag);
                continue;
            };
            fetched.insert(tag, digest);
        }

        if fetched.is_empty() {
            return Ok(());
        }

        let merged = self.merge_under_lock(identifier, fetched).await?;
        self.publish_to_cache(identifier.clone(), merged).await;
        Ok(())
    }

    /// Pins a single `(tag, digest)` pair. Used by `ChainedIndex::walk_chain`
    /// to record the tag pointer once the resolution chain has been
    /// persisted to the blob store. Equivalent to `refresh` with a
    /// pre-fetched digest — no source round-trip.
    pub async fn commit(&self, identifier: &oci::Identifier, tag: &str, digest: &oci::Digest) -> Result<()> {
        let mut fetched = HashMap::new();
        fetched.insert(tag.to_owned(), digest.clone());
        let merged = self.merge_under_lock(identifier, fetched).await?;
        self.publish_to_cache(identifier.clone(), merged).await;
        Ok(())
    }

    /// Reads the tag map for `identifier`, consulting the in-memory cache
    /// first and falling back to the on-disk tag file under a shared lock.
    /// Returns `Ok(None)` when no tag file exists for the repository.
    pub async fn get(&self, identifier: &oci::Identifier) -> Result<Option<HashMap<String, oci::Digest>>> {
        if let Some(cached) = self.cache.get_tags(identifier).await {
            return Ok(Some(cached));
        }

        let tags_path = self.tag_store.tags(identifier);

        // Shared (reader) lock on the tag file itself. `acquire_shared`
        // returns `Ok(None)` when the file doesn't exist, which is how we
        // distinguish "no tags known for this repo" from a real I/O error.
        let Some(guard) = TagGuard::acquire_shared(tags_path.clone()).await? else {
            log::debug!(
                "Tags file '{}' not found for identifier '{}'.",
                tags_path.display(),
                identifier
            );
            return Ok(None);
        };
        let tags = guard.read_disk(identifier).await?;
        drop(guard);

        self.publish_to_cache(identifier.clone(), tags.clone()).await;
        Ok(Some(tags))
    }

    /// Acquires the per-repo exclusive lock, merges `fetched` into the
    /// current on-disk map, and writes the result back under the same lock.
    /// Returns the merged map so callers can publish it to the in-memory
    /// cache without a second read.
    async fn merge_under_lock(
        &self,
        identifier: &oci::Identifier,
        fetched: HashMap<String, oci::Digest>,
    ) -> Result<HashMap<String, oci::Digest>> {
        // Per-repo exclusive lock guards the read-modify-write so concurrent
        // writers in other OCX processes don't clobber each other. Existing
        // disk-only entries are preserved; identical-tag races resolve
        // last-writer-wins.
        let tags_path = self.tag_store.tags(identifier);
        let guard = TagGuard::acquire_exclusive(tags_path).await?;
        let mut merged = guard.read_disk(identifier).await?;
        merged.extend(fetched);
        guard.write_disk(identifier, &merged).await?;
        drop(guard);
        Ok(merged)
    }

    async fn publish_to_cache(&self, identifier: oci::Identifier, tags: HashMap<String, oci::Digest>) {
        self.cache.set_tags(identifier, tags).await;
    }
}
