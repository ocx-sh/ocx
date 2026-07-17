// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Index-driven dependency pin resolution for `ocx package create`.
//!
//! [`pin_dependencies`] is the compile step of the create-resolves /
//! push-gates split (`adr_dependency_manifest_pinning.md`): every tag-only
//! dependency in an [`AuthoringMetadata`] is resolved into a per-platform
//! **manifest** digest via the selected [`Index`] — never an image-index
//! digest, which registry GC collects as soon as the dependency publisher
//! pushes again. Already-pinned dependencies pass through untouched (no
//! network).
//!
//! Resolution routes through [`Index::fetch_candidates`] with
//! [`IndexOperation::Resolve`], so the `--remote` / `--offline` / `--frozen`
//! routing matrix of `adr_index_routing_semantics.md` applies unchanged.
//!
//! A bundle targets exactly one platform per `create` invocation
//! (`adr_platform_model_unification.md` D5) — `declared_platform` is that
//! single value, which may itself be [`Platform::Any`]. Every dependency
//! (fresh or already-pinned) is resolved against the SAME directed
//! compatibility relation [`crate::oci::select_best`] uses at fresh-resolve
//! time (D1), so an `any`-targeted bundle's dependencies are structurally
//! restricted to `any`-offered candidates: [`is_compatible`](crate::oci::is_compatible)
//! rule 2 says an `Any` requirement is satisfied only by an `Any` offer.

use std::collections::BTreeMap;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci::{
    self, Platform, Selection,
    index::{Index, IndexOperation},
    select_best,
};
use crate::package::metadata::authoring::{AuthoringBundle, AuthoringDependency, AuthoringMetadata};
use crate::{log, package::metadata::authoring::AuthoringDependencies};

/// Resolve every unpinned dependency of `metadata` against `index` for the
/// single `declared_platform` (the create `--platform` value).
///
/// Each unpinned dependency must advertise exactly one candidate compatible
/// with `declared_platform` under [`select_best`]:
///
/// - **Specific** `declared_platform`: the winning leaf is pinned directly on
///   the dependency's identifier (a bare `@digest`) — unchanged from before.
/// - **`any`**: the winning leaf (which can only be the dependency's own
///   `any`-typed offer — [`is_compatible`](crate::oci::is_compatible) rule 2)
///   is recorded as a single `"any"`-keyed entry in the dependency's
///   `platforms` map, never a bare digest pin. This keeps the pin's
///   provenance verifiable (D5): a bare digest carries no platform
///   descriptor, so it cannot be checked to be genuinely `any`-offered,
///   while a freshly-written `"any"` map key is exactly what this function
///   just confirmed via `fetch_candidates`.
///
/// Before any resolution, every dependency — pinned or not — is checked
/// against the D5 any-target invariant: an `any`-targeted bundle prohibits a
/// direct digest pin on any dependency (its provenance is unverifiable). This
/// pass inspects already-pinned dependencies too, not just the ones this call
/// freshly resolves.
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

    if declared_platform.is_any()
        && let Some(identifier) = reject_digest_pins_in_any_target(&bundle.dependencies)
    {
        return Err(DependencyPinningError::DirectDigestPinInAnyTarget { identifier });
    }

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
    Ok(AuthoringMetadata::Bundle(AuthoringBundle { dependencies, ..bundle }))
}

