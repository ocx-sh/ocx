// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Index-driven dependency pin resolution for `ocx package create`.
//!
//! [`pin_dependencies`] is the compile step of the create-resolves /
//! push-gates split (`adr_dependency_manifest_pinning.md`): every tag-only
//! dependency in an [`AuthoringMetadata`] is resolved into per-platform
//! **manifest** digests via the selected [`Index`] — never an image-index
//! digest, which registry GC collects as soon as the dependency publisher
//! pushes again. Already-pinned dependencies pass through untouched (no
//! network). The derived package target set is embedded so `ocx package push`
//! can fan out mechanically.
//!
//! Resolution routes through [`Index::fetch_candidates`] with
//! [`IndexOperation::Resolve`], so the `--remote` / `--offline` / `--frozen`
//! routing matrix of `adr_index_routing_semantics.md` applies unchanged.

use std::collections::BTreeMap;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci::{
    self, Platform,
    index::{Index, IndexOperation},
};
use crate::package::metadata::authoring::{AuthoringBundle, AuthoringDependency, AuthoringMetadata, TargetPlatforms};
use crate::{log, package::metadata::authoring::AuthoringDependencies};

/// Resolve every unpinned dependency of `metadata` against `index` and embed
/// the derived target-platform set.
///
/// `declared_platform` is the platform of the package CONTENT (the create
/// `--platform` value):
///
/// - **Specific** platform: each unpinned dependency must advertise exactly
///   one `can_run`-compatible leaf; the dependency gets a single manifest
///   pin. The target set is `[declared_platform]`.
/// - **`any`**: each unpinned dependency's advertised children become a
///   per-dependency `platforms` pin map (or a plain single pin when the
///   dependency itself is platform-agnostic). The target set is the set of
///   specific platforms covered by EVERY dependency, or `[any]` when all
///   dependencies are universal.
///
/// Pinned dependencies (direct digest or non-empty map) are never resolved —
/// they only participate in the coverage intersection.
///
/// # Errors
///
/// See [`DependencyPinningError`]. Index-layer failures (including
/// `--offline`/`--frozen` policy blocks) pass through transparently so exit
/// classification reaches the underlying cause.
pub async fn pin_dependencies(
    metadata: AuthoringMetadata,
    index: &Index,
    declared_platform: &Platform,
) -> Result<AuthoringMetadata, DependencyPinningError> {
    let AuthoringMetadata::Bundle(bundle) = metadata;

    let mut resolved: Vec<AuthoringDependency> = Vec::with_capacity(bundle.dependencies.len());
    for dep in bundle.dependencies.iter() {
        if dep.is_pinned() {
            log::debug!("dependency '{}' already pinned; passing through", dep.identifier);
            resolved.push(dep.clone());
            continue;
        }
        let candidates = fetch_dependency_candidates(index, dep).await?;
        resolved.push(resolve_one(dep, candidates, declared_platform)?);
    }

    let dependencies =
        AuthoringDependencies::new(resolved).expect("re-validated entries mirror an already-validated dependency list");
    let target_platforms = derive_target_platforms(&dependencies, declared_platform)?;
    Ok(AuthoringMetadata::Bundle(AuthoringBundle {
        dependencies,
        platforms: Some(target_platforms),
        ..bundle
    }))
}

/// Fetch the advertised `(leaf identifier, platform)` children for `dep`.
///
/// A flat `Manifest::Image` yields a single `(manifest digest, any)` child by
/// [`Index::fetch_candidates`] construction — GC-safe pins fall out for free.
async fn fetch_dependency_candidates(
    index: &Index,
    dep: &AuthoringDependency,
) -> Result<Vec<(oci::Identifier, Platform)>, DependencyPinningError> {
    let candidates = index
        .fetch_candidates(&dep.identifier, IndexOperation::Resolve)
        .await
        .map_err(DependencyPinningError::Index)?;
    match candidates {
        Some(children) if !children.is_empty() => Ok(children),
        _ => Err(DependencyPinningError::DependencyNotFound {
            identifier: Box::new(dep.identifier.clone()),
        }),
    }
}

