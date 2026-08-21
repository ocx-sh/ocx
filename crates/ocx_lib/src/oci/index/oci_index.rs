// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use super::{IndexOperation, index_impl};
use crate::{Result, oci, oci::client::ReadAddressing, package::tag::Tag};

mod cache;
mod config;

pub use config::OciIndexConfig;

/// Remote client that **derives** an index from a plain OCI registry's tags
/// API (`adr_index_indirection.md` Decision H).
#[derive(Clone)]
pub struct OciIndex {
    client: oci::Client,
    cache: cache::SharedCache,
}

impl OciIndex {
    pub fn new(config: OciIndexConfig) -> Self {
        Self {
            client: config.client,
            cache: Default::default(),
        }
    }
}

#[async_trait]
impl index_impl::IndexImpl for OciIndex {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        if let Some(cached) = self.cache.get_repositories(registry).await {
            return Ok(cached);
        }

        let repositories = self.client.list_repositories(registry).await?;
        self.cache
            .set_repositories(registry.to_string(), repositories.clone())
            .await;
        Ok(repositories)
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        if let Some(cached) = self.cache.get_tags(identifier).await {
            return Ok(Some(cached));
        }

        let tags: Vec<String> = self
            .client
            .list_tags_addressed(identifier.clone(), ReadAddressing::Mirrored)
            .await?
            .into_iter()
            .filter(|t| !Tag::is_reserved_str(t))
            .collect();

        self.cache.set_tags(identifier.clone(), tags.clone()).await;
        Ok(Some(tags))
    }

    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        Ok(Some(
            self.client
                .fetch_manifest_addressed(identifier, ReadAddressing::Mirrored)
                .await?,
        ))
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<oci::Digest>> {
        if let Some(cached) = self.cache.get_tag_digest(identifier).await {
            return Ok(Some(cached));
        }

        // Deriving an index from a registry's tags API backs no write, so the
        // mirror is the right host to ask (Invariant #5).
        let digest = self
            .client
            .fetch_manifest_digest_addressed(identifier, crate::oci::client::ReadAddressing::Mirrored)
            .await?;
        self.cache.set_tag_digest(identifier, digest.clone()).await;
        Ok(Some(digest))
    }

    async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
        let bytes = self.client.pull_blob(blob_ref).await?;
        Ok(Some(bytes))
    }

    /// Returns the verbatim manifest bytes the registry served, digest
    /// recompute-verified by the client — the trust anchor an index store
    /// persists without re-serialisation (`adr_index_indirection.md` A3).
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        Ok(self
            .client
            .fetch_manifest_raw_bytes_addressed(identifier, ReadAddressing::Mirrored)
            .await?)
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            client: self.client.clone(),
            cache: self.cache.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};

    /// D7 at the DERIVED listing boundary. This is the site where a
    /// `sha256.<hex>` tag actually enters — `ocx package push` writes one per
    /// platform manifest by default — and it also seeds the tag cache, so a
    /// missed filter here poisons the cache for the rest of the invocation.
    #[tokio::test]
    async fn list_tags_filters_reserved_tags() {
        let data = StubTransportData::new();
        data.write().tags = vec![vec![
            "3.28".to_string(),
            "latest".to_string(),
            "__ocx.desc".to_string(),
            "__OCX.future".to_string(),
            format!("sha256.{}", "a".repeat(64)),
        ]];
        let index = OciIndex::new(OciIndexConfig {
            client: oci::Client::with_transport(Box::new(StubTransport::new(data))),
        });

        let tags = index_impl::IndexImpl::list_tags(&index, &oci::Identifier::new_registry("ns/pkg", "example.com"))
            .await
            .unwrap()
            .expect("the stub answers");
        assert_eq!(tags, vec!["3.28".to_string(), "latest".to_string()]);
    }
}
