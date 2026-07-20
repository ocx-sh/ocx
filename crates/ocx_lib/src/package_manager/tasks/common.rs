// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared utilities for task modules.
//!
//! Free functions only — no `impl PackageManager`. Since `tasks` is a private
//! module, these helpers are invisible to external consumers.
//!
//! ## Selection-state lock order
//!
//! Selection-state mutations (the per-repo `current` symlink) are guarded by
//! the per-repo `.select.lock`.
//!
//! - **`{symlinks/{registry}/{repo}}/.select.lock`** (per repo) — held for the
//!   actual symlink writes/rollback inside [`wire_selection`].
//!
//! `deselect` and `uninstall --deselect` acquire the same per-repo
//! `.select.lock` before clearing symlinks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use tokio::task::JoinSet;

use crate::{
    MEDIA_TYPE_PACKAGE_METADATA_V1,
    file_structure::{self, PackageStore},
    log, media_type_select, oci,
    package::{install_info::InstallInfo, metadata, resolved_package::ResolvedPackage},
    package_manager::error::{self, PackageError, PackageErrorKind},
    prelude::SerdeExt,
    reference_manager::ReferenceManager,
    utility,
    utility::fs::LockedFile,
};

/// Finds a package in the object store without index resolution.
///
/// The identifier must carry a digest. Returns the installed package info if
/// present, or `None` if the object is absent. Also serves as defense layer 2
/// in the concurrent pull safety model.
///
/// Metadata read from disk is validated through [`metadata::ValidMetadata`]
/// before the [`InstallInfo`] is constructed — defense in depth against stale
/// or tampered on-disk metadata that predates current validation rules.
pub async fn find_in_store(
    objects: &PackageStore,
    identifier: &oci::PinnedIdentifier,
) -> Result<Option<InstallInfo>, PackageErrorKind> {
    let pkg = objects.package_dir(identifier);
    let content = pkg.content();
    let metadata_path = pkg.metadata();
    let resolve_path = pkg.resolve();
    if utility::fs::path_exists_lossy(&content).await
        && utility::fs::path_exists_lossy(&metadata_path).await
        && utility::fs::path_exists_lossy(&resolve_path).await
    {
        let (metadata_result, resolved_result): (crate::Result<metadata::Metadata>, crate::Result<ResolvedPackage>) = tokio::join!(
            metadata::Metadata::read_json(&metadata_path),
            ResolvedPackage::read_json(&resolve_path),
        );
        let metadata = metadata_result.map_err(PackageErrorKind::Internal)?;
        let metadata = metadata::ValidMetadata::try_from(metadata)
            .map_err(PackageErrorKind::Internal)?
            .into();
        let resolved = resolved_result.map_err(PackageErrorKind::Internal)?;
        Ok(Some(InstallInfo::new(identifier.clone(), metadata, resolved, pkg)))
    } else {
        Ok(None)
    }
}

/// Reconstructs the [`PinnedIdentifier`](oci::PinnedIdentifier) for a package
/// loaded through an install symlink (candidate or current).
///
/// Registry and repository come from the caller-supplied [`oci::Identifier`].
/// The digest is read from the shared package directory's `digest` file so the
/// result is the truly installed content digest, not whatever first installer
/// happened to win a cross-repo dedup race.
///
/// Tag handling depends on `kind`:
/// - [`SymlinkKind::Candidate`]: caller's tag is preserved — candidate symlinks
///   are keyed by tag, so the tag and digest always agree by construction.
/// - [`SymlinkKind::Current`]: caller's tag is stripped. `current` points at
///   whatever digest was most recently selected, which may have been installed
///   under a different tag than the caller supplied. Keeping the caller's tag
///   would produce a hybrid identifier (`pkg:old-tag@new-digest`) that never
///   existed as a real install.
pub async fn identifier_for_symlink(
    objects: &PackageStore,
    symlink_path: &Path,
    identifier: &oci::Identifier,
    kind: file_structure::SymlinkKind,
) -> Result<oci::PinnedIdentifier, crate::Error> {
    let digest_path = objects.digest_file_for_content(symlink_path)?;
    let digest = file_structure::read_digest_file(&digest_path).await?;
    let base = match kind {
        file_structure::SymlinkKind::Candidate => identifier.clone(),
        file_structure::SymlinkKind::Current => identifier.without_tag(),
    };
    Ok(oci::PinnedIdentifier::try_from(base.clone_with_digest(digest))?)
}

/// Loads metadata.json and resolve.json for an existing content path.
///
/// Uses `PackageStore::metadata_for_content` / `resolve_for_content` which
/// follow symlinks, making this safe for both direct object paths and install
/// symlinks.
///
/// Metadata is validated via [`metadata::ValidMetadata`] before being returned
/// — every on-disk metadata blob the package_manager loads goes through the
/// same validation as a publish-time blob, so consumers operate on metadata
/// that is structurally and semantically well-formed.
pub async fn load_object_data(
    objects: &PackageStore,
    content_path: &Path,
) -> Result<(metadata::Metadata, ResolvedPackage), crate::Error> {
    let metadata_path = objects.metadata_for_content(content_path)?;
    let resolve_path = objects.resolve_for_content(content_path)?;
    let (metadata_result, resolved_result): (crate::Result<metadata::Metadata>, crate::Result<ResolvedPackage>) = tokio::join!(
        metadata::Metadata::read_json(&metadata_path),
        ResolvedPackage::read_json(&resolve_path),
    );
    let metadata = metadata::ValidMetadata::try_from(metadata_result?)?.into();
    Ok((metadata, resolved_result?))
}

