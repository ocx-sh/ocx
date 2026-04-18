// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cascade algebra and platform-aware push orchestration.
//!
//! The cascade algebra ([`decompose`], [`cascade`]) computes which rolling
//! tags a build-tagged version should update, based on the set of existing
//! versions and blocking ranges.
//!
//! The orchestration layer ([`resolve_cascade_tags`], [`push_with_cascade`])
//! composes the algebra with [`Client`](crate::oci::Client) OCI transport
//! to implement cascade pushes that correctly handle multi-platform registries.

use std::collections::BTreeSet;
use std::ops::Bound::{Excluded, Unbounded};

use crate::{
    log, oci,
    package::{self, version::Version},
    prelude::*,
};

// ── Cascade algebra ─────────────────────────────────────────────

/// One level in the cascade chain.
pub struct CascadeLevel {
    /// The rolling tag to cascade to (e.g., 3.28.1, 3.28, 3).
    pub target: Version,
    /// Versions in the blocking range (current, target) that could prevent cascade.
    pub blockers: Vec<Version>,
}

/// Result of [`decompose`].
pub struct CascadeDecomposition {
    /// Cascade levels from most-specific to least-specific.
    pub levels: Vec<CascadeLevel>,
    /// Whether this version is eligible to become `latest`.
    /// Always `false` for pre-release versions.
    pub latest_eligible: bool,
    /// Versions above the highest cascade level that would prevent becoming
    /// `latest`. Only meaningful when `latest_eligible` is `true`.
    pub latest_blockers: Vec<Version>,
}

/// Decomposes a version's cascade into discrete levels with pre-computed blocking ranges.
///
/// Pre-releases without build produce zero levels (no cascade).
/// Pre-releases with build produce at most one level (cascade to parent pre-release).
/// Pre-releases are never eligible for `latest`.
pub fn decompose(version: &Version, others: &BTreeSet<Version>) -> CascadeDecomposition {
    // Pre-releases without build never cascade beyond their own level.
    if version.has_prerelease() && !version.has_build() {
        return CascadeDecomposition {
            levels: vec![],
            latest_eligible: false,
            latest_blockers: vec![],
        };
    }

    // Pre-releases with build: cascade only to the parent pre-release.
    if version.has_prerelease() {
        let parent = version
            .parent()
            .expect("Versions with build fragment shall always have a parent.");
        let blockers: Vec<Version> = others
            .range((Excluded(version), Unbounded))
            .take_while(|v| {
                v.variant() == version.variant()
                    && v.major() == version.major()
                    && v.minor() == version.minor()
                    && v.patch() == version.patch()
                    && v.prerelease() == version.prerelease()
            })
            .cloned()
            .collect();
        return CascadeDecomposition {
            levels: vec![CascadeLevel {
                target: parent,
                blockers,
            }],
            latest_eligible: false,
            latest_blockers: vec![],
        };
    }

    let mut levels = Vec::new();
    let mut current = version.clone();

    loop {
        let parent = current.parent();
        match parent {
            Some(parent) => {
                let blockers: Vec<Version> = others.range((Excluded(&current), Excluded(&parent))).cloned().collect();
                levels.push(CascadeLevel {
                    target: parent.clone(),
                    blockers,
                });
                current = parent;
            }
            None => {
                // Filter to same variant track: take_while works because Ord
                // clusters all same-variant versions together.
                let latest_blockers: Vec<Version> = others
                    .range((Excluded(&current), Unbounded))
                    .take_while(|v| v.variant() == version.variant())
                    .cloned()
                    .collect();
                return CascadeDecomposition {
                    levels,
                    latest_eligible: true,
                    latest_blockers,
                };
            }
        }
    }
}

/// Computes the cascade chain for a version given existing versions.
///
/// Not platform-aware — stops at the first level with any blocker.
/// Use [`resolve_cascade_tags`] for the full platform-aware workflow.
pub fn cascade(version: &Version, others: impl IntoIterator<Item = Version>) -> (Vec<Version>, bool) {
    let others = others.into_iter().collect::<BTreeSet<_>>();
    let decomposition = decompose(version, &others);

    let mut versions = vec![version.clone()];
    for level in &decomposition.levels {
        if !level.blockers.is_empty() {
            return (versions, false);
        }
        versions.push(level.target.clone());
    }

    let is_latest = decomposition.latest_eligible && decomposition.latest_blockers.is_empty();
    (versions, is_latest)
}

// ── Platform-aware orchestration ────────────────────────────────

