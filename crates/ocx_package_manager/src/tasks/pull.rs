// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::task::JoinSet;
use tracing::info_span;

use crate::{composer, concurrency::Concurrency, error::PackageErrorKind};
use ocx_oci::{self, media_type::MEDIA_TYPE_PACKAGE_V1};
use ocx_package::{
    install_info::InstallInfo, install_status::InstallStatus, metadata, resolved_package::ResolvedPackage,
};
use ocx_store::file_structure;

use ocx_util::prelude::SerdeExt;
use ocx_util::singleflight;

use super::super::PackageManager;
use super::resolve::NoTransport;
use crate::activity::{Action, PackageActivity, Role};

/// Unique pinned refs (roots + transitive deps) one `pull_all` may track; past it the
/// pull aborts with `CapacityExceeded`, hard rather than backpressured.
const MAX_NODES: usize = 8192;

/// How long a waiter blocks on a hung singleflight leader; not the OCI download timeout.
const PACKAGE_SETUP_TIMEOUT: Duration = Duration::from_mins(10);

/// Longer than package setup: a single layer may be several hundred MB.
const LAYER_SETUP_TIMEOUT: Duration = Duration::from_mins(30);

/// Temp-dir lock timeout when no OCI client supplies one (offline `pull_local`).
pub const PULL_LOCAL_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// Package-setup dedup, keyed by pinned ref with the advisory tag stripped.
type SetupGroup = singleflight::Group<ocx_oci::PinnedPackageRef, InstallInfo>;

/// Layer-extraction dedup keyed by `(registry, digest)`; the result is `layers/{digest}/content/` on disk.
type LayerGroup = singleflight::Group<(String, ocx_oci::Digest), ()>;

/// Package- and layer-level singleflight groups sharing one pull session.
#[derive(Clone)]
pub struct SetupGroups {
    packages: SetupGroup,
    layers: LayerGroup,
    /// Packages whose leaf manifest the credential prefetch read from a source. That read leaves a home copy,
    /// so the pull's own resolve sees a home hit; keyed by digest, the record applies to that package only.
    network_leaves: Arc<std::collections::HashSet<ocx_oci::PinnedPackageRef>>,
}

impl SetupGroups {
    pub fn new() -> Self {
        Self {
            packages: SetupGroup::new(MAX_NODES, PACKAGE_SETUP_TIMEOUT),
            layers: LayerGroup::new(MAX_NODES, LAYER_SETUP_TIMEOUT),
            network_leaves: Arc::default(),
        }
    }

    fn with_network_leaves(mut self, leaves: std::collections::HashSet<ocx_oci::PinnedPackageRef>) -> Self {
        self.network_leaves = Arc::new(leaves);
        self
    }
}

fn map_singleflight_error(e: singleflight::Error) -> PackageErrorKind {
    let dep_err: crate::DependencyError = e.into();
    PackageErrorKind::Internal(dep_err.into())
}

impl PackageManager {
    /// Downloads a package and its transitive dependencies into the object store,
    /// without install symlinks (see [`PackageManager::install`]).
    ///
    /// Idempotent and safe under concurrent, cross-process calls.
    pub fn pull(
        &self,
        package: &ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
    ) -> Pin<Box<dyn Future<Output = Result<InstallInfo, PackageErrorKind>> + Send + '_>> {
        pull_as(self, package, platform, Role::Root)
    }

    /// Pulls packages in parallel, sharing one singleflight session across them, then re-checks
    /// their patches unless `skip_discovery`.
    ///
    /// `concurrency` bounds only root pulls; bounding dependency or layer setup would
    /// deadlock a dependency waiting on a permit its own ancestor holds.
    pub async fn pull_all(
        &self,
        packages: &[ocx_oci::PackageRef],
        platform: ocx_oci::Platform,
        concurrency: Concurrency,
        // `true` for callers that discover later themselves, and for internal pulls of `ocx` itself.
        skip_discovery: bool,
    ) -> Result<Vec<InstallInfo>, crate::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }

        let count = packages.len();
        let outer = info_span!("Pulling", count);
        // The batch's only trace in non-TTY log output.
        tracing::info!(parent: &outer, count, "pulling");

        let mut prefetched = prefetch_credentials(self, packages, &platform).await;

        let network_leaves = prefetched
            .iter_mut()
            .flat_map(|p| std::mem::take(&mut p.network_leaves))
            .collect();
        let shared_groups = SetupGroups::new().with_network_leaves(network_leaves);
        let semaphore = concurrency.semaphore();
        let mut tasks = JoinSet::new();
        for (slot, package) in packages.iter().enumerate() {
            // The prefetch already met this root's failure; dialling it again would only repeat it.
            if let Some(kind) = prefetched[slot].error.take() {
                let package = package.clone();
                tasks.spawn(async move { (package, Err(kind)) });
                continue;
            }
            let mgr = self.clone();
            let package = package.clone();
            let platform = platform.clone();
            let groups = shared_groups.clone();
            let sem = semaphore.clone();
            tasks.spawn(async move {
                // Named, not `_`: dropping it before setup returns lifts the root-pull cap.
                let _permit = super::super::concurrency::acquire_permit(&sem).await;
                let result = setup_impl(&mgr, &package, platform, groups, None, Role::Root).await;
                // Also when the package lost the singleflight race to a sibling's dependency path, whose
                // entry says `Dependency`: the drain reads the root sets, not only the entries.
                if let (Ok(info), Some(log)) = (&result, mgr.activity_log()) {
                    log.record_roots([info.identifier().clone()]);
                }
                (package, result)
            });
        }

        let infos = super::common::drain_package_tasks(packages, tasks, crate::error::Error::InstallFailed).await?;
        if !skip_discovery {
            self.discover_patches_all(
                packages,
                &platform,
                super::patch_discovery::PatchDiscoveryMode::Revalidate,
                concurrency,
            )
            .await?;
        }
        Ok(infos)
    }
}

/// Resolves the credentials of every registry the closures of the uninstalled `packages` touch.
///
/// Runs before any setup task takes a staging lock: a credential helper that is itself an ocx
/// trampoline would otherwise start while a lock is held and wait on it (#623). Best effort except
/// for a root that does not resolve, whose error is returned in its slot: the pull would hit the
/// same one, and a refused handshake must not be dialled twice. A closure that cannot be walked
/// resurfaces, with its proper error, from the pull that follows.
async fn prefetch_credentials(
    mgr: &PackageManager,
    packages: &[ocx_oci::PackageRef],
    platform: &ocx_oci::Platform,
) -> Vec<Prefetched> {
    if mgr.is_offline() {
        return packages.iter().map(|_| Prefetched::default()).collect();
    }
    let walks = packages.iter().map(|package| async move {
        match walk_uninstalled_closure(mgr, package, platform).await {
            Ok(network_leaves) => Prefetched {
                error: None,
                network_leaves,
            },
            Err(kind) => Prefetched {
                error: Some(kind),
                network_leaves: Default::default(),
            },
        }
    });
    futures::future::join_all(walks).await
}

/// What the credential prefetch learned about one root.
#[derive(Default)]
struct Prefetched {
    /// The root's own resolution failure.
    error: Option<PackageErrorKind>,
    /// The root's leaf and every dependency whose manifest the prefetch read from a source.
    network_leaves: std::collections::HashSet<ocx_oci::PinnedPackageRef>,
}

/// # Errors
///
/// Only the root's own resolution failure; the closure walk after it is logged and dropped.
async fn walk_uninstalled_closure(
    mgr: &PackageManager,
    package: &ocx_oci::PackageRef,
    platform: &ocx_oci::Platform,
) -> Result<std::collections::HashSet<ocx_oci::PinnedPackageRef>, PackageErrorKind> {
    // A digest-pinned root answers from the store without resolving, which would reach the network.
    if let Ok(pinned) = ocx_oci::PinnedPackageRef::try_from(package.clone())
        && is_installed(mgr, &pinned).await
    {
        return Ok(Default::default());
    }
    let resolved = mgr.resolve(package, platform.clone()).await?;
    let mut network_leaves = std::collections::HashSet::new();
    if resolved.leaf_origin == ocx_index::BlobOrigin::Network {
        network_leaves.insert(resolved.pinned.strip_advisory());
    }
    if is_installed(mgr, &resolved.pinned).await {
        return Ok(network_leaves);
    }
    match walk_closure(mgr, &resolved, platform).await {
        Ok(deps) => network_leaves.extend(deps),
        Err(error) => log::debug!(
            "Credential prefetch for '{package}' stopped early: {chain}",
            chain = ocx_util::error::render_chain(&error)
        ),
    }
    Ok(network_leaves)
}

async fn walk_closure(
    mgr: &PackageManager,
    resolved: &super::resolve::ResolvedChain,
    platform: &ocx_oci::Platform,
) -> Result<std::collections::HashSet<ocx_oci::PinnedPackageRef>, PackageErrorKind> {
    let (fs, index) = (mgr.file_structure(), mgr.index());
    let metadata = super::common::load_config_metadata(index, &resolved.pinned, &resolved.final_manifest).await?;
    let config_digest = super::common::config_blob_digest(&resolved.final_manifest)?;
    let (_, network) = super::common::walk_closure_nodes_with_origins(
        fs,
        index,
        false,
        &resolved.pinned,
        &metadata,
        config_digest,
        platform,
    )
    .await?;
    Ok(network)
}

/// A probe failure reads as "not installed": the pull itself will meet and report it.
async fn is_installed(mgr: &PackageManager, pinned: &ocx_oci::PinnedPackageRef) -> bool {
    matches!(mgr.find_plain(pinned).await, Ok(Some(_)))
        && ocx_package::install_status::check_install_status(&mgr.file_structure().packages.install_status(pinned))
            .await
}

/// [`PackageManager::pull`], recording its activity entry as `role`.
pub(super) fn pull_as<'a>(
    mgr: &'a PackageManager,
    package: &ocx_oci::PackageRef,
    platform: ocx_oci::Platform,
    role: Role,
) -> Pin<Box<dyn Future<Output = Result<InstallInfo, PackageErrorKind>> + Send + 'a>> {
    setup_with_tracker(mgr, package, platform, SetupGroups::new(), role)
}

fn setup_with_tracker<'a>(
    mgr: &'a PackageManager,
    package: &ocx_oci::PackageRef,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
    role: Role,
) -> Pin<Box<dyn Future<Output = Result<InstallInfo, PackageErrorKind>> + Send + 'a>> {
    let package = package.clone();
    Box::pin(async move { setup_impl(mgr, &package, platform, groups, None, role).await })
}

/// Singleflight-gated core of [`PackageManager::pull`]; `dest_override` is forwarded to [`setup_owned`].
///
/// The leader appends one activity entry, as `role`, when its setup succeeds; a waiter appends none.
async fn setup_impl(
    mgr: &PackageManager,
    package: &ocx_oci::PackageRef,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
    dest_override: Option<&std::path::Path>,
    role: Role,
) -> Result<InstallInfo, PackageErrorKind> {
    log::debug!("Pulling package: {}", package);

    let resolve_watch = Stopwatch::start(mgr.activity_log().is_some());
    let mut resolved = mgr.resolve(package, platform.clone()).await?;
    if groups.network_leaves.contains(&resolved.pinned.strip_advisory()) {
        resolved.leaf_origin = ocx_index::BlobOrigin::Network;
    }
    let resolve_time = resolve_watch.elapsed();
    let pinned = resolved.pinned.clone();
    log::debug!("Resolved package identifier: {}", &pinned);

    let handle = match groups
        .packages
        .try_acquire(pinned.strip_advisory())
        .await
        .map_err(map_singleflight_error)?
    {
        singleflight::Acquisition::Resolved(info) => {
            log::debug!("Package '{}' already set up by another task, reusing.", &pinned);
            // Alias tags share a leader by leaf digest, not a chain: link the waiter's own
            // chain too, or its distinct index blobs stay unreferenced for GC.
            super::common::stage_and_link_chain_blobs(
                mgr.file_structure(),
                mgr.index(),
                &info.dir().content(),
                &resolved,
            )
            .await?;
            return Ok(info);
        }
        singleflight::Acquisition::Leader(handle) => handle,
    };

    // Before any layer download, so a fail-closed abort leaves no partial install state.
    // Leader-only: waiters share the verified digest and receive a failure via `handle.fail`.
    let publisher_verified = match mgr.maybe_auto_verify_requested(pinned.as_identifier(), package).await {
        Ok(verified) => verified,
        Err(kind) => {
            let shared = handle.fail(kind);
            return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
        }
    };

    // Fail the handle explicitly: a dropped one tells waiters only "abandoned".
    let (installed, outcome) =
        match setup_owned_outcome(mgr, &pinned, resolved, platform, groups, dest_override, None).await {
            Ok(done) => done,
            Err(e) => {
                let shared = handle.fail(e);
                return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
            }
        };

    // Written last, on the placed content: a crash before it leaves content `find` re-verifies, never a marker
    // for content that was not verified. Content already in the store gets it too, which no fetch ever writes.
    if publisher_verified
        && dest_override.is_none()
        && let Err(error) = file_structure::record_verified(installed.dir(), pinned.as_identifier()).await
    {
        let shared = handle.fail(PackageErrorKind::Internal(error.into()));
        return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
    }
    if let Some(log) = mgr.activity_log() {
        log.record(outcome.into_activity(&pinned, &installed, role, resolve_time));
    }
    handle.complete(installed.clone());
    Ok(installed)
}

/// A wall-clock timer that costs nothing when no activity log is attached.
#[derive(Clone, Copy)]
struct Stopwatch(Option<Instant>);

impl Stopwatch {
    fn start(enabled: bool) -> Self {
        Self(enabled.then(Instant::now))
    }

    fn elapsed(self) -> Option<Duration> {
        self.0.map(|started| started.elapsed())
    }
}

/// What [`setup_owned_outcome`] observed, for the entry its singleflight leader records.
///
/// Timings are `None` when no activity log is attached, and the download and extract fields stay
/// empty for a store hit.
#[derive(Debug, Clone, Copy, Default)]
struct SetupOutcome {
    /// The package was already installed, so nothing was fetched or assembled.
    store_hit: bool,
    /// Descriptor sizes of the layers this setup fetched as layer leader that were not in the layer store.
    download_size: u64,
    /// Wall time of the whole layer phase, set only when this setup fetched at least one layer.
    download: Option<Duration>,
    /// Wall time from layer assembly through the move into the store; excludes dependency setup.
    extract: Option<Duration>,
}

impl SetupOutcome {
    fn store_hit() -> Self {
        Self {
            store_hit: true,
            ..Self::default()
        }
    }

    fn into_activity(
        self,
        pinned: &ocx_oci::PinnedPackageRef,
        installed: &InstallInfo,
        role: Role,
        resolve: Option<Duration>,
    ) -> PackageActivity {
        PackageActivity {
            pinned: pinned.clone(),
            platform: installed.platform().cloned(),
            action: Action::Pulled,
            role,
            store_hit: self.store_hit,
            download_size: self.download_size,
            resolve,
            download: self.download,
            extract: self.extract,
        }
    }
}