/// Upper bound on a metadata config blob's size (declared descriptor size AND
/// fetched byte length), enforced by [`load_config_metadata`].
///
/// Config metadata is KB-scale in practice — a 4 MiB ceiling is orders of
/// magnitude above any real package and therefore never bites a legitimate
/// publisher. See `adr_inspect_metadata_closure.md` D5.
pub(super) const MAX_METADATA_BLOB_BYTES: usize = 4 * 1024 * 1024;

/// Fetches the OCX metadata config blob referenced by `manifest`, validates
/// its media type, deserializes it, and runs publish-time validation.
///
/// Shared by the pull pipeline (`setup_owned`) and `inspect` so both apply
/// identical media-type + [`ValidMetadata`](metadata::ValidMetadata) gating
/// to the config blob. The blob is fetched through the index
/// ([`Index::fetch_blob`](oci::index::Index::fetch_blob)), the single
/// offline-aware blob accessor: local-CAS first, chain-walk on miss,
/// write-through on hit, `Ok(None)` when offline and absent locally.
pub async fn load_config_metadata(
    index: &oci::index::Index,
    pinned: &oci::PinnedIdentifier,
    manifest: &oci::ImageManifest,
) -> Result<metadata::ValidMetadata, PackageErrorKind> {
    // Config blob media-type check before any fetch — refuse to stage a
    // wrong-media-type blob into the local CAS.
    media_type_select(&manifest.config.media_type, &[MEDIA_TYPE_PACKAGE_METADATA_V1])
        .map_err(|e| PackageErrorKind::from(oci::client::error::ClientError::internal(e)))?;

    // D5 step 1 (pre-fetch): the manifest's declared config size is known
    // before any blob request — reject an over-cap declared size without
    // touching the network or the cache.
    if manifest.config.size < 0 || manifest.config.size as u64 > MAX_METADATA_BLOB_BYTES as u64 {
        return Err(PackageErrorKind::Internal(crate::Error::MetadataBlobTooLarge {
            size: manifest.config.size,
            max: MAX_METADATA_BLOB_BYTES,
        }));
    }

    let config_digest =
        oci::Digest::try_from(manifest.config.digest.as_str()).map_err(|e| PackageErrorKind::Internal(e.into()))?;
    let config_ref = pinned.clone_with_digest(config_digest);
    let bytes = match index
        .fetch_blob(&config_ref)
        .await
        .map_err(PackageErrorKind::Internal)?
    {
        Some(bytes) => bytes,
        None => {
            // The config blob is absent locally and no source could supply it
            // (offline, or it was never cached — e.g. after a bare `ocx index
            // update`, which persists the manifest chain into the index snapshot
            // but not the config blob). Name the missing digest so the user knows
            // what to re-pull, mirroring `resolve_top_manifest`'s offline
            // manifest-missing error — never a bare, digest-less `OfflineMode`.
            return Err(PackageErrorKind::OfflineManifestMissing(Box::new(
                error::OfflineManifestMissing {
                    identifier: pinned.as_identifier().clone(),
                    digest: config_ref.digest(),
                },
            )));
        }
    };

    // D5 step 2 (post-fetch): re-check the actual fetched length — defends
    // against a registry that declares a small size but serves a larger body.
    if bytes.len() > MAX_METADATA_BLOB_BYTES {
        return Err(PackageErrorKind::Internal(crate::Error::MetadataBlobTooLarge {
            size: bytes.len() as i64,
            max: MAX_METADATA_BLOB_BYTES,
        }));
    }

    let raw: metadata::Metadata = serde_json::from_slice(&bytes)
        .map_err(|e| PackageErrorKind::Internal(crate::Error::SerializationFailure(e)))?;
    // Reject malformed metadata at the ingress boundary — refuse to write
    // unvalidated metadata to disk so the consumption-side validation never
    // has to deal with publisher-side bugs after the fact.
    metadata::ValidMetadata::try_from(raw).map_err(PackageErrorKind::Internal)
}

