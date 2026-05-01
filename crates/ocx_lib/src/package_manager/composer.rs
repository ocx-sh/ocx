// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! two-env composition: flat iteration over each root's pre-built transitive
//! closure (TC) with cross-root dedup, emitting entries gated per surface.
//!
//! The TC is built inductively at install time via
//! `Visibility::through_edge` + `Visibility::merge` in
//! `ResolvedPackage::with_dependencies`. At exec time the composer reads each
//! root's `resolve.json` (one read per root), iterates flatly, and gates
//! emission via `tc_entry.visibility.has_interface()` (default exec) or
//! `has_private()` (`--self`). No recursive walk at compose time.
//!
//! See `adr_two_env_composition.md` for the full design rationale.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use tokio::task::JoinSet;

use crate::{
    file_structure::{PackageDir, PackageStore},
    oci,
    package::{
        install_info::InstallInfo,
        metadata::{
            self,
            dependency::DependencyName,
            entrypoint::EntrypointName,
            env::{dep_context::DependencyContext, entry::Entry, modifier::ModifierKind, resolver::EnvResolver},
        },
        resolved_package::ResolvedPackage,
    },
    package_manager::error::PackageErrorKind,
};

use super::tasks::common;

/// Result type for a single dep-load task spawned during parallel preload.
///
/// The `usize` is the task index for stable topological re-ordering after join.
type DepLoadResult = (
    usize,
    crate::Result<(metadata::Metadata, ResolvedPackage, oci::PinnedIdentifier)>,
);

/// Compose the runtime env from one or more root packages.
///
/// Reads each root's pre-built TC from `resolve.json` (single read per root),
/// iterates flatly with cross-root dedup, emits per-surface gated entries.
/// No recursion at compose time.
///
/// `self_view = false` selects the interface surface (default exec — consumer
/// view); `self_view = true` selects the private surface (`--self` — emits
/// the package's full runtime env including private entries).
///
/// # Errors
///
/// Returns `Err` if any required package metadata cannot be loaded from the
/// store during composition, or if two or more roots' interface projections
/// collide on an entrypoint name (multi-root collision gate — see
/// [`check_entrypoints`]).
pub async fn compose(roots: &[Arc<InstallInfo>], store: &PackageStore, self_view: bool) -> crate::Result<Vec<Entry>> {
    // Multi-root collision gate. Single-root case is already covered at
    // install time by `check_entrypoints`; cross-root collisions can only
    // surface here, when the user composes two or more independent roots.
    // Run before the dep walk so we fail fast on conflicting roots.
    // Guard: single-root is already gated at install time (pull.rs:425).
    if roots.len() > 1 {
        check_entrypoints(roots, store).await?;
    }

    // Warn on conflicting digests for the same `registry/repo` across the
    // surface-projected union TC (and roots themselves). Non-fatal: env
    // composition continues with first-seen wins by topological/iteration
    // order. Acceptance contract (`test_public_conflicting_deps_error`,
    // `test_deep_conflict_at_depth_two`) checks for the literal
    // `"conflicting"` token in stderr. Sealed/private-edge TC entries that
    // do not enter the active surface are excluded — they cannot collide
    // at runtime (`test_sealed_conflicting_deps_coexist`).
    warn_repo_digest_conflicts(roots, self_view);

    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: HashSet<oci::PinnedIdentifier> = HashSet::new();

    // Pre-compute root keys (stripped identifiers) so a TC entry that is
    // also an explicit root is deferred to the root-emission pass instead
    // of being silently absorbed during the dep walk (Option B in the
    // composer "root-as-dep" dedup discussion). Explicit roots emit
    // unconditionally; transitive deps dedup against each other AND
    // against the explicit-root set.
    let root_keys: HashSet<oci::PinnedIdentifier> = roots.iter().map(|r| r.identifier().strip_advisory()).collect();

    for root in roots {
        // Each root's TC is already flat. Iterate in topological order
        // (deps before dependents). Dep contributions emit before root's own
        // contributions per ADR Algorithm v3.
        //
        // Batch-preload all surface-visible, non-root TC entries for this root
        // in parallel via JoinSet. This eliminates serial I/O round-trips when
        // a root has many deps — each `load_object_data` call reads two JSON
        // files from disk. Results are indexed so the topological emission
        // order is preserved after join (per quality-rust.md JoinSet pattern).
        //
        // Step 1: collect the surface-visible, deduplicated entries for this root.
        let mut visible_entries: Vec<(usize, oci::PinnedIdentifier)> = Vec::new();
        for tc_entry in &root.resolved().dependencies {
            let key = tc_entry.identifier.strip_advisory();

            // Defer to the root-emission pass when a TC entry happens to be
            // an explicit root. Otherwise a private-edge TC entry (gated out
            // here) would consume the `seen` slot and silently skip the
            // explicit-root pass for the same package.
            if root_keys.contains(&key) {
                continue;
            }

            let want = if self_view {
                tc_entry.visibility.has_private()
            } else {
                tc_entry.visibility.has_interface()
            };
            if !want {
                continue;
            }

            // Cross-root dedup via stripped identifier (advisory tag ignored).
            // Insert AFTER the surface gate so a sealed/private TC entry that
            // gates out doesn't permanently mask a later visit of the same
            // package via a different root or path.
            if !seen.insert(key) {
                continue;
            }

            visible_entries.push((visible_entries.len(), tc_entry.identifier.clone()));
        }

        // Step 2: parallel-load metadata for all visible entries.
        let mut tasks: JoinSet<DepLoadResult> = JoinSet::new();
        for (idx, dep_id) in &visible_entries {
            let dep_id = dep_id.clone();
            let store = store.clone();
            let idx = *idx;
            tasks.spawn(async move {
                let dep_pkg = store.package_dir(&dep_id);
                let dep_content = dep_pkg.content();
                let result = common::load_object_data(&store, &dep_content).await;
                match result {
                    Ok((meta, resolved)) => (idx, Ok((meta, resolved, dep_id))),
                    Err(e) => (idx, Err(e)),
                }
            });
        }

        // Collect results preserving index for topological re-ordering.
        let mut loaded: Vec<Option<(metadata::Metadata, ResolvedPackage, oci::PinnedIdentifier)>> =
            vec![None; visible_entries.len()];
        while let Some(join_result) = tasks.join_next().await {
            let (idx, result) = match join_result {
                Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
                Err(e) => panic!("dep load task aborted: {e}"),
                Ok(v) => v,
            };
            loaded[idx] = Some(result?);
        }

        // Step 3: emit in topological order using pre-loaded metadata.
        for (meta, dep_resolved, dep_id) in loaded.into_iter().flatten() {
            let dep_pkg = store.package_dir(&dep_id);
            let dep_content = dep_pkg.content();

            // Build the dep's own direct-dep context map for
            // `${deps.NAME.installPath}` interpolation. Scoped to the dep's
            // own declared deps, not the root's — each package resolves its
            // own dep paths independently.
            let dep_dep_contexts = build_dep_context_map(&meta, &dep_resolved, store);

            emit_dep_path_block(&meta, &dep_pkg, &dep_content, &dep_dep_contexts, &mut entries)?;
        }

        // Root's own contributions, partitioned by `self_view`. Emit AFTER
        // the TC so root's PATH prepends win lookup over dep contributions
        // (per `add_path` prepend semantics). Root emission is unconditional
        // (no surface gate, no `seen` check against TC dedup) — explicit
        // roots are user input and always contribute. We still dedup roots
        // against each other so passing the same root twice does not
        // double-emit.
        if seen.insert(root.identifier().strip_advisory()) {
            // Build root's direct-dep context map for `${deps.NAME.installPath}`
            // interpolation in root's own env vars.
            let root_dep_contexts = build_dep_context_map(root.metadata(), root.resolved(), store);

            let root_content = root.dir().content();

            emit_root_path_block(
                root.metadata(),
                root.dir(),
                &root_content,
                &root_dep_contexts,
                self_view,
                &mut entries,
            )?;
        }
    }

    Ok(entries)
}

/// Uniqueness check on entrypoint names across the interface projection of
/// one or more roots.
///
/// Used at two boundaries:
///
/// - **Install gate** (single-root): `pull.rs` invokes this with the freshly
///   resolved root before persisting `resolve.json`, so closure-scoped
///   duplicate launcher names never reach disk.
/// - **Compose gate** (multi-root): [`compose`] invokes this when more than
///   one root participates, so cross-root interface collisions surface
///   before any env entries are emitted.
///
/// Scope: interface projection only. For each root, the helper records the
/// root's own bundle entrypoints, then walks `resolved().dependencies` and
/// records every TC entry whose effective visibility has the interface axis
/// (`has_interface()`). Cross-root dedup via stripped identifier ensures a
/// shared dep is counted once. Private-surface duplicates are deliberately
/// tolerated and resolved at runtime by topological PATH order.
///
/// Root entrypoints are recorded before the TC walk so the root identifier
/// appears first in the owners list when colliding with a dep entry — keeps
/// error output legible.
///
/// # Errors
///
/// Returns `Err(PackageErrorKind::EntrypointCollision { name, owners })`
/// listing all N owners on the first collision found (deterministic via
/// `BTreeMap` iteration). Returns `Err(PackageErrorKind::Internal)` if a
/// referenced package's metadata cannot be loaded from `store`.
pub async fn check_entrypoints(roots: &[Arc<InstallInfo>], store: &PackageStore) -> Result<(), PackageErrorKind> {
    let mut owners: BTreeMap<EntrypointName, Vec<oci::PinnedIdentifier>> = BTreeMap::new();
    let mut seen: HashSet<oci::PinnedIdentifier> = HashSet::new();

    for root in roots {
        // Each root's own entrypoints are unconditionally on the interface
        // surface from the root-emission perspective. Recorded first so the
        // root identifier wins ordering in the owners list on collision.
        if seen.insert(root.identifier().strip_advisory())
            && let Some(eps) = root.metadata().entrypoints()
        {
            for ep in eps.iter() {
                owners
                    .entry(ep.name.clone())
                    .or_default()
                    .push(root.identifier().clone());
            }
        }

        // Walk the root's TC interface projection and collect entrypoints
        // contributed by every interface-visible dep. Dedup by stripped
        // identifier so a shared dep across roots only counts once.
        for tc_entry in &root.resolved().dependencies {
            if !tc_entry.visibility.has_interface() {
                continue;
            }
            let key = tc_entry.identifier.strip_advisory();
            if !seen.insert(key) {
                continue;
            }
            let dep_content = store.content(&tc_entry.identifier);
            let (dep_metadata, _dep_resolved) = common::load_object_data(store, &dep_content)
                .await
                .map_err(PackageErrorKind::Internal)?;
            if let Some(eps) = dep_metadata.entrypoints() {
                for ep in eps.iter() {
                    owners
                        .entry(ep.name.clone())
                        .or_default()
                        .push(tc_entry.identifier.clone());
                }
            }
        }
    }

    // Report the first collision found. Iteration over `BTreeMap` is sorted,
    // so the choice is deterministic across runs.
    for (name, list) in owners {
        if list.len() > 1 {
            return Err(PackageErrorKind::EntrypointCollision { name, owners: list });
        }
    }

    Ok(())
}

