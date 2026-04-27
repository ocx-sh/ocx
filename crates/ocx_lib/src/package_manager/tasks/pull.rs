// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    MEDIA_TYPE_PACKAGE_V1, file_structure, log, media_type_select_some, oci,
    package::{install_info::InstallInfo, install_status::InstallStatus, metadata, resolved_package::ResolvedPackage},
    package_manager::{
        self,
        error::{PackageError, PackageErrorKind},
        visible::{collect_entrypoints, import_visible_packages},
    },
    prelude::SerdeExt,
    utility::{self, singleflight},
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
const PACKAGE_SETUP_TIMEOUT: Duration = Duration::from_mins(10);

/// Layer extraction can take substantially longer than package setup because
/// a single layer may be several hundred MB. 30 minutes is generous enough
/// to accommodate slow networks and large payloads while still bounding a
/// hung leader.
const LAYER_SETUP_TIMEOUT: Duration = Duration::from_mins(30);

/// Singleflight group keyed by [`PinnedIdentifier`](oci::PinnedIdentifier)
/// (advisory tag stripped) for in-process dedup of concurrent dependency setups.
type SetupGroup = singleflight::Group<oci::PinnedIdentifier, InstallInfo>;

/// Singleflight group for in-process dedup of concurrent layer extractions.
/// Two packages that share a layer run the extraction exactly once.
/// Key: `(registry, layer_digest)`. Value: `()` — the observable artifact
/// is the on-disk presence of `layers/{digest}/content/`.
type LayerGroup = singleflight::Group<(String, oci::Digest), ()>;

/// Bundle of singleflight groups threaded through the pull pipeline so
/// package-level and layer-level dedup share a session lifetime.
#[derive(Clone)]
struct SetupGroups {
    packages: SetupGroup,
    layers: LayerGroup,
}

impl SetupGroups {
    fn new() -> Self {
        Self {
            packages: SetupGroup::new(MAX_NODES, PACKAGE_SETUP_TIMEOUT),
            layers: LayerGroup::new(MAX_NODES, LAYER_SETUP_TIMEOUT),
        }
    }
}

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
        let groups = SetupGroups::new();
        setup_with_tracker(self, package, platforms, groups)
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

        let shared_groups = SetupGroups::new();
        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let groups = shared_groups.clone();
            let span = crate::cli::progress::spinner_span(info_span!("Pulling", package = %package), &package);
            tasks.spawn(
                async move {
                    let result = setup_with_tracker(&mgr, &package, platforms, groups).await;
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
    groups: SetupGroups,
) -> Pin<Box<dyn Future<Output = Result<InstallInfo, PackageErrorKind>> + Send + 'a>> {
    let package = package.clone();
    Box::pin(async move { setup_impl(mgr, &package, platforms, groups).await })
}

