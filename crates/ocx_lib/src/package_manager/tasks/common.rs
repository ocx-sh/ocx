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
/// The identifier must carry a digest. Returns the installed package description pull if
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
/// Metadata is checked for *structural readability* via
/// [`metadata::ValidMetadata`] before being returned. That is deliberately
/// weaker than the publish gate (D14): a consumption path never refuses a
/// document because one of its tokens is unrecognised, so metadata written by a
/// newer ocx still loads and still shows. The refusal moves to the operation
/// that needs the value — `TemplateResolver::resolve` cannot produce bytes for
/// a token it does not recognise.
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
    //
    // `crate::Error::UnsupportedMediaType` already classifies as `DataError`
    // (65), matching every sibling artifact-type gate. Carry it straight
    // through: wrapping it in `ClientError::internal` would classify as the
    // terminal `Failure` (1) and end the chain walk before the inner error is
    // ever consulted.
    media_type_select(&manifest.config.media_type, &[MEDIA_TYPE_PACKAGE_METADATA_V1])
        .map_err(PackageErrorKind::Internal)?;

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
    // Reject structurally unreadable metadata at the ingress boundary — a
    // modifier type this binary cannot interpret, a `list` entry violating the
    // separator contract. Not the token checks: those left this layer with D14,
    // because a fetch refusing a token it does not recognise would make an ocx
    // unable to even *show* a package a newer ocx published.
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

/// Checks whether `identifier`'s content is already present and valid in
/// `fs.blobs`, healing (removing) a present-but-corrupt copy first (CWE-345
/// — the on-disk bytes are re-hashed against the digest that names them, the
/// same check [`crate::oci::index::chained_index`]'s
/// `recover_absent_dispatch` applies to a dispatch object recovered from the
/// same store). Returns `true` when the caller still needs to fetch and write
/// the bytes.
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
/// - [`ChainRole::Index`] — the top-level dispatch entry, which is an OCI
///   image index whatever the source. Fetched the same way as
///   [`ChainRole::Manifest`], so `add_index_retention_edges` can later parse
///   the staged blob and hang each advertised child leaf's retention edge
///   off it.
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
/// existence probe ([`blob_needs_fetch`]) short-circuits that registry
/// round-trip while leaving the genuine-remote path (blob absent locally →
/// index → source) untouched.
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

    // A deferred root's package directory has not been created — the shim tree
    // creates it on first invocation — so pointing `candidates/{tag}` or
    // `current` at it would publish a dangling install into the one namespace
    // users address by name. No live path mints one here (`--lazy-mode` is off
    // `install`/`select` by contract, and only `compose_roots` builds a deferred
    // root), so this refuses a state the flag grammar already prevents — which
    // is the point: the type, not the grammar, is what a future caller meets.
    if info.deferred().is_some() {
        return Err(PackageErrorKind::Internal(crate::error::file_error(
            pkg_root,
            std::io::Error::other("refusing to point an install symlink at a deferred package"),
        )));
    }

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

// ── Metadata-only dependency closure walker (ADR D3) ────────────────────────
//
// Shared by two genuinely different callers, which is why it lives here rather
// than inside either of them: `inspect --deps` projects the node list into a
// report (`Surface` / `ClosureConflicts`), and `prepare_lazy` derives a shim
// name set from it (plan contract C-008 (a), which forbids a second walker).
// The *projection* stays in `inspect.rs` — it is report shape, not walk.

/// Phase-1 gather concurrency bound — caps how many per-node fetches
/// [`gather_closure_nodes`]'s admission queue spawns at once over the closure
/// frontier (ADR D3 panel W5). Codex C2: this bounds SPAWNED tasks, not just
/// running fetch bodies — see `gather_closure_nodes`'s doc.
pub(super) const CLOSURE_FETCH_CONCURRENCY: usize = 8;