/// Drains a [`JoinSet`] of package tasks and collects results preserving
/// the order given by `packages`.
///
/// Uses an index-based `Vec<Option<T>>` (aligned with `pull.rs::setup_dependencies`)
/// for O(1) slot assignment rather than a `HashMap` + linear reorder pass.
/// A `pending` [`HashSet`] is kept as a panic-fallback sentinel: any ID that
/// completes (success or per-package error) is removed from `pending`; IDs
/// that survive into the post-drain loop indicate a task vanished without
/// reporting back (e.g. it panicked without `resume_unwind`).
///
/// Tasks whose `JoinHandle` reports a panic are recorded as
/// [`PackageErrorKind::TaskPanicked`]. If any errors accumulated, they are
/// wrapped with `error_ctor` and returned as a single batch error.
pub async fn drain_package_tasks<T: 'static>(
    packages: &[oci::Identifier],
    mut tasks: JoinSet<(oci::Identifier, Result<T, PackageErrorKind>)>,
    error_ctor: fn(Vec<PackageError>) -> error::Error,
) -> Result<Vec<T>, error::Error> {
    // Build a reverse index: identifier → slot position in `results`.
    let index_map: HashMap<oci::Identifier, usize> =
        packages.iter().cloned().enumerate().map(|(i, id)| (id, i)).collect();

    let mut pending: HashSet<oci::Identifier> = packages.iter().cloned().collect();
    let mut results: Vec<Option<T>> = std::iter::repeat_with(|| None).take(packages.len()).collect();
    // Errors carry their input slot index so the batch can be sorted back into
    // input order before it is surfaced. `join_next` yields in completion
    // (race) order; without this sort the exit-code classifier — which picks
    // `errors.first()` — would be nondeterministic (quality-rust.md Async
    // Patterns; subsystem-cli-api.md "Report Actual Results"). An id absent
    // from `index_map` (should never happen) sorts last via `usize::MAX`.
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

    // Any ID still in `pending` represents a task that vanished without
    // reporting back (panic without propagation or JoinError without matching
    // Ok/Err from a task that was silently dropped).
    for id in pending {
        let idx = index_map.get(&id).copied().unwrap_or(usize::MAX);
        errors.push((idx, PackageError::new(id, PackageErrorKind::TaskPanicked)));
    }

    if !errors.is_empty() {
        errors.sort_by_key(|(idx, _)| *idx);
        let errors: Vec<PackageError> = errors.into_iter().map(|(_, error)| error).collect();
        return Err(error_ctor(errors));
    }

    // Collect in input order; `flatten` drops the `None` slots left by
    // tasks that reported errors (already surfaced above).
    Ok(results.into_iter().flatten().collect())
}