/// Inner implementation of [`PackageManager::pull`] — see that method for
/// concurrency safety documentation.
async fn setup_impl(
    mgr: &PackageManager,
    package: &oci::Identifier,
    platforms: Vec<oci::Platform>,
    groups: SetupGroups,
) -> Result<InstallInfo, PackageErrorKind> {
    log::debug!("Pulling package: {}", package);

    // Step 1: Resolve via the index (Image Index → platform manifest).
    // The returned `ResolvedChain` carries the full `(registry, digest)` walk
    // (top-level index + platform child where applicable) and the leaf
    // `final_manifest` — the pull pipeline no longer re-fetches the manifest
    // from the client, and the chain is linked into `refs/blobs/` in one shot.
    let resolved = mgr.resolve(package, platforms.clone()).await?;
    let pinned = resolved.pinned.clone();
    log::debug!("Resolved package identifier: {}", &pinned);

    // Step 2: In-process dedup — check if another task is already setting
    // up this exact dependency, or has already completed it.
    let handle = match groups
        .packages
        .try_acquire(pinned.strip_advisory())
        .await
        .map_err(map_singleflight_error)?
    {
        singleflight::Acquisition::Resolved(info) => {
            log::debug!("Package '{}' already set up by another task, reusing.", &pinned);
            // Singleflight key is the final pinned digest, so two concurrent
            // installs of different alias tags (e.g. `cmake:latest` and
            // `cmake:3.28`) that land on the same leaf manifest share a
            // leader — but their resolution chains may differ (different
            // top-level image indexes). The leader linked only its chain,
            // so top up the waiter's chain here. `link_blobs` is idempotent,
            // so overlapping entries are no-ops; only the unique entries
            // (e.g., the waiter's distinct image-index digest) get linked.
            super::common::reference_manager(mgr.file_structure())
                .link_blobs(&info.content, &resolved.chain)
                .await
                .map_err(PackageErrorKind::Internal)?;
            return Ok(info);
        }
        singleflight::Acquisition::Leader(handle) => handle,
    };

    // From here on, we own the handle. On success, broadcast the result
    // to waiters. On error, broadcast the error message so waiters get a
    // meaningful diagnostic instead of a generic "abandoned".
    // If we panic, Drop sends Abandoned as a fallback.
    match setup_owned(mgr, &pinned, resolved, platforms, groups).await {
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
    resolved: super::resolve::ResolvedChain,
    platforms: Vec<oci::Platform>,
    groups: SetupGroups,
) -> Result<InstallInfo, PackageErrorKind> {
    // Defense layer 2 — skip if already fully installed (cross-process).
    if let Some(info) = mgr.find_plain(pinned).await? {
        let install_path = mgr.file_structure().packages.install_status(pinned);
        if utility::fs::path_exists_lossy(&install_path).await {
            log::debug!("Package '{}' already fully installed, skipping.", pinned);
            // Top up chain refs in case the already-installed package was
            // resolved via a different image-index path (alias tag). See
            // the waiter branch in `setup_impl` for the same invariant.
            super::common::reference_manager(mgr.file_structure())
                .link_blobs(&info.content, &resolved.chain)
                .await
                .map_err(PackageErrorKind::Internal)?;
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
        let install_path = mgr.file_structure().packages.install_status(pinned);
        if utility::fs::path_exists_lossy(&install_path).await {
            log::debug!(
                "Package '{}' installed by another process while waiting for lock, skipping.",
                pinned
            );
            super::common::reference_manager(mgr.file_structure())
                .link_blobs(&info.content, &resolved.chain)
                .await
                .map_err(PackageErrorKind::Internal)?;
            return Ok(info);
        }
    }

    // Manifest comes from the resolver — ChainedIndex already persisted it to
    // `blobs/` via write-through during resolve, so no extra fetch is needed.
    let client = mgr.client().map_err(PackageErrorKind::Internal)?;
    let manifest = resolved.final_manifest.clone();
    let metadata = client.pull_metadata(pinned, Some(&manifest)).await?;
    // Reject malformed metadata at the ingress boundary — refuse to write
    // unvalidated metadata to disk so the consumption-side `load_object_data`
    // validation never has to deal with publisher-side bugs after the fact.
    let metadata: metadata::Metadata = metadata::ValidMetadata::try_from(metadata)
        .map_err(PackageErrorKind::Internal)?
        .into();

    // Validate manifest before any extraction work. Zero layers is valid —
    // the package is a config-only artifact whose `content/` is the empty
    // directory and whose metadata is the only carried payload.
    media_type_select_some(&manifest.artifact_type, &[MEDIA_TYPE_PACKAGE_V1])
        .map_err(|e| PackageErrorKind::from(oci::client::error::ClientError::internal(e)))?;

    // Wrap the temp directory in a PackageDir so all sibling-file accesses
    // use the canonical accessors instead of hardcoded strings.
    let pkg = file_structure::PackageDir {
        dir: temp.dir.dir.clone(),
    };

    // Store manifest in temp dir for audit trail — gets moved with the package.
    manifest
        .write_json(pkg.manifest())
        .await
        .map_err(PackageErrorKind::Internal)?;

    let fs = mgr.file_structure();

    // Extract layers to layers/ store and pull dependencies in parallel.
    let (layer_digests, dependencies) = tokio::join!(
        extract_layers(mgr, pinned, &manifest, &metadata, groups.layers.clone()),
        setup_dependencies(mgr, &metadata, pinned, platforms, groups.clone()),
    );
    let (layer_digests, dependencies) = (layer_digests?, dependencies?);

    // Write metadata.json to package temp dir.
    metadata
        .write_json(pkg.metadata())
        .await
        .map_err(PackageErrorKind::Internal)?;

    // GC race-closure: create refs/layers/ forward-refs BEFORE the walker
    // runs. Once any file in the package content/ shares an inode with a
    // layer file, the layer MUST already be protected by a forward-ref so
    // a concurrent `ocx clean` cannot sweep it mid-assembly.
    let layers_dir = pkg.refs_layers_dir();
    tokio::fs::create_dir_all(&layers_dir)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(layers_dir.clone(), e)))?;
    link_layers_in_temp(&pkg, pinned.registry(), &layer_digests, fs)?;

    // Assemble package content/ by hardlinking files from all layers.
    // The walker mirrors each layer's directory tree into a single content/
    // directory — regular files become hardlinks; intra-layer symlinks are
    // preserved verbatim. Layers must not overlap (same file in two layers
    // is an error).
    let layer_contents: Vec<std::path::PathBuf> = layer_digests
        .iter()
        .map(|d| fs.layers.content(pinned.registry(), d))
        .collect();
    let sources: Vec<&std::path::Path> = layer_contents.iter().map(AsRef::as_ref).collect();
    crate::utility::fs::assemble_from_layers(&sources, &pkg.content())
        .await
        .map_err(PackageErrorKind::Internal)?;

    // Entry point launcher generation.
    // Launchers are written into pkg.entrypoints() — the temp dir's entrypoints/ sibling —
    // BEFORE post_download_actions so they are carried by the existing atomic move.
    // The launcher BAKES THE FINAL packages/<digest>/ package-root path (resolved via
    // `fs.packages.path(pinned)`), NOT the temp staging path: the launcher file
    // moves atomically with the package, but the path it bakes must reference the
    // post-move location to remain valid after the move.
    if let Some(entrypoints) = metadata.bundle_entrypoints()
        && !entrypoints.is_empty()
    {
        use crate::package::metadata::dependency::DependencyName;
        use crate::package::metadata::env::accumulator::DependencyContext;
        use std::sync::Arc;
        let dep_contexts: std::collections::HashMap<DependencyName, DependencyContext> = metadata
            .dependencies()
            .iter()
            .zip(dependencies.iter())
            .map(|(decl, info)| {
                let name = decl.name();
                let ctx = DependencyContext::full(Arc::new(info.clone()));
                (name, ctx)
            })
            .collect();
        let dest = pkg.entrypoints();
        // Bake the post-move package-root path into the launcher so it remains
        // valid after the temp→final atomic rename.
        let final_pkg_root = fs.packages.path(pinned);
        crate::package_manager::entrypoints::generate(&final_pkg_root, entrypoints, &dep_contexts, &dest)
            .await
            .map_err(PackageErrorKind::Internal)?;
    }

    // Build resolved package and enrich temp dir.
    // Order invariant: setup_dependencies returns results in declaration order.
    let resolved_package = ResolvedPackage::new().with_dependencies(
        metadata
            .dependencies()
            .iter()
            .zip(dependencies.iter())
            .map(|(decl, info)| (info.identifier.clone(), info.resolved.clone(), decl.visibility)),
    );

    // Stage 1 entrypoint collision check — runs against the full transitive
    // closure, before `resolve.json` is persisted. Catches intra-closure
    // duplicate launcher names at install time so the bad state never reaches
    // disk. `import_visible_packages` loads each transitive dep's on-disk
    // metadata (already installed by `setup_dependencies`) and applies the
    // same visibility filter as Phase A of the env pipeline, so the collision
    // check covers the complete reachable set — not just direct deps.
    {
        use std::sync::Arc;

        let root_info = InstallInfo {
            identifier: pinned.clone(),
            metadata: metadata.clone(),
            resolved: resolved_package.clone(),
            content: pkg.content(),
        };
        let visible_for_check = import_visible_packages(&fs.packages, &[Arc::new(root_info)])
            .await
            .map_err(PackageErrorKind::Internal)?;
        collect_entrypoints(&visible_for_check)?;
    }

    post_download_actions(&pkg, pinned, &resolved_package).await?;

    // Create remaining forward-ref symlinks in temp dir BEFORE move — targets
    // are absolute paths already in their respective stores. This ensures the
    // move is atomic: no window where the package exists without refs/.
    // NOTE: issue #23 (relative symlinks) will need to revisit this approach.
    if !dependencies.is_empty() {
        let deps_dir = pkg.refs_deps_dir();
        tokio::fs::create_dir_all(&deps_dir)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(deps_dir.clone(), e)))?;
    }
    link_dependencies_in_temp(&pkg, &dependencies)?;
    // Forward-ref every blob the resolver touched into `refs/blobs/` so GC
    // can reach the full resolution chain from the installed package.
    super::common::reference_manager(fs)
        .link_blobs(&pkg.content(), &resolved.chain)
        .await
        .map_err(PackageErrorKind::Internal)?;

    // Atomic move temp → object store.
    let install_info =
        move_temp_to_object_store(mgr.file_structure(), pinned, &metadata, resolved_package, temp).await?;

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
    groups: SetupGroups,
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
        let groups = groups.clone();
        tasks.spawn(async move {
            let info = setup_with_tracker(&mgr, &dep_id, platforms, groups).await?;
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
/// - Writes the `digest` file for recovery of the full digest from the truncated CAS path.
async fn post_download_actions(
    pkg: &file_structure::PackageDir,
    pinned: &oci::PinnedIdentifier,
    resolved: &ResolvedPackage,
) -> Result<(), PackageErrorKind> {
    resolved
        .write_json(pkg.resolve())
        .await
        .map_err(PackageErrorKind::Internal)?;

    InstallStatus::new()
        .ok()
        .write_json(pkg.install_status())
        .await
        .map_err(PackageErrorKind::Internal)?;

    file_structure::write_digest_file(&pkg.digest_file(), &pinned.digest())
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
    let output_path = fs.packages.path(identifier);
    let temp_path = temp.dir.dir.clone();
    let content = fs.packages.content(identifier);

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

/// Extracts all layers referenced by a manifest in parallel, returning their
/// parsed digests in manifest declaration order.
///
/// Each layer is dispatched through [`extract_layer_atomic`], which provides
/// the same atomicity guarantees as package install — in-process
/// singleflight dedup, find-plain gate, exclusive temp lock, post-lock
/// recheck, and atomic move into `layers/{digest}/`.
async fn extract_layers(
    mgr: &PackageManager,
    pinned: &oci::PinnedIdentifier,
    manifest: &oci::ImageManifest,
    metadata: &metadata::Metadata,
    layer_group: LayerGroup,
) -> Result<Vec<oci::Digest>, PackageErrorKind> {
    // Parse all layer digests up front so we can return a typed error
    // before spawning any work.
    let mut parsed = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let digest: oci::Digest = layer
            .digest
            .clone()
            .try_into()
            .map_err(|e: oci::digest::error::DigestError| PackageErrorKind::Internal(e.into()))?;
        parsed.push((layer.clone(), digest));
    }

    // Dispatch extractions in parallel. JoinSet results come back in
    // completion order, so we tag each task with its index and reorder at
    // the end to preserve manifest declaration order.
    let mut tasks: JoinSet<(usize, Result<oci::Digest, PackageErrorKind>)> = JoinSet::new();
    for (idx, (layer, digest)) in parsed.into_iter().enumerate() {
        let mgr = mgr.clone();
        let pinned = pinned.clone();
        let metadata = metadata.clone();
        let layer_group = layer_group.clone();
        tasks.spawn(async move {
            let res = extract_layer_atomic(&mgr, &pinned, &layer, &digest, &metadata, layer_group).await;
            (idx, res)
        });
    }

    let mut results: Vec<Option<oci::Digest>> = (0..tasks.len()).map(|_| None).collect();
    while let Some(join_res) = tasks.join_next().await {
        let (idx, task_res) = join_res.expect("layer extraction task panicked");
        results[idx] = Some(task_res?);
    }
    Ok(results.into_iter().flatten().collect())
}

/// Atomically extracts a single layer into `layers/{digest}/`, dedup-ing
/// in-process concurrent extractions via the layer singleflight group.
///
/// Flow:
/// 1. Singleflight acquire — `Resolved` means another task already
///    finished; `Leader` means we do the work and broadcast on completion.
/// 2. Find-plain — skip if `layers/{digest}/content/` already exists.
/// 3. Acquire exclusive temp dir via `TempStore::layer_path`.
/// 4. Post-lock recheck — another process may have finished while we waited.
/// 5. Pull the layer blob into the temp dir.
/// 6. Write the `digest` file for CAS-path recovery.
/// 7. Atomic rename temp → `layers/{digest}/`. A late race at rename time
///    (another process finishing between step 4 and step 7) is still
///    tolerated as a no-op cleanup.
/// 8. Broadcast completion via the singleflight handle.
async fn extract_layer_atomic(
    mgr: &PackageManager,
    pinned: &oci::PinnedIdentifier,
    layer: &oci::Descriptor,
    layer_digest: &oci::Digest,
    metadata: &metadata::Metadata,
    layer_group: LayerGroup,
) -> Result<oci::Digest, PackageErrorKind> {
    let fs = mgr.file_structure();
    let client = mgr.client().map_err(PackageErrorKind::Internal)?;
    let registry = pinned.registry().to_string();

    // Step 1: singleflight gate.
    let key = (registry.clone(), layer_digest.clone());
    let handle = match layer_group.try_acquire(key).await.map_err(map_singleflight_error)? {
        singleflight::Acquisition::Resolved(()) => {
            log::debug!("Layer {} already extracted by another task, reusing.", layer_digest);
            return Ok(layer_digest.clone());
        }
        singleflight::Acquisition::Leader(handle) => handle,
    };

    // We own the handle. Either complete on Ok or fail on Err before return.
    match extract_layer_inner(pinned, layer, layer_digest, metadata, client, fs).await {
        Ok(()) => {
            handle.complete(());
            Ok(layer_digest.clone())
        }
        Err(e) => {
            let shared = handle.fail(e);
            Err(map_singleflight_error(singleflight::Error::Failed(shared)))
        }
    }
}

/// Inner extraction implementation — runs only for the leader task.
async fn extract_layer_inner(
    pinned: &oci::PinnedIdentifier,
    layer: &oci::Descriptor,
    layer_digest: &oci::Digest,
    metadata: &metadata::Metadata,
    client: &oci::Client,
    fs: &file_structure::FileStructure,
) -> Result<(), PackageErrorKind> {
    // Step 2: find-plain — skip if already extracted on disk.
    let layer_content = fs.layers.content(pinned.registry(), layer_digest);
    if utility::fs::path_exists_lossy(&layer_content).await {
        log::debug!("Layer {} already present on disk, skipping.", layer_digest);
        return Ok(());
    }

    // Step 3: acquire exclusive temp dir at the layer-keyed path.
    let temp_path = fs.temp.layer_path(pinned.registry(), layer_digest);
    let temp = match fs.temp.try_acquire(&temp_path).map_err(PackageErrorKind::Internal)? {
        Some(r) => r,
        None => {
            log::debug!(
                "Layer temp dir locked by another process, waiting: {}",
                temp_path.display()
            );
            fs.temp
                .acquire_with_timeout(&temp_path, client.lock_timeout())
                .await
                .map_err(PackageErrorKind::Internal)?
        }
    };

    // Step 4: post-lock recheck.
    if utility::fs::path_exists_lossy(&layer_content).await {
        log::debug!(
            "Layer {} installed by another process while waiting for lock, skipping.",
            layer_digest
        );
        return Ok(());
    }

    // Step 5: pull the layer blob.
    client.pull_layer(pinned, layer, metadata, &temp.dir.dir).await?;

    // Step 6: write digest file for CAS-path recovery.
    file_structure::write_digest_file(&temp.dir.dir.join(file_structure::DIGEST_FILENAME), layer_digest)
        .await
        .map_err(PackageErrorKind::Internal)?;

    // Step 7: atomic rename temp → layers/{digest}/.
    let layer_path = fs.layers.path(pinned.registry(), layer_digest);
    if let Some(parent) = layer_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(parent.to_path_buf(), e)))?;
    }
    let temp_path_for_rename = temp.dir.dir.clone();
    match tokio::fs::rename(&temp_path_for_rename, &layer_path).await {
        Ok(()) => {
            log::debug!("Extracted layer {} to {}", layer_digest, layer_path.display());
        }
        Err(_) if utility::fs::path_exists_lossy(&layer_content).await => {
            // Another process extracted the same layer — discard our copy.
            log::debug!("Layer {} already exists (race), cleaning up temp.", layer_digest);
            // Best-effort cleanup — stale temps are reclaimed by TempStore::try_acquire.
            let _ = tokio::fs::remove_dir_all(&temp_path_for_rename).await;
        }
        Err(e) => {
            return Err(PackageErrorKind::Internal(crate::Error::InternalFile(
                temp_path_for_rename,
                e,
            )));
        }
    }

    drop(temp);
    Ok(())
}

