// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    file_structure, log, oci,
    package::{install_info::InstallInfo, install_status::InstallStatus, metadata, resolved_package::ResolvedPackage},
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    prelude::SerdeExt,
    utility::singleflight,
};

use super::super::PackageManager;

/// Singleflight group capacity — maximum number of unique
/// [`PinnedIdentifier`](oci::PinnedIdentifier)s (root packages + transitive
/// dependencies) tracked across all packages in a single `pull_all` call.
const MAX_NODES: usize = 1024;

/// How long a waiter blocks on the singleflight watch channel for a leader
/// to complete. This is **not** the OCI download timeout — the OCI client
/// has separate `read_timeout` / `connect_timeout` fields (both default to
/// `None`). This guards against a leader that hangs indefinitely.
const SETUP_TIMEOUT: Duration = Duration::from_mins(10);

/// Singleflight group keyed by [`PinnedIdentifier`](oci::PinnedIdentifier)
/// (advisory tag stripped) for in-process dedup of concurrent dependency setups.
type SetupGroup = singleflight::Group<oci::PinnedIdentifier, InstallInfo>;

/// Maps singleflight errors into the package manager error model.
fn map_singleflight_error(e: singleflight::Error) -> PackageErrorKind {
    let dep_err: crate::package_manager::DependencyError = e.into();
    PackageErrorKind::Internal(dep_err.into())
}

impl PackageManager {
    /// Downloads a package and its transitive dependencies to the object store.
    ///
    /// Creates dependency back-references so GC can track the relationship.
    /// Does NOT create install symlinks (candidate/current) — that is the
    /// responsibility of [`PackageManager::install`].
    ///
    /// # Idempotency and concurrent safety
    ///
    /// Multiple pulls of the same package — within one process or across
    /// processes — are safe and idempotent. Three defense layers prevent
    /// redundant downloads:
    ///
    /// 1. **Singleflight dedup** — in-process dedup via a shared
    ///    [`singleflight::Group`]. The first task to claim a dependency
    ///    gets a [`singleflight::Handle`]; subsequent tasks block until
    ///    the result is broadcast.
    ///
    /// 2. **[`PackageManager::find_plain`]** — checks the object store before
    ///    acquiring any lock. If a concurrent process already placed the
    ///    package in the store, returns immediately without downloading.
    ///
    /// 3. **File lock + post-lock recheck** — acquires an exclusive lock on a
    ///    deterministic temp directory via
    ///    [`TempStore::try_acquire`](crate::file_structure::TempStore::try_acquire).
    ///    After the lock is acquired, the object store is re-checked so a
    ///    process that waited for the lock skips the download if the first
    ///    process already wrote the package. Manifest and metadata fetches
    ///    only happen after this gate, avoiding redundant network calls.
    pub fn pull(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
    ) -> Pin<Box<dyn Future<Output = Result<InstallInfo, PackageErrorKind>> + Send + '_>> {
        let group = SetupGroup::new(MAX_NODES, SETUP_TIMEOUT);
        setup_with_tracker(self, package, platforms, group)
    }

    /// Pulls multiple packages in parallel with a shared singleflight group
    /// for cross-package diamond dependency dedup.
    pub async fn pull_all(
        &self,
        packages: &[oci::Identifier],
        platforms: Vec<oci::Platform>,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let info = self
                .pull(&packages[0], platforms)
                .instrument(crate::cli::progress::spinner_span(
                    info_span!("Pulling", package = %packages[0]),
                    &packages[0],
                ))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::InstallFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![info]);
        }

        let shared_group = SetupGroup::new(MAX_NODES, SETUP_TIMEOUT);
        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let group = shared_group.clone();
            let span = crate::cli::progress::spinner_span(info_span!("Pulling", package = %package), &package);
            tasks.spawn(
                async move {
                    let result = setup_with_tracker(&mgr, &package, platforms, group).await;
                    (package, result)
                }
                .instrument(span),
            );
        }

        super::common::drain_package_tasks(packages, tasks, package_manager::error::Error::InstallFailed).await
    }
}

/// Setup with an externally provided singleflight group, allowing shared
/// dedup state across parallel pulls.
fn setup_with_tracker<'a>(
    mgr: &'a PackageManager,
    package: &oci::Identifier,
    platforms: Vec<oci::Platform>,
    group: SetupGroup,
) -> Pin<Box<dyn Future<Output = Result<InstallInfo, PackageErrorKind>> + Send + 'a>> {
    let package = package.clone();
    Box::pin(async move { setup_impl(mgr, &package, platforms, group).await })
}