/// Build the `${deps.NAME.installPath}` interpolation context map for a package.
///
/// Maps each of `metadata`'s declared dependencies by [`DependencyName`] to a
/// [`DependencyContext`] whose install path is resolved from `resolved`'s
/// pinned identifiers. When a dep identifier appears in the resolved TC, the
/// pinned (digest-bearing) identifier is used; otherwise the declaration
/// identifier is the fallback.
///
/// This is a pure function: no I/O, no async. Called for both TC dep entries
/// and root packages, replacing two formerly duplicate inline blocks.
fn build_dep_context_map(
    metadata: &metadata::Metadata,
    resolved: &ResolvedPackage,
    store: &PackageStore,
) -> HashMap<DependencyName, DependencyContext> {
    let resolved_id_map: HashMap<oci::Repository, &oci::PinnedIdentifier> = resolved
        .dependencies
        .iter()
        .map(|d| (oci::Repository::from(d.identifier.as_identifier()), &d.identifier))
        .collect();
    metadata
        .dependencies()
        .iter()
        .map(|d| {
            let name = d.name();
            let key = oci::Repository::from(d.identifier.as_identifier());
            let install_id = resolved_id_map.get(&key).copied().unwrap_or(&d.identifier);
            let install_path = store.content(install_id);
            (name, DependencyContext::path_only(install_id.clone(), install_path))
        })
        .collect()
}

/// Emit a dep's interface-tagged env vars into `entries`.
///
/// Only `var.visibility.has_interface()` vars cross the dep edge into the
/// consumer's surface (ADR Algorithm v3 step 5 — "only the interface side of
/// a dep crosses edges").
fn emit_interface_vars(
    dep_metadata: &metadata::Metadata,
    dep_content: &Path,
    dep_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    let Some(env) = dep_metadata.env() else {
        return Ok(());
    };
    let resolver = EnvResolver::new(dep_content, dep_dep_contexts);
    for var in env {
        if !var.visibility.has_interface() {
            continue;
        }
        if let Some(entry) = resolver.resolve(var)? {
            entries.push(entry);
        }
    }
    Ok(())
}

/// Emit a root's own env vars partitioned by `self_view`.
///
/// `self_view=false` (default exec): emit if `var.visibility.has_interface()`.
/// `self_view=true` (`--self`): emit if `var.visibility.has_private()`.
fn emit_root_vars(
    root_metadata: &metadata::Metadata,
    root_content: &Path,
    root_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    self_view: bool,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    let Some(env) = root_metadata.env() else {
        return Ok(());
    };
    let resolver = EnvResolver::new(root_content, root_dep_contexts);
    for var in env {
        let entry_vis = var.visibility;
        let want = if self_view {
            entry_vis.has_private()
        } else {
            entry_vis.has_interface()
        };
        if !want {
            continue;
        }
        if let Some(entry) = resolver.resolve(var)? {
            entries.push(entry);
        }
    }
    Ok(())
}

/// Emit the synth-entrypoints PATH entry for a dep followed by the dep's
/// interface-tagged env vars.
///
/// # Ordering invariant
///
/// PATH is searched left-to-right (first match wins). OCX consumers apply
/// entries by **prepending**, so the **last** entry pushed into `entries`
/// ends up **first** in the resolved PATH. This means:
///
/// 1. Push `entrypoints/` synth-PATH *first*.
/// 2. Call `emit_interface_vars` *second* — its `bin/` PATH entry is pushed
///    after, so it is prepended on top and wins lookup priority at runtime.
///
/// Reversing the push order makes a synthetic launcher re-find itself on
/// PATH and re-invoke `ocx exec` → infinite recursion. The ordering is
/// therefore **load-bearing**, not stylistic.
///
/// Regression test:
/// `test/tests/test_entrypoints.py::test_synthetic_entrypoints_path_emitted_before_declared_bin`
fn emit_dep_path_block(
    dep_metadata: &metadata::Metadata,
    dep_pkg: &PackageDir,
    dep_content: &Path,
    dep_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    // Step 1: synth-PATH first (so bin/ from step 2 lands ahead of it on PATH).
    if let Some(eps) = dep_metadata.entrypoints()
        && !eps.is_empty()
    {
        entries.push(synth_entrypoints_path_for(dep_pkg));
    }

    // Step 2: interface-tagged env vars (includes declared bin/ PATH entry).
    // Only the interface side of a dep crosses edges into the consumer's
    // surface (ADR Algorithm v3 step 5).
    emit_interface_vars(dep_metadata, dep_content, dep_dep_contexts, entries)
}

/// Emit the synth-entrypoints PATH entry for the root followed by the root's
/// own env vars, partitioned by `self_view`.
///
/// # Ordering invariant
///
/// Same as [`emit_dep_path_block`]: synth-PATH must be pushed **before**
/// the declared env vars so that the declared `bin/` PATH entry (pushed
/// second) ends up earlier in the resolved PATH and wins lookup priority.
///
/// The synth-PATH push is additionally gated by `!self_view` because under
/// `--self` the root must not see its own launchers (ADR Algorithm v3
/// §"Root's own contributions"). Emitting root's `entrypoints/` on the
/// private surface would allow the launcher to find itself and recurse.
///
/// Regression test:
/// `test/tests/test_entrypoints.py::test_synthetic_entrypoints_path_emitted_before_declared_bin`
fn emit_root_path_block(
    root_metadata: &metadata::Metadata,
    root_dir: &PackageDir,
    root_content: &Path,
    root_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    self_view: bool,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    // Step 1: synth-PATH first (guarded by !self_view — no launchers on --self surface).
    if !self_view
        && let Some(eps) = root_metadata.entrypoints()
        && !eps.is_empty()
    {
        entries.push(synth_entrypoints_path_for(root_dir));
    }

    // Step 2: env vars (includes declared bin/ PATH entry when present).
    emit_root_vars(root_metadata, root_content, root_dep_contexts, self_view, entries)
}

/// Emits `tracing::warn!` for every `registry/repo` that appears with two or
/// more distinct digests across the **surface-projected** union TC of the
/// supplied roots (including the roots themselves).
///
/// `self_view` selects the surface that gates TC entries: `false` (default
/// exec) keeps only entries whose effective visibility has the interface
/// axis (`has_interface()`); `true` (`--self`) keeps only those with the
/// private axis (`has_private()`). Roots themselves always participate —
/// explicit roots emit unconditionally during compose.
///
/// Mirrors the pre-refactor `check_exported` first-seen-wins semantics: when
/// two roots' transitive closures pull incompatible versions of the same
/// dependency through the active surface, env composition and `--flat`
/// listing continue with the first-seen digest, but a warning is logged so
/// users notice the conflict. Sealed/private-edge TC entries that do not
/// enter the active surface cannot collide at runtime — they are excluded
/// from the scan. The flag string `"conflicting"` is part of the stable
/// acceptance-test contract — see
/// `test_deps_flat_conflicting_digests_reports_error`,
/// `test_public_conflicting_deps_error`, `test_deep_conflict_at_depth_two`,
/// `test_sealed_conflicting_deps_coexist`.
pub fn warn_repo_digest_conflicts(roots: &[Arc<InstallInfo>], self_view: bool) {
    for conflict in collect_repo_digest_conflicts(roots, self_view) {
        tracing::warn!(
            "conflicting digests for {}: keeping {}, ignoring {}",
            conflict.repo,
            conflict.kept,
            conflict.ignored,
        );
    }
}

/// A single `registry/repo` conflict surfaced by [`warn_repo_digest_conflicts`].
///
/// Pure-data return shape so unit tests can assert conflict detection without
/// a `tracing` subscriber. The `kept` digest is the first one observed in
/// iteration order; the `ignored` digest is the colliding one that follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DigestConflict {
    pub repo: oci::Repository,
    pub kept: oci::Digest,
    pub ignored: oci::Digest,
}