/// Creates dependency forward-refs (`refs/deps/` symlinks) inside the temp directory.
///
/// Symlink targets are absolute paths to dependency content directories already
/// present in the object store (deps are pulled before the dependent). After
/// `move_dir` the symlinks remain valid because their targets are not inside the
/// temp directory being moved.
///
/// ALL deps get symlinks regardless of visibility — visibility only controls
/// env composition, not GC or filesystem presence.
///
/// The caller is responsible for pre-creating `pkg.refs_deps_dir()` via an
/// async `tokio::fs::create_dir_all` — this helper stays sync so it does not
/// introduce blocking I/O into an async context.
#[allow(clippy::result_large_err)]
fn link_dependencies_in_temp(
    pkg: &file_structure::PackageDir,
    dep_infos: &[InstallInfo],
) -> Result<(), PackageErrorKind> {
    if dep_infos.is_empty() {
        return Ok(());
    }
    let deps_dir = pkg.refs_deps_dir();
    for info in dep_infos {
        // The dep's digest is already in hand via its pinned identifier —
        // no path-based recovery needed.
        let dep_digest = info.identifier.digest();
        let name = crate::file_structure::cas_ref_name(&dep_digest);
        let link_path = deps_dir.join(name);
        crate::symlink::create(&info.content, &link_path).map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}

/// Creates layer forward-refs (`refs/layers/` symlinks) inside the temp directory.
///
/// Symlink targets point to `layers/.../content/` — the content path inside
/// each layer. GC's `read_refs` takes `.parent()` on each target to recover
/// the layer entry directory, matching the same convention deps use (targets
/// point to `{entry}/content/`, not the entry dir itself).
///
/// The caller is responsible for pre-creating `pkg.refs_layers_dir()` via an
/// async `tokio::fs::create_dir_all` — this helper stays sync so it does not
/// introduce blocking I/O into an async context.
#[allow(clippy::result_large_err)]
fn link_layers_in_temp(
    pkg: &file_structure::PackageDir,
    registry: &str,
    layer_digests: &[oci::Digest],
    fs: &file_structure::FileStructure,
) -> Result<(), PackageErrorKind> {
    if layer_digests.is_empty() {
        return Ok(());
    }
    let layers_dir = pkg.refs_layers_dir();
    for digest in layer_digests {
        let layer_content = fs.layers.content(registry, digest);
        let name = crate::file_structure::cas_ref_name(digest);
        let link_path = layers_dir.join(name);
        crate::symlink::create(&layer_content, &link_path).map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}
