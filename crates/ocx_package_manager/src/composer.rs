// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Two-env composition: flat iteration over each root's pre-built transitive closure with
//! cross-root dedup, gated per surface (`adr_two_env_composition.md`).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::future::join_all;
use tokio::task::JoinSet;

use crate::{error::DependencyError, error::PackageErrorKind};
use ocx_package::{
    install_info::DeferredComposition, install_info::InstallInfo, metadata, metadata::binary::Binaries,
    metadata::binary::BinaryName, metadata::dependency::DependencyName, metadata::entrypoint::EntrypointName,
    metadata::entrypoint::Entrypoints, metadata::env::dep_context::DependencyContext, metadata::env::entry::Entry,
    metadata::env::modifier::ModifierKind, metadata::env::resolver::ContentState, metadata::env::resolver::EnvResolver,
    metadata::integrations::INTEGRATION_TOKENS, metadata::integrations::IntegrationEntry,
    metadata::template::SelfEnvScope, metadata::visibility::Visibility, resolved_package::ResolvedDependency,
    resolved_package::ResolvedPackage,
};
use ocx_project::{ProjectConfig, ProjectLock, ladder::Ladder, lazy::LazyMode, lazy::LazyModeLadder};
use ocx_store::{
    file_structure::FileStructure, file_structure::PackageDir, file_structure::PackageStore,
    file_structure::RenderStampScope, file_structure::ShimDir, file_structure::SymlinkKind,
    file_structure::ToolchainHome,
};

use super::tasks::common;
use super::tasks::common::ClosureNode;
use super::tasks::patch_discovery::PatchDiscoveryMode;
use super::tasks::render_toolchain::HealOutcome;
use super::{
    Arrival, LazyAdvisory, PackageManager,
    concurrency::{Concurrency, acquire_permit},
};

/// One dep-load task's result; the `usize` is its index, for re-ordering after join.
type DepLoadResult = (
    usize,
    crate::Result<(metadata::Metadata, ResolvedPackage, ocx_oci::PinnedPackageRef)>,
);

/// The return value of [`compose`].
pub struct ComposeOutput {
    /// The composed env entries in emit order.
    pub entries: Vec<Entry>,

    /// Identifiers admitted by the surface gate, one per content (the first tag wins), deps
    /// before their root; the patch overlay applies only to these.
    pub admitted: Vec<ocx_oci::PinnedPackageRef>,

    /// Declared `binaries` claims of each admitted identifier that cross the surface
    /// (`adr_declared_binaries_metadata.md` §4 Decision A).
    pub admitted_binaries: Vec<(ocx_oci::PinnedPackageRef, BinaryName)>,

    /// Declared `entrypoints` claims, admitted like `admitted_binaries`.
    pub admitted_entrypoints: Vec<(ocx_oci::PinnedPackageRef, EntrypointName)>,

    /// Declared `integrations` of each admitted identifier, interface surface only
    /// (`adr_package_integrations.md`); deduped within this call only, never across calls.
    pub admitted_integrations: Vec<(ocx_oci::PinnedPackageRef, IntegrationEntry)>,
}

// ── Surface algebra ──────────────────────────────────────────────────────────
// `inspect::project_surface` must call these too, never re-derive them, or `ocx env` and inspect disagree.

/// The axes a single-surface composition emits: `--self` is the private axis, the consumer view
/// the interface axis.
pub(crate) fn surface_axes(self_view: bool) -> Visibility {
    if self_view {
        Visibility::PRIVATE
    } else {
        Visibility::INTERFACE
    }
}

/// Whether `visibility` reaches any axis of `axes`.
fn on_axes(visibility: Visibility, axes: Visibility) -> bool {
    (visibility.has_private() && axes.has_private()) || (visibility.has_interface() && axes.has_interface())
}

/// Whether a transitive dependency is admitted to a surface; roots are always admitted by the caller.
pub(crate) fn dep_admitted(effective: Visibility, self_view: bool) -> bool {
    dep_admitted_on(effective, surface_axes(self_view))
}

/// [`dep_admitted`] over a set of surface axes: admitted when its effective visibility reaches any.
fn dep_admitted_on(effective: Visibility, axes: Visibility) -> bool {
    on_axes(effective, axes)
}

/// Whether one carrier crosses onto a surface: a root's on the surface's axis, a dependency's only
/// on its interface side, on either surface.
pub(crate) fn carrier_crosses(carrier: Visibility, is_root: bool, self_view: bool) -> bool {
    carrier_crosses_on(carrier, is_root, surface_axes(self_view))
}

/// [`carrier_crosses`] over a set of surface axes: a root's carrier on any of them, a dependency's
/// only on its interface side.
fn carrier_crosses_on(carrier: Visibility, is_root: bool, axes: Visibility) -> bool {
    if is_root {
        on_axes(carrier, axes)
    } else {
        carrier.has_interface()
    }
}

/// Whether the integrations carrier crosses: interface surface only, at every depth
/// (`adr_package_integrations.md` §4.1).
pub(crate) fn integrations_cross(self_view: bool) -> bool {
    !self_view
}

/// Whether a composed root is a package of its own or a patch companion grafted onto its targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootRole {
    /// Its launchers are its own: `entrypoints/` on `PATH` and claimed.
    Package,
    /// Its launchers would run the companion's env, not its target's: off `PATH`, unclaimed.
    Graft,
}

/// Compose the runtime env from one or more root packages; `self_view` selects the private
/// (`--self`) surface over the interface one.
///
/// # Errors
///
/// A metadata load failure, an entrypoint collision across roots ([`check_entrypoints`]), or one
/// repository at two digests on the surface ([`check_repo_digest_conflicts`]).
pub(crate) async fn compose(
    roots: &[Arc<InstallInfo>],
    store: &PackageStore,
    self_view: bool,
    paths: &ComposePaths,
) -> crate::Result<ComposeOutput> {
    compose_gated(
        roots,
        store,
        surface_axes(self_view),
        integrations_cross(self_view),
        paths,
        RootRole::Package,
        &mut HashSet::new(),
    )
    .await
}

/// Compose one patch companion as part of its targets, on the surface `axes` those targets are
/// admitted through; `collect_integrations` is the outer composition's gate.
///
/// `emitted` holds the packages already emitted (advisory tag stripped): a dependency in it is
/// skipped, and on success the companion's own emissions join it. The companion itself is always
/// emitted, but never its launchers ([`RootRole::Graft`]).
///
/// # Errors
///
/// As [`compose`].
pub(crate) async fn compose_companion(
    companion: &Arc<InstallInfo>,
    store: &PackageStore,
    axes: Visibility,
    collect_integrations: bool,
    emitted: &mut HashSet<ocx_oci::PinnedPackageRef>,
) -> crate::Result<ComposeOutput> {
    // Committed only on success, or an optional companion that fails half-way hides deps it never emitted.
    let mut attempt = emitted.clone();
    // Digest lane: a companion is not a lock entry, so no link names it.
    let out = compose_gated(
        std::slice::from_ref(companion),
        store,
        axes,
        // Never derived from `axes`: an absent dep dir would fail a surface that carries no integrations.
        collect_integrations,
        &crate::composer::ComposePaths::digest_only(),
        RootRole::Graft,
        &mut attempt,
    )
    .await?;
    *emitted = attempt;
    Ok(out)
}

/// The composition itself over the surface axes `axes`, with the integrations carrier gated by an
/// explicit input.
///
/// Every emitted package path goes through `paths`, so "digest or link" is answered in one place.
/// A dependency already in `seen` (advisory tag stripped) is skipped; every emitted package joins it.
async fn compose_gated(
    roots: &[Arc<InstallInfo>],
    store: &PackageStore,
    axes: Visibility,
    collect_integrations: bool,
    paths: &ComposePaths,
    role: RootRole,
    seen: &mut HashSet<ocx_oci::PinnedPackageRef>,
) -> crate::Result<ComposeOutput> {
    // A single root was already gated at install time.
    if roots.len() > 1 {
        check_entrypoints(roots, store).await?;
    }

    check_repo_digest_conflicts(roots, axes)?;

    let mut entries: Vec<Entry> = Vec::new();
    let mut emitted_roots: HashSet<ocx_oci::PinnedPackageRef> = HashSet::new();
    let mut admitted: Vec<ocx_oci::PinnedPackageRef> = Vec::new();
    let mut admitted_binaries: Vec<(ocx_oci::PinnedPackageRef, BinaryName)> = Vec::new();
    let mut admitted_entrypoints: Vec<(ocx_oci::PinnedPackageRef, EntrypointName)> = Vec::new();
    let mut admitted_integrations: Vec<(ocx_oci::PinnedPackageRef, IntegrationEntry)> = Vec::new();

    let root_keys: HashSet<ocx_oci::PinnedPackageRef> = roots.iter().map(|r| r.identifier().strip_advisory()).collect();

    for root in roots {
        // A deferred root's package directories do not exist yet, so the `required` path probe must not run.
        let content_state = if root.deferred().is_some() {
            ContentState::Deferred
        } else {
            ContentState::Materialized
        };
        // Step 1: collect the surface-visible, deduplicated non-root entries, indexed to keep topological order.
        let mut visible_entries: Vec<(usize, ocx_oci::PinnedPackageRef)> = Vec::new();
        for tc_entry in &root.resolved().dependencies {
            let key = tc_entry.identifier.strip_advisory();

            // Or a gated-out private edge takes the `seen` slot and the explicit root never emits.
            if root_keys.contains(&key) {
                continue;
            }

            if !dep_admitted_on(tc_entry.visibility, axes) {
                continue;
            }

            // After the surface gate, or a gated-out entry masks a later admitted visit of the same package.
            if !seen.insert(key) {
                continue;
            }

            // Tag-bearing, or a tag-anchored patch rule (`*:21`) silently drops a required overlay.
            admitted.push(tc_entry.identifier.clone());

            visible_entries.push((visible_entries.len(), tc_entry.identifier.clone()));
        }

        // Step 2: parallel-load metadata for all visible entries.
        let mut tasks: JoinSet<DepLoadResult> = JoinSet::new();
        for (idx, dep_id) in &visible_entries {
            let dep_id = dep_id.clone();
            let store = store.clone();
            let root = Arc::clone(root);
            let idx = *idx;
            tasks.spawn(async move {
                let result = tc_entry_object_data(&root, &store, &dep_id).await;
                match result {
                    Ok((meta, resolved)) => (idx, Ok((meta, resolved, dep_id))),
                    Err(e) => (idx, Err(e)),
                }
            });
        }

        let mut loaded: Vec<Option<(metadata::Metadata, ResolvedPackage, ocx_oci::PinnedPackageRef)>> =
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
            // Digest lane: a dependency is not a lock entry, so no link names it.
            let dep_pkg = paths.install_path_for(&store.package_dir(&dep_id), PathLane::Digest);
            let dep_content = dep_pkg.content();

            if carrier_crosses_on(Binaries::IMPLICIT_VISIBILITY, false, axes)
                && let Some(binaries) = meta.binaries()
            {
                admitted_binaries.extend(binaries.iter().map(|name| (dep_id.clone(), name.clone())));
            }
            if carrier_crosses_on(Entrypoints::IMPLICIT_VISIBILITY, false, axes)
                && let Some(entrypoints) = meta.entrypoints()
            {
                admitted_entrypoints.extend(entrypoints.names().map(|name| (dep_id.clone(), name.clone())));
            }

            // The dep's own direct deps, never the root's.
            let dep_dep_contexts = build_dep_context_map(&meta, &dep_resolved, store, paths);

            if collect_integrations {
                // `INTEGRATION_TOKENS`, not the default: a hostile registry skips the publish check, and `${self.env.*}` would leak a private value.
                let resolver = metadata::template::TemplateResolver::new(&dep_content, &dep_dep_contexts)
                    .usage(INTEGRATION_TOKENS);
                admitted_integrations.extend(
                    meta.integrations()
                        .resolve(&resolver)?
                        .into_iter()
                        .map(|entry| (dep_id.clone(), entry)),
                );
            }

            emit_dep_path_block(
                &meta,
                &dep_pkg,
                &dep_content,
                &dep_dep_contexts,
                axes,
                content_state,
                &mut entries,
            )?;
        }

        // Roots emit after their TC, so the root's `PATH` prepends win lookup over its deps.
        let root_key = root.identifier().strip_advisory();
        // Keyed apart from `seen`, so a root an earlier composition already emitted still grafts here.
        if emitted_roots.insert(root_key.clone()) {
            seen.insert(root_key);
            // Tag-bearing, as for deps.
            admitted.push(root.identifier().clone());

            if carrier_crosses_on(Binaries::IMPLICIT_VISIBILITY, true, axes)
                && let Some(binaries) = root.metadata().binaries()
            {
                admitted_binaries.extend(binaries.iter().map(|name| (root.identifier().clone(), name.clone())));
            }
            if role == RootRole::Package
                && carrier_crosses_on(Entrypoints::IMPLICIT_VISIBILITY, true, axes)
                && let Some(entrypoints) = root.metadata().entrypoints()
            {
                admitted_entrypoints.extend(
                    entrypoints
                        .names()
                        .map(|name| (root.identifier().clone(), name.clone())),
                );
            }

            let root_dep_contexts = build_dep_context_map(root.metadata(), root.resolved(), store, paths);

            // Following lane: an explicit root is a lock entry, so a trusted link may name it.
            let root_pkg = paths.install_path_for(root.dir(), PathLane::Following);
            let root_content = root_pkg.content();

            if collect_integrations {
                let resolver = metadata::template::TemplateResolver::new(&root_content, &root_dep_contexts)
                    .usage(INTEGRATION_TOKENS);
                admitted_integrations.extend(
                    root.metadata()
                        .integrations()
                        .resolve(&resolver)?
                        .into_iter()
                        .map(|entry| (root.identifier().clone(), entry)),
                );
            }

            // First, so it resolves last (consumers prepend): `entrypoints/` > `bin/` > `shims/`.
            emit_shim_slot(root, axes, &mut entries);

            match role {
                RootRole::Package => emit_root_path_block(
                    root.metadata(),
                    &root_pkg,
                    &root_content,
                    &root_dep_contexts,
                    axes,
                    content_state,
                    &mut entries,
                )?,
                RootRole::Graft => emit_package_vars(
                    root.metadata(),
                    &root_content,
                    &root_dep_contexts,
                    /* is_root = */ true,
                    axes,
                    content_state,
                    &mut entries,
                )?,
            }
        }
    }

    Ok(ComposeOutput {
        entries,
        admitted,
        admitted_binaries,
        admitted_entrypoints,
        admitted_integrations,
    })
}

/// Uniqueness check on entrypoint names across the interface projection of one or more roots;
/// private-surface duplicates are tolerated.
///
/// # Errors
///
/// [`PackageErrorKind::EntrypointCollision`] listing every owner of the first (sorted) colliding
/// name, or [`PackageErrorKind::Internal`] when a package's metadata cannot be read.
pub async fn check_entrypoints(roots: &[Arc<InstallInfo>], store: &PackageStore) -> Result<(), PackageErrorKind> {
    let mut owners: BTreeMap<EntrypointName, Vec<ocx_oci::PinnedPackageRef>> = BTreeMap::new();
    let mut seen: HashSet<ocx_oci::PinnedPackageRef> = HashSet::new();

    for root in roots {
        if seen.insert(root.identifier().strip_advisory())
            && let Some(eps) = root.metadata().entrypoints()
        {
            for name in eps.names() {
                owners.entry(name.clone()).or_default().push(root.identifier().clone());
            }
        }

        for tc_entry in &root.resolved().dependencies {
            if !tc_entry.visibility.has_interface() {
                continue;
            }
            let key = tc_entry.identifier.strip_advisory();
            if !seen.insert(key) {
                continue;
            }
            // Never `store.content`: a deferred root's entries have no package directory.
            let (dep_metadata, _dep_resolved) = tc_entry_object_data(root, store, &tc_entry.identifier)
                .await
                .map_err(PackageErrorKind::Internal)?;
            if let Some(eps) = dep_metadata.entrypoints() {
                for name in eps.names() {
                    owners
                        .entry(name.clone())
                        .or_default()
                        .push(tc_entry.identifier.clone());
                }
            }
        }
    }

    for (name, list) in owners {
        if list.len() > 1 {
            return Err(PackageErrorKind::EntrypointCollision { name, owners: list });
        }
    }

    Ok(())
}

/// Build a package's `${deps.NAME.installPath}` context map, preferring `resolved`'s pinned identifiers.
// Digest lane only: a dependency has no link, and a lock lookup would duplicate platform selection.
fn build_dep_context_map(
    metadata: &metadata::Metadata,
    resolved: &ResolvedPackage,
    store: &PackageStore,
    paths: &ComposePaths,
) -> HashMap<DependencyName, DependencyContext> {
    let resolved_id_map: HashMap<ocx_oci::Repository, &ocx_oci::PinnedPackageRef> = resolved
        .dependencies
        .iter()
        .map(|d| (ocx_oci::Repository::from(d.identifier.as_identifier()), &d.identifier))
        .collect();
    metadata
        .dependencies()
        .iter()
        .map(|d| {
            let name = d.name();
            let key = ocx_oci::Repository::from(d.identifier.as_identifier());
            let install_id = resolved_id_map.get(&key).copied().unwrap_or(&d.identifier);
            let install_path = paths
                .install_path_for(&store.package_dir(install_id), PathLane::Digest)
                .content();
            (name, DependencyContext::path_only(install_id.clone(), install_path))
        })
        .collect()
}

/// Resolve one package's declared env into a private per-package scope, then push the crossing entries.
///
/// Every var resolves before gating, or `${self.env.KEY}` misses a private var that never crosses.
///
/// # Errors
///
/// The first resolution failure in declaration order.
fn emit_package_vars(
    metadata: &metadata::Metadata,
    content: &Path,
    dep_contexts: &HashMap<DependencyName, DependencyContext>,
    is_root: bool,
    axes: Visibility,
    content_state: ContentState,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    let Some(env) = metadata.env() else {
        return Ok(());
    };
    let resolver = EnvResolver::new(content, dep_contexts).with_content_state(content_state);

    // Relies on `validate_env_modifier_types` refusing unknown modifiers, or a duplicate `KEY` escapes `AmbiguousSelfEnvRef`.
    let mut declared_before: SelfEnvScope<Entry> = SelfEnvScope::new();

    for var in env {
        let crosses = carrier_crosses_on(var.visibility, is_root, axes);
        // A non-crossing var skips emit assertions, so a value nobody emits cannot fail the composition.
        let resolved = if crosses {
            resolver.resolve(var, &declared_before)?
        } else {
            resolver.resolve_without_emit_assertions(var, &declared_before)?
        };
        let Some(entry) = resolved else {
            continue;
        };
        if crosses {
            entries.push(entry.clone());
        }
        declared_before.push(entry);
    }

    Ok(())
}

/// Emit the dep's env vars, then its `entrypoints/` PATH entry, pushed last so it shadows `bin/`
/// (`test_synthetic_entrypoints_path_emitted_after_declared_bin`).
fn emit_dep_path_block(
    dep_metadata: &metadata::Metadata,
    dep_pkg: &PackageDir,
    dep_content: &Path,
    dep_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    axes: Visibility,
    content_state: ContentState,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    emit_package_vars(
        dep_metadata,
        dep_content,
        dep_dep_contexts,
        /* is_root = */ false,
        axes,
        content_state,
        entries,
    )?;

    // The same gate as the claim list, so a claim never contradicts `PATH`.
    if carrier_crosses_on(Entrypoints::IMPLICIT_VISIBILITY, false, axes)
        && let Some(eps) = dep_metadata.entrypoints()
        && !eps.is_empty()
    {
        entries.push(synth_entrypoints_path_for(dep_pkg));
    }

    Ok(())
}

/// Emit the root's env vars, then its `entrypoints/` PATH entry (absent under `--self`), ordered as
/// [`emit_dep_path_block`].
fn emit_root_path_block(
    root_metadata: &metadata::Metadata,
    root_dir: &PackageDir,
    root_content: &Path,
    root_dep_contexts: &HashMap<DependencyName, DependencyContext>,
    axes: Visibility,
    content_state: ContentState,
    entries: &mut Vec<Entry>,
) -> crate::Result<()> {
    emit_package_vars(
        root_metadata,
        root_content,
        root_dep_contexts,
        /* is_root = */ true,
        axes,
        content_state,
        entries,
    )?;

    // The same gate as the root's `admitted_entrypoints` claim.
    if carrier_crosses_on(Entrypoints::IMPLICIT_VISIBILITY, true, axes)
        && let Some(eps) = root_metadata.entrypoints()
        && !eps.is_empty()
    {
        entries.push(synth_entrypoints_path_for(root_dir));
    }

    Ok(())
}

/// Refuse a composition whose closure projected onto `axes` holds one `registry/repo` at two or
/// more digests; two tags on one digest are fine.
///
/// # Errors
///
/// [`DependencyError::Conflict`] for the first conflicting repository.
pub fn check_repo_digest_conflicts(roots: &[Arc<InstallInfo>], axes: Visibility) -> Result<(), DependencyError> {
    refuse_digest_conflicts(surface_closure(roots, axes))
}

/// [`check_repo_digest_conflicts`] over an identifier set already projected onto its surface.
///
/// # Errors
///
/// [`DependencyError::Conflict`] for the first conflicting repository.
pub(crate) fn refuse_digest_conflicts<'a>(
    identifiers: impl IntoIterator<Item = &'a ocx_oci::PinnedPackageRef>,
) -> Result<(), DependencyError> {
    match digest_conflicts(identifiers).into_iter().next() {
        Some(conflict) => Err(DependencyError::Conflict {
            repository: conflict.repository,
            identifiers: conflict.identifiers,
        }),
        None => Ok(()),
    }
}

/// Warn for every conflict [`check_repo_digest_conflicts`] would refuse; `deps` uses it so the tree stays inspectable.
pub fn warn_repo_digest_conflicts(roots: &[Arc<InstallInfo>], self_view: bool) {
    for conflict in collect_repo_digest_conflicts(roots, surface_axes(self_view)) {
        tracing::warn!(
            "conflicting versions for {}: {}",
            conflict.repository,
            conflict
                .identifiers
                .iter()
                .map(|identifier| identifier.to_string())
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
}

/// One repository resolved to two or more distinct digests on the active surface, in first-seen order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DigestConflict {
    pub repository: ocx_oci::Repository,
    pub identifiers: Vec<ocx_oci::PinnedPackageRef>,
}

/// Collects version conflicts across the union closure projected onto `axes`, sorted by repository.
pub(crate) fn collect_repo_digest_conflicts(roots: &[Arc<InstallInfo>], axes: Visibility) -> Vec<DigestConflict> {
    digest_conflicts(surface_closure(roots, axes))
}

/// Each root's identifier and its dependencies admitted on `axes`, from `resolved()`.
pub(crate) fn surface_closure(
    roots: &[Arc<InstallInfo>],
    axes: Visibility,
) -> impl Iterator<Item = &ocx_oci::PinnedPackageRef> {
    roots
        .iter()
        .flat_map(move |root| std::iter::once(root.identifier()).chain(admitted_dependencies(root, axes)))
}

/// `root`'s dependencies admitted on `axes`, from `resolved()`.
pub(crate) fn admitted_dependencies(
    root: &InstallInfo,
    axes: Visibility,
) -> impl Iterator<Item = &ocx_oci::PinnedPackageRef> {
    root.resolved()
        .dependencies
        .iter()
        .filter(move |dep| dep_admitted_on(dep.visibility, axes))
        .map(|dep| &dep.identifier)
}

/// The repositories `identifiers` reach at two or more digests, sorted by repository.
pub(crate) fn digest_conflicts<'a>(
    identifiers: impl IntoIterator<Item = &'a ocx_oci::PinnedPackageRef>,
) -> Vec<DigestConflict> {
    let mut by_repository: BTreeMap<ocx_oci::Repository, Vec<ocx_oci::PinnedPackageRef>> = BTreeMap::new();
    for identifier in identifiers {
        record_repo_identifier(identifier, &mut by_repository);
    }
    by_repository
        .into_iter()
        .filter(|(_, identifiers)| identifiers.len() >= 2)
        .map(|(repository, identifiers)| DigestConflict {
            repository,
            identifiers,
        })
        .collect()
}