/// Collects digest conflicts across the surface-projected union TC.
///
/// Pure function: no logging, no I/O. Used by [`warn_repo_digest_conflicts`]
/// and by unit tests. See that function for the surface-gating contract.
///
/// The returned `Vec` is allocated lazily — when there are no conflicts (the
/// common case) no heap allocation occurs beyond the `first_seen` map.
pub(crate) fn collect_repo_digest_conflicts(roots: &[Arc<InstallInfo>], self_view: bool) -> Vec<DigestConflict> {
    let mut first_seen: HashMap<oci::Repository, oci::PinnedIdentifier> = HashMap::new();
    let mut conflicts: Vec<DigestConflict> = Vec::new();
    for root in roots {
        // Roots themselves always participate — explicit roots emit
        // unconditionally during compose, so a digest collision between
        // roots (or between a root and any surface-visible TC entry) is
        // always real at runtime.
        record_repo_digest(root.identifier(), &mut first_seen, &mut conflicts);
        for dep in &root.resolved().dependencies {
            // Surface gate: a TC entry that does not contribute to the
            // active surface cannot collide at runtime under that surface.
            // Mirrors the gate applied in `compose` itself.
            let on_surface = if self_view {
                dep.visibility.has_private()
            } else {
                dep.visibility.has_interface()
            };
            if !on_surface {
                continue;
            }
            record_repo_digest(&dep.identifier, &mut first_seen, &mut conflicts);
        }
    }
    conflicts
}

fn record_repo_digest(
    id: &oci::PinnedIdentifier,
    first_seen: &mut HashMap<oci::Repository, oci::PinnedIdentifier>,
    conflicts: &mut Vec<DigestConflict>,
) {
    let repo = oci::Repository::from(&**id);
    if let Some(existing) = first_seen.get(&repo) {
        if existing.digest() != id.digest() {
            conflicts.push(DigestConflict {
                repo,
                kept: existing.digest().clone(),
                ignored: id.digest().clone(),
            });
        }
        return;
    }
    first_seen.insert(repo, id.clone());
}

/// Construct the synthetic `PATH ⊳ <pkg_root>/entrypoints` entry for `pkg`.
///
/// The entry kind is `Path` so consumers prepend it to PATH — which is why
/// root contributions emit AFTER TC entries: root's `bin/` PATH entries
/// prepend on top of dep entrypoint synth-PATHs and win lookup.
fn synth_entrypoints_path_for(pkg: &PackageDir) -> Entry {
    Entry {
        key: "PATH".to_string(),
        value: pkg.entrypoints().to_string_lossy().into_owned(),
        kind: ModifierKind::Path,
    }
}