/// Resolves cascade tags by walking [`decompose`] levels and checking
/// each level's blockers for platform membership.
///
/// Returns the list of tag strings to cascade to (excluding the primary
/// tag) and whether this version should also become `latest`.
///
/// Registry errors on blocker verification stop the cascade conservatively
/// (with a warning) rather than propagating — a transient fetch failure
/// should not abort the entire push.
pub async fn resolve_cascade_tags(
    client: &oci::Client,
    identifier: &oci::Identifier,
    version: &Version,
    other_versions: &BTreeSet<Version>,
    platform: &oci::Platform,
) -> Result<(Vec<String>, bool)> {
    let decomposition = decompose(version, other_versions);
    let mut tags = Vec::new();

    for level in &decomposition.levels {
        match has_blocking_platform(client, identifier, &level.blockers, platform).await {
            Ok(true) => return Ok((tags, false)),
            Ok(false) => tags.push(level.target.to_string()),
            Err(e) => {
                log::warn!("Cascade stopped at {}: could not verify blocker — {e}", level.target);
                return Ok((tags, false));
            }
        }
    }

    let is_latest = if decomposition.latest_eligible {
        match has_blocking_platform(client, identifier, &decomposition.latest_blockers, platform).await {
            Ok(blocked) => !blocked,
            Err(e) => {
                log::warn!("Cascade skipping latest: could not verify blocker — {e}");
                false
            }
        }
    } else {
        false
    };
    if is_latest {
        match version.variant() {
            Some(variant) => tags.push(variant.to_string()),
            None => tags.push("latest".to_string()),
        }
    }
    Ok((tags, is_latest))
}

/// Pushes a package to its primary tag, then merges the platform entry
/// into each cascade tag sequentially (most-specific → least-specific
/// for partial-failure safety).
pub async fn push_with_cascade(
    client: &oci::Client,
    package_info: package::info::Info,
    layers: &[crate::publisher::LayerRef],
    other_versions: BTreeSet<Version>,
    version: &Version,
) -> Result<()> {
    let (cascade_tags, _) = resolve_cascade_tags(
        client,
        &package_info.identifier,
        version,
        &other_versions,
        &package_info.platform,
    )
    .await?;

    client
        .push_manifest_and_merge_tags(&package_info, layers, &cascade_tags)
        .await?;

    Ok(())
}

