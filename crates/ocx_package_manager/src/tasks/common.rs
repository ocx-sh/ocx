// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared utilities for task modules.
//!
//! Every `current` symlink mutation ([`wire_selection`], `deselect`, `uninstall --deselect`) holds the
//! per-repo `.select.lock`, or two of them race on the same symlink.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tokio::task::JoinSet;

use crate::{error, error::PackageError, error::PackageErrorKind};
use ocx_oci::{self, media_type::MEDIA_TYPE_PACKAGE_METADATA_V1, media_type::media_type_select};
use ocx_package::{install_info::InstallInfo, metadata, resolved_package::ResolvedPackage};
use ocx_store::{file_structure, file_structure::PackageStore, reference_manager::ReferenceManager};

use ocx_util::fs::LockedFile;
use ocx_util::prelude::SerdeExt;

/// Finds a package in the object store without index resolution; `None` if absent.
///
/// On-disk metadata is re-validated through [`metadata::ValidMetadata`], as it may be stale or tampered.
pub async fn find_in_store(
    objects: &PackageStore,
    identifier: &ocx_oci::PinnedPackageRef,
) -> Result<Option<InstallInfo>, PackageErrorKind> {
    let pkg = objects.package_dir(identifier);
    let content = pkg.content();
    let metadata_path = pkg.metadata();
    let resolve_path = pkg.resolve();
    if ocx_util::fs::path_exists_lossy(&content).await
        && ocx_util::fs::path_exists_lossy(&metadata_path).await
        && ocx_util::fs::path_exists_lossy(&resolve_path).await
    {
        let (metadata_result, resolved_result): (
            ocx_util::error::Result<metadata::Metadata>,
            ocx_util::error::Result<ResolvedPackage>,
        ) = tokio::join!(
            metadata::Metadata::read_json(&metadata_path),
            ResolvedPackage::read_json(&resolve_path),
        );
        let metadata = metadata_result.map_err(|error| PackageErrorKind::Internal(error.into()))?;
        let metadata = metadata::ValidMetadata::try_from(metadata)
            .map_err(|error| PackageErrorKind::Internal(error.into()))?
            .into();
        let resolved = resolved_result.map_err(|error| PackageErrorKind::Internal(error.into()))?;
        Ok(Some(InstallInfo::new(identifier.clone(), metadata, resolved, pkg)))
    } else {
        Ok(None)
    }
}

/// Reconstructs the [`PinnedPackageRef`](ocx_oci::PinnedPackageRef) behind an install link.
///
/// `Current` drops the caller's tag, which `current` may not hold, or it fabricates a never-installed identifier.
pub async fn identifier_for_symlink(
    objects: &PackageStore,
    symlink_path: &Path,
    identifier: &ocx_oci::PackageRef,
    source: &crate::composer::LinkSource,
) -> Result<ocx_oci::PinnedPackageRef, crate::Error> {
    let digest_path = objects.digest_file_for_content(symlink_path)?;
    let digest = file_structure::read_digest_file(&digest_path).await?;
    let base = match source {
        crate::composer::LinkSource::Current => identifier.without_tag(),
        crate::composer::LinkSource::Candidate | crate::composer::LinkSource::Path(_) => identifier.clone(),
    };
    Ok(ocx_oci::PinnedPackageRef::try_from(base.clone_with_digest(digest))?)
}

/// Whether `path` resolves to a directory inside the package store; false on any resolution failure.
pub(crate) fn leads_into_store(objects: &PackageStore, path: &Path) -> bool {
    let (Ok(target), Ok(root)) = (dunce::canonicalize(path), dunce::canonicalize(objects.root())) else {
        return false;
    };
    target.starts_with(root)
}

/// Loads metadata.json and resolve.json for an object path or install symlink.
///
/// Only structural readability is checked, never publish-time tokens, or metadata from a newer ocx fails to load.
pub async fn load_object_data(
    objects: &PackageStore,
    content_path: &Path,
) -> Result<(metadata::Metadata, ResolvedPackage), crate::Error> {
    let metadata_path = objects.metadata_for_content(content_path)?;
    let resolve_path = objects.resolve_for_content(content_path)?;
    let (metadata_result, resolved_result): (
        ocx_util::error::Result<metadata::Metadata>,
        ocx_util::error::Result<ResolvedPackage>,
    ) = tokio::join!(
        metadata::Metadata::read_json(&metadata_path),
        ResolvedPackage::read_json(&resolve_path),
    );
    let metadata = metadata::ValidMetadata::try_from(metadata_result?)?.into();
    Ok((metadata, resolved_result?))
}

/// Cap on a metadata config blob's declared and fetched size (`adr_inspect_metadata_closure.md` D5).
pub(super) const MAX_METADATA_BLOB_BYTES: usize = 4 * 1024 * 1024;

/// Fetches, media-type checks and validates the OCX metadata config blob `manifest` references.
pub async fn load_config_metadata(
    index: &ocx_index::Index,
    pinned: &ocx_oci::PinnedPackageRef,
    manifest: &ocx_oci::ImageManifest,
) -> Result<metadata::ValidMetadata, PackageErrorKind> {
    // Before any fetch, or a wrong-media-type blob is staged into the local CAS.
    media_type_select(&manifest.config.media_type, &[MEDIA_TYPE_PACKAGE_METADATA_V1])
        .map_err(|e| PackageErrorKind::Internal(e.into()))?;

    // Reject an over-cap declared size before touching the network or the cache.
    if manifest.config.size < 0 || manifest.config.size as u64 > MAX_METADATA_BLOB_BYTES as u64 {
        return Err(PackageErrorKind::Internal(crate::Error::MetadataBlobTooLarge {
            size: manifest.config.size,
            max: MAX_METADATA_BLOB_BYTES,
        }));
    }

    let config_digest =
        ocx_oci::Digest::try_from(manifest.config.digest.as_str()).map_err(|e| PackageErrorKind::Internal(e.into()))?;
    let config_ref = pinned.clone_with_digest(config_digest);
    let bytes = match index
        .fetch_blob(&config_ref)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        Some(bytes) => bytes,
        None => {
            // Offline, or after `ocx index update` (which skips config blobs): name the missing digest.
            return Err(PackageErrorKind::OfflineManifestMissing(Box::new(
                error::OfflineManifestMissing {
                    identifier: pinned.as_identifier().clone(),
                    digest: config_ref.digest(),
                },
            )));
        }
    };

    // Re-check: a registry may declare a small size but serve a larger body.
    if bytes.len() > MAX_METADATA_BLOB_BYTES {
        return Err(PackageErrorKind::Internal(crate::Error::MetadataBlobTooLarge {
            size: bytes.len() as i64,
            max: MAX_METADATA_BLOB_BYTES,
        }));
    }

    let raw: metadata::Metadata = serde_json::from_slice(&bytes)
        .map_err(|e| PackageErrorKind::Internal(crate::Error::SerializationFailure(e)))?;
    // Structural checks only: refusing unknown tokens here would hide packages a newer ocx published.
    metadata::ValidMetadata::try_from(raw).map_err(|error| PackageErrorKind::Internal(error.into()))
}

/// Drains a [`JoinSet`] of package tasks in `packages` order, batching errors through `error_ctor`.
///
/// A task that never reports back is recorded as [`PackageErrorKind::TaskPanicked`].
pub async fn drain_package_tasks<T: 'static>(
    packages: &[ocx_oci::PackageRef],
    mut tasks: JoinSet<(ocx_oci::PackageRef, Result<T, PackageErrorKind>)>,
    error_ctor: fn(Vec<PackageError>) -> crate::error::Error,
) -> Result<Vec<T>, crate::error::Error> {
    let index_map: HashMap<ocx_oci::PackageRef, usize> =
        packages.iter().cloned().enumerate().map(|(i, id)| (id, i)).collect();

    let mut pending: HashSet<ocx_oci::PackageRef> = packages.iter().cloned().collect();
    let mut results: Vec<Option<T>> = std::iter::repeat_with(|| None).take(packages.len()).collect();
    // Sorted by input slot before surfacing, or the classifier's `errors.first()` follows completion order.
    let mut errors: Vec<(usize, PackageError)> = Vec::new();

    while let Some(join_result) = tasks.join_next().await {
        match join_result {
            Ok((id, Ok(value))) => {
                pending.remove(&id);
                if let Some(&idx) = index_map.get(&id) {
                    results[idx] = Some(value);
                }
            }
            Ok((id, Err(kind))) => {
                pending.remove(&id);
                let idx = index_map.get(&id).copied().unwrap_or(usize::MAX);
                errors.push((idx, PackageError::new(id, kind)));
            }
            Err(e) => log::error!("Task panicked: {}", e),
        }
    }

    for id in pending {
        let idx = index_map.get(&id).copied().unwrap_or(usize::MAX);
        errors.push((idx, PackageError::new(id, PackageErrorKind::TaskPanicked)));
    }

    if !errors.is_empty() {
        errors.sort_by_key(|(idx, _)| *idx);
        let errors: Vec<PackageError> = errors.into_iter().map(|(_, error)| error).collect();
        return Err(error_ctor(errors));
    }

    Ok(results.into_iter().flatten().collect())
}