/// Downloads, sets up dependencies and places the package; call only while owning the singleflight handle.
///
/// Appends one activity entry as a root, without a resolve time, when a log is attached: a caller
/// that holds the handle itself (`pull_local`) names the package, so no resolve ran.
///
/// `dest_override: Some(path)` places the package at `path` instead of the content-addressed store.
/// `provided_metadata: Some(_)` is trusted as already validated and skips the registry fetch.
pub async fn setup_owned(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    resolved: super::resolve::ResolvedChain,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
    dest_override: Option<&std::path::Path>,
    provided_metadata: Option<metadata::Metadata>,
) -> Result<InstallInfo, PackageErrorKind> {
    let (info, outcome) = setup_owned_outcome(
        mgr,
        pinned,
        resolved,
        platform,
        groups,
        dest_override,
        provided_metadata,
    )
    .await?;
    if let Some(log) = mgr.activity_log() {
        log.record(outcome.into_activity(pinned, &info, Role::Root, None));
    }
    Ok(info)
}

/// A package's place in a vendor tree: its directory `p/<HEX16>` and the content directory `p/<HEX16>/c` inside it.
#[derive(Debug, Clone)]
pub(crate) struct VendorTarget {
    package_dir: std::path::PathBuf,
    content_dir: std::path::PathBuf,
}

impl VendorTarget {
    /// The package directory `key` names under `layout`, and its content directory.
    pub(crate) fn new(layout: &file_structure::VendorLayout, key: &file_structure::PackageKey) -> Self {
        Self {
            package_dir: layout.resolve(&file_structure::VendorLayout::relative_package_dir(key)),
            content_dir: layout.resolve(&file_structure::VendorLayout::relative_content_dir(key)),
        }
    }

    pub(crate) fn package_dir(&self) -> &std::path::Path {
        &self.package_dir
    }

    pub(crate) fn content_dir(&self) -> &std::path::Path {
        &self.content_dir
    }
}

/// Assembles `pinned`'s layers into `target`'s content directory `p/<HEX16>/c` and writes nothing else.
///
/// The layers are copied, not hardlinked, since the tree leaves the volume `$OCX_HOME` sits on. Its package
/// directory holds no `metadata.json`, `resolve.json`, `install.json`, `digest`, `refs/` or `launchers/`, and no
/// install symlink: the caller writes the direct launchers beside it in `p/<HEX16>/e/`. Dependencies are set up in
/// the store as for any pull; the caller vendors each package of the closure itself.
///
/// The returned [`InstallInfo`] is rooted at `target`'s package directory, so its `content()` names `content` there,
/// not `c`; nothing reads it.
///
/// # Errors
///
/// As [`setup_owned`].
pub(crate) async fn setup_vendored(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    resolved: super::resolve::ResolvedChain,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
    target: &VendorTarget,
    provided_metadata: Option<metadata::Metadata>,
) -> Result<InstallInfo, PackageErrorKind> {
    let manifest = resolved.final_manifest.clone();
    let metadata = load_package_metadata(mgr, pinned, &manifest, provided_metadata).await?;
    check_package_artifact_type(&manifest)?;

    let (layers, dependencies) = tokio::join!(
        extract_layers(
            mgr,
            pinned,
            &resolved.transport_pinned,
            &manifest,
            groups.layers.clone()
        ),
        setup_dependencies(mgr, &metadata, pinned, platform, groups.clone()),
    );
    let (layer_digests, dependencies) = (layers?.digests, dependencies?);

    tokio::fs::create_dir_all(target.package_dir())
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(target.package_dir().to_path_buf(), e)))?;
    assemble_layers(
        mgr.file_structure(),
        pinned,
        &layer_digests,
        &manifest,
        &metadata,
        target.content_dir(),
        ocx_store::hardlink::LinkMode::Copy,
    )
    .await?;

    let resolved_package = resolved_package_of(&metadata, &dependencies);
    let dir = file_structure::PackageDir::with_root(target.package_dir().to_path_buf());
    Ok(InstallInfo::new(pinned.clone(), metadata, resolved_package, dir).with_platform(resolved.platform))
}

/// Writes `launchers/<G>/` into the temp package directory `pkg`, each record naming `final_root`.
///
/// The package move publishes it; `entrypoints/` is never written.
///
/// # Errors
///
/// The host has no launcher blob (the error names the launcher path), or as [`crate::launcher::generate`].
async fn write_package_launchers(
    pkg: &file_structure::PackageDir,
    final_root: &std::path::Path,
    entrypoints: &ocx_package::metadata::entrypoint::Entrypoints,
) -> Result<(), crate::Error> {
    let generation = ocx_store::launcher_generation::LauncherGeneration::host().ok_or_else(|| {
        crate::error::file_error(
            pkg.launchers_root(),
            std::io::Error::other("no launcher blob for this host"),
        )
    })?;
    crate::launcher::generate(final_root, entrypoints, &pkg.launchers(&generation)).await
}

/// [`setup_owned`] plus what it observed.
async fn setup_owned_outcome(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    resolved: super::resolve::ResolvedChain,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
    dest_override: Option<&std::path::Path>,
    provided_metadata: Option<metadata::Metadata>,
) -> Result<(InstallInfo, SetupOutcome), PackageErrorKind> {
    // Stamped at this single return boundary so no early return in `setup_owned_impl` skips it;
    // the candidate-symlink gate reads the platform to suppress foreign-platform installs.
    let resolved_platform = resolved.platform.clone();
    let transport_registry = resolved
        .transport_pinned
        .as_ref()
        .ok()
        .map(|transport| transport.registry().to_string());
    setup_owned_impl(
        mgr,
        pinned,
        resolved,
        platform,
        groups,
        dest_override,
        provided_metadata,
    )
    .await
    .map(|(info, outcome)| {
        let info = info.with_platform(resolved_platform);
        let info = match transport_registry {
            Some(registry) => info.with_transport_registry(registry),
            None => info,
        };
        (info, outcome)
    })
}

async fn setup_owned_impl(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    resolved: super::resolve::ResolvedChain,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
    dest_override: Option<&std::path::Path>,
    provided_metadata: Option<metadata::Metadata>,
) -> Result<(InstallInfo, SetupOutcome), PackageErrorKind> {
    // Only `pull_local` supplies metadata, and it has no registry in the loop.
    let from_registry = provided_metadata.is_none();

    // Skipped under `dest_override`, or a store hit never reaches the override path.
    if dest_override.is_none()
        && let Some(info) = mgr.find_plain(pinned).await?
    {
        let install_path = mgr.file_structure().packages.install_status(pinned);
        if ocx_package::install_status::check_install_status(&install_path).await {
            log::debug!("Package '{}' already fully installed, skipping.", pinned);
            // Same alias-tag chain top-up as the waiter branch in `setup_impl`.
            super::common::stage_and_link_chain_blobs(
                mgr.file_structure(),
                mgr.index(),
                &info.dir().content(),
                &resolved,
            )
            .await?;
            return Ok((info, SetupOutcome::store_hit()));
        }
        log::debug!(
            "Package '{}' present in object store but install status not OK, re-pulling.",
            pinned
        );
    }

    let lock_timeout = mgr
        .client()
        .map(|c| c.lock_timeout())
        .unwrap_or(PULL_LOCAL_LOCK_TIMEOUT);
    let temp = acquire_temp_dir(mgr.file_structure(), pinned, lock_timeout).await?;

    if dest_override.is_none()
        && let Some(info) = mgr.find_plain(pinned).await?
    {
        let install_path = mgr.file_structure().packages.install_status(pinned);
        if ocx_package::install_status::check_install_status(&install_path).await {
            log::debug!(
                "Package '{}' installed by another process while waiting for lock, skipping.",
                pinned
            );
            super::common::stage_and_link_chain_blobs(
                mgr.file_structure(),
                mgr.index(),
                &info.dir().content(),
                &resolved,
            )
            .await?;
            return Ok((info, SetupOutcome::store_hit()));
        }
    }

    let manifest = resolved.final_manifest.clone();
    let metadata = load_package_metadata(mgr, pinned, &manifest, provided_metadata).await?;
    check_package_artifact_type(&manifest)?;

    let pkg = file_structure::PackageDir::with_root(temp.dir.dir.clone());

    // `consent::verified_sources` trusts this record, so `pull_local` must never write one,
    // or unverified content passes as verified.
    // A home, blob-directory or cache hit proves nothing about the registry, so it mints no origin.
    if from_registry && resolved.leaf_origin == ocx_index::BlobOrigin::Network {
        // `pinned`, not the transport address, or consent pins to routing (`adr_lock_records_physical_address.md`).
        file_structure::record_origin(&pkg, pinned.as_identifier())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    }

    manifest
        .write_json(pkg.manifest())
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    let fs = mgr.file_structure();

    let timed = mgr.activity_log().is_some();
    let (layers, dependencies) = tokio::join!(
        async {
            let watch = Stopwatch::start(timed);
            let phase = extract_layers(
                mgr,
                pinned,
                &resolved.transport_pinned,
                &manifest,
                groups.layers.clone(),
            )
            .await;
            (phase, watch.elapsed())
        },
        setup_dependencies(mgr, &metadata, pinned, platform, groups.clone()),
    );
    let (layer_phase, layer_phase_time) = layers;
    let (layer_phase, dependencies) = (layer_phase?, dependencies?);
    let layer_digests = layer_phase.digests;

    metadata
        .write_json(pkg.metadata())
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    // Forward-refs before assembly hardlinks into the layers, or `ocx clean` sweeps a layer mid-assembly.
    let layers_dir = pkg.refs_layers_dir();
    tokio::fs::create_dir_all(&layers_dir)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(layers_dir.clone(), e)))?;
    link_layers_in_temp(&pkg, pinned.registry(), &layer_digests, fs)?;

    let extract_watch = Stopwatch::start(timed);
    assemble_layers(
        fs,
        pinned,
        &layer_digests,
        &manifest,
        &metadata,
        &pkg.content(),
        ocx_store::hardlink::LinkMode::Hardlink,
    )
    .await?;

    if let Some(entrypoints) = metadata.entrypoints()
        && !entrypoints.is_empty()
    {
        // The post-move root, not the temp path, or every launcher breaks after the rename.
        let final_pkg_root = dest_override
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| fs.packages.path(pinned));
        write_package_launchers(&pkg, &final_pkg_root, entrypoints)
            .await
            .map_err(PackageErrorKind::Internal)?;
    }

    let resolved_package = resolved_package_of(&metadata, &dependencies);

    // Before `resolve.json` is written, so a duplicate interface launcher name never reaches disk.
    {
        let root_info = Arc::new(InstallInfo::new(
            pinned.clone(),
            metadata.clone(),
            resolved_package.clone(),
            pkg.clone(),
        ));
        // Private-surface duplicates are tolerated: PATH order resolves them (`adr_two_env_composition.md`).
        composer::check_entrypoints(std::slice::from_ref(&root_info), &fs.packages).await?;
    }

    post_download_actions(&pkg, pinned, &resolved_package).await?;

    // Refs go in before the move, so the package never exists without `refs/`.
    if !dependencies.is_empty() {
        let deps_dir = pkg.refs_deps_dir();
        tokio::fs::create_dir_all(&deps_dir)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(deps_dir.clone(), e)))?;
    }
    link_dependencies_in_temp(&pkg, &dependencies)?;
    // Leaf manifests live only in the blob store (`adr_index_indirection.md`); unlinked, GC cannot
    // reach the chain from the installed package.
    super::common::stage_and_link_chain_blobs(fs, mgr.index(), &pkg.content(), &resolved).await?;

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
    let outcome = SetupOutcome {
        store_hit: false,
        download_size: layer_phase.fetched_bytes,
        download: layer_phase.fetched_any.then_some(layer_phase_time).flatten(),
        extract: extract_watch.elapsed(),
    };
    Ok((install_info, outcome))
}

/// Relies on `setup_dependencies` returning declaration order, to zip declarations with their installs.
fn resolved_package_of(metadata: &metadata::Metadata, dependencies: &[Arc<InstallInfo>]) -> ResolvedPackage {
    ResolvedPackage::new().with_dependencies(
        metadata
            .dependencies()
            .iter()
            .zip(dependencies.iter())
            .map(|(decl, info)| (info.identifier().clone(), info.resolved().clone(), decl.visibility)),
    )
}

/// The package's metadata: `provided`, which is trusted as validated, or the config blob of `manifest`.
async fn load_package_metadata(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    manifest: &ocx_oci::ImageManifest,
    provided: Option<metadata::Metadata>,
) -> Result<metadata::Metadata, PackageErrorKind> {
    match provided {
        Some(metadata) => Ok(metadata),
        None => Ok(super::common::load_config_metadata(mgr.index(), pinned, manifest)
            .await?
            .into()),
    }
}

/// Zero layers is valid: a config-only package with an empty `content/`.
fn check_package_artifact_type(manifest: &ocx_oci::ImageManifest) -> Result<(), PackageErrorKind> {
    if manifest.artifact_type.as_deref() != Some(MEDIA_TYPE_PACKAGE_V1) {
        return Err(PackageErrorKind::from(
            ocx_oci::client::error::ClientError::UnexpectedArtifactType {
                expected: MEDIA_TYPE_PACKAGE_V1.to_string(),
                actual: manifest.artifact_type.clone(),
            },
        ));
    }
    Ok(())
}

/// Assembles the extracted layers into `content_dir`; `placements` aligns 1:1 with the layer
/// contents only because `extract_layers` returns manifest order.
async fn assemble_layers(
    fs: &file_structure::FileStructure,
    pinned: &ocx_oci::PinnedPackageRef,
    layer_digests: &[ocx_oci::Digest],
    manifest: &ocx_oci::ImageManifest,
    metadata: &metadata::Metadata,
    content_dir: &std::path::Path,
    link_mode: ocx_store::hardlink::LinkMode,
) -> Result<(), PackageErrorKind> {
    let layer_contents: Vec<std::path::PathBuf> = layer_digests
        .iter()
        .map(|digest| fs.layers.content(pinned.registry(), digest))
        .collect();
    let sources: Vec<&std::path::Path> = layer_contents.iter().map(AsRef::as_ref).collect();
    let placements = layer_placements(manifest, metadata)?;
    ocx_store::file_structure::assemble_from_layers_with_layouts(&sources, &placements, content_dir, link_mode)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    Ok(())
}

/// One placement per layer, aligned with `manifest.layers`; layers are stored unstripped, so placement applies at assembly.
fn layer_placements(
    manifest: &ocx_oci::ImageManifest,
    metadata: &metadata::Metadata,
) -> Result<Vec<ocx_util::fs::path::LayerPlacement>, PackageErrorKind> {
    let bundle_strip = metadata.strip_components();
    manifest
        .layers
        .iter()
        .map(|layer| {
            ocx_oci::resolve_layer_placement(layer.annotations.as_ref(), bundle_strip)
                .map_err(|e| PackageErrorKind::Internal(crate::Error::LayerLayout(e)))
        })
        .collect()
}

/// Acquires the exclusive temp directory for `identifier`, waiting up to `lock_timeout`
/// while another process holds it.
pub async fn acquire_temp_dir(
    fs: &file_structure::FileStructure,
    identifier: &ocx_oci::PinnedPackageRef,
    lock_timeout: Duration,
) -> Result<ocx_store::file_structure::TempAcquireResult, PackageErrorKind> {
    let temp_path = fs
        .temp
        .path(identifier)
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    let acquire = match fs
        .temp
        .try_acquire(&temp_path)
        .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        Some(r) => r,
        None => {
            log::debug!("Temp dir locked by another process, waiting: {}", temp_path.display());
            fs.temp
                .acquire_with_timeout(&temp_path, lock_timeout)
                .await
                .map_err(|error| PackageErrorKind::Internal(error.into()))?
        }
    };
    if acquire.was_cleaned {
        log::debug!("Cleaned previous temp data at {}", temp_path.display());
    }
    Ok(acquire)
}