/// One node of a metadata-only dependency closure.
#[derive(Debug)]
pub struct ClosureNode {
    /// Digest-addressed; advisory tag preserved for display.
    pub identifier: oci::PinnedIdentifier,
    /// Digest of the node's OCX metadata config blob, in the same registry as
    /// [`identifier`](Self::identifier) — pair the two to address it
    /// (`BlobStore::data(node.identifier.registry(), &node.config_digest)`).
    ///
    /// Carried because a *deferred* tool has no package directory: its config
    /// blobs are reachable only through the shim tree's `refs/blobs/`, and the
    /// generation task has no second walk to re-derive them from (plan
    /// contracts C-008 / C-014 / C-020). `inspect` ignores it.
    pub config_digest: oci::Digest,
    /// Composed from the root via `Visibility::through_edge`/`merge`. `None`
    /// iff `is_root` — the composed-from-root axis is undefined for the root
    /// itself (the wire key is absent exactly when `root: true`).
    pub effective_visibility: Option<metadata::visibility::Visibility>,
    /// Tri-state, straight from the node's `Bundle.binaries`: key absent on
    /// the wire means undeclared; `Some(empty)` means the publisher asserts
    /// zero interface executables.
    pub binaries: Option<metadata::Binaries>,
    /// The node's declared entrypoint map keys.
    pub entrypoints: Vec<metadata::EntrypointName>,
    /// The node's own env vars, each carrying its declared visibility so the
    /// interface (`has_interface`) and private (`has_private`) surface
    /// projections can filter per-axis at aggregate time.
    pub env: Vec<ClosureEnvVar>,
    /// The node's declared integration namespace keys, in `BTreeMap` order.
    /// Keys only — see `inspect::Surface::integrations` for why no payload.
    pub integrations: Vec<String>,
    /// The node's own declared dependency edges (as authored).
    pub dependencies: Vec<ClosureEdge>,
    pub is_root: bool,
}

/// One declared environment variable of a [`ClosureNode`]: the key, its
/// modifier kind (path vs constant), and its declared visibility (which surface
/// axes it crosses). The value is deliberately absent — a `${installPath}`-
/// templated value is only concrete after install, and the surface summary is a
/// "what keys" claim, not a resolved environment.
#[derive(Debug, Clone)]
pub struct ClosureEnvVar {
    pub key: String,
    pub kind: metadata::env::modifier::ModifierKind,
    /// The declared separator for a `list`-kind var; `None` for every other
    /// kind. Package metadata requires `list` to carry one, so this is only
    /// ever `None` for a non-list var — declaration order is preserved and
    /// there is no cross-node agreement to settle here, unlike the applied
    /// entries `ocx env` composes.
    pub separator: Option<String>,
    pub visibility: metadata::visibility::Visibility,
}

/// A declared dependency edge (as authored), carrying its declared visibility.
#[derive(Debug, Clone)]
pub struct ClosureEdge {
    pub identifier: oci::PinnedIdentifier,
    /// The DECLARED edge visibility (goal #2 — "dependencies state their
    /// linkage visibility"), as distinct from [`ClosureNode::effective_visibility`]
    /// (the composed-from-root visibility).
    pub visibility: metadata::visibility::Visibility,
    pub name: metadata::dependency::DependencyName,
}

/// One gathered closure node: its RESOLVED pinned identity (the
/// platform-selected child for an image-index-pinned dep, unchanged for a
/// flat dep — Codex C1, matches install-time resolution — `pull.rs`'s
/// `info.identifier()` is likewise the resolved identity, never the index),
/// its config-blob digest, its validated metadata, and its own declared
/// dependency edges. [`gather_closure_nodes`]'s output element type.
type GatheredClosureNode = (
    oci::PinnedIdentifier,
    oci::Digest,
    metadata::ValidMetadata,
    Vec<ClosureEdge>,
);

/// A [`GatheredClosureNode`] tagged with its spawn slot and the edge's
/// DECLARED identity (as authored — may differ from the gathered node's
/// resolved identity for an image-index-pinned dep), so completion order
/// (nondeterministic per `JoinSet::join_next`) can be re-sorted back into
/// deterministic spawn order (quality-rust.md JoinSet rule) and
/// [`gather_closure_nodes`]'s declared→resolved alias can be built.
type SlottedClosureNode = (
    usize,
    oci::PinnedIdentifier,
    oci::PinnedIdentifier,
    oci::Digest,
    metadata::ValidMetadata,
    Vec<ClosureEdge>,
);

