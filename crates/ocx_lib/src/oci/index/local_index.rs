// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;
use std::collections::HashMap;

use crate::{Result, file_structure::IndexStore, log, oci, prelude::*};

use super::index_impl;

mod cache;
mod config;

pub use config::Config;

#[derive(Clone)]
pub struct LocalIndex {
    index_store: IndexStore,
    cache: cache::SharedCache,
}

impl LocalIndex {
    pub fn new(config: Config) -> Self {
        Self {
            index_store: IndexStore::new(config.root),
            cache: cache::SharedCache::default(),
        }
    }

    /// Updates the local index with the tags and manifests from the given index for the specified identifier.
    /// Usually you would want to call this method with a `RemoteIndex` to sync the local index with the remote registry.
    pub async fn update(&mut self, index: &super::Index, identifier: &oci::Identifier) -> Result<()> {
        let other_tags = index.list_tags(identifier).await?.unwrap_or_default();
        let mut this_tags = self.get_tags(identifier).await?.unwrap_or_default();

        for tag in other_tags {
            log::info!("Updating tag '{}' for identifier '{}'.", tag, identifier);
            let identifier = identifier.clone_with_tag(&tag);

            if let Some(this_digest) = this_tags.get(&tag) {
                let Some(other_digest) = index.fetch_manifest_digest(&identifier).await? else {
                    log::debug!("Remote has no digest for tag '{}' — skipping.", tag);
                    continue;
                };

                if this_digest == &other_digest {
                    log::debug!(
                        "Tag '{}' for identifier '{}' is up to date with digest '{}'.",
                        tag,
                        identifier,
                        this_digest
                    );
                    continue;
                }

                self.update_manifest(index, &identifier, &other_digest).await?;
                this_tags.insert(tag, other_digest);
            } else {
                let Some(digest) = index.fetch_manifest_digest(&identifier).await? else {
                    log::debug!("Remote has no digest for tag '{}' — skipping.", tag);
                    continue;
                };
                let path = self.index_store.manifest(&identifier, &digest);

                if !path.exists() {
                    self.update_manifest(index, &identifier, &digest).await?;
                }
                this_tags.insert(tag, digest);
            }
        }

        let tags_path = self.index_store.tags(identifier);
        this_tags.write_json_to_path(tags_path)?;

        {
            let cache = self.cache.write().await;
            cache.set_tags(identifier.clone(), this_tags).await;
        }

        Ok(())
    }

    async fn update_manifest(
        &mut self,
        index: &super::Index,
        identifier: &oci::Identifier,
        digest: &oci::Digest,
    ) -> Result<()> {
        let (_, manifest) = index.fetch_manifest(identifier).await?.ok_or_else(|| {
            crate::Error::UndefinedWithMessage(format!(
                "Remote manifest not found for '{identifier}' during index update"
            ))
        })?;
        let path = self.index_store.manifest(identifier, digest);
        manifest.write_json_to_path(path)?;

        if let oci::Manifest::ImageIndex(image_index) = manifest {
            for manifest in image_index.manifests {
                let digest = manifest.digest.clone().try_into()?;
                let identifier = identifier.clone_with_digest(digest);
                let (digest, manifest) = index.fetch_manifest(&identifier).await?.ok_or_else(|| {
                    crate::Error::UndefinedWithMessage(format!(
                        "Remote platform manifest not found for '{identifier}' during index update"
                    ))
                })?;
                let path = self.index_store.manifest(&identifier, &digest);
                manifest.write_json_to_path(path)?;
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

        let tags_path = self.index_store.tags(identifier);
        if !tags_path.exists() {
            log::debug!(
                "Tags file '{}' not found for identifier '{}'.",
                tags_path.display(),
                identifier
            );
            return Ok(None);
        }

        let tags = HashMap::<String, oci::Digest>::read_json_from_path(tags_path)?;
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

        let manifest_path = self.index_store.manifest(identifier, digest);
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
        let manifest = oci::Manifest::read_json_from_path(manifest_path)?;
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
        self.index_store.list_repositories(registry)
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        Ok(self.get_tags(identifier).await?.map(|tags| tags.into_keys().collect()))
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
            index_store: self.index_store.clone(),
            cache: self.cache.clone(),
        })
    }
}
