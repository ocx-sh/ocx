// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Two-phase visible-package pipeline for env resolution.
//!
//! Phase A — [`import_visible_packages`] — walks each root's `resolve.json`
//! closure and returns the set of packages that are visible to consumers, in
//! topological order (deps before dependents).
//!
//! Phase B — [`apply_visible_packages`] — iterates the visible set and emits
//! resolved environment variable entries by calling the existing
//! `tasks::common::export_env` helper. The apply phase performs no I/O and
//! does not reload metadata; all required data is already on `InstallInfo`.
//!
//! Together these two phases replace the inline loop that previously lived
//! inside `PackageManager::resolve_env`. Each phase is independently testable:
//! Phase A unit tests assert topological order, sealed-dep exclusion, and
//! diamond-dep dedup; Phase B unit tests assert emission order.
//!
//! # ADR
//!
//! See `.claude/artifacts/adr_visible_package_pipeline.md` for the full design
//! rationale, invariants, and implementation order (F8a/F8b/F8c/F9).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::{
    file_structure::PackageStore,
    oci,
    package::{
        install_info::InstallInfo,
        metadata::visibility::Visibility,
        metadata::{
            dependency::DependencyName,
            entrypoint::EntrypointName,
            env::{accumulator::DependencyContext, exporter::Entry, modifier::ModifierKind},
        },
    },
    package_manager::error::PackageErrorKind,
};

use super::tasks::common;

/// One package in a root's visible closure.
///
/// Wraps the on-disk `Arc<InstallInfo>` without cloning metadata or content
/// path. `scope` carries the propagated visibility and any import-time metadata
/// that the apply phase needs.
pub struct VisiblePackage {
    /// The installed package data — metadata, resolved closure, content path.
    pub install_info: Arc<InstallInfo>,
    /// Propagated visibility from the root plus import-time-resolved metadata.
    pub scope: ImportScope,
}

/// Import-time metadata for one entry in the visible closure.
///
/// This struct is the carrier for Phase A results that Phase B consumes.
/// Future fields (resolved version/digest for `${deps.NAME.version}`) land
/// here additively without changing the apply-phase signature.
pub struct ImportScope {
    /// Effective visibility from the root, post-propagation/merge.
    pub visibility: Visibility,
    /// `true` iff this package was one of the input roots (not a transitive dep).
    pub is_root: bool,
    /// Pre-built dep_contexts for `${deps.NAME.installPath}` interpolation.
    /// Scoped to this package's own direct deps (not the root's deps).
    pub(crate) dep_contexts: HashMap<DependencyName, DependencyContext>,
}

