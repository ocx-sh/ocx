// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use crate::{Result, oci};

#[async_trait]
pub trait IndexImpl: Send + Sync {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>>;

    /// List all user-visible tags for the given identifier.
    ///
    /// Internal tags ([`Tag::Internal`](crate::package::tag::Tag::Internal)) must be
    /// filtered out by every implementation.
    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>>;

    async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, oci::Manifest)>>;
    async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>>;

    fn box_clone(&self) -> Box<dyn IndexImpl>;
}