/// Sets up dependencies in parallel, returning results in declaration order.
async fn setup_dependencies(
    mgr: &PackageManager,
    metadata: &ocx_package::metadata::Metadata,
    parent: &ocx_oci::PinnedPackageRef,
    platform: ocx_oci::Platform,
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
        let platform = platform.clone();
        let groups = groups.clone();
        tasks.spawn(async move {
            let info = setup_with_tracker(&mgr, &dep_id, platform, groups, Role::Dependency).await?;
            Ok::<_, PackageErrorKind>((idx, Arc::new(info)))
        });
    }

    let mut results: Vec<Option<Arc<InstallInfo>>> = vec![None; deps.len()];
    while let Some(join_result) = tasks.join_next().await {
        let (idx, info) = match join_result {
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => panic!("dependency setup task aborted: {e}"),
            Ok(v) => v,
        }?;
        results[idx] = Some(info);
    }

    Ok(results.into_iter().flatten().collect())
}

/// Writes `resolve.json`, the `install.json` completion sentinel, and the `digest` file
/// that recovers the full digest from the truncated CAS path.
async fn post_download_actions(
    pkg: &file_structure::PackageDir,
    pinned: &ocx_oci::PinnedPackageRef,
    resolved: &ResolvedPackage,
) -> Result<(), PackageErrorKind> {
    // Keeps each dependency's advisory tag; only `ocx.lock` strips it
    // (`adr_project_toolchain_config.md` § Amendment B).
    resolved
        .write_json(pkg.resolve())
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    // Through the exclusive lock, or a shared-lock `check_install_status` reader sees partial JSON.
    {
        let mut locked = ocx_util::fs::LockedJsonFile::<InstallStatus>::open_exclusive(pkg.install_status())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
        locked
            .write(&InstallStatus::new().ok())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    }

    file_structure::write_digest_file(&pkg.digest_file(), &pinned.digest())
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    Ok(())
}

/// Moves the temp directory into the object store, or to `dest_override`; the returned
/// [`InstallInfo`] is rooted at the chosen destination.
async fn move_temp_to_object_store(
    fs: &file_structure::FileStructure,
    identifier: &ocx_oci::PinnedPackageRef,
    metadata: &metadata::Metadata,
    resolved: ResolvedPackage,
    temp: ocx_store::file_structure::TempAcquireResult,
    dest_override: Option<&std::path::Path>,
) -> Result<InstallInfo, PackageErrorKind> {
    let output_path = dest_override
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| fs.packages.path(identifier));
    let temp_path = temp.dir.dir.clone();
    let pkg = file_structure::PackageDir::with_root(output_path.clone());

    ocx_util::fs::move_dir(&temp_path, &output_path)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    drop(temp);

    Ok(InstallInfo::new(identifier.clone(), metadata.clone(), resolved, pkg))
}

/// Extracts a manifest's layers in parallel, returning their digests in manifest order.
async fn extract_layers(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    transport: &Result<ocx_oci::PinnedOciIdentifier, NoTransport>,
    manifest: &ocx_oci::ImageManifest,
    layer_group: LayerGroup,
) -> Result<LayerPhase, PackageErrorKind> {
    let mut parsed = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let digest: ocx_oci::Digest = layer
            .digest
            .clone()
            .try_into()
            .map_err(|e: ocx_oci::digest::error::DigestError| PackageErrorKind::Internal(e.into()))?;
        parsed.push((layer.clone(), digest));
    }

    // Run lazily by the first layer that must dial, not hoisted, so an all-cached pull never
    // resolves the physical host.
    let dial_guard: Arc<tokio::sync::OnceCell<DialVerdict>> = Arc::new(tokio::sync::OnceCell::new());

    let mut tasks: JoinSet<(usize, Result<LayerExtraction, PackageErrorKind>)> = JoinSet::new();
    for (idx, (layer, digest)) in parsed.into_iter().enumerate() {
        let mgr = mgr.clone();
        let pinned = pinned.clone();
        let transport = transport.clone();
        let layer_group = layer_group.clone();
        let dial_guard = dial_guard.clone();
        tasks.spawn(async move {
            let res = extract_layer_atomic(
                &mgr,
                &pinned,
                transport.as_ref(),
                &layer,
                &digest,
                layer_group,
                &dial_guard,
            )
            .await;
            (idx, res)
        });
    }

    let mut results: Vec<Option<ocx_oci::Digest>> = vec![None; tasks.len()];
    let mut phase = LayerPhase::default();
    while let Some(join_res) = tasks.join_next().await {
        let (idx, task_res) = match join_res {
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => panic!("layer extraction task aborted: {e}"),
            Ok(v) => v,
        };
        let extraction = task_res?;
        if let Some(bytes) = extraction.fetched_bytes {
            phase.fetched_any = true;
            phase.fetched_bytes += bytes;
        }
        results[idx] = Some(extraction.digest);
    }
    phase.digests = results.into_iter().flatten().collect();
    Ok(phase)
}

/// What one package's layers came to: the digests in manifest order, and what this package fetched itself.
#[derive(Debug, Default)]
struct LayerPhase {
    digests: Vec<ocx_oci::Digest>,
    /// Descriptor sizes of the layers this package fetched as layer leader that were not in the layer store.
    fetched_bytes: u64,
    /// At least one layer was fetched by this package, which is what makes the phase a download.
    fetched_any: bool,
}

/// One layer's digest and, when this task fetched it, the bytes fetched.
struct LayerExtraction {
    digest: ocx_oci::Digest,
    fetched_bytes: Option<u64>,
}

impl LayerExtraction {
    /// Already extracted or fetched by another task: nothing came over the wire for this package.
    fn reused(digest: &ocx_oci::Digest) -> Self {
        Self {
            digest: digest.clone(),
            fetched_bytes: None,
        }
    }
}

/// Memoized SSRF pre-flight for one pull. The whole `Result` is cached, not just `Ok`:
/// re-checking per layer lets a DNS-rebinding host pass on any single lookup.
type DialVerdict = Result<(), DialRefusal>;

/// A dial refusal shared across layer tasks; `source()` exposes the original so
/// `classify_error` still reports the `SsrfError`'s exit code.
#[derive(Debug, Clone)]
struct DialRefusal(Arc<crate::Error>);

impl std::fmt::Display for DialRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for DialRefusal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

/// Atomically extracts one layer into `layers/{digest}/`, deduped in-process by the layer group.
///
/// Without a `transport`, a missing layer is refused ([`NoTransport::missing_layer`]), never dialled.
async fn extract_layer_atomic(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    transport: Result<&ocx_oci::PinnedOciIdentifier, &NoTransport>,
    layer: &ocx_oci::Descriptor,
    layer_digest: &ocx_oci::Digest,
    layer_group: LayerGroup,
    dial_guard: &tokio::sync::OnceCell<DialVerdict>,
) -> Result<LayerExtraction, PackageErrorKind> {
    let fs = mgr.file_structure();
    let registry = pinned.registry().to_string();

    let key = (registry.clone(), layer_digest.clone());
    let handle = match layer_group.try_acquire(key).await.map_err(map_singleflight_error)? {
        singleflight::Acquisition::Resolved(()) => {
            log::debug!("Layer {} already extracted by another task, reusing.", layer_digest);
            return Ok(LayerExtraction::reused(layer_digest));
        }
        singleflight::Acquisition::Leader(handle) => handle,
    };

    // Before the client is required, so an offline manager can re-assemble from cached layers.
    let layer_content = fs.layers.content(pinned.registry(), layer_digest);
    if ocx_util::fs::path_exists_lossy(&layer_content).await {
        log::debug!("Layer {} present on disk, skipping fetch.", layer_digest);
        handle.complete(());
        return Ok(LayerExtraction::reused(layer_digest));
    }

    // After the home, before any client is required: a blob directory or the cache serves an offline pull.
    let mut miss = None;
    if !mgr.blob_lookup().is_empty() {
        match extract_layer_from_lookup(mgr, pinned, layer, layer_digest).await {
            Ok(LookupOutcome::Extracted) => {
                handle.complete(());
                return Ok(LayerExtraction::reused(layer_digest));
            }
            Ok(LookupOutcome::Miss(found)) => miss = Some(found),
            Err(e) => {
                let shared = handle.fail(e);
                return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
            }
        }
    }

    if mgr
        .client()
        .is_some_and(|c| c.blob_overlay_mode() == Some(ocx_oci::client::OverlayMode::LookupOnly))
    {
        let kind = end_of_sources(miss, |_| export_lacks_layer(mgr, pinned, layer_digest));
        let shared = handle.fail(kind);
        return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
    }

    let transport = match transport {
        Ok(transport) => transport,
        Err(absent) => {
            let kind = end_of_sources(miss, |_| {
                PackageErrorKind::Internal(absent.missing_layer(pinned, layer_digest))
            });
            let shared = handle.fail(kind);
            return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
        }
    };

    let client = match mgr.require_client() {
        Ok(c) => c,
        Err(e) => {
            let kind = end_of_sources(miss, |discarded| {
                if discarded > 0 {
                    log::warn!(
                        "{discarded} cached blob(s) failed verification and were discarded; the pull is offline"
                    );
                }
                PackageErrorKind::Internal(e)
            });
            let shared = handle.fail(kind);
            return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
        }
    };

    // The only SSRF check before dialling an index-rewritten target: `client` has none, and
    // resolve-time routing tolerated lookup failures.
    if let Err(refusal) = dial_guard
        .get_or_init(|| async {
            mgr.index()
                .guard_physical_dial(pinned.as_identifier(), transport.as_oci_identifier())
                .await
                .map_err(|error| DialRefusal(Arc::new(error.into())))
        })
        .await
    {
        let shared = handle.fail(refusal.clone());
        return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
    }

    match extract_layer_inner(pinned, transport, layer, layer_digest, client, fs).await {
        Ok(pulled) => {
            handle.complete(());
            Ok(LayerExtraction {
                digest: layer_digest.clone(),
                // A successful pull read exactly the descriptor size (digest-verified).
                fetched_bytes: pulled.then(|| u64::try_from(layer.size).unwrap_or(0)),
            })
        }
        Err(e) => {
            // A refused or failed network step must not hide the bad copy that made it necessary.
            let kind = end_of_sources(miss, |_| e);
            let shared = handle.fail(kind);
            Err(map_singleflight_error(singleflight::Error::Failed(shared)))
        }
    }
}

/// What the blob directories and the cache came to for one layer.
enum LookupOutcome {
    /// The layer is in `layers/`, extracted from a verified local copy.
    Extracted,
    Miss(LookupMiss),
}

/// Why the lookup did not serve a layer.
struct LookupMiss {
    /// A blob directory held a copy that failed verification; the verdict stays 65 whatever follows.
    bad_copy: Option<ocx_oci::blob_lookup::LookupError>,
    /// Cache entries removed after failing verification.
    discarded: usize,
}

/// The error for a layer no source served: a remembered bad directory copy beats `otherwise`.
fn end_of_sources(miss: Option<LookupMiss>, otherwise: impl FnOnce(usize) -> PackageErrorKind) -> PackageErrorKind {
    match miss {
        Some(LookupMiss {
            bad_copy: Some(bad), ..
        }) => PackageErrorKind::Internal(ocx_index::error::Error::from(bad).into()),
        Some(LookupMiss { discarded, .. }) => otherwise(discarded),
        None => otherwise(0),
    }
}

/// An import's lookup is its export directory, so a layer nothing holds is missing from the export.
fn export_lacks_layer(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    layer_digest: &ocx_oci::Digest,
) -> PackageErrorKind {
    let dir = mgr
        .blob_lookup()
        .dirs()
        .first()
        .map_or_else(std::path::PathBuf::new, |blobs| {
            blobs.parent().unwrap_or(blobs).to_path_buf()
        });
    PackageErrorKind::Internal(crate::Error::ExportNotFound {
        dir,
        what: format!("layer {layer_digest} of {pinned}"),
    })
}

/// Names the serving origin the way the debug log reports it.
fn origin_label(origin: ocx_oci::blob_lookup::LookupOrigin) -> String {
    match origin {
        ocx_oci::blob_lookup::LookupOrigin::Dir(position) => format!("dir[{position}]"),
        ocx_oci::blob_lookup::LookupOrigin::Cache => "cache".to_string(),
    }
}

/// Removes what a failed extraction left, so the next source starts in an empty output directory.
async fn reset_layer_output(output_dir: &std::path::Path) -> Result<(), PackageErrorKind> {
    match tokio::fs::remove_dir_all(output_dir.join("content")).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PackageErrorKind::Internal(
            ocx_oci::client::error::ClientError::internal(error).into(),
        )),
    }
}

/// Offers the layer's blob-directory and cache copies in lookup order and extracts the first that verifies.
///
/// A bad directory copy is remembered and the walk goes on; a bad cache entry is removed and is only a miss.
async fn extract_layer_from_lookup(
    mgr: &PackageManager,
    pinned: &ocx_oci::PinnedPackageRef,
    layer: &ocx_oci::Descriptor,
    layer_digest: &ocx_oci::Digest,
) -> Result<LookupOutcome, PackageErrorKind> {
    let lookup_error = |error: ocx_oci::blob_lookup::LookupError| {
        PackageErrorKind::Internal(ocx_index::error::Error::from(error).into())
    };
    let fs = mgr.file_structure();
    let mut walk = mgr.blob_lookup().walk(pinned.registry(), layer_digest);
    let Some(mut stream) = walk.next_source().await.map_err(lookup_error)? else {
        return Ok(LookupOutcome::Miss(lookup_miss(&walk)));
    };

    // Extraction never dials, so an offline manager borrows a client that has no transport built yet.
    let offline_client;
    let client = match mgr.client() {
        Some(client) => client,
        None => {
            offline_client = ocx_oci::ClientBuilder::new().build();
            &offline_client
        }
    };

    let temp_path = fs.temp.layer_path(pinned.registry(), layer_digest);
    let temp = match fs
        .temp
        .try_acquire(&temp_path)
        .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        Some(r) => r,
        None => fs
            .temp
            .acquire_with_timeout(&temp_path, client.lock_timeout())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?,
    };

    let layer_content = fs.layers.content(pinned.registry(), layer_digest);
    if ocx_util::fs::path_exists_lossy(&layer_content).await {
        return Ok(LookupOutcome::Extracted);
    }

    loop {
        let origin = stream.origin;
        reset_layer_output(&temp.dir.dir).await?;
        match client.extract_layer_from_stream(stream, layer, &temp.dir.dir).await {
            Ok(()) => {
                log::debug!("Layer {} served from {}", layer_digest, origin_label(origin));
                finish_layer(fs, pinned, layer_digest, &temp.dir.dir).await?;
                drop(temp);
                return Ok(LookupOutcome::Extracted);
            }
            // A copy that decompresses past the cap is as unusable as a wrong one: try the next source.
            Err(
                ocx_oci::client::error::ClientError::DigestMismatch { .. }
                | ocx_oci::client::error::ClientError::DecompressionCapExceeded { .. },
            ) => walk.reject(origin).await,
            Err(other) => return Err(other.into()),
        }
        match walk.next_source().await.map_err(lookup_error)? {
            Some(next) => stream = next,
            None => break,
        }
    }
    reset_layer_output(&temp.dir.dir).await?;
    Ok(LookupOutcome::Miss(lookup_miss(&walk)))
}

