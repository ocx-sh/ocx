// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Remote registry publishing facade.
//!
//! [`Publisher`] owns an OCI [`Client`](crate::oci::Client) and exposes
//! high-level push operations, including cascade tag management.
//! It is the publishing counterpart to [`PackageManager`](crate::package_manager::PackageManager),
//! which handles local-store operations.

mod layer_ref;

pub use layer_ref::LayerRef;

use std::collections::BTreeSet;
use std::path::Path;

use crate::{
    log, oci,
    package::{self, description::Description, info::Info, version::Version},
    prelude::*,
};

/// Remote registry publishing facade.
///
/// Holds an OCI client and provides push operations with optional
/// cascade tag management. Does not depend on local file structure
/// or index — only on the remote registry via the client.
#[derive(Clone)]
pub struct Publisher {
    client: oci::Client,
}

impl Publisher {
    pub fn new(client: oci::Client) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &oci::Client {
        &self.client
    }

    /// Pre-authenticate against the registry for `identifier` with Push scope.
    ///
    /// Call at the start of a publishing command to fail fast on credential
    /// issues before reading files or doing any other preparation.
    pub async fn ensure_auth(&self, identifier: &oci::Identifier) -> Result<()> {
        self.client.ensure_auth(identifier, oci::RegistryOperation::Push).await
    }

    /// Push a package with one or more layers to the registry.
    ///
    /// Each `LayerRef::File` is uploaded as a new blob. Each `LayerRef::Digest`
    /// is verified to exist via HEAD. The manifest contains one descriptor per
    /// layer in the order provided.
    pub async fn push(&self, info: Info, layers: &[LayerRef]) -> Result<()> {
        log::debug!("Pushing package with identifier {}", info.identifier);
        self.client.push_package(info, layers).await?;
        Ok(())
    }

    /// Push a package with cascade tag management.
    ///
    /// `existing_versions` is the set of versions already in the registry,
    /// used to compute which rolling tags this push should update.
    pub async fn push_cascade(
        &self,
        info: Info,
        layers: &[LayerRef],
        existing_versions: BTreeSet<Version>,
    ) -> Result<()> {
        let version = Version::parse(info.identifier.tag_or_latest())
            .ok_or_else(|| crate::package::error::Error::VersionInvalid(info.identifier.tag_or_latest().to_string()))?;

        package::cascade::push_with_cascade(&self.client, info, layers, existing_versions, &version).await
    }

    /// Push a complete description artifact to the `__ocx.desc` tag.
    pub async fn push_description(&self, identifier: &oci::Identifier, description: &Description) -> Result<()> {
        log::debug!("Pushing description for {}", identifier);
        self.client.push_description(identifier, description).await?;
        Ok(())
    }

    /// Pull the existing description from the `__ocx.desc` tag.
    ///
    /// Returns `Ok(None)` if no description exists yet.
    pub async fn pull_description(&self, identifier: &oci::Identifier, temp_dir: &Path) -> Result<Option<Description>> {
        Ok(self.client.pull_description(identifier, temp_dir).await?)
    }

    /// Lists existing tags for the given identifier from the registry.
    ///
    /// Convenience method for callers that need to fetch existing versions
    /// before calling [`push_cascade`](Self::push_cascade).
    pub async fn list_tags(&self, identifier: oci::Identifier) -> Result<Vec<String>> {
        self.client.list_tags(identifier).await
    }

    /// Parses a list of tag strings into a set of valid versions,
    /// skipping tags that are not valid versions.
    pub fn parse_versions(tags: &[String]) -> BTreeSet<Version> {
        tags.iter().filter_map(|t| Version::parse(t)).collect()
    }
}