fn record_repo_identifier(
    id: &ocx_oci::PinnedPackageRef,
    by_repository: &mut BTreeMap<ocx_oci::Repository, Vec<ocx_oci::PinnedPackageRef>>,
) {
    let repository = ocx_oci::Repository::from(&**id);
    let seen = by_repository.entry(repository).or_default();
    if seen.iter().any(|existing| existing.digest() == id.digest()) {
        return;
    }
    seen.push(id.clone());
}

/// Construct the synthetic `PATH ⊳ <pkg_root>/entrypoints` entry for `pkg`.
fn synth_entrypoints_path_for(pkg: &PackageDir) -> Entry {
    Entry {
        key: "PATH".to_string(),
        value: pkg.entrypoints().to_string_lossy().into_owned(),
        kind: ModifierKind::Path,
        separator: None,
    }
}

// ── Following-lane install paths (`adr_project_toolchain_links.md` § Rationale from code: ocx_package_manager composer) ──
// Shim slots, `${deps.*}` and persisted files stay digest: a baked link outlives its tree.

/// Which producer answers for one package's install path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathLane {
    /// Follow the rendered link when trustworthy at probe time, else the digest root; for explicit roots.
    Following,

    /// The digest root, always; for dependencies, which are not lock entries.
    Digest,
}

/// The composition's single install-path producer, carrying the already-probed trusted links;
/// empty means digest paths for everything.
pub(crate) struct ComposePaths {
    /// Digest root → the link naming it; of two sharing a digest, the first in `groups`, then `lock.tools`, order.
    trusted: HashMap<PathBuf, PathBuf>,
}

/// The toolchain tree one composing invocation may follow, as the emitter resolved it.
#[derive(Debug, Clone)]
pub struct ToolchainLinks {
    /// The resolved `pinned` value; `true` means digest paths, with no I/O.
    pub pinned: bool,

    /// The resolved toolchain home whose link tree is probed.
    pub home: ToolchainHome,

    /// Lets the heal refuse a symlinked project path, which `home` alone cannot answer.
    pub scope: RenderStampScope,

    /// The lock the links are compared against.
    pub lock: ProjectLock,

    /// The groups this invocation selected and emits, never the default group alone.
    pub groups: Vec<String>,
}

impl ComposePaths {
    /// Digest paths for everything, touching no filesystem.
    pub(crate) fn digest_only() -> Self {
        Self {
            trusted: HashMap::new(),
        }
    }

    /// Heal the selected groups, probe their entries, and keep the ones this composition may follow;
    /// an untrustworthy link degrades to the digest path, never the wrong package.
    ///
    /// # Errors
    ///
    /// Only [`PackageErrorKind::ToolchainPath`] (exit 78); every I/O condition degrades an entry instead.
    pub(crate) async fn resolve(
        links: &ToolchainLinks,
        file_structure: &FileStructure,
        platform: &ocx_oci::Platform,
    ) -> Result<Self, PackageErrorKind> {
        if links.pinned {
            return Ok(Self::digest_only());
        }

        // A refused heal ends at digest paths: probing a tree it would not enter puts an attacker's link on `PATH`.
        if let HealOutcome::Refused { reason } = super::tasks::render_toolchain::heal_links(
            file_structure,
            &links.home,
            &links.scope,
            &links.lock,
            &links.groups,
            platform,
        )
        .await?
        {
            log::debug!(
                "Toolchain links under '{}' were not followed: {reason}",
                links.home.root().display()
            );
            return Ok(Self::digest_only());
        }

        // Walk order is the tie-break: `groups`, then `lock.tools`.
        let mut candidates: Vec<(PathBuf, PathBuf)> = Vec::new();
        for group in &links.groups {
            for tool in links.lock.tools.iter().filter(|tool| &tool.group == group) {
                let entry = links.home.entry(&tool.group, &tool.name)?;
                if let Some(target) = super::tasks::render_toolchain::link_target(file_structure, tool, platform) {
                    candidates.push((target, entry));
                }
            }
        }

        let guarded_home = links.home.clone();
        let guarded_scope = links.scope.clone();
        let trusted = tokio::task::spawn_blocking(move || {
            // Rechecked on the read path: the heal's verdict predates this pass.
            if let Err(error) = super::tasks::render_toolchain::refuse_symlinked_home(&guarded_scope, &guarded_home) {
                log::debug!(
                    "Toolchain links under '{}' were not followed: {error}",
                    guarded_home.root().display()
                );
                return HashMap::new();
            }

            let mut trusted: HashMap<PathBuf, PathBuf> = HashMap::new();
            for (target, entry) in candidates {
                // Checked before trust, so only a trusted candidate can shadow its sibling.
                if trusted.contains_key(&target) {
                    continue;
                }
                // Only this keeps a symlinked `<home>/<group>` off `PATH`; `is_link`, as `is_symlink` misses a junction.
                if entry.parent().is_some_and(ocx_util::fs::symlink::is_link) {
                    continue;
                }
                if link_is_trustworthy(&entry, &target) {
                    trusted.insert(target, entry);
                }
            }
            trusted
        })
        .await
        .unwrap_or_else(|error| {
            log::debug!("The toolchain link probe did not complete: {error}");
            HashMap::new()
        });

        Ok(Self { trusted })
    }

    /// The install path one package composes to: its trusted link on the following lane, else its digest root.
    pub(crate) fn install_path_for(&self, package: &PackageDir, lane: PathLane) -> PackageDir {
        match lane {
            PathLane::Digest => package.clone(),
            PathLane::Following => self
                .trusted
                .get(package.root())
                .map_or_else(|| package.clone(), |link| PackageDir::with_root(link.clone())),
        }
    }
}

/// Whether the link at `entry` still names `target`; blocking, and any bad entry is `false`.
fn link_is_trustworthy(entry: &Path, target: &Path) -> bool {
    // Un-canonicalised, as the heal compares: canonicalising follows through a hostile target the heal refused.
    ocx_util::fs::symlink::is_link(entry) && std::fs::read_link(entry).is_ok_and(|current| current == target)
}

/// Fill the project-tier `pinned` ladder, unresolved: `--pinned`/`--no-pinned` ▸ `ocx.toml` ▸
/// `OCX_TOOLCHAIN_PINNED`, the environment weakest.
// `cli` is `None` when neither flag was given, never `false`, or `--no-pinned` loses its override.
pub fn pinned_ladder_for_project(cli: Option<bool>, config: &ProjectConfig) -> Ladder<bool> {
    Ladder {
        cli,
        file: config.pinned,
        environment: ocx_project::activate::pinned_from_env(),
    }
}

/// Resolve the `pinned` ladder for one project, down to [`ocx_project::activate::PINNED_FLOOR`].
pub fn pinned_for_project(cli: Option<bool>, config: &ProjectConfig) -> bool {
    pinned_ladder_for_project(cli, config).resolve(ocx_project::activate::PINNED_FLOOR)
}

// ── Lazy package loading: the shim slot ─────────────────────────────────────
// A deferred tool differs from a materialized one in its shim slot and carrier source only, or it stops composing byte-identically once materialized.

/// Construct the synthetic `PATH ⊳ <shim-root>/bin` entry for a deferred tool.
// `bin/`, never the shim root, where a `digest` or `refs` claim would shadow the store's own entries.
fn synth_shim_path_for(shim: &ShimDir) -> Entry {
    Entry {
        key: "PATH".to_string(),
        value: shim.bin().to_string_lossy().into_owned(),
        kind: ModifierKind::Path,
        separator: None,
    }
}

/// Push a deferred root's shim slot, the lowest-precedence entry of its block; absent under `--self`.
///
/// Pushed first because consumers prepend: once materialized, the real `bin/` then shadows the shim.
fn emit_shim_slot(root: &InstallInfo, axes: Visibility, entries: &mut Vec<Entry>) {
    if !carrier_crosses_on(Entrypoints::IMPLICIT_VISIBILITY, true, axes) {
        return;
    }
    if let Some(deferred) = root.deferred() {
        entries.push(synth_shim_path_for(deferred.shim()));
    }
}

/// One closure entry's `(metadata, resolved)` pair: from the package store for a materialized
/// root, from the deferred closure (no package directory) otherwise.
///
/// # Errors
///
/// The load failure, or [`PackageErrorKind::NotFound`] when a deferred closure lacks `dep_id`.
async fn tc_entry_object_data(
    root: &InstallInfo,
    store: &PackageStore,
    dep_id: &ocx_oci::PinnedPackageRef,
) -> crate::Result<(metadata::Metadata, ResolvedPackage)> {
    let Some(deferred) = root.deferred() else {
        return common::load_object_data(store, &store.content(dep_id)).await;
    };
    // Never fall back to the store: a closure miss would compose from a directory that may not exist.
    let member = deferred
        .member(dep_id)
        .ok_or_else(|| crate::Error::package(dep_id.as_identifier().clone(), PackageErrorKind::NotFound))?;
    Ok((member.metadata().clone(), member.resolved().clone()))
}

// ── Lazy package loading: resolving the ladder and building the roots ────────

/// One tool to compose, with its resolved `lazy-mode` ([`lazy_mode_for_tool`] or [`lazy_mode_for_package`]).
#[derive(Debug, Clone)]
pub struct ComposeRequest {
    /// The identifier to compose, as the caller's tier resolved it.
    pub identifier: ocx_oci::PackageRef,
    /// The resolved `lazy-mode`.
    pub mode: LazyMode,
}

/// How a request whose resolved mode is [`LazyMode::Never`] reaches the store.
#[derive(Debug, Clone)]
pub enum Materialization {
    /// Resolve locally, install on a miss.
    Install,
    /// Probe the local store only; a miss is omitted, not an error (`ocx env --no-pull`).
    LocalOnly,
    /// Resolve through the stable install-symlink namespace
    /// (`ocx package env --candidate` / `--current`).
    Symlink(SymlinkKind),
}

/// A request the composing caller dropped rather than failed on.
#[derive(Debug)]
pub struct ComposeOmission {
    /// The request that was dropped, as the caller named it.
    pub identifier: ocx_oci::PackageRef,
    /// Why it was dropped, verbatim from the probe; wider than not-found, so no exit code may be derived from it.
    pub reason: PackageErrorKind,
}

/// The compose roots [`PackageManager::compose_roots`] produced, plus the two
/// things only the composing caller can report.
#[derive(Debug)]
pub struct ComposeRoots {
    /// One root per surviving request, in request order.
    pub roots: Vec<Arc<InstallInfo>>,

    /// Advisories raised by the deferred tools' declared metadata, returned for `--format json`.
    pub advisories: Vec<LazyAdvisory>,

    /// Requests dropped under [`Materialization::LocalOnly`].
    pub omitted: Vec<ComposeOmission>,

    /// Requests this invocation pulled, in request order (`resolution.autoInstalled`).
    pub pulled: Vec<ocx_oci::PackageRef>,
}

impl PackageManager {
    /// Turn composition requests into compose roots: eager ones materialize per `materialization`,
    /// deferred ones get a shim tree. The result never depends on content-cache state.
    ///
    /// # Errors
    ///
    /// [`Error::FindFailed`](super::error::Error::FindFailed) for an eager request,
    /// [`Error::DiscoverFailed`](super::error::Error::DiscoverFailed) when an installed one's patch
    /// discovery fails fatally, or [`Error::ResolveFailed`](super::error::Error::ResolveFailed) when a
    /// shim tree cannot be generated.
    pub async fn compose_roots(
        &self,
        requests: &[ComposeRequest],
        platform: &ocx_oci::Platform,
        materialization: Materialization,
        concurrency: Concurrency,
    ) -> Result<ComposeRoots, super::error::Error> {
        let mut slots: Vec<Option<Arc<InstallInfo>>> = (0..requests.len()).map(|_| None).collect();
        let mut advisories: Vec<LazyAdvisory> = Vec::new();
        let mut omitted: Vec<ComposeOmission> = Vec::new();
        let mut pulled: Vec<ocx_oci::PackageRef> = Vec::new();

        let eager: Vec<(usize, ocx_oci::PackageRef)> = requests
            .iter()
            .enumerate()
            .filter(|(_, request)| request.mode == LazyMode::Never)
            .map(|(index, request)| (index, request.identifier.clone()))
            .collect();

        let identifiers: Vec<ocx_oci::PackageRef> = eager.iter().map(|(_, id)| id.clone()).collect();
        match &materialization {
            Materialization::Install => {
                let found = self
                    .find_or_install_all(&identifiers, platform.clone(), concurrency)
                    .await?;
                // Lazy: no network in steady state, so a per-prompt `ocx exec` only fetches a never-looked descriptor.
                self.discover_patches_all(&identifiers, platform, PatchDiscoveryMode::Lazy, concurrency)
                    .await?;
                for ((index, identifier), found) in eager.iter().zip(found) {
                    if found.arrival == Arrival::Pulled {
                        pulled.push(identifier.clone());
                    }
                    slots[*index] = Some(Arc::new(found.info));
                }
            }
            Materialization::Symlink(kind) => {
                let found = self.find_symlink_all(identifiers, *kind).await?;
                for ((index, _), info) in eager.iter().zip(found) {
                    slots[*index] = Some(Arc::new(info));
                }
            }
            Materialization::LocalOnly => {
                let semaphore = concurrency.semaphore();
                // `join_all`, not `buffer_unordered`: the `omitted` order is observable and must stay request order.
                let probed = join_all(eager.iter().map(|(_, identifier)| {
                    let semaphore = semaphore.clone();
                    async move {
                        let _permit = acquire_permit(&semaphore).await;
                        self.local_root(identifier, platform).await
                    }
                }))
                .await;

                // In request order, so the lowest-index error is the one that surfaces.
                for ((index, identifier), result) in eager.iter().zip(probed) {
                    match result {
                        Ok(info) => slots[*index] = Some(Arc::new(info)),
                        Err(kind) if unavailable_locally(&kind) => omitted.push(ComposeOmission {
                            identifier: identifier.clone(),
                            reason: kind,
                        }),
                        Err(kind) => {
                            return Err(super::error::Error::FindFailed(vec![super::error::PackageError::new(
                                identifier.clone(),
                                kind,
                            )]));
                        }
                    }
                }
            }
        }

        // Sequential: `prepare_lazy` is already a bounded parallel walk.
        for (index, request) in requests.iter().enumerate() {
            if request.mode != LazyMode::Always {
                continue;
            }
            match self.deferred_root(&request.identifier, platform.clone()).await {
                Ok((root, mut raised)) => {
                    slots[index] = Some(root);
                    advisories.append(&mut raised);
                }
                Err(kind) if matches!(materialization, Materialization::LocalOnly) && unavailable_locally(&kind) => {
                    omitted.push(ComposeOmission {
                        identifier: request.identifier.clone(),
                        reason: kind,
                    });
                }
                Err(kind) => {
                    return Err(super::error::Error::ResolveFailed(vec![
                        super::error::PackageError::new(request.identifier.clone(), kind),
                    ]));
                }
            }
        }

        Ok(ComposeRoots {
            roots: slots.into_iter().flatten().collect(),
            advisories,
            omitted,
            pulled,
        })
    }

    /// Probe the local package store for one eager request.
    ///
    /// # Errors
    ///
    /// [`PackageErrorKind::NotFound`] when it is not materialized, else whatever the resolve surfaced.
    async fn local_root(
        &self,
        identifier: &ocx_oci::PackageRef,
        platform: &ocx_oci::Platform,
    ) -> Result<InstallInfo, PackageErrorKind> {
        // A digest skips the index: an absent cached leaf manifest must not turn an installed tool into a miss.
        match ocx_oci::PinnedPackageRef::try_from(identifier.clone()) {
            Ok(pinned) => self.find_plain(&pinned).await?.ok_or(PackageErrorKind::NotFound),
            Err(_) => self.find(identifier, platform.clone()).await,
        }
    }

    /// Build one deferred tool's compose root: generate the shim tree, then read its closure's
    /// carriers back from the ref-linked config blobs.
    ///
    /// # Errors
    ///
    /// Whatever [`prepare_lazy`](Self::prepare_lazy) surfaces, or [`PackageErrorKind::Internal`]
    /// when a closure member's config blob cannot be read.
    async fn deferred_root(
        &self,
        package: &ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
    ) -> Result<(Arc<InstallInfo>, Vec<LazyAdvisory>), PackageErrorKind> {
        let prepared = self.prepare_lazy(package, platform).await?;
        let store = &self.file_structure().packages;

        // The full closure in `resolve.json`'s shape: a truncated one silently disables the surface, collision and conflict gates.
        let mut closure: Vec<Arc<InstallInfo>> = Vec::new();
        let mut root_dependencies: Vec<ResolvedDependency> = Vec::new();
        let mut root_node: Option<ClosureNode> = None;
        for node in prepared.closure {
            if node.is_root {
                root_node = Some(node);
                continue;
            }
            root_dependencies.push(ResolvedDependency {
                identifier: node.identifier.clone(),
                visibility: node.effective_visibility.unwrap_or(Visibility::SEALED),
            });
            closure.push(Arc::new(self.closure_member(&node, store).await?));
        }

        let root_node = root_node.ok_or_else(|| {
            PackageErrorKind::Internal(crate::error::file_error(
                prepared.shim.root(),
                std::io::Error::other("shim closure carries no root node"),
            ))
        })?;
        let metadata = self.closure_metadata(&root_node).await?;
        let root = InstallInfo::new(
            root_node.identifier.clone(),
            metadata,
            ResolvedPackage {
                dependencies: root_dependencies,
            },
            store.package_dir(&root_node.identifier),
        )
        .with_deferred(DeferredComposition::new(prepared.shim, closure));

        Ok((Arc::new(root), prepared.advisories))
    }

    /// One closure member as a compose-time [`InstallInfo`], carrying its declared dependency edges.
    async fn closure_member(&self, node: &ClosureNode, store: &PackageStore) -> Result<InstallInfo, PackageErrorKind> {
        let metadata = self.closure_metadata(node).await?;
        let resolved = ResolvedPackage {
            dependencies: node
                .dependencies
                .iter()
                .map(|edge| ResolvedDependency {
                    identifier: edge.identifier.clone(),
                    visibility: edge.visibility,
                })
                .collect(),
        };
        Ok(InstallInfo::new(
            node.identifier.clone(),
            metadata,
            resolved,
            store.package_dir(&node.identifier),
        ))
    }

    /// Read one closure node's metadata back from its config blob; a deferred tool has no package directory.
    async fn closure_metadata(&self, node: &ClosureNode) -> Result<metadata::Metadata, PackageErrorKind> {
        let blobs = &self.file_structure().blobs;
        let registry = node.identifier.registry();
        let bytes = blobs
            .read_blob(registry, &node.config_digest)
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?
            .ok_or_else(|| {
                let path = blobs.path(registry, &node.config_digest);
                PackageErrorKind::Internal(crate::error::file_error(
                    &path,
                    std::io::Error::from(std::io::ErrorKind::NotFound),
                ))
            })?;
        let raw: metadata::Metadata = serde_json::from_slice(&bytes)
            .map_err(|e| PackageErrorKind::Internal(crate::Error::SerializationFailure(e)))?;
        // Validated like every load path: a config blob is publisher-authored input.
        Ok(metadata::ValidMetadata::try_from(raw)
            .map_err(|error| PackageErrorKind::Internal(error.into()))?
            .into())
    }
}

/// Whether a failed request means "not available on this machine" rather than "broken".
// Includes policy blocks: `--no-pull` with `--offline`/`--frozen` reports a missing tag that way, not as `NotFound`.
fn unavailable_locally(kind: &PackageErrorKind) -> bool {
    match kind {
        PackageErrorKind::NotFound | PackageErrorKind::OfflineManifestMissing(_) => true,
        PackageErrorKind::Internal(crate::Error::OfflineMode) => true,
        PackageErrorKind::Internal(crate::Error::OciIndex(error)) => {
            matches!(error, ocx_index::error::Error::PolicyResolutionBlocked { .. })
        }
        _ => false,
    }
}

/// Fill the OCI-tier `lazy-mode` ladder, unresolved: `--lazy-mode` ▸ `OCX_LAZY_MODE`; no `ocx.toml` tiers.
pub fn lazy_mode_ladder_for_package(cli: Option<LazyMode>) -> LazyModeLadder {
    LazyModeLadder {
        cli,
        environment: LazyMode::from_env(),
        ..LazyModeLadder::default()
    }
}

/// Resolve the `lazy-mode` ladder for one OCI-tier package, through the host-aware `resolve_for_host`.
pub fn lazy_mode_for_package(cli: Option<LazyMode>) -> LazyMode {
    lazy_mode_ladder_for_package(cli).resolve_for_host()
}