fn lookup_miss(walk: &ocx_oci::blob_lookup::LookupWalk<'_>) -> LookupMiss {
    match walk.exhausted() {
        Ok(end) => LookupMiss {
            bad_copy: None,
            discarded: end.discarded_cache_entries,
        },
        Err(bad) => LookupMiss {
            bad_copy: Some(bad),
            discarded: 0,
        },
    }
}

/// Signs, marks and publishes an extracted layer directory into `layers/`.
async fn finish_layer(
    fs: &file_structure::FileStructure,
    pinned: &ocx_oci::PinnedPackageRef,
    layer_digest: &ocx_oci::Digest,
    output_dir: &std::path::Path,
) -> Result<(), PackageErrorKind> {
    // Signed before anything reads `content/`: nothing may link an unsigned Mach-O out of the layer store.
    ocx_store::codesign::sign_extracted_content(&output_dir.join("content"))
        .await
        .map_err(ocx_oci::client::error::ClientError::internal)?;

    file_structure::write_digest_file(&output_dir.join(file_structure::DIGEST_FILENAME), layer_digest)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    super::layer_staging::finalize_layer_dir(fs, pinned.registry(), layer_digest, output_dir).await?;
    Ok(())
}

/// Inner extraction implementation — runs only for the leader task.
///
/// Returns whether this call pulled the layer; `false` when it found the layer already extracted.
async fn extract_layer_inner(
    pinned: &ocx_oci::PinnedPackageRef,
    transport: &ocx_oci::PinnedOciIdentifier,
    layer: &ocx_oci::Descriptor,
    layer_digest: &ocx_oci::Digest,
    client: &ocx_oci::Client,
    fs: &file_structure::FileStructure,
) -> Result<bool, PackageErrorKind> {
    let layer_content = fs.layers.content(pinned.registry(), layer_digest);
    if ocx_util::fs::path_exists_lossy(&layer_content).await {
        log::debug!("Layer {} already present on disk, skipping.", layer_digest);
        return Ok(false);
    }

    let temp_path = fs.temp.layer_path(pinned.registry(), layer_digest);
    let temp = match fs
        .temp
        .try_acquire(&temp_path)
        .map_err(|error| PackageErrorKind::Internal(error.into()))?
    {
        Some(r) => r,
        None => {
            log::debug!(
                "Layer temp dir locked by another process, waiting: {}",
                temp_path.display()
            );
            fs.temp
                .acquire_with_timeout(&temp_path, client.lock_timeout())
                .await
                .map_err(|error| PackageErrorKind::Internal(error.into()))?
        }
    };

    if ocx_util::fs::path_exists_lossy(&layer_content).await {
        log::debug!(
            "Layer {} installed by another process while waiting for lock, skipping.",
            layer_digest
        );
        return Ok(false);
    }

    // Fetched from the physical `transport`, but storage paths stay keyed on the logical
    // `pinned` (`adr_index_indirection.md`).
    client
        .pull_layer(transport, layer, &temp.dir.dir, Some(pinned.registry()))
        .await?;

    finish_layer(fs, pinned, layer_digest, &temp.dir.dir).await?;

    drop(temp);
    Ok(true)
}

/// Creates `refs/deps/` symlinks in the temp directory; targets are absolute store paths, so they survive the move.
///
/// Every dependency is linked regardless of visibility, which gates env composition, never GC.
/// The caller must pre-create `pkg.refs_deps_dir()`.
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
        let dep_digest = info.identifier().digest();
        let name = ocx_store::file_structure::cas_ref_name(&dep_digest);
        let link_path = deps_dir.join(name);
        ocx_util::fs::symlink::create(info.dir().content(), &link_path)
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    }
    Ok(())
}

