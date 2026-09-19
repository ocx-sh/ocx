// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cascade algebra and platform-aware push orchestration.
//!
//! The cascade algebra ([`decompose`], [`cascade`]) computes which rolling
//! tags a build-tagged version should update, based on the set of existing
//! versions and blocking ranges.
//!
//! The orchestration layer ([`resolve_cascade_tags`], [`push_with_cascade`])
//! composes the algebra with [`Client`](ocx_oci::Client) OCI transport
//! to implement cascade pushes that correctly handle multi-platform registries.
//!
//! The submodules read the tag graph back and repair it — the audit half of
//! the same algebra, behind `ocx package cascade check|repair`. They split
//! along the one seam that makes the state space unit-testable: [`gather`]
//! reads, [`graph`] computes with no I/O at all, [`apply`] writes.

pub mod apply;
pub mod gather;
pub mod graph;

#[cfg(test)]
mod equivalence;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};

use crate::error::Error as PackageError;
use crate::version::Version;

/// This tier's own result (E1, plan DEC-27).
type Result<T> = std::result::Result<T, PackageError>;

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

/// The cascade chain of a version taken alone: which rolling tags it would
/// update, and whether it could become `latest`, if nothing blocked it.
///
/// The half of [`decompose`] that is a function of `version` only — no version
/// set is consulted, and none could change the answer. That is what makes it
/// safe for a caller who wants the chain and not the blockers to skip the
/// per-level range scans.
pub struct CascadeTargets {
    /// The rolling tags to cascade to, most-specific first.
    pub targets: Vec<Version>,
    /// Whether this version is eligible to become `latest`.
    /// Always `false` for pre-release versions.
    pub latest_eligible: bool,
}

/// Derives a version's cascade chain, with no blocker analysis.
///
/// Pre-releases without build produce zero targets (no cascade).
/// Pre-releases with build produce exactly one (the parent pre-release).
/// Pre-releases are never eligible for `latest`.
pub fn decompose_targets(version: &Version) -> CascadeTargets {
    // Pre-releases without build never cascade beyond their own level.
    if version.has_prerelease() && !version.has_build() {
        return CascadeTargets {
            targets: vec![],
            latest_eligible: false,
        };
    }

    // Pre-releases with build: cascade only to the parent pre-release.
    if version.has_prerelease() {
        let parent = version
            .parent()
            .expect("Versions with build fragment shall always have a parent.");
        return CascadeTargets {
            targets: vec![parent],
            latest_eligible: false,
        };
    }

    CascadeTargets {
        targets: std::iter::successors(version.parent(), Version::parent).collect(),
        latest_eligible: true,
    }
}

