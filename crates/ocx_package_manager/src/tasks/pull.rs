// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

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
}

impl SetupGroups {
    pub fn new() -> Self {
        Self {
            packages: SetupGroup::new(MAX_NODES, PACKAGE_SETUP_TIMEOUT),
            layers: LayerGroup::new(MAX_NODES, LAYER_SETUP_TIMEOUT),
        }
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
        let groups = SetupGroups::new();
        setup_with_tracker(self, package, platform, groups)
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

        let shared_groups = SetupGroups::new();
        let semaphore = concurrency.semaphore();
        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platform = platform.clone();
            let groups = shared_groups.clone();
            let sem = semaphore.clone();
            tasks.spawn(async move {
                // Named, not `_`: dropping it before setup returns lifts the root-pull cap.
                let _permit = super::super::concurrency::acquire_permit(&sem).await;
                let result = setup_with_tracker(&mgr, &package, platform, groups).await;
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

fn setup_with_tracker<'a>(
    mgr: &'a PackageManager,
    package: &ocx_oci::PackageRef,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
) -> Pin<Box<dyn Future<Output = Result<InstallInfo, PackageErrorKind>> + Send + 'a>> {
    let package = package.clone();
    Box::pin(async move { setup_impl(mgr, &package, platform, groups, None).await })
}

/// Singleflight-gated core of [`PackageManager::pull`]; `dest_override` is forwarded to [`setup_owned`].
async fn setup_impl(
    mgr: &PackageManager,
    package: &ocx_oci::PackageRef,
    platform: ocx_oci::Platform,
    groups: SetupGroups,
    dest_override: Option<&std::path::Path>,
) -> Result<InstallInfo, PackageErrorKind> {
    log::debug!("Pulling package: {}", package);

    let resolved = mgr.resolve(package, platform.clone()).await?;
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
    if let Err(kind) = mgr.maybe_auto_verify(pinned.as_identifier()).await {
        let shared = handle.fail(kind);
        return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
    }

    // Fail the handle explicitly: a dropped one tells waiters only "abandoned".
    match setup_owned(mgr, &pinned, resolved, platform, groups, dest_override, None).await {
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

/// Downloads, sets up dependencies and places the package; call only while owning the singleflight handle.
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
    .map(|info| {
        let info = info.with_platform(resolved_platform);
        match transport_registry {
            Some(registry) => info.with_transport_registry(registry),
            None => info,
        }
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
) -> Result<InstallInfo, PackageErrorKind> {
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
            return Ok(info);
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
            return Ok(info);
        }
    }

    let manifest = resolved.final_manifest.clone();
    let metadata = if let Some(meta) = provided_metadata {
        meta
    } else {
        super::common::load_config_metadata(mgr.index(), pinned, &manifest)
            .await?
            .into()
    };

    // Zero layers is valid: a config-only package with an empty `content/`.
    if manifest.artifact_type.as_deref() != Some(MEDIA_TYPE_PACKAGE_V1) {
        return Err(PackageErrorKind::from(
            ocx_oci::client::error::ClientError::UnexpectedArtifactType {
                expected: MEDIA_TYPE_PACKAGE_V1.to_string(),
                actual: manifest.artifact_type.clone(),
            },
        ));
    }

    let pkg = file_structure::PackageDir::with_root(temp.dir.dir.clone());

    // `consent::verified_sources` trusts this record, so `pull_local` must never write one,
    // or unverified content passes as verified.
    if from_registry {
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

    let (layer_digests, dependencies) = tokio::join!(
        extract_layers(
            mgr,
            pinned,
            &resolved.transport_pinned,
            &manifest,
            groups.layers.clone()
        ),
        setup_dependencies(mgr, &metadata, pinned, platform, groups.clone()),
    );
    let (layer_digests, dependencies) = (layer_digests?, dependencies?);

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

    let layer_contents: Vec<std::path::PathBuf> = layer_digests
        .iter()
        .map(|d| fs.layers.content(pinned.registry(), d))
        .collect();
    let sources: Vec<&std::path::Path> = layer_contents.iter().map(AsRef::as_ref).collect();
    // Layers are stored unstripped, so placement applies here; `placements` aligns 1:1 with
    // `sources` only because `extract_layers` returns manifest order.
    let bundle_strip = metadata.strip_components();
    let mut placements: Vec<ocx_util::fs::path::LayerPlacement> = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let placement = ocx_oci::resolve_layer_placement(layer.annotations.as_ref(), bundle_strip)
            .map_err(|e| PackageErrorKind::Internal(crate::Error::LayerLayout(e)))?;
        placements.push(placement);
    }
    ocx_store::file_structure::assemble_from_layers_with_layouts(&sources, &placements, &pkg.content())
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    if let Some(entrypoints) = metadata.entrypoints()
        && !entrypoints.is_empty()
    {
        let dest = pkg.entrypoints();
        // The post-move root, not the temp path, or every launcher breaks after the rename.
        let final_pkg_root = dest_override
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| fs.packages.path(pinned));
        crate::launcher::generate(&final_pkg_root, entrypoints, &dest, &fs.shim_bin)
            .await
            .map_err(PackageErrorKind::Internal)?;
    }

    // The `zip` relies on `setup_dependencies` returning declaration order.
    let resolved_package = ResolvedPackage::new().with_dependencies(
        metadata
            .dependencies()
            .iter()
            .zip(dependencies.iter())
            .map(|(decl, info)| (info.identifier().clone(), info.resolved().clone(), decl.visibility)),
    );

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
    Ok(install_info)
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
            let info = setup_with_tracker(&mgr, &dep_id, platform, groups).await?;
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
) -> Result<Vec<ocx_oci::Digest>, PackageErrorKind> {
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

    let mut tasks: JoinSet<(usize, Result<ocx_oci::Digest, PackageErrorKind>)> = JoinSet::new();
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
    while let Some(join_res) = tasks.join_next().await {
        let (idx, task_res) = match join_res {
            Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
            Err(e) => panic!("layer extraction task aborted: {e}"),
            Ok(v) => v,
        };
        results[idx] = Some(task_res?);
    }
    Ok(results.into_iter().flatten().collect())
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
) -> Result<ocx_oci::Digest, PackageErrorKind> {
    let fs = mgr.file_structure();
    let registry = pinned.registry().to_string();

    let key = (registry.clone(), layer_digest.clone());
    let handle = match layer_group.try_acquire(key).await.map_err(map_singleflight_error)? {
        singleflight::Acquisition::Resolved(()) => {
            log::debug!("Layer {} already extracted by another task, reusing.", layer_digest);
            return Ok(layer_digest.clone());
        }
        singleflight::Acquisition::Leader(handle) => handle,
    };

    // Before the client is required, so an offline manager can re-assemble from cached layers.
    let layer_content = fs.layers.content(pinned.registry(), layer_digest);
    if ocx_util::fs::path_exists_lossy(&layer_content).await {
        log::debug!("Layer {} present on disk, skipping fetch.", layer_digest);
        handle.complete(());
        return Ok(layer_digest.clone());
    }

    let transport = match transport {
        Ok(transport) => transport,
        Err(absent) => {
            let shared = handle.fail(PackageErrorKind::Internal(absent.missing_layer(pinned, layer_digest)));
            return Err(map_singleflight_error(singleflight::Error::Failed(shared)));
        }
    };

    let client = match mgr.require_client() {
        Ok(c) => c,
        Err(e) => {
            let kind = PackageErrorKind::Internal(e);
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
    pinned: &ocx_oci::PinnedPackageRef,
    transport: &ocx_oci::PinnedOciIdentifier,
    layer: &ocx_oci::Descriptor,
    layer_digest: &ocx_oci::Digest,
    client: &ocx_oci::Client,
    fs: &file_structure::FileStructure,
) -> Result<(), PackageErrorKind> {
    let layer_content = fs.layers.content(pinned.registry(), layer_digest);
    if ocx_util::fs::path_exists_lossy(&layer_content).await {
        log::debug!("Layer {} already present on disk, skipping.", layer_digest);
        return Ok(());
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
        return Ok(());
    }

    // Fetched from the physical `transport`, but storage paths stay keyed on the logical
    // `pinned` (`adr_index_indirection.md`).
    client.pull_layer(transport, layer, &temp.dir.dir).await?;

    // Signed before anything reads `content/`: nothing may link an unsigned Mach-O out of the layer store.
    ocx_store::codesign::sign_extracted_content(&temp.dir.dir.join("content"))
        .await
        .map_err(ocx_oci::client::error::ClientError::internal)?;

    file_structure::write_digest_file(&temp.dir.dir.join(file_structure::DIGEST_FILENAME), layer_digest)
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;

    super::layer_staging::finalize_layer_dir(fs, pinned.registry(), layer_digest, &temp.dir.dir).await?;

    drop(temp);
    Ok(())
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
}