/// Stages `pinned`'s raw manifest bytes into the machine-global blob store
/// when not already local — mirrors [`stage_and_link_chain_blobs`]'s
/// `ChainRole::Manifest` step (`adr_index_indirection.md` A3/B2: the local
/// index never caches a leaf, only dispatch objects; `$OCX_HOME/blobs` is the
/// sanctioned content-cache home). No ref-link: the walker has no installed
/// package directory to link into, so the staged blob is an unreferenced
/// cache entry — `ocx clean` may reclaim it, same as any other cache-warming
/// write. Shared by the root's own fetch and each dep's fetch
/// ([`fetch_closure_node`]).
///
/// # Errors
///
/// Returns an error if the manifest cannot be fetched, fails its digest
/// re-verification, or cannot be written to the blob store.
pub async fn stage_leaf_manifest(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    pinned: &oci::PinnedIdentifier,
) -> Result<(), PackageErrorKind> {
    if blob_needs_fetch(fs, pinned).await?
        && let Some((bytes, _, _)) = index
            .fetch_manifest_raw_bytes(pinned.as_identifier())
            .await
            .map_err(PackageErrorKind::Internal)?
    {
        // `fetch_manifest_raw_bytes` verifies the source's returned bytes are
        // self-consistent with the digest the source computed, never against
        // the digest actually requested (CWE-345) — re-verify against
        // `pinned`'s own digest before this write, mirroring
        // `stage_chain_blobs`'s identical check for the same seam.
        verify_requested_digest(pinned, &bytes)?;
        fs.blobs
            .write_blob(pinned.registry(), &pinned.digest(), &bytes)
            .await
            .map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}

/// Two-phase metadata-only closure walker: Phase 1 parallel metadata gather
/// (I/O-bound, via [`gather_closure_nodes`]), Phase 2 pure visibility fold
/// (via [`fold_effective_visibility`]).
///
/// Returns the flat, deduped node list in transitive-closure order (deps
/// before dependents, root last). Diamonds appear once with the most-open
/// merged visibility. Projecting that list onto a surface is the caller's
/// concern, not this walk's.
///
/// `root_config_digest` is the root's own metadata config-blob digest, which
/// only the caller that fetched the root manifest has; every other node's is
/// read during the gather.
///
/// Fail-closed: any single node error aborts the whole closure — a partial
/// closure must never render as a complete one.
///
/// # Errors
///
/// See `adr_inspect_metadata_closure.md` Error Taxonomy: dep manifest/config
/// absent under offline policy → `PackageErrorKind::Internal(crate::Error::OfflineMode)`;
/// dep genuinely absent with a source consulted → `PackageErrorKind::NotFound`;
/// malformed / wrong-media-type / over-cap config → the existing
/// [`load_config_metadata`] errors; dep image-index child with no platform
/// match → `PackageErrorKind::FeatureMismatch`.
pub async fn walk_closure_nodes(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    offline: bool,
    root_pinned: &oci::PinnedIdentifier,
    root_metadata: &metadata::ValidMetadata,
    root_config_digest: oci::Digest,
    platform: &oci::Platform,
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

/// Per-gather invariants shared by every spawned fetch — grouped so `spawn`
/// stays under the arg-count lint instead of taking each field separately.
struct GatherContext<'a> {
    fs: &'a file_structure::FileStructure,
    index: &'a oci::index::Index,
    offline: bool,
    platform: &'a oci::Platform,
}

/// Phase 1 — parallel metadata gather. BFS the DAG from `frontier` (the
/// root's declared dependency edges, deduped by advisory-stripped
/// DECLARED-identity), fetching each *unique* edge's node concurrently
/// through a bounded ADMISSION QUEUE: discovered edges wait in `pending`
/// until fewer than [`CLOSURE_FETCH_CONCURRENCY`] fetches are spawned, so the
/// bound caps outstanding tasks, not merely running fetch bodies (Codex C2 —
/// the prior `Semaphore` only gated the fetch body, so a wide frontier still
/// spawned every edge's task immediately). Digest-addressed edges make
/// cycles impossible, so the BFS always terminates. Fail-closed: any node
/// fetch error aborts the whole gather.
///
/// Per-node fetch: `fetch_manifest(dep.identifier, IndexOperation::Resolve)`
/// then [`load_config_metadata`] for an image manifest, or platform-select the
/// child then [`load_config_metadata`] for a dep pinned to an image index (ADR
/// D3 "Per-node fetch").
///
/// Returns each gathered node's RESOLVED pinned identifier, its config-blob
/// digest, its [`metadata::ValidMetadata`], and its own declared dependency
/// edges (feeding [`fold_effective_visibility`]) — deduped by RESOLVED
/// identity, since two different declared edges (a direct edge and an
/// image-index edge) can resolve to the same digest (Codex C1). Also returns
/// the declared→resolved alias map [`fold_effective_visibility`] needs to
/// translate a [`ClosureEdge`] (always as-authored) to the node it actually
/// reached.
async fn gather_closure_nodes(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    offline: bool,
    frontier: Vec<ClosureEdge>,
    platform: &oci::Platform,
) -> Result<
    (
        Vec<GatheredClosureNode>,
        HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
    ),
    PackageErrorKind,
> {
    let context = GatherContext {
        fs,
        index,
        offline,
        platform,
    };
    let mut visited: HashSet<oci::PinnedIdentifier> = HashSet::new();
    let mut tasks: JoinSet<Result<SlottedClosureNode, PackageErrorKind>> = JoinSet::new();
    let mut next_slot = 0usize;
    // Discovered edges not yet admitted into `tasks` — `admit` drains this
    // up to the concurrency bound; slot numbers are assigned at DISCOVERY
    // time (below), independent of admission order, so the final ordering
    // stays deterministic regardless of scheduling.
    let mut pending: std::collections::VecDeque<(usize, ClosureEdge)> = std::collections::VecDeque::new();

    // Spawns one fetch for `edge` into slot `slot`. A plain function (not a
    // closure) so it can be called from `admit` without fighting the borrow
    // checker over a captured `&mut JoinSet`.
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

    // Admits queued edges into `tasks` up to the concurrency bound — the
    // admission bound itself (Codex C2), enforced by never having more than
    // `CLOSURE_FETCH_CONCURRENCY` tasks spawned at once, rather than a
    // `Semaphore` gating only the fetch body inside an already-spawned task.
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

    // Results indexed by spawn slot for deterministic ordering
    // (quality-rust.md JoinSet rule) — `join_next` completion order is
    // otherwise nondeterministic.
    let mut slots: Vec<Option<GatheredClosureNode>> = Vec::new();
    // Declared (as-authored) identity → RESOLVED identity, built as each
    // fetch completes. `fold_effective_visibility` needs this because a
    // `ClosureEdge` always names the DECLARED identity, which for an
    // image-index-pinned dep differs from the node it resolved to.
    let mut resolved_identity: HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier> = HashMap::new();
    // RESOLVED identities already gathered (Codex C1/C2 post-selection
    // dedup). The `visited` check above dedups by DECLARED edge identity,
    // which cannot see that two different declared edges (e.g. a direct edge
    // and an image-index edge selecting the same child) resolve to the same
    // digest — this second, post-fetch check catches that and drops the
    // duplicate fetch's node instead of inserting a second one (double
    // counting its claims / manufacturing a false repo conflict downstream).
    let mut resolved_seen: HashSet<oci::PinnedIdentifier> = HashSet::new();
    while let Some(joined) = tasks.join_next().await {
        // Fail-closed: the `?`s below return early on the first node error,
        // dropping `tasks` — `JoinSet::drop` aborts every task still
        // in-flight, so a partial closure is never observed by the caller.
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

        // Drop a duplicate resolution instead of inserting a second node;
        // `slots[slot]` stays `None` and is filtered out below. Still worth
        // discovering `edges` above regardless of the outcome here — content
        // addressing guarantees a duplicate's declared deps are byte-identical
        // to the first resolution's, so `visited` already no-ops any re-spawn.
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

/// Per-node fetch for the closure walker (ADR D3 "Per-node fetch"): fetches
/// `dep_pinned`'s manifest via the same `IndexOperation::Resolve` routing
/// `inspect`'s default mode uses (local-first, write-through on miss), loads
/// its OCX metadata, and returns the node's RESOLVED identity plus its
/// config-blob digest, its metadata and its own declared dependency edges.
///
/// A dep pinned to an image index (hand-authored — ordinary `ocx package
/// create` always pins a platform-manifest digest, `dependency_pinning.rs`)
/// platform-selects the child before loading its config; the returned
/// identity is the SELECTED CHILD's digest — `dep_pinned`'s own advisory tag
/// is preserved on it (`Index::fetch_candidates` derives the candidate via
/// `identifier.clone_with_digest`, so the tag survives selection unchanged)
/// — matching install-time resolution (`pull.rs`'s `info.identifier()` is
/// likewise the platform-selected identity, never the index digest; Codex
/// C1). The authored index reference stays visible, unchanged, on the
/// parent's [`ClosureEdge`] — only the node's own identity and the gather-time
/// dedup key move to the resolved child.
async fn fetch_closure_node(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    offline: bool,
    dep_pinned: &oci::PinnedIdentifier,
    platform: &oci::Platform,
) -> Result<
    (
        oci::PinnedIdentifier,
        oci::Digest,
        metadata::ValidMetadata,
        Vec<ClosureEdge>,
    ),
    PackageErrorKind,
> {
    let dep_identifier = dep_pinned.as_identifier().clone();
    let manifest = match index
        .fetch_manifest(&dep_identifier, oci::index::IndexOperation::Resolve)
        .await
        .map_err(PackageErrorKind::Internal)?
    {
        Some((_, manifest)) => manifest,
        None => return Err(closure_fetch_miss(offline)),
    };

    let (resolved_pinned, image) = match manifest {
        oci::Manifest::Image(img) => (dep_pinned.clone(), img),
        oci::Manifest::ImageIndex(_) => {
            let selected = match index
                .select(&dep_identifier, platform, oci::index::IndexOperation::Resolve)
                .await
                .map_err(PackageErrorKind::Internal)?
            {
                oci::index::SelectResult::Found(id) => id,
                oci::index::SelectResult::Ambiguous(candidates) => {
                    return Err(PackageErrorKind::SelectionAmbiguous(candidates));
                }
                oci::index::SelectResult::NotFound => return Err(PackageErrorKind::NotFound),
                oci::index::SelectResult::FeatureMismatch {
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
                oci::PinnedIdentifier::try_from(selected.clone()).map_err(|_| PackageErrorKind::DigestMissing)?;
            let image = match index
                .fetch_manifest(&selected, oci::index::IndexOperation::Resolve)
                .await
                .map_err(PackageErrorKind::Internal)?
            {
                Some((_, oci::Manifest::Image(img))) => img,
                // A selected child that is itself an image index, or that
                // vanished between select and fetch, is not a valid OCI
                // dependency shape — mirrors the "absent child digest" row
                // of the ADR Error Taxonomy.
                Some((_, oci::Manifest::ImageIndex(_))) | None => return Err(closure_fetch_miss(offline)),
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

/// The digest of an image manifest's config descriptor — the OCX metadata
/// blob. A descriptor whose `digest` string does not parse is a corrupt
/// manifest, not a missing one, so the structured `DigestError` is carried
/// (still classifies to `DataError`/65).
///
/// # Errors
///
/// Returns an error if the config descriptor's digest string is malformed.
pub fn config_blob_digest(image: &oci::ImageManifest) -> Result<oci::Digest, PackageErrorKind> {
    oci::Digest::try_from(image.config.digest.as_str()).map_err(|e| PackageErrorKind::Internal(crate::Error::from(e)))
}

/// Resolves a closure-frontier manifest miss to the correct error, matching
/// the ADR D3 Error Taxonomy: a policy block under `--offline` (no source
/// was allowed to be consulted), or a genuine not-found when a source could
/// have been (or was) consulted.
fn closure_fetch_miss(offline: bool) -> PackageErrorKind {
    if offline {
        PackageErrorKind::Internal(crate::Error::OfflineMode)
    } else {
        PackageErrorKind::NotFound
    }
}

/// Builds a node's declared dependency edges (as authored) from its
/// validated metadata — the wire-shape source for [`ClosureEdge`], shared by
/// every gather call site and by the root's own `dependencies` field.
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

/// Phase 2 — pure visibility fold (no I/O). Computes each gathered node's
/// effective visibility as seen from the root by folding
/// `Visibility::through_edge` down every path from the root and
/// `Visibility::merge`-ing at diamonds — the identical algorithm
/// [`crate::package::resolved_package::ResolvedPackage::with_dependencies`]
/// applies to an installed transitive closure, sourced here from `gathered`
/// metadata instead of `resolve.json`.
///
/// Returns the flat, deduped node list in topological order (deps before
/// dependents, root last) — `root_pinned`'s own node carries
/// `effective_visibility: None` and `is_root: true` (the composed-from-root
/// axis is undefined for the root itself).
fn fold_effective_visibility(
    root_pinned: &oci::PinnedIdentifier,
    root_metadata: &metadata::ValidMetadata,
    root_config_digest: oci::Digest,
    gathered: Vec<GatheredClosureNode>,
    resolved_identity: &HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
) -> Vec<ClosureNode> {
    let by_identity: HashMap<oci::PinnedIdentifier, GatheredClosureNode> = gathered
        .into_iter()
        .map(|entry| (entry.0.strip_advisory(), entry))
        .collect();

    // Bottom-up (post-order) DFS: each node's own `ResolvedPackage` needs its
    // direct children's already-computed `ResolvedPackage`s, exactly as the
    // install pipeline computes `resolve.json` while recursively pulling
    // deps (`pull.rs`). `order` collects the post-order visitation sequence
    // — deps before dependents, by construction.
    let mut resolved: HashMap<oci::PinnedIdentifier, ResolvedPackage> = HashMap::new();
    let mut order: Vec<oci::PinnedIdentifier> = Vec::new();
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

    // Root's own `ResolvedPackage`, built the same way, yields every
    // descendant's effective visibility as seen from the root — the
    // identical algorithm `ResolvedPackage::with_dependencies` applies at
    // install time, sourced here from gathered metadata instead of
    // `resolve.json`. Keyed by RESOLVED identity (Codex C1) — `with_dependencies`
    // dedups by its identifier argument, so a declared identity here would
    // fragment a single package into two entries whenever a dep is pinned to
    // an image index.
    let root_children: Vec<(oci::PinnedIdentifier, ResolvedPackage, metadata::visibility::Visibility)> = root_edges
        .iter()
        .map(|edge| {
            let resolved_key = resolved_edge_identity(resolved_identity, edge);
            let child_resolved = resolved.get(&resolved_key).cloned().unwrap_or_default();
            (resolved_key, child_resolved, edge.visibility)
        })
        .collect();
    let effective: HashMap<oci::PinnedIdentifier, metadata::visibility::Visibility> = ResolvedPackage::new()
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

    // Root last — the composed-from-root axis is undefined for the root
    // itself, so `effective_visibility` stays `None` (panel W3).
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

/// A node's own declared env vars as [`ClosureEnvVar`]s, each carrying its
/// visibility — UNFILTERED, so both surface projections can gate per-axis at
/// aggregate time. Declaration order is preserved. The metadata-only source
/// for the composer's two-env emission gate
/// (`var.visibility.has_interface()` / `has_private()`).
fn closure_env_vars(metadata: &metadata::ValidMetadata) -> Vec<ClosureEnvVar> {
    metadata
        .env()
        .into_iter()
        .flatten()
        .map(|var| ClosureEnvVar {
            key: var.key.clone(),
            // `ValidMetadata` (the parameter type) rejects every modifier type
            // this binary does not know, so no `Unknown` survives to here.
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

/// A node's own declared integration NAMESPACE keys, in `BTreeMap` order.
///
/// Keys only — the payload is deliberately absent for the same reason
/// [`closure_env_vars`] drops env values: a closure node is not installed, so
/// `${installPath}` has no value and an interpolated payload would be a
/// half-truth. Unfiltered, like `closure_env_vars`; the surface gate
/// (`composer::integrations_cross`) applies at aggregate time in
/// `inspect::project_surface`.
fn closure_integrations(metadata: &metadata::ValidMetadata) -> Vec<String> {
    metadata
        .integrations()
        .iter()
        .map(|(namespace, _)| namespace.to_owned())
        .collect()
}

/// Translates a declared [`ClosureEdge`]'s identity to the RESOLVED identity
/// [`gather_closure_nodes`] actually gathered a node under (Codex C1) — the
/// platform-selected child for an image-index-pinned dep, unchanged for a
/// flat dep. Every edge reachable from root has an alias entry by
/// construction (`gather_closure_nodes` populates one per fetch it completes,
/// and it completes a fetch for every edge the BFS discovers).
fn resolved_edge_identity(
    resolved_identity: &HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
    edge: &ClosureEdge,
) -> oci::PinnedIdentifier {
    resolved_identity
        .get(&edge.identifier.strip_advisory())
        .cloned()
        .expect("gather_closure_nodes populates the alias map for every edge reachable from root")
}

/// Post-order DFS over `key`'s declared edges, memoizing each visited node's
/// own [`ResolvedPackage`] (its transitive closure as seen from itself) into
/// `resolved` and recording deps-before-dependents visitation order in
/// `order`. A no-op once `key` is already memoized — every node is visited
/// at most once regardless of how many edges reach it (diamond dedup).
/// `key` and every memoization key here are RESOLVED identities (Codex C1) —
/// `resolved_identity` translates each child edge's declared identity before
/// recursing, so two different declared edges resolving to the same digest
/// (a direct edge and an image-index edge selecting it) memoize to the SAME
/// entry instead of computing the fold twice.
fn visit_closure_node(
    key: &oci::PinnedIdentifier,
    by_identity: &HashMap<oci::PinnedIdentifier, GatheredClosureNode>,
    resolved_identity: &HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
    resolved: &mut HashMap<oci::PinnedIdentifier, ResolvedPackage>,
    order: &mut Vec<oci::PinnedIdentifier>,
) {
    if resolved.contains_key(key) {
        return;
    }
    let (_, _, _, edges) = by_identity
        .get(key)
        .expect("gather_closure_nodes populates by_identity for every edge reachable from root");
    let children: Vec<(oci::PinnedIdentifier, ResolvedPackage, metadata::visibility::Visibility)> = edges
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
    use crate::file_structure::{FileStructure, PackageStore};
    use crate::oci;
    use crate::package::metadata;
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

        let id = oci::Identifier::new_registry("foo/bar", "example.com")
            .clone_with_digest(oci::Digest::Sha256(digest_byte.repeat(32)));
        let pinned = oci::PinnedIdentifier::try_from(id).unwrap();

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

    /// Plan F-2: a **deferred** `InstallInfo` is refused by the install-symlink
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

        let deferred = info.with_deferred(crate::package::install_info::DeferredComposition::new(
            crate::file_structure::ShimDir {
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
        manifests: Vec<(oci::Digest, Vec<u8>, oci::Manifest)>,
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
                .manifests
                .iter()
                .find(|(d, _, _)| *d == digest)
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
            manifests: vec![(
                manifest_digest.clone(),
                manifest_bytes.clone(),
                oci::Manifest::Image(oci::ImageManifest::default()),
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

    /// Everything the two published-dispatch staging tests below need to look
    /// at after [`stage_and_link_chain_blobs`] has run once.
    struct StagedPublishedChain {
        /// Held only for its `Drop` — the whole fixture lives under it.
        _tempdir: tempfile::TempDir,
        file_structure: crate::file_structure::FileStructure,
        registry: &'static str,
        dispatch_digest: oci::Digest,
        dispatch_bytes: Vec<u8>,
        manifest_digest: oci::Digest,
        manifest_bytes: Vec<u8>,
        config_digest: oci::Digest,
        config_bytes: Vec<u8>,
        raw_bytes_calls: std::sync::Arc<std::sync::Mutex<Vec<oci::Digest>>>,
        blob_calls: std::sync::Arc<std::sync::Mutex<Vec<oci::Digest>>>,
    }

    /// Resolves and stages a three-entry chain — `Index` / `Manifest` /
    /// `Config` — from a **published** (`index.ocx.sh`) source, i.e. one whose
    /// `physical_reference` resolves. The `Index` entry's bytes are a real OCI
    /// image index advertising the leaf manifest as its single child, and its
    /// digest is the hash of exactly those bytes, so the staging path's
    /// recompute-and-verify (`verify_requested_digest`) is exercised rather
    /// than bypassed.
    async fn stage_published_index_chain() -> StagedPublishedChain {
        use crate::file_structure::{FileStructure, IndexStore};
        use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
        use crate::package_manager::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};

        let tempdir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "ocx.sh";
        let repository = "ns/cmake";

        let manifest_bytes = br#"{"manifest":true}"#.to_vec();
        let manifest_digest = oci::Algorithm::Sha256.hash(&manifest_bytes);
        let config_bytes = br#"{"config":true}"#.to_vec();
        let config_digest = oci::Algorithm::Sha256.hash(&config_bytes);

        let dispatch = oci::Manifest::ImageIndex(oci::ImageIndex {
            schema_version: 2,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            artifact_type: None,
            manifests: vec![oci::ImageIndexEntry {
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
        let dispatch_digest = oci::Algorithm::Sha256.hash(&dispatch_bytes);

        let source = EndpointSpySource {
            namespace: registry.to_string(),
            published: true,
            manifests: vec![
                (dispatch_digest.clone(), dispatch_bytes.clone(), dispatch),
                (
                    manifest_digest.clone(),
                    manifest_bytes.clone(),
                    oci::Manifest::Image(oci::ImageManifest::default()),
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

        let pin = |digest: &oci::Digest| {
            oci::PinnedIdentifier::try_from(
                oci::Identifier::new_registry(repository, registry).clone_with_digest(digest.clone()),
            )
            .unwrap()
        };
        let chain_blob = |digest: &oci::Digest, role| ChainBlob {
            identifier: pin(digest),
            role,
            media_type: match role {
                // D1: the `Index`-role entry is an OCI image index, whatever the source.
                ChainRole::Index => "application/vnd.oci.image.index.v1+json",
                ChainRole::Manifest => "application/vnd.oci.image.manifest.v1+json",
                ChainRole::Config => crate::MEDIA_TYPE_PACKAGE_METADATA_V1,
            }
            .to_string(),
            size: 0,
        };
        let resolved = ResolvedChain {
            pinned: pin(&manifest_digest),
            transport_pinned: pin(&manifest_digest),
            chain: vec![
                chain_blob(&dispatch_digest, ChainRole::Index),
                chain_blob(&manifest_digest, ChainRole::Manifest),
                chain_blob(&config_digest, ChainRole::Config),
            ],
            final_manifest: oci::ImageManifest::default(),
            platform: oci::Platform::any(),
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
    /// store, parses each candidate as an `oci::Manifest`, and resolves every
    /// advertised child under the registry root **three levels above the
    /// index's own shard directory**. This asserts that whole read path lands
    /// on the leaf manifest's real blob directory — the retention edge's
    /// content. Before the dispatch entry was staged there was no blob to
    /// enumerate, so an index-resolved package's index had no retention edge
    /// at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn staged_published_dispatch_object_yields_a_child_leaf_retention_edge() {
        use crate::file_structure::cas_shard_path;

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
        let oci::Manifest::ImageIndex(index) = serde_json::from_slice::<oci::Manifest>(&bytes).unwrap() else {
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

        let child_digest = oci::Digest::try_from(index.manifests[0].digest.as_str()).unwrap();
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
        use crate::file_structure::{FileStructure, IndexStore};
        use crate::oci::index::{ChainMode, Index, LocalConfig, LocalIndex};
        use crate::package_manager::error::PackageErrorKind;
        use crate::package_manager::tasks::resolve::{ChainBlob, ChainRole, ResolvedChain};

        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let registry = "ocx.sh";
        let repository = "ns/cmake";

        // The chain entry requests the digest of these real image-index bytes …
        let dispatch = oci::Manifest::ImageIndex(oci::ImageIndex {
            schema_version: 2,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            artifact_type: None,
            manifests: Vec::new(),
            annotations: None,
        });
        let dispatch_bytes = serde_json::to_vec(&dispatch).unwrap();
        let dispatch_digest = oci::Algorithm::Sha256.hash(&dispatch_bytes);
        // … but the source serves different bytes under exactly that digest.
        let served_bytes = b"not the index bytes".to_vec();
        assert_ne!(oci::Algorithm::Sha256.hash(&served_bytes), dispatch_digest);

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

        let pinned = oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(repository, registry).clone_with_digest(dispatch_digest.clone()),
        )
        .unwrap();
        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: pinned.clone(),
            chain: vec![ChainBlob {
                identifier: pinned,
                role: ChainRole::Index,
                media_type: "application/vnd.oci.image.index.v1+json".to_string(),
                size: 0,
            }],
            final_manifest: oci::ImageManifest::default(),
            platform: oci::Platform::any(),
        };

        let err = super::stage_chain_blobs(&fs, &index, &resolved)
            .await
            .expect_err("index bytes that don't hash to the requested digest must be rejected");

        assert!(
            matches!(
                err,
                PackageErrorKind::Internal(crate::Error::FileStructure(
                    crate::file_structure::error::Error::DigestMismatch { .. }
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
