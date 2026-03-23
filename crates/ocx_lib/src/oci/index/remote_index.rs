// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use super::index_impl;
use crate::{Result, oci, package::tag::Tag};

mod cache;
mod config;

pub use config::Config;

#[derive(Clone)]
pub struct Index {
    client: oci::Client,
    cache: cache::SharedCache,
}

impl Index {
    pub fn new(config: Config) -> Self {
        Self {
            client: config.client,
            cache: Default::default(),
        }
    }
}

#[async_trait]
impl index_impl::IndexImpl for Index {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get_repositories(registry).await {
                return Ok(cached);
            }
        }

        let repositories = self.client.list_repositories(registry).await?;

        {
            let cache = self.cache.write().await;
            cache.set_repositories(registry.to_string(), repositories.clone()).await;
        }

        Ok(repositories)
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get_tags(identifier).await {
                return Ok(Some(cached));
            }
        }

        let tags: Vec<String> = self
            .client
            .list_tags(identifier.clone())
            .await?
            .into_iter()
            .filter(|t| !Tag::is_internal_str(t))
            .collect();

        {
            let cache = self.cache.write().await;
            cache.set_tags(identifier.clone(), tags.clone()).await;
        }

        Ok(Some(tags))
    }

    async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        Ok(Some(self.client.fetch_manifest(identifier).await?))
    }

    async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get_tag_digest(identifier).await {
                return Ok(Some(cached));
            }
        }

        let digest = self.client.fetch_manifest_digest(identifier).await?;
        {
            let cache = self.cache.write().await;
            cache.set_tag_digest(identifier, digest.clone()).await;
        }

        Ok(Some(digest))
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            client: self.client.clone(),
            cache: self.cache.clone(),
        })
    }
}