/// D5: find a direct digest pin among an `any`-targeted bundle's
/// dependencies, if any — a leaf manifest carries no platform descriptor, so
/// a bare `@digest` pin cannot be verified to be `any`-offered. Runs as its
/// own pass over every declared dependency — including already-pinned ones —
/// rather than being folded into the fresh-resolution loop, which only ever
/// sees unpinned entries.
///
/// Shared by [`pin_dependencies`] (create time) and
/// [`crate::publisher::publish_gate::verify_dependency_pins`] (push time) —
/// both must reject the same condition (`adr_platform_model_unification.md`
/// D5).
pub(crate) fn reject_digest_pins_in_any_target(dependencies: &AuthoringDependencies) -> Option<Box<oci::Identifier>> {
    dependencies
        .iter()
        .find(|dep| dep.identifier.digest().is_some())
        .map(|dep| Box::new(dep.identifier.clone()))
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

/// Resolve a single unpinned dependency's advertised children against
/// `declared_platform` via [`select_best`] — the same relation and scoring
/// [`crate::oci::Index::select`] uses at fresh-resolve time
/// (authoring-vs-Index parity, D1).
fn resolve_one(
    dep: &AuthoringDependency,
    candidates: Vec<(oci::Identifier, Platform)>,
    declared_platform: &Platform,
) -> Result<AuthoringDependency, DependencyPinningError> {
    let available: Vec<String> = candidates.iter().map(|(_, platform)| platform.to_string()).collect();

    match select_best(declared_platform, &candidates) {
        Selection::Found(leaf) if declared_platform.is_any() => any_pin(dep, &leaf),
        Selection::Found(leaf) => direct_pin(dep, &leaf),
        Selection::None => Err(DependencyPinningError::NoCompatiblePlatform {
            identifier: Box::new(dep.identifier.clone()),
            platform: declared_platform.to_string(),
            available,
        }),
        Selection::Ambiguous(winners) => Err(DependencyPinningError::AmbiguousPlatform {
            identifier: Box::new(dep.identifier.clone()),
            platform: declared_platform.to_string(),
            candidates: winning_platforms(&winners, &candidates),
        }),
    }
}

/// Map each winning leaf identifier back to its advertised platform string,
/// for the [`DependencyPinningError::AmbiguousPlatform`] diagnostic —
/// `select_best`'s `Ambiguous` outcome carries only the tied leaves, not
/// their platforms.
fn winning_platforms(winners: &[oci::Identifier], candidates: &[(oci::Identifier, Platform)]) -> Vec<String> {
    winners
        .iter()
        .filter_map(|winner| {
            candidates
                .iter()
                .find(|(identifier, _)| identifier == winner)
                .map(|(_, platform)| platform.to_string())
        })
        .collect()
}

/// Collapse `dep` to a single direct manifest pin on `leaf`'s digest: attach
/// the leaf digest to the identifier and clear any `platforms` map.
///
/// Used for a **Specific** `declared_platform` — D5 keeps direct digest pins
/// unchanged for concrete-targeted bundles (their target platform is known,
/// so the pin's provenance is not in question).
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

/// Record `leaf`'s digest as the single `"any"`-keyed entry in `dep`'s
/// `platforms` map, leaving the identifier's own digest unset.
///
/// Used for an **`any`** `declared_platform` — D5 prohibits a bare `@digest`
/// pin on an `any`-targeted dependency (unverifiable provenance), so the pin
/// lives in the map instead, under the canonical `Platform::Any` key.
/// `resolve_one` only calls this for a `select_best` winner scored against an
/// `any` requirement, which by [`is_compatible`](crate::oci::is_compatible)
/// rule 2 is always itself an `any`-typed offer — the key is always literally
/// `"any"`.
fn any_pin(dep: &AuthoringDependency, leaf: &oci::Identifier) -> Result<AuthoringDependency, DependencyPinningError> {
    let digest = require_leaf_digest(dep, leaf)?;
    let mut pinned = dep.clone();
    let mut map: BTreeMap<String, oci::Digest> = BTreeMap::new();
    map.insert(Platform::any().to_string(), digest);
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

/// Errors resolving dependency pins at `ocx package create` time.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DependencyPinningError {
    /// The dependency tag does not resolve in the selected index.
    #[error("dependency '{identifier}' not found in the selected index")]
    DependencyNotFound { identifier: Box<oci::Identifier> },
    /// No advertised leaf is compatible with the declared platform. For a
    /// declared `any` platform this is D5's "the dependency offers no `any`
    /// manifest" case — the same variant, since the underlying cause
    /// (`select_best` found no compatible candidate) is identical.
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
    /// D5: an `any`-targeted bundle carries a direct digest pin on a
    /// dependency. A leaf manifest carries no platform descriptor, so a
    /// bare `@digest` pin cannot be verified to be `any`-offered — the rule
    /// applies to already-pinned dependencies too, not only freshly
    /// resolved ones.
    #[error(
        "dependency '{identifier}' carries a direct digest pin in an `any`-targeted bundle; `any` deps must resolve through `ocx package create --platform any` (unverifiable pin provenance)"
    )]
    DirectDigestPinInAnyTarget { identifier: Box<oci::Identifier> },
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
            | DependencyPinningError::DirectDigestPinInAnyTarget { .. } => Some(ExitCode::DataError),
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

    const LINUX_AMD64: &str = r#"{"os":"linux","architecture":"amd64"}"#;
    const DARWIN_ARM64: &str = r#"{"os":"darwin","architecture":"arm64"}"#;
    const LINUX_AMD64_GLIBC: &str = r#"{"os":"linux","architecture":"amd64","os.features":["libc.glibc"]}"#;
    const LINUX_AMD64_MUSL: &str = r#"{"os":"linux","architecture":"amd64","os.features":["libc.musl"]}"#;
    const ANY_PLATFORM: &str = r#"{"os":"any","architecture":"any"}"#;

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

        let err = pin_dependencies(metadata, &index, &platform("linux/amd64+libc.glibc,libc.musl"))
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

    /// D1 authoring-vs-Index parity (a): a Specific candidate present
    /// alongside an `Any` candidate wins outright — no ambiguity — matching
    /// `Index::select`'s specificity scoring.
    #[tokio::test(flavor = "multi_thread")]
    async fn specific_candidate_beats_any_candidate_no_ambiguity() {
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "java",
            "21",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(ANY_PLATFORM))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let pinned = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect("a Specific candidate must win outright over a co-present Any candidate");
        assert_eq!(
            dep(&pinned, 0).identifier.digest(),
            Some(digest('b')),
            "the Specific leaf must win, not the Any leaf"
        );
    }

    /// D1 authoring-vs-Index parity (b): a feature-specific candidate beats
    /// a co-present bare candidate for a feature-bearing declared platform
    /// — matching `Index::select`'s scoring
    /// (`platform.rs::select_best_feature_specific_beats_bare`): score
    /// `(1, 1)` (bare offer, one matched feature) beats `(1, 0)` (bare
    /// offer, no features).
    #[tokio::test(flavor = "multi_thread")]
    async fn feature_specific_candidate_beats_bare_candidate() {
        let dir = TempDir::new().unwrap();
        seed_image_index(
            &dir,
            "java",
            "21",
            &digest('a'),
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(LINUX_AMD64_GLIBC))],
        );
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let pinned = pin_dependencies(metadata, &index, &platform("linux/amd64+libc.glibc"))
            .await
            .expect("a feature-specific candidate must win outright over a co-present bare candidate");
        assert_eq!(
            dep(&pinned, 0).identifier.digest(),
            Some(digest('c')),
            "the glibc-featured leaf must win, not the bare leaf"
        );
    }

    // ── --platform any ────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn any_platform_with_agnostic_dep_pins_into_map() {
        let dir = TempDir::new().unwrap();
        seed_flat_manifest(&dir, "tool", "1.0", &digest('a'));
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/tool:1.0"]);

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("pinning succeeds");
        let tool = dep(&pinned, 0);
        assert!(
            tool.identifier.digest().is_none(),
            "an any-targeted pin must NOT be a bare digest (D5: unverifiable provenance)"
        );
        let map = tool.platforms.as_ref().expect("any pin recorded in the platforms map");
        assert_eq!(map.get("any"), Some(&digest('a')));
        assert_eq!(map.len(), 1);
    }

    /// D5: a dependency offering only Specific leaves (no `any` manifest)
    /// fails an `any`-targeted create with a clear dependency-pinning error.
    #[tokio::test(flavor = "multi_thread")]
    async fn any_platform_with_no_any_offer_fails() {
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

        let err = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect_err("a dependency offering no `any` manifest must fail an any-targeted create");
        assert!(
            matches!(err, DependencyPinningError::NoCompatiblePlatform { .. }),
            "unexpected: {err}"
        );
        assert_eq!(crate::cli::classify_error(&err), ExitCode::DataError);
    }

    // ── D5: direct digest pin prohibition in any-targeted bundles ──────

    /// D5: a fresh `any`-targeted create rejects a dependency that ALREADY
    /// carries a direct digest pin — checked before any network contact.
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_rejects_already_pinned_direct_digest() {
        let dir = TempDir::new().unwrap();
        // Index is empty + offline: success would require zero network
        // contact, but this must fail before even trying.
        let index = make_index(&dir, ChainMode::Offline);
        let identifier = format!("example.com/java:21@sha256:{}", hex('e'));
        let metadata = metadata_with_deps(&[identifier.as_str()]);

        let err = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect_err("a direct digest pin in an any-targeted bundle must be rejected");
        match &err {
            DependencyPinningError::DirectDigestPinInAnyTarget { identifier } => {
                assert_eq!(identifier.digest(), Some(digest('e')));
            }
            other => panic!("expected DirectDigestPinInAnyTarget, got: {other}"),
        }
        assert_eq!(crate::cli::classify_error(&err), ExitCode::DataError);
    }

    /// The direct-digest-pin prohibition applies even when the offending
    /// dependency is not the one this call would otherwise resolve — the
    /// validation pass inspects every declared dependency up front.
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_rejects_direct_digest_among_mixed_dependencies() {
        let dir = TempDir::new().unwrap();
        seed_flat_manifest(&dir, "tool", "1.0", &digest('a'));
        let index = make_index(&dir, ChainMode::Offline);
        let pinned_identifier = format!("example.com/pinned:1@sha256:{}", hex('e'));
        let metadata: AuthoringMetadata = serde_json::from_str(&format!(
            r#"{{"type":"bundle","version":1,"dependencies":[
                {{"identifier":"example.com/tool:1.0"}},
                {{"identifier":"{pinned_identifier}"}}
            ]}}"#
        ))
        .unwrap();

        let err = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect_err("the direct digest pin among mixed deps must be rejected");
        assert!(
            matches!(err, DependencyPinningError::DirectDigestPinInAnyTarget { .. }),
            "unexpected: {err}"
        );
    }

    /// Concrete-targeted bundles keep direct digest pins unchanged (D5) —
    /// the validation pass only fires for an `any`-targeted bundle.
    #[tokio::test(flavor = "multi_thread")]
    async fn specific_target_allows_already_pinned_direct_digest() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir, ChainMode::Offline);
        let identifier = format!("example.com/java:21@sha256:{}", hex('e'));
        let metadata = metadata_with_deps(&[identifier.as_str()]);

        let pinned = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect("a concrete target must not reject a direct digest pin");
        assert_eq!(dep(&pinned, 0).identifier.digest(), Some(digest('e')));
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
}