/// Resolve a single unpinned dependency from its advertised children.
fn resolve_one(
    dep: &AuthoringDependency,
    candidates: Vec<(oci::Identifier, Platform)>,
    declared_platform: &Platform,
) -> Result<AuthoringDependency, DependencyPinningError> {
    if declared_platform.is_any() {
        return resolve_for_any(dep, candidates);
    }
    resolve_for_specific(dep, candidates, declared_platform)
}

/// Collapse `dep` to a single direct manifest pin on `leaf`'s digest: attach
/// the leaf digest to the identifier and clear any `platforms` map.
///
/// Shared by the concrete-platform winner and the `--platform any`
/// platform-agnostic collapse (FIX W5 dedup).
fn direct_pin(
    dep: &AuthoringDependency,
    leaf: &oci::Identifier,
) -> Result<AuthoringDependency, DependencyPinningError> {
    let digest = require_leaf_digest(dep, leaf)?;
    let mut pinned = dep.clone();
    pinned.identifier = dep.identifier.clone_with_digest(digest);
    pinned.platforms = None;
    Ok(pinned)
}

/// Concrete `--platform P`: exactly one `can_run`-compatible leaf wins.
fn resolve_for_specific(
    dep: &AuthoringDependency,
    candidates: Vec<(oci::Identifier, Platform)>,
    declared_platform: &Platform,
) -> Result<AuthoringDependency, DependencyPinningError> {
    let available: Vec<String> = candidates.iter().map(|(_, platform)| platform.to_string()).collect();
    let compatible: Vec<&(oci::Identifier, Platform)> = candidates
        .iter()
        .filter(|(_, candidate_platform)| declared_platform.can_run(candidate_platform))
        .collect();

    match compatible.as_slice() {
        [] => Err(DependencyPinningError::NoCompatiblePlatform {
            identifier: Box::new(dep.identifier.clone()),
            platform: declared_platform.to_string(),
            available,
        }),
        [(leaf, _)] => direct_pin(dep, leaf),
        many => Err(DependencyPinningError::AmbiguousPlatform {
            identifier: Box::new(dep.identifier.clone()),
            platform: declared_platform.to_string(),
            candidates: many.iter().map(|(_, platform)| platform.to_string()).collect(),
        }),
    }
}

/// `--platform any`: platform-agnostic deps get a plain pin; anything else
/// gets a per-platform pin map keyed by [`Platform::lock_key`].
fn resolve_for_any(
    dep: &AuthoringDependency,
    candidates: Vec<(oci::Identifier, Platform)>,
) -> Result<AuthoringDependency, DependencyPinningError> {
    // A dependency whose only advertised child is platform-agnostic collapses
    // to a plain single pin — no map noise in the sidecar.
    if let [(leaf, platform)] = candidates.as_slice()
        && platform.is_any()
    {
        return direct_pin(dep, leaf);
    }

    let mut map: BTreeMap<String, oci::Digest> = BTreeMap::new();
    for (leaf, platform) in &candidates {
        let key = platform.lock_key();
        let digest = require_leaf_digest(dep, leaf)?;
        if map.insert(key.clone(), digest).is_some() {
            return Err(DependencyPinningError::DuplicatePlatformKey {
                identifier: Box::new(dep.identifier.clone()),
                key,
            });
        }
    }

    let mut pinned = dep.clone();
    pinned.platforms = Some(map);
    Ok(pinned)
}

/// Extract the leaf manifest digest a candidate carries.
///
/// `fetch_candidates` always attaches the child digest via
/// `clone_with_digest`, so a missing digest indicates a malformed index
/// response rather than a user error.
fn require_leaf_digest(
    dep: &AuthoringDependency,
    leaf: &oci::Identifier,
) -> Result<oci::Digest, DependencyPinningError> {
    leaf.digest().ok_or_else(|| DependencyPinningError::DependencyNotFound {
        identifier: Box::new(dep.identifier.clone()),
    })
}