/// Inner implementation of [`PackageManager::pull`] — see that method for
/// concurrency safety documentation.
async fn setup_impl(
    mgr: &PackageManager,
    package: &oci::Identifier,
    platforms: Vec<oci::Platform>,
    group: SetupGroup,
) -> Result<InstallInfo, PackageErrorKind> {
    log::debug!("Pulling package: {}", package);

    // Step 1: Resolve via the index (Image Index → platform manifest).
    let pinned = mgr.resolve(package, platforms.clone()).await?;
    log::debug!("Resolved package identifier: {}", &pinned);

    // Step 2: In-process dedup — check if another task is already setting
    // up this exact dependency, or has already completed it.
    let handle = match group
        .try_acquire(pinned.strip_advisory())
        .await
        .map_err(map_singleflight_error)?
    {
        singleflight::Acquisition::Resolved(info) => {
            log::debug!("Package '{}' already set up by another task, reusing.", &pinned);
            return Ok(info);
        }
        singleflight::Acquisition::Leader(handle) => handle,
    };

    // From here on, we own the handle. On success, broadcast the result
    // to waiters. On error, broadcast the error message so waiters get a
    // meaningful diagnostic instead of a generic "abandoned".
    // If we panic, Drop sends Abandoned as a fallback.
    match setup_owned(mgr, &pinned, platforms, group).await {
        Ok(info) => {
            handle.complete(info.clone());
            Ok(info)
        }
        Err(e) => {
            let shared = handle.fail(e);
            Err(map_singleflight_error(singleflight::Error::Failed(shared)))
        }
    }
}

/// Performs the actual download, dependency resolution, and store placement.
/// Called only by the task that owns the singleflight handle.
async fn setup_owned(
    mgr: &PackageManager,
    pinned: &oci::PinnedIdentifier,
    platforms: Vec<oci::Platform>,
    group: SetupGroup,
) -> Result<InstallInfo, PackageErrorKind> {
    // Defense layer 2 — skip if already fully installed (cross-process).
    if let Some(info) = mgr.find_plain(pinned).await? {
        let install_path = mgr.file_structure().objects.install_status(pinned);
        if tokio::fs::try_exists(&install_path).await.unwrap_or(false) {
            log::debug!("Package '{}' already fully installed, skipping.", pinned);
            return Ok(info);
        }
        // Content exists but sentinel missing — crash recovery, re-pull.
        log::debug!(
            "Package '{}' present in object store but not stamped, re-pulling.",
            pinned
        );
    }

    // Defense layer 3: Acquire exclusive temp directory (file lock).
    let temp = acquire_temp_dir(
        mgr.file_structure(),
        mgr.client().map_err(PackageErrorKind::Internal)?,
        pinned,
    )
    .await?;

    // Post-lock recheck — if we waited for another process to release the
    // lock, it may have already installed the package.
    if let Some(info) = mgr.find_plain(pinned).await? {
        let install_path = mgr.file_structure().objects.install_status(pinned);
        if tokio::fs::try_exists(&install_path).await.unwrap_or(false) {
            log::debug!(
                "Package '{}' installed by another process while waiting for lock, skipping.",
                pinned
            );
            return Ok(info);
        }
    }

    // Pull manifest + metadata (both fast, ~1KB each).
    let client = mgr.client().map_err(PackageErrorKind::Internal)?;
    let manifest = client.pull_manifest(pinned).await?;
    let metadata = client.pull_metadata(pinned, Some(&manifest)).await?;

    // Pull content and dependencies in parallel.
    let (download, dependencies) = tokio::join!(
        client.pull_content(pinned, Some(&manifest), &metadata, &temp.dir.dir),
        setup_dependencies(mgr, &metadata, pinned, platforms, group),
    );
    let (_, dependencies) = (download?, dependencies?);

    // Build resolved package and enrich temp dir.
    // Order invariant: setup_dependencies returns results in declaration order.
    let resolved = ResolvedPackage::new(pinned.clone()).with_dependencies(
        metadata
            .dependencies()
            .iter()
            .zip(dependencies.iter())
            .map(|(decl, info)| (info.resolved.clone(), decl.visibility)),
    );
    post_download_actions(&temp.dir.dir, &resolved).await?;

    // Create deps/ symlinks in temp dir BEFORE move — targets are absolute
    // paths to dependency content already in the object store. This ensures
    // the move is atomic: no window where the object exists without deps/.
    // NOTE: issue #23 (relative symlinks) will need to revisit this approach.
    link_dependencies_in_temp(&temp.dir.dir, &dependencies)?;

    // Atomic move temp → object store.
    let install_info = move_temp_to_object_store(mgr.file_structure(), pinned, &metadata, resolved, temp).await?;

    log::debug!("Pull succeeded for '{}'.", pinned);
    Ok(install_info)
}

