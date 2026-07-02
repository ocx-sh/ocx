// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Remote registry publishing facade.
//!
//! [`Publisher`] owns an OCI [`Client`](crate::oci::Client) and exposes
//! high-level push operations, including cascade tag management.
//! It is the publishing counterpart to [`PackageManager`](crate::package_manager::PackageManager),
//! which handles local-store operations.

mod layer_ref;

pub use layer_ref::{ArchiveMediaType, LayerRef, LayerRefParseError};

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

/// Outcome of a successful package push.
///
/// Surfaced so callers (notably the `ocx package push` command) can emit a
/// structured report; `ocx-mirror pipeline push` parses this report to record
/// the cascade tags written and to distinguish a real publish from a no-op.
pub struct PushOutcome {
    /// Digest of the pushed multi-platform image index.
    pub manifest_digest: oci::Digest,
    /// Rolling cascade tags written in addition to the primary version tag
    /// (e.g. `3.28`, `3`, `latest`). Empty for a non-cascade push.
    pub cascade_tags: Vec<String>,
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
    ///
    /// When `build_meta` is `Some`, the identifier's tag is parsed as a
    /// [`Version`] and the build segment is attached before push. Errors if
    /// the tag does not parse, lacks `X.Y.Z` form, or already carries build
    /// metadata.
    pub async fn push(&self, info: Info, layers: &[LayerRef], build_meta: Option<&str>) -> Result<PushOutcome> {
        let info = apply_build_meta(info, build_meta)?;
        log::info!("pushing package with identifier {}", info.identifier);
        let (manifest_digest, _manifest) = self.client.push_package(info, layers).await?;
        Ok(PushOutcome {
            manifest_digest,
            cascade_tags: Vec::new(),
        })
    }

    /// Push a package with cascade tag management.
    ///
    /// `existing_versions` is the set of versions already in the registry,
    /// used to compute which rolling tags this push should update. The same
    /// `build_meta` semantics as [`Self::push`] apply; cascade derived tags
    /// always operate on the version core regardless of build segment.
    pub async fn push_cascade(
        &self,
        info: Info,
        layers: &[LayerRef],
        existing_versions: BTreeSet<Version>,
        build_meta: Option<&str>,
    ) -> Result<PushOutcome> {
        let info = apply_build_meta(info, build_meta)?;
        log::info!("pushing package with identifier {} (cascade)", info.identifier);
        let version = Version::parse(info.identifier.tag_or_latest())
            .ok_or_else(|| crate::package::error::Error::VersionInvalid(info.identifier.tag_or_latest().to_string()))?;

        let (manifest_digest, cascade_tags) =
            package::cascade::push_with_cascade(&self.client, info, layers, existing_versions, &version).await?;
        Ok(PushOutcome {
            manifest_digest,
            cascade_tags,
        })
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

/// If `build_meta` is `Some`, parse the identifier's tag, attach the build
/// segment, and return an [`Info`] whose identifier carries the new tag.
fn apply_build_meta(mut info: Info, build_meta: Option<&str>) -> Result<Info> {
    let Some(build) = build_meta else { return Ok(info) };
    let tag = info.identifier.tag_or_latest();
    let version = Version::parse(tag).ok_or_else(|| crate::package::error::Error::VersionInvalid(tag.to_string()))?;
    let with_build = version.with_build(build).map_err(crate::package::error::Error::from)?;
    info.identifier = info.identifier.clone_with_tag(with_build.to_string());
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::{
        Entrypoints, Metadata,
        bundle::{self, Bundle},
        dependency, env as metadata_env,
    };

    fn test_info(tag: &str) -> Info {
        let identifier = oci::Identifier::new_registry("ocx", "ocx.sh").clone_with_tag(tag);
        let metadata = Metadata::Bundle(Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        Info {
            identifier,
            metadata,
            platform: "linux/amd64".parse().expect("platform parses"),
        }
    }

    #[test]
    fn none_returns_info_unchanged() {
        let info = test_info("mirror-0.3.0-dev");
        let out = apply_build_meta(info.clone(), None).expect("no-op succeeds");
        assert_eq!(out.identifier.tag_or_latest(), "mirror-0.3.0-dev");
    }

    #[test]
    fn attaches_build_meta_to_variant_prerelease() {
        let info = test_info("mirror-0.3.0-dev");
        let out = apply_build_meta(info, Some("20260514120000")).expect("attach succeeds");
        // Display normalizes `+` to `_` per OCI tag rules; clone_with_tag does the same.
        assert_eq!(out.identifier.tag_or_latest(), "mirror-0.3.0-dev_20260514120000");
    }

    #[test]
    fn attaches_build_meta_to_bare_patch_version() {
        let info = test_info("0.3.0");
        let out = apply_build_meta(info, Some("20260514120000")).expect("attach succeeds");
        assert_eq!(out.identifier.tag_or_latest(), "0.3.0_20260514120000");
    }

    #[test]
    fn rejects_tag_that_already_carries_build_meta() {
        let info = test_info("0.3.0-dev_alreadyhere");
        let err = apply_build_meta(info, Some("20260514120000")).expect_err("must reject double build meta");
        let msg = err.to_string();
        assert!(msg.contains("already has build metadata"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_tag_that_is_not_a_valid_version() {
        let info = test_info("latest");
        let err = apply_build_meta(info, Some("20260514120000")).expect_err("must reject non-version tag");
        let msg = err.to_string();
        assert!(msg.contains("invalid package version"), "unexpected error: {msg}");
    }

    #[test]
    fn rejects_tag_that_lacks_patch_segment() {
        let info = test_info("1.2");
        let err = apply_build_meta(info, Some("20260514120000")).expect_err("must reject X.Y tag");
        let msg = err.to_string();
        assert!(msg.contains("X.Y.Z"), "unexpected error: {msg}");
    }
}