/// Derive the package target-platform set.
///
/// - Concrete `--platform P` → `[P]` (dep compatibility was asserted during
///   resolution; pass-through map coverage is asserted by the caller's
///   `to_published` validation).
/// - `--platform any` with specific platforms in play → every advertised
///   specific platform covered by EVERY dependency (3-tier lookup). The
///   candidate universe is built UNIFORMLY from every dependency's `platforms`
///   map keys — freshly-resolved AND pass-through — so an all-pass-through
///   (already-pinned) sidecar still derives its shared target set (FIX H1).
/// - `--platform any` with no specific platform advertised anywhere → `[any]`,
///   provided every dependency covers `any` (direct pins and any-keyed maps are
///   universal). Genuinely disjoint advertised sets →
///   [`DependencyPinningError::EmptyPlatformIntersection`].
fn derive_target_platforms(
    dependencies: &AuthoringDependencies,
    declared_platform: &Platform,
) -> Result<TargetPlatforms, DependencyPinningError> {
    if !declared_platform.is_any() {
        return Ok(TargetPlatforms::new(vec![declared_platform.clone()])
            .expect("single-platform set is non-empty and duplicate-free"));
    }

    let covered = covered_platforms(dependencies);
    if !covered.is_empty() {
        return Ok(TargetPlatforms::new(covered).expect("candidate lock keys are unique, so platforms are distinct"));
    }

    // Empty coverage: either the candidate universe was empty (every dependency
    // is universal — direct digest pins or `any`-keyed maps) → the whole
    // package is `any`; or the advertised specific platforms are genuinely
    // disjoint → no shared target exists.
    if dependencies.iter().all(|dep| dep.pin_for(&Platform::any()).is_ok()) {
        return Ok(TargetPlatforms::new(vec![Platform::any()]).expect("non-empty"));
    }
    Err(DependencyPinningError::EmptyPlatformIntersection {
        platforms: advertised_platform_keys(dependencies),
    })
}

/// The specific platforms covered by EVERY dependency.
///
/// The candidate universe is the union of each dependency's `platforms` map
/// keys reconstructed to a [`Platform`] via [`Platform::from_lock_key`]. Keys
/// resolving to `any` are universal (not specific candidates); direct-digest
/// and `{"any"}`-only dependencies advertise nothing. A key that is not a valid
/// lock key cannot be a target platform, so it is skipped. The universe is then
/// filtered to the platforms every dependency can pin via the 3-tier lookup
/// ([`AuthoringDependency::pin_for`]).
///
/// Returns the covered platforms in deterministic (lock-key) order; empty when
/// the advertised sets are disjoint or no dependency advertises a specific
/// platform. Used to derive the `--platform any` target set and to pick a
/// publish-time validation platform for an already-pinned sidecar (FIX H1 / H1
/// sibling).
pub fn covered_platforms(dependencies: &AuthoringDependencies) -> Vec<Platform> {
    candidate_universe(dependencies)
        .into_values()
        .filter(|platform| dependencies.iter().all(|dep| dep.pin_for(platform).is_ok()))
        .collect()
}

