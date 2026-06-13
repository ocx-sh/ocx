// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinSet;
use tracing::info_span;

use crate::{
    MEDIA_TYPE_PACKAGE_V1, file_structure, log, media_type_select_some, oci,
    package::{install_info::InstallInfo, install_status::InstallStatus, metadata, resolved_package::ResolvedPackage},
    package_manager::{self, composer, concurrency::Concurrency, error::PackageErrorKind},
    prelude::SerdeExt,
    utility::{self, singleflight},
};

use super::super::PackageManager;

/// Singleflight group capacity — maximum number of unique
/// [`PinnedIdentifier`](oci::PinnedIdentifier)s (root packages + transitive
/// dependencies) tracked across all packages in a single `pull_all` call.
///
/// 8192 is the soft worst-case bound for realistic large-closure workloads:
/// mise-style toolchains with 50+ tools each carrying 30–50 transitive deps
/// plausibly push peak in-flight key counts above the previous 1024 cap,
/// which would surface as `singleflight::Error::CapacityExceeded` and abort
/// the install. The cap is intentionally hard rather than backpressuring —
/// hitting 8192 in-flight singleflight keys signals a pathological closure
/// that warrants a publisher-side review, not a silent slow-down.
const MAX_NODES: usize = 8192;

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

/// Conservative lock timeout used when there is no OCI client to derive a
/// configured timeout from (e.g., in `pull_local` with offline mode or a
/// caller-supplied local metadata).
pub const PULL_LOCAL_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

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
///
/// Exposed to sibling task modules (`pull_local`) so they can call
/// `setup_owned` with a fresh group and bypass the `setup_impl` dedup gate.
#[derive(Clone)]
pub struct SetupGroups {
    packages: SetupGroup,
    layers: LayerGroup,
}

