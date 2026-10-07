// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Index-driven dependency pin resolution for `ocx package create`.
//!
//! Pins are **manifest** digests, never image-index digests, which registry GC
//! collects once the dependency publisher pushes again (`adr_dependency_manifest_pinning.md`).

use crate::metadata::authoring::AuthoringDependencies;
use crate::metadata::authoring::{AuthoringDependency, AuthoringMetadata};
use ocx_index::{Index, IndexOperation};
use ocx_oci::{self, Platform, Selection, select_best};

/// Pins every unpinned dependency of `metadata` to the one leaf [`select_best`]
/// picks for `declared_platform`; already-pinned ones pass through, except under
/// `any`, where any pre-existing digest pin is refused.
///
/// # Errors
///
/// See [`DependencyPinningError`]; index failures keep their cause reachable.
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
    let mut bundle = bundle;
    bundle.dependencies = dependencies;
    Ok(AuthoringMetadata::Bundle(bundle))
}

/// Finds a pre-existing digest pin in an `any`-targeted bundle, which create
/// cannot verify as `any`-offered (push checks the registry instead).
fn reject_digest_pins_in_any_target(dependencies: &AuthoringDependencies) -> Option<Box<ocx_oci::PackageRef>> {
    dependencies
        .iter()
        .find(|dep| dep.identifier.digest().is_some())
        .map(|dep| Box::new(dep.identifier.clone()))
}

/// Fetches the advertised `(leaf identifier, platform)` children for `dep`.
async fn fetch_dependency_candidates(
    index: &Index,
    dep: &AuthoringDependency,
) -> Result<Vec<(ocx_oci::PackageRef, Platform)>, DependencyPinningError> {
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

/// Resolves one unpinned dependency's children against `declared_platform` via [`select_best`].
fn resolve_one(
    dep: &AuthoringDependency,
    candidates: Vec<(ocx_oci::PackageRef, Platform)>,
    declared_platform: &Platform,
) -> Result<AuthoringDependency, DependencyPinningError> {
    let available: Vec<String> = candidates.iter().map(|(_, platform)| platform.to_string()).collect();

    match select_best(declared_platform, &candidates) {
        Selection::Found(leaf) => pin(dep, &leaf),
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

fn winning_platforms(winners: &[ocx_oci::PackageRef], candidates: &[(ocx_oci::PackageRef, Platform)]) -> Vec<String> {
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

/// Attaches `leaf`'s digest to `dep`'s identifier, keeping the advisory tag.
fn pin(dep: &AuthoringDependency, leaf: &ocx_oci::PackageRef) -> Result<AuthoringDependency, DependencyPinningError> {
    let digest = require_leaf_digest(dep, leaf)?;
    let mut pinned = dep.clone();
    pinned.identifier = dep.identifier.clone_with_digest(digest);
    Ok(pinned)
}

/// The leaf's manifest digest; missing only on a malformed index response.
fn require_leaf_digest(
    dep: &AuthoringDependency,
    leaf: &ocx_oci::PackageRef,
) -> Result<ocx_oci::Digest, DependencyPinningError> {
    leaf.digest().ok_or_else(|| DependencyPinningError::DependencyNotFound {
        identifier: Box::new(dep.identifier.clone()),
    })
}

/// Errors resolving dependency pins at `ocx package create` time.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum DependencyPinningError {
    /// The dependency tag does not resolve in the selected index.
    #[error("dependency '{identifier}' not found in the selected index")]
    #[exit(
        NotFound,
        slug = "dependency_not_found",
        summary = "A dependency is not in the selected index"
    )]
    DependencyNotFound { identifier: Box<ocx_oci::PackageRef> },
    /// No advertised leaf is compatible with the declared platform.
    #[error(
        "dependency '{identifier}' has no leaf compatible with platform '{platform}' (available: {}); pass --platform matching an available platform, or ask the dependency publisher to add a build for '{platform}'",
        available.join(", ")
    )]
    #[exit(
        DataError,
        slug = "dependency_no_compatible_platform",
        summary = "A dependency is published for no platform compatible with the target"
    )]
    NoCompatiblePlatform {
        identifier: Box<ocx_oci::PackageRef>,
        platform: String,
        available: Vec<String>,
    },
    /// More than one advertised leaf is compatible with the declared platform.
    #[error(
        "dependency '{identifier}' is ambiguous for platform '{platform}' (candidates: {}); pin the dependency digest explicitly",
        candidates.join(", ")
    )]
    #[exit(
        DataError,
        slug = "dependency_ambiguous_platform",
        summary = "Several of a dependency's platforms match the target equally"
    )]
    AmbiguousPlatform {
        identifier: Box<ocx_oci::PackageRef>,
        platform: String,
        candidates: Vec<String>,
    },
    /// An `any`-targeted bundle carries a direct digest pin on a dependency.
    #[error(
        "dependency '{identifier}' carries a direct digest pin in an `any`-targeted bundle; `any` deps must resolve through `ocx package create --platform any` (unverifiable pin provenance)"
    )]
    #[exit(
        DataError,
        slug = "direct_digest_pin_in_any_target",
        summary = "An `any` package pins a dependency by a platform-specific digest"
    )]
    DirectDigestPinInAnyTarget { identifier: Box<ocx_oci::PackageRef> },
    /// Index-layer failure (network, policy block, malformed manifest).
    // Not `transparent`, which would hide the index error from the exit-code chain walk.
    #[error("dependency pin resolution failed")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "dependency_pin_resolution_failed",
            summary = "Resolving a dependency pin failed with an unclassified cause"
        )
    )]
    Index(#[from] ocx_index::error::Error),
}