/// Build the specific-platform candidate universe from every dependency's
/// advertised `platforms` map keys, deduped + ordered by lock key.
fn candidate_universe(dependencies: &AuthoringDependencies) -> BTreeMap<String, Platform> {
    let mut candidates: BTreeMap<String, Platform> = BTreeMap::new();
    for dep in dependencies {
        for key in dep.platforms.iter().flatten().map(|(key, _)| key) {
            match Platform::from_lock_key(key) {
                // `any`-keyed entries are universal, not specific candidates.
                Ok(platform) if platform.is_any() => {}
                Ok(platform) => {
                    // Key by the parsed platform's OWN canonical lock key, never
                    // the raw sidecar string: two textually-distinct keys can
                    // reconstruct to the same `Platform` (e.g. `linux/amd64` and
                    // `linux/amd64;osf=` both yield empty `os_features`), and
                    // keying by the raw string would then admit duplicate
                    // `Platform` values that `TargetPlatforms::new` rejects —
                    // turning untrusted sidecar content into a panic.
                    candidates.entry(platform.lock_key()).or_insert(platform);
                }
                Err(error) => log::debug!("ignoring non-platform pin-map key '{key}': {error}"),
            }
        }
    }
    candidates
}

/// Every platform key any dependency advertises (sorted, deduped) — the
/// diagnostic detail for [`DependencyPinningError::EmptyPlatformIntersection`].
fn advertised_platform_keys(dependencies: &AuthoringDependencies) -> Vec<String> {
    dependencies
        .iter()
        .flat_map(|dep| dep.platforms.iter().flatten().map(|(key, _)| key.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Errors resolving dependency pins at `ocx package create` time.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DependencyPinningError {
    /// The dependency tag does not resolve in the selected index.
    #[error("dependency '{identifier}' not found in the selected index")]
    DependencyNotFound { identifier: Box<oci::Identifier> },
    /// No advertised leaf is compatible with the declared platform.
    #[error(
        "dependency '{identifier}' has no leaf compatible with platform '{platform}' (available: {}); pass --platform matching an available platform, or ask the dependency publisher to add a build for '{platform}'",
        available.join(", ")
    )]
    NoCompatiblePlatform {
        identifier: Box<oci::Identifier>,
        platform: String,
        available: Vec<String>,
    },
    /// More than one advertised leaf is compatible with the declared platform.
    #[error(
        "dependency '{identifier}' is ambiguous for platform '{platform}' (candidates: {}); pin the dependency digest explicitly",
        candidates.join(", ")
    )]
    AmbiguousPlatform {
        identifier: Box<oci::Identifier>,
        platform: String,
        candidates: Vec<String>,
    },
    /// No platform is covered by every dependency.
    #[error(
        "no platform is covered by every dependency (advertised platform keys: {}); narrow --platform or pin dependencies explicitly",
        if platforms.is_empty() { "none".to_string() } else { platforms.join(", ") }
    )]
    EmptyPlatformIntersection { platforms: Vec<String> },
    /// Two advertised children of one dependency share a platform lock key.
    #[error("dependency '{identifier}' advertises duplicate platform key '{key}'")]
    DuplicatePlatformKey {
        identifier: Box<oci::Identifier>,
        key: String,
    },
    /// Index-layer failure (network, policy block, malformed manifest).
    ///
    /// Not `transparent`: the chain walker must reach the inner
    /// [`crate::Error`] via `source()` so its own `ClassifyExitCode`
    /// delegation fires (offline/frozen policy blocks → 81).
    #[error("dependency pin resolution failed")]
    Index(#[from] crate::Error),
}

impl ClassifyExitCode for DependencyPinningError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            DependencyPinningError::DependencyNotFound { .. } => Some(ExitCode::NotFound),
            DependencyPinningError::NoCompatiblePlatform { .. }
            | DependencyPinningError::AmbiguousPlatform { .. }
            | DependencyPinningError::EmptyPlatformIntersection { .. }
            | DependencyPinningError::DuplicatePlatformKey { .. } => Some(ExitCode::DataError),
            // Delegate to the inner index/client cause via the chain walker
            // (offline/frozen policy blocks classify to 81 there).
            DependencyPinningError::Index(_) => None,
        }
    }
}