/// Creates `refs/layers/` symlinks in the temp directory, each targeting a layer's `content/`:
/// GC's `read_refs` takes `.parent()` of the target to recover the layer entry.
///
/// The caller must pre-create `pkg.refs_layers_dir()`.
#[allow(clippy::result_large_err)]
fn link_layers_in_temp(
    pkg: &file_structure::PackageDir,
    registry: &str,
    layer_digests: &[ocx_oci::Digest],
    fs: &file_structure::FileStructure,
) -> Result<(), PackageErrorKind> {
    if layer_digests.is_empty() {
        return Ok(());
    }
    let layers_dir = pkg.refs_layers_dir();
    for digest in layer_digests {
        let layer_content = fs.layers.content(registry, digest);
        let name = ocx_store::file_structure::cas_ref_name(digest);
        let link_path = layers_dir.join(name);
        ocx_util::fs::symlink::create(&layer_content, &link_path)
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{NoTransport, SetupGroups, extract_layers, setup_owned};
    use crate::{
        PackageManager,
        error::PackageErrorKind,
        tasks::resolve::{ChainBlob, ChainRole, ResolvedChain},
    };
    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex, test_source::TrustingSource};
    use ocx_oci::{client::test_transport::StubTransport, client::test_transport::StubTransportData};
    use ocx_package::{
        install_info::InstallInfo,
        metadata::{
            Metadata,
            bundle::{Bundle, Version},
            dependency::Dependencies,
            entrypoint::Entrypoints,
            env::Env,
        },
    };
    use ocx_store::file_structure::FileStructure;

    /// A pull writes `launchers/<G>/` recording the post-move root, and nothing under `entrypoints/`.
    #[tokio::test]
    async fn pull_writes_launchers_with_the_final_root_and_no_entrypoints() {
        use ocx_store::launcher_generation::LauncherGeneration;
        let tmp = tempfile::tempdir().unwrap();
        let temp_pkg = tmp.path().join("temp-pkg");
        let final_root = tmp.path().join("packages").join("final");
        std::fs::create_dir_all(&temp_pkg).unwrap();
        let pkg = ocx_store::file_structure::PackageDir::with_root(temp_pkg.clone());

        super::write_package_launchers(&pkg, &final_root, &Entrypoints::from_names(["cmake"]))
            .await
            .unwrap();

        let generation = LauncherGeneration::host().unwrap();
        let record = std::fs::read(
            temp_pkg
                .join("launchers")
                .join(generation.as_str())
                .join("cmake.launch"),
        )
        .unwrap();
        let record: ocx_env_apply::launch::PackageRecord = serde_json::from_slice(&record).unwrap();
        assert_eq!(record.package_root, final_root.to_str().unwrap());
        assert!(!temp_pkg.join("entrypoints").exists());
    }

    /// Drives `setup_owned` against a leaf that is a perfectly valid OCI
    /// artifact but not an OCX package, and returns the refusal.
    ///
    /// `provided_metadata` selects which of the two gates trips first, and the
    /// two differ in more than sequencing:
    ///
    /// - `Some(..)` short-circuits the config-blob fetch, so the manifest
    ///   `artifactType` gate is first.
    /// - `None` — the shape `ocx install` / `ocx pull` actually take — reaches
    ///   `load_config_metadata`, whose config media-type gate fires *before*
    ///   the artifact-type gate ever runs.
    ///
    /// Both must exit 65. Covering only the `Some(..)` shape certifies a fix
    /// on a path the real install never walks.
    async fn refuse_foreign_leaf_artifact(provided_metadata: Option<Metadata>) -> PackageErrorKind {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        // No client: nothing in this path may reach the registry.
        let manager = PackageManager::new(file_structure, index, None, "example.com");

        let digest = ocx_oci::Digest::Sha256("d".repeat(64));
        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("test/foreign", "example.com")
                .clone_with_tag("1.0.0")
                .clone_with_digest(digest.clone()),
        )
        .expect("pinned identifier");

        let platform = ocx_oci::Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: Vec::new(),
        };

        // A leaf manifest that is a perfectly valid OCI artifact — just not an
        // OCX package.
        let final_manifest = ocx_oci::ImageManifest {
            artifact_type: Some("application/vnd.example.other.v1".to_string()),
            config: ocx_oci::Descriptor {
                media_type: "application/vnd.example.other.config.v1+json".to_string(),
                digest: digest.to_string(),
                size: 2,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            ..Default::default()
        };

        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: Ok(ocx_oci::OciIdentifier::passthrough(pinned.as_identifier()).at_pin_of(&pinned)),
            chain: vec![ChainBlob {
                identifier: pinned.clone(),
                role: ChainRole::Manifest,
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                size: 2,
            }],
            final_manifest,
            platform: platform.clone(),
            leaf_origin: ocx_index::BlobOrigin::Network,
        };

        setup_owned(
            &manager,
            &pinned,
            resolved,
            platform,
            SetupGroups::new(),
            None,
            provided_metadata,
        )
        .await
        .expect_err("a foreign leaf artifact type must be refused")
    }

    /// A local materialization carries no transport, so a layer absent from
    /// the store is refused before anything is dialled — never fetched from
    /// the name as typed. The manager is online on purpose: without the
    /// refusal, nothing else would stop the fetch.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_layer_without_a_transport_is_refused_before_any_dial() {
        use ocx_oci::client::test_transport::{StubTransport, StubTransportData};

        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ChainMode::Default,
        );
        let stub_data = StubTransportData::new();
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));
        let manager = PackageManager::new(file_structure, index, Some(client), "example.com");

        let layer_digest = ocx_oci::Digest::Sha256("e".repeat(64));
        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("test/local", "example.com")
                .clone_with_digest(ocx_oci::Digest::Sha256("d".repeat(64))),
        )
        .expect("pinned identifier");
        let manifest = ocx_oci::ImageManifest {
            layers: vec![ocx_oci::Descriptor {
                media_type: ocx_oci::media_type::MEDIA_TYPE_TAR_XZ.to_string(),
                digest: layer_digest.to_string(),
                size: 1,
                urls: None,
                artifact_type: None,
                annotations: None,
            }],
            ..Default::default()
        };

        let error = extract_layers(
            &manager,
            &pinned,
            &Err(NoTransport::LocalMaterialization),
            &manifest,
            SetupGroups::new().layers,
        )
        .await
        .expect_err("an unstaged layer with no transport must be refused");
        let mut rendered = error.to_string();
        let mut cause = std::error::Error::source(&error);
        while let Some(current) = cause {
            rendered.push_str(&format!(": {current}"));
            cause = current.source();
        }
        assert!(
            rendered.contains("is not staged locally") && rendered.contains(&layer_digest.to_string()),
            "{rendered}"
        );
        assert!(
            stub_data.read().calls.is_empty(),
            "nothing may be dialled: {:?}",
            stub_data.read().calls
        );
    }

    // ── offline: an index-owned pin with no recorded location ────────────────
    //
    // `ocx --remote install ocx.sh/x@sha256:…` writes no root, so a later
    // `ocx --offline install` of the same pin has a chain in the blob store and
    // nothing that says where the package lives. The refusal belongs to the
    // layer that would need that answer, not to routing.

    /// Pulls `ocx.sh/acme/tool@<digest>` offline, with `ocx.sh` owned by an
    /// index in config, no root committed, and the manifest + config blob in
    /// the blob store. `missing_layer` adds one layer the store does not hold.
    /// The manager carries a stub client so a dial would be recorded.
    async fn pull_unrecorded_index_owned_pin(
        missing_layer: Option<&ocx_oci::Digest>,
    ) -> (Result<InstallInfo, PackageErrorKind>, StubTransportData) {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained_with_content_store(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::machine_local(&file_structure),
            })
            .with_index_namespaces(std::collections::HashSet::from(["ocx.sh".to_string()])),
            Vec::new(),
            ChainMode::Offline,
            file_structure.blobs.clone(),
        );
        let stub_data = StubTransportData::new();
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));
        let manager = PackageManager::new(file_structure.clone(), index, Some(client), "ocx.sh");

        let config = serde_json::to_vec(&bundle_metadata()).unwrap();
        let config_digest = ocx_oci::Algorithm::Sha256.hash(&config);
        let layers: Vec<serde_json::Value> = missing_layer
            .iter()
            .map(|digest| {
                serde_json::json!({
                    "mediaType": ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
                    "digest": digest.to_string(),
                    "size": 1,
                })
            })
            .collect();
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": ocx_oci::OCI_IMAGE_MEDIA_TYPE,
            "artifactType": super::MEDIA_TYPE_PACKAGE_V1,
            "config": {
                "mediaType": ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1,
                "digest": config_digest.to_string(),
                "size": config.len(),
            },
            "layers": layers,
        }))
        .unwrap();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(&manifest);
        file_structure
            .blobs
            .write_blob("ocx.sh", &manifest_digest, &manifest)
            .await
            .unwrap();
        file_structure
            .blobs
            .write_blob("ocx.sh", &config_digest, &config)
            .await
            .unwrap();

        let package = ocx_oci::PackageRef::new_registry("acme/tool", "ocx.sh").clone_with_digest(manifest_digest);
        let outcome = manager.pull(&package, ocx_oci::Platform::any()).await;
        (outcome, stub_data)
    }

    /// Every blob the pin needs is in the store, so the missing location is
    /// never asked for: the pull succeeds offline, dialling nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unrecorded_index_owned_pin_materializes_offline_from_the_blob_store() {
        let (outcome, stub_data) = pull_unrecorded_index_owned_pin(None).await;
        assert!(
            outcome.is_ok(),
            "a cached chain needs no location; err: {:?}",
            outcome.err()
        );
        assert!(
            stub_data.read().calls.is_empty() && stub_data.read().auth_calls.is_empty(),
            "nothing may be dialled: {:?}",
            stub_data.read().calls
        );
    }

    /// A layer the store does not hold needs the location offline mode cannot
    /// look up: the pull is refused with the policy block (exit 81), not the
    /// internal `LayerNotStaged`, and still dials nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_missing_layer_of_an_unrecorded_index_owned_pin_is_refused_as_unrecorded() {
        let layer_digest = ocx_oci::Digest::Sha256("e".repeat(64));
        let (outcome, stub_data) = pull_unrecorded_index_owned_pin(Some(&layer_digest)).await;
        let error = outcome.expect_err("a missing layer with no location must be refused offline");

        let mut rendered = error.to_string();
        let mut cause = std::error::Error::source(&error);
        let mut blocked = false;
        while let Some(current) = cause {
            rendered.push_str(&format!(": {current}"));
            // The node exit-code classification descends to for 81. Only a
            // refusal raised at the layer sits below the top: the same
            // refusal raised at routing is the top error and is never visited.
            blocked |= matches!(
                current.downcast_ref::<PackageErrorKind>(),
                Some(PackageErrorKind::Internal(crate::Error::OciIndex(
                    ocx_index::error::Error::PolicyResolutionBlocked {
                        policy: "offline",
                        block: ocx_index::error::PolicyBlock::UnrecordedLocation,
                        ..
                    }
                )))
            );
            cause = current.source();
        }
        assert!(blocked, "the refusal must be the offline policy block: {rendered}");
        assert!(
            rendered.contains(
                "'ocx.sh/acme/tool' is served by an index and has no locally recorded location, \
                 which offline mode cannot look up; run `ocx index update ocx.sh/acme/tool` once online"
            ),
            "{rendered}"
        );
        assert!(
            stub_data.read().calls.is_empty() && stub_data.read().auth_calls.is_empty(),
            "nothing may be dialled: {:?}",
            stub_data.read().calls
        );
    }

    fn bundle_metadata() -> Metadata {
        Metadata::Bundle(Bundle {
            binaries: None,
            version: Version::V1,
            strip_components: None,
            env: Env::default(),
            dependencies: Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        })
    }

    /// Bug 20 / F1: the leaf artifact-type gate used to wrap a media-type
    /// error in `ClientError::Internal`, whose terminal `Failure` arm ends the
    /// chain walk — so installing a non-OCX artifact exited 1 while every
    /// sibling artifact-type gate exits 65.
    ///
    /// The assertion is deliberately on the **classified exit code**, not the
    /// error variant: `ClientError::UnexpectedArtifactType` already classified
    /// as `DataError` before the fix. A variant-only assertion would have
    /// passed against the bug.
    #[tokio::test(flavor = "multi_thread")]
    async fn unexpected_leaf_artifact_type_exits_data_error() {
        let error = refuse_foreign_leaf_artifact(Some(bundle_metadata())).await;

        let message = error.to_string();
        assert!(
            message.contains("application/vnd.sh.ocx.package.v1"),
            "the error must name the expected type: {message}"
        );
        assert!(
            message.contains("application/vnd.example.other.v1"),
            "the error must name the actual type: {message}"
        );
    }

    /// The path `ocx install docker.io/library/alpine:3` actually takes:
    /// `provided_metadata` is `None`, so `load_config_metadata`'s config
    /// media-type gate fires before the artifact-type gate. That gate used to
    /// wrap `crate::Error::UnsupportedMediaType` — which classifies as 65 on
    /// its own — in `ClientError::internal`, whose terminal `Failure` arm
    /// stops the chain walk at exit 1.
    #[tokio::test(flavor = "multi_thread")]
    async fn unexpected_leaf_config_media_type_exits_data_error() {
        let error = refuse_foreign_leaf_artifact(None).await;

        let message = error.to_string();
        assert!(
            message.contains("application/vnd.example.other.config.v1+json"),
            "the error must name the offending config media type: {message}"
        );
    }

    // ── The dial-site SSRF floor over a rewritten physical target ────────────

    /// Whatever a layer fetch touches on the transport — the request log and the
    /// auth handshake that precedes it. Both must be empty for "no dial".
    struct TransportTouches {
        calls: Vec<String>,
        auth: usize,
    }

    impl TransportTouches {
        fn none(&self) -> bool {
            self.calls.is_empty() && self.auth == 0
        }
    }

    /// What a pull left behind for the assertions: the outcome, what reached the
    /// transport, and how many times the dial-site guard was evaluated.
    struct PullObservation {
        outcome: Result<InstallInfo, PackageErrorKind>,
        touches: TransportTouches,
        guard_evaluations: usize,
    }

    /// Runs a real `setup_owned` for a `layers`-layer package whose physical
    /// transport registry differs from its logical one — the index-indirected
    /// shape.
    ///
    /// `layers_cached` decides whether the pull has anything to fetch: with every
    /// layer already extracted on disk the operation is fully warm and no dial is
    /// imminent, which is exactly the case the guard must not judge.
    ///
    /// The chain always carries a `TrustingSource` owning the LOGICAL registry.
    /// It trusts nothing, so it changes no verdict — it is there to count, since
    /// `guard_physical_dial` asks it for `trusted_hosts` exactly once per
    /// evaluation.
    async fn pull_indirected_package(physical_registry: &str, layers: usize, layers_cached: bool) -> PullObservation {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let guard_evaluations = Arc::new(AtomicUsize::new(0));
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            vec![TrustingSource::new("example.com", Vec::new(), guard_evaluations.clone()).into_index()],
            ChainMode::Default,
        );
        let transport_data = StubTransportData::new();
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(transport_data.clone())));
        let manager = PackageManager::new(file_structure.clone(), index, Some(client), "example.com");

        let manifest_digest = ocx_oci::Digest::Sha256("d".repeat(64));
        let layer_digests: Vec<ocx_oci::Digest> = (0..layers)
            .map(|index| ocx_oci::Digest::Sha256(format!("{index:064}")))
            .collect();
        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("test/indirected", "example.com")
                .clone_with_tag("1.0.0")
                .clone_with_digest(manifest_digest.clone()),
        )
        .expect("pinned identifier");
        // The physical pointer an index root would mint: a different registry,
        // carrying the same leaf digest (transport-only, Decision C2).
        let transport_pinned = ocx_oci::OciIdentifier::from_parts("mirror/indirected", physical_registry)
            .pinned_at(manifest_digest.clone());

        if layers_cached {
            for layer_digest in &layer_digests {
                tokio::fs::create_dir_all(file_structure.layers.content(pinned.registry(), layer_digest))
                    .await
                    .unwrap();
            }
        }

        let platform = ocx_oci::Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: Vec::new(),
        };

        let final_manifest = ocx_oci::ImageManifest {
            artifact_type: Some(super::MEDIA_TYPE_PACKAGE_V1.to_string()),
            config: ocx_oci::Descriptor {
                media_type: ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                digest: manifest_digest.to_string(),
                size: 2,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            layers: layer_digests
                .iter()
                .map(|layer_digest| ocx_oci::Descriptor {
                    media_type: ocx_oci::media_type::MEDIA_TYPE_TAR_GZ.to_string(),
                    digest: layer_digest.to_string(),
                    size: 1,
                    urls: None,
                    artifact_type: None,
                    annotations: None,
                })
                .collect(),
            ..Default::default()
        };

        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: Ok(transport_pinned),
            chain: vec![ChainBlob {
                identifier: pinned.clone(),
                role: ChainRole::Manifest,
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                size: 2,
            }],
            final_manifest,
            platform: platform.clone(),
            leaf_origin: ocx_index::BlobOrigin::Network,
        };

        let outcome = setup_owned(
            &manager,
            &pinned,
            resolved,
            platform,
            SetupGroups::new(),
            None,
            Some(bundle_metadata()),
        )
        .await;

        PullObservation {
            outcome,
            touches: TransportTouches {
                calls: transport_data.read().calls.clone(),
                auth: transport_data.read().auth_calls.len(),
            },
            guard_evaluations: guard_evaluations.load(Ordering::SeqCst),
        }
    }

    /// The gap the dial-site guard closes: `physical_reference` resolves the
    /// pointer on every resolve, so its own pre-flight must tolerate a lookup
    /// failure — and the pull then dials the admitted host on the shared client,
    /// which performs its own independent lookup and carries no
    /// `GuardedResolver`. A hostile local index tree (an rsync'd copy is a
    /// supported distribution mechanism) naming a host that answers NXDOMAIN at
    /// check time and loopback at dial time would otherwise reach a forbidden
    /// target deterministically.
    ///
    /// `localhost` is the fixture because it is a plain DNS name that genuinely
    /// resolves into the forbidden range — the *post*-rebind state of that
    /// attack, judged the way the dial would judge it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rewritten_forbidden_transport_is_refused_before_the_first_layer_request() {
        let (host, port) = ocx_oci::ssrf::split_host_port("localhost:5999");
        assert!(
            matches!(
                ocx_oci::ssrf::resolve_and_validate(host, port, &[]).await,
                Err(ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. })
            ),
            "the fixture must resolve into the forbidden range, or the refusal below proves nothing"
        );

        let observed = pull_indirected_package("localhost:5999", 1, false).await;

        let _error = observed
            .outcome
            .expect_err("a forbidden physical target must not be dialled");
        // Asserted before the exit code: without the guard the pull still fails
        // (the stub serves no blob), so only the empty log distinguishes "refused"
        // from "dialled and happened to fail".
        assert!(
            observed.touches.none(),
            "the refusal must land BEFORE any request reaches the transport; saw {:?} and {} auth handshake(s)",
            observed.touches.calls,
            observed.touches.auth
        );
    }

    /// The verdict is memoized **whole**, so one refusal is final for the pull.
    ///
    /// Caching only success would leave every missing layer free to re-ask, and a
    /// host that answers differently between two adjacent lookups needs exactly
    /// one `Ok` to be dialled — N missing layers would hand a rebinding attacker
    /// N attempts at it. Two layers, both missing: the guard must be evaluated
    /// once, not twice.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dial_refusal_is_evaluated_once_however_many_layers_are_missing() {
        let observed = pull_indirected_package("localhost:5999", 2, false).await;

        observed
            .outcome
            .expect_err("a forbidden physical target must not be dialled");
        assert_eq!(
            observed.guard_evaluations, 1,
            "a refusal must stay refused for the whole pull; the second missing layer re-asked it"
        );
        assert!(
            observed.touches.none(),
            "no layer may reach the transport; saw {:?} and {} auth handshake(s)",
            observed.touches.calls,
            observed.touches.auth
        );
    }

    /// The check is gated on a dial, not on a resolve: a fully-warm pull whose
    /// layer is already extracted never resolves the physical host at all. The
    /// fixture is an unresolvable name, so if the guard ran the pull would fail
    /// closed — the warm-store-no-network property `physical_reference`'s
    /// local-first order exists to serve would be gone.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_warm_pull_never_judges_an_unresolvable_transport_host() {
        let (host, port) = ocx_oci::ssrf::split_host_port("physical.invalid");
        assert!(
            matches!(
                ocx_oci::ssrf::resolve_and_validate(host, port, &[]).await,
                Err(ocx_oci::ssrf::SsrfError::Resolution { .. })
            ),
            "the fixture must genuinely fail to resolve, or a skipped guard is indistinguishable from a passing one"
        );

        let observed = pull_indirected_package("physical.invalid", 1, true).await;

        observed
            .outcome
            .expect("a fully-warm pull must not depend on resolving the physical host");
        assert_eq!(
            observed.guard_evaluations, 0,
            "a warm pull must not evaluate the guard at all"
        );
        assert!(
            observed.touches.none(),
            "a warm pull must reach the transport for nothing; saw {:?} and {} auth handshake(s)",
            observed.touches.calls,
            observed.touches.auth
        );
    }

    /// Records span names with their `count` field, and each event's
    /// explicit parent. `tracing-subscriber` is not a dependency here.
    #[derive(Default)]
    struct Recorded {
        next_id: std::sync::atomic::AtomicU64,
        spans: std::sync::Mutex<Vec<(u64, &'static str, Option<u64>)>>,
        event_parents: std::sync::Mutex<Vec<Option<u64>>>,
    }

    #[derive(Clone, Default)]
    struct SpanRecorder(Arc<Recorded>);

    struct CountVisitor(Option<u64>);

    impl tracing::field::Visit for CountVisitor {
        fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
            if field.name() == "count" {
                self.0 = Some(value);
            }
        }
        fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
    }

    impl tracing::Subscriber for SpanRecorder {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            let id = self.0.next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let mut count = CountVisitor(None);
            span.record(&mut count);
            self.0.spans.lock().unwrap().push((id, span.metadata().name(), count.0));
            tracing::span::Id::from_u64(id)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            if *event.metadata().level() == tracing::Level::INFO {
                self.0
                    .event_parents
                    .lock()
                    .unwrap()
                    .push(event.parent().map(|id| id.into_u64()));
            }
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// A batch pull opens exactly one `Pulling` span carrying the batch size,
    /// and parents an info event on it: the batch's non-TTY observable.
    #[tokio::test]
    async fn pull_all_opens_one_pulling_span_carrying_the_batch_size() {
        let dir = tempfile::tempdir().unwrap();
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        let manager = PackageManager::new(
            FileStructure::with_root(dir.path().to_path_buf()),
            index,
            None,
            "example.com",
        );
        let packages: Vec<_> = ["test/a", "test/b"]
            .into_iter()
            .map(|repo| ocx_oci::PackageRef::new_registry(repo, "example.com").clone_with_tag("1.0.0"))
            .collect();

        let recorder = SpanRecorder::default();
        {
            let _default = tracing::subscriber::set_default(recorder.clone());
            // Offline and unindexed: the pulls fail; only the batch span matters.
            let _ = manager
                .pull_all(
                    &packages,
                    ocx_oci::Platform::Any,
                    crate::concurrency::Concurrency::cores(),
                    true,
                )
                .await;
        }

        let spans = recorder.0.spans.lock().unwrap();
        let pulling: Vec<_> = spans.iter().filter(|(_, name, _)| *name == "Pulling").collect();
        assert_eq!(pulling.len(), 1, "expected one batch span, recorded {spans:?}");
        let (pulling_id, _, count) = pulling[0];
        assert_eq!(*count, Some(2), "batch span must carry count = packages.len()");
        let parents = recorder.0.event_parents.lock().unwrap();
        assert!(
            parents.contains(&Some(*pulling_id)),
            "no info event parented on the batch span; info parents: {parents:?}"
        );
    }

    // ── activity log ─────────────────────────────────────────────────────────

    /// An offline manager with `log` attached, over an empty home.
    fn offline_manager(log: crate::activity::ActivityLog) -> (PackageManager, FileStructure, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained_with_content_store(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::machine_local(&file_structure),
            })
            .with_index_namespaces(std::collections::HashSet::from(["ocx.sh".to_string()])),
            Vec::new(),
            ChainMode::Offline,
            file_structure.blobs.clone(),
        );
        let manager = PackageManager::new(file_structure.clone(), index, None, "ocx.sh").with_activity_log(log);
        (manager, file_structure, dir)
    }

    /// Writes a zero-layer package with `metadata` into the blob store and returns its manifest digest.
    async fn write_zero_layer_package(file_structure: &FileStructure, metadata: serde_json::Value) -> ocx_oci::Digest {
        write_package(file_structure, metadata, &[]).await
    }

    /// Writes a package with `metadata` and manifest `layers` into the blob store (the layers' bytes are not stored).
    async fn write_package(
        file_structure: &FileStructure,
        metadata: serde_json::Value,
        layers: &[ocx_oci::Descriptor],
    ) -> ocx_oci::Digest {
        let config = serde_json::to_vec(&metadata).unwrap();
        let config_digest = ocx_oci::Algorithm::Sha256.hash(&config);
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": ocx_oci::OCI_IMAGE_MEDIA_TYPE,
            "artifactType": super::MEDIA_TYPE_PACKAGE_V1,
            "config": {
                "mediaType": ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1,
                "digest": config_digest.to_string(),
                "size": config.len(),
            },
            "layers": layers,
        }))
        .unwrap();
        let manifest_digest = ocx_oci::Algorithm::Sha256.hash(&manifest);
        file_structure
            .blobs
            .write_blob("ocx.sh", &manifest_digest, &manifest)
            .await
            .unwrap();
        file_structure
            .blobs
            .write_blob("ocx.sh", &config_digest, &config)
            .await
            .unwrap();
        manifest_digest
    }

    /// An offline manager whose blob store holds a zero-layer package, with `log` attached.
    async fn zero_layer_package(
        log: crate::activity::ActivityLog,
    ) -> (PackageManager, ocx_oci::PackageRef, tempfile::TempDir) {
        let (manager, file_structure, dir) = offline_manager(log);
        let digest = write_zero_layer_package(&file_structure, serde_json::to_value(bundle_metadata()).unwrap()).await;
        let package = ocx_oci::PackageRef::new_registry("acme/tool", "ocx.sh").clone_with_digest(digest);
        (manager, package, dir)
    }

    /// Waits, bounded, until `waiters` tasks have joined the leader's in-flight entry.
    async fn wait_for_waiters<V>(handle: &super::singleflight::Handle<V>, waiters: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while handle.waiter_count() < waiters {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the second task never joined the in-flight entry");
    }

    /// A setup that finds its package set up, or being set up, by another task records nothing.
    ///
    /// Asserted on the raw log, which `drain` would collapse per package. The test holds the
    /// singleflight leader itself, so the second setup is a waiter whatever the scheduling.
    #[tokio::test(flavor = "multi_thread")]
    async fn activity_log_records_leaders_only() {
        let log = crate::activity::ActivityLog::new();
        let (manager, package, _dir) = zero_layer_package(log.clone()).await;
        let pinned = ocx_oci::PinnedPackageRef::try_from(package.clone()).expect("pinned identifier");

        let info = super::setup_impl(
            &manager,
            &package,
            ocx_oci::Platform::any(),
            SetupGroups::new(),
            None,
            crate::activity::Role::Root,
        )
        .await
        .expect("the leader sets up");
        assert_eq!(log.recorded(), 1, "the leader records its entry");

        let groups = SetupGroups::new();
        let super::singleflight::Acquisition::Leader(handle) = groups
            .packages
            .try_acquire(pinned.strip_advisory())
            .await
            .expect("the key is free")
        else {
            panic!("the test must hold the leader");
        };
        let waiter = tokio::spawn({
            let (manager, package, groups) = (manager.clone(), package.clone(), groups.clone());
            async move {
                super::setup_impl(
                    &manager,
                    &package,
                    ocx_oci::Platform::any(),
                    groups,
                    None,
                    crate::activity::Role::Root,
                )
                .await
            }
        });
        wait_for_waiters(&handle, 1).await;
        assert!(!waiter.is_finished(), "a waiter cannot finish while the leader is held");
        handle.complete(info);
        waiter
            .await
            .expect("the waiter task joins")
            .expect("the waiter reuses the leader's result");

        assert_eq!(log.recorded(), 1, "the waiter must append no entry");
    }

    /// The first setup of a package fetches it; a second setup in the same store only finds it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_setup_records_a_store_hit() {
        let log = crate::activity::ActivityLog::new();
        let (manager, package, _dir) = zero_layer_package(log.clone()).await;

        let mut store_hits = Vec::new();
        for _ in 0..2 {
            super::setup_impl(
                &manager,
                &package,
                ocx_oci::Platform::any(),
                SetupGroups::new(),
                None,
                crate::activity::Role::Root,
            )
            .await
            .expect("the setup succeeds");
            let entries = log.drain();
            assert_eq!(entries.len(), 1, "one record per package per run");
            assert_eq!(entries[0].action, crate::activity::Action::Pulled);
            store_hits.push(entries[0].store_hit);
        }

        assert_eq!(store_hits, [false, true]);
    }

    /// A root with one dependency: the root is `Root`, the dependency `Dependency`, both `Pulled`.
    #[tokio::test(flavor = "multi_thread")]
    async fn dependency_entries_carry_role_dependency() {
        let log = crate::activity::ActivityLog::new();
        let (manager, file_structure, _dir) = offline_manager(log.clone());
        let dep_digest =
            write_zero_layer_package(&file_structure, serde_json::to_value(bundle_metadata()).unwrap()).await;
        let root_metadata = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [],
            "dependencies": [{ "identifier": format!("ocx.sh/acme/dep:1.0@{dep_digest}") }],
        });
        let root_digest = write_zero_layer_package(&file_structure, root_metadata).await;
        let root = ocx_oci::PackageRef::new_registry("acme/root", "ocx.sh").clone_with_digest(root_digest);

        manager
            .pull_all(
                std::slice::from_ref(&root),
                ocx_oci::Platform::any(),
                crate::Concurrency::cores(),
                true,
            )
            .await
            .expect("the pull succeeds");
        let entries = log.drain();

        let by_repository = |name: &str| {
            entries
                .iter()
                .find(|entry| entry.pinned.repository() == name)
                .unwrap_or_else(|| panic!("no entry for {name}, drained {entries:?}"))
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(by_repository("acme/root").role, crate::activity::Role::Root);
        assert_eq!(by_repository("acme/dep").role, crate::activity::Role::Dependency);
        assert!(entries.iter().all(|e| e.action == crate::activity::Action::Pulled));
    }

    /// A root that is also its sibling's dependency is a root however the singleflight race went.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_root_that_is_also_a_dependency_stays_root() {
        let log = crate::activity::ActivityLog::new();
        let (manager, file_structure, _dir) = offline_manager(log.clone());
        let shared_digest =
            write_zero_layer_package(&file_structure, serde_json::to_value(bundle_metadata()).unwrap()).await;
        let sibling_metadata = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [],
            "dependencies": [{ "identifier": format!("ocx.sh/acme/shared:1.0@{shared_digest}") }],
        });
        let sibling_digest = write_zero_layer_package(&file_structure, sibling_metadata).await;
        let roots = [
            ocx_oci::PackageRef::new_registry("acme/shared", "ocx.sh").clone_with_digest(shared_digest),
            ocx_oci::PackageRef::new_registry("acme/sibling", "ocx.sh").clone_with_digest(sibling_digest),
        ];

        manager
            .pull_all(&roots, ocx_oci::Platform::any(), crate::Concurrency::cores(), true)
            .await
            .expect("the pull succeeds");
        let entries = log.drain();

        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().all(|entry| entry.role == crate::activity::Role::Root),
            "both packages were named by the command: {entries:?}"
        );
    }

    /// `install_all` with a candidate link marks its roots `Installed`; without one they stay `Pulled`.
    #[tokio::test(flavor = "multi_thread")]
    async fn install_marks_roots_installed() {
        let log = crate::activity::ActivityLog::new();
        let (manager, file_structure, _dir) = offline_manager(log.clone());
        let digest = write_zero_layer_package(&file_structure, serde_json::to_value(bundle_metadata()).unwrap()).await;
        let package = ocx_oci::PackageRef::new_registry("acme/tool", "ocx.sh")
            .clone_with_tag("1.0")
            .clone_with_digest(digest);

        manager
            .install_all(
                vec![package.clone()],
                ocx_oci::Platform::any(),
                false,
                false,
                crate::Concurrency::cores(),
                true,
            )
            .await
            .expect("the pull succeeds");
        let pulled = log.drain();
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].action, crate::activity::Action::Pulled);

        manager
            .install_all(
                vec![package],
                ocx_oci::Platform::any(),
                true,
                false,
                crate::Concurrency::cores(),
                true,
            )
            .await
            .expect("the install succeeds");
        let installed = log.drain();

        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].action, crate::activity::Action::Installed);
        assert_eq!(installed[0].role, crate::activity::Role::Root);
    }

    /// A gzipped tar with one file, and its descriptor.
    fn shared_layer() -> (Vec<u8>, ocx_oci::Descriptor) {
        layer_with_file("shared.txt", b"shared layer content")
    }

    /// A gzipped tar holding `content` as `name`, and its descriptor.
    fn layer_with_file(name: &str, content: &[u8]) -> (Vec<u8>, ocx_oci::Descriptor) {
        use std::io::Write as _;

        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, name, content).unwrap();
            builder.finish().unwrap();
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        let bytes = encoder.finish().unwrap();
        let descriptor = ocx_oci::Descriptor {
            media_type: ocx_oci::media_type::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: ocx_oci::Algorithm::Sha256.hash(&bytes).to_string(),
            size: bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        (bytes, descriptor)
    }

    /// Two packages referencing one layer fetch it once: only the package that led the layer
    /// counts its bytes.
    ///
    /// Driven at `setup_owned`, the smallest seam that reaches a layer fetch through the stub
    /// transport; the entries are the ones the manager's log holds when both have returned.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_shared_layer_counts_once() {
        let dir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ChainMode::Default,
        );
        let (layer_bytes, layer) = shared_layer();
        let layer_size = u64::try_from(layer.size).unwrap();
        let transport_data = StubTransportData::new();
        transport_data.write().blobs.insert(layer.digest.clone(), layer_bytes);
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(transport_data)));
        let log = crate::activity::ActivityLog::new();
        let manager =
            PackageManager::new(file_structure, index, Some(client), "example.com").with_activity_log(log.clone());

        let platform = ocx_oci::Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: Vec::new(),
        };
        let groups = SetupGroups::new();
        let mut setups = Vec::new();
        for (repository, fill) in [("test/first", "a"), ("test/second", "b")] {
            let digest = ocx_oci::Digest::Sha256(fill.repeat(64));
            let pinned = ocx_oci::PinnedPackageRef::try_from(
                ocx_oci::PackageRef::new_registry(repository, "example.com")
                    .clone_with_tag("1.0.0")
                    .clone_with_digest(digest.clone()),
            )
            .expect("pinned identifier");
            let resolved = ResolvedChain {
                pinned: pinned.clone(),
                transport_pinned: Ok(
                    ocx_oci::OciIdentifier::from_parts(repository, "example.com").pinned_at(digest.clone())
                ),
                chain: vec![ChainBlob {
                    identifier: pinned.clone(),
                    role: ChainRole::Manifest,
                    media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                    size: 2,
                }],
                final_manifest: ocx_oci::ImageManifest {
                    artifact_type: Some(super::MEDIA_TYPE_PACKAGE_V1.to_string()),
                    config: ocx_oci::Descriptor {
                        media_type: ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                        digest: digest.to_string(),
                        size: 2,
                        urls: None,
                        artifact_type: None,
                        annotations: None,
                    },
                    layers: vec![layer.clone()],
                    ..Default::default()
                },
                platform: platform.clone(),
                leaf_origin: ocx_index::BlobOrigin::Network,
            };
            setups.push((pinned, resolved));
        }
        let (first, second) = (setups.remove(0), setups.remove(0));
        let (a, b) = tokio::join!(
            setup_owned(
                &manager,
                &first.0,
                first.1,
                platform.clone(),
                groups.clone(),
                None,
                Some(bundle_metadata())
            ),
            setup_owned(
                &manager,
                &second.0,
                second.1,
                platform.clone(),
                groups.clone(),
                None,
                Some(bundle_metadata())
            ),
        );
        a.expect("the first package sets up");
        b.expect("the second package sets up");

        let entries = log.drain();
        assert_eq!(entries.len(), 2, "one entry per package");
        assert_eq!(
            entries.iter().map(|entry| entry.download_size).sum::<u64>(),
            layer_size,
            "the shared layer's bytes are counted once, by its leader"
        );
        assert_eq!(
            entries.iter().filter(|entry| entry.download_size == 0).count(),
            1,
            "the package that did not lead the layer fetched nothing itself"
        );
    }

    /// A pull that fails mid-setup records nothing: no entry and no role for a package that never landed.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_install_records_nothing() {
        let log = crate::activity::ActivityLog::new();
        let (manager, file_structure, _dir) = offline_manager(log.clone());
        let (_, layer) = shared_layer();
        let digest = write_package(
            &file_structure,
            serde_json::to_value(bundle_metadata()).unwrap(),
            std::slice::from_ref(&layer),
        )
        .await;
        let package = ocx_oci::PackageRef::new_registry("acme/tool", "ocx.sh")
            .clone_with_tag("1.0")
            .clone_with_digest(digest);

        let error = manager
            .install_all(
                vec![package],
                ocx_oci::Platform::any(),
                true,
                false,
                crate::Concurrency::cores(),
                true,
            )
            .await
            .expect_err("the layer is neither cached nor fetchable offline");

        let crate::Error::InstallFailed(failures) = &error else {
            panic!("expected InstallFailed, got {error:?}");
        };
        let [failure] = failures.as_slice() else {
            panic!("expected one failed package, got {failures:?}");
        };
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(&failure.kind);
        let mut blocked = false;
        while let Some(current) = cause {
            blocked |= matches!(
                current.downcast_ref::<crate::error::PackageErrorKind>(),
                Some(crate::error::PackageErrorKind::Internal(crate::Error::OciIndex(
                    ocx_index::error::Error::PolicyResolutionBlocked { .. }
                )))
            );
            cause = current.source();
        }
        assert!(
            blocked,
            "the layer must be unobtainable offline (the index refuses its location), got {failure:?}"
        );
        assert_eq!(log.recorded(), 0, "a failed setup appends no entry");
        assert!(log.drain().is_empty());
    }

    /// Same, when the transport is reached and its blob does not verify.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_whose_layer_fetch_fails_records_nothing() {
        let log = crate::activity::ActivityLog::new();
        let (bytes, layer) = shared_layer();
        let (manager, _dir) = layered_manager(&[(bytes[..bytes.len() / 2].to_vec(), layer.clone())], log.clone());

        let (pinned, resolved) = layered_chain("test/tool", "a", std::slice::from_ref(&layer));
        let outcome = setup_owned(
            &manager,
            &pinned,
            resolved,
            linux_amd64(),
            SetupGroups::new(),
            None,
            Some(bundle_metadata()),
        )
        .await;

        assert!(outcome.is_err(), "a truncated layer must not verify");
        assert_eq!(log.recorded(), 0, "a failed setup appends no entry");
    }

    /// The package that waits on a layer another package leads counts no download for it:
    /// only the layer leader's entry carries the bytes and the download time.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_package_waiting_on_a_layer_leader_counts_no_download() {
        let log = crate::activity::ActivityLog::new();
        let (bytes, layer) = shared_layer();
        let (manager, _dir) = layered_manager(&[(bytes, layer.clone())], log.clone());
        let (pinned, resolved) = layered_chain("test/waiter", "a", std::slice::from_ref(&layer));
        let layer_digest: ocx_oci::Digest = layer.digest.clone().try_into().expect("layer digest");
        let layer_content = manager
            .file_structure()
            .layers
            .content(pinned.registry(), &layer_digest);

        let groups = SetupGroups::new();
        let super::singleflight::Acquisition::Leader(handle) = groups
            .layers
            .try_acquire((pinned.registry().to_string(), layer_digest))
            .await
            .expect("the key is free")
        else {
            panic!("the test must hold the layer leader");
        };
        let waiter = tokio::spawn({
            let (manager, groups) = (manager.clone(), groups.clone());
            async move {
                setup_owned(
                    &manager,
                    &pinned,
                    resolved,
                    linux_amd64(),
                    groups,
                    None,
                    Some(bundle_metadata()),
                )
                .await
            }
        });
        wait_for_waiters(&handle, 1).await;
        assert!(
            !waiter.is_finished(),
            "the package cannot finish while its layer is led elsewhere"
        );
        // What the layer leader leaves behind when it completes.
        tokio::fs::create_dir_all(&layer_content).await.unwrap();
        handle.complete(());
        waiter
            .await
            .expect("the waiter task joins")
            .expect("the package sets up from the leader's layer");

        let entries = log.drain();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].store_hit);
        assert_eq!(entries[0].download_size, 0, "the leader's bytes are not the waiter's");
        assert_eq!(entries[0].download, None, "no layer was fetched by this package");
        assert!(entries[0].extract.is_some(), "the package was still assembled");
    }

    // ── timings and downloaded bytes ─────────────────────────────────────────

    fn linux_amd64() -> ocx_oci::Platform {
        ocx_oci::Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: Vec::new(),
        }
    }

    /// A manager over a stub transport serving `layers`, with `log` attached.
    fn layered_manager(
        layers: &[(Vec<u8>, ocx_oci::Descriptor)],
        log: crate::activity::ActivityLog,
    ) -> (PackageManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ChainMode::Default,
        );
        let transport_data = StubTransportData::new();
        for (bytes, descriptor) in layers {
            transport_data
                .write()
                .blobs
                .insert(descriptor.digest.clone(), bytes.clone());
        }
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(transport_data)));
        let manager = PackageManager::new(
            FileStructure::with_root(dir.path().to_path_buf()),
            index,
            Some(client),
            "example.com",
        )
        .with_activity_log(log);
        (manager, dir)
    }

    /// A resolved package named `repository`, identified by `fill` repeated, whose manifest carries `layers`.
    fn layered_chain(
        repository: &str,
        fill: &str,
        layers: &[ocx_oci::Descriptor],
    ) -> (ocx_oci::PinnedPackageRef, ResolvedChain) {
        let digest = ocx_oci::Digest::Sha256(fill.repeat(64));
        let pinned = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repository, "example.com")
                .clone_with_tag("1.0.0")
                .clone_with_digest(digest.clone()),
        )
        .expect("pinned identifier");
        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: Ok(
                ocx_oci::OciIdentifier::from_parts(repository, "example.com").pinned_at(digest.clone())
            ),
            chain: vec![ChainBlob {
                identifier: pinned.clone(),
                role: ChainRole::Manifest,
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                size: 2,
            }],
            final_manifest: ocx_oci::ImageManifest {
                artifact_type: Some(super::MEDIA_TYPE_PACKAGE_V1.to_string()),
                config: ocx_oci::Descriptor {
                    media_type: ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                    digest: digest.to_string(),
                    size: 2,
                    urls: None,
                    artifact_type: None,
                    annotations: None,
                },
                layers: layers.to_vec(),
                ..Default::default()
            },
            platform: linux_amd64(),
            leaf_origin: ocx_index::BlobOrigin::Network,
        };
        (pinned, resolved)
    }

    async fn set_up(
        manager: &PackageManager,
        (pinned, resolved): (ocx_oci::PinnedPackageRef, ResolvedChain),
        groups: SetupGroups,
    ) -> InstallInfo {
        setup_owned(
            manager,
            &pinned,
            resolved,
            linux_amd64(),
            groups,
            None,
            Some(bundle_metadata()),
        )
        .await
        .expect("the package sets up")
    }

    /// A package fetched once is only found the second time: no layer phase, no assembly, no bytes.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_install_is_a_store_hit_without_download_or_extract() {
        let log = crate::activity::ActivityLog::new();
        let (bytes, layer) = shared_layer();
        let (manager, _dir) = layered_manager(&[(bytes, layer.clone())], log.clone());

        set_up(
            &manager,
            layered_chain("test/tool", "a", std::slice::from_ref(&layer)),
            SetupGroups::new(),
        )
        .await;
        let first = log.drain();
        set_up(
            &manager,
            layered_chain("test/tool", "a", std::slice::from_ref(&layer)),
            SetupGroups::new(),
        )
        .await;
        let second = log.drain();

        assert_eq!((first.len(), second.len()), (1, 1));
        assert!(!first[0].store_hit);
        assert_eq!(first[0].download_size, u64::try_from(layer.size).unwrap());
        assert!(first[0].download.is_some(), "the first run fetched a layer");
        assert!(first[0].extract.is_some(), "the first run assembled the package");
        assert!(second[0].store_hit);
        assert_eq!(second[0].download_size, 0);
        assert_eq!(second[0].download, None);
        assert_eq!(second[0].extract, None);
    }

    /// The bytes are the descriptor sizes of the layers fetched, summed; the layer phase is a download.
    #[tokio::test(flavor = "multi_thread")]
    async fn fetched_bytes_equal_the_layer_descriptor_sizes() {
        let log = crate::activity::ActivityLog::new();
        let (first_bytes, first) = layer_with_file("first.txt", b"the first layer");
        let (second_bytes, second) = layer_with_file("second.txt", b"a second, longer layer body");
        let (manager, _dir) = layered_manager(
            &[(first_bytes, first.clone()), (second_bytes, second.clone())],
            log.clone(),
        );

        set_up(
            &manager,
            layered_chain("test/tool", "a", &[first.clone(), second.clone()]),
            SetupGroups::new(),
        )
        .await;
        let entries = log.drain();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].download_size,
            u64::try_from(first.size + second.size).unwrap()
        );
        assert!(entries[0].download.is_some());
    }

    /// A layer already in the layer store costs no bytes and no download phase, but the package is still assembled.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_layer_already_in_the_store_is_not_counted() {
        let log = crate::activity::ActivityLog::new();
        let (bytes, layer) = shared_layer();
        let (manager, _dir) = layered_manager(&[(bytes, layer.clone())], log.clone());

        set_up(
            &manager,
            layered_chain("test/first", "a", std::slice::from_ref(&layer)),
            SetupGroups::new(),
        )
        .await;
        log.drain();
        set_up(
            &manager,
            layered_chain("test/second", "b", std::slice::from_ref(&layer)),
            SetupGroups::new(),
        )
        .await;
        let entries = log.drain();

        assert_eq!(entries.len(), 1);
        assert!(!entries[0].store_hit);
        assert_eq!(entries[0].download_size, 0);
        assert_eq!(entries[0].download, None);
        assert!(entries[0].extract.is_some());
    }

    /// The resolve time is on every `setup_impl` entry, store hit or not; no log attached means no timing at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn setup_records_resolve_time_for_fetched_and_cached_packages() {
        let log = crate::activity::ActivityLog::new();
        let (manager, package, _dir) = zero_layer_package(log.clone()).await;

        let mut resolves = Vec::new();
        for _ in 0..2 {
            super::setup_impl(
                &manager,
                &package,
                ocx_oci::Platform::any(),
                SetupGroups::new(),
                None,
                crate::activity::Role::Root,
            )
            .await
            .expect("the setup succeeds");
            resolves.push(log.drain()[0].resolve);
        }

        assert!(resolves.iter().all(Option::is_some), "{resolves:?}");
    }

    /// The root's extract time starts after its dependencies are set up: a dependency held back for
    /// `HELD_BACK` (a singleflight leader the root waits on) is inside the root's total, not its extract.
    ///
    /// Asserted as an ordering, not a duration: the extract began after the dependency was released, so
    /// it fits in the time from that release to the end. Extract itself does file I/O whose latency
    /// varies with the host, so a fixed bound on it flakes under load.
    #[tokio::test(flavor = "multi_thread")]
    async fn dependency_setup_time_is_not_inside_the_root_extract() {
        use std::time::{Duration, Instant};

        use ocx_util::singleflight;

        const HELD_BACK: Duration = Duration::from_millis(600);

        let log = crate::activity::ActivityLog::new();
        let (manager, file_structure, _dir) = offline_manager(log.clone());
        let dependency_digest =
            write_zero_layer_package(&file_structure, serde_json::to_value(bundle_metadata()).unwrap()).await;
        let root_digest = write_zero_layer_package(
            &file_structure,
            serde_json::json!({
                "type": "bundle",
                "version": 1,
                "env": [],
                "dependencies": [{ "identifier": format!("ocx.sh/acme/dep:1.0@{dependency_digest}") }],
            }),
        )
        .await;
        let dependency = ocx_oci::PackageRef::new_registry("acme/dep", "ocx.sh")
            .clone_with_tag("1.0")
            .clone_with_digest(dependency_digest);
        let root = ocx_oci::PackageRef::new_registry("acme/root", "ocx.sh").clone_with_digest(root_digest);

        // The dependency installed beforehand is the value the held-back leader later hands over.
        let dependency_info = super::setup_impl(
            &manager,
            &dependency,
            ocx_oci::Platform::any(),
            SetupGroups::new(),
            None,
            crate::activity::Role::Root,
        )
        .await
        .expect("the dependency sets up");
        log.drain();

        let groups = SetupGroups::new();
        let held = match groups
            .packages
            .try_acquire(dependency_info.identifier().strip_advisory())
            .await
            .expect("the group accepts the key")
        {
            singleflight::Acquisition::Leader(handle) => handle,
            singleflight::Acquisition::Resolved(_) => panic!("nothing has set the dependency up in this group"),
        };

        let started = Instant::now();
        let root_setup = super::setup_impl(
            &manager,
            &root,
            ocx_oci::Platform::any(),
            groups,
            None,
            crate::activity::Role::Root,
        );
        let release = async {
            tokio::time::sleep(HELD_BACK).await;
            let released_at = Instant::now();
            held.complete(dependency_info.clone());
            released_at
        };
        let (root_result, released_at) = tokio::join!(root_setup, release);
        root_result.expect("the root sets up");
        let finished_at = Instant::now();
        let total = finished_at - started;
        let after_release = finished_at - released_at;

        let entries = log.drain();
        let root_entry = entries
            .iter()
            .find(|entry| entry.pinned.repository() == "acme/root")
            .unwrap_or_else(|| panic!("no entry for the root: {entries:?}"));
        let extract = root_entry.extract.expect("the root was assembled");
        assert!(total >= HELD_BACK, "the root waited for its dependency: {total:?}");
        assert!(
            extract <= after_release,
            "the dependency's {HELD_BACK:?} is not inside the root's extract: {extract:?} \
             against {after_release:?} between the release and the end"
        );
    }

    // ── layers served from blob directories and the cache ────────────────────

    const LOGICAL: &str = "example.com";

    fn plain_pinned() -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("test/lookup", LOGICAL)
                .clone_with_digest(ocx_oci::Digest::Sha256("c".repeat(64))),
        )
        .expect("pinned identifier")
    }

    fn manifest_of(layer: &ocx_oci::Descriptor) -> ocx_oci::ImageManifest {
        ocx_oci::ImageManifest {
            layers: vec![layer.clone()],
            ..Default::default()
        }
    }

    /// Writes `bytes` as the blob `layer` names under the lookup root `root`.
    fn plant(root: &std::path::Path, layer: &ocx_oci::Descriptor, bytes: &[u8]) {
        let digest = ocx_oci::Digest::try_from(layer.digest.as_str()).unwrap();
        let path = ocx_oci::blob_lookup::blob_data_path(root, LOGICAL, &digest);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn tampered(bytes: &[u8]) -> Vec<u8> {
        vec![7u8; bytes.len()]
    }

    /// A manager over a fresh home with `lookup` attached; `client` is `None` for an offline pull.
    fn lookup_manager(
        home: &std::path::Path,
        lookup: ocx_oci::blob_lookup::BlobLookup,
        client: Option<ocx_oci::Client>,
    ) -> PackageManager {
        let file_structure = FileStructure::with_root(home.to_path_buf());
        let index = Index::from_chained_with_content_store(
            LocalIndex::new(LocalConfig {
                index_store: IndexStore::machine_local(&file_structure),
            }),
            Vec::new(),
            ChainMode::Offline,
            file_structure.blobs.clone(),
        );
        PackageManager::new(file_structure, index, client, LOGICAL).with_blob_lookup(Arc::new(lookup))
    }

    fn lookup_of(dirs: &[&std::path::Path], cache: Option<&std::path::Path>) -> ocx_oci::blob_lookup::BlobLookup {
        ocx_oci::blob_lookup::BlobLookup::new(
            dirs.iter().map(|dir| dir.to_path_buf()).collect(),
            cache.map(|root| ocx_oci::blob_lookup::BlobCacheDir::at(root.to_path_buf())),
        )
    }

    async fn pull_layer_of(
        manager: &PackageManager,
        layer: &ocx_oci::Descriptor,
    ) -> Result<super::LayerPhase, PackageErrorKind> {
        let transport = Ok(ocx_oci::OciIdentifier::from_parts("test/lookup", LOGICAL)
            .pinned_at(ocx_oci::Digest::Sha256("c".repeat(64))));
        extract_layers(
            manager,
            &plain_pinned(),
            &transport,
            &manifest_of(layer),
            SetupGroups::new().layers,
        )
        .await
    }

    fn layer_is_extracted(manager: &PackageManager, layer: &ocx_oci::Descriptor) -> bool {
        let digest = ocx_oci::Digest::try_from(layer.digest.as_str()).unwrap();
        manager.file_structure().layers.content(LOGICAL, &digest).exists()
    }

    /// The exit code of the first classified cause in the chain; the layer failure travels inside a shared error.
    fn exit_code(error: &PackageErrorKind) -> Option<ocx_exit::ExitCode> {
        use ocx_exit::ClassifyExitCode as _;
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(current) = cause {
            if let Some(kind) = current.downcast_ref::<PackageErrorKind>() {
                let code = match kind {
                    PackageErrorKind::Internal(inner) => inner.classify(),
                    other => other.classify(),
                };
                if code.is_some() {
                    return code;
                }
            }
            if let Some(inner) = current.downcast_ref::<crate::Error>()
                && let Some(code) = inner.classify()
            {
                return Some(code);
            }
            cause = current.source();
        }
        None
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn blob_directory_serves_an_offline_pull() {
        let (bytes, layer) = shared_layer();
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), &layer, &bytes);
        let home = tempfile::tempdir().unwrap();
        let manager = lookup_manager(home.path(), lookup_of(&[dir.path()], None), None);

        let phase = pull_layer_of(&manager, &layer)
            .await
            .expect("the directory copy serves");

        assert!(layer_is_extracted(&manager, &layer));
        assert!(!phase.fetched_any, "a local copy is not a download");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_directory_copy_does_not_stop_a_later_source() {
        let (bytes, layer) = shared_layer();
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        plant(first.path(), &layer, &tampered(&bytes));
        plant(second.path(), &layer, &bytes);
        let home = tempfile::tempdir().unwrap();
        let manager = lookup_manager(home.path(), lookup_of(&[first.path(), second.path()], None), None);

        pull_layer_of(&manager, &layer)
            .await
            .expect("the second directory holds a clean copy");

        assert!(layer_is_extracted(&manager, &layer));
        assert!(
            std::fs::read(ocx_oci::blob_lookup::blob_data_path(
                first.path(),
                LOGICAL,
                &ocx_oci::Digest::try_from(layer.digest.as_str()).unwrap()
            ))
            .unwrap()
                == tampered(&bytes),
            "a blob directory is never written"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_directory_copy_with_nothing_else_is_65_offline() {
        let (bytes, layer) = shared_layer();
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), &layer, &tampered(&bytes));
        let home = tempfile::tempdir().unwrap();
        let manager = lookup_manager(home.path(), lookup_of(&[dir.path()], None), None);

        let error = pull_layer_of(&manager, &layer)
            .await
            .expect_err("only a bad copy exists");

        assert_eq!(exit_code(&error), Some(ocx_exit::ExitCode::DataError), "{error}");
        assert!(!layer_is_extracted(&manager, &layer));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_directory_copy_stays_65_when_the_network_fails() {
        let (bytes, layer) = shared_layer();
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), &layer, &tampered(&bytes));
        let home = tempfile::tempdir().unwrap();
        // The stub serves no blob, so the network step fails after the directory copy was refused.
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let manager = lookup_manager(home.path(), lookup_of(&[dir.path()], None), Some(client));

        let error = pull_layer_of(&manager, &layer)
            .await
            .expect_err("nothing serves a clean copy");

        assert_eq!(exit_code(&error), Some(ocx_exit::ExitCode::DataError), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_anywhere_offline_is_81() {
        let (_, layer) = shared_layer();
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let manager = lookup_manager(home.path(), lookup_of(&[dir.path()], None), None);

        let error = pull_layer_of(&manager, &layer)
            .await
            .expect_err("no source holds the layer");

        assert_eq!(exit_code(&error), Some(ocx_exit::ExitCode::PolicyBlocked), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_cache_entry_is_removed_and_offline_is_81() {
        let (bytes, layer) = shared_layer();
        let cache = tempfile::tempdir().unwrap();
        plant(cache.path(), &layer, &tampered(&bytes));
        let home = tempfile::tempdir().unwrap();
        let manager = lookup_manager(home.path(), lookup_of(&[], Some(cache.path())), None);

        let error = pull_layer_of(&manager, &layer)
            .await
            .expect_err("the cache copy is bad");

        assert_eq!(
            exit_code(&error),
            Some(ocx_exit::ExitCode::PolicyBlocked),
            "a cache-only mismatch is a miss, never 65: {error}"
        );
        let digest = ocx_oci::Digest::try_from(layer.digest.as_str()).unwrap();
        assert!(
            !ocx_oci::blob_lookup::blob_data_path(cache.path(), LOGICAL, &digest).exists(),
            "the bad cache entry is deleted"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn layer_absent_from_an_export_is_79() {
        let (_, layer) = shared_layer();
        let export = tempfile::tempdir().unwrap();
        let blobs = export.path().join("blobs");
        std::fs::create_dir_all(&blobs).unwrap();
        let home = tempfile::tempdir().unwrap();
        let lookup = Arc::new(lookup_of(&[&blobs], None));
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())))
            .with_blob_overlay(
                ocx_oci::client::OverlayMode::LookupOnly,
                home.path().join("blobs"),
                lookup.clone(),
                LOGICAL,
                Vec::new(),
            );
        let manager = lookup_manager(home.path(), (*lookup).clone(), Some(client));

        let error = pull_layer_of(&manager, &layer)
            .await
            .expect_err("the export lacks the layer");

        assert_eq!(exit_code(&error), Some(ocx_exit::ExitCode::NotFound), "{error}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_directory_copy_in_an_export_stays_65() {
        let (bytes, layer) = shared_layer();
        let export = tempfile::tempdir().unwrap();
        let blobs = export.path().join("blobs");
        plant(&blobs, &layer, &tampered(&bytes));
        let home = tempfile::tempdir().unwrap();
        let lookup = Arc::new(lookup_of(&[&blobs], None));
        let client = ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())))
            .with_blob_overlay(
                ocx_oci::client::OverlayMode::LookupOnly,
                home.path().join("blobs"),
                lookup.clone(),
                LOGICAL,
                Vec::new(),
            );
        let manager = lookup_manager(home.path(), (*lookup).clone(), Some(client));

        let error = pull_layer_of(&manager, &layer)
            .await
            .expect_err("the export copy is bad");

        assert_eq!(exit_code(&error), Some(ocx_exit::ExitCode::DataError), "{error}");
    }

    // ── provenance gate ──────────────────────────────────────────────────────

    /// Pulls a one-layer package whose layer sits in a blob directory and whose leaf manifest was read from
    /// `leaf_origin`, with no provided metadata so the registry-pull path runs; returns the recorded origins.
    async fn origins_after_pull(leaf_origin: ocx_index::BlobOrigin) -> Vec<String> {
        let (bytes, layer) = shared_layer();
        let blobs = tempfile::tempdir().unwrap();
        plant(blobs.path(), &layer, &bytes);
        let home = tempfile::tempdir().unwrap();
        let manager = lookup_manager(home.path(), lookup_of(&[blobs.path()], None), None);

        let config = serde_json::to_vec(&bundle_metadata()).unwrap();
        let config_digest = ocx_oci::Algorithm::Sha256.hash(&config);
        manager
            .file_structure()
            .blobs
            .write_blob(LOGICAL, &config_digest, &config)
            .await
            .unwrap();

        let pinned = plain_pinned();
        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: Ok(ocx_oci::OciIdentifier::from_parts("test/lookup", LOGICAL).pinned_at(pinned.digest())),
            chain: vec![ChainBlob {
                identifier: pinned.clone(),
                role: ChainRole::Manifest,
                media_type: ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                size: 2,
            }],
            final_manifest: ocx_oci::ImageManifest {
                artifact_type: Some(super::MEDIA_TYPE_PACKAGE_V1.to_string()),
                config: ocx_oci::Descriptor {
                    media_type: ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                    digest: config_digest.to_string(),
                    size: config.len() as i64,
                    urls: None,
                    artifact_type: None,
                    annotations: None,
                },
                layers: vec![layer],
                ..Default::default()
            },
            platform: linux_amd64(),
            leaf_origin,
        };
        let info = setup_owned(
            &manager,
            &pinned,
            resolved,
            linux_amd64(),
            SetupGroups::new(),
            None,
            None,
        )
        .await
        .expect("the package sets up from the lookup");
        info.dir().recorded_origins()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lookup_hit_records_no_origin() {
        let lookup_hit = ocx_index::BlobOrigin::Lookup(ocx_oci::blob_lookup::LookupOrigin::Cache);
        assert!(
            origins_after_pull(lookup_hit).await.is_empty(),
            "a lookup hit mints no origin"
        );
        assert!(
            origins_after_pull(ocx_index::BlobOrigin::Home).await.is_empty(),
            "a home hit mints no origin"
        );
        assert_eq!(
            origins_after_pull(ocx_index::BlobOrigin::Network).await.len(),
            1,
            "a network read still records the origin"
        );
    }

    /// A root with one dependency in a fresh offline home; returns both pinned refs and the manager.
    async fn root_with_dependency() -> (
        PackageManager,
        ocx_oci::PackageRef,
        ocx_oci::PinnedPackageRef,
        ocx_oci::PinnedPackageRef,
        tempfile::TempDir,
    ) {
        let (manager, file_structure, dir) = offline_manager(crate::activity::ActivityLog::new());
        let dep_digest =
            write_zero_layer_package(&file_structure, serde_json::to_value(bundle_metadata()).unwrap()).await;
        let root_metadata = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [],
            "dependencies": [{ "identifier": format!("ocx.sh/acme/dep:1.0@{dep_digest}") }],
        });
        let root_digest = write_zero_layer_package(&file_structure, root_metadata).await;
        let root = ocx_oci::PackageRef::new_registry("acme/root", "ocx.sh").clone_with_digest(root_digest);
        let root_pinned = ocx_oci::PinnedPackageRef::try_from(root.clone()).unwrap();
        let dep = ocx_oci::PackageRef::new_registry("acme/dep", "ocx.sh").clone_with_digest(dep_digest);
        let dep_pinned = ocx_oci::PinnedPackageRef::try_from(dep).unwrap();
        (manager, root, root_pinned, dep_pinned, dir)
    }

    async fn origins_of(manager: &PackageManager, pinned: &ocx_oci::PinnedPackageRef) -> Vec<String> {
        manager.file_structure().packages.package_dir(pinned).recorded_origins()
    }

    /// The prefetch walk leaves home copies, so each dependency it read from a source must still record one.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dependency_the_prefetch_read_from_a_source_records_its_origin() {
        let (manager, root, root_pinned, dep_pinned, _dir) = root_with_dependency().await;
        let groups = SetupGroups::new().with_network_leaves([dep_pinned.clone()].into());

        super::setup_impl(
            &manager,
            &root,
            ocx_oci::Platform::any(),
            groups,
            None,
            crate::activity::Role::Root,
        )
        .await
        .expect("the pull succeeds");

        assert_eq!(origins_of(&manager, &dep_pinned).await.len(), 1);
        assert!(
            origins_of(&manager, &root_pinned).await.is_empty(),
            "the root's leaf was a home hit"
        );
    }

    /// A network record is bound to its digest: another package's record mints nothing here.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_network_record_for_another_digest_mints_no_origin() {
        let (manager, root, root_pinned, dep_pinned, _dir) = root_with_dependency().await;
        let other = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("acme/other", "ocx.sh")
                .clone_with_digest(ocx_oci::Digest::Sha256("e".repeat(64))),
        )
        .unwrap();
        let groups = SetupGroups::new().with_network_leaves([other].into());

        super::setup_impl(
            &manager,
            &root,
            ocx_oci::Platform::any(),
            groups,
            None,
            crate::activity::Role::Root,
        )
        .await
        .expect("the pull succeeds");

        assert!(origins_of(&manager, &root_pinned).await.is_empty());
        assert!(origins_of(&manager, &dep_pinned).await.is_empty());
    }

    // ── vendored destination ─────────────────────────────────────────────────

    /// A gzipped tar of `bin/tool` (executable), a link `bin/alias -> tool` and `share/readme.txt`.
    fn vendor_layer() -> (Vec<u8>, ocx_oci::Descriptor) {
        use std::io::Write as _;

        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, content, mode) in [
                ("bin/tool", &b"#!tool"[..], 0o755),
                ("share/readme.txt", b"read me", 0o644),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(mode);
                header.set_cksum();
                builder.append_data(&mut header, name, content).unwrap();
            }
            // A Windows host refuses an in-layer symlink (`WindowsSymlinksUnsupported`), so only Unix gets the link.
            if cfg!(unix) {
                let mut link = tar::Header::new_gnu();
                link.set_entry_type(tar::EntryType::Symlink);
                link.set_size(0);
                link.set_mode(0o777);
                builder.append_link(&mut link, "bin/alias", "tool").unwrap();
            }
            builder.finish().unwrap();
        }
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        let bytes = encoder.finish().unwrap();
        let descriptor = ocx_oci::Descriptor {
            media_type: ocx_oci::media_type::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: ocx_oci::Algorithm::Sha256.hash(&bytes).to_string(),
            size: bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        (bytes, descriptor)
    }

    /// Vendors the one-layer fixture package from a blob directory into `out`; returns the manager, the
    /// target and the outcome.
    async fn vendor_fixture_package(
        home: &std::path::Path,
        out: &std::path::Path,
    ) -> (
        PackageManager,
        super::VendorTarget,
        Result<InstallInfo, PackageErrorKind>,
    ) {
        let (bytes, layer) = vendor_layer();
        let blobs = tempfile::tempdir().unwrap();
        plant(blobs.path(), &layer, &bytes);
        let manager = lookup_manager(home, lookup_of(&[blobs.path()], None), None);
        let pinned = plain_pinned();
        let resolved = ResolvedChain {
            pinned: pinned.clone(),
            transport_pinned: Ok(ocx_oci::OciIdentifier::from_parts("test/lookup", LOGICAL).pinned_at(pinned.digest())),
            chain: Vec::new(),
            final_manifest: ocx_oci::ImageManifest {
                artifact_type: Some(super::MEDIA_TYPE_PACKAGE_V1.to_string()),
                layers: vec![layer.clone()],
                ..Default::default()
            },
            platform: linux_amd64(),
            leaf_origin: ocx_index::BlobOrigin::Home,
        };
        let layout = ocx_store::file_structure::VendorLayout::new(out);
        let target = super::VendorTarget::new(&layout, &plain_key());
        let outcome = super::setup_vendored(
            &manager,
            &pinned,
            resolved,
            linux_amd64(),
            SetupGroups::new(),
            &target,
            Some(bundle_metadata()),
        )
        .await;
        (manager, target, outcome)
    }

    /// The vendored name of the fixture package.
    fn plain_key() -> ocx_store::file_structure::PackageKey {
        let digest = plain_pinned().digest();
        ocx_store::file_structure::VendorLayout::package_keys([&digest]).expect("one key")[&digest].clone()
    }

    /// The package directory holds `c/` alone: no store bookkeeping, nothing a GC would follow.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_vendored_package_holds_only_its_content() {
        let (home, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (manager, target, outcome) = vendor_fixture_package(home.path(), out.path()).await;

        let info = outcome.expect("the package vendors");

        let mut names: Vec<_> = std::fs::read_dir(target.package_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["c"]);
        assert_eq!(info.dir().root(), target.package_dir());
        assert_eq!(
            std::fs::read(target.content_dir().join("share/readme.txt")).unwrap(),
            b"read me"
        );
        assert!(
            !manager.file_structure().packages.path(&plain_pinned()).exists(),
            "a vendored package is not installed in the store"
        );
    }

    /// Hardlinks into the layer store would leave the volume `$OCX_HOME` sits on.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_vendored_file_is_a_copy_not_a_hardlink() {
        use std::os::unix::fs::MetadataExt as _;

        let (home, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_manager, target, outcome) = vendor_fixture_package(home.path(), out.path()).await;
        outcome.expect("the package vendors");

        let metadata = std::fs::metadata(target.content_dir().join("bin/tool")).unwrap();
        assert_eq!(metadata.nlink(), 1, "the file shares no inode with the layer store");
        assert_ne!(metadata.mode() & 0o111, 0, "the executable bit survives the copy");
    }

    /// The listing of a vendored package: the files with their executable bit, the link, the link-free directory.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_vendored_package_lists_its_tree() {
        let (home, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (_manager, _target, outcome) = vendor_fixture_package(home.path(), out.path()).await;
        outcome.expect("the package vendors");

        let layout = ocx_store::file_structure::VendorLayout::new(out.path());
        let content = ocx_store::file_structure::VendorLayout::relative_content_dir(&plain_key());
        let tree = crate::tasks::vendor::list_tree(&layout, &content, false)
            .await
            .expect("the tree lists");

        let file = |name: &str, executable| ocx_package::vendor::FileEntry {
            path: format!("{content}/{name}"),
            executable,
        };
        assert_eq!(tree.files, [file("bin/tool", true), file("share/readme.txt", false)]);
        assert_eq!(
            tree.links,
            [ocx_package::vendor::LinkEntry {
                path: format!("{content}/bin/alias"),
                target: "tool".into(),
            }]
        );
        assert_eq!(tree.link_free_dirs, [format!("{content}/share")]);
    }

    /// A layer whose decompressed size passes the cap, as a blob-directory copy with a matching digest.
    fn bomb_layer() -> (Vec<u8>, ocx_oci::Descriptor) {
        use std::io::Read as _;

        let size = (257u64) << 20;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(size);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "bomb.bin", std::io::repeat(0).take(size))
                .unwrap();
            builder.finish().unwrap();
        }
        let bytes = encoder.finish().unwrap();
        let descriptor = ocx_oci::Descriptor {
            media_type: ocx_oci::media_type::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: ocx_oci::Algorithm::Sha256.hash(&bytes).to_string(),
            size: bytes.len() as i64,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        (bytes, descriptor)
    }

    /// An over-cap lookup copy is rejected like a bad copy, so the walk ends in the bad-copy verdict (65)
    /// rather than the first source's cap error.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_over_cap_lookup_copy_is_rejected_and_the_walk_continues() {
        let (bytes, layer) = bomb_layer();
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        plant(first.path(), &layer, &bytes);
        plant(second.path(), &layer, &bytes);
        let home = tempfile::tempdir().unwrap();
        let manager = lookup_manager(home.path(), lookup_of(&[first.path(), second.path()], None), None);

        let error = pull_layer_of(&manager, &layer)
            .await
            .expect_err("no source holds a usable copy");

        assert!(
            format!("{error:?}").contains("DigestMismatch"),
            "expected the bad-copy verdict, got {error:?}"
        );
        assert!(!layer_is_extracted(&manager, &layer));
    }
}
