// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use crate::{Result, oci};

use super::IndexOperation;

#[async_trait]
pub trait IndexImpl: Send + Sync {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>>;

    /// List all user-visible tags for the given identifier.
    ///
    /// Internal tags ([`Tag::Internal`](crate::package::tag::Tag::Internal)) must be
    /// filtered out by every implementation.
    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>>;

    /// Fetch the manifest for the given identifier.
    ///
    /// Pure-read callers must pass [`IndexOperation::Query`]; install/pull
    /// callers pass [`IndexOperation::Resolve`]. The trait does not validate
    /// this — misuse silently leaks writes through query paths. The
    /// [`IndexOperation`] enum exists to make the choice unmissable at every
    /// call site.
    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>>;
    /// Fetch the manifest digest for the given identifier.
    ///
    /// `op` carries the same contract as on [`Self::fetch_manifest`].
    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<oci::Digest>>;

    fn box_clone(&self) -> Box<dyn IndexImpl>;
}
