// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use super::error::Result;
use super::error::broadcast_failure;
use super::{IndexOperation, error, index_impl};
use ocx_oci::client::ReadAddressing;
use ocx_oci::tag::is_reserved_tag;

use ocx_util::singleflight::Acquisition;

mod cache;
mod config;

pub use config::OciIndexConfig;

/// Remote client that **derives** an index from a plain OCI registry's tags
/// API (`adr_index_indirection.md` Decision H).
#[derive(Clone)]
pub struct OciIndex {
    client: ocx_oci::Client,
    cache: cache::SharedCache,
    addressing: ReadAddressing,
}

impl OciIndex {
    /// A mirror may answer: for reads that back no write.
    pub fn new(config: OciIndexConfig) -> Self {
        Self {
            client: config.client,
            cache: Default::default(),
            addressing: ReadAddressing::Mirrored,
        }
    }

    /// Every read addresses the canonical registry, never a mirror: for a refresh that must
    /// observe a write this process just made there.
    pub fn canonical(config: OciIndexConfig) -> Self {
        Self {
            addressing: ReadAddressing::Canonical,
            ..Self::new(config)
        }
    }

    /// One tags-API listing, reserved names filtered out before they seed the tag cache.
    async fn fetch_tags(&self, identifier: &ocx_oci::PackageRef) -> Result<Vec<String>> {
        Ok(self
            .client
            .list_tags_addressed(ocx_oci::OciIdentifier::passthrough(identifier), self.addressing)
            .await?
            .into_iter()
            .filter(|tag| !is_reserved_tag(tag))
            .collect())
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

    async fn list_tags(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
        if let Some(cached) = self.cache.get_tags(identifier).await {
            return Ok(Some(cached));
        }

        // Coalesce cold misses, or a fan-out over one repository calls the tags API once per task.
        let handle = match self
            .cache
            .tag_group()
            .try_acquire(identifier.clone())
            .await
            .map_err(error::Error::SingleflightFailed)?
        {
            Acquisition::Leader(handle) => handle,
            Acquisition::Resolved(tags) => return Ok(Some(tags)),
        };
        match self.fetch_tags(identifier).await {
            Ok(tags) => {
                self.cache.set_tags(identifier.clone(), tags.clone()).await;
                handle.complete(tags.clone());
                Ok(Some(tags))
            }
            Err(error) => Err(broadcast_failure(handle, error)),
        }
    }

    async fn fetch_manifest(
        &self,
        identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        Ok(Some(
            self.client
                .fetch_manifest_addressed(&ocx_oci::OciIdentifier::passthrough(identifier), self.addressing)
                .await?,
        ))
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>> {
        if let Some(cached) = self.cache.get_tag_digest(identifier).await {
            return Ok(Some(cached));
        }

        let handle = match self
            .cache
            .tag_digest_group()
            .try_acquire(identifier.clone())
            .await
            .map_err(error::Error::SingleflightFailed)?
        {
            Acquisition::Leader(handle) => handle,
            Acquisition::Resolved(digest) => return Ok(Some(digest)),
        };
        match self
            .client
            .fetch_manifest_digest_addressed(&ocx_oci::OciIdentifier::passthrough(identifier), self.addressing)
            .await
        {
            Ok(digest) => {
                self.cache.set_tag_digest(identifier, digest.clone()).await;
                handle.complete(digest.clone());
                Ok(Some(digest))
            }
            Err(error) => Err(broadcast_failure(handle, error.into())),
        }
    }

    async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
        let location = ocx_oci::OciIdentifier::passthrough(blob_ref.as_identifier()).at_pin_of(blob_ref);
        let bytes = self.client.pull_blob(&location).await?;
        Ok(Some(bytes))
    }

    /// Returns the verbatim, digest-verified manifest bytes the registry served.
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        Ok(self
            .client
            .fetch_manifest_raw_bytes_addressed(&ocx_oci::OciIdentifier::passthrough(identifier), self.addressing)
            .await?)
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            client: self.client.clone(),
            cache: self.cache.clone(),
            addressing: self.addressing,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

    /// D7 at the DERIVED listing boundary. This is the site where a
    /// `__ocx.keep.<algorithm>-<hex>` tag actually enters — `ocx package push` writes one per
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
            format!("__ocx.keep.sha256-{}", "a".repeat(64)),
        ]];
        let index = OciIndex::new(OciIndexConfig {
            client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(data))),
        });

        let tags =
            index_impl::IndexImpl::list_tags(&index, &ocx_oci::PackageRef::new_registry("ns/pkg", "example.com"))
                .await
                .unwrap()
                .expect("the stub answers");
        assert_eq!(tags, vec!["3.28".to_string(), "latest".to_string()]);
    }

    fn call_count(data: &StubTransportData, method: &str) -> usize {
        data.read().calls.iter().filter(|call| *call == method).count()
    }

    /// Concurrent `list_tags` for one repository produces one tags-API call.
    ///
    /// `tag_read_delay` holds the answer, and virtual time only advances once
    /// every task is parked, so the leader cannot finish before all N callers
    /// have arrived. *Red-reachability:* without the coalescing group the count
    /// is N, and the stub's FIFO tag pages make it visible a second way — only
    /// the first caller would receive a page.
    #[tokio::test(start_paused = true)]
    async fn concurrent_list_tags_for_one_repository_produces_one_call() {
        const CALLERS: usize = 8;
        let data = StubTransportData::new();
        data.write().tags = vec![vec!["3.28".to_string(), "latest".to_string()]];
        data.write().tag_read_delay = Some(std::time::Duration::from_secs(1));
        let index = OciIndex::new(OciIndexConfig {
            client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))),
        });
        let identifier = ocx_oci::PackageRef::new_registry("ns/pkg", "example.com");

        let results = futures::future::join_all(
            (0..CALLERS).map(|_| async { index_impl::IndexImpl::list_tags(&index, &identifier).await }),
        )
        .await;
        for result in results {
            assert_eq!(
                result.unwrap().expect("the stub answers"),
                vec!["3.28".to_string(), "latest".to_string()],
                "every caller gets the leader's page, not an exhausted queue"
            );
        }

        assert_eq!(
            call_count(&data, "list_tags"),
            1,
            "one repository's concurrent tag listings coalesce onto one leader"
        );
    }

    /// Concurrent `fetch_manifest_digest` for one tag produces one HEAD.
    /// Same held-answer shape, width 8, one tag.
    #[tokio::test(start_paused = true)]
    async fn concurrent_fetch_manifest_digest_for_one_tag_produces_one_call() {
        const CALLERS: usize = 8;
        let data = StubTransportData::new();
        data.write().digest = Some(format!("sha256:{}", "a".repeat(64)));
        data.write().tag_read_delay = Some(std::time::Duration::from_secs(1));
        let index = OciIndex::new(OciIndexConfig {
            client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))),
        });
        let identifier = ocx_oci::PackageRef::new_registry("ns/pkg", "example.com").clone_with_tag("3.28");

        let results = futures::future::join_all((0..CALLERS).map(|_| async {
            index_impl::IndexImpl::fetch_manifest_digest(&index, &identifier, IndexOperation::Query).await
        }))
        .await;
        for result in results {
            result.unwrap().expect("the stub answers");
        }

        assert_eq!(
            call_count(&data, "fetch_manifest_digest"),
            1,
            "one tag's concurrent digest reads coalesce onto one leader"
        );
    }

    /// A failed digest read memoizes nothing, so a repeat ask re-requests —
    /// the `OciIndex` half of C-006(b), and the reason the shared primitive
    /// evicts a failed key on read. Recovers once the registry heals.
    #[tokio::test]
    async fn a_failed_digest_read_is_re_requested_and_recovers() {
        let data = StubTransportData::new();
        let identifier = ocx_oci::PackageRef::new_registry("ns/pkg", "example.com").clone_with_tag("3.28");
        data.write()
            .manifest_errors
            .insert("example.com/ns/pkg:3.28".to_string(), "registry is down".to_string());
        let index = OciIndex::new(OciIndexConfig {
            client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))),
        });

        index_impl::IndexImpl::fetch_manifest_digest(&index, &identifier, IndexOperation::Query)
            .await
            .expect_err("the read fails");
        index_impl::IndexImpl::fetch_manifest_digest(&index, &identifier, IndexOperation::Query)
            .await
            .expect_err("and fails again");
        assert_eq!(
            call_count(&data, "fetch_manifest_digest"),
            2,
            "a transport failure is not a result: the repeat ask re-requests"
        );

        data.write().manifest_errors.clear();
        data.write().digest = Some(format!("sha256:{}", "a".repeat(64)));
        index_impl::IndexImpl::fetch_manifest_digest(&index, &identifier, IndexOperation::Query)
            .await
            .expect("a transient outage must not poison the tag for the life of the process")
            .expect("the stub answers");
    }

    /// The manifest read a tagged refresh makes goes to the mirror by default and past it when
    /// built `canonical`, so a refresh right after a push cannot pin what a stale mirror serves.
    #[tokio::test]
    async fn canonical_reads_bypass_a_configured_mirror() {
        let identifier = ocx_oci::PackageRef::new_registry("ns/pkg", "example.com").clone_with_tag("3.28");
        let read_host = |canonical: bool| {
            let identifier = identifier.clone();
            async move {
                let data = StubTransportData::new();
                let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(data.clone())))
                    .with_test_mirror("example.com", "mirror.test", "cache");
                let config = OciIndexConfig { client };
                let index = if canonical {
                    OciIndex::canonical(config)
                } else {
                    OciIndex::new(config)
                };
                let _ = index_impl::IndexImpl::fetch_manifest_raw_bytes(&index, &identifier).await;
                let targets = data.read().read_targets.clone();
                assert_eq!(targets.len(), 1, "one manifest read: {targets:?}");
                (targets[0].1.clone(), targets[0].2.clone())
            }
        };

        assert_eq!(
            read_host(false).await,
            ("mirror.test".to_string(), "cache/ns/pkg".to_string())
        );
        assert_eq!(
            read_host(true).await,
            ("example.com".to_string(), "ns/pkg".to_string()),
            "a canonical index must read the registry the push wrote, unrewritten"
        );
    }
}