/// Resolves the top-level manifest for `package` without platform selection; a bare repository means `latest`.
///
/// # Errors
///
/// - [`PackageErrorKind::NotFound`] — tag/digest unknown.
/// - [`PackageErrorKind::OfflineManifestMissing`] — tag known, manifest blob not cached (offline).
/// - [`PackageErrorKind::Internal`] — index I/O failure.
/// - [`PackageErrorKind::DigestMissing`] — the top-level digest could not be pinned.
pub async fn resolve_top_manifest(
    index: &ocx_index::Index,
    package: &ocx_oci::PackageRef,
    op: ocx_index::IndexOperation,
) -> Result<(ocx_oci::PinnedPackageRef, ocx_oci::Manifest), PackageErrorKind> {
    let top_id = if package.digest().is_some() {
        package.clone()
    } else {
        package.clone_with_tag(package.tag_or_latest())
    };
    let (top_digest, top_manifest) = match index
        .fetch_manifest(&top_id, op)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        Some(result) => result,
        None => {
            // A resolvable tag digest means the tag is known and only the manifest blob is missing.
            if let Some(digest) = index
                .fetch_manifest_digest(&top_id, op)
                .await
                .map_err(|error| PackageErrorKind::Internal(error.into()))?
            {
                return Err(PackageErrorKind::OfflineManifestMissing(Box::new(
                    error::OfflineManifestMissing {
                        identifier: top_id.clone(),
                        digest,
                    },
                )));
            }
            return Err(PackageErrorKind::NotFound);
        }
    };

    let top_pinned = ocx_oci::PinnedPackageRef::try_from(top_id.clone_with_digest(top_digest))
        .map_err(|_| PackageErrorKind::DigestMissing)?;
    Ok((top_pinned, top_manifest))
}

/// Creates a [`ReferenceManager`] from a [`FileStructure`].
pub fn reference_manager(fs: &file_structure::FileStructure) -> ReferenceManager {
    ReferenceManager::new(fs.clone())
}

/// Returns `true` when `identifier`'s blob must be fetched, first removing a copy failing its digest (CWE-345).
pub async fn blob_needs_fetch(
    fs: &file_structure::FileStructure,
    identifier: &ocx_oci::PinnedPackageRef,
) -> Result<bool, PackageErrorKind> {
    // Local before remote: `ocx package test` stages synthesized manifests no registry has.
    let digest = identifier.digest();
    match fs
        .blobs
        .read_blob(identifier.registry(), &digest)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        Some(existing) if digest.algorithm().hash(&existing) == digest => Ok(false),
        Some(_) => {
            log::warn!("blob-store copy of chain blob '{digest}' is corrupt; removing and re-fetching");
            fs.blobs
                .remove_blob(identifier.registry(), &digest)
                .await
                .map_err(|error| PackageErrorKind::Internal(error.into()))?;
            Ok(true)
        }
        None => Ok(true),
    }
}

/// Verifies `bytes` hash to `identifier`'s digest before they enter the CAS (CWE-345).
///
/// [`Index::fetch_manifest_raw_bytes`] never checks the requested digest; every caller persisting its bytes calls this.
pub(super) fn verify_requested_digest(
    identifier: &ocx_oci::PinnedPackageRef,
    bytes: &[u8],
) -> Result<(), PackageErrorKind> {
    let claimed = identifier.digest();
    let computed = claimed.algorithm().hash(bytes);
    if computed != claimed {
        return Err(PackageErrorKind::Internal(
            ocx_store::file_structure::error::Error::DigestMismatch { claimed, computed }.into(),
        ));
    }
    Ok(())
}

/// Stages every blob in `resolved.chain` into `fs.blobs`, without ref-linking.
pub async fn stage_chain_blobs(
    fs: &file_structure::FileStructure,
    index: &ocx_index::Index,
    resolved: &super::resolve::ResolvedChain,
) -> Result<(), PackageErrorKind> {
    use super::resolve::ChainRole;

    for blob in &resolved.chain {
        let identifier = &blob.identifier;
        if !blob_needs_fetch(fs, identifier).await? {
            continue;
        }
        match blob.role {
            ChainRole::Config => {
                if let Some(bytes) = index
                    .fetch_blob(identifier)
                    .await
                    .map_err(|error| PackageErrorKind::Internal(error.into()))?
                {
                    fs.blobs
                        .write_blob(identifier.registry(), &identifier.digest(), &bytes)
                        .await
                        .map_err(|error| PackageErrorKind::Internal(error.into()))?;
                }
            }
            // Manifests endpoint: the blobs endpoint does not serve manifest digests.
            ChainRole::Index | ChainRole::Manifest => {
                if let Some((bytes, _, _)) = index
                    .fetch_manifest_raw_bytes(identifier.as_identifier())
                    .await
                    .map_err(|error| PackageErrorKind::Internal(error.into()))?
                {
                    verify_requested_digest(identifier, &bytes)?;
                    fs.blobs
                        .write_blob(identifier.registry(), &identifier.digest(), &bytes)
                        .await
                        .map_err(|error| PackageErrorKind::Internal(error.into()))?;
                }
            }
        }
    }
    Ok(())
}

/// Stages the resolver's chain into `$OCX_HOME/blobs` and forward-refs each blob into the package's `refs/blobs/`.
///
/// Idempotent; a blob the index cannot serve (offline) is skipped, leaving a dangling ref for GC.
pub async fn stage_and_link_chain_blobs(
    fs: &file_structure::FileStructure,
    index: &ocx_index::Index,
    content_path: &Path,
    resolved: &super::resolve::ResolvedChain,
) -> Result<(), PackageErrorKind> {
    stage_chain_blobs(fs, index, resolved).await?;
    reference_manager(fs)
        .link_blobs(content_path, resolved.blobs())
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))
}

/// Acquires the per-repo `.select.lock` for `package`, released on drop.
pub async fn acquire_select_lock(
    fs: &file_structure::FileStructure,
    package: &ocx_oci::PackageRef,
) -> Result<LockedFile, PackageErrorKind> {
    let lock_path = fs.symlinks.select_lock(package);
    LockedFile::open_exclusive(lock_path)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))
}

/// Outcome of [`wire_selection`]: each field is `Some` only when that symlink was written this call.
#[derive(Debug, Clone, Default)]
pub struct WireSelectionOutcome {
    /// `None` when `select` was not requested or the platform is not host-runnable.
    pub current: Option<std::path::PathBuf>,
    /// `None` when no candidate was requested or the platform is not host-runnable.
    pub candidate: Option<std::path::PathBuf>,
}

/// Wires the candidate and/or `current` symlinks for `package` at the package root.
///
/// Entrypoint collisions are not checked here; [`super::super::composer::check_entrypoints`] does that at install.
///
/// # Errors
///
/// - [`PackageErrorKind::Internal`] for I/O or symlink failures, or a deferred package.
#[allow(clippy::result_large_err)]
pub async fn wire_selection(
    fs: &file_structure::FileStructure,
    package: &ocx_oci::PackageRef,
    info: &InstallInfo,
    candidate: bool,
    select: bool,
) -> Result<WireSelectionOutcome, PackageErrorKind> {
    let rm = reference_manager(fs);

    let pkg_root = info.dir().dir.as_path();

    // A deferred package has no directory yet, so a symlink to it would publish a dangling install.
    if info.deferred().is_some() {
        return Err(PackageErrorKind::Internal(crate::error::file_error(
            pkg_root,
            std::io::Error::other("refusing to point an install symlink at a deferred package"),
        )));
    }

    // Platformless readers (`ocx package which`, project env) expect host-runnable content behind both symlinks.
    let host_runnable = info.is_host_runnable();

    let candidate_written = if candidate && host_runnable {
        let link_path = fs.symlinks.candidate(package);
        log::debug!("Creating candidate symlink at '{}'.", link_path.display());
        rm.link(&link_path, pkg_root)
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
        Some(link_path)
    } else {
        if candidate {
            log::debug!(
                "Skipping candidate symlink for '{}': resolved platform {:?} is not host-runnable (issue #179).",
                package,
                info.platform(),
            );
        }
        None
    };

    if !select {
        return Ok(WireSelectionOutcome {
            current: None,
            candidate: candidate_written,
        });
    }

    if !host_runnable {
        log::debug!(
            "Skipping current symlink for '{}': resolved platform {:?} is not host-runnable (issue #179).",
            package,
            info.platform(),
        );
        return Ok(WireSelectionOutcome {
            current: None,
            candidate: candidate_written,
        });
    }

    let current_path = fs.symlinks.current(package);

    // Named, not `_`: held through the write and its rollback.
    let _select_guard = acquire_select_lock(fs, package).await?;

    let prior_current_target = tokio::fs::read_link(&current_path).await.ok();

    log::debug!("Creating current symlink at '{}'.", current_path.display());
    if let Err(e) = rm.link(&current_path, pkg_root) {
        rollback_symlink(&rm, &current_path, prior_current_target.as_deref());
        return Err(PackageErrorKind::Internal(e.into()));
    }

    Ok(WireSelectionOutcome {
        current: Some(current_path),
        candidate: candidate_written,
    })
}

/// RAII guard for the per-repo `.select.lock`; releases on drop.
pub struct SelectionLocks {
    _select: LockedFile,
}

/// Acquires the per-repo `.select.lock` as a [`SelectionLocks`] guard.
#[allow(clippy::result_large_err)]
pub async fn acquire_selection_locks(
    fs: &file_structure::FileStructure,
    package: &ocx_oci::PackageRef,
) -> Result<SelectionLocks, PackageErrorKind> {
    let select = acquire_select_lock(fs, package).await?;
    Ok(SelectionLocks { _select: select })
}