/// Decomposes a version's cascade into discrete levels with pre-computed blocking ranges.
///
/// The chain itself comes from [`decompose_targets`]; this adds the blocking
/// range each level has to clear. Composed rather than duplicated, so the two
/// can never disagree about where a version cascades to.
pub fn decompose(version: &Version, others: &BTreeSet<Version>) -> CascadeDecomposition {
    let CascadeTargets {
        targets,
        latest_eligible,
    } = decompose_targets(version);

    // A pre-release's blocking range is its own: only a later build of the same
    // core and pre-release blocks it, and the build-less rolling parent must be
    // excluded explicitly — Ord sorts it above its build-tagged children, so an
    // unbounded range would let it block its own cascade and freeze the
    // floating tag at the first build.
    if version.has_prerelease() {
        // At most one target, so the range is scanned at most once — and not at
        // all for a build-less pre-release, which cascades nowhere.
        let blockers = || {
            others
                .range((Excluded(version), Unbounded))
                .take_while(|v| {
                    v.variant() == version.variant()
                        && v.major() == version.major()
                        && v.minor() == version.minor()
                        && v.patch() == version.patch()
                        && v.prerelease() == version.prerelease()
                        && v.has_build()
                })
                .cloned()
                .collect()
        };
        return CascadeDecomposition {
            levels: targets
                .into_iter()
                .map(|target| CascadeLevel {
                    target,
                    blockers: blockers(),
                })
                .collect(),
            latest_eligible,
            latest_blockers: vec![],
        };
    }

    // Each level clears the half-open range between the level below it and its
    // own target; `latest` clears everything above the last target on the same
    // variant track (`take_while` works because Ord clusters a variant's
    // versions together).
    let mut current = version.clone();
    let mut levels = Vec::with_capacity(targets.len());
    for target in targets {
        levels.push(CascadeLevel {
            blockers: others.range((Excluded(&current), Excluded(&target))).cloned().collect(),
            target: target.clone(),
        });
        current = target;
    }

    let latest_blockers: Vec<Version> = others
        .range((Excluded(&current), Unbounded))
        .take_while(|v| v.variant() == version.variant())
        .cloned()
        .collect();

    CascadeDecomposition {
        levels,
        latest_eligible,
        latest_blockers,
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
    client: &ocx_oci::Client,
    identifier: &ocx_oci::Identifier,
    version: &Version,
    other_versions: &BTreeSet<Version>,
    platform: &ocx_oci::Platform,
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

/// What one cascade push landed.
///
/// A named struct rather than a tuple because two of its five members are
/// digests and two are tag-shaped: as unlabelled positionals, a swapped pair
/// type-checks silently — and [`platform_digest`](Self::platform_digest) is a
/// signing input, where the wrong digest means signing the wrong object.
/// [`PushOutcome`](crate::publisher::PushOutcome) in the sibling module is the
/// precedent.
#[derive(Debug)]
pub struct CascadePushOutcome {
    /// Digest of the primary tag's image index after this platform merged in.
    pub index_digest: ocx_oci::Digest,
    /// The rolling tags this push cascaded to, most-specific first.
    pub cascade_tags: Vec<String>,
    /// The digest-named keep tag written, or `None` when `keep_tag` was
    /// `false` or the merged index carried no entry for this platform.
    pub keep_tag: Option<String>,
    /// The platform manifest digest this push landed on, whatever `keep_tag`
    /// was — read from the same merged-index descriptor
    /// [`Client::push_keep_tag`](ocx_oci::Client) reads, so the two can
    /// never disagree and no second lookup is issued.
    ///
    /// `None` only when the merged index carries no entry for this platform:
    /// the row is omitted rather than faked, matching `keep_tag`.
    pub platform_digest: Option<ocx_oci::Digest>,
    /// The un-prefixed track tags `--default` aliased this push onto, bare
    /// version first. Empty unless `default` is set and the pushed version
    /// carries a variant.
    pub aliases: Vec<String>,
    /// Layer-push counts for this platform's push.
    pub layer_counts: ocx_oci::LayerCounts,
}

/// Pushes a package to its primary tag, then merges the platform entry
/// into each cascade tag sequentially (most-specific → least-specific
/// for partial-failure safety).
///
/// When `keep_tag` is `true`, the just-pushed platform manifest also
/// gets a digest-named `__ocx.keep.<algorithm>-<hex>` tag (`adr_index_indirection.md`
/// Decision E) — looked up once from the primary tag's merged index, so a
/// cascade never retags a pre-existing entry for a platform this call did
/// not push.
///
/// `annotations` land on the primary tag's index and on every cascade tag's
/// index alike.
///
/// `default` re-tags the pushed version's own variant onto the un-prefixed
/// track — see [`write_default_variant_aliases`]. It is a no-op for a version
/// that carries no variant.
#[expect(
    clippy::too_many_arguments,
    reason = "one push: what to publish, which tracks it moves, and what to stamp on every index it writes"
)]
pub async fn push_with_cascade(
    client: &ocx_oci::Client,
    package_info: crate::info::Info,
    layers: &[ocx_oci::layer_ref::LayerRef],
    other_versions: BTreeSet<Version>,
    version: &Version,
    keep_tag: bool,
    default: bool,
    annotations: &BTreeMap<String, String>,
) -> Result<CascadePushOutcome> {
    let (cascade_tags, _) = resolve_cascade_tags(
        client,
        &package_info.identifier,
        version,
        &other_versions,
        &package_info.platform,
    )
    .await?;

    let (manifest_digest, index, layer_counts) = client
        .push_manifest_and_merge_tags(
            &package_info.identifier,
            &package_info.platform,
            layers,
            &cascade_tags,
            annotations,
            package_info.manifest_builder()?,
        )
        .await?;

    let merged = ocx_oci::Manifest::ImageIndex(index);
    // Hoisted out of the `keep_tag` branch: the platform digest is a signing
    // input and must be reported under `--no-keep-tag` as well.
    let platform_digest = ocx_oci::manifest::platform_manifest_digest(&merged, &package_info.platform);

    let aliases = write_default_variant_aliases(
        client,
        &package_info.identifier,
        &package_info.platform,
        &merged,
        version,
        default,
        Some(&other_versions),
        annotations,
    )
    .await?;

    let keep_tag_written = if keep_tag {
        client
            .push_keep_tag(&package_info.identifier, &merged, &package_info.platform)
            .await?
    } else {
        None
    };

    Ok(CascadePushOutcome {
        index_digest: manifest_digest,
        cascade_tags,
        keep_tag: keep_tag_written,
        platform_digest,
        aliases,
        layer_counts,
    })
}

