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

    /// Updates the local index with the tags and manifests from the given index for the specified identifier.
    /// Usually you would want to call this method with a `RemoteIndex` to sync the local index with the remote registry.
    ///
    /// When the identifier includes a tag (e.g., `cmake:3.28`), only that single tag is fetched — no tag
    /// listing is performed. When the identifier has no tag (e.g., `cmake`), all tags are fetched.
    pub async fn update(&self, index: &super::Index, identifier: &oci::Identifier) -> Result<()> {
        if identifier.tag().is_some() {
            self.update_tag(index, identifier).await
        } else {
            self.update_all_tags(index, identifier).await
        }
    }

    /// Updates a single tag in the local index without listing all remote tags.
    pub(super) async fn update_tag(&self, index: &super::Index, identifier: &oci::Identifier) -> Result<()> {
        let tag = identifier.tag_or_latest().to_owned();
        let mut this_tags = self.get_tags(identifier).await?.unwrap_or_default();

        self.sync_tag(index, identifier, &tag, &mut this_tags).await?;
        self.persist_tags(identifier, this_tags).await
    }

    /// Updates all tags in the local index by listing remote tags and syncing each one.
    async fn update_all_tags(&self, index: &super::Index, identifier: &oci::Identifier) -> Result<()> {
        let remote_tags = index.list_tags(identifier).await?.unwrap_or_default();
        let mut this_tags = self.get_tags(identifier).await?.unwrap_or_default();

        for tag in remote_tags {
            let tagged = identifier.clone_with_tag(&tag);
            self.sync_tag(index, &tagged, &tag, &mut this_tags).await?;
        }

        self.persist_tags(identifier, this_tags).await
    }

    /// Syncs a single tag from the remote index into `local_tags`.
    ///
    /// Fetches the digest for the tag, skips if unchanged, and updates the manifest if needed.
    async fn sync_tag(
        &self,
        index: &super::Index,
        identifier: &oci::Identifier,
        tag: &str,
        local_tags: &mut HashMap<String, oci::Digest>,
    ) -> Result<()> {
        log::info!("Updating tag '{}' for identifier '{}'.", tag, identifier);

        let Some(digest) = index.fetch_manifest_digest(identifier).await? else {
            log::debug!("Remote has no digest for tag '{}' — skipping.", tag);
            return Ok(());
        };

        if let Some(existing_digest) = local_tags.get(tag)
            && existing_digest == &digest
        {
            log::debug!(
                "Tag '{}' for identifier '{}' is up to date with digest '{}'.",
                tag,
                identifier,
                existing_digest
            );
            return Ok(());
        }

        let manifest_path = self.blob_store.data(identifier.registry(), &digest);
        if !manifest_path.exists() {
            self.update_manifest(index, identifier, &digest).await?;
        }

        local_tags.insert(tag.to_owned(), digest);
        Ok(())
    }

    /// Writes the tags map to disk as a versioned [`TagLock`] and updates the in-memory cache.
    async fn persist_tags(&self, identifier: &oci::Identifier, tags: HashMap<String, oci::Digest>) -> Result<()> {
        let tags_path = self.tag_store.tags(identifier);
        let tag_lock = tag_lock::TagLock::new(identifier, tags.clone());
        tag_lock.write_json(&tags_path).await?;

        let cache = self.cache.write().await;
        cache.set_tags(identifier.clone(), tags).await;

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
        if !tags_path.exists() {
            log::debug!(
                "Tags file '{}' not found for identifier '{}'.",
                tags_path.display(),
                identifier
            );
            return Ok(None);
        }

        let tag_lock = tag_lock::TagLock::read_json(&tags_path).await?;
        let tags = tag_lock.into_tags(identifier, &tags_path)?;
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