/// Walks the dependency closure of each root, filters by visibility, deduplicates
/// by digest (diamond merge via first-seen-wins with conflict logging), and
/// returns the visible packages in topological order (deps before dependents,
/// roots last).
///
/// The returned `Vec<VisiblePackage>` satisfies:
/// - Topological order: when a package appears at index `i`, every dep whose
///   env var it needs for `${deps.*}` interpolation appears at some index `j < i`.
/// - No sealed packages: `Visibility::Sealed` deps are excluded entirely.
/// - No duplicates: the same physical package (by stripped digest) appears at
///   most once. Diamond deps keep the first-seen entry; a warning is emitted
///   when two paths reach the same repo with different digests.
/// - Roots last: root packages come after all their transitive visible deps.
///
/// Does **not** load extra metadata from disk — all needed data is already on
/// the `InstallInfo` fields (`metadata`, `resolved`, `content`). Transitive dep
/// metadata is loaded via `common::load_object_data` for deps not already in
/// the root's closure.
///
/// # Errors
///
/// Returns `Err` when a transitive dep's metadata or resolve.json cannot be
/// read from disk.
pub async fn import_visible_packages(
    objects: &PackageStore,
    roots: &[Arc<InstallInfo>],
) -> crate::Result<Vec<VisiblePackage>> {
    let mut seen_digests: HashSet<oci::Digest> = HashSet::new();
    let mut seen_repos: HashMap<oci::Repository, oci::Digest> = HashMap::new();
    let mut result: Vec<VisiblePackage> = Vec::new();

    for root in roots {
        // Build a (registry, repository) → resolved identifier map for content
        // path lookups. Metadata dep identifiers may carry image-index digests;
        // resolved identifiers carry the platform-manifest digest at which the
        // package is actually installed on disk.
        let resolved_id_map: HashMap<(String, String), &oci::PinnedIdentifier> = root
            .resolved
            .dependencies
            .iter()
            .map(|dep| {
                let key = (
                    dep.identifier.registry().to_string(),
                    dep.identifier.repository().to_string(),
                );
                (key, &dep.identifier)
            })
            .collect();

        // Visible transitive deps first (topological order preserved from
        // `ResolvedPackage::with_dependencies`). Root packages are direct exec
        // targets so both self-visible (Private, Public) and consumer-visible
        // (Public, Interface) deps contribute — i.e. everything except Sealed.
        for dep in &root.resolved.dependencies {
            if !dep.visibility.is_visible() {
                continue;
            }
            // check_exported returns false on digest-conflict (first-seen wins)
            // and false on already-seen. Both cases skip emission — same
            // semantics as the old resolve_env loop.
            if !check_exported(&dep.identifier, &mut seen_digests, &mut seen_repos)? {
                continue;
            }

            let content = objects.content(&dep.identifier);
            let (dep_metadata, dep_resolved) = common::load_object_data(objects, &content).await?;

            // Build dep's own direct-dep context map (scoped to the dep's
            // declared deps, not the root's). Each package's interpolation
            // surface is only its own direct deps.
            let dep_resolved_id_map: HashMap<(String, String), &oci::PinnedIdentifier> = dep_resolved
                .dependencies
                .iter()
                .map(|d| {
                    let key = (
                        d.identifier.registry().to_string(),
                        d.identifier.repository().to_string(),
                    );
                    (key, &d.identifier)
                })
                .collect();
            let dep_dep_contexts: HashMap<DependencyName, DependencyContext> = dep_metadata
                .dependencies()
                .iter()
                .map(|d| {
                    let name = d.name();
                    let key = (
                        d.identifier.registry().to_string(),
                        d.identifier.repository().to_string(),
                    );
                    // Falls back to the metadata identifier when the dep is
                    // not present in the resolved closure — e.g. when the dep
                    // was added to metadata after this dep's last pull. The
                    // env-time consumer only needs a stable install path, and
                    // `objects.content(metadata_id)` resolves the same content
                    // tree the resolver would have written.
                    let install_id = dep_resolved_id_map.get(&key).copied().unwrap_or(&d.identifier);
                    let install_path = objects.content(install_id);
                    (name, DependencyContext::path_only(install_id.clone(), install_path))
                })
                .collect();

            // Reconstruct an `InstallInfo` for the dep so `VisiblePackage`
            // can carry it uniformly. We need a stable identity here; use
            // the resolved identifier which has the platform-manifest digest.
            let dep_info = InstallInfo {
                identifier: dep.identifier.clone(),
                metadata: dep_metadata,
                resolved: dep_resolved,
                content,
            };

            result.push(VisiblePackage {
                install_info: Arc::new(dep_info),
                scope: ImportScope {
                    visibility: dep.visibility,
                    is_root: false,
                    dep_contexts: dep_dep_contexts,
                },
            });
        }

        // Then the root package itself, using root's direct dep contexts.
        // Build dep_contexts for this root: all direct deps (no visibility
        // filter). Visibility controls env propagation (is_visible() above);
        // interpolation scope includes all declared deps — these two filters
        // are independent.
        let root_dep_contexts: HashMap<DependencyName, DependencyContext> = root
            .metadata
            .dependencies()
            .iter()
            .map(|dep| {
                let name = dep.name();
                let key = (
                    dep.identifier.registry().to_string(),
                    dep.identifier.repository().to_string(),
                );
                // Falls back to the metadata identifier when the dep is not
                // in this root's resolved closure (same rationale as in the
                // dep-of-dep loop above): metadata may name a dep that the
                // last pull did not record, but `objects.content(metadata_id)`
                // still resolves to the correct content tree.
                let install_id = resolved_id_map.get(&key).copied().unwrap_or(&dep.identifier);
                let install_path = objects.content(install_id);
                (name, DependencyContext::path_only(install_id.clone(), install_path))
            })
            .collect();

        if check_exported(&root.identifier, &mut seen_digests, &mut seen_repos)? {
            result.push(VisiblePackage {
                install_info: Arc::clone(root),
                scope: ImportScope {
                    visibility: Visibility::Public, // root is always fully visible to itself
                    is_root: true,
                    dep_contexts: root_dep_contexts,
                },
            });
        }
    }

    Ok(result)
}