/// Re-tags the platform manifest this push just landed under the un-prefixed
/// version track, when `default` is set and the pushed version carries a
/// variant.
///
/// Returns the tags written, bare version first — empty for a version that
/// carries no variant, which is the library no-op behind the CLI's exit-64
/// refusal of `--default` on a variant-less tag.
///
/// `other_versions` carries the registry's existing versions when the caller
/// is cascading, and is `None` for a plain push: the bare track then gets the
/// version tag alone, exactly as the variant track did.
///
/// # Why the cascade resolution is reused verbatim
///
/// [`resolve_cascade_tags`] is called against `version.without_variant()`, and
/// that scopes the blocker scan to the bare track by construction rather than
/// by a filter: `Version`'s ordering keys on the variant first with `None`
/// sorting **above** every `Some`, so a level's blocker range between two
/// variant-less endpoints can only contain variant-less versions, and
/// `latest`'s `take_while` on `variant()` stops at the first prefixed one. A
/// newer bare `2.0.0` therefore blocks `latest` exactly as it should, while a
/// newer `slim-2.0.0` does not.
///
/// # The write
///
/// Each alias is a [`Client::merge_platform_into_index`] against the digest the
/// variant push already produced — the index at the target tag is pulled or
/// synthesized, this platform's entry swapped, and the index JSON alone is
/// PUT. No blob and no manifest is uploaded a second time.
///
/// # Errors
///
/// Any registry failure resolving the bare track or writing an alias index. A
/// merged index carrying no entry for this platform writes nothing and returns
/// empty, the same no-op
/// [`Client::push_keep_tag`](ocx_oci::Client::push_keep_tag) makes.
#[expect(
    clippy::too_many_arguments,
    reason = "the manifest this push landed, the tag it landed under, and what the bare track has to clear"
)]
pub(crate) async fn write_default_variant_aliases(
    client: &ocx_oci::Client,
    identifier: &ocx_oci::Identifier,
    platform: &ocx_oci::Platform,
    merged: &ocx_oci::Manifest,
    version: &Version,
    default: bool,
    other_versions: Option<&BTreeSet<Version>>,
    annotations: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    // The flag is off: write nothing.
    if !default {
        return Ok(Vec::new());
    }
    // A variant-less version has no default to declare. The CLI refuses
    // `--default` on such a tag with exit 64; this is the matching library
    // no-op for any other caller.
    if version.variant().is_none() {
        return Ok(Vec::new());
    }
    let Some(entry) = ocx_oci::manifest::platform_manifest_entry(merged, platform) else {
        log::debug!("Skipping default-variant aliases for {identifier}: merged index carries no {platform} entry");
        return Ok(Vec::new());
    };

    let bare = version.without_variant();
    let mut tags = vec![bare.to_string()];
    if let Some(others) = other_versions {
        let (cascade_tags, _) = resolve_cascade_tags(client, identifier, &bare, others, platform).await?;
        tags.extend(cascade_tags);
    }

    for tag in &tags {
        log::debug!("Aliasing default variant onto {tag}");
        client
            .merge_platform_into_index(
                identifier,
                tag.clone(),
                platform,
                &entry.digest,
                entry.size,
                annotations,
            )
            .await?;
    }
    Ok(tags)
}