/// Resolves the top-level manifest for `package` **without** platform
/// selection, deriving the top-level pinned identifier from the tag (or the
/// `@digest` when present) and discriminating "tag truly unknown" from "tag
/// known but manifest blob missing offline".
///
/// When `package` carries no digest the tag is taken from
/// [`oci::Identifier::tag_or_latest`], so a bare repository identifier falls
/// back to the `latest` tag — the same default the `resolve` pipeline uses.
///
/// # Errors
///
/// - [`PackageErrorKind::NotFound`] — tag/digest truly unknown.
/// - [`PackageErrorKind::OfflineManifestMissing`] — known tag but the manifest
///   blob is absent from the local cache in offline mode.
/// - [`PackageErrorKind::Internal`] — index I/O failure.
/// - [`PackageErrorKind::DigestMissing`] — the resolved top-level digest
///   could not be pinned onto the identifier.
// Shared by `resolve::PackageManager::resolve` and `inspect`'s
// `fetch_top_manifest`: both need the identical tag/digest top-id derivation
// plus the not-found-vs-offline split before they diverge (resolve continues
// into platform selection / chain building, inspect adapts the manifest as-is).
// `op` is caller-supplied; both current callers pass `IndexOperation::Resolve`
// (inspect deliberately uses `Resolve`, not `Query` — a prior review Block
// proposing `Query` was rejected: default-mode inspect is a Resolve-class read).
pub async fn resolve_top_manifest(
    index: &oci::index::Index,
    package: &oci::Identifier,
    op: oci::index::IndexOperation,
) -> Result<(oci::PinnedIdentifier, oci::Manifest), PackageErrorKind> {
    let top_id = if package.digest().is_some() {
        package.clone()
    } else {
        package.clone_with_tag(package.tag_or_latest())
    };
    let (top_digest, top_manifest) = match index
        .fetch_manifest(&top_id, op)
        .await
        .map_err(PackageErrorKind::Internal)?
    {
        Some(result) => result,
        None => {
            // Distinguish "tag truly unknown" (NotFound) from "tag cached
            // locally but manifest blob missing from the cache"
            // (OfflineManifestMissing — requires online re-pull). We ask
            // the index for the tag → digest mapping: if that succeeds,
            // the tag is known, so fetch_manifest returning None implies
            // the blob is missing rather than the tag is unknown.
            if let Some(digest) = index
                .fetch_manifest_digest(&top_id, op)
                .await
                .map_err(PackageErrorKind::Internal)?
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

    let top_pinned = oci::PinnedIdentifier::try_from(top_id.clone_with_digest(top_digest))
        .map_err(|_| PackageErrorKind::DigestMissing)?;
    Ok((top_pinned, top_manifest))
}

/// Creates a [`ReferenceManager`] from a [`FileStructure`].
pub fn reference_manager(fs: &file_structure::FileStructure) -> ReferenceManager {
    ReferenceManager::new(fs.clone())
}

/// Materializes the resolver's manifest + config chain into `$OCX_HOME/blobs`
/// and forward-refs each blob into the package's `refs/blobs/`.
///
/// `resolve` persists only **dispatch** objects into the local index
/// collection (`$OCX_HOME/index`, `adr_index_indirection.md` A3) — a leaf platform
/// manifest is never copied there. The install path keeps its own copy in
/// `$OCX_HOME/blobs` (Decision B2): the snapshot travels with a committed
/// `.ocx/index/`, whereas the blob store travels with the machine and is what
/// `refs/blobs/` targets and `add_index_retention_edges` traverses for GC.
/// Routing is role-aware, per [`ChainRole`](super::resolve::ChainRole):
///
/// - [`ChainRole::Config`] — genuine content-addressed blob; fetched via the
///   OCI blobs endpoint ([`Index::fetch_blob`]).
/// - [`ChainRole::Manifest`] — a platform-selected leaf manifest; always
///   genuine content, fetched via the OCI manifests endpoint (digest-verified
///   verbatim bytes, [`Index::fetch_manifest_raw_bytes`]) — the blobs endpoint
///   does not serve manifest digests.
/// - [`ChainRole::Index`] — the top-level dispatch entry. A derived (plain
///   OCI-registry) source names a genuine image-index digest, fetched the
///   same way as [`ChainRole::Manifest`] so `add_index_retention_edges` can
///   later parse it. A published (`index.ocx.sh`) source names an
///   observation-object digest — dispatch, never content (A3) — which no
///   registry ever served, so it is skipped instead of staged.
///
/// Both the blob-store write ([`BlobStore::write_blob`]) and the ref link
/// ([`ReferenceManager::link_blobs`]) are content-addressed and idempotent, so
/// the fast-path branches that re-invoke this helper for an already-installed
/// package pay only cheap existence checks. A chain blob the index cannot
/// serve (offline and never fetched — e.g. the `pull_local` path, which never
/// persists the config blob) is skipped; `link_blobs` tolerates the resulting
/// dangling ref (eventual consistency, GC collects).
///
/// A chain blob already present in the blob store is **guaranteed-local** and is
/// never routed through the index: the local `ocx package test` flow synthesizes
/// its manifest and stages it straight into `fs.blobs` (never the snapshot), so
/// an index lookup would miss and — with a client present — fall through to the
/// registry, which 404s a blob/manifest that was never pushed. The blob-store
/// existence probe short-circuits that registry round-trip while leaving the
/// genuine-remote path (blob absent locally → index → source) untouched.
/// Checks whether `identifier`'s content is already present and valid in
/// `fs.blobs`, healing (removing) a present-but-corrupt copy first (CWE-345
/// — the on-disk bytes are re-hashed against the digest that names them, the
/// same check [`crate::oci::index::chained_index`]'s `recover_absent_leaf`
/// applies to a leaf recovered from the same store). Returns `true` when the
/// caller still needs to fetch and write the bytes.
///
/// The guaranteed-local fast-path check factored out of
/// [`stage_and_link_chain_blobs`] so a caller with no installed package to
/// ref-link into — `inspect`'s closure walker (`tasks/inspect.rs`), which
/// stages a fetched dep's leaf manifest into this same content-addressed
/// cache — can reuse it without pulling in ref-linking. A blob staged this
/// way with no ref is an unreferenced cache entry; `ocx clean` may reclaim
/// it, same as any other cache-warming write.
pub async fn blob_needs_fetch(
    fs: &file_structure::FileStructure,
    identifier: &oci::PinnedIdentifier,
) -> Result<bool, PackageErrorKind> {
    let digest = identifier.digest();
    match fs
        .blobs
        .read_blob(identifier.registry(), &digest)
        .await
        .map_err(PackageErrorKind::Internal)?
    {
        Some(existing) if digest.algorithm().hash(&existing) == digest => Ok(false),
        Some(_) => {
            log::warn!("blob-store copy of chain blob '{digest}' is corrupt; removing and re-fetching");
            fs.blobs
                .remove_blob(identifier.registry(), &digest)
                .await
                .map_err(PackageErrorKind::Internal)?;
            Ok(true)
        }
        None => Ok(true),
    }
}

/// Verifies `bytes` — fetched from an index source under `identifier`'s own
/// claimed digest — actually hash to that digest before a caller persists
/// them into content-addressed storage (CWE-345 trust-boundary check).
///
/// [`Index::fetch_manifest_raw_bytes`] is a distinct seam from
/// [`Index::fetch_blob`]: `fetch_blob` (config blobs) digest-verifies inside
/// `ChainedIndex` itself before returning or writing through
/// (`chained_index.rs`'s `digest_matches`), but `fetch_manifest_raw_bytes`
/// only checks a source's returned bytes are self-consistent with the digest
/// the *source* computed from them — never against the digest the *caller*
/// requested. A source that returns wrong bytes under a self-consistent but
/// unrequested digest would otherwise be written straight into the CAS at
/// the caller's requested digest path unverified. Every caller that persists
/// a `fetch_manifest_raw_bytes` result under `identifier`'s digest
/// (`stage_leaf_manifest`, [`stage_chain_blobs`]'s `Index`/`Manifest` roles)
/// must call this first.
pub(super) fn verify_requested_digest(
    identifier: &oci::PinnedIdentifier,
    bytes: &[u8],
) -> Result<(), PackageErrorKind> {
    let claimed = identifier.digest();
    let computed = claimed.algorithm().hash(bytes);
    if computed != claimed {
        return Err(PackageErrorKind::Internal(
            crate::file_structure::error::Error::DigestMismatch { claimed, computed }.into(),
        ));
    }
    Ok(())
}

/// Stages every blob in `resolved.chain` into `fs.blobs` — role-aware fetch
/// (config via [`Index::fetch_blob`], index/manifest via
/// [`Index::fetch_manifest_raw_bytes`]), `blob_needs_fetch`-gated, no
/// ref-linking. The per-blob staging step of [`stage_and_link_chain_blobs`],
/// factored out so a caller with no installed package to ref-link into
/// (`inspect --deps`, which stages the root's own resolution chain the same
/// way it stages each dep node — `tasks/inspect.rs`) can warm the content
/// cache without pulling in ref-linking. See [`blob_needs_fetch`]'s doc for
/// the unreferenced-cache-entry contract this leaves behind.
pub async fn stage_chain_blobs(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
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
                if let Some(bytes) = index.fetch_blob(identifier).await.map_err(PackageErrorKind::Internal)? {
                    fs.blobs
                        .write_blob(identifier.registry(), &identifier.digest(), &bytes)
                        .await
                        .map_err(PackageErrorKind::Internal)?;
                }
            }
            ChainRole::Index | ChainRole::Manifest => {
                // A published source's `Index`-role entry names an
                // observation-object digest — dispatch, never content (A3).
                // No registry serves it; skip rather than stage.
                if blob.role == ChainRole::Index
                    && index
                        .physical_reference(identifier.as_identifier())
                        .await
                        .map_err(PackageErrorKind::Internal)?
                        .is_some()
                {
                    continue;
                }
                if let Some((bytes, _, _)) = index
                    .fetch_manifest_raw_bytes(identifier.as_identifier())
                    .await
                    .map_err(PackageErrorKind::Internal)?
                {
                    verify_requested_digest(identifier, &bytes)?;
                    fs.blobs
                        .write_blob(identifier.registry(), &identifier.digest(), &bytes)
                        .await
                        .map_err(PackageErrorKind::Internal)?;
                }
            }
        }
    }
    Ok(())
}