/// Iterates the visible packages in input order and emits resolved environment
/// variable entries for each one. Pure env emission — does **not** run the
/// closure-scoped entrypoint collision check.
///
/// For each [`VisiblePackage`] in `visible`:
/// 1. When `metadata.bundle_entrypoints()` is non-empty, emits a synthetic
///    `PATH ⊳ <pkg_root>/entrypoints` entry **before** the package's declared
///    env entries. Placing the synthetic entry first means that user-declared
///    `PATH ⊳ ${installPath}/bin` entries (emitted in step 2) prepend on top
///    and end up earlier in the resolved PATH, giving them higher lookup
///    priority. This prevents `ocx exec file://<pkg>` from finding the
///    entrypoint launcher for its own name and recursing infinitely.
///    Sealed deps are already absent from the input slice (Phase A gate), so
///    no sealed dep contributes a launcher PATH.
/// 2. Calls `tasks::common::export_env` with the package's metadata, content
///    path, and pre-built dep_contexts and appends the resulting entries.
///
/// Pair with [`collect_entrypoints`] when the caller wants fail-fast on
/// closure-scoped name collisions (the current consumption-time policy via
/// [`apply_visible_packages`]). Callers that want warn-not-fail behaviour
/// (e.g. `ocx env --json`, `ocx ci export`) can run the two passes
/// independently and surface the map without erroring.
///
/// The apply phase performs **no I/O**. All metadata and content paths are
/// already present on `install_info`. Emission order equals input order so
/// that callers relying on path-type precedence (last path prepended wins) get
/// stable, deterministic results.
///
/// # Errors
///
/// Template-resolution failures from `export_env` (see [`crate::package::metadata::env`]).
#[allow(clippy::result_large_err)]
pub fn emit_env(visible: &[VisiblePackage]) -> Result<Vec<Entry>, PackageErrorKind> {
    let mut entries = Vec::new();
    for pkg in visible {
        // Emit synthetic PATH entry for the entrypoints/ directory BEFORE
        // the package's declared env entries. Declared PATH entries (e.g.
        // `PATH ⊳ ${installPath}/bin`) are emitted next and prepend on top,
        // so `bin/` ends up earlier in PATH than `entrypoints/`. This ensures
        // `ocx exec file://<pkg>` resolves the actual binary (in `bin/`) and
        // not the launcher again, preventing infinite recursion.
        if pkg
            .install_info
            .metadata
            .bundle_entrypoints()
            .is_some_and(|eps| !eps.is_empty())
        {
            let entrypoints_dir = pkg.install_info.package_root().join("entrypoints");
            // Invariant: callers ensure the package root is UTF-8. `LauncherSafeString`
            // (entrypoints.rs) only screens forbidden characters, not UTF-8 validity, so
            // a non-UTF-8 `OCX_HOME` byte sequence on Unix would survive install but get
            // U+FFFD-replaced here and the resulting PATH entry would not resolve to the
            // launcher. Treated as out-of-scope until a real-world report surfaces.
            entries.push(Entry {
                key: "PATH".to_string(),
                value: entrypoints_dir.to_string_lossy().into_owned(),
                kind: ModifierKind::Path,
            });
        }

        common::export_env(
            &pkg.install_info.content,
            &pkg.install_info.metadata,
            &pkg.scope.dep_contexts,
            &mut entries,
        )
        .map_err(PackageErrorKind::Internal)?;
    }
    Ok(entries)
}