/// Restores a symlink to its prior state; a rollback failure is only logged, keeping the caller's error as root cause.
pub fn rollback_symlink(rm: &ReferenceManager, forward_path: &Path, prior_target: Option<&Path>) {
    match prior_target {
        Some(target) => {
            if let Err(rollback_err) = rm.link(forward_path, target) {
                log::warn!(
                    "Failed to roll back symlink at '{}' to prior target '{}': {}",
                    forward_path.display(),
                    target.display(),
                    rollback_err,
                );
            }
        }
        None => {
            if let Err(rollback_err) = rm.unlink(forward_path) {
                log::warn!(
                    "Failed to roll back (unlink) symlink at '{}': {}",
                    forward_path.display(),
                    rollback_err,
                );
            }
        }
    }
}

/// Caps how many per-node fetch tasks [`gather_closure_nodes`] has spawned at once.
pub(super) const CLOSURE_FETCH_CONCURRENCY: usize = 8;

/// One node of a metadata-only dependency closure.
#[derive(Debug)]
pub struct ClosureNode {
    /// Digest-addressed; the tag is advisory, for display.
    pub identifier: ocx_oci::PinnedPackageRef,
    /// Digest of the node's metadata config blob, in [`identifier`](Self::identifier)'s registry.
    ///
    /// A deferred tool has no package directory, so this is the only route to its config blob.
    pub config_digest: ocx_oci::Digest,
    /// Composed from the root; `None` iff `is_root`.
    pub effective_visibility: Option<metadata::visibility::Visibility>,
    /// `None` means undeclared; `Some(empty)` asserts zero interface executables.
    pub binaries: Option<metadata::Binaries>,
    pub entrypoints: Vec<metadata::EntrypointName>,
    /// The node's own env vars, with declared visibility.
    pub env: Vec<ClosureEnvVar>,
    /// Declared integration namespace keys, in `BTreeMap` order.
    pub integrations: Vec<String>,
    pub dependencies: Vec<ClosureEdge>,
    pub is_root: bool,
}

/// One declared env var of a [`ClosureNode`]; no value, as templated values are only concrete after install.
#[derive(Debug, Clone)]
pub struct ClosureEnvVar {
    pub key: String,
    pub kind: metadata::env::modifier::ModifierKind,
    /// The declared separator of a `list`-kind var; `None` for every other kind.
    pub separator: Option<String>,
    pub visibility: metadata::visibility::Visibility,
}

/// A declared dependency edge, as authored.
#[derive(Debug, Clone)]
pub struct ClosureEdge {
    pub identifier: ocx_oci::PinnedPackageRef,
    /// Declared edge visibility, not [`ClosureNode::effective_visibility`].
    pub visibility: metadata::visibility::Visibility,
    pub name: metadata::dependency::DependencyName,
}

/// Resolved identity (the platform-selected child for an image-index dep), config digest, metadata, declared edges.
type GatheredClosureNode = (
    ocx_oci::PinnedPackageRef,
    ocx_oci::Digest,
    metadata::ValidMetadata,
    Vec<ClosureEdge>,
);

/// A [`GatheredClosureNode`] tagged with its spawn slot and the edge's declared identity.
type SlottedClosureNode = (
    usize,
    ocx_oci::PinnedPackageRef,
    ocx_oci::PinnedPackageRef,
    ocx_oci::Digest,
    metadata::ValidMetadata,
    Vec<ClosureEdge>,
);

/// Stages `pinned`'s raw manifest bytes into the blob store when not already local, without ref-linking.
///
/// # Errors
///
/// Returns an error if the manifest cannot be fetched, fails its digest check, or cannot be written.
pub async fn stage_leaf_manifest(
    fs: &file_structure::FileStructure,
    index: &ocx_index::Index,
    pinned: &ocx_oci::PinnedPackageRef,
) -> Result<(), PackageErrorKind> {
    if blob_needs_fetch(fs, pinned).await?
        && let Some((bytes, _, _)) = index
            .fetch_manifest_raw_bytes(pinned.as_identifier())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        verify_requested_digest(pinned, &bytes)?;
        fs.blobs
            .write_blob(pinned.registry(), &pinned.digest(), &bytes)
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    }
    Ok(())
}

/// Walks the metadata-only dependency closure: deduped nodes, deps before dependents, root last.
///
/// Diamonds merge to their most-open visibility; one node error aborts the whole closure.
///
/// # Errors
///
/// See `adr_inspect_metadata_closure.md` Error Taxonomy: offline miss → `Internal(OfflineMode)`;
/// absent with a source consulted → `NotFound`; bad config → [`load_config_metadata`]'s errors;
/// no platform match → `FeatureMismatch`.
pub async fn walk_closure_nodes(
    fs: &file_structure::FileStructure,
    index: &ocx_index::Index,
    offline: bool,
    root_pinned: &ocx_oci::PinnedPackageRef,
    root_metadata: &metadata::ValidMetadata,
    root_config_digest: ocx_oci::Digest,
    platform: &ocx_oci::Platform,
) -> Result<Vec<ClosureNode>, PackageErrorKind> {
    let frontier = closure_edges_from_metadata(root_metadata);
    let (gathered, resolved_identity) = gather_closure_nodes(fs, index, offline, frontier, platform).await?;
    Ok(fold_effective_visibility(
        root_pinned,
        root_metadata,
        root_config_digest,
        gathered,
        &resolved_identity,
    ))
}

/// Per-gather invariants shared by every spawned fetch.
struct GatherContext<'a> {
    fs: &'a file_structure::FileStructure,
    index: &'a ocx_index::Index,
    offline: bool,
    platform: &'a ocx_oci::Platform,
}

/// Parallel BFS metadata gather; returns nodes deduped by resolved identity plus the declared→resolved alias map.
async fn gather_closure_nodes(
    fs: &file_structure::FileStructure,
    index: &ocx_index::Index,
    offline: bool,
    frontier: Vec<ClosureEdge>,
    platform: &ocx_oci::Platform,
) -> Result<
    (
        Vec<GatheredClosureNode>,
        HashMap<ocx_oci::PinnedPackageRef, ocx_oci::PinnedPackageRef>,
    ),
    PackageErrorKind,
> {
    let context = GatherContext {
        fs,
        index,
        offline,
        platform,
    };
    let mut visited: HashSet<ocx_oci::PinnedPackageRef> = HashSet::new();
    let mut tasks: JoinSet<Result<SlottedClosureNode, PackageErrorKind>> = JoinSet::new();
    let mut next_slot = 0usize;
    // Slots are assigned at discovery, not admission, or the output order follows scheduling.
    let mut pending: std::collections::VecDeque<(usize, ClosureEdge)> = std::collections::VecDeque::new();

    fn spawn(
        tasks: &mut JoinSet<Result<SlottedClosureNode, PackageErrorKind>>,
        context: &GatherContext<'_>,
        slot: usize,
        edge: ClosureEdge,
    ) {
        let fs = context.fs.clone();
        let index = context.index.clone();
        let offline = context.offline;
        let platform = context.platform.clone();
        tasks.spawn(async move {
            let declared = edge.identifier.clone();
            let (resolved_pinned, config_digest, metadata, edges) =
                fetch_closure_node(&fs, &index, offline, &declared, &platform).await?;
            Ok((slot, declared, resolved_pinned, config_digest, metadata, edges))
        });
    }

    // Bounds spawned tasks, not just fetch bodies: a `Semaphore` inside each task would still spawn the whole frontier.
    fn admit(
        tasks: &mut JoinSet<Result<SlottedClosureNode, PackageErrorKind>>,
        context: &GatherContext<'_>,
        pending: &mut std::collections::VecDeque<(usize, ClosureEdge)>,
    ) {
        while tasks.len() < CLOSURE_FETCH_CONCURRENCY {
            let Some((slot, edge)) = pending.pop_front() else {
                break;
            };
            spawn(tasks, context, slot, edge);
        }
    }

    for edge in frontier {
        if visited.insert(edge.identifier.strip_advisory()) {
            let slot = next_slot;
            next_slot += 1;
            pending.push_back((slot, edge));
        }
    }
    admit(&mut tasks, &context, &mut pending);

    let mut slots: Vec<Option<GatheredClosureNode>> = Vec::new();
    // A `ClosureEdge` names the declared identity, which differs from the resolved node for an image-index dep.
    let mut resolved_identity: HashMap<ocx_oci::PinnedPackageRef, ocx_oci::PinnedPackageRef> = HashMap::new();
    // Two declared edges can resolve to one digest; without this dedup claims double-count downstream.
    let mut resolved_seen: HashSet<ocx_oci::PinnedPackageRef> = HashSet::new();
    while let Some(joined) = tasks.join_next().await {
        let (slot, declared, resolved_pinned, config_digest, metadata, edges) =
            joined.map_err(|_| PackageErrorKind::TaskPanicked)??;
        resolved_identity.insert(declared.strip_advisory(), resolved_pinned.strip_advisory());

        for child_edge in &edges {
            if visited.insert(child_edge.identifier.strip_advisory()) {
                let slot = next_slot;
                next_slot += 1;
                pending.push_back((slot, child_edge.clone()));
            }
        }

        if resolved_seen.insert(resolved_pinned.strip_advisory()) {
            if slot >= slots.len() {
                slots.resize_with(slot + 1, || None);
            }
            slots[slot] = Some((resolved_pinned, config_digest, metadata, edges));
        }

        admit(&mut tasks, &context, &mut pending);
    }

    Ok((slots.into_iter().flatten().collect(), resolved_identity))
}