pub async fn stage_and_link_chain_blobs(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    content_path: &Path,
    resolved: &super::resolve::ResolvedChain,
) -> Result<(), PackageErrorKind> {
    stage_chain_blobs(fs, index, resolved).await?;
    reference_manager(fs)
        .link_blobs(content_path, resolved.blobs())
        .await
        .map_err(PackageErrorKind::Internal)
}

/// Acquires the per-repo selection lock for `package`.
///
/// The lock file lives at `{symlinks/{registry}/{repo}}/.select.lock` and
/// serializes mutations of the per-repo `current` symlink across
/// `install --select`, `deselect`, and `uninstall --deselect`. The returned
/// [`LockedFile`] guard releases the lock on drop.
pub async fn acquire_select_lock(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
) -> Result<LockedFile, PackageErrorKind> {
    let lock_path = fs.symlinks.select_lock(package);
    LockedFile::open_exclusive(lock_path)
        .await
        .map_err(PackageErrorKind::Internal)
}

/// Outcome of [`wire_selection`] for the caller's reporting.
///
/// Each field is `Some` only when that symlink was actually written this call.
/// The host-only gate (issue #179) suppresses a foreign-platform write, and a
/// plain install without `--select` never writes `current`, so callers must
/// report the real outcome rather than recomputing a path that may not exist.
#[derive(Debug, Clone, Default)]
pub struct WireSelectionOutcome {
    /// The `current` symlink written this call, or `None` when `select` was not
    /// requested or the resolved platform is not host-runnable.
    pub current: Option<std::path::PathBuf>,
    /// The `candidates/{tag}` symlink written this call, or `None` when no
    /// candidate was requested or the resolved platform is not host-runnable.
    pub candidate: Option<std::path::PathBuf>,
}