/// Phase B convenience: runs [`emit_env`] then [`collect_entrypoints`] and
/// returns both results together. Fails fast on closure-scoped name collisions.
///
/// Use [`emit_env`] / [`collect_entrypoints`] directly when you want
/// warn-not-fail behaviour (env-only consumers that only need the entrypoints
/// map for diagnostics).
///
/// # Errors
///
/// - Template-resolution failures from [`emit_env`].
/// - [`PackageErrorKind::EntrypointNameCollision`] when two packages in
///   `visible` declare the same entrypoint name.
#[allow(clippy::result_large_err)]
pub fn apply_visible_packages(
    visible: &[VisiblePackage],
) -> Result<(Vec<Entry>, BTreeMap<EntrypointName, oci::PinnedIdentifier>), PackageErrorKind> {
    let entries = emit_env(visible)?;
    let entrypoints_map = collect_entrypoints(visible)?;
    Ok((entries, entrypoints_map))
}

/// Builds a launcher-name → identifier map by scanning every visible package's
/// entrypoints in topological order. The first owner wins; a second package
/// claiming the same name returns `Err(PackageErrorKind::EntrypointNameCollision)`.
///
/// # Stage 1 caller
///
/// `pull.rs::setup_owned` — runs against the just-resolved closure to catch
/// intra-closure dupes at install time, before `resolve.json` is persisted.
/// A failure here aborts the install.
///
/// # Stage 2 caller
///
/// `apply_visible_packages` — runs as part of the per-call emission. A
/// collision here surfaces in the return value; the consuming command
/// (`ocx exec`, `ocx env`) decides whether to warn or fail.
///
/// # Errors
///
/// Returns [`PackageErrorKind::EntrypointNameCollision`] when two packages
/// claim the same entrypoint name.
#[allow(clippy::result_large_err)]
pub fn collect_entrypoints(
    visible: &[VisiblePackage],
) -> Result<BTreeMap<EntrypointName, oci::PinnedIdentifier>, PackageErrorKind> {
    let mut map: BTreeMap<EntrypointName, oci::PinnedIdentifier> = BTreeMap::new();
    for pkg in visible {
        if let Some(eps) = pkg.install_info.metadata.bundle_entrypoints() {
            for ep in eps.iter() {
                let name = ep.name.clone();
                if let Some(existing) = map.get(&name) {
                    return Err(PackageErrorKind::EntrypointNameCollision {
                        name,
                        first: existing.clone(),
                        second: pkg.install_info.identifier.clone(),
                    });
                }
                map.insert(name, pkg.install_info.identifier.clone());
            }
        }
    }
    Ok(map)
}