/// Fetches one closure node; a dep pinned to an image index resolves to its platform-selected child.
async fn fetch_closure_node(
    fs: &file_structure::FileStructure,
    index: &ocx_index::Index,
    offline: bool,
    dep_pinned: &ocx_oci::PinnedPackageRef,
    platform: &ocx_oci::Platform,
) -> Result<
    (
        ocx_oci::PinnedPackageRef,
        ocx_oci::Digest,
        metadata::ValidMetadata,
        Vec<ClosureEdge>,
    ),
    PackageErrorKind,
> {
    let dep_identifier = dep_pinned.as_identifier().clone();
    let manifest = match index
        .fetch_manifest(&dep_identifier, ocx_index::IndexOperation::Resolve)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        Some((_, manifest)) => manifest,
        None => return Err(closure_fetch_miss(offline)),
    };

    let (resolved_pinned, image) = match manifest {
        ocx_oci::Manifest::Image(img) => (dep_pinned.clone(), img),
        ocx_oci::Manifest::ImageIndex(_) => {
            let selected = match index
                .select(&dep_identifier, platform, ocx_index::IndexOperation::Resolve)
                .await
                .map_err(|error| PackageErrorKind::Internal(error.into()))?
            {
                ocx_index::SelectResult::Found(id) => id,
                ocx_index::SelectResult::Ambiguous(candidates) => {
                    return Err(PackageErrorKind::SelectionAmbiguous(candidates));
                }
                ocx_index::SelectResult::NotFound => return Err(PackageErrorKind::NotFound),
                ocx_index::SelectResult::FeatureMismatch {
                    host_features,
                    available,
                } => {
                    return Err(PackageErrorKind::FeatureMismatch {
                        host_features,
                        available,
                    });
                }
            };
            let child_pinned =
                ocx_oci::PinnedPackageRef::try_from(selected.clone()).map_err(|_| PackageErrorKind::DigestMissing)?;
            let image = match index
                .fetch_manifest(&selected, ocx_index::IndexOperation::Resolve)
                .await
                .map_err(|error| PackageErrorKind::Internal(error.into()))?
            {
                Some((_, ocx_oci::Manifest::Image(img))) => img,
                Some((_, ocx_oci::Manifest::ImageIndex(_))) | None => return Err(closure_fetch_miss(offline)),
            };
            (child_pinned, image)
        }
    };

    stage_leaf_manifest(fs, index, &resolved_pinned).await?;

    let config_digest = config_blob_digest(&image)?;
    let metadata = load_config_metadata(index, &resolved_pinned, &image).await?;
    let edges = closure_edges_from_metadata(&metadata);
    Ok((resolved_pinned, config_digest, metadata, edges))
}

/// The digest of an image manifest's config descriptor, the OCX metadata blob.
///
/// # Errors
///
/// Returns an error if the config descriptor's digest string is malformed.
pub fn config_blob_digest(image: &ocx_oci::ImageManifest) -> Result<ocx_oci::Digest, PackageErrorKind> {
    ocx_oci::Digest::try_from(image.config.digest.as_str())
        .map_err(|e| PackageErrorKind::Internal(crate::Error::from(e)))
}

/// A closure manifest miss: an offline policy block, or a genuine not-found when a source could be consulted.
fn closure_fetch_miss(offline: bool) -> PackageErrorKind {
    if offline {
        PackageErrorKind::Internal(crate::Error::OfflineMode)
    } else {
        PackageErrorKind::NotFound
    }
}

/// A node's declared dependency edges, as authored.
fn closure_edges_from_metadata(metadata: &metadata::ValidMetadata) -> Vec<ClosureEdge> {
    metadata
        .dependencies()
        .iter()
        .map(|dep| ClosureEdge {
            identifier: dep.identifier.clone(),
            visibility: dep.visibility,
            name: dep.name(),
        })
        .collect()
}

/// Pure visibility fold, the same algorithm as
/// [`ocx_package::resolved_package::ResolvedPackage::with_dependencies`], over gathered metadata.
fn fold_effective_visibility(
    root_pinned: &ocx_oci::PinnedPackageRef,
    root_metadata: &metadata::ValidMetadata,
    root_config_digest: ocx_oci::Digest,
    gathered: Vec<GatheredClosureNode>,
    resolved_identity: &HashMap<ocx_oci::PinnedPackageRef, ocx_oci::PinnedPackageRef>,
) -> Vec<ClosureNode> {
    let by_identity: HashMap<ocx_oci::PinnedPackageRef, GatheredClosureNode> = gathered
        .into_iter()
        .map(|entry| (entry.0.strip_advisory(), entry))
        .collect();

    // Post-order: each node's `ResolvedPackage` needs its children's first.
    let mut resolved: HashMap<ocx_oci::PinnedPackageRef, ResolvedPackage> = HashMap::new();
    let mut order: Vec<ocx_oci::PinnedPackageRef> = Vec::new();
    let root_edges = closure_edges_from_metadata(root_metadata);
    for edge in &root_edges {
        let resolved_key = resolved_edge_identity(resolved_identity, edge);
        visit_closure_node(
            &resolved_key,
            &by_identity,
            resolved_identity,
            &mut resolved,
            &mut order,
        );
    }

    // Keyed by resolved identity, or an image-index dep fragments into two packages.
    let root_children: Vec<(
        ocx_oci::PinnedPackageRef,
        ResolvedPackage,
        metadata::visibility::Visibility,
    )> = root_edges
        .iter()
        .map(|edge| {
            let resolved_key = resolved_edge_identity(resolved_identity, edge);
            let child_resolved = resolved.get(&resolved_key).cloned().unwrap_or_default();
            (resolved_key, child_resolved, edge.visibility)
        })
        .collect();
    let effective: HashMap<ocx_oci::PinnedPackageRef, metadata::visibility::Visibility> = ResolvedPackage::new()
        .with_dependencies(root_children)
        .dependencies
        .into_iter()
        .map(|dep| (dep.identifier.strip_advisory(), dep.visibility))
        .collect();

    let mut nodes: Vec<ClosureNode> = order
        .into_iter()
        .map(|key| {
            let (identifier, config_digest, metadata, edges) = by_identity
                .get(&key)
                .expect("fold_effective_visibility only visits keys populated by gather_closure_nodes");
            let effective_visibility = Some(
                *effective
                    .get(&key)
                    .expect("every gathered node is reachable from root by construction"),
            );
            ClosureNode {
                identifier: identifier.clone(),
                config_digest: config_digest.clone(),
                effective_visibility,
                binaries: metadata.binaries().cloned(),
                entrypoints: metadata
                    .entrypoints()
                    .map(|entries| entries.names().cloned().collect())
                    .unwrap_or_default(),
                env: closure_env_vars(metadata),
                integrations: closure_integrations(metadata),
                dependencies: edges.clone(),
                is_root: false,
            }
        })
        .collect();

    nodes.push(ClosureNode {
        identifier: root_pinned.clone(),
        config_digest: root_config_digest,
        effective_visibility: None,
        binaries: root_metadata.binaries().cloned(),
        entrypoints: root_metadata
            .entrypoints()
            .map(|entries| entries.names().cloned().collect())
            .unwrap_or_default(),
        env: closure_env_vars(root_metadata),
        integrations: closure_integrations(root_metadata),
        dependencies: root_edges,
        is_root: true,
    });

    nodes
}

/// A node's own declared env vars, unfiltered, so both surface projections gate per-axis later.
fn closure_env_vars(metadata: &metadata::ValidMetadata) -> Vec<ClosureEnvVar> {
    metadata
        .env()
        .into_iter()
        .flatten()
        .map(|var| ClosureEnvVar {
            key: var.key.clone(),
            kind: metadata::env::modifier::ModifierKind::try_from(&var.modifier)
                .expect("ValidMetadata rejects unknown modifier types before any closure walk"),
            separator: match &var.modifier {
                metadata::env::modifier::Modifier::List(list) => list.separator.clone(),
                _ => None,
            },
            visibility: var.visibility,
        })
        .collect()
}

/// A node's own declared integration namespace keys, unfiltered; payloads need an install path it lacks.
fn closure_integrations(metadata: &metadata::ValidMetadata) -> Vec<String> {
    metadata
        .integrations()
        .iter()
        .map(|(namespace, _)| namespace.to_owned())
        .collect()
}

/// The resolved identity [`gather_closure_nodes`] gathered `edge` under.
fn resolved_edge_identity(
    resolved_identity: &HashMap<ocx_oci::PinnedPackageRef, ocx_oci::PinnedPackageRef>,
    edge: &ClosureEdge,
) -> ocx_oci::PinnedPackageRef {
    resolved_identity
        .get(&edge.identifier.strip_advisory())
        .cloned()
        .expect("gather_closure_nodes populates the alias map for every edge reachable from root")
}

