// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Remote registry publishing facade.
//!
//! [`Publisher`] owns an OCI [`Client`](crate::oci::Client) and exposes
//! high-level push operations, including cascade tag management.
//! It is the publishing counterpart to [`PackageManager`](crate::package_manager::PackageManager),
//! which handles local-store operations.

mod layer_ref;
pub mod publish_gate;

pub use layer_ref::{ArchiveMediaType, LayerRef, LayerRefParseError};
pub use publish_gate::{PublishGateError, verify_dependency_pins};

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
#[derive(Debug)]
pub struct PushOutcome {
    /// Digest of the pushed multi-platform image index. For a multi-platform
    /// fan-out this is the primary tag's index digest after the LAST platform
    /// merge — the final state of the tag.
    pub manifest_digest: oci::Digest,
    /// Rolling cascade tags written in addition to the primary version tag
    /// (e.g. `3.28`, `3`, `latest`). Empty for a non-cascade push. For a
    /// multi-platform fan-out this is the ordered union across platforms.
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

    /// Push a package — one [`Info`] per target platform — with one or more
    /// layers to the registry.
    ///
    /// Each `LayerRef::File` is uploaded as a new blob. Each `LayerRef::Digest`
    /// is verified to exist via HEAD. The manifest contains one descriptor per
    /// layer in the order provided. Platforms are pushed **sequentially**:
    /// the per-tag index merge is a read-modify-write, so concurrent merges
    /// would race.
    ///
    /// When `build_meta` is `Some`, each identifier's tag is parsed as a
    /// [`Version`] and the build segment is attached before push (the infos
    /// share one identifier by construction, so every platform lands on the
    /// same tag). Errors if the tag does not parse, lacks `X.Y.Z` form, or
    /// already carries build metadata.
    pub async fn push(&self, infos: Vec<Info>, layers: &[LayerRef], build_meta: Option<&str>) -> Result<PushOutcome> {
        let infos = apply_build_meta_all(infos, build_meta)?;
        let mut manifest_digest: Option<oci::Digest> = None;
        for info in infos {
            log::info!(
                "pushing package with identifier {} (platform {})",
                info.identifier,
                info.platform
            );
            let (digest, _manifest) = self.client.push_package(info, layers).await?;
            manifest_digest = Some(digest);
        }
        Ok(PushOutcome {
            manifest_digest: manifest_digest.ok_or(crate::package::error::Error::EmptyPushSet)?,
            cascade_tags: Vec::new(),
        })
    }

    /// Push a package — one [`Info`] per target platform — with cascade tag
    /// management.
    ///
    /// `existing_versions` is the set of versions already in the registry,
    /// used to compute which rolling tags each platform's push should update
    /// (cascade blocker checks are platform-aware). The same `build_meta`
    /// semantics as [`Self::push`] apply. The outcome's `cascade_tags` is the
    /// ordered union across platforms.
    pub async fn push_cascade(
        &self,
        infos: Vec<Info>,
        layers: &[LayerRef],
        existing_versions: BTreeSet<Version>,
        build_meta: Option<&str>,
    ) -> Result<PushOutcome> {
        let infos = apply_build_meta_all(infos, build_meta)?;
        let mut manifest_digest: Option<oci::Digest> = None;
        let mut cascade_tags: Vec<String> = Vec::new();
        for info in infos {
            log::info!(
                "pushing package with identifier {} (cascade, platform {})",
                info.identifier,
                info.platform
            );
            let version = Version::parse(info.identifier.tag_or_latest()).ok_or_else(|| {
                crate::package::error::Error::VersionInvalid(info.identifier.tag_or_latest().to_string())
            })?;
            let (digest, tags) =
                package::cascade::push_with_cascade(&self.client, info, layers, existing_versions.clone(), &version)
                    .await?;
            manifest_digest = Some(digest);
            for tag in tags {
                if !cascade_tags.contains(&tag) {
                    cascade_tags.push(tag);
                }
            }
        }
        Ok(PushOutcome {
            manifest_digest: manifest_digest.ok_or(crate::package::error::Error::EmptyPushSet)?,
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

/// Apply [`apply_build_meta`] to every [`Info`] of a fan-out set.
///
/// The infos share one identifier (only metadata + platform differ), and the
/// build segment is a fixed string computed once by the caller — every
/// platform therefore lands on the same tag.
fn apply_build_meta_all(infos: Vec<Info>, build_meta: Option<&str>) -> Result<Vec<Info>> {
    infos
        .into_iter()
        .map(|info| apply_build_meta(info, build_meta))
        .collect()
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

    // ── Multi-platform fan-out — adr_dependency_manifest_pinning.md ──────

    #[test]
    fn build_meta_all_lands_every_platform_on_the_same_tag() {
        let mut mac = test_info("0.3.0");
        mac.platform = "darwin/arm64".parse().expect("platform parses");
        let infos =
            apply_build_meta_all(vec![test_info("0.3.0"), mac], Some("20260514120000")).expect("attach succeeds");
        let tags: Vec<_> = infos.iter().map(|info| info.identifier.tag_or_latest()).collect();
        assert_eq!(tags, vec!["0.3.0_20260514120000", "0.3.0_20260514120000"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_fan_out_set_is_an_error() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(
            StubTransportData::new(),
        ))));
        let err = publisher.push(Vec::new(), &[], None).await.expect_err("empty set");
        assert!(err.to_string().contains("at least one target platform"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fan_out_merges_every_platform_into_the_primary_index() {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};

        let data = StubTransportData::new();
        data.write().capture_pushes = true;
        let publisher = Publisher::new(oci::Client::with_transport(Box::new(StubTransport::new(data.clone()))));

        let mut mac = test_info("1.0.0");
        mac.platform = "darwin/arm64".parse().expect("platform parses");
        let outcome = publisher
            .push(vec![test_info("1.0.0"), mac], &[], None)
            .await
            .expect("fan-out push succeeds");

        // The captured primary-tag index must carry BOTH platform entries —
        // the second (sequential) merge read the first platform back and
        // appended, never clobbered.
        let inner = data.read();
        let (index_bytes, digest) = inner
            .manifests
            .get("ocx.sh/ocx:1.0.0")
            .expect("primary tag index captured");
        let index: serde_json::Value = serde_json::from_slice(index_bytes).expect("index parses");
        let platforms: Vec<String> = index["manifests"]
            .as_array()
            .expect("manifests array")
            .iter()
            .map(|entry| {
                format!(
                    "{}/{}",
                    entry["platform"]["os"].as_str().unwrap_or("?"),
                    entry["platform"]["architecture"].as_str().unwrap_or("?")
                )
            })
            .collect();
        assert_eq!(platforms, vec!["linux/amd64", "darwin/arm64"]);
        assert_eq!(
            outcome.manifest_digest.to_string(),
            *digest,
            "outcome digest must be the final (last-merge) index digest"
        );
    }
}