/// Checks blockers sequentially, returning `true` on first platform match.
///
/// Returns `Err` on registry errors — the caller decides how to handle
/// (typically: stop cascade conservatively with a warning).
///
/// The blocker manifests are read from the **canonical** registry
/// ([`ReadAddressing::Canonical`]), never a configured mirror, for every caller
/// — `subsystem-oci.md` Invariant #5. This probe's answer decides whether a
/// rolling tag at the canonical registry moves, and the two outcomes are not
/// symmetric: an `Err` stops the cascade conservatively (the caller's `Err`
/// arms above), but a *successful* answer that merely omits the platform is
/// taken at face value and moves the tag. A stale or hostile mirror therefore
/// does not need to fail to walk `latest` backwards onto an older release — it
/// only needs to under-report the platforms of the version that should have
/// blocked (CWE-345/367).
async fn has_blocking_platform(
    client: &ocx_oci::Client,
    identifier: &ocx_oci::Identifier,
    blockers: &[Version],
    platform: &ocx_oci::Platform,
) -> Result<bool> {
    for blocker in blockers {
        let blocker_id = identifier.clone_with_tag(blocker.to_string());
        let (_, manifest) = client.fetch_manifest(&blocker_id).await?;
        if ocx_oci::manifest::has_platform(&manifest, platform) {
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
    fn decompose_prerelease_rolling_parent_does_not_self_block() {
        // The rolling parent prerelease (1.0.0-beta, build-less) must never appear
        // as a blocker of its own build-tagged child. `Version` Ord sorts a no-build
        // version above an otherwise-equal with-build version, so an unbounded blocker
        // range pulled the parent in and froze the floating tag at the first build.
        let v = Version::new_prerelease_with_build(1, 0, 0, "beta", "b2");
        let parent = Version::new_prerelease(1, 0, 0, "beta");
        let others: BTreeSet<_> = [parent.clone()].into();
        let d = decompose(&v, &others);

        assert_eq!(d.levels.len(), 1);
        assert_eq!(d.levels[0].target, parent);
        assert!(
            d.levels[0].blockers.is_empty(),
            "rolling parent must not block its own cascade, got {:?}",
            d.levels[0].blockers
        );
        assert!(!d.latest_eligible);
    }

    #[test]
    fn cascade_advances_rolling_parent_prerelease() {
        // A second (and every later) build must still advance the floating
        // `<core>-<prerelease>` tag even though it already exists in `others`.
        let v = Version::new_prerelease_with_build(1, 0, 0, "beta", "b2");
        let parent = Version::new_prerelease(1, 0, 0, "beta");
        let others: BTreeSet<_> = [parent.clone()].into();
        let (versions, is_latest) = cascade(&v, others);

        assert_eq!(versions, vec![v, parent]);
        assert!(!is_latest);
    }

    /// The two halves of the decomposition agree about where a version
    /// cascades to, whatever the version set is.
    ///
    /// `decompose_targets` exists so the audit's fold can derive an alias chain
    /// without paying for a blocker scan at every level; the moment it answered
    /// a different chain from the push path's `decompose`, check would expect a
    /// graph no push would ever have written. Composition is what makes that
    /// impossible, and this is the check on the composition.
    #[test]
    fn decompose_targets_is_the_chain_decompose_walks() {
        let versions = [
            "3.28.1_b1",
            "3.28.1",
            "3.28",
            "3",
            "debug-3.28.1_b1",
            "pgo.lto-1.0",
            "1.0.0-beta",
            "1.0.0-beta_b1",
        ];
        let populations: [Vec<Version>; 3] = [
            vec![],
            vec![v("3.28.2_b1"), v("4.0.0_b1")],
            vec![v("debug-3.28.2_b1"), v("1.0.0-beta_b2"), v("3.28.1")],
        ];

        for version in versions.map(v) {
            let targets = decompose_targets(&version);
            for population in &populations {
                let others: BTreeSet<Version> = population.iter().cloned().collect();
                let full = decompose(&version, &others);
                assert_eq!(
                    full.levels.iter().map(|level| &level.target).collect::<Vec<_>>(),
                    targets.targets.iter().collect::<Vec<_>>(),
                    "chain disagreement for {version} against {population:?}"
                );
                assert_eq!(
                    full.latest_eligible, targets.latest_eligible,
                    "latest eligibility disagreement for {version} against {population:?}"
                );
            }
        }
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
        use ocx_oci::client::test_transport::StubTransportData;

        fn test_client(data: &StubTransportData) -> ocx_oci::Client {
            use ocx_oci::client::test_transport::StubTransport;
            ocx_oci::Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn test_identifier() -> ocx_oci::Identifier {
            ocx_oci::Identifier::new_registry("test/pkg", "example.com")
        }

        fn platform(s: &str) -> ocx_oci::Platform {
            s.parse().unwrap()
        }

        /// Seed an image index manifest for a tag in the stub transport.
        fn seed_index(data: &StubTransportData, tag: &str, platforms: &[&str]) {
            let id = test_identifier().clone_with_tag(tag);
            let manifests: Vec<ocx_oci::ImageIndexEntry> = platforms
                .iter()
                .map(|p| {
                    let plat: ocx_oci::Platform = p.parse().unwrap();
                    let native: ocx_oci::native::Platform = plat.into();
                    ocx_oci::ImageIndexEntry {
                        media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                        digest: format!("sha256:fake_{tag}_{p}"),
                        size: 100,
                        platform: Some(native),
                        artifact_type: None,
                        annotations: None,
                    }
                })
                .collect();
            let index = ocx_oci::Manifest::ImageIndex(ocx_oci::ImageIndex {
                schema_version: 2,
                media_type: None,
                artifact_type: None,
                manifests,
                annotations: None,
            });
            let manifest_data = serde_json::to_vec(&index).unwrap();
            let digest = ocx_oci::Algorithm::Sha256.hash(&manifest_data).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (manifest_data, digest));
        }

        /// Seeds an index under an explicit reference string, so a test can
        /// give the mirror host and the canonical host *different* answers.
        fn seed_index_at(data: &StubTransportData, reference: &str, platforms: &[&str]) {
            let manifests: Vec<ocx_oci::ImageIndexEntry> = platforms
                .iter()
                .map(|p| {
                    let plat: ocx_oci::Platform = p.parse().unwrap();
                    ocx_oci::ImageIndexEntry {
                        media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                        digest: format!("sha256:fake_{p}"),
                        size: 100,
                        platform: Some(plat.into()),
                        artifact_type: None,
                        annotations: None,
                    }
                })
                .collect();
            let index = ocx_oci::Manifest::ImageIndex(ocx_oci::ImageIndex {
                schema_version: 2,
                media_type: None,
                artifact_type: None,
                manifests,
                annotations: None,
            });
            let manifest_data = serde_json::to_vec(&index).unwrap();
            let digest = ocx_oci::Algorithm::Sha256.hash(&manifest_data).to_string();
            data.write()
                .manifests
                .insert(reference.to_string(), (manifest_data, digest));
        }

        /// The blocker probe decides whether a rolling tag at the **canonical**
        /// registry moves, so it must read the canonical registry — Invariant #5.
        ///
        /// The two hosts are seeded with *different* answers, which is what makes
        /// this discriminate: the mirror under-reports the platform (the cheap
        /// half of the attack — the mirror never has to fail), the canonical host
        /// carries it. A mirrored read therefore reports "nothing blocks" and
        /// walks the rolling tag backwards onto the older release; a canonical
        /// read blocks. Asserting only that the mirror 404s would pass for a
        /// mirrored implementation too, via the conservative `Err` arm.
        #[tokio::test]
        async fn the_blocker_probe_reads_the_canonical_registry_not_a_mirror() {
            let data = StubTransportData::new();
            let blocker = Version::new_build(3, 28, 1, "b1");
            let identifier = test_identifier().clone_with_tag(blocker.to_string());
            let mirrored = test_client(&data).with_test_mirror("example.com", "mirror.invalid", "upstream");

            let mirror_reference = mirrored
                .read_reference(&identifier, ocx_oci::client::ReadAddressing::Mirrored)
                .to_string();
            let canonical_reference = identifier.canonical_reference().to_string();
            assert_ne!(
                mirror_reference, canonical_reference,
                "the fixture only discriminates while the two hosts differ"
            );

            // The mirror omits the platform; the canonical registry carries it.
            seed_index_at(&data, &mirror_reference, &["linux/arm64"]);
            seed_index_at(&data, &canonical_reference, &["linux/amd64"]);

            let blocked = has_blocking_platform(
                &mirrored,
                &test_identifier(),
                std::slice::from_ref(&blocker),
                &platform("linux/amd64"),
            )
            .await
            .expect("the canonical registry is seeded, so the probe resolves");

            assert!(
                blocked,
                "the probe must see the canonical registry's platform list; a mirror that merely \
                 under-reports would report 'nothing blocks' and move the rolling tag backwards"
            );
            let auth_hosts: Vec<String> = data
                .read()
                .auth_calls
                .iter()
                .map(|(registry, _)| registry.clone())
                .collect();
            assert!(
                auth_hosts.iter().all(|host| host == "example.com"),
                "a canonical read authenticates against the canonical host, got {auth_hosts:?}"
            );
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
            let manifest = ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default());
            let manifest_data = serde_json::to_vec(&manifest).unwrap();
            let digest = ocx_oci::Algorithm::Sha256.hash(&manifest_data).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (manifest_data, digest));
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

        // ── push_with_cascade keep-tag gating ────────────────

        fn test_info(tag: &str, platform_str: &str) -> crate::info::Info {
            use crate::metadata::{
                Entrypoints, Metadata,
                bundle::{self, Bundle},
                dependency, env as metadata_env,
            };
            let metadata = Metadata::Bundle(Bundle {
                version: bundle::Version::V1,
                strip_components: None,
                env: metadata_env::Env::default(),
                dependencies: dependency::Dependencies::default(),
                entrypoints: Entrypoints::default(),
                binaries: None,
                integrations: Default::default(),
            });
            crate::info::Info {
                identifier: test_identifier().clone_with_tag(tag),
                metadata,
                platform: platform(platform_str),
            }
        }

        #[tokio::test]
        async fn keep_tag_true_tags_only_the_pushed_platform() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = test_client(&data);

            // Pre-existing arm64 entry on the "3" cascade target — must never
            // get retro-tagged by this push (only linux/amd64 was pushed).
            seed_index(&data, "3", &["linux/arm64"]);

            let info = test_info("3.28.1", "linux/amd64");
            let version = Version::new_patch(3, 28, 1);

            let outcome = push_with_cascade(
                &client,
                info,
                &[],
                BTreeSet::new(),
                &version,
                true,
                false,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");
            let keep_tag_written = outcome.keep_tag;

            let inner = data.read();
            let keep_tags: Vec<&String> = inner
                .manifests
                .keys()
                .filter(|key| key.contains(":__ocx.keep."))
                .collect();
            assert_eq!(
                keep_tags.len(),
                1,
                "exactly one keep tag must be pushed, for the platform this call pushed: {keep_tags:?}"
            );
            let reported = keep_tag_written.expect("the cascade push must report the keep tag it wrote");
            assert!(
                keep_tags[0].ends_with(&format!(":{reported}")),
                "the reported tag must be the one on the wire: reported {reported}, wire {keep_tags:?}"
            );
        }

        #[tokio::test]
        async fn keep_tag_false_pushes_no_extra_tag() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = test_client(&data);

            let info = test_info("3.28.1", "linux/amd64");
            let version = Version::new_patch(3, 28, 1);

            let outcome = push_with_cascade(
                &client,
                info,
                &[],
                BTreeSet::new(),
                &version,
                false,
                false,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");

            assert_eq!(outcome.keep_tag, None, "keep_tag=false must report no tag");
            // The independence claim, at the seam that could break it: the
            // platform digest is looked up outside the `keep_tag` branch, so
            // `--no-keep-tag` still reports it. Deriving it from the keep tag
            // would make this `None`.
            let platform_digest = outcome
                .platform_digest
                .expect("the platform digest must be reported with keep tagging off");
            assert_ne!(
                platform_digest, outcome.index_digest,
                "the platform manifest digest must not be the index digest"
            );
            let inner = data.read();
            assert!(
                inner.manifests.keys().all(|key| !key.contains(":__ocx.keep.")),
                "keep_tag=false must not push the extra tag: {:?}",
                inner.manifests.keys().collect::<Vec<_>>()
            );
        }

        // ── push_with_cascade default-variant aliasing ───────

        /// The tags this push wrote, in no particular order. A leaf manifest is
        /// pushed to a digest reference and is deliberately excluded — the
        /// caller counts those separately.
        fn written_tags(data: &StubTransportData) -> Vec<String> {
            let inner = data.read();
            inner
                .manifests
                .keys()
                .filter(|key| !key.contains('@'))
                .filter_map(|key| key.rsplit_once(':').map(|(_, tag)| tag.to_string()))
                .collect()
        }

        /// How many leaf manifests this push uploaded. Every index write goes
        /// to a tag; only `push_multi_layer_manifest` addresses a digest.
        fn manifest_uploads(data: &StubTransportData) -> usize {
            let inner = data.read();
            inner.manifests.keys().filter(|key| key.contains('@')).count()
        }

        #[tokio::test]
        async fn default_variant_aliases_the_bare_track_with_one_manifest_upload() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = test_client(&data);

            let outcome = push_with_cascade(
                &client,
                test_info("full-1.2.3", "linux/amd64"),
                &[],
                BTreeSet::new(),
                &v("full-1.2.3"),
                false,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");

            assert_eq!(
                outcome.aliases,
                vec!["1.2.3", "1.2", "1", "latest"],
                "the bare track is the variant's own cascade with the prefix stripped"
            );
            let mut tags = written_tags(&data);
            tags.sort();
            assert_eq!(
                tags,
                vec![
                    "1",
                    "1.2",
                    "1.2.3",
                    "full",
                    "full-1",
                    "full-1.2",
                    "full-1.2.3",
                    "latest"
                ],
                "both tag sets must be on the wire"
            );
            // The whole point of the feature: eight tags, one upload. A second
            // `push --identifier` would make this two.
            assert_eq!(
                manifest_uploads(&data),
                1,
                "aliasing must re-tag the manifest this push already produced, never upload a second one"
            );
            // The map-based helper above cannot witness a second upload of the
            // SAME digest (idempotent key); the monotonic counter can.
            assert_eq!(
                data.read().digest_manifest_writes,
                1,
                "exactly one leaf-manifest upload, counted rather than deduped by digest key"
            );
        }

        #[tokio::test]
        async fn default_on_a_variant_less_version_writes_no_alias() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = test_client(&data);

            // The CLI refuses `--default` on a variant-less tag (exit 64); this
            // is the library no-op that backs that refusal. A version with no
            // variant has no default track to alias onto.
            let outcome = push_with_cascade(
                &client,
                test_info("1.2.3", "linux/amd64"),
                &[],
                BTreeSet::new(),
                &v("1.2.3"),
                false,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");

            assert!(
                outcome.aliases.is_empty(),
                "a variant-less version aliases nothing even with default set: {:?}",
                outcome.aliases
            );
            // The variant track *is* the bare track here, so a leaked self-alias
            // would re-tag `1.2.3` and its rolling tags a second time. The
            // `manifests` map is keyed on the tag and would swallow that, so the
            // discriminating proof is the write COUNT against the distinct-tag
            // count: every tag written exactly once.
            let mut written = written_tags(&data);
            written.sort();
            assert_eq!(
                written,
                vec!["1", "1.2", "1.2.3", "latest"],
                "the version's own cascade tags: {:?}",
                written
            );
            assert_eq!(
                data.read().tag_manifest_writes,
                written.len(),
                "each cascade tag must be written exactly once — a self-alias would re-write them: \
                 {} writes for {} distinct tags",
                data.read().tag_manifest_writes,
                written.len()
            );
        }

        #[tokio::test]
        async fn without_the_flag_a_variant_push_leaves_the_bare_track_alone() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = test_client(&data);

            let outcome = push_with_cascade(
                &client,
                test_info("full-1.2.3", "linux/amd64"),
                &[],
                BTreeSet::new(),
                &v("full-1.2.3"),
                false,
                false,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");

            assert!(outcome.aliases.is_empty(), "no flag, no aliases: {:?}", outcome.aliases);
            assert!(
                written_tags(&data).iter().all(|tag| tag.starts_with("full")),
                "no bare-track tag may appear: {:?}",
                written_tags(&data)
            );
        }

        #[tokio::test]
        async fn without_the_flag_a_variant_less_push_writes_no_self_alias() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = test_client(&data);

            // The flag is off, so nothing is aliased even though this version
            // carries no variant — the `!default` early return handles it
            // before the variant is ever inspected.
            let outcome = push_with_cascade(
                &client,
                test_info("1.2.3", "linux/amd64"),
                &[],
                BTreeSet::new(),
                &v("1.2.3"),
                false,
                false,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");

            assert!(
                outcome.aliases.is_empty(),
                "an absent flag must not read as a match: {:?}",
                outcome.aliases
            );
        }

        #[tokio::test]
        async fn a_newer_bare_version_blocks_the_aliased_latest() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            seed_index(&data, "2.0.0", &["linux/amd64"]);
            let client = test_client(&data);

            let outcome = push_with_cascade(
                &client,
                test_info("full-1.2.3", "linux/amd64"),
                &[],
                BTreeSet::from([v("2.0.0")]),
                &v("full-1.2.3"),
                false,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");

            assert_eq!(
                outcome.aliases,
                vec!["1.2.3", "1.2", "1"],
                "a published bare 2.0.0 must keep this push off `latest`"
            );
            // The variant's own track is judged against its own blockers and is
            // untouched by the bare release.
            assert_eq!(outcome.cascade_tags, vec!["full-1.2", "full-1", "full"]);
        }

        #[tokio::test]
        async fn a_newer_other_variant_does_not_block_the_aliased_latest() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            // Seeded carrying the very platform being pushed: what keeps it
            // from blocking is the track it is on, not its absence.
            seed_index(&data, "slim-2.0.0", &["linux/amd64"]);
            let client = test_client(&data);

            let outcome = push_with_cascade(
                &client,
                test_info("full-1.2.3", "linux/amd64"),
                &[],
                BTreeSet::from([v("slim-2.0.0")]),
                &v("full-1.2.3"),
                false,
                true,
                &BTreeMap::new(),
            )
            .await
            .expect("cascade push succeeds");

            assert_eq!(
                outcome.aliases,
                vec!["1.2.3", "1.2", "1", "latest"],
                "a newer version on a different variant track has no say over the bare `latest`"
            );
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