/// Post-order DFS memoizing each node's [`ResolvedPackage`] under its resolved identity.
fn visit_closure_node(
    key: &ocx_oci::PinnedPackageRef,
    by_identity: &HashMap<ocx_oci::PinnedPackageRef, GatheredClosureNode>,
    resolved_identity: &HashMap<ocx_oci::PinnedPackageRef, ocx_oci::PinnedPackageRef>,
    resolved: &mut HashMap<ocx_oci::PinnedPackageRef, ResolvedPackage>,
    order: &mut Vec<ocx_oci::PinnedPackageRef>,
) {
    if resolved.contains_key(key) {
        return;
    }
    let (_, _, _, edges) = by_identity
        .get(key)
        .expect("gather_closure_nodes populates by_identity for every edge reachable from root");
    let children: Vec<(
        ocx_oci::PinnedPackageRef,
        ResolvedPackage,
        metadata::visibility::Visibility,
    )> = edges
        .iter()
        .map(|edge| {
            let child_key = resolved_edge_identity(resolved_identity, edge);
            visit_closure_node(&child_key, by_identity, resolved_identity, resolved, order);
            let child_resolved = resolved.get(&child_key).cloned().unwrap_or_default();
            (child_key, child_resolved, edge.visibility)
        })
        .collect();
    resolved.insert(key.clone(), ResolvedPackage::new().with_dependencies(children));
    order.push(key.clone());
}

#[cfg(test)]
mod tests {
    use ocx_store::file_structure::{FileStructure, PackageStore};

    use ocx_package::metadata;
    use ocx_package::resolved_package::ResolvedPackage;
    use ocx_util::prelude::SerdeExt as _;