impl SetupGroups {
    pub fn new() -> Self {
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
    ///
    /// Every caller — including single-package invocations — goes through the
    /// same JoinSet dispatch so progress is symmetric. Each per-package task
    /// owns a span-free `Spinner` guard (`Pulling '<id>'`) and `scope`s its
    /// work so the download bar nests beneath it. An outer
    /// `info_span!("Pulling", count)` carries only the batch log event.
    ///
    /// `concurrency` caps the **outer** dispatch only — at most N root-package
    /// pulls run in parallel. Inner `setup_dependencies` and `extract_layers`
    /// stay unbounded so a transitive dependency never blocks waiting for a
    /// permit held by its own ancestor (deadlock).
    pub async fn pull_all(
        &self,
        packages: &[oci::Identifier],
        platforms: Vec<oci::Platform>,
        concurrency: Concurrency,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }

        let count = packages.len();
        let outer = info_span!("Pulling", count);
        // Emit an info-level event inside the outer span so the batch is
        // visible in tracing log output (e.g. `RUST_LOG=info`) regardless of
        // whether stderr is a TTY — the non-TTY observable counterpart of the
        // per-package spinners.
        tracing::info!(parent: &outer, count, "pulling");

        let shared_groups = SetupGroups::new();
        let semaphore = concurrency.semaphore();
        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let groups = shared_groups.clone();
            let sem = semaphore.clone();
            tasks.spawn(async move {
                // Permit lives for the full setup_with_tracker call; drop
                // happens after the await returns, releasing the slot for
                // the next queued root pull.
                let _permit = super::super::concurrency::acquire_permit(&sem).await;
                let spin = mgr.progress().spinner(format!("Pulling '{package}'"));
                let result = spin.scope(setup_with_tracker(&mgr, &package, platforms, groups)).await;
                (package, result)
            });
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
    Box::pin(async move { setup_impl(mgr, &package, platforms, groups, None).await })
}

/// Inner implementation of [`PackageManager::pull`] — see that method for
/// concurrency safety documentation.
///
/// `dest_override` — when `Some(path)`, forwarded to [`setup_owned`] and
/// ultimately to [`move_temp_to_object_store`] so the package lands at `path`
/// instead of the content-addressed object store location. All existing callers
/// pass `None`; only `pull_local` supplies a value here (via `setup_owned`
/// directly, bypassing this function's singleflight gate).
async fn setup_impl(
    mgr: &PackageManager,
    package: &oci::Identifier,
    platforms: Vec<oci::Platform>,
    groups: SetupGroups,
    dest_override: Option<&std::path::Path>,
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
                .link_blobs(&info.dir().content(), resolved.blobs())
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
    match setup_owned(mgr, &pinned, resolved, platforms, groups, dest_override, None).await {
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
///
/// `dest_override` — when `Some(path)`, the package is written to `path` instead
/// of the content-addressed object store location. Passed through to
/// [`move_temp_to_object_store`] and the launcher bake-in step. All existing
/// callers pass `None`.
///
/// `provided_metadata` — when `Some`, the metadata validation + `pull_metadata`
/// registry call is skipped and the supplied value is used directly. Used by
/// `pull_local` which has already validated the metadata before calling this
/// function. When `None` (normal pull path), metadata is fetched from the
/// registry via `client.pull_metadata`.
pub async fn setup_owned(
    mgr: &PackageManager,
    pinned: &oci::PinnedIdentifier,
    resolved: super::resolve::ResolvedChain,
    platforms: Vec<oci::Platform>,
    groups: SetupGroups,
    dest_override: Option<&std::path::Path>,
    provided_metadata: Option<metadata::Metadata>,
) -> Result<InstallInfo, PackageErrorKind> {
    // Defense layer 2 — skip if already fully installed (cross-process).
    // When dest_override is set the caller wants to materialize to a specific
    // path, not the object-store CAS path — bypass the fast-path so the
    // materialization always proceeds to the override destination.
    if dest_override.is_none()
        && let Some(info) = mgr.find_plain(pinned).await?
    {
        let install_path = mgr.file_structure().packages.install_status(pinned);
        if crate::package::install_status::check_install_status(&install_path).await {
            log::debug!("Package '{}' already fully installed, skipping.", pinned);
            // Top up chain refs in case the already-installed package was
            // resolved via a different image-index path (alias tag). See
            // the waiter branch in `setup_impl` for the same invariant.
            super::common::reference_manager(mgr.file_structure())
                .link_blobs(&info.dir().content(), resolved.blobs())
                .await
                .map_err(PackageErrorKind::Internal)?;
            return Ok(info);
        }
        // Status missing / partial / not-ok — crash recovery, re-pull.
        log::debug!(
            "Package '{}' present in object store but install status not OK, re-pulling.",
            pinned
        );
    }

    // Defense layer 3: Acquire exclusive temp directory (file lock).
    // When `provided_metadata` is set (pull_local path), there may be no OCI
    // client (offline mode). In that case fall back to a conservative timeout.
    let lock_timeout = mgr
        .client()
        .map(|c| c.lock_timeout())
        .unwrap_or(PULL_LOCAL_LOCK_TIMEOUT);
    let temp = acquire_temp_dir(mgr.file_structure(), pinned, lock_timeout).await?;

    // Post-lock recheck — if we waited for another process to release the
    // lock, it may have already installed the package. Same dest_override
    // gate as above: skip when a specific destination is requested.
    if dest_override.is_none()
        && let Some(info) = mgr.find_plain(pinned).await?
    {
        let install_path = mgr.file_structure().packages.install_status(pinned);
        if crate::package::install_status::check_install_status(&install_path).await {
            log::debug!(
                "Package '{}' installed by another process while waiting for lock, skipping.",
                pinned
            );
            super::common::reference_manager(mgr.file_structure())
                .link_blobs(&info.dir().content(), resolved.blobs())
                .await
                .map_err(PackageErrorKind::Internal)?;
            return Ok(info);
        }
    }

    // Manifest comes from the resolver — ChainedIndex already persisted it to
    // `blobs/` via write-through during resolve, so no extra fetch is needed.
    // When `provided_metadata` is `Some` (pull_local path), the metadata has
    // already been validated by the caller; skip the registry round-trip.
    let manifest = resolved.final_manifest.clone();
    let metadata = if let Some(meta) = provided_metadata {
        meta
    } else {
        // Fetch + media-type-check + validate the config blob. GC-protection
        // comes from the config-blob digest carried in `ResolvedChain.chain`
        // driving `ReferenceManager::link_blobs` below.
        super::common::load_config_metadata(mgr.index(), pinned, &manifest)
            .await?
            .into()
    };

    // Validate manifest before any extraction work. Zero layers is valid —
    // the package is a config-only artifact whose `content/` is the empty
    // directory and whose metadata is the only carried payload.
    media_type_select_some(&manifest.artifact_type, &[MEDIA_TYPE_PACKAGE_V1])
        .map_err(|e| PackageErrorKind::from(oci::client::error::ClientError::internal(e)))?;

    // Wrap the temp directory in a PackageDir so all sibling-file accesses
    // use the canonical accessors instead of hardcoded strings.
    let pkg = file_structure::PackageDir::with_root(temp.dir.dir.clone());

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
    if let Some(entrypoints) = metadata.entrypoints()
        && !entrypoints.is_empty()
    {
        let dest = pkg.entrypoints();
        // Bake the post-move package-root path into the launcher so it remains
        // valid after the temp→final atomic rename.
        let final_pkg_root = dest_override
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| fs.packages.path(pinned));
        crate::package_manager::launcher::generate(&final_pkg_root, entrypoints, &dest)
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
            .map(|(decl, info)| (info.identifier().clone(), info.resolved().clone(), decl.visibility)),
    );

    // Stage 1 entrypoint collision check — runs against the interface
    // projection of the transitive closure, before `resolve.json` is
    // persisted. Catches user-facing duplicate launcher names at install
    // time so the bad state never reaches disk.
    //
    // Scope: interface-projection only (`tc_entry.visibility.has_interface()`).
    // Private-surface duplicates that arise when a private-edge dep
    // contributes its own entrypoint synth-PATH under `--self` are
    // deliberately tolerated and resolved at runtime by topological PATH
    // order (root prepends last so root wins). See `adr_two_env_composition.md`.
    {
        let root_info = Arc::new(InstallInfo::new(
            pinned.clone(),
            metadata.clone(),
            resolved_package.clone(),
            pkg.clone(),
        ));
        composer::check_entrypoints(std::slice::from_ref(&root_info), &fs.packages).await?;
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
        .link_blobs(&pkg.content(), resolved.blobs())
        .await
        .map_err(PackageErrorKind::Internal)?;

    // Atomic move temp → object store.
    let install_info = move_temp_to_object_store(
        mgr.file_structure(),
        pinned,
        &metadata,
        resolved_package,
        temp,
        dest_override,
    )
    .await?;

    log::debug!("Pull succeeded for '{}'.", pinned);
    Ok(install_info)
}

/// Acquires an exclusive temp directory for the given identifier.
///
/// `lock_timeout` is used when the temp directory is locked by another
/// process — this is how long to wait before giving up. Pass
/// `client.lock_timeout()` on the normal pull path; pass
/// [`PULL_LOCAL_LOCK_TIMEOUT`] when there is no client available (e.g.,
/// the `pull_local` offline path).
pub async fn acquire_temp_dir(
    fs: &file_structure::FileStructure,
    identifier: &oci::PinnedIdentifier,
    lock_timeout: Duration,
) -> Result<crate::file_structure::TempAcquireResult, PackageErrorKind> {
    let temp_path = fs.temp.path(identifier).map_err(PackageErrorKind::Internal)?;

    let acquire = match fs.temp.try_acquire(&temp_path).map_err(PackageErrorKind::Internal)? {
        Some(r) => r,
        None => {
            log::debug!("Temp dir locked by another process, waiting: {}", temp_path.display());
            fs.temp
                .acquire_with_timeout(&temp_path, lock_timeout)
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
///
/// Returns `Vec<Arc<InstallInfo>>` rather than `Vec<InstallInfo>` so the
/// caller (`setup_owned`) can hand each dep straight to
/// [`DependencyContext::full(Arc<InstallInfo>)`](crate::package::metadata::env::dep_context::DependencyContext::full)
/// without re-allocating an `Arc` (and cloning the underlying `InstallInfo`)
/// per direct dep. The Arc-sharing invariant introduced by the metadata
/// pipeline (commit 40b001f) is meant to live end-to-end on this path; the
/// previous `Vec<InstallInfo>` return type forced the consumer to
/// `Arc::new(info.clone())` and undid the saving.
async fn setup_dependencies(
    mgr: &PackageManager,
    metadata: &crate::package::metadata::Metadata,
    parent: &oci::PinnedIdentifier,
    platforms: Vec<oci::Platform>,
    groups: SetupGroups,
) -> Result<Vec<Arc<InstallInfo>>, PackageErrorKind> {
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
        tasks.spawn(crate::cli::progress::inherit_scope(async move {
            let info = setup_with_tracker(&mgr, &dep_id, platforms, groups).await?;
            Ok::<_, PackageErrorKind>((idx, Arc::new(info)))
        }));
    }

    let mut results: Vec<Option<Arc<InstallInfo>>> = vec![None; deps.len()];
    while let Some(join_result) = tasks.join_next().await {
        // Preserve the original task panic for diagnostics: per
        // `quality-rust.md` Async Patterns, resume_unwind on panic and
        // panic with the JoinError context on cancellation.
        let (idx, info) = match join_result {
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => panic!("dependency setup task aborted: {e}"),
            Ok(v) => v,
        }?;
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
    // Tag-preservation policy: `resolve.json` deliberately keeps each
    // dependency's advisory tag (the form that won the install-time race).
    // `ocx.lock` strips it via `PinnedIdentifier::strip_advisory()` because
    // a project lock is the canonical pinned record and a tag-only churn
    // would bust `generated_at` preservation. The two files have different
    // jobs: install-time audit trail vs. canonical project pin. Do not
    // harmonise without revisiting plan_project_toolchain.md §7.4.
    resolved
        .write_json(pkg.resolve())
        .await
        .map_err(PackageErrorKind::Internal)?;

    // Write install.json through the lock-owning handle so concurrent
    // `check_install_status` readers (shared lock) wait for the write to
    // complete before reading. Eliminates the existence-probe race where a
    // reader could observe a partial JSON document from a mid-write writer.
    {
        let mut locked = utility::fs::LockedJsonFile::<InstallStatus>::open_exclusive(pkg.install_status())
            .await
            .map_err(PackageErrorKind::Internal)?;
        locked
            .write(&InstallStatus::new().ok())
            .await
            .map_err(PackageErrorKind::Internal)?;
    }

    file_structure::write_digest_file(&pkg.digest_file(), &pinned.digest())
        .await
        .map_err(PackageErrorKind::Internal)?;

    Ok(())
}

/// Non-destructive publish of an enriched package temp directory into the CAS.
///
/// Mirrors the proven layer-tier [`super::layer_staging::finalize_layer_dir`]
/// pattern extended with a guarded swap for the broken-install replacement
/// case (the one case the layer tier never hits).
///
/// **Race semantics (three branches):**
///
/// 1. `rename(temp, output_path)` succeeds → first writer wins; `Ok(())`.
/// 2. `rename` fails AND `output_path` contains a committed OK install
///    (`install.json` present and valid) → discard our temp, reuse the winner.
///    Stricter than the layer tier's `path_exists_lossy` check: requires a
///    committed install so a half-written loser cannot masquerade as a winner.
/// 3. `rename` fails AND `output_path` exists but is **not** a committed OK
///    install (broken/partial) → **stash→swap under the per-digest temp lock
///    already held by the pull path**:
///    ```text
///    stash = {temp_root}/__stale_{pid}_{rand}   // reclaimed by stale sweep
///    rename(output_path, stash)                 // move old live dir out
///    rename(temp, output_path)                  // new dir into canonical name
///    remove_dir_all(stash)  (best-effort)
///    ```
///    Post-lock recheck collapses a second concurrent writer's swap to a no-op.
///
/// **Invariant INV-M1.** No lock-free reader ever observes `packages/{digest}/`
/// missing or half-deleted during publish or re-pull. The happy path is
/// rename-only (no `remove_dir_all` of the canonical name); the broken-install
/// replace only ever `rename`s the canonical name outward before the new dir
/// is renamed in, so open file descriptors survive; two writers are serialized
/// by the held digest lock.
///
/// **Not for `dest_override` paths.** When the caller supplies an override
/// destination (e.g. `pull_local` with a caller-owned empty target), use
/// [`move_temp_to_object_store`] instead — that path is empty by contract and
/// does not share the CAS address space.
async fn finalize_package_dir(
    fs: &file_structure::FileStructure,
    pinned: &oci::PinnedIdentifier,
    temp_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<(), PackageErrorKind> {
    // Branch 1: bare first-writer-wins rename. No `remove_dir_all` of the
    // canonical name on this path ever.
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(parent.to_path_buf(), e)))?;
    }
    match tokio::fs::rename(temp_path, output_path).await {
        Ok(()) => {
            log::debug!(
                "Package '{}' published (first writer): {}",
                pinned,
                output_path.display()
            );
            return Ok(());
        }
        Err(rename_err) => {
            // A rename can only fail with the dest occupied (ENOTEMPTY /
            // directory-not-empty) on the contended paths we care about; any
            // other error with no dest present is a genuine I/O failure.
            if !utility::fs::path_exists_lossy(output_path).await {
                return Err(PackageErrorKind::Internal(crate::Error::InternalFile(
                    output_path.to_path_buf(),
                    rename_err,
                )));
            }
        }
    }

    // Dest exists. Branch 2: a committed OK install already won the race —
    // discard our temp and reuse the winner. Stricter than the layer tier's
    // bare existence check: a half-written loser must not masquerade as a
    // winner, so require a committed `install.json`.
    let output_status = file_structure::PackageDir::with_root(output_path.to_path_buf()).install_status();
    if crate::package::install_status::check_install_status(&output_status).await {
        log::debug!(
            "Package '{}' already committed by a concurrent winner; discarding temp.",
            pinned
        );
        // Best-effort: reclaim our now-redundant temp dir immediately so it
        // does not linger until the next stale sweep.
        if let Err(e) = tokio::fs::remove_dir_all(temp_path).await {
            log::debug!("Failed to remove redundant temp dir {}: {}", temp_path.display(), e);
        }
        return Ok(());
    }

    // Branch 3: dest exists but is NOT a committed OK install (broken/partial).
    // Stash→swap under the per-digest TempStore lock the pull path already
    // holds — never `remove_dir_all(output_path)`. The canonical name is only
    // ever renamed outward (to a stash under the temp zone) before the new dir
    // is renamed in, so a lock-free reader never observes it missing and open
    // file descriptors survive the unlink.
    swap_broken_install(fs, pinned, temp_path, output_path).await
}

/// Replaces a broken/partial install at `output_path` with the freshly-built
/// `temp_path` via the stash→swap idiom (INV-M1 branch 3).
///
/// Holds the per-digest [`TempStore`](crate::file_structure::TempStore) lock
/// already acquired by the pull path (its sibling `.lock` survives the rename
/// because it lives outside the dir). A post-lock recheck collapses a second
/// concurrent writer's swap to a no-op. The directory renames are blocking and
/// routed through [`with_windows_rename_retry`](crate::utility::fs::with_windows_rename_retry)
/// inside `spawn_blocking` so a live launcher holding a handle open
/// (`ERROR_SHARING_VIOLATION`) does not fail the swap.
async fn swap_broken_install(
    fs: &file_structure::FileStructure,
    pinned: &oci::PinnedIdentifier,
    temp_path: &std::path::Path,
    output_path: &std::path::Path,
) -> Result<(), PackageErrorKind> {
    // Post-lock recheck: another writer may have committed an OK install
    // between branch 2's probe and here (both run under the held digest lock,
    // so this only catches a winner that committed before we acquired it —
    // belt-and-suspenders against a TOCTOU on the status read).
    let output_status = file_structure::PackageDir::with_root(output_path.to_path_buf()).install_status();
    if crate::package::install_status::check_install_status(&output_status).await {
        log::debug!(
            "Package '{}' became a committed install before swap; discarding temp.",
            pinned
        );
        if let Err(e) = tokio::fs::remove_dir_all(temp_path).await {
            log::debug!("Failed to remove redundant temp dir {}: {}", temp_path.display(), e);
        }
        return Ok(());
    }

    // Stash path under the temp zone so the existing stale sweep reclaims it
    // even if this process dies mid-swap. `__stale_` prefix + pid + random
    // suffix keeps it disjoint from the 32-hex temp-dir names.
    let stash = stash_path(fs);
    // Ensure the temp root exists so `rename(output, stash)` has a valid
    // parent. Normally the pull path already created it via `acquire_temp_dir`,
    // but the unit test calls `finalize_package_dir` directly without one.
    if let Some(parent) = stash.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(parent.to_path_buf(), e)))?;
    }
    log::debug!(
        "Replacing broken install for '{}' via stash→swap (stash: {})",
        pinned,
        stash.display()
    );

    let temp_owned = temp_path.to_path_buf();
    let output_owned = output_path.to_path_buf();
    let stash_owned = stash.clone();

    // The two swap renames run on a blocking thread (std::fs::rename) and route
    // through the Windows rename-retry helper. spawn_blocking keeps the async
    // runtime free while the blocking renames + the optional fault pause run.
    let pinned_for_log = pinned.to_string();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        // Fault hook (M5): block here BEFORE any rename, while the broken dir
        // still occupies the canonical name, so a reader-loop test can prove
        // INV-M1 — a lock-free reader keeps finding the package throughout the
        // (test-controlled) pause window. Gated on `__OCX_TESTING_PUBLISH_PAUSE`;
        // blocks until the release file named by
        // `__OCX_TESTING_PUBLISH_RELEASE_FILE` appears. Once released the two
        // renames run back-to-back with no `.await` and no further pause, so the
        // only "missing" window is the microscopic inter-rename gap the OS
        // makes atomic per step.
        maybe_pause_publish(&pinned_for_log)?;

        // Step 1: move the old live dir out of the canonical name (atomic; open
        // fds survive via the stash inode).
        utility::fs::with_windows_rename_retry(|| std::fs::rename(&output_owned, &stash_owned))?;

        // Step 2: move the new dir into the canonical name (atomic).
        utility::fs::with_windows_rename_retry(|| std::fs::rename(&temp_owned, &output_owned))?;
        Ok(())
    })
    .await
    .map_err(|join_err| {
        if join_err.is_panic() {
            std::panic::resume_unwind(join_err.into_panic());
        }
        PackageErrorKind::Internal(crate::Error::InternalFile(
            output_path.to_path_buf(),
            std::io::Error::other(format!("broken-install swap task aborted: {join_err}")),
        ))
    })?
    .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(output_path.to_path_buf(), e)))?;

    // Best-effort: reclaim the stash now; otherwise the stale sweep does it.
    if let Err(e) = tokio::fs::remove_dir_all(&stash).await {
        log::debug!(
            "Failed to remove stash dir {} (stale sweep will reclaim): {}",
            stash.display(),
            e
        );
    }

    Ok(())
}