/// Acquires an exclusive temp directory for the given identifier.
async fn acquire_temp_dir(
    fs: &file_structure::FileStructure,
    client: &oci::Client,
    identifier: &oci::PinnedIdentifier,
) -> Result<crate::file_structure::TempAcquireResult, PackageErrorKind> {
    let temp_path = fs.temp.path(identifier).map_err(PackageErrorKind::Internal)?;

    let acquire = match fs.temp.try_acquire(&temp_path).map_err(PackageErrorKind::Internal)? {
        Some(r) => r,
        None => {
            log::debug!("Temp dir locked by another process, waiting: {}", temp_path.display());
            fs.temp
                .acquire_with_timeout(&temp_path, client.lock_timeout())
                .await
                .map_err(PackageErrorKind::Internal)?
        }
    };
    if acquire.was_cleaned {
        log::debug!("Cleaned previous temp data at {}", temp_path.display());
    }
    Ok(acquire)
}

/// Sets up dependencies in parallel, returning results in declaration order.
///
/// Each dependency is dispatched through [`setup_with_tracker`], which calls
/// the singleflight group internally. Diamond dependencies are deduplicated
/// automatically — the first task to claim a dependency does the work, and
/// all others block on the watch channel.
async fn setup_dependencies(
    mgr: &PackageManager,
    metadata: &crate::package::metadata::Metadata,
    parent: &oci::PinnedIdentifier,
    platforms: Vec<oci::Platform>,
    group: SetupGroup,
) -> Result<Vec<InstallInfo>, PackageErrorKind> {
    let deps = metadata.dependencies();
    if deps.is_empty() {
        return Ok(Vec::new());
    }

    log::debug!(
        "Package '{}' has {} dependencies, pulling transitively.",
        parent,
        deps.len(),
    );

    let mut tasks = JoinSet::new();

    for (idx, dep) in deps.iter().enumerate() {
        let mgr = mgr.clone();
        let dep_id = dep.identifier.clone();
        let platforms = platforms.clone();
        let group = group.clone();
        tasks.spawn(async move {
            let info = setup_with_tracker(&mgr, &dep_id, platforms, group).await?;
            Ok::<_, PackageErrorKind>((idx, info))
        });
    }

    let mut results: Vec<Option<InstallInfo>> = vec![None; deps.len()];
    while let Some(join_result) = tasks.join_next().await {
        let (idx, info) = join_result.expect("dependency setup task panicked")?;
        results[idx] = Some(info);
    }

    Ok(results.into_iter().flatten().collect())
}

/// Post processing after download of the package content and all dependencies are fully set up.
///
/// - Writes the `resolve.json` metadata file with the resolved dependencies for this package.
/// - Writes the `install.json` sentinel file to indicate the package is fully installed.
async fn post_download_actions(
    temp_path: &std::path::Path,
    resolved: &ResolvedPackage,
) -> Result<(), PackageErrorKind> {
    resolved
        .write_json(temp_path.join("resolve.json"))
        .await
        .map_err(PackageErrorKind::Internal)?;

    InstallStatus::new()
        .ok()
        .write_json(temp_path.join("install.json"))
        .await
        .map_err(PackageErrorKind::Internal)?;

    Ok(())
}

/// Atomically moves the enriched temp directory to the object store.
async fn move_temp_to_object_store(
    fs: &file_structure::FileStructure,
    identifier: &oci::PinnedIdentifier,
    metadata: &metadata::Metadata,
    resolved: ResolvedPackage,
    temp: crate::file_structure::TempAcquireResult,
) -> Result<InstallInfo, PackageErrorKind> {
    let output_path = fs.objects.path(identifier);
    let temp_path = temp.dir.dir.clone();
    let content = fs.objects.content(identifier);

    crate::utility::fs::move_dir(&temp_path, &output_path)
        .await
        .map_err(PackageErrorKind::Internal)?;

    drop(temp);

    Ok(InstallInfo {
        identifier: identifier.clone(),
        metadata: metadata.clone(),
        resolved,
        content,
    })
}

/// Creates dependency forward-refs (`deps/` symlinks) inside the temp directory.
///
/// Symlink targets are absolute paths to dependency content directories already
/// present in the object store (deps are pulled before the dependent). After
/// `move_dir` the symlinks remain valid because their targets are not inside the
/// temp directory being moved.
///
/// ALL deps get symlinks regardless of visibility — visibility only controls
/// env composition, not GC or filesystem presence.
fn link_dependencies_in_temp(temp_dir: &std::path::Path, dep_infos: &[InstallInfo]) -> Result<(), PackageErrorKind> {
    if dep_infos.is_empty() {
        return Ok(());
    }
    let deps_dir = temp_dir.join("deps");
    std::fs::create_dir_all(&deps_dir)
        .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(deps_dir.clone(), e)))?;
    for info in dep_infos {
        let name = crate::reference_manager::ReferenceManager::ref_name(&info.content);
        let link_path = deps_dir.join(name);
        crate::symlink::create(&info.content, &link_path).map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}