// ── Specification tests — adr_dependency_manifest_pinning.md Phase 2 ─────
//
// Offline harness: a seeded `LocalIndex` behind `ChainMode::Offline` (or
// `Default` with no sources for the not-found path) exercises the full
// `fetch_candidates` route without any network. Seeder pattern mirrors
// `package_manager/tasks/resolve.rs` spec_tests.
#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::file_structure::{BlobStore, TagStore};
    use crate::oci::index::{ChainMode, LocalConfig, LocalIndex};
    use crate::oci::{Digest, Identifier};

    const REGISTRY: &str = "example.com";

    fn hex(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn digest(ch: char) -> Digest {
        Digest::Sha256(hex(ch))
    }

    fn platform(value: &str) -> Platform {
        value.parse().expect("platform parses")
    }

    fn make_index(dir: &TempDir, mode: ChainMode) -> Index {
        Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            mode,
        )
    }

    fn write_tag_lock(dir: &TempDir, repo: &str, tag: &str, top: &Digest) {
        let tag_store = TagStore::new(dir.path().join("tags"));
        let id = Identifier::new_registry(repo, REGISTRY).clone_with_tag(tag);
        let tag_path = tag_store.tags(&id);
        std::fs::create_dir_all(tag_path.parent().unwrap()).unwrap();
        let json = format!(r#"{{"version":1,"repository":"{REGISTRY}/{repo}","tags":{{"{tag}":"{top}"}}}}"#);
        std::fs::write(tag_path, json).unwrap();
    }

    fn write_blob(dir: &TempDir, blob_digest: &Digest, content: &str) {
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let blob_path = blob_store.data(REGISTRY, blob_digest);
        std::fs::create_dir_all(blob_path.parent().unwrap()).unwrap();
        std::fs::write(blob_path, content).unwrap();
    }

    /// Seed `repo:tag` as an image INDEX whose children are
    /// `(digest, platform-json-or-none)` pairs.
    fn seed_image_index(dir: &TempDir, repo: &str, tag: &str, top: &Digest, children: &[(Digest, Option<&str>)]) {
        write_tag_lock(dir, repo, tag, top);
        let manifests = children
            .iter()
            .map(|(child, platform_json)| {
                let platform_field = platform_json
                    .map(|json| format!(r#","platform":{json}"#))
                    .unwrap_or_default();
                format!(
                    r#"{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child}","size":1{platform_field}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        write_blob(
            dir,
            top,
            &format!(
                r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{manifests}]}}"#
            ),
        );
        for (child, _) in children {
            write_blob(
                dir,
                child,
                r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#,
            );
        }
    }

    /// Seed `repo:tag` as a flat `ImageManifest` (no index).
    fn seed_flat_manifest(dir: &TempDir, repo: &str, tag: &str, top: &Digest) {
        write_tag_lock(dir, repo, tag, top);
        write_blob(
            dir,
            top,
            r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#,
        );
    }

    fn metadata_with_deps(deps: &[&str]) -> AuthoringMetadata {
        let entries = deps
            .iter()
            .map(|identifier| format!(r#"{{"identifier":"{identifier}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[{entries}]}}"#
        ))
        .expect("metadata parses")
    }

    fn dep(metadata: &AuthoringMetadata, index: usize) -> &AuthoringDependency {
        metadata.dependencies().iter().nth(index).expect("dep present")
    }

    fn target_set(metadata: &AuthoringMetadata) -> Vec<String> {
        metadata
            .target_platforms()
            .expect("target set embedded")
            .iter()
            .map(|platform| platform.to_string())
            .collect()
    }

    const LINUX_AMD64: &str = r#"{"os":"linux","architecture":"amd64"}"#;
    const DARWIN_ARM64: &str = r#"{"os":"darwin","architecture":"arm64"}"#;
    const LINUX_AMD64_GLIBC: &str = r#"{"os":"linux","architecture":"amd64","os.features":["libc.glibc"]}"#;
    const LINUX_AMD64_MUSL: &str = r#"{"os":"linux","architecture":"amd64","os.features":["libc.musl"]}"#;

    // ── concrete --platform ───────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn concrete_platform_pins_single_manifest_digest() {
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "java",
            "21",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(DARWIN_ARM64))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let pinned = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect("pinning succeeds");

        let java = dep(&pinned, 0);
        assert_eq!(java.identifier.digest(), Some(digest('b')), "must pin the LEAF digest");
        assert_ne!(
            java.identifier.digest(),
            Some(digest('a')),
            "must NOT pin the index digest"
        );
        assert!(java.platforms.is_none(), "concrete pin carries no map");
        assert_eq!(java.identifier.tag(), Some("21"), "advisory tag preserved");
        assert_eq!(target_set(&pinned), vec!["linux/amd64"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flat_dependency_pins_manifest_for_concrete_platform() {
        // A flat ImageManifest fans out to a single `any` candidate — a
        // concrete platform can run it, so it pins directly.
        let dir = TempDir::new().unwrap();
        seed_flat_manifest(&dir, "tool", "1.0", &digest('a'));
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/tool:1.0"]);

        let pinned = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect("pinning succeeds");
        assert_eq!(dep(&pinned, 0).identifier.digest(), Some(digest('a')));
        assert!(dep(&pinned, 0).platforms.is_none());
        assert_eq!(target_set(&pinned), vec!["linux/amd64"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_compatible_platform_lists_available() {
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "java",
            "21",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64_GLIBC))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let err = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect_err("plain platform cannot run a glibc-only leaf (fail-closed)");
        match &err {
            DependencyPinningError::NoCompatiblePlatform { available, .. } => {
                assert_eq!(available, &vec!["linux/amd64+libc.glibc".to_string()]);
            }
            other => panic!("expected NoCompatiblePlatform, got: {other}"),
        }
        assert_eq!(crate::cli::classify_error(&err), ExitCode::DataError);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ambiguous_platform_lists_candidates() {
        // A host platform declaring both libc families can run both leaves —
        // genuine ambiguity surfaces instead of an arbitrary winner.
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "java",
            "21",
            &digest('a'),
            &[
                (digest('b'), Some(LINUX_AMD64_GLIBC)),
                (digest('c'), Some(LINUX_AMD64_MUSL)),
            ],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let err = pin_dependencies(metadata, &index, &platform("linux/amd64+libc.glibc+libc.musl"))
            .await
            .expect_err("two compatible leaves are ambiguous");
        match &err {
            DependencyPinningError::AmbiguousPlatform { candidates, .. } => {
                assert_eq!(candidates.len(), 2, "both leaves listed: {candidates:?}");
            }
            other => panic!("expected AmbiguousPlatform, got: {other}"),
        }
        assert_eq!(crate::cli::classify_error(&err), ExitCode::DataError);
    }

    // ── --platform any ────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn any_platform_with_agnostic_dep_collapses_to_plain_pin() {
        let dir = TempDir::new().unwrap();
        seed_flat_manifest(&dir, "tool", "1.0", &digest('a'));
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/tool:1.0"]);

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("pinning succeeds");
        assert_eq!(dep(&pinned, 0).identifier.digest(), Some(digest('a')));
        assert!(
            dep(&pinned, 0).platforms.is_none(),
            "agnostic dep gets a plain pin, no map"
        );
        assert_eq!(target_set(&pinned), vec!["any"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn any_platform_builds_per_dep_map() {
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "java",
            "21",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(DARWIN_ARM64))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("pinning succeeds");
        let java = dep(&pinned, 0);
        assert!(
            java.identifier.digest().is_none(),
            "map-bearing dep keeps a digest-less identifier"
        );
        let map = java.platforms.as_ref().expect("map built");
        assert_eq!(map.get("linux/amd64"), Some(&digest('b')));
        assert_eq!(map.get("darwin/arm64"), Some(&digest('c')));
        assert_eq!(target_set(&pinned), vec!["darwin/arm64", "linux/amd64"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mixed_plain_and_libc_deps_intersect_via_base_tier() {
        // dep1 ships a plain linux/amd64 leaf; dep2 ships a glibc-tagged
        // leaf. The glibc platform is covered by BOTH (dep1 via base tier),
        // the plain platform only by dep1 — set = [linux/amd64+libc.glibc].
        let dir = TempDir::new().unwrap();
        seed_image_index(&dir, "plain", "1", &digest('a'), &[(digest('b'), Some(LINUX_AMD64))]);
        seed_image_index(
            &dir,
            "glibc",
            "1",
            &digest('c'),
            &[(digest('d'), Some(LINUX_AMD64_GLIBC))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/plain:1", "example.com/glibc:1"]);

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("pinning succeeds");
        assert_eq!(target_set(&pinned), vec!["linux/amd64+libc.glibc"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disjoint_dep_platforms_yield_empty_intersection() {
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "linuxonly",
            "1",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64))],
        );
        seed_image_index(&dir, "maconly", "1", &digest('c'), &[(digest('d'), Some(DARWIN_ARM64))]);
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/linuxonly:1", "example.com/maconly:1"]);

        let err = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect_err("no platform covered by every dep");
        assert!(
            matches!(err, DependencyPinningError::EmptyPlatformIntersection { .. }),
            "unexpected: {err}"
        );
        assert_eq!(crate::cli::classify_error(&err), ExitCode::DataError);
    }

    // ── pass-through + policy ─────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn pinned_dep_untouched_with_empty_offline_index() {
        // The index is completely empty and offline: any consultation would
        // fail. Success proves pinned deps never reach the index.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Offline);
        let identifier = format!("example.com/java:21@sha256:{}", hex('e'));
        let metadata = metadata_with_deps(&[identifier.as_str()]);

        let pinned = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect("pass-through needs no index");
        assert_eq!(dep(&pinned, 0).identifier.digest(), Some(digest('e')));
        assert_eq!(target_set(&pinned), vec!["linux/amd64"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn offline_unpinned_miss_classifies_policy_blocked() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let err = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect_err("offline + unpinned + local miss must be policy-blocked");
        assert!(matches!(err, DependencyPinningError::Index(_)), "unexpected: {err}");
        assert_eq!(
            crate::cli::classify_error(&err),
            ExitCode::PolicyBlocked,
            "index policy block must reach the classifier through the transparent variant"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_tag_is_dependency_not_found() {
        // Default mode with no sources: the chain walk returns a clean miss
        // (`Ok(None)`), which maps to DependencyNotFound (79).
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Default);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let err = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect_err("unknown tag must not resolve");
        assert!(
            matches!(err, DependencyPinningError::DependencyNotFound { .. }),
            "unexpected: {err}"
        );
        assert_eq!(crate::cli::classify_error(&err), ExitCode::NotFound);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn any_pinned_and_map_dep_intersection_respects_pass_through_maps() {
        // A pass-through dep with a platform map participates in coverage:
        // fresh dep covers linux+darwin, pass-through map covers linux only.
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "fresh",
            "1",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(DARWIN_ARM64))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata: AuthoringMetadata = serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[
                {{"identifier":"example.com/fresh:1"}},
                {{"identifier":"example.com/mapped:1","platforms":{{"linux/amd64":"sha256:{f}"}}}}
            ]}}"#,
            f = hex('f'),
        ))
        .unwrap();

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("pinning succeeds");
        assert_eq!(
            target_set(&pinned),
            vec!["linux/amd64"],
            "darwin excluded: the pass-through map does not cover it"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn all_pass_through_specific_maps_derive_shared_target_set() {
        // H1 regression: every dep is ALREADY pinned as a specific-only map
        // (no `any` key), so nothing is freshly resolved. The candidate
        // universe must come from the maps themselves — otherwise
        // `create --platform any` wrongly fails with EmptyPlatformIntersection.
        // The index is empty + offline, so success proves no network was
        // consulted for the pass-through deps.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Offline);
        let metadata: AuthoringMetadata = serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[
                {{"identifier":"example.com/one:1","platforms":{{"linux/amd64":"sha256:{a}","darwin/arm64":"sha256:{b}"}}}},
                {{"identifier":"example.com/two:1","platforms":{{"linux/amd64":"sha256:{c}","darwin/arm64":"sha256:{d}"}}}}
            ]}}"#,
            a = hex('a'),
            b = hex('b'),
            c = hex('c'),
            d = hex('d'),
        ))
        .unwrap();

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("all-pass-through specific maps derive a shared target set without network");
        assert_eq!(
            target_set(&pinned),
            vec!["darwin/arm64", "linux/amd64"],
            "both platforms are covered by every pass-through map"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pass_through_feature_map_survives_in_target_set() {
        // A feature-bearing pass-through map key must reconstruct via
        // from_lock_key and survive into the derived target set.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Offline);
        let metadata: AuthoringMetadata = serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[
                {{"identifier":"example.com/one:1","platforms":{{"linux/amd64;osf=libc.glibc":"sha256:{a}"}}}}
            ]}}"#,
            a = hex('a'),
        ))
        .unwrap();

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("feature-bearing pass-through map derives a target set");
        assert_eq!(
            target_set(&pinned),
            vec!["linux/amd64+libc.glibc"],
            "the featured platform reconstructs and survives"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn colliding_pass_through_map_keys_do_not_panic() {
        // Regression: two textually-distinct pass-through map keys can
        // reconstruct to the SAME `Platform` (`linux/amd64` and `linux/amd64;osf=`
        // both yield empty `os_features`). The candidate universe must dedupe by
        // canonical lock key, not the raw sidecar string — otherwise
        // `TargetPlatforms::new` sees a duplicate `Platform` and the derivation
        // panics on untrusted sidecar content.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Offline);
        let metadata: AuthoringMetadata = serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[
                {{"identifier":"example.com/one:1","platforms":{{"linux/amd64":"sha256:{a}","linux/amd64;osf=":"sha256:{a}"}}}}
            ]}}"#,
            a = hex('a'),
        ))
        .unwrap();

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("colliding map keys must not panic the derivation");
        assert_eq!(
            target_set(&pinned),
            vec!["linux/amd64"],
            "both raw keys collapse to one canonical platform"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn disjoint_pass_through_maps_yield_empty_intersection() {
        // Genuinely disjoint pass-through maps still error — no shared target.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Offline);
        let metadata: AuthoringMetadata = serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[
                {{"identifier":"example.com/one:1","platforms":{{"linux/amd64":"sha256:{a}"}}}},
                {{"identifier":"example.com/two:1","platforms":{{"darwin/arm64":"sha256:{b}"}}}}
            ]}}"#,
            a = hex('a'),
            b = hex('b'),
        ))
        .unwrap();

        let err = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect_err("no platform covered by every pass-through dep");
        assert!(
            matches!(err, DependencyPinningError::EmptyPlatformIntersection { .. }),
            "unexpected: {err}"
        );
        assert_eq!(crate::cli::classify_error(&err), ExitCode::DataError);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_platform_key_is_data_error() {
        // Two advertised children share a platform (same lock key), so the
        // per-dep pin map cannot hold both.
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "java",
            "21",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(LINUX_AMD64))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let err = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect_err("two children sharing a platform key must fail");
        match &err {
            DependencyPinningError::DuplicatePlatformKey { key, .. } => {
                assert_eq!(key, "linux/amd64", "the colliding key is surfaced");
            }
            other => panic!("expected DuplicatePlatformKey, got: {other}"),
        }
        assert_eq!(crate::cli::classify_error(&err), ExitCode::DataError);
    }
}