/// Builds a unique stash path under the package-staging temp zone for the
/// broken-install swap. Name = `__stale_<pid>_<rand>` so it is reclaimed by the
/// existing `TempStore` stale sweep and never collides with a real temp dir.
fn stash_path(fs: &file_structure::FileStructure) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let pid = std::process::id();
    // Cheap process-local uniqueness without a `rand` dep: wall-clock nanos
    // XORed with a monotonically-increasing counter. Collisions only matter
    // within one process holding the same digest lock, which is serialized.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let seq = STASH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    fs.temp.root().join(format!("__stale_{pid}_{nanos:08x}{seq:08x}"))
}

/// Monotonic counter feeding [`stash_path`] uniqueness within a process.
static STASH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Test-only fault hook (M5) for the broken-install swap.
///
/// Gated on `__OCX_TESTING_PUBLISH_PAUSE` (any non-empty value). When active,
/// blocks (with the broken dir still at the canonical name) until the release
/// file named by `__OCX_TESTING_PUBLISH_RELEASE_FILE` appears, so a reader-loop
/// test can prove INV-M1 holds across a long pause window.
///
/// Off the hook this is a zero-cost env probe. Production builds never set the
/// var; the `__OCX_TESTING_` prefix keeps it out of the public env namespace.
/// Both keys are read through [`crate::env::var`] so the in-process test
/// override seam (`crate::test::env`) can drive the pause without an unsafe
/// `std::env::set_var`; the release-file is polled on disk.
fn maybe_pause_publish(pinned: &str) -> std::io::Result<()> {
    match crate::env::var("__OCX_TESTING_PUBLISH_PAUSE") {
        Some(value) if !value.is_empty() => {}
        _ => return Ok(()),
    }

    crate::log::debug!("[__OCX_TESTING_PUBLISH_PAUSE] paused broken-install swap for '{pinned}'");
    let release = crate::env::var("__OCX_TESTING_PUBLISH_RELEASE_FILE");
    loop {
        if let Some(ref path) = release
            && std::fs::metadata(path).is_ok()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    Ok(())
}

/// Publishes the enriched temp directory to its destination.
///
/// Branches on the destination kind:
///
/// - **CAS dest** (`dest_override == None`) → [`finalize_package_dir`]: the
///   non-destructive INV-M1 publish (first-writer-wins bare rename, OK-winner
///   reuse, or stash→swap over a broken install under the held digest lock).
///   The canonical `packages/{digest}/` name is never `remove_dir_all`'d.
/// - **Override dest** (`dest_override == Some(path)`) → [`move_dir`]: the
///   caller owns `path` and it is empty-by-contract (not a shared CAS target),
///   so the destructive overwrite-then-rename is appropriate there.
///
/// The returned [`InstallInfo`] is constructed with a
/// [`PackageDir`](file_structure::PackageDir) rooted at whichever destination
/// was chosen.
///
/// [`move_dir`]: crate::utility::fs::move_dir
async fn move_temp_to_object_store(
    fs: &file_structure::FileStructure,
    identifier: &oci::PinnedIdentifier,
    metadata: &metadata::Metadata,
    resolved: ResolvedPackage,
    temp: crate::file_structure::TempAcquireResult,
    dest_override: Option<&std::path::Path>,
) -> Result<InstallInfo, PackageErrorKind> {
    let temp_path = temp.dir.dir.clone();

    let output_path = match dest_override {
        // Override path: caller-owned, empty-by-contract. Keep the destructive
        // move_dir — it is not a shared CAS address.
        Some(path) => {
            let output_path = path.to_path_buf();
            crate::utility::fs::move_dir(&temp_path, &output_path)
                .await
                .map_err(PackageErrorKind::Internal)?;
            output_path
        }
        // CAS path: route through the non-destructive INV-M1 publish.
        None => {
            let output_path = fs.packages.path(identifier);
            finalize_package_dir(fs, identifier, &temp_path, &output_path).await?;
            output_path
        }
    };
    let pkg = file_structure::PackageDir::with_root(output_path);

    // Drop the temp handle last so the per-digest lock guarding the stash→swap
    // is held through `finalize_package_dir`.
    drop(temp);

    Ok(InstallInfo::new(identifier.clone(), metadata.clone(), resolved, pkg))
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
        tasks.spawn(crate::cli::progress::inherit_scope(async move {
            let res = extract_layer_atomic(&mgr, &pinned, &layer, &digest, &metadata, layer_group).await;
            (idx, res)
        }));
    }

    let mut results: Vec<Option<oci::Digest>> = vec![None; tasks.len()];
    while let Some(join_res) = tasks.join_next().await {
        // Preserve the original task panic for diagnostics: per
        // `quality-rust.md` Async Patterns, resume_unwind on panic and
        // panic with the JoinError context on cancellation.
        let (idx, task_res) = match join_res {
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => panic!("layer extraction task aborted: {e}"),
            Ok(v) => v,
        };
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

    // Step 2: layer cache fast path — if `layers/{digest}/content/` is present,
    // no fetch is needed. Acquire the client lazily so an offline manager (no
    // client) can still re-assemble a package whose layers are all cached.
    let layer_content = fs.layers.content(pinned.registry(), layer_digest);
    if utility::fs::path_exists_lossy(&layer_content).await {
        log::debug!("Layer {} present on disk, skipping fetch.", layer_digest);
        handle.complete(());
        return Ok(layer_digest.clone());
    }

    let client = match mgr.require_client() {
        Ok(c) => c,
        Err(e) => {
            let kind = PackageErrorKind::Internal(e);
            let shared = handle.fail(kind);
            return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
        }
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

    // Step 3: acquire exclusive temp dir at the layer-keyed path. Layer
    // staging uses `layer_temp` (cache-zone) so the final rename into
    // `layers/` is always an intra-volume move even when packages live on a
    // different volume. In the unified-zone layout `layer_temp` and `temp`
    // point at the same directory, so this is identical to before the split.
    let temp_path = fs.layer_temp.layer_path(pinned.registry(), layer_digest);
    let temp = match fs
        .layer_temp
        .try_acquire(&temp_path)
        .map_err(PackageErrorKind::Internal)?
    {
        Some(r) => r,
        None => {
            log::debug!(
                "Layer temp dir locked by another process, waiting: {}",
                temp_path.display()
            );
            fs.layer_temp
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
    super::layer_staging::finalize_layer_dir(fs, pinned.registry(), layer_digest, &temp.dir.dir).await?;

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
    dep_infos: &[Arc<InstallInfo>],
) -> Result<(), PackageErrorKind> {
    if dep_infos.is_empty() {
        return Ok(());
    }
    let deps_dir = pkg.refs_deps_dir();
    for info in dep_infos {
        // The dep's digest is already in hand via its pinned identifier —
        // no path-based recovery needed.
        let dep_digest = info.identifier().digest();
        let name = crate::file_structure::cas_ref_name(&dep_digest);
        let link_path = deps_dir.join(name);
        crate::symlink::create(info.dir().content(), &link_path).map_err(PackageErrorKind::Internal)?;
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

// ── finalize_package_dir behavioral specifications (P1.2) ────────────────────
//
// These tests verify the three branches of INV-M1 non-destructive publish:
//
//   Branch 1: first writer — rename(temp, output_path) succeeds.
//   Branch 2: race loser — dest is a committed OK install; temp discarded,
//             winner content unchanged.
//   Branch 3: broken install replace — dest install.json ok=false; new
//             content must be present at output_path after finalize.
//
// Tests are deterministic (no pause hook) and operate at the filesystem
// level.  The pause-hook tests (reader_never_observes_remove_window) require
// the __OCX_TESTING_PUBLISH_PAUSE fault hook added in P1.5 — see the
// TODO comment below.
//
// Requirement: system_design_shared_store.md §5 M1 (INV-M1).
// Traced to: plan_shared_store P1.2.
//
// TODO P1.5: reader_never_observes_remove_window (INV-M1 reader-loop) —
// requires the __OCX_TESTING_PUBLISH_PAUSE fault hook added in P1.5.
#[cfg(test)]
mod finalize_package_dir_tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::file_structure::FileStructure;
    use crate::oci::{Digest, Identifier, PinnedIdentifier};
    use crate::package::install_status::InstallStatus;

    use super::finalize_package_dir;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn make_pinned() -> PinnedIdentifier {
        let id = Identifier::new_registry("cmake", "example.com")
            .clone_with_tag("3.28")
            .clone_with_digest(Digest::Sha256("a".repeat(64)));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Write an `install.json` with `ok` set to the given value.
    fn write_install_json(dir: &Path, ok: bool) {
        let status = if ok {
            InstallStatus::new().ok()
        } else {
            InstallStatus::new()
        };
        let json = serde_json::to_vec(&status).unwrap();
        std::fs::write(dir.join("install.json"), json).unwrap();
    }

    /// Write a distinguishing sentinel file into `dir` so we can assert
    /// which directory's content is present at the output path after finalize.
    fn write_sentinel(dir: &Path, content: &str) {
        std::fs::write(dir.join("sentinel.txt"), content).unwrap();
    }

    fn read_sentinel(dir: &Path) -> Option<String> {
        std::fs::read_to_string(dir.join("sentinel.txt")).ok()
    }

    // ── Branch 1: first writer wins ──────────────────────────────────────────
    //
    // Requirement: M1 branch 1 — "`rename(temp, output_path)` succeeds →
    // first writer wins; Ok(())."
    //
    // When no directory exists at output_path, finalize_package_dir must
    // atomically rename temp into output_path and return Ok.
    //
    // Multi-thread flavor: the OK-winner/broken-install branches call
    // `check_install_status`, which acquires a blocking advisory lock through
    // `LockedJsonFile` and so requires the multi-threaded Tokio runtime.
    #[tokio::test(flavor = "multi_thread")]
    async fn finalize_package_dir_first_writer_renames_in() {
        let root = TempDir::new().unwrap();
        let root_path = root.path().to_path_buf();

        let fs = FileStructure::with_root(root_path.clone());
        let pinned = make_pinned();

        // Build a temp directory with sentinel content.
        let temp_dir = root_path.join("temp_stage");
        std::fs::create_dir_all(&temp_dir).unwrap();
        write_sentinel(&temp_dir, "winner-content");
        write_install_json(&temp_dir, true);

        // output_path must not exist yet (first writer).
        let output_path = root_path.join("output_pkg");
        assert!(!output_path.exists(), "precondition: output_path must not exist");

        let result = finalize_package_dir(&fs, &pinned, &temp_dir, &output_path).await;

        result.unwrap();
        assert!(output_path.exists(), "output_path must exist after first-writer rename");
        assert!(
            !temp_dir.exists(),
            "temp dir must not exist after successful rename (it was moved)"
        );
        assert_eq!(
            read_sentinel(&output_path),
            Some("winner-content".to_string()),
            "output_path must contain the temp dir's content after rename"
        );
    }

    // ── Branch 2: race loser — winner already committed ───────────────────────
    //
    // Requirement: M1 branch 2 — "rename fails AND output_path contains a
    // committed OK install → discard our temp, reuse the winner."
    //
    // When a concurrent writer already installed a committed OK package at
    // output_path, finalize_package_dir must return Ok and leave the winner's
    // content intact.  The loser's temp may be discarded or cleaned up.
    #[tokio::test(flavor = "multi_thread")]
    async fn finalize_package_dir_loser_discards_temp_keeps_ok_winner() {
        let root = TempDir::new().unwrap();
        let root_path = root.path().to_path_buf();

        let fs = FileStructure::with_root(root_path.clone());
        let pinned = make_pinned();

        // Simulate "already committed by a concurrent winner": create
        // output_path with an OK install.json and distinct content.
        let output_path = root_path.join("output_pkg");
        std::fs::create_dir_all(&output_path).unwrap();
        write_install_json(&output_path, true);
        write_sentinel(&output_path, "committed-winner-content");

        // Our losing temp has different content.
        let temp_dir = root_path.join("temp_loser");
        std::fs::create_dir_all(&temp_dir).unwrap();
        write_install_json(&temp_dir, true);
        write_sentinel(&temp_dir, "loser-content");

        let result = finalize_package_dir(&fs, &pinned, &temp_dir, &output_path).await;

        // Must succeed: losing race to a committed winner is not an error.
        result.unwrap();

        // Winner's content must be unchanged.
        assert_eq!(
            read_sentinel(&output_path),
            Some("committed-winner-content".to_string()),
            "committed winner's content must be preserved; loser must not overwrite"
        );
    }

    // ── Branch 3: broken install replacement ─────────────────────────────────
    //
    // Requirement: M1 branch 3 — "rename fails AND output_path exists but is
    // NOT a committed OK install → stash→swap under the per-digest temp lock
    // already held by the pull path."
    //
    // The design invariant: no `remove_dir_all(output_path)`.  The canonical
    // name is only ever `rename`d outward (to stash) before the new dir is
    // renamed inward.
    //
    // Observable contract tested here: after finalize_package_dir returns Ok,
    // `output_path` must contain our new content (not the broken content), and
    // no stash directory must remain under the packages/ tree.
    #[tokio::test(flavor = "multi_thread")]
    async fn finalize_package_dir_replaces_broken_install_via_swap() {
        let root = TempDir::new().unwrap();
        let root_path = root.path().to_path_buf();

        let fs = FileStructure::with_root(root_path.clone());
        let pinned = make_pinned();

        // Simulate a "broken" install at output_path: install.json with ok=false.
        let output_path = root_path.join("output_pkg");
        std::fs::create_dir_all(&output_path).unwrap();
        write_install_json(&output_path, false);
        write_sentinel(&output_path, "broken-content");

        // New temp dir with the correct (repaired) content.
        let temp_dir = root_path.join("temp_repair");
        std::fs::create_dir_all(&temp_dir).unwrap();
        write_install_json(&temp_dir, true);
        write_sentinel(&temp_dir, "repaired-content");

        let result = finalize_package_dir(&fs, &pinned, &temp_dir, &output_path).await;

        result.unwrap();

        // After finalize, output_path must contain the repaired content.
        assert_eq!(
            read_sentinel(&output_path),
            Some("repaired-content".to_string()),
            "output_path must contain the new (repaired) content after broken-install swap"
        );

        // No stash directory must remain in the temp zone after finalize.
        // The stash is reclaimed by the existing stale sweep; for deterministic
        // unit tests we assert there is no leftover __stale_* entry under
        // the temp root (root_path/temp in the unified-zone layout used here).
        let temp_root = root_path.join("temp");
        if temp_root.exists() {
            let stale_remains: Vec<PathBuf> = std::fs::read_dir(&temp_root)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with("__stale_"))
                .map(|e| e.path())
                .collect();
            assert!(
                stale_remains.is_empty(),
                "no __stale_ directory must remain under temp/ after finalize: {:?}",
                stale_remains
            );
        }
    }
}