    /// Regression: `drain_package_tasks` must return batch errors in **input**
    /// order, not `JoinSet` completion order. The exit-code classifier picks
    /// `errors.first()`, so a completion-order leak makes `find_all` /
    /// `resolve_all` exit codes race-dependent. Feed a later-input task that
    /// completes first with a distinct error kind and assert the returned
    /// `Vec<PackageError>` is index-ordered. Async analogue of the
    /// `install.rs` `install_failures_are_sorted_by_index_for_deterministic_exit_code`
    /// unit test.
    #[tokio::test(flavor = "multi_thread")]
    async fn drain_package_tasks_sorts_errors_by_input_index() {
        use crate::error::{Error, PackageErrorKind};
        use std::time::Duration;
        use tokio::task::JoinSet;

        let pkg0 = ocx_oci::PackageRef::new_registry("alpha", "example.com");
        let pkg1 = ocx_oci::PackageRef::new_registry("bravo", "example.com");
        let packages = vec![pkg0.clone(), pkg1.clone()];

        let mut tasks: JoinSet<(ocx_oci::PackageRef, Result<(), PackageErrorKind>)> = JoinSet::new();
        // Later-input task (index 1) completes first with a distinct kind.
        let pkg1_task = pkg1.clone();
        tasks.spawn(async move { (pkg1_task, Err(PackageErrorKind::SymlinkRequiresTag)) });
        // Earlier-input task (index 0) completes last (delayed).
        let pkg0_task = pkg0.clone();
        tasks.spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            (pkg0_task, Err(PackageErrorKind::NotFound))
        });

        let err = super::drain_package_tasks(&packages, tasks, Error::FindFailed)
            .await
            .unwrap_err();

        match err {
            Error::FindFailed(errors) => {
                assert_eq!(errors.len(), 2, "both failures collected");
                assert_eq!(
                    errors[0].identifier, pkg0,
                    "input index 0 must sort first regardless of completion order"
                );
                assert!(matches!(errors[0].kind, PackageErrorKind::NotFound));
                assert_eq!(errors[1].identifier, pkg1);
                assert!(matches!(errors[1].kind, PackageErrorKind::SymlinkRequiresTag));
            }
            other => panic!("expected FindFailed, got {other:?}"),
        }
    }

    /// Writes `resolve.json` plus a `metadata.json` under a fake content path
    /// and returns what `load_object_data` makes of it.
    async fn load_object_data_for(
        tempdir: &std::path::Path,
        digest_byte: &str,
        metadata_json: &str,
    ) -> Result<(metadata::Metadata, ResolvedPackage), crate::Error> {
        let store_root = tempdir.join("packages");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = PackageStore::new(&store_root);

        let id = ocx_oci::PackageRef::new_registry("foo/bar", "example.com")
            .clone_with_digest(ocx_oci::Digest::Sha256(digest_byte.repeat(32)));
        let pinned = ocx_oci::PinnedPackageRef::try_from(id).unwrap();

        let pkg_dir = store.path(&pinned);
        let content_dir = pkg_dir.join("content");
        std::fs::create_dir_all(&content_dir).unwrap();

        std::fs::write(pkg_dir.join("metadata.json"), metadata_json).unwrap();
        ResolvedPackage::new()
            .write_json(pkg_dir.join("resolve.json"))
            .await
            .unwrap();

        super::load_object_data(&store, &content_dir).await
    }

    /// Consumption reads a document it cannot resolve, and refuses one it cannot
    /// read (D14).
    ///
    /// Inverted: an env var naming an undeclared dep used to be rejected here.
    /// It now loads, because refusing on a *read* path means an ocx meeting
    /// metadata a newer ocx wrote cannot even list the package — the refusal
    /// belongs to the operation that asks for the value. What still fails
    /// closed is a document whose grammar this binary cannot read at all.
    #[tokio::test]
    async fn load_object_data_reads_unresolvable_metadata_but_refuses_unreadable_metadata() {
        let tempdir = tempfile::tempdir().unwrap();

        let unresolvable = r#"{"type":"bundle","version":1,"dependencies":[],"env":[{"key":"FOO","type":"constant","value":"${deps.missing.installPath}/x"}]}"#;
        assert!(
            load_object_data_for(tempdir.path(), "ab", unresolvable).await.is_ok(),
            "an unresolvable ${{deps.*}} reference must still load — resolution is where it fails"
        );

        let unreadable = r#"{"type":"bundle","version":1,"env":[{"key":"FOO","type":"frobnicate","value":"x"}]}"#;
        let err = load_object_data_for(tempdir.path(), "cd", unreadable)
            .await
            .expect_err("a modifier type this binary cannot interpret must fail closed");
        // Render the way `main.rs` does: wrapped in `anyhow`, whose `{:#}` walks
        // the `source()` chain. A bare `crate::Error` would print only its top
        // message, which no longer restates its own source.
        let chain = format!("{:#}", anyhow::Error::from(err));
        assert!(
            chain.contains("frobnicate"),
            "error chain must name the unreadable modifier type: {chain}"
        );
    }

    /// Builds a valid, installed `InstallInfo` under `fs` for `foo/bar:1.0` and
    /// returns it paired with the tagged identifier whose `candidates/{tag}`
    /// slot `wire_selection` targets.
    async fn install_info_fixture(fs: &FileStructure) -> (ocx_oci::PackageRef, ocx_package::install_info::InstallInfo) {
        let digest_hex: String = "cd".repeat(32);
        let tagged = ocx_oci::PackageRef::new_registry("foo/bar", "example.com")
            .clone_with_tag("1.0")
            .clone_with_digest(ocx_oci::Digest::Sha256(digest_hex));
        let pinned = ocx_oci::PinnedPackageRef::try_from(tagged.clone()).unwrap();

        let pkg_dir = fs.packages.path(&pinned);
        let content_dir = pkg_dir.join("content");
        std::fs::create_dir_all(&content_dir).unwrap();
        std::fs::write(pkg_dir.join("metadata.json"), r#"{"type":"bundle","version":1}"#).unwrap();
        ResolvedPackage::new()
            .write_json(pkg_dir.join("resolve.json"))
            .await
            .unwrap();

        let (metadata, resolved) = super::load_object_data(&fs.packages, &content_dir)
            .await
            .expect("fixture metadata is valid");
        let dir = ocx_store::file_structure::PackageDir::with_root(pkg_dir);
        let info = ocx_package::install_info::InstallInfo::new(pinned, metadata, resolved, dir);
        (tagged, info)
    }

    /// A supported platform the current host cannot run, or `None` when the host
    /// platform is undeterminable (unsupported CI arch), in which case the gate
    /// writes unconditionally and suppression cannot be exercised.
    fn a_foreign_platform() -> Option<ocx_oci::Platform> {
        ["windows/amd64", "linux/amd64", "darwin/arm64", "linux/arm64"]
            .into_iter()
            .map(|spec| spec.parse::<ocx_oci::Platform>().expect("valid platform string"))
            .find(|platform| !ocx_oci::Platform::host_can_run(Some(platform)))
    }

    /// Regression (issue #179, defect 2): a foreign-platform install must NOT
    /// write `candidates/{tag}`, and must leave a pre-existing host candidate
    /// untouched — the actual clobber scenario from the bug report. The pure
    /// gate contract is covered host-independently by `Platform::host_can_run_on`
    /// in `oci/platform.rs`; this test proves `wire_selection` acts on it.
    #[tokio::test]
    async fn wire_selection_suppresses_foreign_platform_candidate() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let (tagged, info) = install_info_fixture(&fs).await;

        let Some(foreign) = a_foreign_platform() else {
            return; // host undeterminable: gate writes all, nothing to suppress
        };
        let foreign_info = info.clone().with_platform(foreign);
        let candidate_path = fs.symlinks.candidate(&tagged);

        // Fresh foreign install → no candidate written.
        let outcome = super::wire_selection(&fs, &tagged, &foreign_info, true, false)
            .await
            .expect("wire_selection succeeds");
        assert!(
            outcome.candidate.is_none(),
            "foreign platform must not report a candidate"
        );
        assert!(
            !ocx_util::fs::symlink::is_link(&candidate_path),
            "foreign platform must not create candidates/{{tag}}"
        );

        // Pre-existing host candidate must survive a subsequent foreign install.
        let host_info = info; // no platform stamp → host-runnable
        super::wire_selection(&fs, &tagged, &host_info, true, false)
            .await
            .expect("host wire_selection succeeds");
        let host_target = std::fs::read_link(&candidate_path).expect("host candidate exists");

        super::wire_selection(&fs, &tagged, &foreign_info, true, false)
            .await
            .expect("foreign wire_selection succeeds");
        assert_eq!(
            std::fs::read_link(&candidate_path).unwrap(),
            host_target,
            "foreign install must not clobber the host candidate slot"
        );
    }

    /// A **deferred** `InstallInfo` is refused by the install-symlink
    /// writer, and refused before anything is written.
    ///
    /// `candidates/{tag}` and `current` are the namespace users address by
    /// name; a deferred root's package directory does not exist yet, so a link
    /// into it is a dangling install. No live path produces one today — the
    /// guard is what keeps that true when a future caller reaches for
    /// `wire_selection` with whatever `compose_roots` handed it.
    #[tokio::test]
    async fn wire_selection_refuses_a_deferred_install_info() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let (tagged, info) = install_info_fixture(&fs).await;

        // Control: the same fixture wires cleanly when it is not deferred, so
        // the refusal below cannot be attributed to the fixture.
        super::wire_selection(&fs, &tagged, &info, true, true)
            .await
            .expect("a materialized package wires its symlinks");
        let candidate_path = fs.symlinks.candidate(&tagged);
        let materialized_target = std::fs::read_link(&candidate_path).expect("the control wrote a candidate");

        let deferred = info.with_deferred(ocx_package::install_info::DeferredComposition::new(
            ocx_store::file_structure::ShimDir {
                dir: tempdir.path().join("shims").join("tool"),
            },
            Vec::new(),
        ));

        let error = super::wire_selection(&fs, &tagged, &deferred, true, true)
            .await
            .expect_err("a deferred package has no directory to point a symlink at");
        assert!(
            matches!(error, super::PackageErrorKind::Internal(_)),
            "expected an internal refusal, got {error:?}"
        );
        assert_eq!(
            std::fs::read_link(&candidate_path).unwrap(),
            materialized_target,
            "the refusal must leave the existing candidate untouched"
        );
    }

    /// `acquire_select_lock` materializes the per-repo lock file and returns
    /// a guard. Serializes Cluster 3's transactional select state.
    #[tokio::test]
    async fn acquire_select_lock_creates_lock_file_at_expected_path() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let id = ocx_oci::PackageRef::new_registry("cmake", "example.com");

        let _guard = super::acquire_select_lock(&fs, &id).await.expect("acquire lock");

        let lock_path = fs.symlinks.select_lock(&id);
        assert!(
            lock_path.exists(),
            "lock file must be created at {}",
            lock_path.display()
        );
        assert_eq!(
            lock_path.file_name().unwrap().to_str().unwrap(),
            ".select.lock",
            "lock file must use the documented name"
        );
    }

    /// A second `acquire_select_lock` for the same package must block until
    /// the first guard is dropped — proves `current` symlink updates
    /// serialize across concurrent installer/deselect callers.
    #[tokio::test]
    async fn acquire_select_lock_serializes_concurrent_callers() {
        use futures::FutureExt;

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let id = ocx_oci::PackageRef::new_registry("cmake", "example.com");

        let first = super::acquire_select_lock(&fs, &id).await.expect("first acquire");

        // Second acquire must not be ready while `first` is held.
        let second_fut = super::acquire_select_lock(&fs, &id);
        tokio::pin!(second_fut);
        assert!(
            second_fut.as_mut().now_or_never().is_none(),
            "second acquire must block while the first guard is held"
        );

        drop(first);
        // After releasing, the second acquire becomes ready.
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), second_fut)
            .await
            .expect("second acquire timed out after release")
            .expect("second acquire failed");
        drop(second);
    }

    /// Fake `IndexImpl` source recording which digests are requested through
    /// each endpoint method — `fetch_manifest_raw_bytes` (manifests endpoint)
    /// vs `fetch_blob` (blobs endpoint) — so a test can prove a manifest
    /// digest never crosses the blobs-endpoint stream and vice versa
    /// (`adr_index_indirection.md` B2). `published` mirrors `OcxIndex`'s
    /// `physical_reference` override, letting a test simulate a published
    /// (`index.ocx.sh`) source's dispatch entries without a real one.
    #[derive(Clone, Default)]
    struct EndpointSpySource {
        namespace: String,
        published: bool,
        manifests: Vec<(ocx_oci::Digest, Vec<u8>, ocx_oci::Manifest)>,
        blob: Option<(ocx_oci::Digest, Vec<u8>)>,
        raw_bytes_calls: std::sync::Arc<std::sync::Mutex<Vec<ocx_oci::Digest>>>,
        blob_calls: std::sync::Arc<std::sync::Mutex<Vec<ocx_oci::Digest>>>,
    }

    #[async_trait::async_trait]
    impl ocx_index::IndexImpl for EndpointSpySource {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &ocx_oci::PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(
            &self,
            _: &ocx_oci::PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<ocx_oci::Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            let digest = blob_ref.digest();
            self.blob_calls.lock().unwrap().push(digest.clone());
            Ok(self
                .blob
                .as_ref()
                .filter(|(d, _)| *d == digest)
                .map(|(_, bytes)| bytes.clone()))
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &ocx_oci::PackageRef,
        ) -> ocx_index::error::Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
            let Some(digest) = identifier.digest() else {
                return Ok(None);
            };
            self.raw_bytes_calls.lock().unwrap().push(digest.clone());
            Ok(self
                .manifests
                .iter()
                .find(|(d, _, _)| *d == digest)
                .map(|(d, bytes, manifest)| (bytes.clone(), d.clone(), manifest.clone())))
        }
        async fn physical_reference(
            &self,
            identifier: &ocx_oci::PackageRef,
        ) -> ocx_index::error::Result<Option<ocx_oci::OciIdentifier>> {
            if self.published && identifier.registry() == self.namespace {
                Ok(Some(ocx_oci::OciIdentifier::passthrough(identifier)))
            } else {
                Ok(None)
            }
        }
        fn box_clone(&self) -> Box<dyn ocx_index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// B2 (`adr_index_indirection.md`): a flat (single-platform) resolve's
    /// chain has a `ChainRole::Manifest` entry that is a leaf platform
    /// manifest — content the local dispatch cache never holds (A3), so it
    /// must be staged via the manifests endpoint (`fetch_manifest_raw_bytes`),
    /// never `fetch_blob`/the blobs endpoint (which 404s a manifest digest on
    /// a real registry). The config entry keeps using the blobs endpoint.
    #[tokio::test(flavor = "multi_thread")]
    async fn stage_and_link_chain_blobs_stages_leaf_manifest_via_manifests_endpoint() {
        use crate::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};
        use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
        use ocx_store::file_structure::FileStructure;

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "example.com";
        let repository = "cmake";

        let manifest_bytes = br#"{"manifest":true}"#.to_vec();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(&manifest_bytes);
        let config_bytes = br#"{"config":true}"#.to_vec();
        let config_digest = ocx_oci::Algorithm::Sha256.hash(&config_bytes);

        let source = EndpointSpySource {
            namespace: registry.to_string(),
            published: false,
            manifests: vec![(
                manifest_digest.clone(),
                manifest_bytes.clone(),
                ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
            )],
            blob: Some((config_digest.clone(), config_bytes.clone())),
            ..Default::default()
        };
        let raw_bytes_calls = source.raw_bytes_calls.clone();
        let blob_calls = source.blob_calls.clone();

        let snapshot = IndexStore::new(tempdir.path().join("index"));
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig { index_store: snapshot }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
        );

        let pin = |digest: &ocx_oci::Digest| {
            ocx_oci::PinnedPackageRef::try_from(
                ocx_oci::PackageRef::new_registry(repository, registry).clone_with_digest(digest.clone()),
            )
            .unwrap()
        };
        let chain_blob = |digest: &ocx_oci::Digest, role| ChainBlob {
            identifier: pin(digest),
            role,
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            size: 0,
        };
        let manifest_pin = pin(&manifest_digest);
        let resolved = ResolvedChain {
            pinned: manifest_pin.clone(),
            transport_pinned: Ok(
                ocx_oci::OciIdentifier::passthrough(manifest_pin.as_identifier()).at_pin_of(&manifest_pin)
            ),
            chain: vec![
                chain_blob(&manifest_digest, ChainRole::Manifest),
                chain_blob(&config_digest, ChainRole::Config),
            ],
            final_manifest: ocx_oci::ImageManifest::default(),
            platform: ocx_oci::Platform::any(),
        };

        let content_path = tempdir.path().join("pkg-content");
        std::fs::create_dir_all(&content_path).unwrap();

        super::stage_and_link_chain_blobs(&fs, &index, &content_path, &resolved)
            .await
            .expect("staging the resolved chain into the blob store must succeed");

        assert_eq!(
            fs.blobs.read_blob(registry, &manifest_digest).await.unwrap().as_deref(),
            Some(manifest_bytes.as_slice()),
            "leaf manifest must be materialized into the blob store"
        );
        assert_eq!(
            fs.blobs.read_blob(registry, &config_digest).await.unwrap().as_deref(),
            Some(config_bytes.as_slice()),
            "config blob must be materialized into the blob store"
        );

        assert_eq!(
            raw_bytes_calls.lock().unwrap().as_slice(),
            std::slice::from_ref(&manifest_digest),
            "the leaf manifest must be fetched via the manifests endpoint exactly once"
        );
        assert!(
            !blob_calls.lock().unwrap().contains(&manifest_digest),
            "the manifest digest must never be requested through the blobs endpoint (it 404s on a real registry)"
        );
        assert_eq!(
            blob_calls.lock().unwrap().as_slice(),
            [config_digest],
            "the config blob must still be fetched via the blobs endpoint"
        );

        let refs_blobs = fs.packages.refs_blobs_dir_for_content(&content_path).unwrap();
        assert_eq!(
            std::fs::read_dir(&refs_blobs).unwrap().count(),
            2,
            "both chain blobs must be forward-ref linked into refs/blobs/"
        );
    }

    /// Everything the two published-dispatch staging tests below need to look
    /// at after [`stage_and_link_chain_blobs`] has run once.
    struct StagedPublishedChain {
        /// Held only for its `Drop` — the whole fixture lives under it.
        _tempdir: tempfile::TempDir,
        file_structure: ocx_store::file_structure::FileStructure,
        registry: &'static str,
        dispatch_digest: ocx_oci::Digest,
        dispatch_bytes: Vec<u8>,
        manifest_digest: ocx_oci::Digest,
        manifest_bytes: Vec<u8>,
        config_digest: ocx_oci::Digest,
        config_bytes: Vec<u8>,
        raw_bytes_calls: std::sync::Arc<std::sync::Mutex<Vec<ocx_oci::Digest>>>,
        blob_calls: std::sync::Arc<std::sync::Mutex<Vec<ocx_oci::Digest>>>,
    }

    /// Resolves and stages a three-entry chain — `Index` / `Manifest` /
    /// `Config` — from a **published** (`index.ocx.sh`) source, i.e. one whose
    /// `physical_reference` resolves. The `Index` entry's bytes are a real OCI
    /// image index advertising the leaf manifest as its single child, and its
    /// digest is the hash of exactly those bytes, so the staging path's
    /// recompute-and-verify (`verify_requested_digest`) is exercised rather
    /// than bypassed.
    async fn stage_published_index_chain() -> StagedPublishedChain {
        use crate::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};
        use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
        use ocx_store::file_structure::FileStructure;

        let tempdir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "ocx.sh";
        let repository = "ns/cmake";

        let manifest_bytes = br#"{"manifest":true}"#.to_vec();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(&manifest_bytes);
        let config_bytes = br#"{"config":true}"#.to_vec();
        let config_digest = ocx_oci::Algorithm::Sha256.hash(&config_bytes);

        let dispatch = ocx_oci::Manifest::ImageIndex(ocx_oci::ImageIndex {
            schema_version: 2,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            artifact_type: None,
            manifests: vec![ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: manifest_digest.to_string(),
                size: manifest_bytes.len() as i64,
                platform: None,
                artifact_type: None,
                annotations: None,
            }],
            annotations: None,
        });
        let dispatch_bytes = serde_json::to_vec(&dispatch).unwrap();
        let dispatch_digest = ocx_oci::Algorithm::Sha256.hash(&dispatch_bytes);

        let source = EndpointSpySource {
            namespace: registry.to_string(),
            published: true,
            manifests: vec![
                (dispatch_digest.clone(), dispatch_bytes.clone(), dispatch),
                (
                    manifest_digest.clone(),
                    manifest_bytes.clone(),
                    ocx_oci::Manifest::Image(ocx_oci::ImageManifest::default()),
                ),
            ],
            blob: Some((config_digest.clone(), config_bytes.clone())),
            ..Default::default()
        };
        let raw_bytes_calls = source.raw_bytes_calls.clone();
        let blob_calls = source.blob_calls.clone();

        let snapshot = IndexStore::new(tempdir.path().join("index"));
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig { index_store: snapshot }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
        );

        let pin = |digest: &ocx_oci::Digest| {
            ocx_oci::PinnedPackageRef::try_from(
                ocx_oci::PackageRef::new_registry(repository, registry).clone_with_digest(digest.clone()),
            )
            .unwrap()
        };
        let chain_blob = |digest: &ocx_oci::Digest, role| ChainBlob {
            identifier: pin(digest),
            role,
            media_type: match role {
                // D1: the `Index`-role entry is an OCI image index, whatever the source.
                ChainRole::Index => "application/vnd.oci.image.index.v1+json",
                ChainRole::Manifest => "application/vnd.oci.image.manifest.v1+json",
                ChainRole::Config => ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1,
            }
            .to_string(),
            size: 0,
        };
        let manifest_pin = pin(&manifest_digest);
        let resolved = ResolvedChain {
            pinned: manifest_pin.clone(),
            transport_pinned: Ok(
                ocx_oci::OciIdentifier::passthrough(manifest_pin.as_identifier()).at_pin_of(&manifest_pin)
            ),
            chain: vec![
                chain_blob(&dispatch_digest, ChainRole::Index),
                chain_blob(&manifest_digest, ChainRole::Manifest),
                chain_blob(&config_digest, ChainRole::Config),
            ],
            final_manifest: ocx_oci::ImageManifest::default(),
            platform: ocx_oci::Platform::any(),
        };

        let content_path = tempdir.path().join("pkg-content");
        std::fs::create_dir_all(&content_path).unwrap();

        super::stage_and_link_chain_blobs(&file_structure, &index, &content_path, &resolved)
            .await
            .expect("staging the resolved chain into the blob store must succeed");

        StagedPublishedChain {
            _tempdir: tempdir,
            file_structure,
            registry,
            dispatch_digest,
            dispatch_bytes,
            manifest_digest,
            manifest_bytes,
            config_digest,
            config_bytes,
            raw_bytes_calls,
            blob_calls,
        }
    }

    /// D1 (`adr_oci_index_only_dispatch.md`): a `ChainRole::Index` entry names
    /// an OCI image index the registry serves, and that holds for a published
    /// (`index.ocx.sh`) source exactly as it does for a plain-registry one.
    /// It is therefore staged into the blob store like any other chain entry —
    /// fetched once through the manifests endpoint, never the blobs endpoint.
    /// Staging it is what makes the published absent-dispatch offline recovery
    /// work at all: nothing else puts those bytes in `$OCX_HOME/blobs`.
    #[tokio::test(flavor = "multi_thread")]
    async fn published_index_role_chain_entry_is_staged_into_the_blob_store() {
        let staged = stage_published_index_chain().await;
        let blobs = &staged.file_structure.blobs;

        assert_eq!(
            blobs
                .read_blob(staged.registry, &staged.dispatch_digest)
                .await
                .unwrap()
                .as_deref(),
            Some(staged.dispatch_bytes.as_slice()),
            "a published source's dispatch object must be staged into the blob store"
        );
        assert_eq!(
            blobs
                .read_blob(staged.registry, &staged.manifest_digest)
                .await
                .unwrap()
                .as_deref(),
            Some(staged.manifest_bytes.as_slice()),
            "the selected leaf manifest must still be materialized"
        );
        assert_eq!(
            blobs
                .read_blob(staged.registry, &staged.config_digest)
                .await
                .unwrap()
                .as_deref(),
            Some(staged.config_bytes.as_slice()),
            "the config blob must still be materialized"
        );

        assert_eq!(
            staged.raw_bytes_calls.lock().unwrap().as_slice(),
            [staged.dispatch_digest.clone(), staged.manifest_digest.clone()],
            "dispatch object and leaf manifest each cross the manifests endpoint exactly once"
        );
        assert!(
            !staged.blob_calls.lock().unwrap().contains(&staged.dispatch_digest),
            "the dispatch digest must never be requested through the blobs endpoint (it 404s on a real registry)"
        );
    }

    /// The staged dispatch object must be readable by GC's index-retention
    /// scan on its own terms: `add_index_retention_edges`
    /// (`tasks/garbage_collection/reachability_graph.rs`) enumerates the blob
    /// store, parses each candidate as an `ocx_oci::Manifest`, and resolves every
    /// advertised child under the registry root **three levels above the
    /// index's own shard directory**. This asserts that whole read path lands
    /// on the leaf manifest's real blob directory — the retention edge's
    /// content. Before the dispatch entry was staged there was no blob to
    /// enumerate, so an index-resolved package's index had no retention edge
    /// at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn staged_published_dispatch_object_yields_a_child_leaf_retention_edge() {
        use ocx_store::file_structure::cas_shard_path;

        let staged = stage_published_index_chain().await;
        let blobs = &staged.file_structure.blobs;

        let dispatch_dir = blobs.path(staged.registry, &staged.dispatch_digest);
        let listed = blobs.list_all().await.unwrap();
        let entry = listed
            .iter()
            .find(|blob| blob.dir == dispatch_dir)
            .expect("the staged dispatch object must be enumerated by the blob-store walk");

        // Same two reads `index_retention_pairs` performs.
        let bytes = tokio::fs::read(entry.data()).await.unwrap();
        let ocx_oci::Manifest::ImageIndex(index) = serde_json::from_slice::<ocx_oci::Manifest>(&bytes).unwrap() else {
            panic!("the staged dispatch object must parse as an OCI image index");
        };

        // Same registry-root arithmetic `add_index_retention_edges` performs:
        // {blobs_root}/{registry_slug} is three levels above the shard dir.
        let registry_root = entry
            .dir
            .parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.parent())
            .expect("a shard dir always has a registry root three levels up");

        let child_digest = ocx_oci::Digest::try_from(index.manifests[0].digest.as_str()).unwrap();
        assert_eq!(
            registry_root.join(cas_shard_path(&child_digest)),
            blobs.path(staged.registry, &staged.manifest_digest),
            "the advertised child must resolve to the leaf manifest's own blob directory"
        );
    }

    /// CWE-345 regression for the `ChainRole::Index` role: the bytes are
    /// publisher-controlled and arrive over the network, so `stage_chain_blobs`
    /// must recompute the digest from the bytes it actually fetched
    /// (`verify_requested_digest`) and refuse to store them under the requested
    /// digest when they disagree. Trusting the descriptor instead would let a
    /// compromised or buggy source poison the content-addressed store at the
    /// requested digest's path — and every later content-addressed read of it
    /// (`blob_needs_fetch`'s heal check, `add_index_retention_edges`' parse)
    /// trusts whatever is found there.
    #[tokio::test(flavor = "multi_thread")]
    async fn stage_chain_blobs_rejects_index_bytes_that_do_not_hash_to_the_requested_digest() {
        use crate::error::PackageErrorKind;
        use crate::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};
        use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
        use ocx_store::file_structure::FileStructure;

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "ocx.sh";
        let repository = "ns/cmake";

        // The chain entry requests the digest of these real image-index bytes …
        let dispatch = ocx_oci::Manifest::ImageIndex(ocx_oci::ImageIndex {
            schema_version: 2,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            artifact_type: None,
            manifests: Vec::new(),
            annotations: None,
        });
        let dispatch_bytes = serde_json::to_vec(&dispatch).unwrap();
        let dispatch_digest = ocx_oci::Algorithm::Sha256.hash(&dispatch_bytes);
        // … but the source serves different bytes under exactly that digest.
        let served_bytes = b"not the index bytes".to_vec();
        assert_ne!(ocx_oci::Algorithm::Sha256.hash(&served_bytes), dispatch_digest);

        let source = EndpointSpySource {
            namespace: registry.to_string(),
            published: true,
            manifests: vec![(dispatch_digest.clone(), served_bytes, dispatch)],
            ..Default::default()
        };

        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(tempdir.path().join("index")),
            }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
        );

        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repository, registry).clone_with_digest(dispatch_digest.clone()),
        )
        .unwrap();
        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: Ok(ocx_oci::OciIdentifier::passthrough(pinned.as_identifier()).at_pin_of(&pinned)),
            chain: vec![ChainBlob {
                identifier: pinned,
                role: ChainRole::Index,
                media_type: "application/vnd.oci.image.index.v1+json".to_string(),
                size: 0,
            }],
            final_manifest: ocx_oci::ImageManifest::default(),
            platform: ocx_oci::Platform::any(),
        };

        let err = super::stage_chain_blobs(&fs, &index, &resolved)
            .await
            .expect_err("index bytes that don't hash to the requested digest must be rejected");

        assert!(
            matches!(
                err,
                PackageErrorKind::Internal(crate::Error::FileStructure(
                    ocx_store::file_structure::error::Error::DigestMismatch { .. }
                ))
            ),
            "must surface the digest-mismatch error (CWE-345), not silently accept the bytes: {err:?}"
        );
        assert_eq!(
            fs.blobs.read_blob(registry, &dispatch_digest).await.unwrap(),
            None,
            "the mismatched bytes must never be written into the blob store at the requested digest's path"
        );
    }

    /// Regression (`ocx package test` local flow, rc=69): a chain blob already
    /// staged in the blob store — as the local package-test flow does with its
    /// synthesized manifest — must resolve **guaranteed-local**, never routed
    /// through the index/registry. Nothing is seeded index-side (see the inline
    /// note below): the blob-store existence guard must short-circuit before
    /// any index consultation. Without the guard, `stage_and_link_chain_blobs`
    /// fetches through the index and fails (offline miss here; in production,
    /// a registry 404 for the never-pushed blob).
    #[tokio::test(flavor = "multi_thread")]
    async fn stage_and_link_chain_blobs_never_indexes_a_blob_already_in_the_store() {
        use crate::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};
        use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
        use ocx_store::file_structure::FileStructure;

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let snapshot = IndexStore::new(tempdir.path().join("index"));
        let registry = "example.com";
        let repository = "cmake";

        let manifest_bytes = br#"{"local":"manifest"}"#.to_vec();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(&manifest_bytes);

        // The package-test flow stages its synthesized manifest straight into the
        // blob store — never the snapshot.
        fs.blobs
            .write_blob(registry, &manifest_digest, &manifest_bytes)
            .await
            .unwrap();

        // No corresponding snapshot/index object is seeded at all: the
        // `fs.blobs.data()` guaranteed-local guard in `stage_and_link_chain_blobs`
        // fires before the `ChainRole::Manifest` arm ever reaches the index, so
        // a tampered snapshot object at this digest is unreachable from this
        // test — it was proven dead even before the index-home flat blob CAS
        // was retired (`stage_and_link_chain_blobs` never routes a
        // `ChainRole::Manifest` entry through `Index::fetch_blob` in the first
        // place; that role always uses `fetch_manifest_raw_bytes`).
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig { index_store: snapshot }),
            vec![],
            ChainMode::Offline,
        );

        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repository, registry).clone_with_digest(manifest_digest.clone()),
        )
        .unwrap();
        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: Ok(ocx_oci::OciIdentifier::passthrough(pinned.as_identifier()).at_pin_of(&pinned)),
            chain: vec![ChainBlob {
                identifier: pinned.clone(),
                role: ChainRole::Manifest,
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                size: i64::try_from(manifest_bytes.len()).unwrap(),
            }],
            final_manifest: ocx_oci::ImageManifest::default(),
            platform: ocx_oci::Platform::any(),
        };

        let content_path = tempdir.path().join("pkg-content");
        std::fs::create_dir_all(&content_path).unwrap();

        super::stage_and_link_chain_blobs(&fs, &index, &content_path, &resolved)
            .await
            .expect("a blob already in the blob store must resolve without touching the index");

        // The forward-ref still lands so GC can reach the locally-staged blob.
        let refs_blobs = fs.packages.refs_blobs_dir_for_content(&content_path).unwrap();
        assert_eq!(std::fs::read_dir(&refs_blobs).unwrap().count(), 1);
    }

    /// AC10 regression: an offline install whose config blob was never cached
    /// (e.g. after a bare `ocx index update`, which now persists the manifest
    /// chain into the index snapshot but not the config blob) must fail with a
    /// clean error **naming the missing digest** — not a bare, digest-less
    /// `OfflineMode`. `OfflineManifestMissing` classifies to `PolicyBlocked`
    /// (81) and its message carries the `sha256:` digest + "cache".
    #[tokio::test(flavor = "multi_thread")]
    async fn load_config_metadata_offline_missing_config_names_the_digest() {
        use crate::error::PackageErrorKind;
        use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};

        let tempdir = tempfile::tempdir().unwrap();
        // Offline index over an empty snapshot → `fetch_blob` always yields None.
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(tempdir.path().join("index")),
            }),
            vec![],
            ChainMode::Offline,
        );

        let config_digest = ocx_oci::Algorithm::Sha256.hash(b"config-bytes");
        let manifest_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{}","digest":"{}","size":12}},"layers":[]}}"#,
            ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1,
            config_digest,
        );
        let ocx_oci::Manifest::Image(image_manifest) = serde_json::from_str(&manifest_json).unwrap() else {
            panic!("fixture must parse as an image manifest");
        };

        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("cmake", "example.com")
                .clone_with_tag("3.28")
                .clone_with_digest(ocx_oci::Algorithm::Sha256.hash(b"manifest-bytes")),
        )
        .unwrap();

        let err = super::load_config_metadata(&index, &pinned, &image_manifest)
            .await
            .expect_err("offline install with a missing config blob must fail");

        match err {
            PackageErrorKind::OfflineManifestMissing(missing) => {
                assert_eq!(
                    missing.digest, config_digest,
                    "error must name the missing config digest"
                );
                let text = PackageErrorKind::OfflineManifestMissing(missing).to_string();
                assert!(text.contains("sha256:"), "message must carry the digest: {text}");
                assert!(text.contains("cache"), "message must mention the local cache: {text}");
            }
            other => panic!("expected OfflineManifestMissing naming the digest, got {other:?}"),
        }
    }

    /// Distinct packages must not contend on the same lock — each repo gets
    /// its own `.select.lock` file under `{base}/`.
    #[tokio::test]
    async fn acquire_select_lock_is_per_repo() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let id_a = ocx_oci::PackageRef::new_registry("cmake", "example.com");
        let id_b = ocx_oci::PackageRef::new_registry("ninja", "example.com");

        let _guard_a = super::acquire_select_lock(&fs, &id_a).await.expect("acquire a");
        // Distinct repo: must succeed immediately, no contention.
        let _guard_b = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            super::acquire_select_lock(&fs, &id_b),
        )
        .await
        .expect("distinct-repo acquire timed out — locks are not per-repo")
        .expect("distinct-repo acquire failed");
    }
}