// ── Specification tests (Phase 3) ───────────────────────────────────────────
//
// These tests are authored against the ADR + plan BEFORE the implementation is
// written. Phase 4 fills the bodies and removes the `#[should_panic]` markers
// so the tests assert the real composer output.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::error::PackageErrorKind;
    use ocx_oci::{Digest, PackageRef, PinnedPackageRef};
    use ocx_package::{
        install_info::InstallInfo,
        metadata::{
            self, bundle, dependency,
            entrypoint::{EntrypointName, Entrypoints},
            env::{
                self as metadata_env,
                var::{Modifier, Var},
            },
            visibility::Visibility,
        },
        resolved_package::{ResolvedDependency, ResolvedPackage},
    };
    use ocx_store::file_structure::{FileStructure, PackageStore};

    use super::{
        ContentState, DependencyError, DigestConflict, carrier_crosses, carrier_crosses_on, check_entrypoints,
        check_repo_digest_conflicts, collect_repo_digest_conflicts, compose, compose_companion, dep_admitted,
        dep_admitted_on, emit_dep_path_block, emit_root_path_block, integrations_cross, surface_axes,
    };

    const REGISTRY: &str = "example.com";

    // ── Fixture helpers (adapted from visible.rs::tests) ──────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned(repo: &str, hex_char: char) -> PinnedPackageRef {
        let id = PackageRef::new_registry(repo, REGISTRY).clone_with_digest(sha256(hex_char));
        PinnedPackageRef::try_from(id).unwrap()
    }

    /// Build a minimal `InstallInfo` with an empty env and the given resolved closure.
    fn make_install_info(repo: &str, hex_char: char, resolved: ResolvedPackage) -> InstallInfo {
        make_install_info_for(pinned(repo, hex_char), resolved)
    }

    /// [`make_install_info`] for a caller-built identifier, e.g. a tagged one.
    fn make_install_info_for(id: PinnedPackageRef, resolved: ResolvedPackage) -> InstallInfo {
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        InstallInfo::new(
            id,
            metadata,
            resolved,
            ocx_store::file_structure::PackageDir {
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            ocx_store::file_structure::PackageDir { dir: pkg_root },
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
        let entrypoints = Entrypoints::from_names([ep_name]);
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            ocx_store::file_structure::PackageDir { dir: pkg_root },
        )
    }

    fn make_store(root: &std::path::Path) -> PackageStore {
        let fs = FileStructure::with_root(root.to_path_buf());
        fs.packages.clone()
    }

    /// Write a minimal on-disk package directory (metadata.json + resolve.json)
    /// so `PackageStore::lookup` can find it. Mirrors the visible.rs
    /// `seed_package_in_store` helper.
    fn seed_package_in_store(store: &PackageStore, id: &PinnedPackageRef, resolved: &ResolvedPackage) {
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
        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(
            out.entries.is_empty(),
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // SEALED.has_interface() = false → skip in default exec.
        assert!(
            out.entries.is_empty(),
            "SEALED dep must contribute nothing in default exec"
        );
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // SEALED.has_private() = false → skip under --self too.
        assert!(
            out.entries.is_empty(),
            "SEALED dep must contribute nothing under --self"
        );
    }

    /// The root's own entry points are interface-only —
    /// `Entrypoints::IMPLICIT_VISIBILITY` (INTERFACE) under `carrier_crosses`
    /// on the root's own axis.
    ///
    /// Couples `admitted_entrypoints` to `emit_root_path_block`'s synth-PATH
    /// push — both route through the same carrier gate: `--self` deliberately
    /// keeps the root's `entrypoints/` off PATH, so it must not claim those
    /// launchers either. The divergence this locks out surfaced through
    /// `ocx package inspect --closure`, which listed the root's `app` launcher
    /// on the private surface.
    #[tokio::test]
    async fn compose_root_entrypoints_are_interface_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "app",
        ));

        let synth_path_emitted = |out: &super::ComposeOutput| {
            out.entries
                .iter()
                .any(|e| e.key == "PATH" && e.value.contains("entrypoints"))
        };

        let consumer = compose(
            std::slice::from_ref(&root),
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        .unwrap();
        assert_eq!(
            consumer.admitted_entrypoints.len(),
            1,
            "root launcher must be claimed on the interface surface"
        );
        assert!(
            synth_path_emitted(&consumer),
            "interface surface must put the root's entrypoints/ on PATH"
        );

        let self_view = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(
            self_view.admitted_entrypoints.is_empty(),
            "root launcher must not be claimed under --self: {:?}",
            self_view.admitted_entrypoints
        );
        assert!(
            !synth_path_emitted(&self_view),
            "--self must not put the root's entrypoints/ on PATH"
        );
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
        let out = compose(&[a, b], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(
            out.entries.is_empty(),
            "no env vars + no entrypoints declared; composed env must be empty regardless of dedup"
        );
    }

    /// Diamond dep declaring both `binaries` and `entrypoints`, shared by two
    /// roots: each claim must be admitted exactly once, not once per root.
    ///
    /// Same shared-dep shape as `compose_multi_root_diamond_dep_emitted_once`,
    /// but asserts on `admitted_binaries`/`admitted_entrypoints` instead of
    /// `entries` — the cross-root dedup applies identically to claim
    /// attribution (`adr_declared_binaries_metadata.md` §4 Decision A).
    #[tokio::test]
    async fn compose_multi_root_diamond_dep_claims_emitted_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let c_id = pinned("c", 'c');
        let pkg_path = store.path(&c_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "binaries": ["ctool"],
            "entrypoints": { "ctool": {} },
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();

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

        let out = compose(&[a, b], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let binary_claims: Vec<_> = out.admitted_binaries.iter().filter(|(id, _)| *id == c_id).collect();
        let entrypoint_claims: Vec<_> = out.admitted_entrypoints.iter().filter(|(id, _)| *id == c_id).collect();
        assert_eq!(
            binary_claims.len(),
            1,
            "shared dep's binaries claim must be admitted exactly once across roots: {:?}",
            out.admitted_binaries
        );
        assert_eq!(
            entrypoint_claims.len(),
            1,
            "shared dep's entrypoints claim must be admitted exactly once across roots: {:?}",
            out.admitted_entrypoints
        );
    }

    /// One content selected under two tags is one admitted root, carrying the
    /// tag of whichever came first in selection order.
    #[tokio::test]
    async fn compose_one_content_under_two_tags_admits_the_first_tag() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let tagged = |tag: &str| {
            let id = PackageRef::new_registry("java", REGISTRY)
                .clone_with_tag(tag)
                .clone_with_digest(sha256('a'));
            PinnedPackageRef::try_from(id).unwrap()
        };
        let first = Arc::new(make_install_info_for(tagged("21.0"), ResolvedPackage::new()));
        let second = Arc::new(make_install_info_for(tagged("21"), ResolvedPackage::new()));

        let out = compose(
            &[first, second],
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        .unwrap();

        assert_eq!(out.admitted, vec![tagged("21.0")]);
    }

    // ─ Repo-conflict (same repo, different digest — fatal) ─────────────────────

    /// Same repository with two different digests across two roots' interface
    /// surfaces is a fatal version conflict: a single environment cannot expose
    /// two versions of one package, so `compose` returns
    /// `Err(Dependency(Conflict))`.
    #[tokio::test]
    async fn compose_same_repo_conflicting_digest_errors() {
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

        match compose(&[a, b], &store, false, &crate::composer::ComposePaths::digest_only()).await {
            Err(crate::Error::Dependency(DependencyError::Conflict {
                repository,
                identifiers,
            })) => {
                assert_eq!(repository, ocx_oci::Repository::from(&*dep_v1));
                assert_eq!(identifiers, vec![dep_v1, dep_v2]);
            }
            Err(other) => panic!("expected Dependency(Conflict), got {other:?}"),
            Ok(_) => panic!("expected Err(Dependency(Conflict)), got Ok"),
        }
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // PRIVATE.has_interface()=false → skip in default exec.
        assert!(
            out.entries.is_empty(),
            "PRIVATE TC entry must be skipped in default exec"
        );
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
        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(out.entries.is_empty(), "no env vars on the dep, so output is empty");
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // PRIVATE.has_private()=true → emit dep contributions; dep has no
        // env vars so output is empty but visit happened.
        assert!(out.entries.is_empty(), "no env vars on the dep, so output is empty");
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // INTERFACE.has_private()=false → skip under --self.
        assert!(
            out.entries.is_empty(),
            "INTERFACE TC entry must be skipped under --self"
        );
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // Synth-PATH for root's entrypoints/ present in default exec.
        let path_entries: Vec<_> = out
            .entries
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // No synth-PATH in the --self output (root must not see its own launchers).
        let path_entries: Vec<_> = out
            .entries
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
            "entrypoints": { "cmake": {} },
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // PUBLIC.has_interface()=true → dep's synth-PATH emitted.
        let path_entries: Vec<_> = out
            .entries
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // dep's Interface-tagged var present.
        assert!(
            out.entries.iter().any(|e| e.key == "DEP_IFACE"),
            "dep's Interface var must be present: {:?}",
            out.entries.iter().map(|e| &e.key).collect::<Vec<_>>()
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // root's Interface var present in default exec.
        assert!(
            out.entries.iter().any(|e| e.key == "PKG_CONFIG_PATH"),
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // root's Private var absent in default exec.
        assert!(
            !out.entries.iter().any(|e| e.key == "PRIVATE_FLAG"),
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // root's Private var present under --self.
        assert!(
            out.entries.iter().any(|e| e.key == "PRIVATE_FLAG"),
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // root's Interface var absent under --self.
        assert!(
            !out.entries.iter().any(|e| e.key == "PKG_CONFIG_PATH"),
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // dep's contributions absent in consumer surface.
        assert!(out.entries.is_empty());
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
        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(
            out.entries.is_empty(),
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // sealed excluded.
        assert!(out.entries.is_empty());
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // PUBLIC.has_private()=true → leaf visited; no env vars, so empty.
        assert!(out.entries.is_empty());
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            id,
            metadata,
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: pkg_root },
        ));

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let keys: Vec<&str> = out.entries.iter().map(|e| e.key.as_str()).collect();
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root2");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            id,
            metadata,
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: pkg_root },
        ));

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let keys: Vec<&str> = out.entries.iter().map(|e| e.key.as_str()).collect();
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // No deps declare env vars or entrypoints, so output is empty.
        // The gating is observable via the load_object_data calls — sealed
        // and private deps should NOT be visited, while public and iface
        // SHOULD be. The visit happens via on-disk metadata lookup; this
        // test validates the path doesn't panic when sealed/private deps
        // are skipped (no lookup attempt).
        assert!(out.entries.is_empty());
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

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(out.entries.is_empty());
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

        let out = compose(&[a, b], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // shared's contributions emitted exactly once (cross-root dedup).
        let shared_count = out.entries.iter().filter(|e| e.key == "SHARED_VAR").count();
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
        let out = compose(&[], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(out.entries.is_empty(), "compose(&[], ...) must return empty Env");
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // ROOT_VAR present, no dep contributions.
        assert_eq!(
            out.entries.len(),
            1,
            "leaf root with one Public var must emit one entry"
        );
        assert_eq!(out.entries[0].key, "ROOT_VAR");
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
        use ocx_package::resolved_package::{ResolvedDependency, ResolvedPackage};

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
        use ocx_package::resolved_package::{ResolvedDependency, ResolvedPackage};

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
        use ocx_package::resolved_package::ResolvedPackage;

        // interface_env / private_env were proposed in an early draft (rejected).
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
            "entrypoints": { "e": {} },
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
            "entrypoints": { "e": {} },
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
            "entrypoints": { "e": {} },
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
            "entrypoints": { "e": {} },
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

        let result = compose(
            &[a.clone(), b.clone()],
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await;
        let err = match result {
            Ok(_) => panic!("expected EntrypointCollision, got Ok"),
            Err(e) => e,
        };
        let errs = match err {
            crate::error::Error::ResolveFailed(es) => es,
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

        let out = compose(
            &[a, b_root],
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        .unwrap();
        let keys: Vec<&str> = out.entries.iter().map(|e| e.key.as_str()).collect();
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

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        // dep's contributions appear before ROOT_OWN_VAR in the Env.
        let dep_pos = out
            .entries
            .iter()
            .position(|e| e.key == "DEP_VAR")
            .expect("DEP_VAR present");
        let root_pos = out
            .entries
            .iter()
            .position(|e| e.key == "ROOT_OWN_VAR")
            .expect("ROOT_OWN_VAR present");
        assert!(
            dep_pos < root_pos,
            "DEP_VAR (pos {dep_pos}) must come before ROOT_OWN_VAR (pos {root_pos})"
        );
    }

    /// Within each root, root's synth-PATH (entrypoints) entry is emitted
    /// AFTER root's declared envvars on the consumer surface.
    ///
    /// PATH semantics are last-prepended-wins, so emitting synth-PATH after
    /// the declared `bin/` PATH entry makes `entrypoints/` win lookup priority
    /// at runtime — entrypoint launchers shadow declared `bin/`. See
    /// acceptance test
    /// `test_synthetic_entrypoints_path_emitted_after_declared_bin`.
    #[tokio::test]
    async fn compose_root_synth_path_emitted_after_root_own_vars() {
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

        let entrypoints = Entrypoints::from_names(["mytool"]);

        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            root_id,
            metadata,
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: pkg_root },
        ));

        // Store is needed by compose but root has no deps, so it stays empty.
        let store = make_store(dir.path());

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();

        let var_pos = out
            .entries
            .iter()
            .position(|e| e.key == "ROOT_VAR")
            .expect("ROOT_VAR present");
        let path_pos = out
            .entries
            .iter()
            .position(|e| e.key == "PATH")
            .expect("synth-PATH entry present");
        assert!(
            var_pos < path_pos,
            "ROOT_VAR (pos {var_pos}) must come before synth-PATH (pos {path_pos})"
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

        let conflicts = collect_repo_digest_conflicts(&[a, b], surface_axes(false));
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

        let conflicts = collect_repo_digest_conflicts(&[a, b], surface_axes(false));
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
        let expected_repo = ocx_oci::Repository::from(&*d_v1);

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

        let conflicts = collect_repo_digest_conflicts(&[a, b], surface_axes(false));
        assert_eq!(
            conflicts,
            vec![DigestConflict {
                repository: expected_repo,
                identifiers: vec![d_v1.clone(), d_v2.clone()],
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
        let expected_repo = ocx_oci::Repository::from(&*d_v1);

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
            collect_repo_digest_conflicts(&[a.clone(), b.clone()], surface_axes(false)).is_empty(),
            "private deps must not collide on the interface surface"
        );
        // `--self` surface: private deps participate, conflict is reported.
        assert_eq!(
            collect_repo_digest_conflicts(&[a, b], surface_axes(true)),
            vec![DigestConflict {
                repository: expected_repo,
                identifiers: vec![d_v1.clone(), d_v2.clone()],
            }],
        );
    }

    /// Two tags of the same repository that resolve to the **same** digest are
    /// not a conflict — `check_repo_digest_conflicts` returns `Ok`. Guards the
    /// `test_env_same_digest_roots_ok` acceptance contract.
    #[test]
    fn same_digest_is_not_a_conflict() {
        // Same repo "d", same digest '1' — two references to one version.
        let one = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));
        let two = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));

        assert!(
            collect_repo_digest_conflicts(&[one.clone(), two.clone()], surface_axes(false)).is_empty(),
            "two references to the same digest must not be reported as a conflict"
        );
        assert!(check_repo_digest_conflicts(&[one, two], surface_axes(false)).is_ok());
    }

    /// Two explicit roots for the same repository at different digests are a
    /// version conflict — `check_repo_digest_conflicts` returns
    /// `Err(DependencyError::Conflict)` naming both identifiers. This is the
    /// root-vs-root case the user reported (e.g. `cmake:4.1 cmake:4`).
    #[test]
    fn conflicting_roots_are_fatal() {
        let d_v1 = pinned("d", '1');
        let d_v2 = pinned("d", '2');
        let expected_repo = ocx_oci::Repository::from(&*d_v1);

        let a = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));
        let b = Arc::new(make_install_info("d", '2', ResolvedPackage::new()));

        match check_repo_digest_conflicts(&[a, b], surface_axes(false)) {
            Err(DependencyError::Conflict {
                repository,
                identifiers,
            }) => {
                assert_eq!(repository, expected_repo);
                assert_eq!(identifiers, vec![d_v1, d_v2]);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    /// End-to-end: `compose` aborts with `Error::Dependency(Conflict)` when two
    /// roots collide on the same repository at different digests. Locks the
    /// behaviour behind `package env`/`exec`/`run`.
    #[tokio::test]
    async fn compose_errors_on_conflicting_roots() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let a = Arc::new(make_install_info("d", '1', ResolvedPackage::new()));
        let b = Arc::new(make_install_info("d", '2', ResolvedPackage::new()));

        match compose(&[a, b], &store, false, &crate::composer::ComposePaths::digest_only()).await {
            Err(crate::Error::Dependency(DependencyError::Conflict { identifiers, .. })) => {
                assert_eq!(identifiers.len(), 2, "both colliding versions must be named");
            }
            Err(other) => panic!("expected Dependency(Conflict), got {other:?}"),
            Ok(_) => panic!("expected Err(Dependency(Conflict)), got Ok"),
        }
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
        let out = compose(&[b, a], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();

        let a_var_count = out.entries.iter().filter(|e| e.key == "A_VAR").count();
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
    // Invariant: declared `bin/` PATH entry MUST appear at a lower index than
    // the synth-entrypoints PATH entry in `entries` so that, when a consumer
    // prepends each entry in order, `entrypoints/` ends up at the front of
    // PATH and wins lookup priority — entrypoint launchers shadow declared
    // `bin/`.
    //
    // The second test in each pair demonstrates that swapping the two pushes
    // inside the helper would produce a DIFFERENT ordering, proving the order is
    // load-bearing and that a reversal is detectable.

    /// `emit_dep_path_block` emits the declared `bin/` PATH entry before the synth-PATH.
    ///
    /// Construct a dep with both an entrypoint (so synth-PATH is emitted) and a
    /// declared PATH env var (simulating `bin/`). Assert that the declared PATH
    /// entry appears at a lower index in `entries` than the synth-PATH entry.
    #[test]
    fn emit_dep_path_block_declared_bin_precedes_synth_path() {
        let dir = tempfile::tempdir().unwrap();

        // Build dep metadata: one entrypoint + one public PATH var (the bin/).
        let entrypoints = Entrypoints::from_names(["tool"]);

        use ocx_package::metadata::env::{path::Path as EnvPath, var::Modifier};
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });

        let pkg_root = dir.path().join("dep");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let dep_pkg = ocx_store::file_structure::PackageDir { dir: pkg_root.clone() };

        let dep_content = pkg_root.join("content");
        let dep_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_dep_path_block(
            &dep_metadata,
            &dep_pkg,
            &dep_content,
            &dep_dep_contexts,
            surface_axes(false),
            ContentState::Materialized,
            &mut entries,
        )
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
            bin_idx < synth_idx,
            "declared bin/ PATH (index {bin_idx}) must precede synth-PATH (index {synth_idx}); \
             reversing would let bin/ win lookup priority over launchers. entries: {:?}",
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

        let entrypoints = Entrypoints::from_names(["tool"]);

        use ocx_package::metadata::env::{path::Path as EnvPath, var::Modifier};
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("dep2");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let dep_pkg = ocx_store::file_structure::PackageDir { dir: pkg_root.clone() };
        let dep_content = pkg_root.join("content");
        let dep_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_dep_path_block(
            &dep_metadata,
            &dep_pkg,
            &dep_content,
            &dep_dep_contexts,
            surface_axes(false),
            ContentState::Materialized,
            &mut entries,
        )
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

        // After the swap, synth-PATH must now be at the *lower* index.
        // (synth_idx < bin_idx after swap.) This proves that swapping breaks the invariant.
        let new_synth_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("entrypoints"))
            .unwrap();
        let new_bin_idx = entries
            .iter()
            .position(|e| e.key == "PATH" && e.value.contains("bin") && !e.value.contains("entrypoints"))
            .unwrap();

        // The swapped vector has synth BEFORE bin — the invariant is violated.
        assert!(
            new_synth_idx < new_bin_idx,
            "after swap, synth-PATH (index {new_synth_idx}) must precede bin/ (index {new_bin_idx}); \
             this confirms the swap reverses the invariant"
        );
    }

    /// `emit_root_path_block` emits the declared `bin/` PATH entry before the synth-PATH
    /// on the consumer (default exec, self_view=false) surface.
    #[test]
    fn emit_root_path_block_declared_bin_precedes_synth_path_consumer_surface() {
        let dir = tempfile::tempdir().unwrap();

        let entrypoints = Entrypoints::from_names(["rootool"]);

        use ocx_package::metadata::env::{path::Path as EnvPath, var::Modifier};
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root_dir = ocx_store::file_structure::PackageDir { dir: pkg_root.clone() };
        let root_content = pkg_root.join("content");
        let root_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_root_path_block(
            &root_metadata,
            &root_dir,
            &root_content,
            &root_dep_contexts,
            surface_axes(false), // consumer surface (default exec)
            ContentState::Materialized,
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
            bin_idx < synth_idx,
            "declared bin/ (index {bin_idx}) must precede synth-PATH (index {synth_idx}) \
             in emit_root_path_block output; entrypoint launchers must shadow declared bin/"
        );
    }

    /// `emit_root_path_block` does NOT emit synth-PATH on the `--self` surface.
    ///
    /// Under `self_view=true` the root must not see its own launchers —
    /// `entrypoints/` is suppressed. Only the declared env vars appear.
    #[test]
    fn emit_root_path_block_no_synth_path_on_self_surface() {
        let dir = tempfile::tempdir().unwrap();

        let entrypoints = Entrypoints::from_names(["rootool"]);

        use ocx_package::metadata::env::{path::Path as EnvPath, var::Modifier};
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints,
            integrations: Default::default(),
        });
        let pkg_root = dir.path().join("root_self");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root_dir = ocx_store::file_structure::PackageDir { dir: pkg_root.clone() };
        let root_content = pkg_root.join("content");
        let root_dep_contexts = std::collections::HashMap::new();

        let mut entries = Vec::new();
        emit_root_path_block(
            &root_metadata,
            &root_dir,
            &root_content,
            &root_dep_contexts,
            surface_axes(true), // --self surface
            ContentState::Materialized,
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

    // ── admitted_binaries / admitted_entrypoints (adr_declared_binaries_metadata.md §4 Decision A) ──
    //
    // Same admission rule as `admitted`: root claims unconditional, dep
    // claims gated by the active surface (has_interface()/has_private()).
    // The entrypoints-flavored tests below exercise the rule end-to-end
    // through the already-working `Entrypoints` machinery (no dependency on
    // WP1's still-stubbed `BinaryName`/`Binaries`); the binaries-flavored
    // tests pin the identical contract for the new `BinaryName` type.

    /// An explicit root's own declared entrypoints are admitted
    /// unconditionally — mirrors `admitted`'s unconditional root emission.
    #[tokio::test]
    async fn compose_admitted_entrypoints_includes_root_claims_unconditionally() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root = Arc::new(make_install_info_with_ep(
            dir.path(),
            "root",
            'r',
            ResolvedPackage::new(),
            "cmake",
        ));
        let root_id = root.identifier().clone();

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let claimed: Vec<&str> = out
            .admitted_entrypoints
            .iter()
            .filter(|(id, _)| *id == root_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["cmake"],
            "an explicit root's own entrypoint claims must be admitted unconditionally"
        );
    }

    /// A dep whose TC entry has `has_interface()==true` (PUBLIC) contributes
    /// its declared entrypoints to `admitted_entrypoints` in default exec.
    #[tokio::test]
    async fn compose_admitted_entrypoints_includes_interface_visible_dep() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_id = pinned("ninja", 'n');
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({"type": "bundle", "version": 1, "entrypoints": { "ninja": {} }});
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
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
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let claimed: Vec<&str> = out
            .admitted_entrypoints
            .iter()
            .filter(|(id, _)| *id == dep_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["ninja"],
            "PUBLIC.has_interface()==true dep's declared entrypoints must be admitted"
        );
    }

    /// PRIVATE and SEALED deps' declared entrypoints never reach
    /// `admitted_entrypoints` on the default (interface) surface — both fail
    /// `has_interface()`, so the per-root loop never even visits them.
    #[tokio::test]
    async fn compose_admitted_entrypoints_excludes_private_and_sealed_dep_default_exec() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let sealed_id = pinned("sealed-tool", 's');
        for (id, name) in [(&priv_id, "privtool"), (&sealed_id, "sealedtool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "entrypoints": { name: {} }});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: sealed_id.clone(),
                    visibility: Visibility::SEALED,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(
            out.admitted_entrypoints.is_empty(),
            "PRIVATE and SEALED deps' claims must never be admitted on the default (interface) surface: {:?}",
            out.admitted_entrypoints
        );
    }

    /// `--self` flips admission: a PRIVATE dep (has_private()==true) is
    /// admitted, an INTERFACE-only dep (has_private()==false) is excluded —
    /// the mirror image of the default-exec gate.
    #[tokio::test]
    async fn compose_admitted_entrypoints_self_view_flips_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let iface_id = pinned("iface-tool", 'i');
        for (id, name) in [(&priv_id, "privtool"), (&iface_id, "ifacetool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "entrypoints": { name: {} }});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: iface_id.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let claimed: Vec<&str> = out.admitted_entrypoints.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(
            claimed,
            vec!["privtool"],
            "--self must admit PRIVATE.has_private()==true deps and exclude INTERFACE.has_private()==false deps"
        );
    }

    /// Same unconditional-root rule as the entrypoints test above, pinned
    /// for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_includes_root_claims_unconditionally() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let root_meta: metadata::Metadata =
            serde_json::from_str(r#"{"type":"bundle","version":1,"binaries":["cmake"]}"#).expect("fixture parses");
        let root_id = pinned("root", 'r');
        let pkg_root = dir.path().join("root");
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            root_meta,
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: pkg_root },
        ));

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let claimed: Vec<&str> = out
            .admitted_binaries
            .iter()
            .filter(|(id, _)| *id == root_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["cmake"],
            "an explicit root's declared binaries must be admitted unconditionally"
        );
    }

    /// Same interface-surface gate as the entrypoints test above, pinned
    /// for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_dep_gated_by_interface_surface() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_id = pinned("ninja", 'n');
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({"type": "bundle", "version": 1, "binaries": ["ninja"]});
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
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
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let claimed: Vec<&str> = out
            .admitted_binaries
            .iter()
            .filter(|(id, _)| *id == dep_id)
            .map(|(_, name)| name.as_str())
            .collect();
        assert_eq!(
            claimed,
            vec!["ninja"],
            "PUBLIC.has_interface()==true dep's declared binaries must be admitted"
        );
    }

    /// Same PRIVATE/SEALED exclusion as `compose_admitted_entrypoints_excludes_private_and_sealed_dep_default_exec`,
    /// pinned for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_excludes_private_and_sealed_dep_default_exec() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let sealed_id = pinned("sealed-tool", 's');
        for (id, name) in [(&priv_id, "privtool"), (&sealed_id, "sealedtool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "binaries": [name]});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: sealed_id.clone(),
                    visibility: Visibility::SEALED,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(
            out.admitted_binaries.is_empty(),
            "PRIVATE and SEALED deps' binaries claims must never be admitted on the default (interface) surface: {:?}",
            out.admitted_binaries
        );
    }

    /// Same `--self` admission flip as `compose_admitted_entrypoints_self_view_flips_admission`,
    /// pinned for the new `BinaryName`-typed `admitted_binaries` array.
    #[tokio::test]
    async fn compose_admitted_binaries_self_view_flips_admission() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let priv_id = pinned("priv-tool", 'p');
        let iface_id = pinned("iface-tool", 'i');
        for (id, name) in [(&priv_id, "privtool"), (&iface_id, "ifacetool")] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({"type": "bundle", "version": 1, "binaries": [name]});
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: priv_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
                ResolvedDependency {
                    identifier: iface_id.clone(),
                    visibility: Visibility::INTERFACE,
                },
            ],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        let out = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        let claimed: Vec<&str> = out.admitted_binaries.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(
            claimed,
            vec!["privtool"],
            "--self must admit PRIVATE.has_private()==true deps and exclude INTERFACE.has_private()==false deps"
        );
    }

    /// A SEALED dependency and a PRIVATE dependency, each declaring both
    /// `binaries` and `entrypoints` claims, contribute nothing to either
    /// admitted-claim array — while the root's own (unrelated) env-var
    /// contribution still reaches `entries` in the same compose call. Guards
    /// `adr_declared_binaries_metadata.md` §4 Decision A: a non-interface
    /// dependency's claims never leak into the env report, and the exclusion
    /// does not collaterally swallow the root's own surface contribution.
    #[tokio::test]
    async fn compose_sealed_and_private_dep_claims_excluded_while_root_contributes() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let sealed_id = pinned("sealed-tool", 's');
        let private_id = pinned("private-tool", 'p');
        for id in [&sealed_id, &private_id] {
            let pkg_path = store.path(id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            let meta = serde_json::json!({
                "type": "bundle",
                "version": 1,
                "binaries": ["secret"],
                "entrypoints": { "secret": {} },
            });
            std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        let root_resolved = ResolvedPackage {
            dependencies: vec![
                ResolvedDependency {
                    identifier: sealed_id.clone(),
                    visibility: Visibility::SEALED,
                },
                ResolvedDependency {
                    identifier: private_id.clone(),
                    visibility: Visibility::PRIVATE,
                },
            ],
        };
        let root = Arc::new(make_install_info_with_var(
            dir.path(),
            "root",
            'r',
            root_resolved,
            "OWN_VAR",
            Visibility::PUBLIC,
        ));

        let out = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();

        assert!(
            out.admitted_binaries.is_empty(),
            "SEALED and PRIVATE deps' binaries claims must never be admitted: {:?}",
            out.admitted_binaries
        );
        assert!(
            out.admitted_entrypoints.is_empty(),
            "SEALED and PRIVATE deps' entrypoints claims must never be admitted: {:?}",
            out.admitted_entrypoints
        );
        assert!(
            out.entries.iter().any(|e| e.key == "OWN_VAR"),
            "the root's own env-var contribution must still reach entries, unaffected by dep claim exclusion: {:?}",
            out.entries
        );
    }

    // ── `${self.env.KEY}` — scope, order, and the resolve-then-gate split ─────
    //
    // Every test below builds a package whose `env` array IS the fixture: the
    // declaration order of `vars` is the property under test, so the helper
    // preserves it and never sorts.

    use std::collections::HashMap;

    use ocx_package::error::Error as PackageError;
    use ocx_package::metadata::dependency::DependencyName;
    use ocx_package::metadata::env::dep_context::DependencyContext;
    use ocx_package::metadata::env::entry::Entry;
    use ocx_package::metadata::template::TemplateError;

    type DepContexts = HashMap<DependencyName, DependencyContext>;

    /// Package metadata carrying exactly `vars`, in the order given.
    fn metadata_with_vars(vars: Vec<Var>) -> metadata::Metadata {
        let mut builder = metadata_env::EnvBuilder::new();
        for var in vars {
            builder.add_var(var);
        }
        metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: builder.build(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: metadata::Integrations::default(),
        })
    }

    /// A package directory with a real `content/` tree, so path resolution has
    /// something to root itself in.
    fn package_content(root: &std::path::Path, name: &str) -> std::path::PathBuf {
        let content = root.join(name).join("content");
        std::fs::create_dir_all(&content).unwrap();
        content
    }

    /// Compose one root package's own env onto the surface `self_view` selects.
    fn compose_root(
        meta: &metadata::Metadata,
        content: &std::path::Path,
        dep_contexts: &DepContexts,
        self_view: bool,
    ) -> crate::Result<Vec<Entry>> {
        let root_dir = ocx_store::file_structure::PackageDir {
            dir: content.parent().unwrap().to_path_buf(),
        };
        let mut entries = Vec::new();
        emit_root_path_block(
            meta,
            &root_dir,
            content,
            dep_contexts,
            surface_axes(self_view),
            ContentState::Materialized,
            &mut entries,
        )?;
        Ok(entries)
    }

    /// Compose one dependency's env across the edge onto the surface
    /// `self_view` selects.
    fn compose_dep(meta: &metadata::Metadata, content: &std::path::Path, self_view: bool) -> crate::Result<Vec<Entry>> {
        let dep_pkg = ocx_store::file_structure::PackageDir {
            dir: content.parent().unwrap().to_path_buf(),
        };
        let dep_contexts = DepContexts::new();
        let mut entries = Vec::new();
        emit_dep_path_block(
            meta,
            &dep_pkg,
            content,
            &dep_contexts,
            surface_axes(self_view),
            ContentState::Materialized,
            &mut entries,
        )?;
        Ok(entries)
    }

    fn value_of<'a>(entries: &'a [Entry], key: &str) -> &'a str {
        entries
            .iter()
            .find(|entry| entry.key == key)
            .unwrap_or_else(|| panic!("expected an entry for {key}; got {entries:?}"))
            .value
            .as_str()
    }

    // ─ Declaration order ────────────────────────────────

    /// First leg — a var may reference one declared strictly
    /// earlier in the same package, and gets its resolved value.
    ///
    /// Paired with
    /// `the_same_two_vars_in_the_opposite_declaration_order_are_refused`: the
    /// two documents are identical modulo the order of the `env` array, so an
    /// implementation that ignores order gives them the same verdict and one
    /// of the two legs reds. Neither leg pins order on its own.
    #[test]
    fn a_var_may_reference_one_declared_earlier_in_the_same_package() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "forward");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "alpha", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
        ]);

        let entries = compose_root(&meta, &content, &DepContexts::new(), false).expect("the document must compose");
        assert_eq!(value_of(&entries, "A"), "alpha");
        assert_eq!(value_of(&entries, "B"), "alpha/x");
    }

    /// Second leg — the same two vars, same keys, same values, only
    /// the array order swapped: the reference now points forward and is
    /// refused.
    #[test]
    fn the_same_two_vars_in_the_opposite_declaration_order_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "backward");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
            Var::new_constant_with_visibility("A", "alpha", Visibility::PUBLIC),
        ]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("a forward reference names a var that is not yet in scope");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    var_key,
                    source: TemplateError::UndefinedSelfEnvRef { key, .. },
                } if var_key == "B" && key == "A"
            )),
            "unexpected error: {error}"
        );
    }

    /// A var referencing itself is the same fault, reached through a
    /// one-var document: its own declaration is not strictly earlier than
    /// itself, so the scope is empty and there is no cycle to detect.
    #[test]
    fn a_var_referencing_itself_is_undefined_rather_than_a_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "self-ref");
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "A",
            "${self.env.A}",
            Visibility::PUBLIC,
        )]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("a var cannot see its own declaration");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UndefinedSelfEnvRef { key, declared_before }, ..
                } if key == "A" && declared_before.is_empty()
            )),
            "unexpected error: {error}"
        );
    }

    /// First leg — a key declared twice earlier is refused, not
    /// picked: both contributions are legally visible and neither is
    /// privileged.
    #[test]
    fn a_key_declared_twice_earlier_makes_the_reference_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "ambiguous");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "one", Visibility::PUBLIC),
            Var::new_constant_with_visibility("A", "two", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}", Visibility::PUBLIC),
        ]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("two earlier contributions leave no non-arbitrary answer");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::AmbiguousSelfEnvRef { key }, ..
                } if key == "A"
            )),
            "unexpected error: {error}"
        );
    }

    /// Second leg — duplicates stay legal. Only *referencing* an
    /// ambiguous key is refused; without this leg the refusal above is
    /// indistinguishable from a new uniqueness rule on `Env`.
    #[test]
    fn a_key_declared_twice_with_no_reference_to_it_composes_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "duplicates");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "one", Visibility::PUBLIC),
            Var::new_constant_with_visibility("A", "two", Visibility::PUBLIC),
        ]);

        let entries =
            compose_root(&meta, &content, &DepContexts::new(), false).expect("duplicate keys remain publishable");
        let values: Vec<&str> = entries
            .iter()
            .filter(|entry| entry.key == "A")
            .map(|entry| entry.value.as_str())
            .collect();
        assert_eq!(
            values,
            vec!["one", "two"],
            "both contributions must still be emitted, in declaration order"
        );
    }

    // ─ Surface independence ──────────────────────────────────────

    /// An `interface` var may reference a `private` one, and the
    /// resolved bytes are identical on both surfaces.
    ///
    /// The fixture is a dependency, where `carrier_crosses` is
    /// `has_interface()` on either surface: `I` crosses both times and `S`
    /// crosses neither, so the two runs differ only in the surface asked for.
    ///
    /// The literal value assertion is not redundant with the ordering check: an
    /// implementation resolving `${self.env.S}` to the empty string on **both**
    /// surfaces satisfies equality perfectly, so equality alone cannot tell
    /// surface-independence from uniformly-degenerate.
    #[test]
    fn an_interface_var_referencing_a_private_one_resolves_identically_on_both_surfaces() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "surfaces");
        let build = || {
            metadata_with_vars(vec![
                Var::new_constant_with_visibility("S", "private-value", Visibility::PRIVATE),
                Var::new_constant_with_visibility("I", "${self.env.S}", Visibility::INTERFACE),
            ])
        };

        let interface_surface = compose_dep(&build(), &content, false).expect("the interface surface must compose");
        let private_surface = compose_dep(&build(), &content, true).expect("the private surface must compose");

        assert_eq!(
            value_of(&interface_surface, "I"),
            value_of(&private_surface, "I"),
            "the same metadata must not produce different bytes depending on who asked"
        );
        assert_eq!(
            value_of(&interface_surface, "I"),
            "private-value",
            "the agreed value must be the referenced var's own resolved value"
        );

        for (surface, entries) in [("interface", &interface_surface), ("private", &private_surface)] {
            assert!(
                !entries.iter().any(|entry| entry.key == "S"),
                "a dep's private var crosses no edge, so it must not be emitted on the {surface} surface: {entries:?}"
            );
        }
    }

    // ─ Resolve, then gate: assertions on emit only ────────

    /// Case (a) — a `required` path var whose target is absent and that does
    /// **not** cross the active surface is resolved but not asserted.
    ///
    /// This leg cannot red against the stub, and it cannot red against `main`
    /// either: today the var is `continue`d before `EnvResolver::resolve` runs,
    /// so the assertion never fires. Its red state exists only against
    /// resolve-then-gate code that left the existence assertion on the
    /// non-emitted path — which is exactly the regression resolve-then-gate would otherwise
    /// introduce. `..._that_crosses_is_asserted_to_exist` is what makes the
    /// pair a check: without it, deleting the assertion outright passes here.
    #[test]
    fn a_missing_required_path_that_does_not_cross_is_not_asserted_to_exist() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "required-private");
        let meta = metadata_with_vars(vec![Var::new_path_with_visibility(
            "TOOL_DIR",
            "${installPath}/absent",
            /* required = */ true,
            Visibility::PRIVATE,
        )]);

        let entries = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect("a var nobody emits must not assert its target exists");
        assert!(
            entries.is_empty(),
            "a private var does not cross the interface surface: {entries:?}"
        );
    }

    /// Case (b) — the otherwise identical `required` path var that **does**
    /// cross still raises `RequiredPathMissing`. This leg carries the pair's
    /// discrimination.
    #[test]
    fn a_missing_required_path_that_crosses_is_asserted_to_exist() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "required-interface");
        let meta = metadata_with_vars(vec![Var::new_path_with_visibility(
            "TOOL_DIR",
            "${installPath}/absent",
            /* required = */ true,
            Visibility::INTERFACE,
        )]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("an emitted required path must be asserted to exist");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(), PackageError::RequiredPathMissing(_))),
            "unexpected error: {error}"
        );
    }

    /// A dependency context whose install path does not exist on disk — the
    /// declared-but-not-installed case `build_dep_context_map` produces when a
    /// declared dep is absent from the resolved toolchain.
    fn uninstalled_dep_contexts(missing: std::path::PathBuf) -> DepContexts {
        let mut contexts = DepContexts::new();
        contexts.insert(
            DependencyName::try_from("tool").unwrap(),
            DependencyContext::path_only(pinned("tool", 'e'), missing),
        );
        contexts
    }

    /// Case (a) — a var referencing a declared-but-uninstalled dependency
    /// composes cleanly when it does not cross the active surface.
    ///
    /// Same standing as the path-var case (a): green today because the var is never
    /// resolved at all, and red only against resolve-then-gate code that kept
    /// `check_exists = true` on the non-emitted path — which would turn a
    /// working install into exit 79. The crossing sibling below is what makes
    /// the pair a check.
    #[test]
    fn an_uninstalled_dependency_in_a_non_crossing_var_does_not_fail_composition() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "dep-private");
        let contexts = uninstalled_dep_contexts(dir.path().join("not-installed"));
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "TOOL",
            "${deps.tool.installPath}/bin",
            Visibility::PRIVATE,
        )]);

        let entries = compose_root(&meta, &content, &contexts, false)
            .expect("an uninstalled dep must not fail a value nobody emits");
        assert!(
            entries.is_empty(),
            "a private var does not cross the interface surface: {entries:?}"
        );
    }

    /// Case (b) — the otherwise identical var that **does** cross fails with
    /// `DependencyNotInstalled`, exit 79.
    #[test]
    fn an_uninstalled_dependency_in_a_crossing_var_fails_composition() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "dep-interface");
        let contexts = uninstalled_dep_contexts(dir.path().join("not-installed"));
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "TOOL",
            "${deps.tool.installPath}/bin",
            Visibility::INTERFACE,
        )]);

        let error = compose_root(&meta, &content, &contexts, false)
            .expect_err("an emitted value must still assert its dependency is installed");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::DependencyNotInstalled { ref_name, .. }, ..
                } if ref_name.as_str() == "tool"
            )),
            "unexpected error: {error}"
        );
    }

    /// Resolve-then-gate — a template *fault* in a non-crossing var now surfaces where it
    /// previously never ran. This is the accepted behaviour change, and it is
    /// what keeps the two suppression legs above from reading as "a
    /// non-crossing var is never resolved at all".
    #[test]
    fn a_template_fault_in_a_non_crossing_var_still_fails_composition() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "fault-private");
        let meta = metadata_with_vars(vec![Var::new_constant_with_visibility(
            "BROKEN",
            "${deps.undeclared.installPath}",
            Visibility::PRIVATE,
        )]);

        let error = compose_root(&meta, &content, &DepContexts::new(), false)
            .expect_err("a package whose own metadata cannot resolve is broken regardless of who is looking");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UnknownDependencyRef { ref_name, .. }, ..
                } if ref_name.as_str() == "undeclared"
            )),
            "unexpected error: {error}"
        );
    }

    // ─ What is substituted: the resolved value, never the template ───

    /// Composition substitutes the referenced var's **resolved value**,
    /// not its template.
    ///
    /// The red state is a mutant of the code WP4 adds: an accumulator holding
    /// each var's authored template instead of its resolved `Entry`. `A`'s
    /// template and `A`'s resolved value differ visibly here, so the mutant is
    /// caught by the assertion rather than by an equality that both satisfy.
    #[test]
    fn a_self_env_reference_substitutes_the_resolved_value_not_the_template() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "resolved-value");
        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "${installPath}/bin", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
        ]);

        let entries = compose_root(&meta, &content, &DepContexts::new(), false).expect("the document must compose");
        let a = value_of(&entries, "A");
        let b = value_of(&entries, "B");

        assert_ne!(
            a, "${installPath}/bin",
            "the fixture is only a check if A's template and A's resolved value differ"
        );
        assert_eq!(b, format!("{a}/x"), "B must carry A's resolved value");
        assert!(
            !b.contains("${installPath}"),
            "substituting A's template would leave an unresolved token in B: {b:?}"
        );
    }

    /// Sibling — bytes a `${self.env.*}` reference substitutes
    /// are never rescanned.
    ///
    /// `A` resolves, through the escape, to the literal text
    /// `${deps.tool.installPath}`, and `tool` is present in `dep_contexts`. If
    /// composition re-read substituted bytes, `B` would come out carrying the
    /// dependency's install path. Dropping the install-path injection
    /// defence on the grounds that substituted bytes are never re-examined;
    /// `${self.env.*}` is the second composition path where that premise could
    /// be falsified.
    #[test]
    fn bytes_a_self_env_reference_substitutes_are_never_rescanned() {
        let dir = tempfile::tempdir().unwrap();
        let content = package_content(dir.path(), "injection");
        let installed = dir.path().join("tool-installed");
        std::fs::create_dir_all(&installed).unwrap();
        let mut contexts = DepContexts::new();
        contexts.insert(
            DependencyName::try_from("tool").unwrap(),
            DependencyContext::path_only(pinned("tool", 'f'), installed.clone()),
        );

        let meta = metadata_with_vars(vec![
            Var::new_constant_with_visibility("A", "$${deps.tool.installPath}", Visibility::PUBLIC),
            Var::new_constant_with_visibility("B", "${self.env.A}/x", Visibility::PUBLIC),
        ]);

        let entries = compose_root(&meta, &content, &contexts, false).expect("the document must compose");
        assert_eq!(
            value_of(&entries, "B"),
            "${deps.tool.installPath}/x",
            "substituted bytes must reach the output verbatim"
        );
        assert!(
            !value_of(&entries, "B").contains(&*installed.to_string_lossy()),
            "the dependency's install path must not appear — that would mean the output was rescanned"
        );
    }

    // ── Integrations_cross — interface-surface-only carrier ────────
    //
    // ADR `adr_package_integrations.md` §4.1's four-cell truth table.
    // `integrations_cross` takes only `self_view` — no `is_root` — because
    // the answer is the same at every depth (a lie a parameter the body
    // ignores would tell); the root and dep cells below therefore exercise
    // the identical call, documenting all four ADR rows explicitly.

    #[test]
    fn integrations_cross_root_interface_surface_is_true() {
        assert!(integrations_cross(/* self_view = */ false));
    }

    #[test]
    fn integrations_cross_root_private_surface_is_false() {
        assert!(!integrations_cross(/* self_view = */ true));
    }

    #[test]
    fn integrations_cross_dep_interface_surface_is_true() {
        assert!(integrations_cross(/* self_view = */ false));
    }

    #[test]
    fn integrations_cross_dep_private_surface_is_false() {
        assert!(!integrations_cross(/* self_view = */ true));
    }

    // ── Surface mask ≡ bool surface ──────────────────────────────

    /// The single-axis mask of a surface admits and crosses exactly what the bool surface does,
    /// over every visibility × depth × surface; the expected column is the bool rule spelled out.
    #[test]
    fn surface_mask_matches_bool_surface_for_every_visibility() {
        let visibilities = [
            Visibility::SEALED,
            Visibility::PRIVATE,
            Visibility::INTERFACE,
            Visibility::PUBLIC,
        ];
        for visibility in visibilities {
            for self_view in [false, true] {
                let axes = surface_axes(self_view);
                let on_surface = if self_view {
                    visibility.has_private()
                } else {
                    visibility.has_interface()
                };
                assert_eq!(
                    dep_admitted(visibility, self_view),
                    on_surface,
                    "{visibility:?} {self_view}"
                );
                assert_eq!(
                    dep_admitted_on(visibility, axes),
                    on_surface,
                    "{visibility:?} {self_view}"
                );
                for is_root in [false, true] {
                    let expected = if is_root {
                        on_surface
                    } else {
                        visibility.has_interface()
                    };
                    assert_eq!(
                        carrier_crosses(visibility, is_root, self_view),
                        expected,
                        "{visibility:?} root={is_root} {self_view}"
                    );
                    assert_eq!(
                        carrier_crosses_on(visibility, is_root, axes),
                        expected,
                        "{visibility:?} root={is_root} {self_view}"
                    );
                }
            }
        }
    }

    /// A two-axis mask is the union of its surfaces: a root carrier or dependency on either axis passes.
    #[test]
    fn a_public_mask_is_the_union_of_both_surfaces() {
        for visibility in [Visibility::PRIVATE, Visibility::INTERFACE, Visibility::PUBLIC] {
            assert!(dep_admitted_on(visibility, Visibility::PUBLIC), "{visibility:?}");
            assert!(
                carrier_crosses_on(visibility, true, Visibility::PUBLIC),
                "{visibility:?}"
            );
        }
        assert!(!dep_admitted_on(Visibility::SEALED, Visibility::PUBLIC));
        assert!(!carrier_crosses_on(Visibility::PRIVATE, false, Visibility::PUBLIC));
    }

    // ── The companion projection's integrations gate ──────────

    /// A companion projection composed with integrations SUPPRESSED must not
    /// resolve the payloads at all — not resolve-then-discard.
    ///
    /// The projection composes on its targets' surface, not the caller's, so it
    /// cannot derive the caller's gate; before the explicit input it collected
    /// unconditionally. Resolution asserts every `${deps.*}` content directory
    /// exists, so a payload naming an uninstalled dependency failed the WHOLE
    /// projection — on a `--self` composition that carries zero integrations.
    ///
    /// Both outcomes are demonstrated on the one fixture: the gate ON leg proves
    /// the payload really is resolved (and really can fail), so the gate OFF
    /// leg's success is suppression rather than an inert fixture.
    #[tokio::test]
    async fn compose_companion_with_integrations_suppressed_skips_payload_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        // Declared, never installed: `store.content(..)` names a directory that
        // does not exist, which is exactly what `${deps.*}` resolution asserts.
        let absent_dep = pinned("absentdep", 'd');
        // `from_str`, not `from_value`: `Visibility` deserializes from a
        // borrowed string, which `serde_json::Value` cannot supply.
        let metadata: metadata::Metadata = serde_json::from_str(
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "dependencies": [{ "identifier": absent_dep.to_string(), "visibility": "public" }],
                "integrations": { "vendor.example": { "path": "${deps.absentdep.installPath}" } },
            })
            .to_string(),
        )
        .expect("fixture metadata parses");
        let companion = Arc::new(InstallInfo::new(
            pinned("companion", 'c'),
            metadata,
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir {
                dir: dir.path().join("companion"),
            },
        ));

        let collected = compose_companion(
            &companion,
            &store,
            Visibility::INTERFACE,
            /* collect_integrations = */ true,
            &mut Default::default(),
        )
        .await;
        assert!(
            collected.is_err(),
            "fixture check: collecting integrations must resolve the payload and fail on the absent dependency"
        );

        let suppressed = compose_companion(
            &companion,
            &store,
            Visibility::INTERFACE,
            /* collect_integrations = */ false,
            &mut Default::default(),
        )
        .await
        .expect("a suppressed carrier must not be resolved, so the absent dependency cannot fail the projection");
        assert!(
            suppressed.admitted_integrations.is_empty(),
            "suppressed projection must carry no integrations: {:?}",
            suppressed.admitted_integrations
        );
    }

    // ── A companion's closure dedups against what is already emitted ──

    /// A companion dependency that is also a base root is not emitted a second time.
    ///
    /// The base emits the root on its followed link while the companion would emit the digest
    /// root, so the two entries differ and only package identity can tell they are one package.
    #[tokio::test]
    async fn a_companion_dependency_that_is_a_base_root_on_a_followed_link_is_not_emitted_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let shared_id = pinned("shared", 's');
        let shared_json = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{ "key": "SHARED_HOME", "type": "constant", "value": "${installPath}", "visibility": "public" }],
        })
        .to_string();
        let shared_dir = store.path(&shared_id);
        std::fs::create_dir_all(shared_dir.join("content")).unwrap();
        std::fs::write(shared_dir.join("metadata.json"), &shared_json).unwrap();
        std::fs::write(
            shared_dir.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();
        let base_root = Arc::new(InstallInfo::new(
            shared_id.clone(),
            serde_json::from_str::<metadata::Metadata>(&shared_json).unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir {
                dir: shared_dir.clone(),
            },
        ));

        let link = dir.path().join("toolchain").join("shared");
        let paths = crate::composer::ComposePaths {
            trusted: std::collections::HashMap::from([(shared_dir.clone(), link.clone())]),
        };
        let base = compose(&[base_root], &store, false, &paths).await.unwrap();
        let base_value = base
            .entries
            .iter()
            .find(|entry| entry.key == "SHARED_HOME")
            .map(|entry| entry.value.clone())
            .expect("premise: the base emits the shared root");
        assert!(
            base_value.starts_with(&*link.to_string_lossy()),
            "premise: the base root is emitted on its followed link, got {base_value}"
        );

        let companion_json = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{ "key": "COMPANION_VAR", "type": "constant", "value": "on", "visibility": "interface" }],
        })
        .to_string();
        let companion = Arc::new(InstallInfo::new(
            pinned("companion", 'c'),
            serde_json::from_str::<metadata::Metadata>(&companion_json).unwrap(),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: shared_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
            ocx_store::file_structure::PackageDir {
                dir: dir.path().join("companion"),
            },
        ));

        let mut emitted: std::collections::HashSet<PinnedPackageRef> =
            base.admitted.iter().map(PinnedPackageRef::strip_advisory).collect();
        let out = compose_companion(&companion, &store, Visibility::INTERFACE, true, &mut emitted)
            .await
            .unwrap();

        assert!(
            out.entries.iter().any(|entry| entry.key == "COMPANION_VAR"),
            "positive control: the companion's own var composes; entries: {:?}",
            out.entries
        );
        assert!(
            !out.entries.iter().any(|entry| entry.key == "SHARED_HOME"),
            "a dependency the base already emitted as a root must not be emitted again; entries: {:?}",
            out.entries
        );
    }

    // ── H-2: `${self.env.*}` is gated at COMPOSE, not only at publish ────────
    //
    // `validate_integration_tokens` runs inside `validate_for_publish`, which
    // a hostile registry never runs — so compose meets a published payload's
    // tokens with only its own capability set. Both resolver sites therefore
    // carry `INTEGRATION_TOKENS`, one test each, because they are two
    // independent call sites that can regress separately.
    //
    // Each asserts WHICH refusal, not merely that one happened: the composer
    // supplies no self-env scope, so an ungated resolver also fails here — as
    // `UndefinedSelfEnvRef`, indistinguishable from a gate unless the message is
    // read. That coincidence is one edit (`.with_self_env(&declared_before)`
    // "for consistency") away from resolving instead, which would put a value
    // the publisher declared `private` into an interface-surface JSON payload
    // (CWE-200). `integrations.rs` holds the sibling unit test that supplies a
    // scope which DOES define the key, so the gate is proven independent of it.

    /// Dependency site: a dep's payload carrying `${self.env.*}` is refused by
    /// the capability gate.
    #[tokio::test]
    async fn compose_gates_a_self_env_token_in_a_dependency_integrations_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let dep_id = pinned("dep", 'd');
        let dep_resolved = ResolvedPackage::new();
        let pkg_path = store.path(&dep_id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "integrations": { "vendor.example": { "token": "${self.env.SECRET}" } },
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&dep_resolved).unwrap(),
        )
        .unwrap();

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id,
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info("root", 'r', root_resolved));

        // `let Err(..) else`, not `expect_err`: `ComposeOutput` is not `Debug`.
        let Err(err) = compose(
            &[root],
            &store,
            /* self_view = */ false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        else {
            panic!("a self-env token in a dependency's payload must be refused");
        };
        let message = err.to_string();
        assert!(
            message.contains("not permitted"),
            "expected the capability gate's refusal, not an undefined-reference one: {message}"
        );
    }

    /// Root site: a root's own payload carrying `${self.env.*}` is refused by
    /// the capability gate. Exercised through `compose_companion`, which reaches
    /// the root branch with no store seeding.
    #[tokio::test]
    async fn compose_gates_a_self_env_token_in_a_root_integrations_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let metadata: metadata::Metadata = serde_json::from_str(
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "integrations": { "vendor.example": { "token": "${self.env.SECRET}" } },
            })
            .to_string(),
        )
        .expect("fixture metadata parses");
        let root = Arc::new(InstallInfo::new(
            pinned("root", 'r'),
            metadata,
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir {
                dir: dir.path().join("root"),
            },
        ));

        let Err(err) = compose_companion(
            &root,
            &store,
            Visibility::INTERFACE,
            /* collect_integrations = */ true,
            &mut Default::default(),
        )
        .await
        else {
            panic!("a self-env token in the root's own payload must be refused");
        };
        let message = err.to_string();
        assert!(
            message.contains("not permitted"),
            "expected the capability gate's refusal, not an undefined-reference one: {message}"
        );
    }

    // ── The composer's shim slot (lazy package loading) ─────────────────────
    //
    // Covers the `lazy-mode` ladders, the shim slot and its PATH position, an env
    // that is a function of the lock and the mode (never of content-cache state),
    // an advisory channel fed by the deferred branch only, a deferred root's
    // carriers read from ref-linked config blobs (never a package directory), and
    // `--no-pull` warning and omitting.

    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_oci::Platform;
    use ocx_package::install_info::DeferredComposition;
    use ocx_project::{Group, PackageSettings};

    // `lazy_mode_for_tool` is imported from `project`, not `super`: the ladder
    // decision puts the project-tier ladder assembler with the config it
    // reads. Its OCI-tier sibling reads no config and stays in this module.
    use ocx_project::{ProjectConfig, lazy_mode_for_tool, lazy_mode_ladder_for_tool};

    // `Entry` is already in scope from the `${self.env.KEY}` section above.
    use super::{
        ComposeOmission, ComposeRequest, ComposeRoots, Concurrency, LazyAdvisory, LazyMode, Materialization,
        ModifierKind, PackageManager, ShimDir, emit_shim_slot, lazy_mode_for_package, lazy_mode_ladder_for_package,
        synth_shim_path_for, tc_entry_object_data,
    };

    // ── Fixtures ────────────────────────────────────────────────────────────

    /// A shim directory rooted at `dir` — the shape `ShimStore::shim_dir`
    /// hands the composer.
    fn shim_dir_at(dir: std::path::PathBuf) -> ShimDir {
        ShimDir { dir }
    }

    /// Bundle metadata carrying `vars` and `entrypoint_names`.
    fn bundle_metadata(vars: Vec<Var>, entrypoint_names: &[&str]) -> metadata::Metadata {
        let mut builder = metadata_env::EnvBuilder::new();
        for var in vars {
            builder.add_var(var);
        }
        metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: builder.build(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::from_names(entrypoint_names.iter().copied()),
            integrations: metadata::Integrations::default(),
        })
    }

    /// A `path`-modifier env var — the carrier whose `required` leg the
    /// lock-only env decision suppresses for a deferred root.
    fn path_var(key: &str, value: &str, required: bool, visibility: Visibility) -> Var {
        Var {
            key: key.to_string(),
            modifier: Modifier::Path(ocx_package::metadata::env::path::Path {
                required,
                value: value.to_string(),
            }),
            visibility,
        }
    }

    /// A `constant`-modifier env var, used purely as a source marker so a test
    /// can tell WHICH carrier source answered.
    fn marker_var(key: &str) -> Var {
        Var {
            key: key.to_string(),
            modifier: Modifier::Constant(metadata_env::constant::Constant {
                value: "marker".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        }
    }

    /// An `InstallInfo` with caller-supplied metadata whose package directory
    /// is `<root>/<repo>` and is **not** created on disk — the state a deferred
    /// tool is in.
    fn install_info_with(
        package_root: &std::path::Path,
        repo: &str,
        hex_char: char,
        metadata: metadata::Metadata,
        resolved: ResolvedPackage,
    ) -> InstallInfo {
        InstallInfo::new(
            pinned(repo, hex_char),
            metadata,
            resolved,
            ocx_store::file_structure::PackageDir {
                dir: package_root.join(repo),
            },
        )
    }

    /// Write a package directory `common::load_object_data` can read back.
    fn seed_package_with_metadata(
        store: &PackageStore,
        id: &PinnedPackageRef,
        metadata: &metadata::Metadata,
        resolved: &ResolvedPackage,
    ) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        std::fs::write(pkg_path.join("metadata.json"), serde_json::to_string(metadata).unwrap()).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), serde_json::to_string(resolved).unwrap()).unwrap();
    }

    /// An offline `PackageManager` with no sources — every remote resolve is a
    /// genuine local miss, which is the state `--no-pull` describes.
    fn offline_manager(dir: &std::path::Path) -> PackageManager {
        let fs = FileStructure::with_root(dir.to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(&fs),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// The value a `${installPath}/bin` carrier resolves to for a package whose
    /// content tree is `content`.
    ///
    /// Built by substituting into the template STRING, which is what
    /// `EnvResolver` does — so the separator before `bin` stays the template's
    /// own `/` on every host. `content.join("bin")` renders a `\` on Windows and
    /// matches nothing the resolver ever emits (`quality-rust.md`
    /// "Cross-Platform Path Handling": build the expectation the way the code
    /// builds the value, never with a hand-picked separator).
    fn declared_bin(content: &std::path::Path) -> String {
        format!("{}/bin", content.display())
    }

    /// The `PATH` values in emit (push) order — the projection every shim-slot
    /// ordering assertion reads.
    fn path_values(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .filter(|e| e.key == "PATH")
            .map(|e| e.value.clone())
            .collect()
    }

    // ── The slot names `bin/`, never the shim root ───────────

    /// The emitted PATH entry is the shim's `bin/`
    /// subdirectory. A legal `binaries` claim of `["digest", "refs"]` would
    /// otherwise put a generated launcher on top of the CAS marker and the
    /// forward-ref directory, so the root is never the PATH entry.
    #[test]
    fn the_shim_slot_entry_names_the_bin_subdirectory_never_the_shim_root() {
        let dir = tempfile::tempdir().unwrap();
        let shim = shim_dir_at(dir.path().join("shims").join("tool"));

        let entry = synth_shim_path_for(&shim);

        assert_eq!(entry.key, "PATH", "the shim slot is a PATH contribution");
        assert_eq!(
            std::path::PathBuf::from(&entry.value),
            shim.bin(),
            "the slot must name bin/, not the shim root (C-003)"
        );
        assert_ne!(
            std::path::PathBuf::from(&entry.value),
            shim.root(),
            "naming the shim root would put launchers beside 'digest' and 'refs'"
        );
        assert!(
            matches!(entry.kind, ModifierKind::Path),
            "the slot must be a Path entry so consumers prepend it, got {:?}",
            entry.kind
        );
    }

    // ── Which roots get a slot, and where in the block ───────────────

    /// A deferred root contributes exactly one shim slot.
    #[test]
    fn a_deferred_root_contributes_its_shim_slot() {
        let dir = tempfile::tempdir().unwrap();
        let shim = shim_dir_at(dir.path().join("shims").join("tool"));
        let root = install_info_with(
            dir.path(),
            "tool",
            'a',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage::new(),
        )
        .with_deferred(DeferredComposition::new(shim.clone(), Vec::new()));

        let mut entries = Vec::new();
        emit_shim_slot(&root, surface_axes(false), &mut entries);

        assert_eq!(
            path_values(&entries),
            vec![shim.bin().to_string_lossy().into_owned()],
            "a deferred root contributes exactly its shim bin/ directory"
        );
    }

    /// A materialized root has no shim directory, so the slot is a
    /// no-op. Without this row an implementation that pushed unconditionally
    /// would still pass the row above.
    #[test]
    fn a_materialized_root_contributes_no_shim_slot() {
        let dir = tempfile::tempdir().unwrap();
        let root = install_info_with(
            dir.path(),
            "tool",
            'a',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage::new(),
        );

        let mut entries = Vec::new();
        emit_shim_slot(&root, surface_axes(false), &mut entries);

        assert!(
            entries.is_empty(),
            "a root with no DeferredComposition has no shim tree to put on PATH: {entries:?}"
        );
    }

    /// The slot carries `Entrypoints::IMPLICIT_VISIBILITY` (INTERFACE)
    /// at the root, so it is absent under `--self` — a package's private view
    /// bypasses launchers, and a shim is nothing but a launcher.
    #[test]
    fn the_shim_slot_is_absent_under_self_view() {
        let dir = tempfile::tempdir().unwrap();
        let shim = shim_dir_at(dir.path().join("shims").join("tool"));
        let root = install_info_with(
            dir.path(),
            "tool",
            'a',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage::new(),
        )
        .with_deferred(DeferredComposition::new(shim, Vec::new()));

        let mut entries = Vec::new();
        emit_shim_slot(&root, surface_axes(true), &mut entries);

        assert!(
            entries.is_empty(),
            "the shim slot is INTERFACE-gated at the root and must not reach the --self surface: {entries:?}"
        );
    }

    /// The shim slot's ordering clause at the seam: the slot is pushed **before** the
    /// root's declared vars and before its synthetic `entrypoints/`.
    ///
    /// Consumers apply entries by PREPENDING (`composer.rs` `emit_dep_path_block`
    /// ordering invariant), so last-pushed is first-resolved. Pushing the slot
    /// first therefore gives it the LOWEST precedence — `entrypoints/` >
    /// `bin/` > `shims/` — which is the whole point: once the first invocation
    /// has materialized the package, the real directories shadow the shim.
    /// A reader who assumes push order equals PATH order inverts this.
    #[test]
    fn the_shim_slot_is_pushed_before_the_roots_own_path_carriers() {
        let dir = tempfile::tempdir().unwrap();
        let shim = shim_dir_at(dir.path().join("shims").join("tool"));
        let metadata = bundle_metadata(
            vec![path_var("PATH", "${installPath}/bin", false, Visibility::PUBLIC)],
            &["app"],
        );
        let root = install_info_with(dir.path(), "tool", 'a', metadata, ResolvedPackage::new())
            .with_deferred(DeferredComposition::new(shim.clone(), Vec::new()));

        let mut entries = Vec::new();
        emit_shim_slot(&root, surface_axes(false), &mut entries);
        emit_root_path_block(
            root.metadata(),
            root.dir(),
            &root.dir().content(),
            &std::collections::HashMap::new(),
            surface_axes(false),
            ContentState::Deferred,
            &mut entries,
        )
        .expect("the root's own block resolves");

        let pushed = path_values(&entries);
        assert_eq!(
            pushed,
            vec![
                shim.bin().to_string_lossy().into_owned(),
                declared_bin(&root.dir().content()),
                root.dir().entrypoints().to_string_lossy().into_owned(),
            ],
            "push order must be [shim bin/] [declared bin/] [entrypoints/] — which RESOLVES as \
             entrypoints/ > bin/ > shims/ under prepend semantics (C-012)"
        );
    }

    // ── A deferred root's carriers come from its closure ─────────────

    /// The closure is keyed the way the composer dedups TC entries — on
    /// the advisory-stripped identifier — so a tag-bearing TC entry finds the
    /// member the walker recorded under a different tag.
    #[test]
    fn the_deferred_closure_is_keyed_by_the_advisory_stripped_identifier() {
        let dir = tempfile::tempdir().unwrap();
        let member = Arc::new(install_info_with(
            dir.path(),
            "dep",
            'd',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage::new(),
        ));
        let deferred = DeferredComposition::new(shim_dir_at(dir.path().join("shims")), vec![Arc::clone(&member)]);

        let tagged = PinnedPackageRef::try_from(
            PackageRef::new_registry("dep", REGISTRY)
                .clone_with_tag("1.2.3")
                .clone_with_digest(sha256('d')),
        )
        .unwrap();

        let found = deferred
            .member(&tagged)
            .expect("a tag-bearing TC entry must find the closure member for the same digest");
        assert_eq!(found.identifier().digest(), sha256('d'));
    }

    /// A TC entry the closure does not carry is a disagreement between
    /// the shim tree's ref-linked blobs and the composed TC. `None` is what
    /// lets the caller surface it instead of reaching for a package directory
    /// that does not exist.
    #[test]
    fn the_deferred_closure_has_no_member_for_a_package_it_does_not_carry() {
        let dir = tempfile::tempdir().unwrap();
        let member = Arc::new(install_info_with(
            dir.path(),
            "dep",
            'd',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage::new(),
        ));
        let deferred = DeferredComposition::new(shim_dir_at(dir.path().join("shims")), vec![member]);

        assert!(
            deferred.member(&pinned("other", 'e')).is_none(),
            "a package outside the closure must not resolve to some other member"
        );
    }

    /// For a deferred root the carriers come from the closure, even
    /// when a package directory for the same identifier happens to exist.
    ///
    /// The two sources declare DIFFERENT env keys, so a fallback to the package
    /// store is visible in the assertion rather than merely unproven.
    #[tokio::test]
    async fn a_deferred_roots_tc_entry_reads_its_carriers_from_the_closure_not_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let dep_id = pinned("dep", 'd');

        seed_package_with_metadata(
            &store,
            &dep_id,
            &bundle_metadata(vec![marker_var("FROM_PACKAGE_STORE")], &[]),
            &ResolvedPackage::new(),
        );

        let member = Arc::new(install_info_with(
            dir.path(),
            "dep",
            'd',
            bundle_metadata(vec![marker_var("FROM_CONFIG_BLOB")], &[]),
            ResolvedPackage::new(),
        ));
        let root = install_info_with(
            dir.path(),
            "tool",
            'a',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        )
        .with_deferred(DeferredComposition::new(
            shim_dir_at(dir.path().join("shims")),
            vec![member],
        ));

        let (metadata, _resolved) = tc_entry_object_data(&root, &store, &dep_id)
            .await
            .expect("a deferred root's TC entry resolves from its closure");

        let keys: Vec<&str> = metadata
            .env()
            .expect("the fixture declares one env var")
            .into_iter()
            .map(|var| var.key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec!["FROM_CONFIG_BLOB"],
            "a deferred root must read carriers from the ref-linked config blobs (C-020)"
        );
    }

    /// Control: a materialized root still reads its TC entries from the
    /// package store. Without this row the row above is satisfied by an
    /// implementation that never consults the store at all.
    #[tokio::test]
    async fn a_materialized_roots_tc_entry_reads_its_carriers_from_the_package_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let dep_id = pinned("dep", 'd');

        seed_package_with_metadata(
            &store,
            &dep_id,
            &bundle_metadata(vec![marker_var("FROM_PACKAGE_STORE")], &[]),
            &ResolvedPackage::new(),
        );

        let root = install_info_with(
            dir.path(),
            "tool",
            'a',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        );

        let (metadata, _resolved) = tc_entry_object_data(&root, &store, &dep_id)
            .await
            .expect("a materialized root's TC entry resolves from the package store");

        let keys: Vec<&str> = metadata
            .env()
            .expect("the fixture declares one env var")
            .into_iter()
            .map(|var| var.key.as_str())
            .collect();
        assert_eq!(keys, vec!["FROM_PACKAGE_STORE"]);
    }

    /// A TC entry absent from a deferred closure is an error, never a
    /// silent fall-back to the package store — even when the store could
    /// answer. This is the row that reds on the tempting "try the closure,
    /// else the store" implementation.
    #[tokio::test]
    async fn a_tc_entry_absent_from_a_deferred_closure_is_refused_not_read_from_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let dep_id = pinned("dep", 'd');

        seed_package_with_metadata(
            &store,
            &dep_id,
            &bundle_metadata(vec![marker_var("FROM_PACKAGE_STORE")], &[]),
            &ResolvedPackage::new(),
        );

        let root = install_info_with(
            dir.path(),
            "tool",
            'a',
            bundle_metadata(Vec::new(), &[]),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        )
        .with_deferred(DeferredComposition::new(
            shim_dir_at(dir.path().join("shims")),
            Vec::new(),
        ));

        let error = tc_entry_object_data(&root, &store, &dep_id)
            .await
            .expect_err("a closure that does not carry the entry must refuse, not fall back");

        let crate::Error::ResolveFailed(failures) = &error else {
            panic!("expected a package-manager resolve failure, got {error:?}");
        };
        assert!(
            matches!(failures[0].kind, PackageErrorKind::NotFound),
            "expected NotFound for a TC entry the closure does not carry, got {:?}",
            failures[0].kind
        );
    }

    // ── The shim slot at compose level, and the gates a deferred root must survive ──

    /// End to end: `compose` puts a deferred root's shim slot BELOW its
    /// declared `bin/` and its `entrypoints/`.
    ///
    /// The seam-level sibling above cannot catch a `compose` that never calls
    /// `emit_shim_slot` at all; this row is the one that does.
    #[tokio::test]
    async fn compose_resolves_a_deferred_roots_path_as_entrypoints_then_bin_then_shims() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let shim = shim_dir_at(dir.path().join("shims").join("tool"));
        let metadata = bundle_metadata(
            vec![path_var("PATH", "${installPath}/bin", false, Visibility::PUBLIC)],
            &["app"],
        );
        let root = Arc::new(
            install_info_with(dir.path(), "tool", 'a', metadata, ResolvedPackage::new())
                .with_deferred(DeferredComposition::new(shim.clone(), Vec::new())),
        );

        let out = compose(
            std::slice::from_ref(&root),
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        .unwrap();

        let mut resolved_order = path_values(&out.entries);
        // Consumers prepend, so the resolved PATH is the reverse of emit order.
        resolved_order.reverse();
        assert_eq!(
            resolved_order,
            vec![
                root.dir().entrypoints().to_string_lossy().into_owned(),
                declared_bin(&root.dir().content()),
                shim.bin().to_string_lossy().into_owned(),
            ],
            "resolved PATH for a deferred root must be entrypoints/ > bin/ > shims/ (C-012)"
        );
    }

    /// The composed env carries the shim slot on the interface surface
    /// and **not** under `--self`.
    ///
    /// Both surfaces in one row deliberately: a `--self`-only assertion is a
    /// negative over a set that is empty today and would stay green against an
    /// implementation that never emits the slot at all.
    #[tokio::test]
    async fn compose_carries_the_shim_slot_on_the_interface_surface_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let shim = shim_dir_at(dir.path().join("shims").join("tool"));
        let metadata = bundle_metadata(
            vec![path_var("PATH", "${installPath}/bin", false, Visibility::PUBLIC)],
            &["app"],
        );
        let root = Arc::new(
            install_info_with(dir.path(), "tool", 'a', metadata, ResolvedPackage::new())
                .with_deferred(DeferredComposition::new(shim.clone(), Vec::new())),
        );
        let shim_bin = shim.bin().to_string_lossy().into_owned();

        let consumer = compose(
            std::slice::from_ref(&root),
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        .unwrap();
        assert!(
            path_values(&consumer.entries).contains(&shim_bin),
            "the interface surface must route through the deferred tool's launchers: {:?}",
            consumer.entries
        );

        let self_view = compose(&[root], &store, true, &crate::composer::ComposePaths::digest_only())
            .await
            .unwrap();
        assert!(
            !path_values(&self_view.entries).contains(&shim_bin),
            "--self bypasses launchers, and a shim is nothing but a launcher: {:?}",
            self_view.entries
        );
    }

    /// The deferred-carrier defect clause: `check_entrypoints` runs on every
    /// compose of two or more roots and loads each interface-visible TC entry's
    /// metadata. A deferred root's TC entries have no package directory, so the
    /// multi-root gate must read them through the same closure-aware accessor
    /// the emission walk uses — or every `ocx env` with two lazy tools fails
    /// before a single entry is emitted.
    #[tokio::test]
    async fn compose_of_two_roots_one_deferred_survives_the_multi_root_entrypoint_gate() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());

        let eager = Arc::new(make_install_info_with_ep(
            dir.path(),
            "eager",
            'e',
            ResolvedPackage::new(),
            "eager-app",
        ));

        let dep_id = pinned("dep", 'd');
        let dep_member = Arc::new(install_info_with(
            dir.path(),
            "dep",
            'd',
            bundle_metadata(Vec::new(), &["dep-app"]),
            ResolvedPackage::new(),
        ));
        let deferred = Arc::new(
            install_info_with(
                dir.path(),
                "deferred",
                'f',
                bundle_metadata(Vec::new(), &["deferred-app"]),
                ResolvedPackage {
                    dependencies: vec![ResolvedDependency {
                        identifier: dep_id,
                        visibility: Visibility::PUBLIC,
                    }],
                },
            )
            .with_deferred(DeferredComposition::new(
                shim_dir_at(dir.path().join("shims").join("deferred")),
                vec![dep_member],
            )),
        );

        let out = compose(
            &[eager, deferred],
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        .expect("a deferred root's TC entry has no package directory, and that is not a failure");

        assert_eq!(
            out.admitted_entrypoints.len(),
            3,
            "both roots and the deferred root's interface dep must claim their launchers: {:?}",
            out.admitted_entrypoints
        );
    }

    /// `check_repo_digest_conflicts` reads only `identifier()` and
    /// `resolved().dependencies`, so a deferred root participates in the fatal
    /// version-conflict gate **only if the `ResolvedPackage` synthesized from
    /// its closure is faithful**. An empty synthesized TC compiles, passes
    /// every other test, and silently disables the gate for every deferred
    /// tool — an Unchecked Green by construction.
    #[test]
    fn two_deferred_roots_colliding_on_one_repository_at_two_digests_still_conflict() {
        let dir = tempfile::tempdir().unwrap();

        let deferred_root = |repo: &str, hex: char, shared_hex: char| {
            Arc::new(
                install_info_with(
                    dir.path(),
                    repo,
                    hex,
                    bundle_metadata(Vec::new(), &[]),
                    ResolvedPackage {
                        dependencies: vec![ResolvedDependency {
                            identifier: pinned("shared", shared_hex),
                            visibility: Visibility::PUBLIC,
                        }],
                    },
                )
                .with_deferred(DeferredComposition::new(
                    shim_dir_at(dir.path().join("shims").join(repo)),
                    Vec::new(),
                )),
            )
        };

        let roots = vec![deferred_root("a", 'a', '1'), deferred_root("b", 'b', '2')];

        let error = check_repo_digest_conflicts(&roots, surface_axes(false))
            .expect_err("two deferred tools pinning one repository at two digests is fatal");

        let DependencyError::Conflict {
            repository,
            identifiers,
        } = error
        else {
            panic!("expected a version conflict, got {error:?}");
        };
        assert!(
            repository.to_string().ends_with("shared"),
            "the conflict must name the shared repository, got {repository}"
        );
        let digests: Vec<_> = identifiers.iter().map(|id| id.digest()).collect();
        assert_eq!(
            digests,
            vec![sha256('1'), sha256('2')],
            "both colliding digests must be named"
        );
    }

    // ── The env is not a function of content-cache state ─────

    /// `ocx env` twice, cold store then warm,
    /// is byte-identical for a deferred root **including one that declares a
    /// `required` path var**.
    ///
    /// `env/resolver.rs`'s `required && !path.exists()` probe validates an
    /// INSTALLED tree; a deferred tool has none, so the probe's premise does
    /// not hold and it is suppressed for a deferred root. Without the required
    /// var this fixture would pass for a package class the feature never
    /// supported — the whole point of the row.
    #[tokio::test]
    async fn compose_is_identical_cold_and_warm_for_a_deferred_root_with_a_required_path_var() {
        let dir = tempfile::tempdir().unwrap();
        let store = make_store(dir.path());
        let shim = shim_dir_at(dir.path().join("shims").join("tool"));
        let metadata = bundle_metadata(
            vec![path_var("PATH", "${installPath}/bin", true, Visibility::PUBLIC)],
            &[],
        );
        let root = Arc::new(
            install_info_with(dir.path(), "tool", 'a', metadata, ResolvedPackage::new())
                .with_deferred(DeferredComposition::new(shim, Vec::new())),
        );

        // Cold: nothing under the package directory exists yet.
        assert!(
            !root.dir().content().exists(),
            "the cold leg must genuinely have no content tree"
        );
        let cold = compose(
            std::slice::from_ref(&root),
            &store,
            false,
            &crate::composer::ComposePaths::digest_only(),
        )
        .await
        .expect("a required path var must not make a deferred compose depend on content state");

        // Warm: the first invocation materialized the package.
        std::fs::create_dir_all(root.dir().content().join("bin")).unwrap();
        let warm = compose(&[root], &store, false, &crate::composer::ComposePaths::digest_only())
            .await
            .expect("warm compose succeeds");

        assert_eq!(
            format!("{:?}", cold.entries),
            format!("{:?}", warm.entries),
            "the composed env must be a function of (lock, lazy-mode, metadata availability) alone (C-013)"
        );
    }

    // ── What `compose_roots` reports ─────────────────────

    /// The shape of `compose_roots`' return, pinned at compile time: the three
    /// channels advisories and `--no-pull` need are `roots`, `advisories`, `omitted`,
    /// plus `pulled` for the execution record's `autoInstalled`.
    /// Referenced, never run.
    #[test]
    fn compose_roots_returns_roots_advisories_omissions_and_pulls() {
        async fn signature_binding(
            manager: &PackageManager,
            requests: &[ComposeRequest],
            platform: &Platform,
        ) -> (
            Vec<Arc<InstallInfo>>,
            Vec<LazyAdvisory>,
            Vec<ComposeOmission>,
            Vec<ocx_oci::PackageRef>,
        ) {
            let ComposeRoots {
                roots,
                advisories,
                omitted,
                pulled,
            } = manager
                .compose_roots(requests, platform, Materialization::LocalOnly, Concurrency::default())
                .await
                .expect("compose_roots");
            (roots, advisories, omitted, pulled)
        }

        let _ = signature_binding;
    }

    /// Under `--no-pull` a tool whose metadata is not local is warned
    /// about and omitted — never a hard failure. Warn-and-omit is the composing
    /// caller's decision, so the omission has to reach it as data.
    #[tokio::test]
    async fn local_only_omits_a_deferred_tool_whose_metadata_is_not_local() {
        let dir = tempfile::tempdir().unwrap();
        let manager = offline_manager(dir.path());
        let request = ComposeRequest {
            identifier: PackageRef::new_registry("tool", REGISTRY),
            mode: LazyMode::Always,
        };

        let out = manager
            .compose_roots(
                std::slice::from_ref(&request),
                &Platform::any(),
                Materialization::LocalOnly,
                Concurrency::default(),
            )
            .await
            .expect("--no-pull omits an unreachable tool rather than failing the compose (S-009)");

        assert!(out.roots.is_empty(), "no root can be built for an unreachable tool");
        assert_eq!(
            out.omitted.len(),
            1,
            "the dropped tool must be reported: {:?}",
            out.omitted
        );
        assert_eq!(
            out.omitted[0].identifier.to_string(),
            request.identifier.to_string(),
            "the omission names the request the caller made"
        );
    }

    /// The eager `LocalOnly` half probes concurrently, so **which result lands
    /// on which request** is no longer implied by the order the probes finish.
    /// Three tools with the middle one absent pins all three observable
    /// consequences at once: `roots` in request order, each root carrying its
    /// OWN identifier, and the omission naming the tool that was actually
    /// missing.
    ///
    /// The hole is what makes the row discriminate. With every tool present,
    /// `roots` and the request list are the same length and a misaligned
    /// implementation still returns the right multiset; the gap desynchronizes
    /// the two, so any pairing that is not by request index reds here.
    #[tokio::test]
    async fn local_only_pairs_every_concurrent_probe_with_its_own_request() {
        let dir = tempfile::tempdir().unwrap();
        let manager = offline_manager(dir.path());
        let store = manager.file_structure().packages.clone();

        let present = [pinned("first", 'a'), pinned("third", 'c')];
        for id in &present {
            seed_package_with_metadata(
                &store,
                id,
                &bundle_metadata(Vec::new(), &["app"]),
                &ResolvedPackage::new(),
            );
        }
        // Seeded by nobody: the hole in the middle of the request list.
        let absent = pinned("second", 'b');

        let requests: Vec<ComposeRequest> = [&present[0], &absent, &present[1]]
            .into_iter()
            .map(|id| ComposeRequest {
                identifier: id.as_identifier().clone(),
                mode: LazyMode::Never,
            })
            .collect();

        let out = manager
            .compose_roots(
                &requests,
                &Platform::any(),
                Materialization::LocalOnly,
                Concurrency::default(),
            )
            .await
            .expect("a locally absent tool is an omission under LocalOnly, never a failure");

        assert_eq!(
            out.roots
                .iter()
                .map(|root| root.identifier().to_string())
                .collect::<Vec<_>>(),
            present.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "the surviving roots must come back in request order, each carrying its own identifier"
        );
        assert_eq!(
            out.omitted
                .iter()
                .map(|omission| omission.identifier.to_string())
                .collect::<Vec<_>>(),
            vec![absent.as_identifier().to_string()],
            "the omission must name the tool that was actually missing"
        );
    }

    /// The eager `LocalOnly` half is the arm the per-prompt reconciler takes and
    /// it resolves its requests concurrently, so what the caller is warned about
    /// is whatever finished first unless request order is preserved. The same
    /// order decides which failure aborts a whole compose, so "same inputs, same
    /// report" rests on it.
    ///
    /// The shipped implementation preserves it *by construction* — `join_all`
    /// yields in input order — so there is no sort to delete and this test pins
    /// the contract rather than proving a mechanism. Red state: collect the
    /// results in completion order instead (or reverse them before the zip) and
    /// this comes back in the wrong order. Its sibling
    /// `local_only_pairs_every_concurrent_probe_with_its_own_request` is the one
    /// that discriminates pairing, using an absent middle tool so the hole
    /// desynchronizes the lists.
    #[tokio::test]
    async fn local_only_reports_omissions_in_request_order() {
        let dir = tempfile::tempdir().unwrap();
        let manager = offline_manager(dir.path());
        let names = ["alpha", "beta", "gamma"];
        let requests: Vec<ComposeRequest> = names
            .iter()
            .map(|name| ComposeRequest {
                identifier: PackageRef::new_registry(*name, REGISTRY),
                mode: LazyMode::Never,
            })
            .collect();

        let out = manager
            .compose_roots(
                &requests,
                &Platform::any(),
                Materialization::LocalOnly,
                Concurrency::default(),
            )
            .await
            .expect("--no-pull omits unreachable tools rather than failing the compose");

        assert_eq!(
            out.omitted
                .iter()
                .map(|omission| omission.identifier.to_string())
                .collect::<Vec<_>>(),
            requests
                .iter()
                .map(|request| request.identifier.to_string())
                .collect::<Vec<_>>(),
            "the omission report follows the order the caller asked in"
        );
    }

    /// Advisories are classified for a **deferred** tool only. An
    /// eagerly-materialized request never reaches `prepare_lazy`, which is the
    /// fact that makes the clause testable at all.
    ///
    /// The `roots` assertion is what keeps this from passing vacuously: an
    /// implementation that omitted the tool entirely would also report no
    /// advisories.
    #[tokio::test]
    async fn an_eagerly_materialized_tool_contributes_no_advisories() {
        let dir = tempfile::tempdir().unwrap();
        let manager = offline_manager(dir.path());
        let store = manager.file_structure().packages.clone();

        let eager_id = pinned("tool", 'a');
        seed_package_with_metadata(
            &store,
            &eager_id,
            // No `binaries` claim — the shape that raises `UndeclaredBinaries`
            // when it IS classified, so silence here is a decision, not luck.
            &bundle_metadata(Vec::new(), &["app"]),
            &ResolvedPackage::new(),
        );

        let request = ComposeRequest {
            identifier: eager_id.as_identifier().clone(),
            mode: LazyMode::Never,
        };

        let out = manager
            .compose_roots(
                std::slice::from_ref(&request),
                &Platform::any(),
                Materialization::LocalOnly,
                Concurrency::default(),
            )
            .await
            .expect("a locally present package composes eagerly");

        assert_eq!(
            out.roots.len(),
            1,
            "the eager tool must actually be composed: {:?}",
            out.omitted
        );
        assert!(
            out.advisories.is_empty(),
            "an eagerly-materialized tool never reaches the advisory classifier (C-015 d): {:?}",
            out.advisories
        );
    }

    // ── The `lazy-mode` ladders ──────────────────────────────────────
    //
    // Every row expects `Always`, which is never the ladder's floor, and sets
    // every tier BELOW the one under test to `Never`. A resolver that consults
    // the tiers in the wrong order — or consults none — answers `Never` and
    // reds; the construction also makes each row deterministic regardless of
    // the ambient `OCX_LAZY_MODE`.
    //
    // The rows drive the ladder ASSEMBLERS and resolve with the pure
    // `resolve()`, never `resolve_for_host()`. Which config tier feeds which
    // ladder slot is a host-independent contract, but the host form answers
    // `Never` for every input on Windows — so a row driving it would be
    // green there whatever the wiring did, which is no check at all. The host
    // floor gets its own two-halved row at the end of this block.

    fn tool_identifier() -> PackageRef {
        PackageRef::new_registry("ns/tool", REGISTRY).clone_with_tag("1.2.3")
    }

    /// A `ProjectConfig` carrying the three config tiers that give
    /// `lazy-mode`. `group` names the group whose table the mode lands in, so
    /// a row can assert the tier applies only to the selected group.
    fn ladder_config(
        toolchain: Option<LazyMode>,
        group: Option<(&str, LazyMode)>,
        package: Option<(&str, LazyMode)>,
    ) -> ProjectConfig {
        let mut config = ProjectConfig::default();
        if let Some(mode) = toolchain {
            config.lazy_mode = Some(mode);
        }
        if let Some((name, mode)) = group {
            config.groups.insert(
                name.to_string(),
                Group {
                    lazy_mode: Some(mode),
                    ..Group::default()
                },
            );
        }
        if let Some((key, mode)) = package {
            config.packages.insert(
                key.to_string(),
                PackageSettings {
                    no_patches: false,
                    lazy_mode: Some(mode),
                    lazy_report: None,
                },
            );
        }
        config
    }

    /// The CLI flag outranks every config tier.
    #[test]
    fn the_cli_lazy_mode_outranks_every_config_tier() {
        let config = ladder_config(
            Some(LazyMode::Never),
            Some(("ci", LazyMode::Never)),
            Some((&format!("{REGISTRY}/ns/tool"), LazyMode::Never)),
        );

        assert_eq!(
            lazy_mode_ladder_for_tool(&config, &tool_identifier(), Some("ci"), Some(LazyMode::Always)).resolve(),
            LazyMode::Always
        );
    }

    /// `[package."<id>"]` outranks `[group.<g>]`.
    #[test]
    fn the_package_tier_outranks_the_group_tier() {
        let config = ladder_config(
            Some(LazyMode::Never),
            Some(("ci", LazyMode::Never)),
            Some((&format!("{REGISTRY}/ns/tool"), LazyMode::Always)),
        );

        assert_eq!(
            lazy_mode_ladder_for_tool(&config, &tool_identifier(), Some("ci"), None).resolve(),
            LazyMode::Always
        );
    }

    /// `[group.<g>]` outranks the toolchain tier — and only for the
    /// group the binding came from.
    #[test]
    fn the_group_tier_outranks_the_toolchain_tier() {
        let config = ladder_config(Some(LazyMode::Never), Some(("ci", LazyMode::Always)), None);

        assert_eq!(
            lazy_mode_ladder_for_tool(&config, &tool_identifier(), Some("ci"), None).resolve(),
            LazyMode::Always,
            "the selected group's tier applies"
        );
        assert_eq!(
            lazy_mode_ladder_for_tool(&config, &tool_identifier(), None, None).resolve(),
            LazyMode::Never,
            "a positional package has no group tier, so the toolchain tier answers"
        );
    }

    /// The toolchain tier answers when no more specific tier is set.
    #[test]
    fn the_toolchain_tier_answers_when_no_more_specific_tier_is_set() {
        let config = ladder_config(Some(LazyMode::Always), None, None);

        assert_eq!(
            lazy_mode_ladder_for_tool(&config, &tool_identifier(), None, None).resolve(),
            LazyMode::Always
        );
    }

    /// The package tier keys on `registry/repository` with tag and
    /// digest excluded — a config author cannot know the digest a lock pins.
    #[test]
    fn the_package_tier_matches_the_repository_without_its_tag() {
        let config = ladder_config(
            Some(LazyMode::Never),
            None,
            Some((&format!("{REGISTRY}/ns/tool"), LazyMode::Always)),
        );

        assert_eq!(
            lazy_mode_ladder_for_tool(&config, &tool_identifier(), None, None).resolve(),
            LazyMode::Always
        );
    }

    /// An unrelated `[package.*]` entry must not answer. Without this
    /// row the one above passes for "take whatever single entry exists".
    #[test]
    fn a_package_entry_for_another_package_never_answers() {
        let config = ladder_config(
            Some(LazyMode::Never),
            None,
            Some((&format!("{REGISTRY}/ns/other"), LazyMode::Always)),
        );

        assert_eq!(
            lazy_mode_ladder_for_tool(&config, &tool_identifier(), None, None).resolve(),
            LazyMode::Never
        );
    }

    /// `OCX_LAZY_MODE` is the last tier above the floor. Without this
    /// row an implementation that drops the environment tier entirely passes
    /// every other ladder test.
    #[test]
    fn the_environment_tier_answers_when_every_config_tier_is_absent() {
        let env = ocx_util::env::overrides::lock();
        env.set("OCX_LAZY_MODE", "always");

        assert_eq!(
            lazy_mode_ladder_for_tool(&ProjectConfig::default(), &tool_identifier(), None, None).resolve(),
            LazyMode::Always
        );
    }

    /// The floor is the literal `Never`, reached only when every tier
    /// above it is absent.
    #[test]
    fn the_ladder_floor_is_never() {
        let env = ocx_util::env::overrides::lock();
        env.remove("OCX_LAZY_MODE");

        assert_eq!(
            lazy_mode_ladder_for_tool(&ProjectConfig::default(), &tool_identifier(), None, None).resolve(),
            LazyMode::Never
        );
    }

    /// The OCI tier has two tiers, not five — `ocx package env` /
    /// `exec` read no `ocx.toml` at any tier, so the config tiers are absent
    /// rather than silently ignored.
    #[test]
    fn the_oci_tier_ladder_is_the_cli_flag_then_the_environment() {
        let env = ocx_util::env::overrides::lock();

        env.set("OCX_LAZY_MODE", "never");
        assert_eq!(
            lazy_mode_ladder_for_package(Some(LazyMode::Always)).resolve(),
            LazyMode::Always,
            "the CLI flag outranks the environment tier"
        );

        env.set("OCX_LAZY_MODE", "always");
        assert_eq!(
            lazy_mode_ladder_for_package(None).resolve(),
            LazyMode::Always,
            "the environment tier answers when the flag is absent"
        );

        env.remove("OCX_LAZY_MODE");
        assert_eq!(
            lazy_mode_ladder_for_package(None).resolve(),
            LazyMode::Never,
            "the floor is Never"
        );
    }

    // ── Both tier wrappers resolve through the HOST form ─────────────
    //
    // The rows above drive the ladder assemblers, so nothing there would notice
    // a wrapper that called `resolve()` instead of `resolve_for_host()`. This
    // one does.
    //
    // **One row, on every host.** It used to be a host-gated pair, because
    // `resolve_for_host` forced `LazyMode::Never` on Windows while nothing
    // there could write a deferred tool's shim slot. That producer now ships
    // and the floor is gone, so `resolve_for_host` is now a passthrough
    // and both halves assert the same literal — a Windows half saying `Never`
    // would only re-state a removed rule.
    //
    // The ladder asks for `always` at the toolchain tier, which outranks
    // `OCX_LAZY_MODE`, so this does not depend on ambient process state.

    #[test]
    fn both_tier_wrappers_answer_the_ladder_where_a_shim_producer_exists() {
        let config = ladder_config(Some(LazyMode::Always), None, None);

        assert_eq!(
            lazy_mode_for_tool(&config, &tool_identifier(), None, None),
            LazyMode::Always,
            "the project tier answers its ladder"
        );
        assert_eq!(
            lazy_mode_for_package(Some(LazyMode::Always)),
            LazyMode::Always,
            "and so does the OCI tier, even for the most specific tier there is"
        );
    }

    // ── The two rows the first pass deferred ───────────────────────
    //
    // Both need `prepare_lazy` to actually SUCCEED, which needs a manifest
    // source serving a manifest plus a config blob. Specify refused to ship the
    // fixture it could not show green (`compose_roots` was `unimplemented!()`
    // then); with the implementation in place it is shown green here, against
    // the shared `FakeManifestSource` promoted out of `tasks/inspect.rs` and
    // `tasks/resolve.rs` for exactly this third caller.

    /// A `Bundle`-shaped config blob with **no `binaries` key** — the
    /// tri-state wire shape that raises `UndeclaredBinaries` when it is
    /// classified, which is what makes the mixed-batch row below able to tell
    /// which half of the split ran the classifier.
    fn lazy_config(dependencies: serde_json::Value, entrypoint: &str) -> String {
        serde_json::json!({
            "type": "bundle",
            "version": 1,
            "dependencies": dependencies,
            "entrypoints": { entrypoint: {} },
            // A cleanly substitutable `path` value: the ONLY advisory this
            // fixture raises is `UndeclaredBinaries`, so the mixed-batch count
            // below measures the classifier's gating and nothing else.
            "env": [{
                "key": format!("{}_HOME", entrypoint.to_ascii_uppercase().replace('-', "_")),
                "type": "path",
                "value": "${installPath}/bin",
                "required": false,
                "visibility": "public",
            }],
        })
        .to_string()
    }

    /// A flat image manifest referencing `config_json`'s digest, sized to its
    /// real length. No layers — nothing on this path reads them.
    fn lazy_manifest_json(config_digest: &ocx_oci::Digest, config_json: &str) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{media}","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
            media = ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1,
            size = config_json.len(),
        )
    }

    /// Register one closure node (manifest keyed by its own digest, plus the
    /// config blob), returning the manifest digest a dep edge addresses it by.
    fn register_lazy_node(
        source: crate::test_support::manifest_source::FakeManifestSource,
        config_json: &str,
    ) -> (
        crate::test_support::manifest_source::FakeManifestSource,
        ocx_oci::Digest,
    ) {
        let config_digest = ocx_oci::Algorithm::Sha256.hash(config_json.as_bytes());
        let manifest_json = lazy_manifest_json(&config_digest, config_json);
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(manifest_json.as_bytes());
        let source = source
            .with(&manifest_digest.to_string(), manifest_json.as_bytes())
            .with_blob(&config_digest.to_string(), config_json.as_bytes());
        (source, manifest_digest)
    }

    /// Like [`register_lazy_node`] but also answers `tag`, so a tag-addressed
    /// request resolves to it.
    fn register_lazy_tagged(
        source: crate::test_support::manifest_source::FakeManifestSource,
        tag: &str,
        config_json: &str,
    ) -> (
        crate::test_support::manifest_source::FakeManifestSource,
        ocx_oci::Digest,
    ) {
        let (source, manifest_digest) = register_lazy_node(source, config_json);
        let config_digest = ocx_oci::Algorithm::Sha256.hash(config_json.as_bytes());
        let manifest_json = lazy_manifest_json(&config_digest, config_json);
        (source.with(tag, manifest_json.as_bytes()), manifest_digest)
    }

    /// A `PackageManager` chained to `source` under `ChainMode::Default`, so a
    /// leaf platform manifest is recovered exactly as a live registry would
    /// serve it (a leaf is never locally cached).
    fn lazy_manager(
        dir: &std::path::Path,
        source: crate::test_support::manifest_source::FakeManifestSource,
    ) -> PackageManager {
        let fs = FileStructure::with_root(dir.to_path_buf());
        // With the blob store attached, exactly as `Context::try_init` builds
        // it: `fetch_blob`'s write-through is what puts a closure node's config
        // blob into `$OCX_HOME/blobs`, which is where the shim tree's
        // `refs/blobs/` points and where a deferred root reads its carriers
        // back from.
        let index = Index::from_chained_with_content_store(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(&fs),
            }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
            fs.blobs.clone(),
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// One `dependencies[]` entry as authored: pinned identifier, declared edge
    /// visibility, interpolation name.
    fn lazy_edge(identifier: &PinnedPackageRef, visibility: &str, name: &str) -> serde_json::Value {
        serde_json::json!({ "identifier": identifier.to_string(), "visibility": visibility, "name": name })
    }

    /// The mixed batch: one eager tool and one deferred tool in a
    /// single `compose_roots`, both carrying the advisory-raising metadata
    /// shape. The advisory appears **exactly once**, and it names the
    /// **deferred** one.
    ///
    /// This is the clause's only real proof. The two halves deliberately name
    /// DIFFERENT packages: with one identifier used twice, an implementation
    /// that classified only the eager half would still report exactly one
    /// advisory carrying the right name, and the row would pass for the wrong
    /// reason. Distinct packages make both wrong implementations red — two
    /// advisories if both halves classify, the wrong package named if only the
    /// eager one does.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_mixed_batch_raises_the_advisory_once_and_only_for_the_deferred_tool() {
        let dir = tempfile::tempdir().unwrap();

        let deferred_config = lazy_config(serde_json::json!([]), "deferred-app");
        let (source, deferred_manifest_digest) = register_lazy_tagged(
            crate::test_support::manifest_source::FakeManifestSource::default(),
            "1.0",
            &deferred_config,
        );
        let manager = lazy_manager(dir.path(), source);
        let store = manager.file_structure().packages.clone();

        // The eager half: locally materialized, and its on-disk metadata
        // declares no `binaries` either — so silence about it is a decision
        // about the MODE, not about the metadata.
        let eager_id = pinned("eager", 'e');
        seed_package_with_metadata(
            &store,
            &eager_id,
            &bundle_metadata(Vec::new(), &["eager-app"]),
            &ResolvedPackage::new(),
        );

        let requests = vec![
            ComposeRequest {
                identifier: eager_id.as_identifier().clone(),
                mode: LazyMode::Never,
            },
            ComposeRequest {
                identifier: PackageRef::new_registry("deferred", REGISTRY).clone_with_tag("1.0"),
                mode: LazyMode::Always,
            },
        ];

        let out = manager
            .compose_roots(
                &requests,
                &Platform::any(),
                Materialization::LocalOnly,
                Concurrency::default(),
            )
            .await
            .expect("a mixed batch composes both halves");

        assert_eq!(
            out.roots.len(),
            2,
            "both halves must compose, or the advisory count below is vacuous: omitted={:?}",
            out.omitted
        );
        assert!(
            out.roots[0].deferred().is_none() && out.roots[1].deferred().is_some(),
            "roots come back in request order: eager first, deferred second"
        );
        assert_eq!(
            out.advisories.len(),
            1,
            "the classifier runs for the deferred tool and for nothing else: {:?}",
            out.advisories
        );
        let LazyAdvisory::UndeclaredBinaries { package } = &out.advisories[0] else {
            panic!("expected UndeclaredBinaries, got {:?}", out.advisories[0]);
        };
        assert_eq!(
            package.digest(),
            deferred_manifest_digest,
            "the advisory must name the DEFERRED tool, not the eager one"
        );
    }

    /// The half the first pass could not reach: `deferred_root`
    /// synthesizes a **faithful** `ResolvedPackage` from the walked closure.
    ///
    /// The existing row proves the version-conflict gate stays armed for
    /// deferred roots and is sensitive to an empty TC; it cannot prove the
    /// synthesis, because that needs `prepare_lazy` to succeed. An empty or
    /// visibility-flattened synthesis compiles, passes every other test, and
    /// silently disables the conflict gate, the entry-point collision gate and
    /// the surface algebra for every deferred tool at once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_deferred_roots_transitive_closure_is_synthesized_faithfully_from_the_walked_closure() {
        let dir = tempfile::tempdir().unwrap();

        let dep_config = lazy_config(serde_json::json!([]), "dep-app");
        let (source, dep_manifest_digest) = register_lazy_node(
            crate::test_support::manifest_source::FakeManifestSource::default(),
            &dep_config,
        );
        let dep_id = PinnedPackageRef::try_from(
            PackageRef::new_registry("dep", REGISTRY).clone_with_digest(dep_manifest_digest.clone()),
        )
        .unwrap();

        let root_config = lazy_config(serde_json::json!([lazy_edge(&dep_id, "public", "dep")]), "root-app");
        let (source, root_manifest_digest) = register_lazy_tagged(source, "2.0", &root_config);

        let manager = lazy_manager(dir.path(), source);
        let request = ComposeRequest {
            identifier: PackageRef::new_registry("tool", REGISTRY).clone_with_tag("2.0"),
            mode: LazyMode::Always,
        };

        let out = manager
            .compose_roots(
                std::slice::from_ref(&request),
                &Platform::any(),
                Materialization::Install,
                Concurrency::default(),
            )
            .await
            .expect("a deferred request composes without downloading content");

        assert_eq!(out.roots.len(), 1);
        let root = &out.roots[0];
        assert_eq!(root.identifier().digest(), root_manifest_digest);
        assert!(
            !root.dir().content().exists(),
            "no content may be materialized for a deferred root"
        );

        // The synthesized TC: the dep, at its resolved digest, carrying the
        // visibility the walk composed from the root — not an empty vector and
        // not a flattened one.
        let dependencies = &root.resolved().dependencies;
        assert_eq!(
            dependencies.len(),
            1,
            "the synthesized TC must carry the closure's one dep: {dependencies:?}"
        );
        assert_eq!(dependencies[0].identifier.digest(), dep_manifest_digest);
        assert_eq!(
            dependencies[0].visibility,
            Visibility::PUBLIC,
            "a public edge composes to a public effective visibility"
        );

        // The member the composer will read that TC entry through is
        // present, and its carriers came from the ref-linked config blob — the
        // package directory for it does not exist.
        let deferred = root.deferred().expect("a deferred request yields a deferred root");
        let member = deferred
            .member(&dep_id)
            .expect("the closure carries the member its TC entry names");
        assert!(!member.dir().content().exists());
        let keys: Vec<String> = member
            .metadata()
            .env()
            .expect("the fixture declares one env var")
            .into_iter()
            .map(|var| var.key.clone())
            .collect();
        assert_eq!(
            keys,
            vec!["DEP_APP_HOME".to_string()],
            "the member's carriers must come from its config blob"
        );
    }
}

