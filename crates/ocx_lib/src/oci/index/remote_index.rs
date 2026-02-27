use async_trait::async_trait;

use super::index_impl;
use crate::{Result, oci};

mod cache;
mod config;

pub use config::Config;

#[derive(Clone)]
pub struct Index {
    client: oci::Client,
    cache : cache::SharedCache,
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
    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Vec<String>> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get_tags(identifier).await {
                return Ok(cached);
            }
        }

        let tags = self.client.list_tags(identifier.clone()).await?;

        {
            let cache = self.cache.write().await;
            cache.set_tags(identifier.clone(), tags.clone()).await;
        }

        Ok(tags)
    }

    async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<(oci::Digest, oci::Manifest)> {
        self.client.fetch_manifest(identifier).await
    }

    async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<oci::Digest> {
        {
            let cache = self.cache.read().await;
            if let Some(cached) = cache.get_tag_digest(identifier).await {
                return Ok(cached);
            }
        }

        let digest = self.client.fetch_manifest_digest(identifier).await?;
        {
            let cache = self.cache.write().await;
            cache.set_tag_digest(identifier, digest.clone()).await;
        }

        Ok(digest)
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            client: self.client.clone(),
            cache: self.cache.clone(),
        })
    }
}
