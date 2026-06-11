// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fail-safe target-registry state loading shared by `sync` and
//! `pipeline plan`.
//!
//! Discover classifies a version as `New` when the target registry reports
//! no platforms for it — so a transient read failure must never masquerade
//! as "nothing published". Treating the two alike re-flags already-published
//! versions, and the subsequent republish re-points the version tag and
//! orphans the previously published digest (GC/pin hazard — issue #157).
//! Only an authoritative registry not-found response may classify a
//! repository or tag as absent; every other error aborts the run.

use std::collections::{BTreeMap, HashSet};

use ocx_lib::oci::client::error::ClientError;
use ocx_lib::oci::{Identifier, Platform};
use ocx_lib::package::version::Version;
use ocx_lib::publisher::Publisher;

use crate::command::sync::extract_platforms;
use crate::error::MirrorError;

/// Lists all tags on the target repository.
///
/// An authoritative "repository not found" (first publish of a new mirror)
/// yields an empty list. Any other error aborts with
/// [`MirrorError::TargetError`].
pub(crate) async fn list_target_tags(
    publisher: &Publisher,
    identifier: &Identifier,
) -> Result<Vec<String>, MirrorError> {
    tags_from_result(publisher.list_tags(identifier.clone()).await, identifier)
}

/// Fetches the already-published platform set for each version tag.
///
/// An authoritative "manifest not found" (tag listed but manifest deleted
/// since) skips the tag. Any other error aborts with
/// [`MirrorError::TargetError`].
pub(crate) async fn fetch_published_platforms(
    publisher: &Publisher,
    identifier: &Identifier,
    tags: &[&str],
) -> Result<BTreeMap<Version, HashSet<Platform>>, MirrorError> {
    let mut platform_info: BTreeMap<Version, HashSet<Platform>> = BTreeMap::new();
    for tag in tags {
        let tag_identifier = identifier.clone_with_tag((*tag).to_string());
        let result = publisher.client().fetch_manifest(&tag_identifier).await;
        merge_manifest_result(tag, result, &mut platform_info)?;
    }
    Ok(platform_info)
}

/// Classifies a `list_tags` result — fail-safe (issue #157).
fn tags_from_result(result: ocx_lib::Result<Vec<String>>, identifier: &Identifier) -> Result<Vec<String>, MirrorError> {
    match result {
        Ok(tags) => Ok(tags),
        Err(ocx_lib::Error::OciClient(ClientError::RepositoryNotFound(_))) => Ok(Vec::new()),
        Err(error) => Err(MirrorError::TargetError(format!(
            "failed to list tags for {identifier}: {error}"
        ))),
    }
}