/// Checks blockers sequentially, returning `true` on first platform match.
///
/// Returns `Err` on registry errors — the caller decides how to handle
/// (typically: stop cascade conservatively with a warning).
async fn has_blocking_platform(
    client: &oci::Client,
    identifier: &oci::Identifier,
    blockers: &[Version],
    platform: &oci::Platform,
) -> Result<bool> {
    for blocker in blockers {
        let blocker_id = identifier.clone_with_tag(blocker.to_string());
        let (_, manifest) = client.fetch_manifest(&blocker_id).await?;
        if oci::manifest::has_platform(&manifest, platform) {
            return Ok(true);
        }
        log::debug!("Blocker {blocker} lacks platform {platform}, skipping");
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cascade_no_blockers_full_chain() {
        let version_build = Version::new_build(1, 7, 3, "20260216");
        let version_patch = Version::new_patch(1, 7, 3);
        let version_minor = Version::new_minor(1, 7);
        let version_major = Version::new_major(1);

        let (versions, is_latest) = cascade(&version_build, vec![]);
        assert_eq!(
            versions,
            vec![version_build, version_patch, version_minor, version_major]
        );
        assert!(is_latest);
    }

    #[test]
    fn cascade_older_versions_do_not_block() {
        let version_build = Version::new_build(1, 7, 3, "20260216");
        let (_, is_latest) = cascade(
            &version_build,
            vec![
                Version::new_build(1, 7, 3, "20260215"),
                Version::new_patch(1, 7, 2),
                Version::new_minor(1, 6),
                Version::new_major(0),
            ],
        );
        assert!(is_latest);
    }

    #[test]
    fn cascade_blocked_at_build_level() {
        let v = Version::new_build(1, 7, 3, "20260216");
        let (versions, is_latest) = cascade(&v, vec![Version::new_build(1, 7, 3, "20260217")]);
        assert_eq!(versions, vec![v]);
        assert!(!is_latest);
    }

    #[test]
    fn cascade_blocked_at_patch_level() {
        let v = Version::new_build(1, 7, 3, "20260216");
        let (versions, is_latest) = cascade(&v, vec![Version::new_patch(1, 7, 4)]);
        assert_eq!(versions, vec![v, Version::new_patch(1, 7, 3)]);
        assert!(!is_latest);
    }

    #[test]
    fn cascade_blocked_at_minor_level() {
        let v = Version::new_build(1, 7, 3, "20260216");
        let (versions, is_latest) = cascade(&v, vec![Version::new_minor(1, 8)]);
        assert_eq!(versions, vec![v, Version::new_patch(1, 7, 3), Version::new_minor(1, 7)]);
        assert!(!is_latest);
    }

    #[test]
    fn cascade_blocked_at_major_level() {
        let v = Version::new_build(1, 7, 3, "20260216");
        let (versions, is_latest) = cascade(&v, vec![Version::new_major(2)]);
        assert_eq!(
            versions,
            vec![
                v,
                Version::new_patch(1, 7, 3),
                Version::new_minor(1, 7),
                Version::new_major(1),
            ]
        );
        assert!(!is_latest);
    }

    #[test]
    fn cascade_prerelease_without_build_no_cascade() {
        let v = Version::new_prerelease(1, 7, 3, "beta");
        let (versions, is_latest) = cascade(&v, vec![]);
        assert_eq!(versions, vec![v]);
        assert!(!is_latest);
    }

    #[test]
    fn cascade_prerelease_with_build_cascades_to_parent() {
        let v = Version::new_prerelease_with_build(1, 7, 3, "beta", "20260216");
        let parent = Version::new_prerelease(1, 7, 3, "beta");
        let (versions, is_latest) = cascade(&v, vec![]);
        assert_eq!(versions, vec![v, parent]);
        assert!(!is_latest);
    }

    #[test]
    fn cascade_prerelease_with_build_blocked() {
        let v = Version::new_prerelease_with_build(1, 7, 3, "beta", "20260216");
        let (versions, is_latest) = cascade(
            &v,
            vec![Version::new_prerelease_with_build(1, 7, 3, "beta", "20260217")],
        );
        assert_eq!(versions, vec![v]);
        assert!(!is_latest);
    }

    #[test]
    fn cascade_prerelease_with_build_not_blocked_by_different_prerelease() {
        let v = Version::new_prerelease_with_build(1, 7, 3, "beta", "20260216");
        let parent = Version::new_prerelease(1, 7, 3, "beta");
        let (versions, _) = cascade(&v, vec![Version::new_prerelease(1, 7, 3, "gamma")]);
        assert_eq!(versions, vec![v, parent]);
    }

    // ── decompose ───────────────────────────────────────────────

    #[test]
    fn decompose_no_blockers() {
        let v = Version::new_build(3, 28, 1, "b1");
        let others = BTreeSet::new();
        let d = decompose(&v, &others);

        assert_eq!(d.levels.len(), 3);
        assert_eq!(d.levels[0].target, Version::new_patch(3, 28, 1));
        assert!(d.levels[0].blockers.is_empty());
        assert_eq!(d.levels[1].target, Version::new_minor(3, 28));
        assert!(d.levels[1].blockers.is_empty());
        assert_eq!(d.levels[2].target, Version::new_major(3));
        assert!(d.levels[2].blockers.is_empty());
        assert!(d.latest_eligible);
        assert!(d.latest_blockers.is_empty());
    }

    #[test]
    fn decompose_blocker_at_patch() {
        let v = Version::new_build(3, 28, 1, "b1");
        let blocker = Version::new_build(3, 28, 2, "b1");
        let others: BTreeSet<_> = [blocker.clone()].into();
        let d = decompose(&v, &others);

        assert_eq!(d.levels.len(), 3);
        assert_eq!(d.levels[0].target, Version::new_patch(3, 28, 1));
        assert!(d.levels[0].blockers.is_empty()); // 3.28.2_b1 is NOT between self and 3.28.1
        assert_eq!(d.levels[1].target, Version::new_minor(3, 28));
        assert_eq!(d.levels[1].blockers, vec![blocker]); // 3.28.2_b1 IS between 3.28.1 and 3.28
        assert!(d.levels[2].blockers.is_empty());
        assert!(d.latest_eligible);
        assert!(d.latest_blockers.is_empty());
    }

    #[test]
    fn decompose_blockers_at_multiple_levels() {
        let v = Version::new_build(3, 28, 1, "b1");
        let blocker1 = Version::new_build(3, 28, 2, "b1");
        let blocker2 = Version::new_build(3, 29, 0, "b1");
        let others: BTreeSet<_> = [blocker1.clone(), blocker2.clone()].into();
        let d = decompose(&v, &others);

        assert_eq!(d.levels.len(), 3);
        assert!(d.levels[0].blockers.is_empty());
        assert_eq!(d.levels[1].blockers, vec![blocker1]);
        assert_eq!(d.levels[2].blockers, vec![blocker2]);
        assert!(d.latest_eligible);
        assert!(d.latest_blockers.is_empty());
    }

    #[test]
    fn decompose_latest_blockers() {
        let v = Version::new_build(3, 28, 1, "b1");
        let higher = Version::new_build(4, 0, 0, "b1");
        let others: BTreeSet<_> = [higher.clone()].into();
        let d = decompose(&v, &others);

        assert_eq!(d.levels.len(), 3);
        assert!(d.levels.iter().all(|l| l.blockers.is_empty()));
        assert!(d.latest_eligible);
        assert_eq!(d.latest_blockers, vec![higher]);
    }

    #[test]
    fn decompose_prerelease_with_build() {
        let v = Version::new_prerelease_with_build(1, 0, 0, "beta", "b1");
        let blocker = Version::new_prerelease_with_build(1, 0, 0, "beta", "b2");
        let others: BTreeSet<_> = [blocker.clone()].into();
        let d = decompose(&v, &others);

        assert_eq!(d.levels.len(), 1);
        assert_eq!(d.levels[0].target, Version::new_prerelease(1, 0, 0, "beta"));
        assert_eq!(d.levels[0].blockers, vec![blocker]);
        assert!(!d.latest_eligible);
    }

    #[test]
    fn decompose_prerelease_without_build() {
        let v = Version::new_prerelease(1, 0, 0, "beta");
        let other = Version::new_prerelease(1, 0, 0, "gamma");
        let others: BTreeSet<_> = [other].into();
        let d = decompose(&v, &others);

        assert!(d.levels.is_empty());
        assert!(!d.latest_eligible);
    }

    #[test]
    fn decompose_matches_cascade() {
        let v = Version::new_build(1, 7, 3, "20260216");
        let scenarios: Vec<Vec<Version>> = vec![
            vec![],
            vec![Version::new_build(1, 7, 3, "20260217")],
            vec![Version::new_build(1, 7, 4, "b1")],
            vec![Version::new_build(1, 8, 0, "b1")],
            vec![Version::new_build(2, 0, 0, "b1")],
        ];

        let expected: Vec<(Vec<Version>, bool)> = vec![
            (
                vec![
                    v.clone(),
                    Version::new_patch(1, 7, 3),
                    Version::new_minor(1, 7),
                    Version::new_major(1),
                ],
                true,
            ),
            (vec![v.clone()], false),
            (vec![v.clone(), Version::new_patch(1, 7, 3)], false),
            (
                vec![v.clone(), Version::new_patch(1, 7, 3), Version::new_minor(1, 7)],
                false,
            ),
            (
                vec![
                    v.clone(),
                    Version::new_patch(1, 7, 3),
                    Version::new_minor(1, 7),
                    Version::new_major(1),
                ],
                false,
            ),
        ];

        for (others, expected) in scenarios.into_iter().zip(expected) {
            let result = cascade(&v, others);
            assert_eq!(result, expected);
        }
    }

    // ── Additional algebra tests (Phase 5) ──────────────────────

    #[test]
    fn rolling_tags_in_others_dont_self_block() {
        // Rolling tags (3.28.0, 3.28, 3) should not appear as blockers
        // because they sort below their build-tagged children.
        let v = Version::new_build(3, 28, 0, "b1");
        let others: BTreeSet<_> = [
            Version::new_patch(3, 28, 0),
            Version::new_minor(3, 28),
            Version::new_major(3),
        ]
        .into();
        let d = decompose(&v, &others);
        assert!(d.levels.iter().all(|l| l.blockers.is_empty()));
        assert!(d.latest_blockers.is_empty());
    }

    #[test]
    fn old_version_blocked_by_newer_at_minor() {
        // 3.27.0_b1 should cascade to 3.27.0 and 3.27, but be blocked at 3
        // because 3.28.0_b1 is between 3.27 and 3.
        let v = Version::new_build(3, 27, 0, "b1");
        let others: BTreeSet<_> = [Version::new_build(3, 28, 0, "b1")].into();
        let (versions, is_latest) = cascade(&v, others);
        assert_eq!(
            versions,
            vec![v, Version::new_patch(3, 27, 0), Version::new_minor(3, 27)]
        );
        assert!(!is_latest);
    }

    #[test]
    fn self_in_others_doesnt_self_block() {
        // The version itself in `others` should not block because
        // Excluded bound prevents self-matching.
        let v = Version::new_build(3, 28, 0, "b1");
        let others: BTreeSet<_> = [v.clone()].into();
        let d = decompose(&v, &others);
        assert!(d.levels.iter().all(|l| l.blockers.is_empty()));
        assert!(d.latest_blockers.is_empty());
    }

    #[test]
    fn patch_without_build_cascades() {
        // A bare patch version like 3.28.1 cascades to 3.28, 3, latest.
        let v = Version::new_patch(3, 28, 1);
        let (versions, is_latest) = cascade(&v, vec![]);
        assert_eq!(versions, vec![v, Version::new_minor(3, 28), Version::new_major(3)]);
        assert!(is_latest);
    }

    #[test]
    fn minor_version_cascades() {
        // A bare minor version like 3.28 cascades to 3, latest.
        let v = Version::new_minor(3, 28);
        let (versions, is_latest) = cascade(&v, vec![]);
        assert_eq!(versions, vec![v, Version::new_major(3)]);
        assert!(is_latest);
    }

    // ── Orchestration tests (Phase 2) ───────────────────────────

    mod orchestration {
        use super::*;
        use crate::oci::client::test_transport::StubTransportData;

        fn test_client(data: &StubTransportData) -> oci::Client {
            use crate::oci::client::test_transport::StubTransport;
            oci::Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn test_identifier() -> oci::Identifier {
            oci::Identifier::new_registry("test/pkg", "example.com")
        }

        fn platform(s: &str) -> oci::Platform {
            s.parse().unwrap()
        }

        /// Seed an image index manifest for a tag in the stub transport.
        fn seed_index(data: &StubTransportData, tag: &str, platforms: &[&str]) {
            let id = test_identifier().clone_with_tag(tag);
            let manifests: Vec<oci::ImageIndexEntry> = platforms
                .iter()
                .map(|p| {
                    let plat: oci::Platform = p.parse().unwrap();
                    let native: oci::native::Platform = plat.into();
                    oci::ImageIndexEntry {
                        media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                        digest: format!("sha256:fake_{tag}_{p}"),
                        size: 100,
                        platform: Some(native),
                        annotations: None,
                    }
                })
                .collect();
            let index = oci::Manifest::ImageIndex(oci::ImageIndex {
                schema_version: 2,
                media_type: None,
                artifact_type: None,
                manifests,
                annotations: None,
            });
            let manifest_data = serde_json::to_vec(&index).unwrap();
            let digest = oci::Algorithm::Sha256.hash(&manifest_data).to_string();
            data.write()
                .manifests
                .insert(oci::native::Reference::from(&id).to_string(), (manifest_data, digest));
        }

        // ── has_blocking_platform ───────────────────────────────

        #[tokio::test]
        async fn empty_blockers_returns_clear() {
            let data = StubTransportData::new();
            let client = test_client(&data);
            let result = has_blocking_platform(&client, &test_identifier(), &[], &platform("linux/amd64")).await;
            assert!(!result.unwrap());
        }

        #[tokio::test]
        async fn blocker_with_matching_platform_blocks() {
            let data = StubTransportData::new();
            let blocker = Version::new_build(3, 28, 1, "b1");
            seed_index(&data, &blocker.to_string(), &["linux/amd64"]);
            let client = test_client(&data);
            let result = has_blocking_platform(&client, &test_identifier(), &[blocker], &platform("linux/amd64")).await;
            assert!(result.unwrap());
        }

        #[tokio::test]
        async fn blocker_without_matching_platform_passes() {
            let data = StubTransportData::new();
            let blocker = Version::new_build(3, 28, 1, "b1");
            seed_index(&data, &blocker.to_string(), &["linux/amd64"]);
            let client = test_client(&data);
            let result = has_blocking_platform(&client, &test_identifier(), &[blocker], &platform("linux/arm64")).await;
            assert!(!result.unwrap());
        }

        #[tokio::test]
        async fn blocker_manifest_fetch_error_returns_err() {
            let data = StubTransportData::new();
            // No manifest seeded for this blocker — fetch will fail.
            let blocker = Version::new_build(3, 28, 1, "b1");
            let client = test_client(&data);
            let result = has_blocking_platform(&client, &test_identifier(), &[blocker], &platform("linux/amd64")).await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn first_blocker_lacks_platform_second_has_it() {
            let data = StubTransportData::new();
            let b1 = Version::new_build(3, 28, 1, "b1");
            let b2 = Version::new_build(3, 28, 2, "b1");
            seed_index(&data, &b1.to_string(), &["linux/arm64"]);
            seed_index(&data, &b2.to_string(), &["linux/amd64"]);
            let client = test_client(&data);
            let result = has_blocking_platform(&client, &test_identifier(), &[b1, b2], &platform("linux/amd64")).await;
            assert!(result.unwrap());
        }

        #[tokio::test]
        async fn all_blockers_lack_platform() {
            let data = StubTransportData::new();
            let b1 = Version::new_build(3, 28, 1, "b1");
            let b2 = Version::new_build(3, 28, 2, "b1");
            seed_index(&data, &b1.to_string(), &["linux/arm64"]);
            seed_index(&data, &b2.to_string(), &["linux/arm64"]);
            let client = test_client(&data);
            let result = has_blocking_platform(&client, &test_identifier(), &[b1, b2], &platform("linux/amd64")).await;
            assert!(!result.unwrap());
        }

        #[tokio::test]
        async fn blocker_is_image_manifest_not_index() {
            // A plain ImageManifest has no platform info → has_platform returns false.
            let data = StubTransportData::new();
            let blocker = Version::new_build(3, 28, 1, "b1");
            let id = test_identifier().clone_with_tag(blocker.to_string());
            let manifest = oci::Manifest::Image(oci::ImageManifest::default());
            let manifest_data = serde_json::to_vec(&manifest).unwrap();
            let digest = oci::Algorithm::Sha256.hash(&manifest_data).to_string();
            data.write()
                .manifests
                .insert(oci::native::Reference::from(&id).to_string(), (manifest_data, digest));
            let client = test_client(&data);
            let result = has_blocking_platform(&client, &test_identifier(), &[blocker], &platform("linux/amd64")).await;
            assert!(!result.unwrap());
        }

        // ── resolve_cascade_tags ────────────────────────────────

        #[tokio::test]
        async fn no_blockers_full_cascade_with_latest() {
            let data = StubTransportData::new();
            let client = test_client(&data);
            let v = Version::new_build(3, 28, 0, "b1");
            let others = BTreeSet::new();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["3.28.0", "3.28", "3", "latest"]);
            assert!(is_latest);
        }

        #[tokio::test]
        async fn blocker_at_minor_lacks_platform_cascade_continues() {
            let data = StubTransportData::new();
            // 3.28.1_b1 exists but only for arm64 — should not block amd64 cascade.
            let blocker = Version::new_build(3, 28, 1, "b1");
            seed_index(&data, &blocker.to_string(), &["linux/arm64"]);
            let client = test_client(&data);
            let v = Version::new_build(3, 28, 0, "b1");
            let others: BTreeSet<_> = [blocker].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert!(
                tags.contains(&"3.28".to_string()),
                "3.28 should be in cascade: {tags:?}"
            );
            assert!(tags.contains(&"3".to_string()));
            assert!(is_latest);
        }

        #[tokio::test]
        async fn blocker_at_minor_has_platform_cascade_stops() {
            let data = StubTransportData::new();
            let blocker = Version::new_build(3, 28, 1, "b1");
            seed_index(&data, &blocker.to_string(), &["linux/amd64"]);
            let client = test_client(&data);
            let v = Version::new_build(3, 28, 0, "b1");
            let others: BTreeSet<_> = [blocker].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["3.28.0"]);
            assert!(!is_latest);
        }

        #[tokio::test]
        async fn latest_blocked_by_higher_version_with_platform() {
            let data = StubTransportData::new();
            let higher = Version::new_build(4, 0, 0, "b1");
            seed_index(&data, &higher.to_string(), &["linux/amd64"]);
            let client = test_client(&data);
            let v = Version::new_build(3, 28, 0, "b1");
            let others: BTreeSet<_> = [higher].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["3.28.0", "3.28", "3"]);
            assert!(!is_latest);
        }

        #[tokio::test]
        async fn error_on_blocker_stops_cascade_with_warning() {
            let data = StubTransportData::new();
            // Blocker 3.28.1_b1 has no manifest seeded — fetch will error.
            let blocker = Version::new_build(3, 28, 1, "b1");
            let client = test_client(&data);
            let v = Version::new_build(3, 28, 0, "b1");
            let others: BTreeSet<_> = [blocker].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            // Cascade stops at 3.28.0 because minor-level blocker errored.
            assert_eq!(tags, vec!["3.28.0"]);
            assert!(!is_latest);
        }

        #[tokio::test]
        async fn error_on_latest_blocker_skips_latest() {
            let data = StubTransportData::new();
            // All levels clear, but latest blocker 4.0.0_b1 has no manifest.
            let higher = Version::new_build(4, 0, 0, "b1");
            let client = test_client(&data);
            let v = Version::new_build(3, 28, 0, "b1");
            let others: BTreeSet<_> = [higher].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["3.28.0", "3.28", "3"]);
            assert!(!is_latest);
        }

        // ── Variant-aware resolve_cascade_tags ────────────────────

        #[tokio::test]
        async fn variant_no_blockers_full_cascade_with_variant_terminal() {
            let data = StubTransportData::new();
            let client = test_client(&data);
            let v = Version::parse("debug-3.28.0_b1").unwrap();
            let others = BTreeSet::new();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["debug-3.28.0", "debug-3.28", "debug-3", "debug"]);
            assert!(is_latest);
        }

        #[tokio::test]
        async fn variant_cross_variant_blocker_does_not_block() {
            let data = StubTransportData::new();
            let cross_variant = Version::parse("pgo-3.29.0_b1").unwrap();
            seed_index(&data, &cross_variant.to_string(), &["linux/amd64"]);
            let client = test_client(&data);
            let v = Version::parse("debug-3.28.0_b1").unwrap();
            let others: BTreeSet<_> = [cross_variant].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["debug-3.28.0", "debug-3.28", "debug-3", "debug"]);
            assert!(is_latest);
        }

        #[tokio::test]
        async fn variant_same_variant_blocker_blocks() {
            let data = StubTransportData::new();
            let blocker = Version::parse("debug-3.28.1_b1").unwrap();
            seed_index(&data, &blocker.to_string(), &["linux/amd64"]);
            let client = test_client(&data);
            let v = Version::parse("debug-3.28.0_b1").unwrap();
            let others: BTreeSet<_> = [blocker].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["debug-3.28.0"]);
            assert!(!is_latest);
        }

        #[tokio::test]
        async fn variant_same_variant_blocker_different_platform_passes() {
            let data = StubTransportData::new();
            let blocker = Version::parse("debug-3.28.1_b1").unwrap();
            seed_index(&data, &blocker.to_string(), &["linux/arm64"]);
            let client = test_client(&data);
            let v = Version::parse("debug-3.28.0_b1").unwrap();
            let others: BTreeSet<_> = [blocker].into();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert!(tags.contains(&"debug-3.28".to_string()));
            assert!(tags.contains(&"debug-3".to_string()));
            assert!(tags.contains(&"debug".to_string()));
            assert!(is_latest);
        }

        #[tokio::test]
        async fn non_variant_terminal_unchanged() {
            let data = StubTransportData::new();
            let client = test_client(&data);
            let v = Version::new_build(3, 28, 0, "b1");
            let others = BTreeSet::new();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert!(tags.contains(&"latest".to_string()));
            assert!(!tags.contains(&"debug".to_string()));
            assert!(is_latest);
        }

        #[tokio::test]
        async fn dotted_variant_terminal() {
            let data = StubTransportData::new();
            let client = test_client(&data);
            let v = Version::parse("pgo.lto-3.28.0_b1").unwrap();
            let others = BTreeSet::new();
            let (tags, is_latest) =
                resolve_cascade_tags(&client, &test_identifier(), &v, &others, &platform("linux/amd64"))
                    .await
                    .unwrap();
            assert_eq!(tags, vec!["pgo.lto-3.28.0", "pgo.lto-3.28", "pgo.lto-3", "pgo.lto"]);
            assert!(is_latest);
        }
    }

    // ── Variant cascade algebra tests ─────────────────────────────

    /// Helper to parse a variant version string.
    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("Failed to parse version: {s}"))
    }

    // ── cascade() with variant versions, no blockers ──────────────

    #[test]
    fn variant_cascade_no_blockers_build_tagged() {
        let version = v("debug-3.28.1_b1");
        let (versions, is_latest) = cascade(&version, vec![]);
        assert_eq!(
            versions,
            vec![v("debug-3.28.1_b1"), v("debug-3.28.1"), v("debug-3.28"), v("debug-3")]
        );
        assert!(is_latest);
    }

    #[test]
    fn variant_cascade_no_blockers_patch() {
        let version = v("debug-3.28.1");
        let (versions, is_latest) = cascade(&version, vec![]);
        assert_eq!(versions, vec![v("debug-3.28.1"), v("debug-3.28"), v("debug-3")]);
        assert!(is_latest);
    }

    #[test]
    fn variant_cascade_no_blockers_minor() {
        let version = v("debug-3.28");
        let (versions, is_latest) = cascade(&version, vec![]);
        assert_eq!(versions, vec![v("debug-3.28"), v("debug-3")]);
        assert!(is_latest);
    }

    #[test]
    fn variant_cascade_dotted_name() {
        let version = v("pgo.lto-1.0.0_b1");
        let (versions, is_latest) = cascade(&version, vec![]);
        assert_eq!(
            versions,
            vec![
                v("pgo.lto-1.0.0_b1"),
                v("pgo.lto-1.0.0"),
                v("pgo.lto-1.0"),
                v("pgo.lto-1")
            ]
        );
        assert!(is_latest);
    }

    // ── Same-variant blocking ─────────────────────────────────────

    #[test]
    fn variant_cascade_blocked_at_build() {
        let version = v("debug-3.28.1_b1");
        let (versions, is_latest) = cascade(&version, vec![v("debug-3.28.1_b2")]);
        assert_eq!(versions, vec![v("debug-3.28.1_b1")]);
        assert!(!is_latest);
    }

    #[test]
    fn variant_cascade_blocked_at_minor() {
        let version = v("debug-3.28.1_b1");
        let (versions, is_latest) = cascade(&version, vec![v("debug-3.28.2_b1")]);
        assert_eq!(versions, vec![v("debug-3.28.1_b1"), v("debug-3.28.1")]);
        assert!(!is_latest);
    }

    #[test]
    fn variant_cascade_blocked_at_major() {
        let version = v("debug-3.28.1_b1");
        let (versions, is_latest) = cascade(&version, vec![v("debug-3.29.0_b1")]);
        assert_eq!(versions, vec![v("debug-3.28.1_b1"), v("debug-3.28.1"), v("debug-3.28")]);
        assert!(!is_latest);
    }

    #[test]
    fn variant_cascade_blocked_at_latest() {
        let version = v("debug-3.28.1_b1");
        let (versions, is_latest) = cascade(&version, vec![v("debug-4.0.0_b1")]);
        assert_eq!(
            versions,
            vec![v("debug-3.28.1_b1"), v("debug-3.28.1"), v("debug-3.28"), v("debug-3")]
        );
        assert!(!is_latest);
    }

    // ── Cross-variant non-blocking ────────────────────────────────

    #[test]
    fn variant_not_blocked_by_different_variant() {
        let version = v("debug-3.28.1_b1");
        let (_, is_latest) = cascade(&version, vec![v("pgo-3.29.0_b1")]);
        assert!(is_latest, "pgo variant should not block debug cascade");
    }

    #[test]
    fn variant_not_blocked_by_default_variant() {
        let version = v("debug-3.28.1_b1");
        let (_, is_latest) = cascade(&version, vec![Version::new_build(3, 29, 0, "b1")]);
        assert!(is_latest, "default (None) variant should not block debug cascade");
    }

    #[test]
    fn default_not_blocked_by_variant() {
        let version = Version::new_build(3, 28, 1, "b1");
        let (_, is_latest) = cascade(&version, vec![v("debug-3.29.0_b1")]);
        assert!(is_latest, "debug variant should not block default cascade");
    }

    #[test]
    fn default_not_blocked_by_multiple_variants() {
        let version = Version::new_build(3, 28, 1, "b1");
        let (_, is_latest) = cascade(&version, vec![v("debug-4.0.0_b1"), v("pgo-5.0.0_b1")]);
        assert!(is_latest, "multiple variant versions should not block default");
    }

    #[test]
    fn variant_same_variant_blocks_cross_variant_doesnt() {
        let version = v("debug-3.28.1_b1");
        let (versions, is_latest) = cascade(&version, vec![v("debug-3.28.2_b1"), v("pgo-3.29.0_b1")]);
        // Blocked by debug-3.28.2_b1 at minor level; pgo-3.29.0_b1 is irrelevant
        assert_eq!(versions, vec![v("debug-3.28.1_b1"), v("debug-3.28.1")]);
        assert!(!is_latest);
    }

    // ── Mixed variant sets ────────────────────────────────────────

    #[test]
    fn variant_mixed_set_older_same_variant_doesnt_block() {
        let version = v("debug-3.28.1_b1");
        let (_, is_latest) = cascade(
            &version,
            vec![
                v("debug-3.27.0_b1"),
                v("pgo-3.29.0_b1"),
                Version::new_build(3, 30, 0, "b1"),
            ],
        );
        assert!(is_latest, "older debug + cross-variant versions should not block");
    }

    #[test]
    fn variant_mixed_set_newer_same_variant_blocks() {
        let version = v("debug-3.28.1_b1");
        let (versions, _) = cascade(
            &version,
            vec![v("debug-3.28.2_b1"), v("debug-3.27.0_b1"), v("pgo-3.28.0_b1")],
        );
        // Blocked by debug-3.28.2_b1 at minor level
        assert_eq!(versions, vec![v("debug-3.28.1_b1"), v("debug-3.28.1")]);
    }

    // ── Prerelease variant isolation ──────────────────────────────

    #[test]
    fn variant_prerelease_not_blocked_by_different_variant() {
        let version = v("debug-1.0.0-beta_b1");
        let parent = v("debug-1.0.0-beta");
        let (versions, _) = cascade(&version, vec![v("pgo-1.0.0-beta_b2")]);
        assert_eq!(versions, vec![version, parent], "pgo prerelease should not block debug");
    }

    #[test]
    fn variant_prerelease_blocked_by_same_variant() {
        let version = v("debug-1.0.0-beta_b1");
        let (versions, _) = cascade(&version, vec![v("debug-1.0.0-beta_b2")]);
        assert_eq!(versions, vec![version], "same variant prerelease should block");
    }

    #[test]
    fn variant_prerelease_without_build_no_cascade() {
        let version = v("debug-1.0.0-beta");
        let (versions, is_latest) = cascade(&version, vec![v("pgo-1.0.0-gamma")]);
        assert_eq!(versions, vec![version]);
        assert!(!is_latest);
    }

    // ── decompose() variant-specific assertions ───────────────────

    #[test]
    fn decompose_variant_latest_blockers_only_same_variant() {
        let version = v("debug-3.28.1_b1");
        let others: BTreeSet<_> = [
            v("debug-4.0.0_b1"),
            v("pgo-5.0.0_b1"),
            Version::new_build(6, 0, 0, "b1"),
        ]
        .into();
        let d = decompose(&version, &others);

        assert!(d.latest_eligible);
        // Only debug-4.0.0_b1 should be a latest_blocker; pgo and default are different tracks
        assert_eq!(d.latest_blockers, vec![v("debug-4.0.0_b1")]);
    }

    #[test]
    fn decompose_variant_level_blockers_only_same_variant() {
        let version = v("debug-3.28.1_b1");
        let others: BTreeSet<_> = [
            v("debug-3.28.2_b1"),
            v("pgo-3.28.3_b1"),
            Version::new_build(3, 28, 4, "b1"),
        ]
        .into();
        let d = decompose(&version, &others);

        // Only debug-3.28.2_b1 should appear as a blocker at the minor level
        assert!(d.levels[0].blockers.is_empty()); // patch level: nothing between debug-3.28.1_b1 and debug-3.28.1
        assert_eq!(d.levels[1].blockers, vec![v("debug-3.28.2_b1")]); // minor level
        assert!(d.levels[2].blockers.is_empty()); // major level
    }

    #[test]
    fn decompose_default_variant_not_affected_by_named_variants() {
        let version = Version::new_build(3, 28, 1, "b1");
        let others: BTreeSet<_> = [v("debug-3.29.0_b1"), v("pgo-4.0.0_b1")].into();
        let d = decompose(&version, &others);

        assert!(d.latest_eligible);
        assert!(
            d.latest_blockers.is_empty(),
            "Named variants should not be latest blockers for default"
        );
        assert!(
            d.levels.iter().all(|l| l.blockers.is_empty()),
            "Named variants should not appear as level blockers for default"
        );
    }
}