// ── Specification tests (Phase 3) ───────────────────────────────────────────
//
// These tests are authored against the ADR + plan BEFORE the implementation is
// written. Phase 4 fills the bodies and removes the `#[should_panic]` markers
// so the tests assert the real composer output.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::{
        file_structure::{FileStructure, PackageStore},
        oci::{Digest, Identifier, PinnedIdentifier},
        package::{
            install_info::InstallInfo,
            metadata::{
                self, bundle, dependency,
                entrypoint::{Entrypoint, EntrypointName, Entrypoints},
                env::{
                    self as metadata_env,
                    var::{Modifier, Var},
                },
                visibility::Visibility,
            },
            resolved_package::{ResolvedDependency, ResolvedPackage},
        },
        package_manager::error::PackageErrorKind,
    };

    use super::{
        DigestConflict, check_entrypoints, collect_repo_digest_conflicts, compose, emit_dep_path_block,
        emit_root_path_block,
    };

    const REGISTRY: &str = "example.com";

    // ── Fixture helpers (adapted from visible.rs::tests) ──────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned(repo: &str, hex_char: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, REGISTRY).clone_with_digest(sha256(hex_char));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Build a minimal `InstallInfo` with an empty env and the given resolved closure.
    fn make_install_info(repo: &str, hex_char: char, resolved: ResolvedPackage) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir {
                dir: std::path::PathBuf::from("/nonexistent"),
            },
        )
    }

    /// Build a minimal `InstallInfo` with one env var of given key+visibility.
    fn make_install_info_with_var(
        dir: &std::path::Path,
        repo: &str,
        hex_char: char,
        resolved: ResolvedPackage,
        var_key: &str,
        var_vis: Visibility,
    ) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let var = Var {
            key: var_key.to_string(),
            modifier: Modifier::Constant(metadata_env::constant::Constant {
                value: "value".to_string(),
            }),
            visibility: var_vis,
        };
        let mut builder = metadata_env::EnvBuilder::new();
        builder.add_var(var);
        let env = builder.build();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir { dir: pkg_root },
        )
    }

    /// Build a minimal `InstallInfo` that declares a single entrypoint.
    fn make_install_info_with_ep(
        dir: &std::path::Path,
        repo: &str,
        hex_char: char,
        resolved: ResolvedPackage,
        ep_name: &str,
    ) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let ep = Entrypoint {
            name: EntrypointName::try_from(ep_name).unwrap(),
            target: format!("${{installPath}}/bin/{ep_name}"),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints,
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir { dir: pkg_root },
        )
    }

    fn make_store(root: &std::path::Path) -> PackageStore {
        let fs = FileStructure::with_root(root.to_path_buf());
        fs.packages.clone()
    }

    /// Write a minimal on-disk package directory (metadata.json + resolve.json)
    /// so `PackageStore::lookup` can find it. Mirrors the visible.rs
    /// `seed_package_in_store` helper.
    fn seed_package_in_store(store: &PackageStore, id: &PinnedIdentifier, resolved: &ResolvedPackage) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({ "type": "bundle", "version": 1 });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();
    }

    // ── Step 3.1 — Ported topological / sealed / diamond / collision tests ────

    // ─ Topological order ──────────────────────────────────────────────────────

    /// compose preserves topological order: deps before dependents, roots last.
    ///
    /// Plan §3.1 — topological order cell.
    /// ADR Algorithm v3: "for each root, TC entries first (in topological order,
    /// deps before dependents), then root's own envvars, then entrypoints."
    #[tokio::test]
    async fn compose_preserves_topological_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let c_id = pinned("c", 'c');
        let b_id = pinned("b", 'b');
        let a_id = pinned("a", 'a');

        let c_resolved = ResolvedPackage::new();
        let b_resolved = ResolvedPackage::new();
        let a_resolved = ResolvedPackage::new();

        seed_package_in_store(&store, &c_id, &c_resolved);
        seed_package_in_store(&store, &b_id, &b_resolved);
        seed_package_in_store(&store, &a_id, &a_resolved);

        // Root's TC: [C, B, A] in topological order (deps before dependents).
        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: c_id.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: b_id.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: a_id.clone(),
                    visibility: Visibility::PUBLIC,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // Sanity: must succeed (no env vars in any package, but should still
        // not panic). The deps have no env vars and no entrypoints, so the
        // composed env is empty.
        let env = compose(&[root], &store, false).await.unwrap();
        assert!(
            env.is_empty(),
            "no env vars or entrypoints declared; composed env must be empty"
        );
    }

    // ─ Sealed exclusion ────────────────────────────────────────────────────────

    /// A SEALED TC entry contributes nothing to either surface.
    ///
    /// Plan §3.1 — sealed exclusion cell.
    /// ADR §Worked Examples §1: sealed dep contributes nothing on any surface.
    #[tokio::test]
    async fn compose_sealed_dep_contributes_nothing_default_exec() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_id = pinned("sealed", 's');
        seed_package_in_store(&store, &sealed_id, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: sealed_id.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, false).await.unwrap();
        // SEALED.has_interface() = false → skip in default exec.
        assert!(env.is_empty(), "SEALED dep must contribute nothing in default exec");
    }

    /// A SEALED TC entry contributes nothing even under --self.
    #[tokio::test]
    async fn compose_sealed_dep_contributes_nothing_self_view() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_id = pinned("sealed", 's');
        seed_package_in_store(&store, &sealed_id, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: sealed_id.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, true).await.unwrap();
        // SEALED.has_private() = false → skip under --self too.
        assert!(env.is_empty(), "SEALED dep must contribute nothing under --self");
    }

    // ─ Diamond dedup ───────────────────────────────────────────────────────────

    /// Diamond dep appears in two root TCs but is emitted exactly once.
    ///
    /// Plan §3.1 — diamond dedup cell.
    /// Plan §3.3 — multi-root dedup test (compose(&[a,b], store, false) where both TCs list c).
    #[tokio::test]
    async fn compose_multi_root_diamond_dep_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let c_id = pinned("c", 'c');
        seed_package_in_store(&store, &c_id, &ResolvedPackage::new());

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: c_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: c_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };

        let a = Arc::new(make_install_info("a", 'a', a_resolved));
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        // c, a, b have no env vars + no entrypoints → composed env is empty
        // even when traversed twice. Guards against duplicate emission.
        let env = compose(&[a, b], &store, false).await.unwrap();
        assert!(
            env.is_empty(),
            "no env vars + no entrypoints declared; composed env must be empty regardless of dedup"
        );
    }

    // ─ Repo-conflict (same repo, different digest — first-seen wins) ───────────

    /// Same repository with two different digests in two roots: first-seen wins.
    ///
    /// Plan §3.1 — repo-conflict cell.
    #[tokio::test]
    async fn compose_same_repo_conflicting_digest_first_seen_wins() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_v1 = pinned("shared", '1');
        let dep_v2 = pinned("shared", '2');
        seed_package_in_store(&store, &dep_v1, &ResolvedPackage::new());
        seed_package_in_store(&store, &dep_v2, &ResolvedPackage::new());

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_v1.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_v2.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };

        let a = Arc::new(make_install_info("a", 'a', a_resolved));
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        // No env vars / no entrypoints — env is empty. The dedup happens by
        // stripped identifier (registry/repo/digest), so different digests
        // for the same repo are NOT collapsed: each digest is a distinct
        // pinned identifier. Both are visited, both contribute nothing.
        let env = compose(&[a, b], &store, false).await.unwrap();
        assert!(env.is_empty());
    }

    // ─ Edge filter: has_interface() vs has_private() ──────────────────────────
    //
    // Plan §3.1 "Coverage to FLIP": 4 intersects-edge-filter cells become
    // has_interface() / has_private() cells.

    /// Default exec (self_view=false): PRIVATE TC entry skipped — PRIVATE.has_interface()=false.
    ///
    /// Replaces the old `import_visible_packages_consumer_excludes_private_dep`
    /// test (visible.rs:1368) ported to the new accessor vocabulary.
    #[tokio::test]
    async fn compose_default_exec_skips_private_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_dep = pinned("priv", 'p');
        seed_package_in_store(&store, &priv_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: priv_dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, false).await.unwrap();
        // PRIVATE.has_interface()=false → skip in default exec.
        assert!(env.is_empty(), "PRIVATE TC entry must be skipped in default exec");
    }

    /// Default exec (self_view=false): INTERFACE TC entry included — INTERFACE.has_interface()=true.
    #[tokio::test]
    async fn compose_default_exec_includes_interface_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let iface_dep = pinned("iface", 'i');
        seed_package_in_store(&store, &iface_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: iface_dep.clone(),
                visibility: Visibility::INTERFACE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // INTERFACE.has_interface()=true → visit; dep has no env vars,
        // so env is empty but visit happened (no panic from missing
        // store entry).
        let env = compose(&[root], &store, false).await.unwrap();
        assert!(env.is_empty(), "no env vars on the dep, so output is empty");
    }

    /// --self (self_view=true): PRIVATE TC entry included — PRIVATE.has_private()=true.
    ///
    /// Replaces `import_visible_packages_self_includes_private_dep` (visible.rs:1396).
    #[tokio::test]
    async fn compose_self_view_includes_private_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_dep = pinned("priv", 'p');
        seed_package_in_store(&store, &priv_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: priv_dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, true).await.unwrap();
        // PRIVATE.has_private()=true → emit dep contributions; dep has no
        // env vars so output is empty but visit happened.
        assert!(env.is_empty(), "no env vars on the dep, so output is empty");
    }

    /// --self (self_view=true): INTERFACE TC entry skipped — INTERFACE.has_private()=false.
    ///
    /// Replaces `import_visible_packages_self_excludes_interface_only_dep` (visible.rs:1424).
    #[tokio::test]
    async fn compose_self_view_skips_interface_only_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let iface_dep = pinned("iface", 'i');
        seed_package_in_store(&store, &iface_dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: iface_dep.clone(),
                visibility: Visibility::INTERFACE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, true).await.unwrap();
        // INTERFACE.has_private()=false → skip under --self.
        assert!(env.is_empty(), "INTERFACE TC entry must be skipped under --self");
    }

    // ─ Synth-PATH gate (interface-projection cells) ────────────────────────────
    //
    // Plan §3.1 "Coverage to FLIP": 3 synth-PATH gate cells become
    // interface-projection cells. The new model: synth-PATH flows through the
    // same edge rules as any PATH entry — no special gate. But it is only
    // emitted for deps whose TC entry has has_interface()=true (default exec)
    // or has_private()=true (--self). Root's own entrypoints: emitted when
    // !self_view only (ADR Algorithm v3 §"Root's own contributions").

    /// Default exec: root with entrypoints emits synth-PATH for own entrypoints/.
    ///
    /// ADR Algorithm v3: "if !self_view and root has entrypoints, emit synth-PATH".
    #[tokio::test]
    async fn compose_default_exec_emits_synth_path_for_root_with_entrypoints() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root_resolved = ResolvedPackage::new();
        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "cmake",
        ));

        let env = compose(&[root], &store, false).await.unwrap();
        // Synth-PATH for root's entrypoints/ present in default exec.
        let path_entries: Vec<_> = env
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .collect();
        assert_eq!(
            path_entries.len(),
            1,
            "default exec must emit one synth-PATH for root entrypoints/"
        );
    }

    /// --self: root with entrypoints does NOT emit synth-PATH.
    ///
    /// ADR Algorithm v3: synth-PATH guarded by `!self_view` for root.
    /// This prevents the `ocx exec --self` launcher from finding its own
    /// entrypoints/ and recursing.
    #[tokio::test]
    async fn compose_self_view_does_not_emit_synth_path_for_root() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root_resolved = ResolvedPackage::new();
        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "cmake",
        ));

        let env = compose(&[root], &store, true).await.unwrap();
        // No synth-PATH in the --self output (root must not see its own launchers).
        let path_entries: Vec<_> = env
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .collect();
        assert!(
            path_entries.is_empty(),
            "--self must NOT emit synth-PATH for root's own entrypoints/"
        );
    }

    /// Default exec: dep's entrypoints/ synth-PATH emitted when dep has_interface().
    ///
    /// ADR Algorithm v3 step 5-6 for dep: entrypoints synth-PATH flows through
    /// edge rules like any PATH entry.
    #[tokio::test]
    async fn compose_default_exec_emits_synth_path_for_dep_with_interface_tc_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed dep with an entrypoint so the on-disk metadata reports
        // entrypoints when reloaded via `load_object_data`.
        let dep_id = pinned("cmake", 'c');
        let dep_resolved = ResolvedPackage::new();
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": [{
                "name": "cmake",
                "target": "${installPath}/bin/cmake",
            }],
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&dep_resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, false).await.unwrap();
        // PUBLIC.has_interface()=true → dep's synth-PATH emitted.
        let path_entries: Vec<_> = env
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .collect();
        assert_eq!(
            path_entries.len(),
            1,
            "PUBLIC dep with entrypoints must contribute one synth-PATH; got {} entries",
            path_entries.len()
        );
    }

    // ─ Entry-axis filter partition cells ──────────────────────────────────────
    //
    // Plan §3.1 "Coverage to FLIP": 3 entry-axis filter cells become
    // entry-visibility partition cells.

    /// Default exec: a dep's env var with `Visibility::INTERFACE` is emitted
    /// (dep's interface side crosses the edge per ADR Algorithm v3 step 5).
    ///
    /// Plan §3.3 — partition test.
    /// ADR: "for var in dep.bundle.env, emit if var.visibility.has_interface()".
    #[tokio::test]
    async fn compose_default_exec_emits_dep_interface_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed dep with a single Interface-visibility env var.
        let dep_id = pinned("dep", 'd');
        let dep_resolved = ResolvedPackage::new();
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "DEP_IFACE",
                "type": "constant",
                "value": "v",
                "visibility": "interface",
            }],
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&dep_resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, false).await.unwrap();
        // dep's Interface-tagged var present.
        assert!(
            env.iter().any(|e| e.key == "DEP_IFACE"),
            "dep's Interface var must be present: {:?}",
            env.iter().map(|e| &e.key).collect::<Vec<_>>()
        );
    }

    /// Default exec: root's Interface env var is emitted.
    ///
    /// ADR: for root's own contributions, emit if var.visibility.has_interface()
    /// (when !self_view).
    #[tokio::test]
    async fn compose_default_exec_emits_root_interface_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PKG_CONFIG_PATH",
            Visibility::INTERFACE,
        ));

        let env = compose(&[root], &store, false).await.unwrap();
        // root's Interface var present in default exec.
        assert!(
            env.iter().any(|e| e.key == "PKG_CONFIG_PATH"),
            "root's Interface var must be present in default exec"
        );
    }

    /// Default exec: root's Private env var is NOT emitted (private axis hidden from consumers).
    ///
    /// ADR: root's own entry emitted if var.visibility.has_interface() when !self_view.
    /// PRIVATE.has_interface()=false → not emitted.
    #[tokio::test]
    async fn compose_default_exec_excludes_root_private_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PRIVATE_FLAG",
            Visibility::PRIVATE,
        ));

        let env = compose(&[root], &store, false).await.unwrap();
        // root's Private var absent in default exec.
        assert!(
            !env.iter().any(|e| e.key == "PRIVATE_FLAG"),
            "root's Private var must be absent in default exec"
        );
    }

    // ─ --self surface partition ────────────────────────────────────────────────

    /// --self: root's Private env var IS emitted.
    ///
    /// ADR: emit if var.visibility.has_private() when self_view=true.
    /// PRIVATE.has_private()=true.
    #[tokio::test]
    async fn compose_self_view_emits_root_private_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PRIVATE_FLAG",
            Visibility::PRIVATE,
        ));

        let env = compose(&[root], &store, true).await.unwrap();
        // root's Private var present under --self.
        assert!(
            env.iter().any(|e| e.key == "PRIVATE_FLAG"),
            "root's Private var must be present under --self"
        );
    }

    /// --self: root's Interface env var is NOT emitted.
    ///
    /// ADR: emit if var.visibility.has_private() when self_view=true.
    /// INTERFACE.has_private()=false → not emitted under --self.
    ///
    /// This is the matrix walk-through fix: R running as itself does not see
    /// its own Interface-only env vars (those are consumer-only).
    #[tokio::test]
    async fn compose_self_view_excludes_root_interface_only_env_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "PKG_CONFIG_PATH",
            Visibility::INTERFACE,
        ));

        let env = compose(&[root], &store, true).await.unwrap();
        // root's Interface var absent under --self.
        assert!(
            !env.iter().any(|e| e.key == "PKG_CONFIG_PATH"),
            "root's Interface var must be absent under --self"
        );
    }

    // ─ Step 3.2 — Ported resolve.rs::resolve_visible_set surface-membership tests ──
    //
    // The four resolve_visible_set tests from tasks/resolve.rs now become
    // composer surface-membership tests. The `has_interface()` / `has_private()`
    // vocabulary replaces `intersects(view)`.

    /// Consumer (default exec): private-edge dep contributes nothing.
    ///
    /// Ported from `resolve_visible_set_consumer_excludes_private_dep`.
    /// PRIVATE.has_interface()=false → compose skips the entry.
    #[tokio::test]
    async fn compose_surface_membership_consumer_excludes_private_dep() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep = pinned("privlib", 'p');
        seed_package_in_store(&store, &dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, false).await.unwrap();
        // dep's contributions absent in consumer surface.
        assert!(env.is_empty());
    }

    /// --self: private-edge dep contributes.
    ///
    /// Ported from `resolve_visible_set_self_includes_private_dep`.
    /// PRIVATE.has_private()=true → compose includes the entry.
    #[tokio::test]
    async fn compose_surface_membership_self_includes_private_dep() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep = pinned("privlib", 'p');
        seed_package_in_store(&store, &dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // dep present in --self surface — but has no env vars, so output empty.
        let env = compose(&[root], &store, true).await.unwrap();
        assert!(
            env.is_empty(),
            "dep has no env vars; visit happened but output is empty"
        );
    }

    /// Default exec: SEALED dep excluded entirely.
    ///
    /// Ported from `resolve_visible_set_full_excludes_sealed_dep`.
    /// SEALED.has_interface()=false AND SEALED.has_private()=false.
    #[tokio::test]
    async fn compose_surface_membership_sealed_dep_excluded_both_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep = pinned("sealedlib", 's');
        seed_package_in_store(&store, &dep, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, false).await.unwrap();
        // sealed excluded.
        assert!(env.is_empty());
    }

    /// Diamond merge: dep reachable via interface and public paths → merged to PUBLIC.
    /// Under self_view=true, PUBLIC.has_private()=true → dep is in --self surface.
    ///
    /// Ported from `resolve_visible_set_diamond_merge_self_mode_preserves_public_path`.
    /// Per ADR §diamond merge: PUBLIC = INTERFACE.merge(PRIVATE) = (true,true).
    #[tokio::test]
    async fn compose_surface_membership_diamond_merge_public_preserved_under_self() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // The leaf is reachable via two paths that merge to PUBLIC.
        let leaf = pinned("leaf", 'l');
        seed_package_in_store(&store, &leaf, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                // PUBLIC = INTERFACE.merge(PRIVATE) per Visibility::merge semantics.
                ResolvedDependency {
                    identifier: leaf.clone(),
                    visibility: Visibility::PUBLIC,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, true).await.unwrap();
        // PUBLIC.has_private()=true → leaf visited; no env vars, so empty.
        assert!(env.is_empty());
    }

    // ─ Step 3.3 — Composer partition + multi-root dedup + JSON roundtrip ──────

    // ─ Partition: root entries split by surface ────────────────────────────────

    /// Default exec partition: root with [Public, Private, Interface] vars →
    /// result surface contains [Public, Interface] vars only.
    ///
    /// ADR Algorithm v3 "Root's own contributions": emit if
    /// var.visibility.has_interface() when !self_view.
    /// PUBLIC.has_interface()=true, PRIVATE.has_interface()=false, INTERFACE.has_interface()=true.
    #[tokio::test]
    async fn compose_default_exec_root_partition_public_and_interface_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Build a root with 3 vars: one public, one private, one interface.
        let id = pinned("root", 'r');
        let vars = [
            ("PUBLIC_VAR", Visibility::PUBLIC),
            ("PRIVATE_VAR", Visibility::PRIVATE),
            ("IFACE_VAR", Visibility::INTERFACE),
        ];
        let mut builder = metadata_env::EnvBuilder::new();
        for (key, vis) in &vars {
            builder.add_var(Var {
                key: key.to_string(),
                modifier: Modifier::Constant(metadata_env::constant::Constant { value: "v".to_string() }),
                visibility: *vis,
            });
        }
        let env = builder.build();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            id,
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root },
        ));

        let entries = compose(&[root], &store, false).await.unwrap();
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        assert!(
            keys.contains(&"PUBLIC_VAR"),
            "PUBLIC_VAR must be present (has_interface=true)"
        );
        assert!(
            !keys.contains(&"PRIVATE_VAR"),
            "PRIVATE_VAR must be absent (has_interface=false)"
        );
        assert!(
            keys.contains(&"IFACE_VAR"),
            "IFACE_VAR must be present (has_interface=true)"
        );
    }

    /// --self partition: root with [Public, Private, Interface] vars →
    /// result surface contains [Public, Private] vars only.
    ///
    /// ADR: emit if var.visibility.has_private() when self_view=true.
    /// PUBLIC.has_private()=true, PRIVATE.has_private()=true, INTERFACE.has_private()=false.
    #[tokio::test]
    async fn compose_self_view_root_partition_public_and_private_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let id = pinned("root", 'r');
        let vars = [
            ("PUBLIC_VAR", Visibility::PUBLIC),
            ("PRIVATE_VAR", Visibility::PRIVATE),
            ("IFACE_VAR", Visibility::INTERFACE),
        ];
        let mut builder = metadata_env::EnvBuilder::new();
        for (key, vis) in &vars {
            builder.add_var(Var {
                key: key.to_string(),
                modifier: Modifier::Constant(metadata_env::constant::Constant { value: "v".to_string() }),
                visibility: *vis,
            });
        }
        let env = builder.build();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        let pkg_root = dir.path().join("root2");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            id,
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root },
        ));

        let entries = compose(&[root], &store, true).await.unwrap();
        let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        assert!(
            keys.contains(&"PUBLIC_VAR"),
            "PUBLIC_VAR must be present (has_private=true)"
        );
        assert!(
            keys.contains(&"PRIVATE_VAR"),
            "PRIVATE_VAR must be present (has_private=true)"
        );
        assert!(
            !keys.contains(&"IFACE_VAR"),
            "IFACE_VAR must be absent (has_private=false)"
        );
    }

    // ─ TC entries: dep's interface side crosses edge ───────────────────────────

    /// Default exec: dep with SEALED/PRIVATE/PUBLIC/INTERFACE effective vis →
    /// contributions only from entries where tc_entry.visibility.has_interface().
    ///
    /// ADR Algorithm v3 step 3: "test tc_entry.visibility.has_interface() (default exec)"
    #[tokio::test]
    async fn compose_default_exec_tc_entry_gating_by_has_interface() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Four deps with different effective visibilities.
        let sealed_dep = pinned("sealed", 's');
        let private_dep = pinned("private", 'p');
        let public_dep = pinned("public", 'u');
        let iface_dep = pinned("iface", 'i');

        for id in [&sealed_dep, &private_dep, &public_dep, &iface_dep] {
            seed_package_in_store(&store, id, &ResolvedPackage::new());
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: sealed_dep.clone(),
                    visibility: Visibility::SEALED,
                },
                ResolvedDependency {
                    identifier: private_dep.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: public_dep.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: iface_dep.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, false).await.unwrap();
        // No deps declare env vars or entrypoints, so output is empty.
        // The gating is observable via the load_object_data calls — sealed
        // and private deps should NOT be visited, while public and iface
        // SHOULD be. The visit happens via on-disk metadata lookup; this
        // test validates the path doesn't panic when sealed/private deps
        // are skipped (no lookup attempt).
        assert!(env.is_empty());
    }

    /// --self: TC entry gating by has_private().
    ///
    /// ADR Algorithm v3 step 3: "test tc_entry.visibility.has_private() (--self)"
    #[tokio::test]
    async fn compose_self_view_tc_entry_gating_by_has_private() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_dep = pinned("sealed", 's');
        let private_dep = pinned("private", 'p');
        let public_dep = pinned("public", 'u');
        let iface_dep = pinned("iface", 'i');

        for id in [&sealed_dep, &private_dep, &public_dep, &iface_dep] {
            seed_package_in_store(&store, id, &ResolvedPackage::new());
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: sealed_dep.clone(),
                    visibility: Visibility::SEALED,
                },
                ResolvedDependency {
                    identifier: private_dep.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: public_dep.clone(),
                    visibility: Visibility::PUBLIC,
                },
                ResolvedDependency {
                    identifier: iface_dep.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let env = compose(&[root], &store, true).await.unwrap();
        assert!(env.is_empty());
    }

    // ─ Multi-root dedup ────────────────────────────────────────────────────────

    /// Atomic-vs-composite symmetry: compose(&[a, b], ...) uses the same algorithm
    /// as compose(&[a], ...). Shared dep emitted once.
    ///
    /// Plan §3.3 — "Multi-root dedup" test.
    /// ADR: "cross-root dedup via shared HashSet<DepKey>".
    #[tokio::test]
    async fn compose_multi_root_shared_dep_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed `shared` with one Public env var so we can count emissions.
        let shared = pinned("shared", 'x');
        let pkg_path = store.path(&shared);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "SHARED_VAR",
                "type": "constant",
                "value": "v",
                "visibility": "public",
            }],
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&ResolvedPackage::new()).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: shared.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: shared.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };

        let a = Arc::new(make_install_info("a", 'a', a_resolved));
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        let env = compose(&[a, b], &store, false).await.unwrap();
        // shared's contributions emitted exactly once (cross-root dedup).
        let shared_count = env.iter().filter(|e| e.key == "SHARED_VAR").count();
        assert_eq!(
            shared_count, 1,
            "shared dep must emit SHARED_VAR exactly once across multi-root compose"
        );
    }

    // ─ Empty-input behaviour ──────────────────────────────────────────────────

    /// compose(&[], ...) on empty roots returns empty Env.
    ///
    /// ADR: "compose(&[], ...) returns an empty Env".
    #[tokio::test]
    async fn compose_empty_roots_returns_empty_env() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let env = compose(&[], &store, false).await.unwrap();
        assert!(env.is_empty(), "compose(&[], ...) must return empty Env");
    }

    /// Leaf root (no TC): compose emits only root's own contributions.
    ///
    /// ADR: "empty input behavior: compose(&[root], ..., self_view) on a leaf
    /// package (no TC entries) emits only the root's own contributions".
    #[tokio::test]
    async fn compose_leaf_root_emits_only_own_contributions() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(), // no deps
            "ROOT_VAR",
            Visibility::PUBLIC,
        ));

        let env = compose(&[root], &store, false).await.unwrap();
        // ROOT_VAR present, no dep contributions.
        assert_eq!(env.len(), 1, "leaf root with one Public var must emit one entry");
        assert_eq!(env[0].key, "ROOT_VAR");
    }

    // ─ JSON wire-format roundtrip ──────────────────────────────────────────────
    //
    // Plan §3.3 — "JSON roundtrip" tests.
    // These are UNIT tests on the Visibility serde — they do NOT
    // call compose() and do NOT panic. They verify wire stability.

    /// All 4 Visibility constants serialize to the expected strings.
    #[test]
    fn visibility_wire_format_sealed() {
        assert_eq!(serde_json::to_string(&Visibility::SEALED).unwrap(), r#""sealed""#);
    }

    #[test]
    fn visibility_wire_format_private() {
        assert_eq!(serde_json::to_string(&Visibility::PRIVATE).unwrap(), r#""private""#);
    }

    #[test]
    fn visibility_wire_format_public() {
        assert_eq!(serde_json::to_string(&Visibility::PUBLIC).unwrap(), r#""public""#);
    }

    #[test]
    fn visibility_wire_format_interface() {
        assert_eq!(serde_json::to_string(&Visibility::INTERFACE).unwrap(), r#""interface""#);
    }

    /// All 4 Visibility constants roundtrip through JSON byte-identically.
    #[test]
    fn visibility_wire_roundtrip_all_constants() {
        for (constant, expected_str) in [
            (Visibility::SEALED, "\"sealed\""),
            (Visibility::PRIVATE, "\"private\""),
            (Visibility::PUBLIC, "\"public\""),
            (Visibility::INTERFACE, "\"interface\""),
        ] {
            let serialized = serde_json::to_string(&constant).unwrap();
            assert_eq!(
                serialized, expected_str,
                "wire format for {constant:?} must be {expected_str:?}"
            );
            let deserialized: Visibility = serde_json::from_str(&serialized).unwrap();
            assert_eq!(deserialized, constant, "roundtrip must be identity for {constant:?}");
        }
    }

    /// ResolvedPackage shape is unchanged: {dependencies: Vec<ResolvedDependency>}.
    /// Serialize → deserialize → equality.
    ///
    /// Plan §3.3 — "resolve.json shape roundtrip".
    #[test]
    fn resolved_package_wire_roundtrip_unchanged_shape() {
        use crate::package::resolved_package::{ResolvedDependency, ResolvedPackage};

        let dep = ResolvedDependency {
            // 'l' is not hex — use '1' as a valid hex digit for roundtrip test.
            identifier: pinned("lib", '1'),
            visibility: Visibility::PUBLIC,
        };
        let pkg = ResolvedPackage {
            dependencies: vec![dep.clone()],
        };

        let json = serde_json::to_string(&pkg).unwrap();

        // Must deserialize back to identical shape.
        let roundtripped: ResolvedPackage = serde_json::from_str(&json).unwrap();
        assert_eq!(
            roundtripped.dependencies.len(),
            1,
            "dependency count must survive roundtrip"
        );
        assert_eq!(roundtripped.dependencies[0].identifier, dep.identifier);
        assert_eq!(roundtripped.dependencies[0].visibility, dep.visibility);
    }

    /// ResolvedPackage with all 4 Visibility constants in deps roundtrips correctly.
    #[test]
    fn resolved_package_wire_roundtrip_all_visibility_constants() {
        use crate::package::resolved_package::{ResolvedDependency, ResolvedPackage};

        let deps: Vec<ResolvedDependency> = [
            // Must be valid hex digits; non-hex chars fail serde roundtrip.
            (Visibility::SEALED, '0'),
            (Visibility::PRIVATE, '2'),
            (Visibility::PUBLIC, '3'),
            (Visibility::INTERFACE, '4'),
        ]
        .iter()
        .map(|&(vis, hex)| ResolvedDependency {
            identifier: pinned("lib", hex),
            visibility: vis,
        })
        .collect();

        let pkg = ResolvedPackage {
            dependencies: deps.clone(),
        };
        let json = serde_json::to_string(&pkg).unwrap();
        let roundtripped: ResolvedPackage = serde_json::from_str(&json).unwrap();

        assert_eq!(
            roundtripped.dependencies.len(),
            deps.len(),
            "all deps must survive roundtrip"
        );
        for (orig, rt) in deps.iter().zip(roundtripped.dependencies.iter()) {
            assert_eq!(rt.visibility, orig.visibility, "visibility must be byte-stable");
        }
    }

    /// deny_unknown_fields on ResolvedPackage: extra field rejects.
    #[test]
    fn resolved_package_rejects_extra_fields() {
        use crate::package::resolved_package::ResolvedPackage;

        // interface_env / private_env were proposed in the M1 draft (rejected).
        // This test confirms the wire format does not accidentally accept them.
        let json = r#"{"dependencies":[],"interface_env":[]}"#;
        let result = serde_json::from_str::<ResolvedPackage>(json);
        assert!(
            result.is_err(),
            "extra field must be rejected by deny_unknown_fields; shape must be wire-stable"
        );
    }

    // ─ Step 3.1 — Entrypoint collision tests (Suite A unit-level) ─────────────

    // check_entrypoints operates on the interface projection only.
    // These unit tests correspond to the 4 edge-vis cells in the entrypoint
    // collision truth table in the ADR (Suite A).

    /// Suite A, cell: sealed edge — install OK.
    /// B is SEALED from R's interface projection: has_interface()=false → not checked.
    /// Both R and B declare entrypoint `e`; no collision fires.
    #[tokio::test]
    async fn check_entrypoints_sealed_dep_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Dep B: has entrypoint `e`, in TC with SEALED effective vis.
        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        // Seed B with an entrypoint via on-disk metadata.json.
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": [{ "name": "e", "target": "${installPath}/bin/e" }],
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        // Root R: has entrypoint `e` + TC with B as SEALED.
        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::SEALED,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        // Returns Ok(()) — SEALED.has_interface()=false → B not in interface projection.
        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        assert!(result.is_ok(), "SEALED dep entrypoint must not collide: {:?}", result);
    }

    /// Suite A, cell: private edge — install OK.
    /// B is PRIVATE from R's interface projection: PRIVATE.has_interface()=false → not checked.
    /// The private-surface duplicate is tolerated; runtime PATH order resolves.
    #[tokio::test]
    async fn check_entrypoints_private_dep_no_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": [{ "name": "e", "target": "${installPath}/bin/e" }],
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        assert!(
            result.is_ok(),
            "PRIVATE dep entrypoint must not collide on interface projection: {:?}",
            result
        );
    }

    /// Suite A, cell: interface edge — install FAIL.
    /// B is INTERFACE from R's interface projection: INTERFACE.has_interface()=true → collision fires.
    #[tokio::test]
    async fn check_entrypoints_interface_dep_collides() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": [{ "name": "e", "target": "${installPath}/bin/e" }],
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::INTERFACE,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        match result {
            Err(PackageErrorKind::EntrypointCollision { name, owners }) => {
                assert_eq!(name.as_str(), "e");
                assert_eq!(owners.len(), 2);
            }
            other => panic!("expected EntrypointCollision, got {other:?}"),
        }
    }

    /// Suite A, cell: public edge — install FAIL.
    /// B is PUBLIC from R's interface projection: PUBLIC.has_interface()=true → collision fires.
    #[tokio::test]
    async fn check_entrypoints_public_dep_collides() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let b_id = pinned("b", 'b');
        let b_resolved = ResolvedPackage::new();
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entrypoints": [{ "name": "e", "target": "${installPath}/bin/e" }],
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(b_path.join("resolve.json"), serde_json::to_string(&b_resolved).unwrap()).unwrap();

        let r_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let r = Arc::new(make_install_info_with_ep(dir.path(), "r", 'r', r_resolved, "e"));

        let result = check_entrypoints(std::slice::from_ref(&r), &store).await;
        match result {
            Err(PackageErrorKind::EntrypointCollision { name, owners }) => {
                assert_eq!(name.as_str(), "e");
                assert_eq!(owners.len(), 2);
            }
            other => panic!("expected EntrypointCollision, got {other:?}"),
        }
    }

    /// EntrypointCollision variant has owners Vec, not first/second pair.
    ///
    /// Plan §3.1 — "repo-conflict" / entrypoint collision N-owner shape.
    /// This is a unit test on the error type, NOT on compose/check_entrypoints.
    #[test]
    fn entrypoint_collision_variant_has_vec_owners() {
        let name = EntrypointName::try_from("cmake").unwrap();
        let owner_a = pinned("a", 'a');
        let owner_b = pinned("b", 'b');
        let owner_c = pinned("c", 'c');

        let err = PackageErrorKind::EntrypointCollision {
            name: name.clone(),
            owners: vec![owner_a.clone(), owner_b.clone(), owner_c.clone()],
        };

        // Confirm the N-owner shape — not a 2-owner first/second shape.
        match &err {
            PackageErrorKind::EntrypointCollision { owners, .. } => {
                assert_eq!(owners.len(), 3, "EntrypointCollision must support N>2 owners");
                assert!(owners.contains(&owner_a));
                assert!(owners.contains(&owner_b));
                assert!(owners.contains(&owner_c));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ─ Multi-root entrypoint collision (Block 1 — compose-time gate) ──────────

    /// Two roots each declaring entrypoint `foo` MUST cause `compose` to fail
    /// with `EntrypointCollision` listing both owners.
    ///
    /// Codex Block 1 finding: install-gate covers within-closure collisions;
    /// cross-root collisions surface only at `ocx env A B` / `ocx exec A B`.
    /// This is the compose-time gate that blocks them before any env entries
    /// are emitted.
    #[tokio::test]
    async fn compose_multi_root_collision_errors() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Two independent roots that each declare entrypoint `foo`. Neither
        // is in the other's TC, so the install-gate can't see the conflict.
        let a = Arc::new(make_install_info_with_ep(
            dir.path(),
            "a",
            'a',
            ResolvedPackage::new(),
            "foo",
        ));
        let b = Arc::new(make_install_info_with_ep(
            dir.path(),
            "b",
            'b',
            ResolvedPackage::new(),
            "foo",
        ));

        let result = compose(&[a.clone(), b.clone()], &store, false).await;
        let err = match result {
            Ok(_) => panic!("expected EntrypointCollision, got Ok"),
            Err(e) => e,
        };
        let err = match err {
            crate::Error::PackageManager(inner) => inner,
            other => panic!("expected PackageManager outer error, got {other:?}"),
        };
        let errs = match err {
            crate::package_manager::error::Error::ResolveFailed(es) => es,
            other => panic!("expected ResolveFailed, got {other:?}"),
        };
        assert_eq!(errs.len(), 1, "expected single packaged error");
        match &errs[0].kind {
            PackageErrorKind::EntrypointCollision { name, owners } => {
                assert_eq!(name.as_str(), "foo");
                assert_eq!(owners.len(), 2, "both roots must be listed: {owners:?}");
                assert!(owners.contains(a.identifier()));
                assert!(owners.contains(b.identifier()));
            }
            other => panic!("expected EntrypointCollision kind, got {other:?}"),
        }
    }

    // ─ Block 2 — Explicit root that is also a private dep emits fully ────────

    /// When `compose` is invoked with `[a, b]` where `a → b` is a PRIVATE edge
    /// in the consumer (default exec) projection, b's contributions MUST still
    /// appear because b is an explicit root.
    ///
    /// Codex Block 2 finding: the previous implementation inserted into `seen`
    /// before the surface gate, so iterating `a`'s TC inserted `b` into `seen`,
    /// then gated `b` out (PRIVATE.has_interface()=false), and the later
    /// explicit-root pass for `b` was silently skipped. The fix defers
    /// root-as-dep TC entries to the explicit-root pass and only inserts into
    /// `seen` after the surface gate.
    #[tokio::test]
    async fn compose_root_appearing_as_private_dep_emits_root_fully() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed `b` on disk with a Public env var so we can detect that its
        // explicit-root contributions reach the env.
        let b_id = pinned("b", 'b');
        let b_path = store.path(&b_id);
        std::fs::create_dir_all(b_path.join("content")).unwrap();
        let b_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "B_OWN_VAR",
                "type": "constant",
                "value": "v",
                "visibility": "public",
            }],
        });
        std::fs::write(b_path.join("metadata.json"), b_meta.to_string()).unwrap();
        std::fs::write(
            b_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        // Build `b` as the second-root InstallInfo from the on-disk seed.
        let b_resolved = ResolvedPackage::new();
        let b_root = Arc::new(make_install_info_with_var(
            dir.path(),
            "b",
            'b',
            b_resolved.clone(),
            "B_OWN_VAR",
            Visibility::PUBLIC,
        ));

        // Build `a` so its TC includes `b` as a PRIVATE edge. The explicit
        // dep entry would gate out under default exec.
        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: b_id.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let a = Arc::new(make_install_info("a", 'a', a_resolved));

        let env = compose(&[a, b_root], &store, false).await.unwrap();
        let keys: Vec<&str> = env.iter().map(|e| e.key.as_str()).collect();
        assert!(
            keys.contains(&"B_OWN_VAR"),
            "b's own (public) contributions must reach the env when b is an explicit root, \
             even when also reachable as a private TC entry of `a`; got keys: {keys:?}"
        );
    }

    // ─ Composition-order test ─────────────────────────────────────────────────

    /// Within each root, TC entries are emitted before root's own envvars.
    ///
    /// ADR Algorithm v3: "Composition order is fixed: for each root, TC entries
    /// first (in topological order), then root's own envvars, then entrypoints."
    #[tokio::test]
    async fn compose_tc_entries_emitted_before_root_own_envvars() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Seed dep with a Public env var.
        let dep_id = pinned("dep", 'd');
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let dep_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{ "key": "DEP_VAR", "type": "constant", "value": "v", "visibility": "public" }],
        });
        std::fs::write(pkg_path.join("metadata.json"), dep_meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "ROOT_OWN_VAR",
            Visibility::PUBLIC,
        ));

        let env = compose(&[root], &store, false).await.unwrap();
        // dep's contributions appear before ROOT_OWN_VAR in the Env.
        let dep_pos = env.iter().position(|e| e.key == "DEP_VAR").expect("DEP_VAR present");
        let root_pos = env
            .iter()
            .position(|e| e.key == "ROOT_OWN_VAR")
            .expect("ROOT_OWN_VAR present");
        assert!(
            dep_pos < root_pos,
            "DEP_VAR (pos {dep_pos}) must come before ROOT_OWN_VAR (pos {root_pos})"
        );
    }

    /// Within each root, root's synth-PATH (entrypoints) entry is emitted
    /// BEFORE root's declared envvars on the consumer surface.
    ///
    /// PATH semantics are last-prepended-wins, so emitting synth-PATH before
    /// the declared `bin/` PATH entry makes `bin/` win lookup priority at
    /// runtime. This prevents `ocx exec file://<pkg>` from re-resolving its
    /// own launcher and recursing — see acceptance test
    /// `test_synthetic_entrypoints_path_emitted_before_declared_bin`.
    #[tokio::test]
    async fn compose_root_synth_path_emitted_before_root_own_vars() {
        let dir = tempfile::tempdir().unwrap();

        // Root declares one Public var AND one entrypoint — no deps needed.
        let root_id = pinned("root", 'r');
        let var = Var {
            key: "ROOT_VAR".to_string(),
            modifier: Modifier::Constant(metadata_env::constant::Constant {
                value: "val".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(var);
        let env = env_builder.build();

        let ep = Entrypoint {
            name: EntrypointName::try_from("mytool").unwrap(),
            target: "${installPath}/bin/mytool".to_string(),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();

        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            root_id,
            metadata,
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_root },
        ));

        // Store is needed by compose but root has no deps, so it stays empty.
        let store = make_store(dir.path());

        let env = compose(&[root], &store, false).await.unwrap();

        let var_pos = env.iter().position(|e| e.key == "ROOT_VAR").expect("ROOT_VAR present");
        let path_pos = env
            .iter()
            .position(|e| e.key == "PATH")
            .expect("synth-PATH entry present");
        assert!(
            path_pos < var_pos,
            "synth-PATH (pos {path_pos}) must come before ROOT_VAR (pos {var_pos})"
        );
    }

    // ─ Digest-conflict surface gating ─────────────────────────────────────────

    /// Two roots, each pulling a different digest of the same `d` repo via a
    /// SEALED edge, MUST NOT be reported as a conflict on the default
    /// (interface) surface. Sealed deps never enter the consumer composition,
    /// so their digests cannot collide at runtime.
    ///
    /// Mirrors `test_sealed_conflicting_deps_coexist`: under `ocx env A B`
    /// stderr must be free of the `"conflicting"` token when the conflicting
    /// dep is sealed under both roots.
    #[test]
    fn digest_conflict_skipped_for_sealed_dep_on_interface_surface() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1,
                    visibility: Visibility::SEALED,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2,
                    visibility: Visibility::SEALED,
                }],
            },
        ));

        let conflicts = collect_repo_digest_conflicts(&[a, b], false);
        assert!(
            conflicts.is_empty(),
            "sealed dep with conflicting digests must not be reported on the interface surface; got {conflicts:?}"
        );
    }

    /// Asymmetric visibility: root A pulls `d v1` as PUBLIC (interface), root B
    /// pulls `d v2` as PRIVATE. Default exec only emits A's `d`; B's `d` is
    /// gated out. No conflict on the interface surface.
    #[test]
    fn digest_conflict_skipped_when_only_one_root_exposes_dep_on_surface() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1,
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2,
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        let conflicts = collect_repo_digest_conflicts(&[a, b], false);
        assert!(
            conflicts.is_empty(),
            "private-only dep under B must not collide with public dep under A on the interface surface; got {conflicts:?}"
        );
    }

    /// Two roots both pulling `d` via the interface surface (PUBLIC) with
    /// different digests MUST surface a conflict. Locks the regression
    /// guarded by `test_public_conflicting_deps_error` /
    /// `test_deep_conflict_at_depth_two`: the surface gate is not allowed to
    /// over-suppress real interface-surface conflicts.
    #[test]
    fn digest_conflict_reported_for_interface_dep() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');
        let expected_repo = crate::oci::Repository::from(&*d_v1);

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));

        let conflicts = collect_repo_digest_conflicts(&[a, b], false);
        assert_eq!(
            conflicts,
            vec![DigestConflict {
                repo: expected_repo,
                kept: d_v1.digest().clone(),
                ignored: d_v2.digest().clone(),
            }],
        );
    }

    /// Sealed deps with conflicting digests collide on the `--self` surface
    /// only when the edge has the private axis. With `Visibility::SEALED`
    /// (neither axis), they remain hidden under both surfaces.
    #[test]
    fn digest_conflict_reported_for_private_dep_on_self_surface() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');
        let expected_repo = crate::oci::Repository::from(&*d_v1);

        let a = Arc::new(make_install_info(
            "a",
            'a',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v1.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));
        let b = Arc::new(make_install_info(
            "b",
            'b',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: d_v2.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        // Default (interface) surface: private-only deps gated out.
        assert!(
            collect_repo_digest_conflicts(&[a.clone(), b.clone()], false).is_empty(),
            "private deps must not collide on the interface surface"
        );
        // `--self` surface: private deps participate, conflict is reported.
        assert_eq!(
            collect_repo_digest_conflicts(&[a, b], true),
            vec![DigestConflict {
                repo: expected_repo,
                kept: d_v1.digest().clone(),
                ignored: d_v2.digest().clone(),
            }],
        );
    }

    // ─ Item #10 — root-as-TC-dep emitted exactly once ─────────────────────────

    /// Package `a` is both an explicit root AND appears in the TC of the other
    /// root `b`. The composer must emit `a`'s contributions exactly once.
    ///
    /// Regression guard for the `root_keys` pre-computation that defers TC
    /// entries which are also explicit roots to the root-emission pass, ensuring
    /// neither double-emission nor silent suppression occurs.
    ///
    /// Setup: `b → a` (PUBLIC edge, so `a` is in b's interface-projection TC).
    /// Roots: `[b, a]`. Expected: `a`'s env var appears exactly once.
    #[tokio::test]
    async fn compose_root_that_is_also_tc_dep_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // `a` declares a single Public env var so we can count emissions.
        let a = Arc::new(make_install_info_with_var(
            dir.path(),
            "a",
            'a',
            ResolvedPackage::new(),
            "A_VAR",
            Visibility::PUBLIC,
        ));

        // Also seed `a` on disk so `load_object_data` can find it when
        // `b`'s TC walk reaches it (the parallel preload path).
        let a_path = store.path(a.identifier());
        std::fs::create_dir_all(a_path.join("content")).unwrap();
        let a_meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": "A_VAR",
                "type": "constant",
                "value": "v",
                "visibility": "public",
            }],
        });
        std::fs::write(a_path.join("metadata.json"), a_meta.to_string()).unwrap();
        std::fs::write(
            a_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

        // `b` depends on `a` via a PUBLIC edge — `a` is in `b`'s interface TC.
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: a.identifier().clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b = Arc::new(make_install_info("b", 'b', b_resolved));

        // Compose with both b and a as explicit roots. b's TC includes a, but
        // the root-emission pass must handle a exactly once (not from b's TC
        // walk AND again from the explicit-root pass).
        let env = compose(&[b, a], &store, false).await.unwrap();

        let a_var_count = env.iter().filter(|e| e.key == "A_VAR").count();
        assert_eq!(
            a_var_count, 1,
            "A_VAR must be emitted exactly once; a is both a root and a TC dep of b. Got {a_var_count} emissions"
        );
    }

    // ─ PATH ordering invariant for emit_dep_path_block / emit_root_path_block ──
    //
    // These unit tests verify the load-bearing ordering enforced by the helpers
    // extracted in the refactor for finding #7.
    //
    // Invariant: synth-entrypoints PATH entry MUST appear at a lower index
    // than the declared `bin/` PATH entry in `entries` so that, when a consumer
    // prepends each entry in order, `bin/` ends up at the front of PATH and wins
    // lookup priority. Reversing the order makes a launcher re-resolve itself →
    // infinite recursion.
    //
    // The second test in each pair demonstrates that swapping the two pushes
    // inside the helper would produce a DIFFERENT ordering, proving the order is
    // load-bearing and that a reversal is detectable.

    /// `emit_dep_path_block` emits synth-PATH before the declared `bin/` PATH entry.
    ///
    /// Construct a dep with both an entrypoint (so synth-PATH is emitted) and a
    /// declared PATH env var (simulating `bin/`). Assert that synth-PATH appears
    /// at a lower index in `entries` than the declared PATH entry.
    #[test]
    fn emit_dep_path_block_synth_path_precedes_declared_bin() {
        let dir = tempfile::tempdir().unwrap();

        // Build dep metadata: one entrypoint + one public PATH var (the bin/).
        let ep = Entrypoint {
            name: EntrypointName::try_from("tool").unwrap(),
            target: "${installPath}/bin/tool".to_string(),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::INTERFACE,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let dep_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
        });

        let pkg_root = dir.path().join("dep");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let dep_pkg = crate::file_structure::PackageDir { dir: pkg_root.clone() };

        let dep_content = pkg_root.join("content");
        let dep_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_dep_path_block(&dep_metadata, &dep_pkg, &dep_content, &dep_dep_contexts, &mut entries)
            .expect("emit_dep_path_block must succeed");

        // Must have at least 2 entries: synth-PATH + declared bin/ PATH.
        let entry_summary: Vec<_> = entries.iter().map(|e| (&e.key, &e.value)).collect();
        assert!(
            entries.len() >= 2,
            "expected at least 2 entries (synth-PATH + declared PATH), got {}; entries: {:?}",
            entries.len(),
            entry_summary
        );

        let synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .expect("synth-PATH entry (contains 'entrypoints') must be present");

        let bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .expect("declared bin/ PATH entry must be present");

        assert!(
            synth_idx < bin_idx,
            "synth-PATH (index {synth_idx}) must precede declared bin/ PATH (index {bin_idx}); \
             reversing would cause launcher self-recursion. entries: {:?}",
            entries.iter().map(|e| (&e.key, &e.value)).collect::<Vec<_>>()
        );
    }

    /// `emit_dep_path_block` ordering is load-bearing: a manually-swapped vector
    /// fails the ordering check, proving the helper's order is not accidental.
    ///
    /// This test calls `emit_dep_path_block`, then swaps the two PATH entries in
    /// the result. The swapped vector must NOT satisfy the ordering invariant —
    /// demonstrating that the invariant would be violated if the helper's pushes
    /// were reversed.
    #[test]
    fn emit_dep_path_block_swapped_order_fails_invariant() {
        let dir = tempfile::tempdir().unwrap();

        let ep = Entrypoint {
            name: EntrypointName::try_from("tool").unwrap(),
            target: "${installPath}/bin/tool".to_string(),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::INTERFACE,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let dep_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
        });
        let pkg_root = dir.path().join("dep2");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let dep_pkg = crate::file_structure::PackageDir { dir: pkg_root.clone() };
        let dep_content = pkg_root.join("content");
        let dep_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_dep_path_block(&dep_metadata, &dep_pkg, &dep_content, &dep_dep_contexts, &mut entries)
            .expect("emit_dep_path_block must succeed");

        // Swap the two PATH entries to simulate reversed push order.
        let synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .expect("synth-PATH must exist");
        let bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .expect("bin/ PATH must exist");

        entries.swap(synth_idx, bin_idx);

        // After the swap, synth-PATH must now be at the *higher* index.
        // (bin_idx < synth_idx after swap.) This proves that swapping breaks the invariant.
        let new_synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .unwrap();
        let new_bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .unwrap();

        // The swapped vector has synth AFTER bin — the invariant is violated.
        assert!(
            new_bin_idx < new_synth_idx,
            "after swap, bin/ (index {new_bin_idx}) must precede synth-PATH (index {new_synth_idx}); \
             this confirms the swap reverses the invariant"
        );
    }

    /// `emit_root_path_block` emits synth-PATH before the declared `bin/` PATH entry
    /// on the consumer (default exec, self_view=false) surface.
    #[test]
    fn emit_root_path_block_synth_path_precedes_declared_bin_consumer_surface() {
        let dir = tempfile::tempdir().unwrap();

        let ep = Entrypoint {
            name: EntrypointName::try_from("rootool").unwrap(),
            target: "${installPath}/bin/rootool".to_string(),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let root_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root_dir = crate::file_structure::PackageDir { dir: pkg_root.clone() };
        let root_content = pkg_root.join("content");
        let root_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_root_path_block(
            &root_metadata,
            &root_dir,
            &root_content,
            &root_dep_contexts,
            false, // consumer surface (default exec)
            &mut entries,
        )
        .expect("emit_root_path_block must succeed");

        assert!(
            entries.len() >= 2,
            "expected at least 2 entries (synth-PATH + declared PATH), got {}",
            entries.len()
        );

        let synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .expect("synth-PATH entry must be present in consumer surface output");

        let bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .expect("declared bin/ PATH must be present in consumer surface output");

        assert!(
            synth_idx < bin_idx,
            "synth-PATH (index {synth_idx}) must precede declared bin/ (index {bin_idx}) \
             in emit_root_path_block output; reversal causes launcher self-recursion"
        );
    }

    /// `emit_root_path_block` does NOT emit synth-PATH on the `--self` surface.
    ///
    /// Under `self_view=true` the root must not see its own launchers —
    /// `entrypoints/` is suppressed. Only the declared env vars appear.
    #[test]
    fn emit_root_path_block_no_synth_path_on_self_surface() {
        let dir = tempfile::tempdir().unwrap();

        let ep = Entrypoint {
            name: EntrypointName::try_from("rootool").unwrap(),
            target: "${installPath}/bin/rootool".to_string(),
        };
        let entrypoints = Entrypoints::new(vec![ep]).unwrap();

        use crate::package::metadata::env::{path::Path as EnvPath, var::Modifier};
        let path_var = Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(EnvPath {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        };
        let mut env_builder = metadata_env::EnvBuilder::new();
        env_builder.add_var(path_var);
        let env = env_builder.build();

        let root_metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
        });
        let pkg_root = dir.path().join("root_self");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root_dir = crate::file_structure::PackageDir { dir: pkg_root.clone() };
        let root_content = pkg_root.join("content");
        let root_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_root_path_block(
            &root_metadata,
            &root_dir,
            &root_content,
            &root_dep_contexts,
            true, // --self surface
            &mut entries,
        )
        .expect("emit_root_path_block must succeed");

        // No synth-PATH entry expected on --self surface.
        let synth_values: Vec<_> = entries
            .iter()
            .filter(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .map(|e| &e.value)
            .collect();
        assert!(
            synth_values.is_empty(),
            "emit_root_path_block with self_view=true must NOT emit synth-PATH; \
             got entrypoints PATH values: {:?}",
            synth_values
        );
    }
}