// ── Following-lane specification tests ──────────────────────────────────────
// Each case names the contract it traces to and the mutation that must red it. The
// fixture platform is a constant, never the host's: `ComposePaths::resolve` takes it as
// a parameter, so a host-derived one would make every link assertion answer differently
// on the Windows leg for a reason unrelated to the contract under test.
#[cfg(test)]
mod wp15_following_lane_spec_tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::{
        ComposePaths, PathLane, ToolchainLinks, compose, link_is_trustworthy, pinned_for_project,
        pinned_ladder_for_project,
    };
    use ocx_store::file_structure::{FileStructure, PackageDir, RenderStampScope, ToolchainHome};

    use ocx_package::install_info::InstallInfo;
    use ocx_package::metadata::{
        self, bundle, dependency,
        entrypoint::Entrypoints,
        env::{
            self as metadata_env,
            var::{Modifier, Var},
        },
        visibility::Visibility,
    };
    use ocx_package::resolved_package::ResolvedPackage;
    use ocx_project::{
        DECLARATION_HASH_VERSION, DEFAULT_GROUP, LockMetadata, LockVersion, LockedTool, ProjectConfig, ProjectLock,
    };

    const REGISTRY: &str = "example.com";

    /// The platform every lock leaf in this module is keyed by.
    const PLATFORM_KEY: &str = "linux/amd64";

    fn platform() -> ocx_oci::Platform {
        PLATFORM_KEY.parse().expect("the fixture platform key is canonical")
    }

    fn digest_of(seed: char) -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(seed.to_string().repeat(64))
    }

    fn locked_tool(name: &str, group: &str, repository: &str, seed: char) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            repository: ocx_oci::Repository::new(REGISTRY, repository),
            platforms: BTreeMap::from([(PLATFORM_KEY.to_string(), digest_of(seed))]),
        }
    }

    /// A tool whose only leaf is keyed by a platform the composition never
    /// targets — the "no compatible leaf" input `link_target` answers `None`
    /// for.
    fn locked_tool_for_another_platform(name: &str, group: &str, repository: &str, seed: char) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            repository: ocx_oci::Repository::new(REGISTRY, repository),
            platforms: BTreeMap::from([("windows/arm64".to_string(), digest_of(seed))]),
        }
    }

    fn lock_of(tools: Vec<LockedTool>) -> ProjectLock {
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: DECLARATION_HASH_VERSION,
                declaration_hash: "0".repeat(64),
                generated_by: "wp-15 specification fixture".to_string(),
                generated_at: "2026-01-01T00:00:00Z".to_string(),
            },
            tools,
        }
    }

    fn groups_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    }

    fn config_with_pinned(value: Option<bool>) -> ProjectConfig {
        let mut config = ProjectConfig::default();
        config.pinned = value;
        config
    }

    /// One project tree under one tempdir: an `$OCX_HOME`, a project directory
    /// and the `<project>/.ocx/toolchain` home the links live in.
    struct Tree {
        // Every reader is a `#[cfg(unix)]` symlink test; off Unix the field is
        // still needed to keep the directory alive for the fixture's lifetime,
        // which is a use `dead_code` does not count. `expect` rather than
        // `allow` so the day a Windows test reads it, this line fails.
        #[cfg_attr(
            not(unix),
            expect(dead_code, reason = "lifetime anchor; read only by cfg(unix) tests")
        )]
        tmp: TempDir,
        file_structure: FileStructure,
        project_dir: PathBuf,
        home: ToolchainHome,
    }

    impl Tree {
        fn new() -> Self {
            let tmp = tempfile::tempdir().expect("a tempdir is creatable");
            let file_structure = FileStructure::with_root(tmp.path().join("ocx-home"));
            let project_dir = tmp.path().join("proj");
            std::fs::create_dir_all(&project_dir).expect("the project directory is creatable");
            let home = ToolchainHome::new(project_dir.join(".ocx").join("toolchain"));
            Self {
                tmp,
                file_structure,
                project_dir,
                home,
            }
        }

        fn scope(&self) -> RenderStampScope {
            RenderStampScope::Project(self.project_dir.clone())
        }

        /// The digest root one lock entry's link must name, derived the way the
        /// renderer and the heal derive it — through the shared `select_best`
        /// helper, never by an exact key lookup.
        fn digest_root(&self, tool: &LockedTool) -> PathBuf {
            let leaf = tool
                .host_leaf(&platform())
                .expect("the fixture lock ships a leaf compatible with the fixture platform");
            let pinned = tool.repository.pin_untagged(leaf);
            self.file_structure.packages.path(&pinned)
        }

        /// The digest root, materialised. A mismatch case must plant a link at
        /// a **different real digest root**, never a dangling one — those are
        /// two different arms of the degrade rule.
        fn seed_digest_root(&self, tool: &LockedTool) -> PathBuf {
            let root = self.digest_root(tool);
            std::fs::create_dir_all(root.join("content").join("bin")).expect("a digest root is creatable");
            root
        }

        fn entry(&self, group: &str, name: &str) -> PathBuf {
            self.home.entry(group, name).expect("the fixture names are admitted")
        }

        fn plant_link(&self, group: &str, name: &str, target: &std::path::Path) -> PathBuf {
            let entry = self.entry(group, name);
            ocx_util::fs::symlink::create(target, &entry).expect("the fixture link is creatable");
            entry
        }

        fn links(&self, pinned: bool, lock: ProjectLock, groups: &[&str]) -> ToolchainLinks {
            ToolchainLinks {
                pinned,
                home: self.home.clone(),
                scope: self.scope(),
                lock,
                groups: groups_of(groups),
            }
        }

        async fn resolve(&self, links: &ToolchainLinks) -> ComposePaths {
            ComposePaths::resolve(links, &self.file_structure, &platform())
                .await
                .expect("C-067 — a missing, absent or stale link degrades; it never fails an emission")
        }

        /// What one package's install path composes to on the following lane.
        fn following(&self, paths: &ComposePaths, digest_root: &std::path::Path) -> PathBuf {
            paths
                .install_path_for(&PackageDir::with_root(digest_root.to_path_buf()), PathLane::Following)
                .root()
                .to_path_buf()
        }
    }

    // ── The `pinned` ladder ──────────────────
    //
    // The resolved value is the single input the digest lane gates on, so
    // each precedence case sets its own tier to one value and EVERY weaker tier
    // to a different one: a resolver that consults the tiers in the wrong order
    // returns the other value and reds. The floor case and the single-tier case
    // populate at most one tier, so a transposition cannot reach them.

    /// `--pinned` / `--no-pinned` outranks `ocx.toml` and
    /// `OCX_TOOLCHAIN_PINNED`.
    ///
    /// RED: transposing the `cli` and `file` operands of the resolution chain.
    #[test]
    fn the_cli_pinned_flag_outranks_the_config_and_the_environment() {
        let env = ocx_util::env::overrides::lock();
        env.set(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED, "false");

        assert!(
            pinned_for_project(Some(true), &config_with_pinned(Some(false))),
            "C-007 — the flag on the invoked command is the strongest tier"
        );
    }

    /// `ocx.toml`'s `pinned` key outranks `OCX_TOOLCHAIN_PINNED`: an
    /// exported variable loses to a project that states a value.
    ///
    /// RED: transposing the `file` and `environment` operands.
    #[test]
    fn the_config_tier_outranks_the_environment() {
        let env = ocx_util::env::overrides::lock();
        env.set(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED, "false");

        assert!(
            pinned_for_project(None, &config_with_pinned(Some(true))),
            "C-007 — the environment tier is the weakest, below the file tier"
        );
    }

    /// `OCX_TOOLCHAIN_PINNED` is the last tier above the floor.
    ///
    /// RED: dropping the environment tier entirely. Without this row that
    /// implementation passes every other ladder case.
    #[test]
    fn the_environment_tier_answers_when_the_cli_and_the_config_are_absent() {
        let env = ocx_util::env::overrides::lock();
        env.set(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED, "true");

        assert!(
            pinned_for_project(None, &config_with_pinned(None)),
            "C-007 — an exported value decides for a project that states none"
        );
    }

    /// Every tier absent means `ocx_project::activate::PINNED_FLOOR`, and the
    /// floor is the **following** lane (the default).
    ///
    /// RED: a re-spelled `true` literal in place of `PINNED_FLOOR`, which would
    /// pin every un-configured project and make the following lane unreachable in practice.
    #[test]
    fn an_all_absent_pinned_ladder_resolves_to_the_following_lane_floor() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        assert!(
            !pinned_for_project(None, &config_with_pinned(None)),
            "C-007 — the floor is `false`: follow the links"
        );
    }

    /// `--no-pinned` overrides an `ocx.toml` that asked to pin. Its
    /// entire job.
    ///
    /// RED: collapsing the CLI tier's `Option<bool>` into a bare `bool`, which
    /// makes `Some(false)` indistinguishable from "no flag given" and deletes
    /// `--no-pinned`. The sibling precedence case cannot catch that mutation —
    /// it passes `Some(true)`.
    #[test]
    fn no_pinned_overrides_a_config_that_asked_to_pin() {
        let env = ocx_util::env::overrides::lock();
        env.set(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED, "true");

        assert!(
            !pinned_for_project(Some(false), &config_with_pinned(Some(true))),
            "C-007 — `--no-pinned` is an explicit `false`, never an absent tier"
        );
    }

    /// The unresolved form reports one tier per source, so the tier
    /// order is assertable without also asserting the floor.
    ///
    /// RED: filling `file` from the environment reader (or `environment` from
    /// the config), which every resolving case above would still pass whenever
    /// the two sources happen to agree.
    #[test]
    fn the_unresolved_pinned_ladder_reports_one_tier_per_source() {
        let env = ocx_util::env::overrides::lock();
        env.set(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED, "false");

        let ladder = pinned_ladder_for_project(Some(true), &config_with_pinned(Some(true)));

        assert_eq!(ladder.cli, Some(true), "the CLI tier carries the flag");
        assert_eq!(ladder.file, Some(true), "the file tier carries `ocx.toml`'s key");
        assert_eq!(
            ladder.environment,
            Some(false),
            "the environment tier carries `OCX_TOOLCHAIN_PINNED`"
        );
    }

    // ── The per-entry trust probe ───────────────────────────

    /// A link naming the lock-derived digest root is one the
    /// composition may follow.
    ///
    /// RED: a probe that answers `false` unconditionally, which would collapse
    /// the whole following lane into the digest lane.
    #[test]
    fn a_link_naming_the_lock_derived_target_is_trustworthy() {
        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let target = tree.seed_digest_root(&tool);
        let entry = tree.plant_link(DEFAULT_GROUP, "cmake", &target);

        assert!(link_is_trustworthy(&entry, &target));
    }

    /// An absent entry is not trustworthy, and asking is not an error.
    ///
    /// RED: a probe that answers `true` unconditionally.
    #[test]
    fn an_absent_entry_is_not_trustworthy() {
        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let target = tree.seed_digest_root(&tool);
        let entry = tree.entry(DEFAULT_GROUP, "cmake");

        assert!(!entry.exists(), "precondition: nothing was rendered for this entry");
        assert!(!link_is_trustworthy(&entry, &target));
    }

    /// **a mismatch is treated as absent, not as usable.** The link
    /// names a *different real digest root*, which is the state a branch switch
    /// leaves behind and the exact input a "the link exists, so follow it"
    /// implementation resolves to the previous package on.
    ///
    /// RED: probing existence instead of the target. Both roots are real
    /// directories on disk, so no existence check can tell them apart.
    #[test]
    fn a_link_naming_a_different_real_digest_root_is_not_trustworthy() {
        let tree = Tree::new();
        let current = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let previous = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'b');
        let current_target = tree.seed_digest_root(&current);
        let previous_target = tree.seed_digest_root(&previous);
        assert_ne!(current_target, previous_target, "precondition: two distinct digests");
        assert!(previous_target.is_dir(), "precondition: the stale target is real");

        let entry = tree.plant_link(DEFAULT_GROUP, "cmake", &previous_target);

        assert!(
            !link_is_trustworthy(&entry, &current_target),
            "C-067 — a stale link degrades to a digest path instead of resolving the previous package"
        );
    }

    /// A real directory where a link belongs is not
    /// trustworthy. This is the shape the heal is forbidden to repair (it has
    /// no delete authority), so the composing side has to refuse it.
    ///
    /// RED: `Path::exists()` as the probe, or `read_link`'s error being
    /// swallowed into `true`.
    #[test]
    fn a_real_directory_where_a_link_belongs_is_not_trustworthy() {
        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let target = tree.seed_digest_root(&tool);
        let entry = tree.entry(DEFAULT_GROUP, "cmake");
        std::fs::create_dir_all(entry.join("content")).expect("the impostor directory is creatable");

        assert!(entry.is_dir(), "precondition: an ordinary directory, not a link");
        assert!(!link_is_trustworthy(&entry, &target));
    }

    /// The comparison is against the **lock**, never against
    /// the filesystem: a link naming a digest root that is not materialised is
    /// still the correct link, and that is a lazily-loaded tool's ordinary
    /// state. Paired with the mismatch case above, which is where a "the target
    /// must exist" implementation would still pass.
    ///
    /// RED: adding a `target.exists()` conjunct to the probe.
    #[test]
    fn a_link_naming_the_target_is_trustworthy_even_when_the_target_does_not_exist() {
        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let target = tree.digest_root(&tool);
        let entry = tree.plant_link(DEFAULT_GROUP, "cmake", &target);

        assert!(!target.exists(), "precondition: the package is not materialised");
        assert!(link_is_trustworthy(&entry, &target));
    }

    /// The target comparison is made on the **raw** `read_link`
    /// answer, un-canonicalised, because `heal_links` compares raw targets: a
    /// composer that canonicalised would disagree with the repairer and would
    /// follow *through* a target the heal refused to touch.
    ///
    /// The planted link names the same directory by a different spelling, so a
    /// canonicalising probe answers `true` — the precondition below proves that
    /// red is reachable — while the contract's probe answers `false`.
    ///
    /// Unix-only: a Windows junction stores a normalised absolute target, so
    /// the two spellings are indistinguishable at `read_link` there and the
    /// case has no reachable red on that leg.
    #[cfg(unix)]
    #[test]
    fn the_target_comparison_is_not_canonicalised() {
        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let target = tree.seed_digest_root(&tool);

        let alias = tree.tmp.path().join("alias");
        ocx_util::fs::symlink::create(target.parent().expect("a digest root has a shard parent"), &alias)
            .expect("the alias link is creatable");
        let aliased = alias.join(target.file_name().expect("a digest root has a final component"));

        assert_eq!(
            std::fs::canonicalize(&aliased).expect("the alias resolves"),
            std::fs::canonicalize(&target).expect("the target resolves"),
            "precondition: a canonicalising probe would call this link trustworthy"
        );

        let entry = tree.plant_link(DEFAULT_GROUP, "cmake", &aliased);

        assert!(
            !link_is_trustworthy(&entry, &target),
            "RUL-80 — the raw `read_link` answer is compared, so the composer and the heal cannot disagree"
        );
    }

    // ── The pinned lane ──────────────────────────────────────────────

    /// Under a project that asked to pin, every emitter yields
    /// digest paths, **no link is consulted, and no tree is touched**.
    ///
    /// `home.root()` is the assertion because the heal rule names it: `heal_links`
    /// calls `ensure_home_root`, which *creates* the root. A pinned composition
    /// that reached the heal would leave one behind.
    ///
    /// RED: deleting `resolve`'s `pinned` short-circuit, or a ladder that
    /// resolves `pinned = true` to the following lane.
    #[tokio::test]
    async fn a_pinned_project_composes_on_digest_paths_and_never_creates_the_home_root() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let digest_root = tree.seed_digest_root(&tool);
        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(true))),
            lock_of(vec![tool]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &digest_root),
            digest_root,
            "C-066 — a pinned emitter yields the digest root"
        );
        assert!(
            !tree.home.root().exists(),
            "C-066 / RUL-81 — a pinned composition performs no heal, so nothing creates the home root"
        );
    }

    // ── The following lane ───────────────────────────

    /// A selected group's entry composes through
    /// `<home>/links/<group>/<entry>`, not through its digest root. The link is
    /// absent to begin with, which is the ordinary post-`git pull` state, and
    /// the composing heal creates it before the probe runs.
    ///
    /// RED: `resolve` returning `ComposePaths::digest_only()` on the following
    /// lane, or `install_path_for` ignoring the map.
    #[tokio::test]
    async fn an_entry_of_a_selected_group_composes_through_its_home_link() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let digest_root = tree.seed_digest_root(&tool);
        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![tool]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &digest_root),
            tree.entry(DEFAULT_GROUP, "cmake"),
            "C-065 — the following lane emits `<home>/links/<group>/<entry>`"
        );
    }

    /// **The discriminating case** — `ocx exec -g ci` heals the group it
    /// is about to emit, not the default one. The `ci/cmake` link is stale, the
    /// shape a branch switch leaves behind, and the default group holds
    /// a correct link so that a narrowed heal still finds work to do and still
    /// returns a non-zero repair count.
    ///
    /// RED: narrowing the heal back to `[DEFAULT_GROUP]` (the prompt path's
    /// scope leaking here). The stale `ci` link then survives, the probe refuses
    /// it, and the emitted path degrades to the digest root — so both
    /// assertions red, and the on-disk one names the previous package.
    #[tokio::test]
    async fn a_stale_link_in_a_non_default_selected_group_is_healed_before_it_is_probed() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let default_tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let current = locked_tool("cmake", "ci", "ns/cmake", 'b');
        let previous = locked_tool("cmake", "ci", "ns/cmake", 'c');

        let default_root = tree.seed_digest_root(&default_tool);
        let current_root = tree.seed_digest_root(&current);
        let previous_root = tree.seed_digest_root(&previous);
        tree.plant_link("ci", "cmake", &previous_root);

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![default_tool, current]),
            &["ci"],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &current_root),
            tree.entry("ci", "cmake"),
            "C-070 — the selected group's stale link is repaired, so the emitted path is the link"
        );
        assert_eq!(
            std::fs::read_link(tree.entry("ci", "cmake")).expect("the healed entry is a link"),
            current_root,
            "C-070 / S-006 — the repaired link names the current lock's digest, never the previous package"
        );
        assert_ne!(
            previous_root, current_root,
            "precondition: the stale target is a different real digest root"
        );
        assert!(
            !tree.entry(DEFAULT_GROUP, "cmake").exists(),
            "C-070 — the heal is scoped to the groups this invocation selected, and `default` is not one of them"
        );
        assert_eq!(
            tree.following(&paths, &default_root),
            default_root,
            "C-070 — an unselected group contributes no link to follow"
        );
    }

    /// The belt behind the composing heal: an entry the heal is not permitted
    /// to repair degrades to a **correct digest path**, per entry, and never
    /// fails the emission. A regular file where a link belongs is the shape
    /// `heal_links` leaves exactly as it found it, because it has no delete
    /// authority inside a repository-controlled tree.
    ///
    /// RED: emitting the `<home>/links/<group>/<entry>` spelling for every lock entry
    /// of a selected group regardless of what the probe answered — the emitted
    /// path would then name a regular file and every consumer would break.
    #[tokio::test]
    async fn an_entry_the_heal_cannot_repair_composes_on_the_digest_path() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let repairable = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let blocked = locked_tool("ninja", DEFAULT_GROUP, "ns/ninja", 'b');
        let repairable_root = tree.seed_digest_root(&repairable);
        let blocked_root = tree.seed_digest_root(&blocked);

        let blocked_entry = tree.entry(DEFAULT_GROUP, "ninja");
        std::fs::create_dir_all(blocked_entry.parent().expect("an entry has a group parent"))
            .expect("the group directory is creatable");
        std::fs::write(&blocked_entry, b"not a link").expect("the impostor file is writable");

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![repairable, blocked]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &blocked_root),
            blocked_root,
            "C-067 — the degrade is per entry and yields the digest root"
        );
        assert_eq!(
            tree.following(&paths, &repairable_root),
            tree.entry(DEFAULT_GROUP, "cmake"),
            "C-067 — its sibling in the same group still follows its link"
        );
    }

    /// A lock entry with no leaf compatible with the target
    /// platform has no link to name, so it composes on its digest path rather
    /// than failing the emission.
    ///
    /// RED: inventing a target for an incompatible entry (an exact-key lookup
    /// with a fallback, or a `unwrap_or_default` on `link_target`), which would
    /// emit a link path that nothing ever writes.
    #[tokio::test]
    async fn an_entry_with_no_lock_leaf_for_the_target_platform_composes_on_the_digest_path() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let reachable = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let foreign = locked_tool_for_another_platform("ninja", DEFAULT_GROUP, "ns/ninja", 'b');
        let reachable_root = tree.seed_digest_root(&reachable);
        let foreign_root = tree.file_structure.packages.path(
            &ocx_oci::PinnedPackageRef::try_from(
                ocx_oci::PackageRef::new_registry("ns/ninja", REGISTRY).clone_with_digest(digest_of('b')),
            )
            .expect("a digest-bearing identifier is pinned"),
        );

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![reachable, foreign]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &foreign_root),
            foreign_root,
            "C-067 — no compatible leaf means no link, and no link means the digest path"
        );
        assert!(
            !tree.entry(DEFAULT_GROUP, "ninja").exists(),
            "RUL-31 — nothing was written for an entry the lock does not ship here"
        );
        assert_eq!(
            tree.following(&paths, &reachable_root),
            tree.entry(DEFAULT_GROUP, "cmake"),
            "the compatible sibling still follows its link"
        );
    }

    /// The map is built from **the groups this invocation selected**,
    /// never from every group the lock happens to carry. A correct `ci` link on
    /// disk is not enough to put `ci` on the following lane for an invocation
    /// that selected only the default group.
    ///
    /// RED: iterating `lock.tools` wholesale instead of intersecting with
    /// `links.groups`.
    #[tokio::test]
    async fn a_group_the_invocation_did_not_select_composes_on_the_digest_path() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let selected = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let unselected = locked_tool("shellcheck", "ci", "ns/shellcheck", 'b');
        let selected_root = tree.seed_digest_root(&selected);
        let unselected_root = tree.seed_digest_root(&unselected);
        tree.plant_link("ci", "shellcheck", &unselected_root);

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![selected, unselected]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &unselected_root),
            unselected_root,
            "C-070 — an unselected group is not on the following lane, correct link or not"
        );
        assert_eq!(
            tree.following(&paths, &selected_root),
            tree.entry(DEFAULT_GROUP, "cmake"),
            "the selected group still follows its link"
        );
    }

    /// Two selected groups whose entries collapse to one digest key
    /// keep the entry from the **first group in `groups` order**, not the first
    /// in sorted order. Both links name the same directory, so the choice is
    /// observable only in the emitted spelling — which is exactly why it has to
    /// be deterministic rather than left to hash order.
    ///
    /// RED: last-write-wins insertion, or sorting the group set before the map
    /// is built (the heal's own `BTreeSet` ordering leaking into the map).
    #[tokio::test]
    async fn two_selected_groups_sharing_one_digest_keep_the_first_selected_groups_link() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let alpha = locked_tool("cmake", "alpha", "ns/cmake", 'a');
        let beta = locked_tool("cmake", "beta", "ns/cmake", 'a');
        let shared_root = tree.seed_digest_root(&alpha);
        assert_eq!(
            shared_root,
            tree.digest_root(&beta),
            "precondition: both lock entries collapse to one digest key"
        );

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![alpha, beta]),
            &["beta", "alpha"],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &shared_root),
            tree.entry("beta", "cmake"),
            "RUL-98 — the first group in `groups` order wins the tie"
        );
    }

    /// The discriminating leg — the slot goes to the first
    /// **trustworthy** candidate, not to the first candidate.
    ///
    /// The two cases above plant no links, so the composing heal makes *both*
    /// candidates trustworthy and a slot-reserving implementation — one that
    /// records the first candidate it walks and never revisits the key — passes
    /// them. Here the first selected group's entry is a regular file, the shape
    /// the heal has no authority to repair, so the two implementations disagree:
    /// reserving the slot emits the digest path, refusing a non-candidate emits
    /// the second group's link.
    ///
    /// RED: hoisting the `trusted.contains_key` skip above the trust probe, or
    /// inserting the key unconditionally.
    #[tokio::test]
    async fn a_refused_first_candidate_does_not_shadow_its_trustworthy_sibling() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let alpha = locked_tool("cmake", "alpha", "ns/cmake", 'a');
        let beta = locked_tool("cmake", "beta", "ns/cmake", 'a');
        let shared_root = tree.seed_digest_root(&alpha);
        assert_eq!(
            shared_root,
            tree.digest_root(&beta),
            "precondition: both lock entries collapse to one digest key"
        );

        // A regular file where `alpha`'s link belongs: `heal_links` leaves it
        // exactly as it found it, so the probe refuses it.
        let blocked = tree.entry("alpha", "cmake");
        std::fs::create_dir_all(blocked.parent().expect("an entry has a group parent"))
            .expect("the group directory is creatable");
        std::fs::write(&blocked, b"not a link").expect("the impostor file is writable");

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![alpha, beta]),
            &["alpha", "beta"],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &shared_root),
            tree.entry("beta", "cmake"),
            "RUL-98 — a candidate the probe refused is not a candidate, so the slot stays open for its sibling"
        );
    }

    // ── Block-1: the read path takes the heal's guards too ──────────────────

    /// A home reached through a symlinked project component is
    /// refused on the **read** path, not merely on the write path.
    ///
    /// `heal_links` reports `refuse_symlinked_project_path`'s refusal as
    /// [`HealOutcome::Refused`], which `resolve` turns into a digest-only
    /// composition. This test pins the **second**, independent guard inside
    /// `resolve`'s own blocking unit: without it a repository that commits
    /// `.ocx` as a symlink would need only the heal's verdict to go stale
    /// between the two for all four emitters to resolve `PATH` and
    /// `${installPath}` *through* that symlink.
    ///
    /// The link planted below is correct by every other test in this module, so
    /// only the guard can refuse it.
    ///
    /// RED: deleting the `refuse_symlinked_home` call from `resolve`'s blocking
    /// unit.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_home_behind_a_symlinked_project_component_composes_on_the_digest_path() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let digest_root = tree.seed_digest_root(&tool);

        // `<project>/.ocx` as a symlink — one component above the home root, the
        // hole `ensure_home_root` cannot see from where it stands.
        let elsewhere = tree.tmp.path().join("elsewhere");
        // The home root only: the group directory moved under `links/` and
        // `symlink::create` makes the entry's parents anyway, so pre-creating
        // it here would be a second spelling of the tree shape.
        std::fs::create_dir_all(elsewhere.join("toolchain")).expect("the relocated tree is creatable");
        ocx_util::fs::symlink::create(&elsewhere, tree.project_dir.join(".ocx"))
            .expect("the hostile link is creatable");

        let entry = tree.entry(DEFAULT_GROUP, "cmake");
        ocx_util::fs::symlink::create(&digest_root, &entry).expect("the fixture link is creatable");
        assert_eq!(
            std::fs::read_link(&entry).expect("the planted entry is a link"),
            digest_root,
            "precondition: the link is correct, so only the guard can refuse it"
        );

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![tool]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &digest_root),
            digest_root,
            "C-067 — a home the heal refuses to write is a home the composer refuses to read"
        );
    }

    /// A symlinked `<home>/<group>` directory is refused per entry.
    ///
    /// The sibling hole one level down, and the one `repoint_link`'s own
    /// refusal cannot close: when the entry underneath already reads back the
    /// correct target the heal has nothing to repair, so it never reaches that
    /// refusal and the group link is never examined by any write path.
    /// `link_is_trustworthy` probes only the leaf.
    ///
    /// RED: deleting the `entry.parent().is_some_and(is_link)` skip.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_entry_under_a_symlinked_group_directory_composes_on_the_digest_path() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let digest_root = tree.seed_digest_root(&tool);

        let elsewhere = tree.tmp.path().join("elsewhere-group");
        std::fs::create_dir_all(&elsewhere).expect("the relocated group is creatable");
        // The group directory lives under `links/` now, so the hostile link has
        // to be planted where the composer actually walks — through the
        // accessor, never a literal join.
        let group = tree
            .home
            .links_group(DEFAULT_GROUP)
            .expect("the default group is a valid component");
        std::fs::create_dir_all(group.parent().expect("<root>/links")).expect("the links directory is creatable");
        ocx_util::fs::symlink::create(&elsewhere, &group).expect("the hostile group link is creatable");

        let entry = tree.entry(DEFAULT_GROUP, "cmake");
        ocx_util::fs::symlink::create(&digest_root, &entry).expect("the fixture link is creatable");
        assert_eq!(
            std::fs::read_link(&entry).expect("the planted entry is a link"),
            digest_root,
            "precondition: the leaf is correct, so the heal has nothing to repair and only this guard can refuse it"
        );

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![tool]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &digest_root),
            digest_root,
            "C-067 — an entry the composer reaches through a symlinked group directory degrades"
        );
    }

    /// Second leg — within one group, the tie-break is the first entry
    /// in `lock.tools` order.
    ///
    /// RED: sorting the tools by name, or last-write-wins insertion. The names
    /// are deliberately in reverse alphabetical order in the lock, so either
    /// mutation returns `alpha`.
    #[tokio::test]
    async fn two_entries_of_one_group_sharing_one_digest_keep_the_first_in_lock_order() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let zeta = locked_tool("zeta", DEFAULT_GROUP, "ns/cmake", 'a');
        let alpha = locked_tool("alpha", DEFAULT_GROUP, "ns/cmake", 'a');
        let shared_root = tree.seed_digest_root(&zeta);

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![zeta, alpha]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &shared_root),
            tree.entry(DEFAULT_GROUP, "zeta"),
            "RUL-98 — first in `lock.tools` order, not first alphabetically"
        );
    }

    /// `${deps.*}` stays on digest paths. A dependency is not a lock
    /// entry, so no `<group>/<entry>` link exists for it and no trust test can
    /// be asked; the digest lane goes through the same seam but never
    /// consults the map, **even when the dependency's digest root happens to be
    /// one a selected group's link names**.
    ///
    /// RED: routing `build_dep_context_map` onto `PathLane::Following`. That
    /// mutation is invisible to every other case here, because every other case
    /// asks the following lane.
    #[tokio::test]
    async fn a_dependency_composes_on_the_digest_path_even_when_its_digest_root_carries_a_link() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let digest_root = tree.seed_digest_root(&tool);
        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![tool]),
            &[DEFAULT_GROUP],
        );

        let paths = tree.resolve(&links).await;

        assert_eq!(
            tree.following(&paths, &digest_root),
            tree.entry(DEFAULT_GROUP, "cmake"),
            "precondition: this very digest root is on the following lane"
        );
        assert_eq!(
            paths
                .install_path_for(&PackageDir::with_root(digest_root.clone()), PathLane::Digest)
                .root(),
            digest_root,
            "RUL-78 — the digest lane never consults the map"
        );
    }

    // ── The following lane through the emitters ──────────────────────────────────────────

    /// "a package's real bin directory **and every dereference value**"
    /// composed through the link: the root's declared `${installPath}/bin`
    /// carrier and its synthetic `entrypoints/` entry both name
    /// `<home>/links/<group>/<entry>/…` rather than the digest root.
    ///
    /// This is the case that proves the seam is wired into the emitters rather
    /// than merely computed: `ComposePaths` could resolve a perfect map and
    /// `compose` could still emit digest paths.
    ///
    /// RED: `compose_gated` passing `root.dir()` to the root's path block
    /// instead of `install_path_for(root.dir(), PathLane::Following)`.
    #[tokio::test]
    async fn a_following_root_emits_its_carrier_and_entrypoints_paths_under_the_home_link() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let digest_root = tree.seed_digest_root(&tool);

        let identifier = tool
            .repository
            .pin_untagged(tool.host_leaf(&platform()).expect("a compatible leaf"));

        let mut builder = metadata_env::EnvBuilder::new();
        builder.add_var(Var {
            key: "PATH".to_string(),
            modifier: Modifier::Path(ocx_package::metadata::env::path::Path {
                required: false,
                value: "${installPath}/bin".to_string(),
            }),
            visibility: Visibility::PUBLIC,
        });
        let root = std::sync::Arc::new(InstallInfo::new(
            identifier,
            metadata::Metadata::Bundle(bundle::Bundle {
                binaries: None,
                version: bundle::Version::V1,
                strip_components: None,
                env: builder.build(),
                dependencies: dependency::Dependencies::default(),
                entrypoints: Entrypoints::from_names(["cmake"]),
                integrations: Default::default(),
            }),
            ResolvedPackage::new(),
            PackageDir::with_root(digest_root.clone()),
        ));

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![tool]),
            &[DEFAULT_GROUP],
        );
        let paths = tree.resolve(&links).await;

        let out = compose(&[root], &tree.file_structure.packages, false, &paths)
            .await
            .expect("the fixture composes");
        let path_values: Vec<String> = out
            .entries
            .iter()
            .filter(|entry| entry.key == "PATH")
            .map(|entry| entry.value.clone())
            .collect();

        let link = PackageDir::with_root(tree.entry(DEFAULT_GROUP, "cmake"));
        // Built the way the code builds each value: the carrier is a template
        // substitution, so its separator is the template's own `/`; the
        // synthetic entry is a `join`, so its separator is the host's.
        let expected_carrier = format!("{}/bin", link.content().display());
        let expected_entrypoints = link.entrypoints().to_string_lossy().into_owned();

        assert!(
            path_values.contains(&expected_carrier),
            "C-065 — the declared `${{installPath}}/bin` carrier resolves under the home link; got {path_values:?}"
        );
        assert!(
            path_values.contains(&expected_entrypoints),
            "C-065 — the synthetic `entrypoints/` entry names the home link; got {path_values:?}"
        );
        assert!(
            !path_values
                .iter()
                .any(|value| value.starts_with(&*digest_root.to_string_lossy())),
            "C-065 — no emitted path keeps the digest spelling on the following lane; got {path_values:?}"
        );
    }

    /// An **integrations payload** is a dereference value too, so a
    /// root's `${installPath}` inside one resolves under the home link like
    /// every other emitted path. Integrations are not on the
    /// must-stay-digest list; a consumer that *persists* a payload pins it at
    /// that consumer, not here.
    ///
    /// The sibling case above proves the PATH block follows the link;
    /// `admitted_integrations` is resolved at a **different** site in
    /// `compose_gated` — its own `TemplateResolver`, built from its own content
    /// directory — so only this case can red on that site.
    ///
    /// RED: rebuilding the payload resolver's content directory from
    /// `root.dir()` instead of the `install_path_for` answer.
    #[tokio::test]
    async fn a_following_roots_integrations_payload_resolves_under_the_home_link() {
        let env = ocx_util::env::overrides::lock();
        env.remove(ocx_config::env::keys::OCX_TOOLCHAIN_PINNED);

        let tree = Tree::new();
        let tool = locked_tool("cmake", DEFAULT_GROUP, "ns/cmake", 'a');
        let digest_root = tree.seed_digest_root(&tool);

        let identifier = tool
            .repository
            .pin_untagged(tool.host_leaf(&platform()).expect("a compatible leaf"));

        let metadata: metadata::Metadata = serde_json::from_str(
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "integrations": { "vendor.example": { "toolPath": "${installPath}/bin/cmake" } },
            })
            .to_string(),
        )
        .expect("the fixture metadata parses");

        let root = std::sync::Arc::new(InstallInfo::new(
            identifier,
            metadata,
            ResolvedPackage::new(),
            PackageDir::with_root(digest_root.clone()),
        ));

        let links = tree.links(
            pinned_for_project(None, &config_with_pinned(Some(false))),
            lock_of(vec![tool]),
            &[DEFAULT_GROUP],
        );
        let paths = tree.resolve(&links).await;

        let out = compose(&[root], &tree.file_structure.packages, false, &paths)
            .await
            .expect("the fixture composes");

        let link = PackageDir::with_root(tree.entry(DEFAULT_GROUP, "cmake"));
        // Built the way the code builds it: the payload is a template
        // substitution, so its separator is the template's own `/`.
        let expected = format!("{}/bin/cmake", link.content().display());
        let payloads: Vec<serde_json::Value> = out
            .admitted_integrations
            .iter()
            .map(|(_, entry)| entry.payload.clone())
            .collect();

        assert_eq!(
            payloads,
            vec![serde_json::json!({ "toolPath": expected })],
            "RUL-97 — the payload's `${{installPath}}` names the home link, not the digest root"
        );
    }
}