// ── Specification tests — adr_dependency_manifest_pinning.md ─────────────
//
// Offline harness: a seeded `LocalIndex` behind `ChainMode::Offline` (or
// `Default` with no sources for the not-found path) drives `fetch_candidates`
// with no network.
#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use ocx_index::error::Result;
    use ocx_index::{ChainMode, IndexImpl, IndexStore, LocalConfig, LocalIndex};
    use ocx_oci::{Algorithm, Digest};
    use ocx_store::file_structure::FileStructure;

    const REGISTRY: &str = "example.com";
    const FLAT_MANIFEST_JSON: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;

    fn hex(ch: char) -> String {
        ch.to_string().repeat(64)
    }

    fn digest(ch: char) -> Digest {
        Digest::Sha256(hex(ch))
    }

    fn platform(value: &str) -> Platform {
        value.parse().expect("platform parses")
    }

    fn index_store(dir: &TempDir) -> IndexStore {
        IndexStore::machine_local(&FileStructure::with_root(dir.path().to_path_buf()))
    }

    fn make_index(dir: &TempDir, mode: ChainMode) -> Index {
        Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: index_store(dir),
            }),
            Vec::new(),
            mode,
        )
    }

    /// Author a DERIVED root document (`adr_index_indirection.md` A2) recording
    /// `repo:tag → digest`.
    fn seed_tag(dir: &TempDir, repo: &str, tag: &str, top: &Digest) {
        let store = index_store(dir);
        let root_path = store.root_document_path(REGISTRY, repo);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        let doc = serde_json::json!({
            "repository": format!("oci://{REGISTRY}/{repo}"),
            "tags": { tag: { "content": top.to_string(), "observed": "2026-07-18T00:00:00Z" } }
        });
        std::fs::write(&root_path, serde_json::to_vec(&doc).unwrap()).unwrap();
    }

    /// Write `bytes` verbatim into the index store's dispatch-object CAS under
    /// their own digest (bytes hash to filename, A3) and return it.
    /// Dispatch-shaped fixtures only (image indexes) — a leaf manifest is
    /// never locally cached, see `FlatManifestSource` for that case.
    async fn seed_object(dir: &TempDir, repo: &str, bytes: &[u8]) -> Digest {
        let store = index_store(dir);
        let d = Algorithm::Sha256.hash(bytes);
        store.write_dispatch_object(REGISTRY, repo, &d, bytes).await.unwrap();
        d
    }

    /// Seed `repo:tag` as an image INDEX whose children are
    /// `(digest, platform-json-or-none)` pairs. Returns the computed index
    /// digest (the child manifests are only *referenced*, never read, so they
    /// are not stored).
    async fn seed_image_index(dir: &TempDir, repo: &str, tag: &str, children: &[(Digest, Option<&str>)]) -> Digest {
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
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{manifests}]}}"#
        );
        let top = seed_object(dir, repo, index_json.as_bytes()).await;
        seed_tag(dir, repo, tag, &top);
        top
    }

    /// A minimal fake source serving one `repo:tag → FLAT_MANIFEST_JSON`
    /// mapping — used with `ChainMode::Default` so `pin_dependencies` can
    /// recover a LEAF platform manifest for a single-platform dependency. A
    /// leaf is never locally cached (A3), so an offline-only pre-seeded
    /// fixture cannot answer a resolve for content that names one; this fake
    /// source drives the same absent-dispatch recovery path a live registry would.
    #[derive(Clone)]
    struct FlatManifestSource {
        repo: String,
        tag: String,
    }

    #[async_trait::async_trait]
    impl IndexImpl for FlatManifestSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            identifier: &ocx_oci::PackageRef,
            _op: ocx_index::IndexOperation,
        ) -> Result<Option<(Digest, ocx_oci::Manifest)>> {
            if identifier.repository() != self.repo || identifier.tag_or_latest() != self.tag {
                return Ok(None);
            }
            let digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON.as_bytes());
            Ok(Some((digest, serde_json::from_str(FLAT_MANIFEST_JSON).unwrap())))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &ocx_oci::PackageRef,
            _op: ocx_index::IndexOperation,
        ) -> Result<Option<Digest>> {
            if identifier.repository() != self.repo || identifier.tag_or_latest() != self.tag {
                return Ok(None);
            }
            Ok(Some(Algorithm::Sha256.hash(FLAT_MANIFEST_JSON.as_bytes())))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, Digest, ocx_oci::Manifest)>> {
            Ok(self
                .fetch_manifest(identifier, ocx_index::IndexOperation::Resolve)
                .await?
                .map(|(digest, manifest)| (FLAT_MANIFEST_JSON.as_bytes().to_vec(), digest, manifest)))
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Build a `ChainMode::Default` index chained to a [`FlatManifestSource`]
    /// for `repo:tag`, so a `pin_dependencies` resolve of a single-platform
    /// dependency recovers its (never-locally-cached) leaf manifest.
    fn make_index_with_flat_source(dir: &TempDir, repo: &str, tag: &str) -> Index {
        Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: index_store(dir),
            }),
            vec![Index::from_impl(FlatManifestSource {
                repo: repo.to_string(),
                tag: tag.to_string(),
            })],
            ChainMode::Default,
        )
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
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(DARWIN_ARM64))],
        )
        .await;
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
        assert_eq!(java.identifier.tag(), Some("21"), "advisory tag preserved");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn flat_dependency_pins_manifest_for_concrete_platform() {
        // A flat ImageManifest fans out to a single `any` candidate — a
        // concrete platform can run it, so it pins directly. A leaf platform
        // manifest is never locally cached (A3), so the index is chained to
        // a live fake source under `ChainMode::Default` that recovers it —
        // mirrors a real single-platform dependency resolve.
        let dir = TempDir::new().unwrap();
        let tool_digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON.as_bytes());
        let index = make_index_with_flat_source(&dir, "tool", "1.0");
        let metadata = metadata_with_deps(&["example.com/tool:1.0"]);

        let pinned = pin_dependencies(metadata, &index, &platform("linux/amd64"))
            .await
            .expect("pinning succeeds");
        assert_eq!(dep(&pinned, 0).identifier.digest(), Some(tool_digest));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_compatible_platform_lists_available() {
        let dir = TempDir::new().unwrap();
        seed_image_index(&dir, "java", "21", &[(digest('b'), Some(LINUX_AMD64_GLIBC))]).await;
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
            &[
                (digest('b'), Some(LINUX_AMD64_GLIBC)),
                (digest('c'), Some(LINUX_AMD64_MUSL)),
            ],
        )
        .await;
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
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(ANY_PLATFORM))],
        )
        .await;
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
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(LINUX_AMD64_GLIBC))],
        )
        .await;
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
    async fn any_platform_with_agnostic_dep_pins_the_any_manifest_digest() {
        // An `any` target takes the same pin shape as a concrete one: the
        // winning leaf's digest, bare on the identifier. `select_best` already
        // established the winner is `any`-offered, and push re-derives that
        // from the dependency's own image index rather than trusting the
        // sidecar's word for it.
        //
        // A leaf platform manifest is never locally cached (A3); a live fake
        // source under `ChainMode::Default` recovers it.
        let dir = TempDir::new().unwrap();
        let tool_digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON.as_bytes());
        let index = make_index_with_flat_source(&dir, "tool", "1.0");
        let metadata = metadata_with_deps(&["example.com/tool:1.0"]);

        let pinned = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect("pinning succeeds");
        let tool = dep(&pinned, 0);
        assert_eq!(tool.identifier.digest(), Some(tool_digest));
        assert_eq!(tool.identifier.tag(), Some("1.0"), "advisory tag preserved");
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
            &[(digest('b'), Some(LINUX_AMD64)), (digest('c'), Some(DARWIN_ARM64))],
        )
        .await;
        let index = make_index(&dir, ChainMode::Offline);
        let metadata = metadata_with_deps(&["example.com/java:21"]);

        let err = pin_dependencies(metadata, &index, &Platform::any())
            .await
            .expect_err("a dependency offering no `any` manifest must fail an any-targeted create");
        assert!(
            matches!(err, DependencyPinningError::NoCompatiblePlatform { .. }),
            "unexpected: {err}"
        );
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
    }

    /// The direct-digest-pin prohibition applies even when the offending
    /// dependency is not the one this call would otherwise resolve — the
    /// validation pass inspects every declared dependency up front.
    #[tokio::test(flavor = "multi_thread")]
    async fn any_target_rejects_direct_digest_among_mixed_dependencies() {
        // "tool" is deliberately unseeded — the digest-pin validation fires
        // before any dependency is resolved, so it must never be queried.
        let dir = TempDir::new().unwrap();
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
    }
}