/// Deduplicates a visible dependency by digest, warning on same-repo digest
/// conflicts.
///
/// Returns `true` if the identifier was newly inserted (should be emitted),
/// `false` if already seen or if a conflict was detected and first-seen wins.
/// When the same `registry/repo` appears with different digests, a warning is
/// emitted and the first-seen digest is kept — matching the semantics of
/// scalar env vars (last-writer-wins would require a different ordering).
///
/// Moved here from `tasks/resolve.rs` as a private helper for Phase A. The
/// semantics are identical; the location is the only change.
fn check_exported(
    id: &oci::PinnedIdentifier,
    seen_digests: &mut HashSet<oci::Digest>,
    seen_repos: &mut HashMap<oci::Repository, oci::Digest>,
) -> crate::Result<bool> {
    let digest = id.digest();
    let repo_key = oci::Repository::from(&**id);
    if let Some(existing) = seen_repos.get(&repo_key)
        && *existing != digest
    {
        tracing::warn!(
            "Conflicting digests for {}: keeping {}, ignoring {}.",
            repo_key,
            existing,
            digest,
        );
        return Ok(false);
    }
    seen_repos.insert(repo_key, digest.clone());
    Ok(seen_digests.insert(digest))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        file_structure::{FileStructure, PackageStore},
        oci::{Digest, Identifier, PinnedIdentifier},
        package::{
            install_info::InstallInfo,
            metadata::{self, env::Env, visibility::Visibility},
            resolved_package::{ResolvedDependency, ResolvedPackage},
        },
    };

    use super::{apply_visible_packages, import_visible_packages};

    const REGISTRY: &str = "example.com";

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned(repo: &str, hex_char: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, REGISTRY).clone_with_digest(sha256(hex_char));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Builds a minimal `InstallInfo` with an empty env and the given resolved closure.
    fn make_install_info(repo: &str, hex_char: char, resolved: ResolvedPackage) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let metadata = metadata::Metadata::Bundle(metadata::bundle::Bundle {
            version: metadata::bundle::Version::V1,
            strip_components: None,
            env: Env::default(),
            dependencies: metadata::dependency::Dependencies::default(),
            entrypoints: metadata::entrypoint::Entrypoints::default(),
        });
        InstallInfo {
            identifier: id,
            metadata,
            resolved,
            content: std::path::PathBuf::from("/nonexistent"),
        }
    }

    /// Writes a minimal metadata.json and resolve.json under the `PackageStore`
    /// path for `id` so `load_object_data` can read them.
    fn seed_package_in_store(store: &PackageStore, id: &PinnedIdentifier, resolved: ResolvedPackage) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();
    }

    fn make_store(root: &std::path::Path) -> PackageStore {
        let fs = FileStructure::with_root(root.to_path_buf());
        fs.packages.clone()
    }

    /// `import_visible_packages` returns deps before dependents (topological
    /// order). For pkg A that depends on B which depends on C (all Public),
    /// the result slice must be [C, B, A] followed by the root.
    ///
    /// Layout (all Public): root → A → B → C (leaf)
    ///   resolve.json of root has [C(Public), B(Public), A(Public)] (topological)
    ///   `import_visible_packages` must emit C, B, A, then root.
    #[tokio::test]
    async fn import_visible_packages_returns_topological_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let c_id = pinned("c", 'c');
        let b_id = pinned("b", 'b');
        let a_id = pinned("a", 'a');

        seed_package_in_store(&store, &c_id, ResolvedPackage::new());
        seed_package_in_store(&store, &b_id, ResolvedPackage::new());
        seed_package_in_store(&store, &a_id, ResolvedPackage::new());

        // Root's resolve.json: deps before dependents as `with_dependencies` produces.
        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: c_id.clone(),
                    visibility: Visibility::Public,
                },
                ResolvedDependency {
                    identifier: b_id.clone(),
                    visibility: Visibility::Public,
                },
                ResolvedDependency {
                    identifier: a_id.clone(),
                    visibility: Visibility::Public,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let visible = import_visible_packages(&store, &[root]).await.unwrap();

        // 4 entries: C, B, A (deps in order), then root.
        assert_eq!(visible.len(), 4, "expected 4 visible packages: c, b, a, root");

        let repos: Vec<&str> = visible.iter().map(|v| v.install_info.identifier.repository()).collect();
        assert_eq!(
            repos,
            vec!["c", "b", "a", "root"],
            "topological order violated: {repos:?}"
        );

        // Root must be marked is_root; deps must not.
        assert!(!visible[0].scope.is_root, "c must not be is_root");
        assert!(!visible[1].scope.is_root, "b must not be is_root");
        assert!(!visible[2].scope.is_root, "a must not be is_root");
        assert!(visible[3].scope.is_root, "root must be is_root");
    }

    /// A sealed dep must not appear in the output of `import_visible_packages`.
    ///
    /// Root depends on B (Public) and C (Sealed). Only B should appear.
    #[tokio::test]
    async fn import_visible_packages_drops_sealed_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let b_id = pinned("b", 'b');
        let c_id = pinned("c", 'c');

        seed_package_in_store(&store, &b_id, ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: b_id.clone(),
                    visibility: Visibility::Public,
                },
                ResolvedDependency {
                    identifier: c_id.clone(),
                    visibility: Visibility::Sealed,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let visible = import_visible_packages(&store, &[root]).await.unwrap();

        // 2 entries: b (visible) and root. c (sealed) must be absent.
        assert_eq!(visible.len(), 2, "sealed dep must be excluded");
        let repos: Vec<&str> = visible.iter().map(|v| v.install_info.identifier.repository()).collect();
        assert!(repos.contains(&"b"), "b (Public) must be in visible set");
        assert!(!repos.contains(&"c"), "c (Sealed) must NOT be in visible set");
        assert!(repos.contains(&"root"), "root must be in visible set");
    }

    /// `apply_visible_packages` emits entries in the same order as the input
    /// slice. Given two packages with disjoint env vars, the output must
    /// interleave exactly as the input slice dictates.
    #[test]
    fn apply_visible_packages_emits_in_input_order() {
        let dir = tempfile::tempdir().unwrap();

        // Package A — exports FOO_A as a constant env var.
        // Use EnvBuilder so we don't depend on the private `Env::variables` field.
        let a_content = dir.path().join("a").join("content");
        std::fs::create_dir_all(&a_content).unwrap();
        let a_env = metadata::env::EnvBuilder::new()
            .with_constant("FOO_A", "value_a")
            .build();
        let a_meta = metadata::Metadata::Bundle(metadata::bundle::Bundle {
            version: metadata::bundle::Version::V1,
            strip_components: None,
            env: a_env,
            dependencies: metadata::dependency::Dependencies::default(),
            entrypoints: metadata::entrypoint::Entrypoints::default(),
        });
        let a_info = InstallInfo {
            identifier: pinned("a", 'a'),
            metadata: a_meta,
            resolved: ResolvedPackage::new(),
            content: a_content,
        };

        // Package B — exports FOO_B as a constant env var.
        let b_content = dir.path().join("b").join("content");
        std::fs::create_dir_all(&b_content).unwrap();
        let b_env = metadata::env::EnvBuilder::new()
            .with_constant("FOO_B", "value_b")
            .build();
        let b_meta = metadata::Metadata::Bundle(metadata::bundle::Bundle {
            version: metadata::bundle::Version::V1,
            strip_components: None,
            env: b_env,
            dependencies: metadata::dependency::Dependencies::default(),
            entrypoints: metadata::entrypoint::Entrypoints::default(),
        });
        let b_info = InstallInfo {
            identifier: pinned("b", 'b'),
            metadata: b_meta,
            resolved: ResolvedPackage::new(),
            content: b_content,
        };

        // Build visible slice [A, B].
        let visible = vec![
            super::VisiblePackage {
                install_info: Arc::new(a_info),
                scope: super::ImportScope {
                    visibility: Visibility::Public,
                    is_root: false,
                    dep_contexts: std::collections::HashMap::new(),
                },
            },
            super::VisiblePackage {
                install_info: Arc::new(b_info),
                scope: super::ImportScope {
                    visibility: Visibility::Public,
                    is_root: true,
                    dep_contexts: std::collections::HashMap::new(),
                },
            },
        ];

        let (entries, _entrypoints) = apply_visible_packages(&visible).unwrap();

        assert_eq!(entries.len(), 2, "expected 2 entries (one per package)");
        assert_eq!(entries[0].key, "FOO_A", "first entry must come from package A");
        assert_eq!(entries[1].key, "FOO_B", "second entry must come from package B");
    }

    // ── apply_visible_packages — synthetic PATH entry ─────────────────────────

    /// Builds a `VisiblePackage` with a real on-disk content directory and a
    /// single entrypoint. Needed for `apply_visible_packages` tests that assert
    /// on `package_root().join("entrypoints")`.
    fn make_visible_with_ep_and_content(
        dir: &std::path::Path,
        repo: &str,
        hex_char: char,
        ep_name: &str,
    ) -> super::VisiblePackage {
        use crate::package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};

        let id = pinned(repo, hex_char);
        let ep = Entrypoint {
            name: EntrypointName::try_from(ep_name).unwrap(),
            target: format!("${{installPath}}/bin/{ep_name}"),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();
        let metadata = metadata::Metadata::Bundle(metadata::bundle::Bundle {
            version: metadata::bundle::Version::V1,
            strip_components: None,
            env: Env::default(),
            dependencies: metadata::dependency::Dependencies::default(),
            entrypoints,
        });
        let content = dir.join(repo).join("content");
        std::fs::create_dir_all(&content).unwrap();
        let info = InstallInfo {
            identifier: id,
            metadata,
            resolved: ResolvedPackage::new(),
            content,
        };
        super::VisiblePackage {
            install_info: Arc::new(info),
            scope: super::ImportScope {
                visibility: Visibility::Public,
                is_root: false,
                dep_contexts: std::collections::HashMap::new(),
            },
        }
    }

    /// A visible package with non-empty entrypoints emits a synthetic
    /// `PATH ⊳ <pkg_root>/entrypoints` entry.
    #[test]
    fn apply_visible_packages_emits_synthetic_path_for_package_with_entrypoints() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = make_visible_with_ep_and_content(dir.path(), "cmake", 'a', "cmake");
        let pkg_root = pkg.install_info.package_root().to_path_buf();
        let visible = vec![pkg];

        let (entries, _) = apply_visible_packages(&visible).unwrap();

        let path_entries: Vec<_> = entries.iter().filter(|e| e.key == "PATH").collect();
        assert!(
            !path_entries.is_empty(),
            "at least one PATH entry must be emitted for package with entrypoints"
        );
        let expected_dir = pkg_root.join("entrypoints");
        assert!(
            path_entries
                .iter()
                .any(|e| e.value == expected_dir.to_string_lossy().as_ref()),
            "synthetic PATH value must be <pkg_root>/entrypoints; got {} entries with values: {:?}",
            path_entries.len(),
            path_entries.iter().map(|e| &e.value).collect::<Vec<_>>()
        );
    }

    /// The synthetic `PATH ⊳ <entrypoints>` entry must appear BEFORE the
    /// declared env entries in the output vector. Because `add_path` prepends,
    /// the declared `PATH ⊳ bin/` entry is processed last and ends up earlier
    /// in the resolved PATH, giving `bin/` higher lookup priority than
    /// `entrypoints/`. This prevents launcher recursion.
    #[test]
    fn apply_visible_packages_synthetic_path_before_declared_env() {
        use crate::package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};

        let dir = tempfile::tempdir().unwrap();
        let ep_name = "mytool";
        let content = dir.path().join("mytool").join("content");
        // Create the bin dir so the required PATH env var resolves without error.
        std::fs::create_dir_all(content.join("bin")).unwrap();

        let ep = Entrypoint {
            name: EntrypointName::try_from(ep_name).unwrap(),
            target: format!("${{installPath}}/bin/{ep_name}"),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();
        // Include a PATH env var so we can check relative position.
        let env = metadata::env::EnvBuilder::new()
            .with_path("PATH", "${installPath}/bin", true)
            .build();
        let metadata = metadata::Metadata::Bundle(metadata::bundle::Bundle {
            version: metadata::bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: metadata::dependency::Dependencies::default(),
            entrypoints,
        });
        let info = InstallInfo {
            identifier: pinned("mytool", 'm'),
            metadata,
            resolved: ResolvedPackage::new(),
            content: content.clone(),
        };
        let visible = vec![super::VisiblePackage {
            install_info: Arc::new(info),
            scope: super::ImportScope {
                visibility: Visibility::Public,
                is_root: true,
                dep_contexts: std::collections::HashMap::new(),
            },
        }];

        let (entries, _) = apply_visible_packages(&visible).unwrap();

        // Find positions of the synthetic entrypoints entry and the declared bin/ entry.
        let synthetic_pos = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .expect("synthetic entrypoints PATH entry must be present");
        let bin_pos = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("/bin"))
            .expect("declared PATH bin/ entry must be present");

        assert!(
            synthetic_pos < bin_pos,
            "synthetic entrypoints entry (pos {synthetic_pos}) must come BEFORE \
             declared bin/ entry (pos {bin_pos}) so bin/ wins after add_path prepends"
        );
    }

    /// A package without entrypoints must not emit any synthetic PATH entry.
    #[test]
    fn apply_visible_packages_no_synthetic_path_when_no_entrypoints() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("plain").join("content");
        std::fs::create_dir_all(&content).unwrap();
        let info = InstallInfo {
            identifier: pinned("plain", 'p'),
            metadata: metadata::Metadata::Bundle(metadata::bundle::Bundle {
                version: metadata::bundle::Version::V1,
                strip_components: None,
                env: Env::default(),
                dependencies: metadata::dependency::Dependencies::default(),
                entrypoints: metadata::entrypoint::Entrypoints::default(),
            }),
            resolved: ResolvedPackage::new(),
            content,
        };
        let visible = vec![super::VisiblePackage {
            install_info: Arc::new(info),
            scope: super::ImportScope {
                visibility: Visibility::Public,
                is_root: true,
                dep_contexts: std::collections::HashMap::new(),
            },
        }];

        let (entries, _) = apply_visible_packages(&visible).unwrap();
        assert!(
            entries.iter().all(|e| e.key != "PATH"),
            "package without entrypoints must not emit synthetic PATH entry"
        );
    }

    // ── collect_entrypoints ───────────────────────────────────────────────────

    /// Builds a `VisiblePackage` whose metadata declares a single entrypoint.
    fn make_visible_with_ep(repo: &str, hex_char: char, ep_name: &str) -> super::VisiblePackage {
        use crate::package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};

        let id = pinned(repo, hex_char);
        let ep = Entrypoint {
            name: EntrypointName::try_from(ep_name).unwrap(),
            target: format!("${{installPath}}/bin/{ep_name}"),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();
        let metadata = metadata::Metadata::Bundle(metadata::bundle::Bundle {
            version: metadata::bundle::Version::V1,
            strip_components: None,
            env: Env::default(),
            dependencies: metadata::dependency::Dependencies::default(),
            entrypoints,
        });
        let info = InstallInfo {
            identifier: id,
            metadata,
            resolved: ResolvedPackage::new(),
            content: std::path::PathBuf::from("/nonexistent"),
        };
        super::VisiblePackage {
            install_info: Arc::new(info),
            scope: super::ImportScope {
                visibility: Visibility::Public,
                is_root: false,
                dep_contexts: std::collections::HashMap::new(),
            },
        }
    }

    /// Two packages in the same closure claiming the same entrypoint name must
    /// produce `EntrypointNameCollision`. The `first` field carries the first
    /// owner's identifier and `second` carries the newcomer's identifier.
    #[test]
    fn collect_entrypoints_intra_closure_dupe_rejected() {
        use super::collect_entrypoints;
        use crate::package_manager::error::PackageErrorKind;

        let a = make_visible_with_ep("org/cmake", 'a', "cmake");
        let b = make_visible_with_ep("other/cmake", 'b', "cmake");
        let visible = vec![a, b];

        let err = collect_entrypoints(&visible).expect_err("duplicate name must collide");
        match err {
            PackageErrorKind::EntrypointNameCollision { name, first, second } => {
                assert_eq!(name.as_str(), "cmake");
                assert_eq!(first.repository(), "org/cmake");
                assert_eq!(second.repository(), "other/cmake");
            }
            other => panic!("expected EntrypointNameCollision, got {other:?}"),
        }
    }

    /// Two packages in disjoint closures (different entrypoint names) must
    /// succeed — first-owner-wins only applies within the same name.
    #[test]
    fn collect_entrypoints_distinct_names_accepted() {
        use super::collect_entrypoints;

        let a = make_visible_with_ep("org/cmake", 'a', "cmake");
        let b = make_visible_with_ep("org/ninja", 'b', "ninja");
        let visible = vec![a, b];

        let map = collect_entrypoints(&visible).expect("distinct names must not collide");
        assert_eq!(map.len(), 2, "both entrypoints must be in the map");
        assert!(map.keys().any(|k| k.as_str() == "cmake"), "cmake must be in map");
        assert!(map.keys().any(|k| k.as_str() == "ninja"), "ninja must be in map");
    }
}