/// Classifies a per-tag `fetch_manifest` result — fail-safe (issue #157).
fn merge_manifest_result(
    tag: &str,
    result: ocx_lib::Result<(ocx_lib::oci::Digest, ocx_lib::oci::Manifest)>,
    platform_info: &mut BTreeMap<Version, HashSet<Platform>>,
) -> Result<(), MirrorError> {
    match result {
        Ok((_, manifest)) => {
            if let Some(version) = Version::parse(tag) {
                let platforms = extract_platforms(&manifest);
                if !platforms.is_empty() {
                    platform_info.entry(version).or_default().extend(platforms);
                }
            }
            Ok(())
        }
        Err(ocx_lib::Error::OciClient(ClientError::ManifestNotFound(_))) => Ok(()),
        Err(error) => Err(MirrorError::TargetError(format!(
            "failed to fetch manifest for tag '{tag}': {error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Regression tests for issue #157 — fail-safe, not fail-open ────────
    //
    // The fail-open predecessors (`list_tags().unwrap_or_default()` and the
    // `if let Ok` / warn-and-continue manifest loops in plan.rs + sync.rs)
    // turned transient registry failures into "nothing published", re-flagging
    // published versions as New and re-pointing their tags on push.

    fn identifier() -> Identifier {
        Identifier::new_registry("mirror/cmake", "registry.test")
    }

    fn transient_error() -> ocx_lib::Error {
        ClientError::Registry("registry returned 503".into()).into()
    }

    // ── list_tags classification ──────────────────────────────────────────

    #[test]
    fn transient_list_tags_error_aborts() {
        // Fail-open would return an empty tag list here — every published
        // version would classify as New.
        let result = tags_from_result(Err(transient_error()), &identifier());
        assert!(
            matches!(result, Err(MirrorError::TargetError(_))),
            "transient list_tags failure must abort, got {result:?}"
        );
    }

    #[test]
    fn auth_error_aborts() {
        // Auth hiccups are not authoritative absence either — the catch-all
        // arm must abort for every non-not-found error class.
        let error = ClientError::Authentication("token exchange failed".into());
        let result = tags_from_result(Err(error.into()), &identifier());
        assert!(
            matches!(result, Err(MirrorError::TargetError(_))),
            "auth failure during list_tags must abort, got {result:?}"
        );
    }

    #[test]
    fn repository_not_found_means_no_tags() {
        // First publish of a new mirror repository: the registry
        // authoritatively reports the repository absent — legitimately empty.
        let error = ClientError::RepositoryNotFound("registry.test/mirror/cmake".to_string());
        let result = tags_from_result(Err(error.into()), &identifier());
        assert_eq!(result.expect("repository-absent is not an error"), Vec::<String>::new());
    }

    #[test]
    fn listed_tags_pass_through() {
        let tags = vec!["4.3.3".to_string(), "latest".to_string()];
        let result = tags_from_result(Ok(tags.clone()), &identifier());
        assert_eq!(result.expect("tags pass through"), tags);
    }

    // ── fetch_manifest classification ─────────────────────────────────────

    #[test]
    fn transient_manifest_error_aborts() {
        // Fail-open would skip the tag — the version's platform_info would
        // stay empty and the version would classify as New.
        let mut platform_info = BTreeMap::new();
        let result = merge_manifest_result("4.3.3", Err(transient_error()), &mut platform_info);
        assert!(
            matches!(result, Err(MirrorError::TargetError(_))),
            "transient fetch_manifest failure must abort, got {result:?}"
        );
    }

    #[test]
    fn manifest_not_found_skips_tag() {
        // Authoritative absence: tag listed but manifest deleted since —
        // safe to treat as not published.
        let error = ClientError::ManifestNotFound("registry.test/mirror/cmake:4.3.3".to_string());
        let mut platform_info = BTreeMap::new();
        let result = merge_manifest_result("4.3.3", Err(error.into()), &mut platform_info);
        result.expect("manifest-absent is not an error");
        assert!(platform_info.is_empty());
    }

    #[test]
    fn fetched_manifest_extends_platform_info() {
        let index = ocx_lib::oci::native::ImageIndex {
            schema_version: 2,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            artifact_type: None,
            manifests: vec![ocx_lib::oci::native::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
                size: 1,
                platform: Some(ocx_lib::oci::native::Platform {
                    architecture: ocx_lib::oci::native::Arch::Amd64,
                    os: ocx_lib::oci::native::Os::Linux,
                    os_version: None,
                    os_features: None,
                    variant: None,
                    features: None,
                }),
                annotations: None,
            }],
            annotations: None,
        };
        let manifest = ocx_lib::oci::Manifest::ImageIndex(index);
        let digest = ocx_lib::oci::Digest::Sha256("b".repeat(64));

        let mut platform_info = BTreeMap::new();
        merge_manifest_result("4.3.3", Ok((digest, manifest)), &mut platform_info).expect("manifest merges");

        let version = Version::parse("4.3.3").expect("valid version");
        let platforms = platform_info.get(&version).expect("version recorded");
        assert!(platforms.contains(&"linux/amd64".parse::<Platform>().expect("valid platform")));
    }
}