/// Wires the per-repo `current` selection symlink for `package` and optionally
/// writes the candidate symlink first. Both symlinks target the package root,
/// so consumers traverse `<symlink>/content/`, `<symlink>/entrypoints/`, or
/// `<symlink>/metadata.json` from a single anchor.
///
/// Shared by [`super::install::create_install_symlinks`] and the CLI `select`
/// command so both paths run identical lock acquisition and symlink logic.
/// Entrypoint name collision detection lives in
/// [`super::super::composer::check_entrypoints`], called from
/// `pull.rs` at install Stage 1 against the interface projection of the
/// transitive closure.
///
/// # Lock order
///
/// Acquires the per-repo `.select.lock` before the symlink write. See module
/// docs for the updated lock hierarchy.
///
/// # Errors
///
/// - [`PackageErrorKind::Internal`] for I/O or symlink failures.
#[allow(clippy::result_large_err)]
pub async fn wire_selection(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
    info: &InstallInfo,
    candidate: bool,
    select: bool,
) -> Result<WireSelectionOutcome, PackageErrorKind> {
    let rm = reference_manager(fs);

    // Both `current` and `candidates/{tag}` target the package root.
    let pkg_root = info.dir().dir.as_path();

    // The host-only gate (issue #179): `candidates/{tag}` and `current` are
    // per-repo, platform-agnostic paths that platformless readers (`ocx package
    // which`, project env) expect to resolve to host-runnable content. A
    // foreign-platform install (e.g. `-p windows/amd64` on a Linux host) still
    // lands in the object store, but must not clobber either host pointer;
    // cross-platform consumers resolve digest-pinned roots directly instead.
    let host_runnable = info.is_host_runnable();

    let candidate_written = if candidate && host_runnable {
        let link_path = fs.symlinks.candidate(package);
        log::debug!("Creating candidate symlink at '{}'.", link_path.display());
        rm.link(&link_path, pkg_root).map_err(PackageErrorKind::Internal)?;
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

    // Acquire the per-repo .select.lock for the symlink write.
    let _select_guard = acquire_select_lock(fs, package).await?;

    // Snapshot the prior `current` symlink target so rollback can restore it
    // on symlink write failure.
    let prior_current_target = tokio::fs::read_link(&current_path).await.ok();

    // Commit the `current` symlink. Failure here triggers rollback of the
    // prior symlink target before the error is surfaced.
    log::debug!("Creating current symlink at '{}'.", current_path.display());
    if let Err(e) = rm.link(&current_path, pkg_root) {
        rollback_symlink(&rm, &current_path, prior_current_target.as_deref());
        return Err(PackageErrorKind::Internal(e));
    }

    Ok(WireSelectionOutcome {
        current: Some(current_path),
        candidate: candidate_written,
    })
}

/// RAII guard for the per-repo `.select.lock`. Releases on drop.
///
/// Used by `deselect` / `uninstall --deselect` to hold the same critical
/// section as [`wire_selection`] while unlinking the symlink pair.
pub struct SelectionLocks {
    _select: LockedFile,
}

/// Acquires the per-repo `.select.lock`.
///
/// Serializes mutations of the per-repo `current` symlink across
/// `install --select`, `deselect`, and `uninstall --deselect`.
#[allow(clippy::result_large_err)]
pub async fn acquire_selection_locks(
    fs: &file_structure::FileStructure,
    package: &oci::Identifier,
) -> Result<SelectionLocks, PackageErrorKind> {
    let select = acquire_select_lock(fs, package).await?;
    Ok(SelectionLocks { _select: select })
}

/// Restores a symlink to its prior state after a partial-select failure.
///
/// On any rollback failure we log and continue: the caller already has a real
/// error to surface, and burying it under a rollback secondary failure would
/// obscure the root cause.
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

#[cfg(test)]
mod tests {
    use crate::file_structure::{FileStructure, PackageStore};
    use crate::oci;
    use crate::package::resolved_package::ResolvedPackage;
    use crate::prelude::SerdeExt as _;

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
        use crate::package_manager::error::{Error, PackageErrorKind};
        use std::time::Duration;
        use tokio::task::JoinSet;

        let pkg0 = oci::Identifier::new_registry("alpha", "example.com");
        let pkg1 = oci::Identifier::new_registry("bravo", "example.com");
        let packages = vec![pkg0.clone(), pkg1.clone()];

        let mut tasks: JoinSet<(oci::Identifier, Result<(), PackageErrorKind>)> = JoinSet::new();
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

    /// Writes valid + resolve.json under a fake content path, then writes a
    /// metadata.json that references an undeclared dep — `load_object_data`
    /// must reject it instead of returning a half-formed `InstallInfo`.
    #[tokio::test]
    async fn load_object_data_rejects_invalid_metadata_at_consumption() {
        let tempdir = tempfile::tempdir().unwrap();
        let store_root = tempdir.path().join("packages");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = PackageStore::new(&store_root);

        let digest_hex: String = "ab".repeat(32);
        let id =
            oci::Identifier::new_registry("foo/bar", "example.com").clone_with_digest(oci::Digest::Sha256(digest_hex));
        let pinned = oci::PinnedIdentifier::try_from(id).unwrap();

        let pkg_dir = store.path(&pinned);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let content_dir = pkg_dir.join("content");
        std::fs::create_dir_all(&content_dir).unwrap();

        // Bad metadata: env var references `${deps.missing.installPath}` with no
        // matching declared dep — must trigger ValidMetadata's UnknownDependencyRef.
        let bad = r#"{"type":"bundle","version":1,"dependencies":[],"env":[{"key":"FOO","value":"${deps.missing.installPath}/x"}]}"#;
        std::fs::write(pkg_dir.join("metadata.json"), bad).unwrap();
        ResolvedPackage::new()
            .write_json(pkg_dir.join("resolve.json"))
            .await
            .unwrap();

        let result = super::load_object_data(&store, &content_dir).await;
        assert!(result.is_err(), "expected validation failure, got Ok");
        let err = result.unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("missing"),
            "error chain must mention undeclared dep name 'missing': {chain}"
        );
    }

    /// Builds a valid, installed `InstallInfo` under `fs` for `foo/bar:1.0` and
    /// returns it paired with the tagged identifier whose `candidates/{tag}`
    /// slot `wire_selection` targets.
    async fn install_info_fixture(fs: &FileStructure) -> (oci::Identifier, crate::package::install_info::InstallInfo) {
        let digest_hex: String = "cd".repeat(32);
        let tagged = oci::Identifier::new_registry("foo/bar", "example.com")
            .clone_with_tag("1.0")
            .clone_with_digest(oci::Digest::Sha256(digest_hex));
        let pinned = oci::PinnedIdentifier::try_from(tagged.clone()).unwrap();

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
        let dir = crate::file_structure::PackageDir::with_root(pkg_dir);
        let info = crate::package::install_info::InstallInfo::new(pinned, metadata, resolved, dir);
        (tagged, info)
    }

    /// A supported platform the current host cannot run, or `None` when the host
    /// platform is undeterminable (unsupported CI arch), in which case the gate
    /// writes unconditionally and suppression cannot be exercised.
    fn a_foreign_platform() -> Option<oci::Platform> {
        ["windows/amd64", "linux/amd64", "darwin/arm64", "linux/arm64"]
            .into_iter()
            .map(|spec| spec.parse::<oci::Platform>().expect("valid platform string"))
            .find(|platform| !oci::Platform::host_can_run(Some(platform)))
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
            !crate::symlink::is_link(&candidate_path),
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

    /// `acquire_select_lock` materializes the per-repo lock file and returns
    /// a guard. Serializes Cluster 3's transactional select state.
    #[tokio::test]
    async fn acquire_select_lock_creates_lock_file_at_expected_path() {
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let id = oci::Identifier::new_registry("cmake", "example.com");

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
        let id = oci::Identifier::new_registry("cmake", "example.com");

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
        manifest: Option<(oci::Digest, Vec<u8>, oci::Manifest)>,
        blob: Option<(oci::Digest, Vec<u8>)>,
        raw_bytes_calls: std::sync::Arc<std::sync::Mutex<Vec<oci::Digest>>>,
        blob_calls: std::sync::Arc<std::sync::Mutex<Vec<oci::Digest>>>,
    }

    #[async_trait::async_trait]
    impl crate::oci::index::IndexImpl for EndpointSpySource {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &oci::Identifier,
            _: crate::oci::index::IndexOperation,
        ) -> crate::Result<Option<(oci::Digest, oci::Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(
            &self,
            _: &oci::Identifier,
            _: crate::oci::index::IndexOperation,
        ) -> crate::Result<Option<oci::Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
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
            identifier: &oci::Identifier,
        ) -> crate::Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
            let Some(digest) = identifier.digest() else {
                return Ok(None);
            };
            self.raw_bytes_calls.lock().unwrap().push(digest.clone());
            Ok(self
                .manifest
                .as_ref()
                .filter(|(d, _, _)| *d == digest)
                .map(|(d, bytes, manifest)| (bytes.clone(), d.clone(), manifest.clone())))
        }
        async fn physical_reference(&self, identifier: &oci::Identifier) -> crate::Result<Option<oci::Identifier>> {
            if self.published && identifier.registry() == self.namespace {
                Ok(Some(identifier.clone()))
            } else {
                Ok(None)
            }
        }
        fn box_clone(&self) -> Box<dyn crate::oci::index::IndexImpl> {
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
        use crate::file_structure::{FileStructure, IndexStore};
        use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
        use crate::package_manager::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "example.com";
        let repository = "cmake";

        let manifest_bytes = br#"{"manifest":true}"#.to_vec();
        let manifest_digest = oci::Algorithm::Sha256.hash(&manifest_bytes);
        let config_bytes = br#"{"config":true}"#.to_vec();
        let config_digest = oci::Algorithm::Sha256.hash(&config_bytes);

        let source = EndpointSpySource {
            namespace: registry.to_string(),
            published: false,
            manifest: Some((
                manifest_digest.clone(),
                manifest_bytes.clone(),
                oci::Manifest::Image(oci::ImageManifest::default()),
            )),
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

        let pin = |digest: &oci::Digest| {
            oci::PinnedIdentifier::try_from(
                oci::Identifier::new_registry(repository, registry).clone_with_digest(digest.clone()),
            )
            .unwrap()
        };
        let chain_blob = |digest: &oci::Digest, role| ChainBlob {
            identifier: pin(digest),
            role,
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            size: 0,
        };
        let resolved = ResolvedChain {
            pinned: pin(&manifest_digest),
            transport_pinned: pin(&manifest_digest),
            chain: vec![
                chain_blob(&manifest_digest, ChainRole::Manifest),
                chain_blob(&config_digest, ChainRole::Config),
            ],
            final_manifest: oci::ImageManifest::default(),
            platform: oci::Platform::any(),
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

    /// A3 (`adr_index_indirection.md`): an index-resolved (multi-platform)
    /// chain's `ChainRole::Index` entry is the top-level dispatch pointer. For
    /// a published (`index.ocx.sh`) source that digest is an observation
    /// object's own digest — a local-only construct no registry ever served —
    /// so it must never be staged (never requested through either endpoint).
    /// The selected leaf manifest and config still stage normally.
    #[tokio::test(flavor = "multi_thread")]
    async fn stage_and_link_chain_blobs_never_stages_a_published_index_roles_obs_digest() {
        use crate::file_structure::{FileStructure, IndexStore};
        use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
        use crate::package_manager::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "ocx.sh";
        let repository = "ns/cmake";

        let obs_digest = oci::Algorithm::Sha256.hash(b"observation object bytes never staged");
        let manifest_bytes = br#"{"manifest":true}"#.to_vec();
        let manifest_digest = oci::Algorithm::Sha256.hash(&manifest_bytes);
        let config_bytes = br#"{"config":true}"#.to_vec();
        let config_digest = oci::Algorithm::Sha256.hash(&config_bytes);

        let source = EndpointSpySource {
            namespace: registry.to_string(),
            published: true,
            manifest: Some((
                manifest_digest.clone(),
                manifest_bytes.clone(),
                oci::Manifest::Image(oci::ImageManifest::default()),
            )),
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

        let pin = |digest: &oci::Digest| {
            oci::PinnedIdentifier::try_from(
                oci::Identifier::new_registry(repository, registry).clone_with_digest(digest.clone()),
            )
            .unwrap()
        };
        let chain_blob = |digest: &oci::Digest, role| ChainBlob {
            identifier: pin(digest),
            role,
            media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
            size: 0,
        };
        let resolved = ResolvedChain {
            pinned: pin(&manifest_digest),
            transport_pinned: pin(&manifest_digest),
            chain: vec![
                chain_blob(&obs_digest, ChainRole::Index),
                chain_blob(&manifest_digest, ChainRole::Manifest),
                chain_blob(&config_digest, ChainRole::Config),
            ],
            final_manifest: oci::ImageManifest::default(),
            platform: oci::Platform::any(),
        };

        let content_path = tempdir.path().join("pkg-content");
        std::fs::create_dir_all(&content_path).unwrap();

        super::stage_and_link_chain_blobs(&fs, &index, &content_path, &resolved)
            .await
            .expect("staging the resolved chain into the blob store must succeed");

        assert!(
            fs.blobs.read_blob(registry, &obs_digest).await.unwrap().is_none(),
            "the observation-object digest must never be staged into the blob store"
        );
        assert_eq!(
            fs.blobs.read_blob(registry, &manifest_digest).await.unwrap().as_deref(),
            Some(manifest_bytes.as_slice()),
            "the selected leaf manifest must still be materialized"
        );
        assert_eq!(
            fs.blobs.read_blob(registry, &config_digest).await.unwrap().as_deref(),
            Some(config_bytes.as_slice()),
            "the config blob must still be materialized"
        );

        assert!(
            !raw_bytes_calls.lock().unwrap().contains(&obs_digest),
            "the obs digest must never be requested through the manifests endpoint"
        );
        assert!(
            !blob_calls.lock().unwrap().contains(&obs_digest),
            "the obs digest must never be requested through the blobs endpoint"
        );
        assert_eq!(
            raw_bytes_calls.lock().unwrap().as_slice(),
            [manifest_digest],
            "only the selected leaf manifest crosses the manifests endpoint"
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
        use crate::file_structure::{FileStructure, IndexStore};
        use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
        use crate::package_manager::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let snapshot = IndexStore::new(tempdir.path().join("index"));
        let registry = "example.com";
        let repository = "cmake";

        let manifest_bytes = br#"{"local":"manifest"}"#.to_vec();
        let manifest_digest = oci::Algorithm::Sha256.hash(&manifest_bytes);

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

        let pinned = oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(repository, registry).clone_with_digest(manifest_digest.clone()),
        )
        .unwrap();
        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: pinned.clone(),
            chain: vec![ChainBlob {
                identifier: pinned.clone(),
                role: ChainRole::Manifest,
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                size: i64::try_from(manifest_bytes.len()).unwrap(),
            }],
            final_manifest: oci::ImageManifest::default(),
            platform: oci::Platform::any(),
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
        use crate::file_structure::IndexStore;
        use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
        use crate::package_manager::error::PackageErrorKind;

        let tempdir = tempfile::tempdir().unwrap();
        // Offline index over an empty snapshot → `fetch_blob` always yields None.
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(tempdir.path().join("index")),
            }),
            vec![],
            ChainMode::Offline,
        );

        let config_digest = oci::Algorithm::Sha256.hash(b"config-bytes");
        let manifest_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{}","digest":"{}","size":12}},"layers":[]}}"#,
            crate::MEDIA_TYPE_PACKAGE_METADATA_V1,
            config_digest,
        );
        let oci::Manifest::Image(image_manifest) = serde_json::from_str(&manifest_json).unwrap() else {
            panic!("fixture must parse as an image manifest");
        };

        let pinned = oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry("cmake", "example.com")
                .clone_with_tag("3.28")
                .clone_with_digest(oci::Algorithm::Sha256.hash(b"manifest-bytes")),
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
        let id_a = oci::Identifier::new_registry("cmake", "example.com");
        let id_b = oci::Identifier::new_registry("ninja", "example.com");

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
