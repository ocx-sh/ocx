// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx vendor` and `ocx package vendor`: write a relocatable directory of packages, launchers and a manifest.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::{StreamExt, TryStreamExt};
use ocx_env_apply::ApplyKind;
use ocx_env_apply::launch::{DirectRecord, EnvRecord, LaunchKind, RECORD_EXTENSION, SCHEMA_VERSION as SIDECAR_VERSION};
use ocx_oci::{Digest, OperatingSystem, PackageRef, PinnedPackageRef, Platform};
use ocx_package::install_info::InstallInfo;
use ocx_package::metadata::Entrypoints;
use ocx_package::metadata::env::modifier::{Modifier, ModifierKind};
use ocx_package::metadata::template::{TemplateResolver, Usage};
use ocx_package::vendor::{
    Content, DIR_TOKEN, EntryPoint, Environment, EnvironmentItem, EnvironmentScope, FileEntry, LinkEntry,
    MANIFEST_FILE, Package, SCHEMA_VERSION, Selection as ManifestSelection, Tool, ToolKind, Tree, VendorManifest,
    vendor_id,
};
use ocx_project::{LockedTool, SelectedTool, ToolSource, resolve_selected_tools};
use ocx_store::file_structure::{PackageKey, StagedTree, VendorLayout, normalise_written_file};
use ocx_util::fs::{DirWalker, EmptyOrAbsentError, WalkDecision, ensure_empty_or_absent};

use super::common::ClosureNode;
use super::inspect::{InspectOptions, InspectResult};
use super::pull::{SetupGroups, VendorTarget, setup_vendored};
use super::resolve::{ChainRole, EnvScope};
use super::toolchain_names::{NotEnumerablePolicy, exposed_names, fold_case_insensitive, is_record_name};
use crate::PackageManager;
use crate::error::{PackageError, PackageErrorKind};

/// What to vendor.
#[derive(Debug, Clone)]
pub enum VendorSelection {
    /// What `ocx exec` selects from the project lock: `select_tool_set`, the name filter and the duplicate
    /// check already applied by the caller, host leaves still unresolved.
    Project {
        selected: Vec<SelectedTool>,
        /// The selected groups after `all` expansion, as the manifest records them.
        groups: Vec<String>,
    },
    /// Named packages, resolved as every `ocx package` verb resolves them.
    Packages(Vec<PackageRef>),
}

/// What `ocx vendor` and `ocx package vendor` were asked to write.
#[derive(Debug, Clone)]
pub struct VendorRequest {
    /// The vendor directory: absent or empty, never a symlink.
    pub output: PathBuf,
    pub platform: Platform,
    pub selection: VendorSelection,
    /// Write `manifest.json` only: no package content, no launcher.
    pub manifest_only: bool,
    /// Create no symlink under the directory.
    pub omit_links: bool,
}

/// Whether the run wrote package content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VendorContent {
    Present,
    Absent,
}

/// One package of a vendored set.
#[derive(Debug, Clone)]
pub struct VendoredPackage {
    pub identifier: PackageRef,
    pub digest: Digest,
}

#[derive(Debug, Clone)]
pub struct VendorOutcome {
    pub output_dir: PathBuf,
    pub manifest_path: PathBuf,
    pub content: VendorContent,
    pub platform: Platform,
    pub packages: Vec<VendoredPackage>,
    pub tool_count: usize,
    /// Bytes this run wrote.
    pub written_size: u64,
}

/// Why a vendor run refused or failed.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum VendorError {
    /// The output directory exists and is not an empty directory, or is a symlink.
    #[error("vendor output '{}' exists and is not an empty directory; name a new or empty directory", .path.display())]
    #[exit(
        UsageError,
        slug = "vendor_output_not_empty",
        summary = "The vendor output directory exists and is not an empty directory"
    )]
    OutputNotEmpty { path: PathBuf },
    /// A resolved value or an environment record cannot be written as a vendor-relative value.
    #[error("value cannot be vendored: {reason}")]
    #[exit(
        DataError,
        slug = "vendor_value_unrepresentable",
        summary = "A value cannot be written relative to the vendor directory"
    )]
    ValueUnrepresentable { reason: String },
    /// A program, launcher or link target breaks the vendor path rules.
    #[error("path cannot be vendored: {path}")]
    #[exit(
        DataError,
        slug = "vendor_path_unrepresentable",
        summary = "A path cannot be written inside the vendor directory"
    )]
    PathUnrepresentable { path: String },
    /// Resolution, pull, store or I/O failure; classified as its own error.
    #[error(transparent)]
    #[exit(delegate = 0)]
    Package(#[from] crate::Error),
}

impl PackageManager {
    /// Writes the vendor directory `request` describes.
    ///
    /// # Errors
    ///
    /// [`VendorError::OutputNotEmpty`] before any network request, [`VendorError::ValueUnrepresentable`] and
    /// [`VendorError::PathUnrepresentable`] for a value or path the directory cannot hold; resolution, pull and
    /// I/O failures as [`VendorError::Package`].
    pub async fn vendor(&self, request: VendorRequest) -> Result<VendorOutcome, VendorError> {
        ensure_output_is_free(&request.output).await?;
        let selection = select(self, &request).await?;
        let mut manifest = build_manifest(self, &request, &selection, &|platform| {
            ocx_store::shim::blob_for(platform).is_some()
        })
        .await?;
        let staged = stage(&request.output).await?;
        let content = if request.manifest_only {
            VendorContent::Absent
        } else {
            write_content(self, &request, &selection, &mut manifest, staged.path()).await?;
            VendorContent::Present
        };
        // After the last index read; `None` without `--index`.
        manifest.index_lookups = self.index_lookups();
        let written_size = publish(&request.output, staged, &manifest).await?;
        Ok(VendorOutcome {
            manifest_path: VendorLayout::new(&request.output).manifest_file(),
            output_dir: request.output,
            content,
            platform: request.platform,
            packages: manifest_packages(&selection),
            tool_count: manifest.tools.len(),
            written_size,
        })
    }
}

/// `DIR` is absent or an empty directory, and never a symlink, before any request is made.
async fn ensure_output_is_free(output: &Path) -> Result<(), VendorError> {
    let refused = || VendorError::OutputNotEmpty {
        path: output.to_path_buf(),
    };
    if tokio::fs::symlink_metadata(output)
        .await
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(refused());
    }
    match ensure_empty_or_absent(output).await {
        Ok(()) => Ok(()),
        Err(EmptyOrAbsentError::Io { path, source }) => {
            Err(VendorError::Package(crate::Error::InternalFile(path, source)))
        }
        Err(_) => Err(refused()),
    }
}

/// The selected packages, each with its metadata-only dependency closure, and the platforms they all offer.
#[derive(Debug)]
struct Selection {
    closures: ClosureSet,
    /// The vendored name of every package of the run, built once so every path writer agrees.
    keys: BTreeMap<Digest, PackageKey>,
    /// The platforms on which every root has a leaf, computed before any leaf was chosen.
    available_platforms: Vec<String>,
}

/// The nodes of several closures laid end to end, as `toolchain_surface` merges them, and where each root's own run sits.
///
/// One slice, so the name algebra walks every root's nodes in the one order it walks a rendered toolchain's.
#[derive(Debug)]
struct ClosureSet {
    roots: Vec<SelectedRoot>,
    surface: Vec<ClosureNode>,
}

impl ClosureSet {
    /// `root`'s closure: its dependencies first and the root last.
    fn nodes(&self, root: &SelectedRoot) -> &[ClosureNode] {
        &self.surface[root.nodes.clone()]
    }
}

/// One package the run vendors, resolved for the requested platform.
#[derive(Debug)]
struct SelectedRoot {
    /// The `ocx.toml` binding; `None` for a package named on the command line.
    binding: Option<String>,
    /// As the selection named it: a lock leaf, or the identifier on the command line.
    requested: PackageRef,
    pinned: PinnedPackageRef,
    /// The image index the request resolved through; `None` when it named a platform manifest.
    index_digest: Option<Digest>,
    /// Where this root's closure sits in [`ClosureSet::surface`].
    nodes: Range<usize>,
}

/// A package the selection names, before its leaf and closure are read.
struct PickedRoot {
    binding: Option<String>,
    requested: PackageRef,
}

/// Selects the packages `request` names, reads their closures and checks their trust, all from manifests.
///
/// No layer is fetched here, so the manifest-only run and the full run share every refusal that precedes content.
async fn select(manager: &PackageManager, request: &VendorRequest) -> Result<Selection, VendorError> {
    let (picked, available_platforms) = match &request.selection {
        VendorSelection::Project { selected, .. } => {
            // From the lock's keys, before a leaf is chosen for `request.platform`, so every `-p` sees the same set.
            let available = locked_platforms(selected.iter().filter_map(|tool| match &tool.source {
                ToolSource::Locked(bound) => Some(bound.locked()),
                ToolSource::Explicit(_) => None,
            }));
            // The `no_host_leaf` (78) refusal `ocx exec` raises, before the network.
            let picked = resolve_selected_tools(selected, &request.platform)
                .map_err(|error| VendorError::Package(error.into()))?
                .into_iter()
                .map(|tool| PickedRoot {
                    binding: Some(tool.binding),
                    requested: tool.identifier,
                })
                .collect();
            (picked, available)
        }
        VendorSelection::Packages(packages) => {
            let available = index_platforms(manager, packages).await?;
            let picked = packages
                .iter()
                .map(|package| PickedRoot {
                    binding: None,
                    requested: package.clone(),
                })
                .collect();
            (picked, available)
        }
    };
    let closures = read_closures(manager, picked, &request.platform).await?;
    verify_selection(manager, &closures).await?;
    let keys = package_keys_of(&closures.surface)?;
    Ok(Selection {
        closures,
        keys,
        available_platforms,
    })
}

/// The platforms every one of `offers` serves; an offer holding `any` has a leaf everywhere and narrows nothing.
fn shared_platforms(offers: impl IntoIterator<Item = BTreeSet<String>>) -> Vec<String> {
    let any = Platform::any().to_string();
    let mut shared: Option<BTreeSet<String>> = None;
    for offer in offers.into_iter().filter(|offer| !offer.contains(&any)) {
        shared = Some(match shared {
            None => offer,
            Some(known) => known.intersection(&offer).cloned().collect(),
        });
    }
    shared.map_or_else(|| vec![any], |shared| shared.into_iter().collect())
}

/// The platforms a `-p` request can name: those on which every locked tool has the leaf `ocx exec` would pick.
///
/// Candidates are the lock's own keys, each tried through [`LockedTool::host_leaf`], so a `linux/amd64` leaf serves a
/// `linux/amd64+libc.glibc` request as it does on a glibc host. An `any` leaf and an explicit tool narrow nothing.
fn locked_platforms<'a>(tools: impl IntoIterator<Item = &'a LockedTool>) -> Vec<String> {
    let any = Platform::any().to_string();
    let tools: Vec<_> = tools
        .into_iter()
        .filter(|tool| !tool.platforms.contains_key(&any))
        .collect();
    if tools.is_empty() {
        return vec![any];
    }
    let candidates: BTreeSet<&String> = tools.iter().flat_map(|tool| tool.platforms.keys()).collect();
    candidates
        .into_iter()
        .filter(|key| {
            key.parse::<Platform>()
                .is_ok_and(|platform| tools.iter().all(|tool| tool.host_leaf(&platform).is_ok()))
        })
        .cloned()
        .collect()
}

/// The platforms `packages` serve together, from their image indexes; a flat image manifest serves every platform.
async fn index_platforms(manager: &PackageManager, packages: &[PackageRef]) -> Result<Vec<String>, VendorError> {
    let inspected = manager
        .inspect_all(packages.to_vec(), Platform::any(), InspectOptions::default())
        .await
        .map_err(VendorError::Package)?;
    Ok(shared_platforms(inspected.into_iter().map(offered_platforms)))
}

/// The platforms one inspected package offers; a flat manifest or a resolved leaf offers `any`.
fn offered_platforms(result: InspectResult) -> BTreeSet<String> {
    match result {
        InspectResult::Candidates { candidates, .. } => candidates
            .into_iter()
            .map(|candidate| candidate.platform.to_string())
            .collect(),
        InspectResult::Manifest { .. } | InspectResult::Resolved { .. } => {
            BTreeSet::from([Platform::any().to_string()])
        }
    }
}

/// Resolves every pick for `platform` and reads its closure from config blobs alone.
async fn read_closures(
    manager: &PackageManager,
    picked: Vec<PickedRoot>,
    platform: &Platform,
) -> Result<ClosureSet, VendorError> {
    let requested: Vec<PackageRef> = picked.iter().map(|root| root.requested.clone()).collect();
    let options = InspectOptions {
        resolve: true,
        closure: true,
    };
    let inspected = manager
        .inspect_all(requested, platform.clone(), options)
        .await
        .map_err(VendorError::Package)?;
    let mut roots = Vec::with_capacity(picked.len());
    let mut surface = Vec::new();
    for (root, result) in picked.into_iter().zip(inspected) {
        let InspectResult::Resolved {
            pinned,
            chain,
            closure: Some(closure),
            ..
        } = result
        else {
            return Err(failed(
                &root.requested,
                PackageErrorKind::Internal(crate::error::file_error(
                    root.requested.to_string(),
                    std::io::Error::other("inspect returned no resolved closure"),
                )),
            ));
        };
        let start = surface.len();
        surface.extend(closure.nodes);
        let index_digest = chain
            .chain
            .first()
            .filter(|blob| blob.role == ChainRole::Index)
            .map(|blob| blob.identifier.digest());
        roots.push(SelectedRoot {
            binding: root.binding,
            requested: root.requested,
            pinned,
            index_digest,
            nodes: start..surface.len(),
        });
    }
    Ok(ClosureSet { roots, surface })
}

/// The trust checks an install runs before it fetches a layer, once per package of the closure.
///
/// Sequential: a closure is a handful of packages and the first refusal must be the same one on every run.
async fn verify_selection(manager: &PackageManager, closures: &ClosureSet) -> Result<(), VendorError> {
    // `--index DIR` is a read-only input: a vendor run never grows it.
    let manager = &manager.read_only_view();
    let mut verified: Vec<(PinnedPackageRef, bool)> = Vec::new();
    for root in &closures.roots {
        for node in closures.nodes(root) {
            // The request that named a root decides which declaration its publisher round reads; a dependency is pinned.
            let requested = if node.is_root {
                &root.requested
            } else {
                node.identifier.as_identifier()
            };
            // A pinned sighting runs a weaker round than a tagged one, so it never stands in for a tagged sighting.
            let leaf = (node.identifier.strip_advisory(), requested.digest().is_some());
            if verified.contains(&leaf) {
                continue;
            }
            manager
                .maybe_auto_verify_requested(node.identifier.as_identifier(), requested)
                .await
                .map_err(|kind| failed(node.identifier.as_identifier(), kind))?;
            verified.push(leaf);
        }
    }
    Ok(())
}

/// A selection-half failure of `identifier`, classified by its own kind.
fn failed(identifier: &PackageRef, kind: PackageErrorKind) -> VendorError {
    VendorError::Package(crate::Error::ResolveFailed(vec![PackageError::new(
        identifier.clone(),
        kind,
    )]))
}

// ── Document half: the manifest from the selection alone ─────────────────────

/// The selection id of v1: one selection per manifest.
const SELECTION_ID: &str = "default";

/// The id of the selection's composed environment.
const SELECTION_ENVIRONMENT: &str = "selection:default";

fn folds_case(platform: &Platform) -> bool {
    matches!(
        platform,
        Platform::Specific {
            os: OperatingSystem::Windows | OperatingSystem::Darwin,
            ..
        }
    )
}

fn is_windows(platform: &Platform) -> bool {
    matches!(
        platform,
        Platform::Specific {
            os: OperatingSystem::Windows,
            ..
        }
    )
}

/// Builds `manifest.json` from the selection: no layer is read, so `tree` is left to the full run.
///
/// `has_launcher` answers whether a launcher blob exists for the platform; it is asked only when the selection
/// exposes a name.
///
/// # Errors
///
/// [`VendorError::ValueUnrepresentable`] for an environment value that is not a vendor-relative one,
/// [`VendorError::PathUnrepresentable`] for a path breaking the manifest's path rules; resolution failures as
/// [`VendorError::Package`].
async fn build_manifest(
    manager: &PackageManager,
    request: &VendorRequest,
    selection: &Selection,
    has_launcher: &dyn Fn(&Platform) -> bool,
) -> Result<VendorManifest, VendorError> {
    let platform = &request.platform;
    let windows = is_windows(platform);
    let closures = &selection.closures;

    let roots = compose_roots_of(manager, closures).await?;
    check_declared_values(&roots, closures)?;
    let forbidden = host_paths(manager.file_structure().root(), &request.output)?;
    let keys = &selection.keys;
    let selection_items = environment_items(manager, &roots, false, platform, &forbidden, keys).await?;

    // Every package once, keyed by digest, which is also the order the manifest lists them in.
    let mut nodes: BTreeMap<String, &ClosureNode> = BTreeMap::new();
    for node in &closures.surface {
        nodes.entry(node.identifier.digest().to_string()).or_insert(node);
    }
    // Only a root names the index that selected it; a dependency edge pins a platform manifest.
    let index_digests: HashMap<String, String> = closures
        .roots
        .iter()
        .filter_map(|root| {
            Some((
                root.pinned.digest().to_string(),
                root.index_digest.as_ref()?.to_string(),
            ))
        })
        .collect();
    let by_repository: HashMap<(&str, &str), &String> = nodes
        .iter()
        .map(|(digest, node)| ((node.identifier.registry(), node.identifier.repository()), digest))
        .collect();

    let mut packages = Vec::with_capacity(nodes.len());
    let mut entrypoint_environments = Vec::new();
    for (digest, node) in &nodes {
        let key = key_of(keys, &node.identifier.digest())?;
        let package_dir = VendorLayout::relative_package_dir(key);
        let content_dir = VendorLayout::relative_content_dir(key);

        let mut dependencies: Vec<String> = node
            .dependencies
            .iter()
            .filter_map(|edge| {
                let declared = edge.identifier.digest().to_string();
                // An image-index edge names the index digest; its package is the platform leaf of that repository.
                nodes.contains_key(&declared).then_some(declared).or_else(|| {
                    by_repository
                        .get(&(edge.identifier.registry(), edge.identifier.repository()))
                        .map(|digest| (*digest).clone())
                })
            })
            .collect();
        dependencies.sort_unstable();
        dependencies.dedup();

        let mut names: Vec<&str> = node.entrypoints.iter().map(|name| name.as_str()).collect();
        names.sort_unstable();
        let mut entrypoints = Vec::with_capacity(names.len());
        if !names.is_empty() {
            let own = entrypoint_root(manager, &roots, node, platform).await?;
            let items =
                environment_items(manager, std::slice::from_ref(&own), true, platform, &forbidden, keys).await?;
            for name in names {
                let launcher_path =
                    checked_path(VendorLayout::relative_entrypoint_launcher(key, name, windows), windows)?;
                let record_path = checked_path(VendorLayout::relative_entrypoint_record(key, name), windows)?;
                let id = format!("entrypoint:{digest}:{name}");
                entrypoints.push(EntryPoint {
                    name: name.to_owned(),
                    launcher_path,
                    record_path: Some(record_path),
                    environment: id.clone(),
                });
                entrypoint_environments.push(Environment {
                    id,
                    scope: EnvironmentScope::Entrypoint {
                        package: digest.clone(),
                        name: name.to_owned(),
                    },
                    items: items.clone(),
                });
            }
        }

        packages.push(Package {
            identifier: node.identifier.to_string(),
            digest: digest.clone(),
            index_digest: index_digests.get(digest).cloned(),
            package_dir,
            content_dir,
            dependencies,
            names_declared: node.binaries.as_ref().is_some_and(|binaries| !binaries.is_empty())
                || !node.entrypoints.is_empty(),
            entrypoints,
            tree: None,
        });
    }
    entrypoint_environments.sort_by(|a, b| a.id.cmp(&b.id));

    let mut exposed = exposed_names(&closures.surface, NotEnumerablePolicy::Skip).map_err(crate::Error::from)?;
    // Windows and macOS default to case-folding filesystems: two names differing by case would share one launcher file.
    if folds_case(platform) {
        exposed = fold_case_insensitive(exposed);
    }
    let launchers = !exposed.is_empty() && has_launcher(platform);
    let mut tools = Vec::with_capacity(exposed.len());
    for (name, owner) in &exposed {
        let name = name.as_str();
        refuse_record_name(name)?;
        let owner_digest = owner.tool.digest().to_string();
        let package = packages
            .iter()
            .find(|package| package.digest == owner_digest)
            .ok_or_else(|| VendorError::PathUnrepresentable {
                path: owner_digest.clone(),
            })?;
        let is_entrypoint = closures.surface[owner.walk_index]
            .entrypoints
            .iter()
            .any(|entrypoint| entrypoint.as_str() == name);
        let (kind, target_path) = if is_entrypoint {
            (
                ToolKind::Entrypoint,
                VendorLayout::relative_entrypoint_launcher(key_of(keys, &owner.tool.digest())?, name, windows),
            )
        } else {
            let directory = metadata_of(&roots, &owner.tool)
                .map(|metadata| binary_directory(metadata, windows))
                .unwrap_or_default();
            // The guess is the contract in both modes: a full run refuses it when the content lacks the file.
            (
                ToolKind::Binary,
                format!("{}/{directory}{name}{}", package.content_dir, exe_suffix(windows)),
            )
        };
        let (launcher_path, record_path) = if launchers {
            (
                Some(checked_path(VendorLayout::relative_launcher(name, windows), windows)?),
                Some(checked_path(VendorLayout::relative_record(name), windows)?),
            )
        } else {
            (None, None)
        };
        tools.push(Tool {
            name: name.to_owned(),
            selection: SELECTION_ID.to_owned(),
            kind,
            package: owner_digest,
            environment: SELECTION_ENVIRONMENT.to_owned(),
            target_path: checked_path(target_path, windows)?,
            launcher_path,
            record_path,
        });
    }

    let digests: Vec<&str> = packages.iter().map(|package| package.digest.as_str()).collect();
    let (groups, bindings) = match &request.selection {
        VendorSelection::Project { groups, .. } => (
            groups.clone(),
            closures.roots.iter().filter_map(|root| root.binding.clone()).collect(),
        ),
        VendorSelection::Packages(_) => (Vec::new(), Vec::new()),
    };
    let mut environments = vec![Environment {
        id: SELECTION_ENVIRONMENT.to_owned(),
        scope: EnvironmentScope::Selection {
            selection: SELECTION_ID.to_owned(),
        },
        items: selection_items,
    }];
    environments.extend(entrypoint_environments);

    Ok(VendorManifest {
        schema_version: SCHEMA_VERSION,
        content: Content::Absent,
        platform: platform.to_string(),
        available_platforms: selection.available_platforms.clone(),
        vendor_id: vendor_id(&platform.to_string(), &digests),
        dir_token: DIR_TOKEN.to_owned(),
        packages,
        environments,
        selections: vec![ManifestSelection {
            id: SELECTION_ID.to_owned(),
            groups,
            bindings,
            roots: closures
                .roots
                .iter()
                .map(|root| root.pinned.digest().to_string())
                .collect(),
            environment: SELECTION_ENVIRONMENT.to_owned(),
            launcher_dir: VendorLayout::relative_launcher_dir().to_owned(),
        }],
        tools,
        index_lookups: None,
    })
}

/// A name ending in `.launch` would be some launcher's record file; the same on every target.
fn refuse_record_name(name: &str) -> Result<(), VendorError> {
    if is_record_name(name) {
        return Err(VendorError::PathUnrepresentable {
            path: format!("{name} (a name ending in .{RECORD_EXTENSION} collides with a launcher record)"),
        });
    }
    Ok(())
}

fn exe_suffix(windows: bool) -> &'static str {
    if windows { ".exe" } else { "" }
}

/// `path` when it obeys the manifest's path rules for the target.
fn checked_path(path: String, windows: bool) -> Result<String, VendorError> {
    match VendorLayout::check_relative_path(&path, windows) {
        Ok(()) => Ok(path),
        Err(violation) => Err(VendorError::PathUnrepresentable {
            path: format!("{path} ({violation:?})"),
        }),
    }
}

/// Each root of `closures` as a compose root over its own closure; no package directory is read.
async fn compose_roots_of(
    manager: &PackageManager,
    closures: &ClosureSet,
) -> Result<Vec<Arc<InstallInfo>>, VendorError> {
    let mut composed = Vec::with_capacity(closures.roots.len());
    for root in &closures.roots {
        composed.push(closure_root_of(manager, closures.nodes(root), &root.pinned, &root.requested).await?);
    }
    Ok(composed)
}

async fn closure_root_of(
    manager: &PackageManager,
    nodes: &[ClosureNode],
    pinned: &PinnedPackageRef,
    requested: &PackageRef,
) -> Result<Arc<InstallInfo>, VendorError> {
    // Only named, never read or written: `closure_root` wants a slot, and a vendor composition names no host path.
    // `fs.shims` is the tree an older ocx owns.
    let shim = manager.file_structure().shims.shim_dir(pinned);
    manager
        .closure_root(nodes, shim)
        .await
        .map_err(|kind| failed(requested, kind))
}

/// The compose root of `node` as its own root, for its entry points' private environment.
async fn entrypoint_root(
    manager: &PackageManager,
    roots: &[Arc<InstallInfo>],
    node: &ClosureNode,
    platform: &Platform,
) -> Result<Arc<InstallInfo>, VendorError> {
    let key = node.identifier.strip_advisory();
    if let Some(root) = roots.iter().find(|root| root.identifier().strip_advisory() == key) {
        return Ok(Arc::clone(root));
    }
    // A dependency's closure is relative to itself, so it is read again rather than cut out of its dependent's.
    let requested: PackageRef = node.identifier.clone().into();
    let set = read_closures(
        manager,
        vec![PickedRoot {
            binding: None,
            requested: requested.clone(),
        }],
        platform,
    )
    .await?;
    let root = &set.roots[0];
    closure_root_of(manager, set.nodes(root), &root.pinned, &requested).await
}

/// The metadata of `identifier`: a compose root's own, or one of its closure members'.
fn metadata_of<'a>(
    roots: &'a [Arc<InstallInfo>],
    identifier: &PinnedPackageRef,
) -> Option<&'a ocx_package::metadata::Metadata> {
    let key = identifier.strip_advisory();
    roots.iter().find_map(|root| {
        if root.identifier().strip_advisory() == key {
            return Some(root.metadata());
        }
        root.deferred()?.member(&key).map(|member| member.metadata())
    })
}

/// The directory below `content/` the package's interface `PATH` entry names, `/`-terminated; `bin/` without one.
fn binary_directory(metadata: &ocx_package::metadata::Metadata, windows: bool) -> String {
    metadata
        .env()
        .into_iter()
        .flatten()
        .filter(|var| {
            var.visibility.has_interface()
                && matches!(var.modifier, Modifier::Path(_))
                && if windows {
                    var.key.eq_ignore_ascii_case("PATH")
                } else {
                    var.key == "PATH"
                }
        })
        .filter_map(|var| var.value()?.strip_prefix("${installPath}"))
        .find_map(|rest| match rest {
            "" => Some(String::new()),
            rest => rest
                .strip_prefix('/')
                .map(|rel| format!("{}/", rel.trim_end_matches('/'))),
        })
        .unwrap_or_else(|| "bin/".to_owned())
}

/// A package's own env value never spells the one token the manifest reserves.
fn check_declared_values(roots: &[Arc<InstallInfo>], closures: &ClosureSet) -> Result<(), VendorError> {
    for node in &closures.surface {
        let Some(metadata) = metadata_of(roots, &node.identifier) else {
            continue;
        };
        for var in metadata.env().into_iter().flatten() {
            if var.key.contains(DIR_TOKEN) || var.value().is_some_and(|value| value.contains(DIR_TOKEN)) {
                return Err(VendorError::ValueUnrepresentable {
                    reason: format!("'{}' of {} contains {DIR_TOKEN}", var.key, node.identifier),
                });
            }
        }
    }
    Ok(())
}

/// The absolute paths no value may hold once the token is in place: this home's and the vendor directory's.
///
/// shortcut: the spellings are the home and `DIR` as given and canonicalised, not every 8.3 or `\\?\` form; widen
/// when a Windows run shows one.
fn host_paths(home: &Path, output: &Path) -> Result<Vec<String>, VendorError> {
    let mut paths = vec![home.to_path_buf(), output.to_path_buf()];
    paths.extend(dunce::canonicalize(home).ok());
    let mut spelled = Vec::new();
    for path in paths {
        // A path no UTF-8 string can hold could not be searched for in a value: refuse it, never skip it.
        let text = path.to_str().ok_or_else(|| VendorError::PathUnrepresentable {
            path: path.to_string_lossy().into_owned(),
        })?;
        if !text.is_empty() {
            spelled.push(text.to_owned());
        }
    }
    Ok(spelled)
}

/// The vendored name of every package in `surface`.
///
/// # Errors
///
/// [`VendorError::PathUnrepresentable`] when two packages of the run share a name.
fn package_keys_of(surface: &[ClosureNode]) -> Result<BTreeMap<Digest, PackageKey>, VendorError> {
    let digests: Vec<Digest> = surface.iter().map(|node| node.identifier.digest()).collect();
    VendorLayout::package_keys(&digests).map_err(|clash| VendorError::PathUnrepresentable {
        path: format!(
            "packages {} and {} share one vendored directory name",
            clash.first, clash.second
        ),
    })
}

/// `digest`'s vendored name; every package of the surface has one.
fn key_of<'a>(keys: &'a BTreeMap<Digest, PackageKey>, digest: &Digest) -> Result<&'a PackageKey, VendorError> {
    keys.get(digest).ok_or_else(|| VendorError::PathUnrepresentable {
        path: format!("package {digest} has no vendored directory name"),
    })
}

/// The environment `roots` compose in the vendor lane (no patches, no caller entries), as manifest items.
///
/// # Errors
///
/// [`VendorError::ValueUnrepresentable`] when an item cannot be written relative to `DIR`.
async fn environment_items(
    manager: &PackageManager,
    roots: &[Arc<InstallInfo>],
    self_view: bool,
    platform: &Platform,
    forbidden: &[String],
    keys: &BTreeMap<Digest, PackageKey>,
) -> Result<Vec<EnvironmentItem>, VendorError> {
    let scope = EnvScope::Vendor {
        token_root: PathBuf::from(DIR_TOKEN),
        keys: keys.clone(),
    };
    let entries = manager
        .resolve_env(roots, self_view, scope, platform)
        .await
        .map_err(VendorError::Package)?;
    entries
        .into_iter()
        .map(|entry| {
            let item = EnvironmentItem {
                key: entry.key,
                value: entry.value,
                kind: entry.kind,
                separator: entry.separator,
            };
            check_item(&item, forbidden)?;
            Ok(item)
        })
        .collect()
}

/// On one composed item: the token is the only non-literal text, and no host path survives.
fn check_item(item: &EnvironmentItem, forbidden: &[String]) -> Result<(), VendorError> {
    let unrepresentable = |reason: String| VendorError::ValueUnrepresentable {
        reason: format!("'{}': {reason}", item.key),
    };
    if item.key.is_empty() || item.key.contains(['=', '\0']) {
        return Err(unrepresentable("the key is not a variable name".to_owned()));
    }
    check_value(&item.value, forbidden).map_err(unrepresentable)
}

/// A resolved value holds no NUL and no template, and once the token is gone, no host path; the reason when it does.
fn check_value(value: &str, forbidden: &[String]) -> Result<(), String> {
    if value.contains('\0') {
        return Err("the value holds a NUL".to_owned());
    }
    if value.contains("${") {
        return Err("the value holds an unresolved `${` template".to_owned());
    }
    // The token is what stands for `DIR`; whatever it leaves behind must not name a host path.
    let literal = value.replace(DIR_TOKEN, "");
    if let Some(path) = forbidden.iter().find(|path| holds_path(&literal, path)) {
        return Err(format!("the value holds the host path '{path}'"));
    }
    Ok(())
}

/// Whether `value` holds `path` as a whole path: the match must not run on into another name (`/tmp/o` is not in
/// `/tmp/opt/x`). Separators, list delimiters and the end of the value end a match.
fn holds_path(value: &str, path: &str) -> bool {
    let open_ended = path.ends_with(['/', '\\']);
    value.match_indices(path).any(|(at, _)| {
        open_ended
            || value[at + path.len()..]
                .chars()
                .next()
                .is_none_or(|next| !(next.is_alphanumeric() || matches!(next, '_' | '-' | '.')))
    })
}

/// The packages of the manifest as the report lists them.
fn manifest_packages(selection: &Selection) -> Vec<VendoredPackage> {
    let mut packages: BTreeMap<String, VendoredPackage> = BTreeMap::new();
    for node in &selection.closures.surface {
        packages
            .entry(node.identifier.digest().to_string())
            .or_insert_with(|| VendoredPackage {
                identifier: node.identifier.as_identifier().clone(),
                digest: node.identifier.digest(),
            });
    }
    packages.into_values().collect()
}

/// A fresh staging directory beside `output`, removed on drop unless published.
async fn stage(output: &Path) -> Result<StagedTree, VendorError> {
    let target = output.to_path_buf();
    tokio::task::spawn_blocking(move || StagedTree::create(&target))
        .await
        .map_err(|error| joined(output, error))?
        .map_err(|error| VendorError::Package(crate::Error::from(error)))
}

fn joined(output: &Path, error: tokio::task::JoinError) -> VendorError {
    VendorError::Package(crate::Error::InternalFile(
        output.to_path_buf(),
        std::io::Error::other(error),
    ))
}

/// Writes `manifest.json` last into `staged`, renames it onto `output` and returns the bytes the tree holds.
async fn publish(output: &Path, staged: StagedTree, manifest: &VendorManifest) -> Result<u64, VendorError> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(crate::Error::from)?;
    bytes.push(b'\n');
    let manifest_path = staged.path().join(MANIFEST_FILE);
    tokio::fs::write(&manifest_path, &bytes)
        .await
        .map_err(|error| crate::error::file_error(&manifest_path, error))?;
    // The host umask and clock must not reach the vendored bytes.
    let normalised = manifest_path.clone();
    tokio::task::spawn_blocking(move || normalise_written_file(&normalised, 0o644))
        .await
        .map_err(|error| joined(output, error))?
        .map_err(|error| crate::error::file_error(&error.path, error.cause))?;
    let written_size = tree_size(staged.path()).await?;
    tokio::task::spawn_blocking(move || staged.publish())
        .await
        .map_err(|error| joined(output, error))?
        .map_err(|error| match error.cause.kind() {
            // `DIR` changed between the early check and the rename: the same refusal, not an I/O fault.
            std::io::ErrorKind::DirectoryNotEmpty
            | std::io::ErrorKind::NotADirectory
            | std::io::ErrorKind::AlreadyExists => VendorError::OutputNotEmpty {
                path: output.to_path_buf(),
            },
            _ => VendorError::Package(crate::Error::from(error)),
        })?;
    Ok(written_size)
}

/// The summed size of the regular files under `root`; a link counts for nothing.
async fn tree_size(root: &Path) -> Result<u64, VendorError> {
    let io = |path: &Path, error| VendorError::Package(crate::Error::InternalFile(path.to_path_buf(), error));
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&directory)
            .await
            .map_err(|error| io(&directory, error))?;
        while let Some(entry) = entries.next_entry().await.map_err(|error| io(&directory, error))? {
            let file_type = entry.file_type().await.map_err(|error| io(&entry.path(), error))?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                total += entry.metadata().await.map_err(|error| io(&entry.path(), error))?.len();
            }
        }
    }
    Ok(total)
}

// ── Content half: packages, launchers and sidecars ───────────────────────────

/// Packages pulled at once; a closure is a handful of packages, and each pull bounds its own layers.
const PULL_CONCURRENCY: usize = 8;

/// One package placed in the staging tree.
struct PlacedPackage {
    digest: String,
    tree: Tree,
    /// What the package declared, for the entry-point launchers.
    entrypoints: Option<Entrypoints>,
}

/// Copies every package of `manifest` into `staging`, then writes the launchers and sidecars the manifest names.
///
/// Fills `packages[].tree` and sets `content` to `present`; nothing else of the manifest changes.
async fn write_content(
    manager: &PackageManager,
    request: &VendorRequest,
    selection: &Selection,
    manifest: &mut VendorManifest,
    staging: &Path,
) -> Result<(), VendorError> {
    let platform = &request.platform;
    let windows = is_windows(platform);
    let layout = VendorLayout::new(staging);
    let groups = SetupGroups::new();

    let mut nodes: BTreeMap<String, &ClosureNode> = BTreeMap::new();
    for node in &selection.closures.surface {
        nodes.entry(node.identifier.digest().to_string()).or_insert(node);
    }
    let placed: Vec<PlacedPackage> = futures::stream::iter(nodes)
        .map(|(digest, node)| {
            let key = key_of(&selection.keys, &node.identifier.digest())?;
            Ok::<_, VendorError>(place_package(
                manager,
                platform,
                &layout,
                groups.clone(),
                digest,
                node,
                key,
            ))
        })
        .try_buffer_unordered(PULL_CONCURRENCY)
        .try_collect()
        .await?;

    let mut entrypoints: HashMap<String, Entrypoints> = HashMap::new();
    for package in placed {
        if let Some(listed) = manifest
            .packages
            .iter_mut()
            .find(|listed| listed.digest == package.digest)
        {
            listed.tree = Some(package.tree);
        }
        if let Some(declared) = package.entrypoints {
            entrypoints.insert(package.digest, declared);
        }
    }
    manifest.content = Content::Present;
    mark_declared_binaries_executable(&manifest.tools, &mut manifest.packages);

    ensure_binary_targets_exist(&layout, &manifest.tools, &manifest.packages).await?;

    // No blob for the platform means no launcher and no sidecar; the manifest then lists no `launcher_path`.
    if let Some(blob) = ocx_store::shim::blob_for(platform) {
        let forbidden = host_paths(manager.file_structure().root(), &request.output)?;
        write_entrypoint_launchers(&layout, manifest, &entrypoints, &forbidden, blob, windows).await?;
        write_selection_launchers(&layout, manifest, blob, windows).await?;
    }

    // After the launchers: an entry point's command may resolve through a link.
    if request.omit_links {
        for link in manifest
            .packages
            .iter()
            .filter_map(|package| package.tree.as_ref())
            .flat_map(|tree| &tree.links)
        {
            let path = layout.resolve(&link.path);
            tokio::fs::remove_file(&path)
                .await
                .map_err(|error| crate::error::file_error(&path, error))?;
        }
    }
    Ok(())
}

/// Marks each declared binary's file executable in the listed trees.
///
/// A host without Unix mode bits lists every file as not executable; the declaration is the host-independent
/// fact, so the manifest for one platform reads the same whichever host wrote it.
fn mark_declared_binaries_executable(tools: &[Tool], packages: &mut [Package]) {
    let declared: BTreeSet<&str> = tools
        .iter()
        .filter(|tool| tool.kind == ToolKind::Binary)
        .map(|tool| tool.target_path.as_str())
        .collect();
    for file in packages
        .iter_mut()
        .filter_map(|package| package.tree.as_mut())
        .flat_map(|tree| tree.files.iter_mut())
        .filter(|file| declared.contains(file.path.as_str()))
    {
        file.executable = true;
    }
}

/// Refuses a binary whose manifest `target_path` is not a regular file of the staged tree.
///
/// The path is derived from metadata alone, so `--manifest-only` and a full run emit the same one; this is
/// where a full run proves it. The claim is the publisher's, so the message names the package that made it.
async fn ensure_binary_targets_exist(
    layout: &VendorLayout,
    tools: &[Tool],
    packages: &[Package],
) -> Result<(), VendorError> {
    for tool in tools.iter().filter(|tool| tool.kind == ToolKind::Binary) {
        let is_file = tokio::fs::metadata(layout.resolve(&tool.target_path))
            .await
            .is_ok_and(|metadata| metadata.is_file());
        if !is_file {
            let owner = packages
                .iter()
                .find(|package| package.digest == tool.package)
                .map_or(tool.package.as_str(), |package| package.identifier.as_str());
            return Err(VendorError::PathUnrepresentable {
                path: format!(
                    "{owner}: its `binaries` claim lists `{}`, a program the content does not hold (expected at {}); \
                     the publisher must correct the claim, or expose a batch file or a program outside the first PATH \
                     directory as an entry point",
                    tool.name, tool.target_path
                ),
            });
        }
    }
    Ok(())
}

/// Pulls one package into the staging tree and lists what landed.
async fn place_package(
    manager: &PackageManager,
    platform: &Platform,
    layout: &VendorLayout,
    groups: SetupGroups,
    digest: String,
    node: &ClosureNode,
    key: &PackageKey,
) -> Result<PlacedPackage, VendorError> {
    // `--index DIR` is a read-only input: a vendor run never grows it.
    let manager = &manager.read_only_view();
    let requested: PackageRef = node.identifier.clone().into();
    let pull_failed = |kind| {
        VendorError::Package(crate::Error::InstallFailed(vec![PackageError::new(
            requested.clone(),
            kind,
        )]))
    };
    let resolved = manager
        .resolve(&requested, platform.clone())
        .await
        .map_err(&pull_failed)?;
    let pinned = resolved.pinned.clone();
    let target = VendorTarget::new(layout, key);
    let info = setup_vendored(manager, &pinned, resolved, platform.clone(), groups, &target, None)
        .await
        .map_err(&pull_failed)?;
    let tree = list_tree(layout, &VendorLayout::relative_content_dir(key), is_windows(platform)).await?;
    Ok(PlacedPackage {
        digest,
        tree,
        entrypoints: info.metadata().entrypoints().cloned(),
    })
}

/// Each package's own launchers under `<package_dir>/e/`, started with the package's private environment.
async fn write_entrypoint_launchers(
    layout: &VendorLayout,
    manifest: &VendorManifest,
    declared: &HashMap<String, Entrypoints>,
    forbidden: &[String],
    blob: &[u8],
    windows: bool,
) -> Result<(), VendorError> {
    for package in manifest
        .packages
        .iter()
        .filter(|package| !package.entrypoints.is_empty())
    {
        let Some(entries) = declared.get(&package.digest) else {
            continue;
        };
        // Baked arguments name the content directory; `${deps.*}` is refused by the usage gate, as at launch.
        let install_path = PathBuf::from(format!("{DIR_TOKEN}/{}", package.content_dir));
        let no_dependencies = HashMap::new();
        let resolver = TemplateResolver::new(&install_path, &no_dependencies).usage(Usage::EntryPointArgs);
        // Program resolution is async, so every sidecar is built before the generator asks for one.
        let mut sidecars: HashMap<&str, DirectRecord> = HashMap::new();
        for entry in &package.entrypoints {
            let environment = environment_of(manifest, &entry.environment)?;
            let program = resolve_program(
                layout,
                &environment.items,
                entries.dispatch_command(&entry.name),
                windows,
            )
            .await?;
            let args = entries
                .get(&entry.name)
                .map(|declared| declared.args())
                .unwrap_or_default()
                .iter()
                .map(|arg| {
                    let refused = |reason: String| VendorError::ValueUnrepresentable {
                        reason: format!("entry point '{}' argument '{arg}': {reason}", entry.name),
                    };
                    // The token is the writer's: an author who spells it would be rewritten onto `DIR`.
                    if arg.contains(DIR_TOKEN) {
                        return Err(refused(format!("the argument contains {DIR_TOKEN}")));
                    }
                    let resolved = resolver.resolve(arg).map_err(|error| refused(error.to_string()))?;
                    check_value(&resolved, forbidden).map_err(refused)?;
                    Ok(resolved)
                })
                .collect::<Result<Vec<_>, _>>()?;
            sidecars.insert(
                &entry.name,
                sidecar_of(
                    &manifest.vendor_id,
                    &entry.launcher_path,
                    windows,
                    program,
                    args,
                    &environment.items,
                ),
            );
        }
        // Every launcher of a package sits in one directory, so the first one's parent is it.
        let dir = package
            .entrypoints
            .first()
            .and_then(|entry| entry.launcher_path.rsplit_once('/'))
            .map(|(directory, _)| layout.resolve(directory))
            .ok_or_else(|| VendorError::PathUnrepresentable {
                path: format!("package {} lists no entry-point launcher", package.digest),
            })?;
        crate::launcher::generate_direct(entries, &dir, blob, windows, &|name| {
            sidecars
                .get(name)
                .cloned()
                .ok_or_else(|| VendorError::ValueUnrepresentable {
                    reason: format!("the manifest lists no launcher for entry point '{name}'"),
                })
        })
        .await?;
    }
    Ok(())
}

/// One launcher per exposed name under the selection's launcher directory, started with the selection environment.
async fn write_selection_launchers(
    layout: &VendorLayout,
    manifest: &VendorManifest,
    blob: &[u8],
    windows: bool,
) -> Result<(), VendorError> {
    // `manifest.tools` is the folded `exposed_names` answer; v1 has one selection, so one `l/`.
    let tools: Vec<&Tool> = manifest
        .tools
        .iter()
        .filter(|tool| tool.launcher_path.is_some())
        .collect();
    let mut sidecars: HashMap<&str, DirectRecord> = HashMap::new();
    for tool in &tools {
        let Some(launcher_path) = &tool.launcher_path else {
            continue;
        };
        let environment = environment_of(manifest, &tool.environment)?;
        sidecars.insert(
            &tool.name,
            sidecar_of(
                &manifest.vendor_id,
                launcher_path,
                windows,
                format!("{DIR_TOKEN}/{}", tool.target_path),
                Vec::new(),
                &environment.items,
            ),
        );
    }
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    let dir = layout.resolve(VendorLayout::relative_launcher_dir());
    super::render_toolchain::render_direct_bin(&dir, &names, blob, windows, &|name| {
        sidecars
            .get(name)
            .cloned()
            .ok_or_else(|| VendorError::ValueUnrepresentable {
                reason: format!("the manifest lists no launcher for tool '{name}'"),
            })
    })
    .await
}

fn environment_of<'a>(manifest: &'a VendorManifest, id: &str) -> Result<&'a Environment, VendorError> {
    manifest
        .environments
        .iter()
        .find(|environment| environment.id == id)
        .ok_or_else(|| VendorError::ValueUnrepresentable {
            reason: format!("the manifest has no environment '{id}'"),
        })
}

/// `command` as the first match on the `PATH` entries of `items`, which an entry point's launcher resolves at run time.
///
/// A program must be one fixed path inside `DIR`, so the walk `ocx launcher exec` does against the package's own
/// environment is done here against the staged tree.
async fn resolve_program(
    layout: &VendorLayout,
    items: &[EnvironmentItem],
    command: &str,
    windows: bool,
) -> Result<String, VendorError> {
    let names: Vec<String> = if windows {
        // The target host's PATHEXT is unknown here, so the default list; lowercased because the staged
        // tree is probed on a case-sensitive filesystem where packages ship `tool.exe`.
        ocx_config::env::pathext_extensions(None)
            .into_iter()
            .map(|extension| format!("{command}{}", extension.to_ascii_lowercase()))
            .chain(std::iter::once(command.to_owned()))
            .collect()
    } else {
        vec![command.to_owned()]
    };
    let on_path = |key: &str| {
        if windows {
            key.eq_ignore_ascii_case("PATH")
        } else {
            key == "PATH"
        }
    };
    // Each `path` item moves to the front, so the last one applied is searched first.
    for item in items
        .iter()
        .rev()
        .filter(|item| item.kind == ModifierKind::Path && on_path(&item.key))
    {
        let Some(directory) = item
            .value
            .strip_prefix(DIR_TOKEN)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            continue;
        };
        for name in &names {
            let relative = format!("{directory}/{name}");
            if VendorLayout::check_relative_path(&relative, windows).is_err() {
                continue;
            }
            if tokio::fs::metadata(layout.resolve(&relative))
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return Ok(format!("{DIR_TOKEN}/{relative}"));
            }
        }
    }
    Err(VendorError::PathUnrepresentable {
        path: format!("{command} (found on no PATH entry of the entry point's environment)"),
    })
}

/// The sidecar of the launcher at `launcher_path`; `unset` is the writer's to fill.
fn sidecar_of(
    vendor_id: &str,
    launcher_path: &str,
    windows: bool,
    program: String,
    args: Vec<String>,
    items: &[EnvironmentItem],
) -> DirectRecord {
    let self_path = launcher_path
        .strip_suffix(".exe")
        .filter(|_| windows)
        .unwrap_or(launcher_path);
    DirectRecord {
        kind: LaunchKind::Direct,
        schema_version: SIDECAR_VERSION,
        vendor_id: vendor_id.to_owned(),
        self_path: self_path.to_owned(),
        program,
        args,
        unset: Vec::new(),
        env: items
            .iter()
            .map(|item| EnvRecord {
                key: item.key.clone(),
                value: item.value.clone(),
                kind: match item.kind {
                    ModifierKind::Path => ApplyKind::Path,
                    ModifierKind::Constant => ApplyKind::Constant,
                    ModifierKind::List => ApplyKind::List,
                },
                separator: item.separator.clone(),
            })
            .collect(),
    }
}

/// The files, links and link-free directories under `content_dir`, every path relative to `layout`'s root.
///
/// A link target must be relative and resolve inside `content_dir`; every path must pass the manifest's path
/// rules. `link_free_dirs` are the top-most directories strictly under `content_dir` with no link at or below
/// them; `content_dir` itself is never one, so a package without links lists its child directories.
///
/// # Errors
///
/// [`VendorError::PathUnrepresentable`] for a link that is absolute or escapes, a path that breaks the manifest's
/// rules, or an entry that is neither a file, a directory nor a link; I/O failures as [`VendorError::Package`].
pub(super) async fn list_tree(
    layout: &VendorLayout,
    content_dir: &str,
    platform_is_windows: bool,
) -> Result<Tree, VendorError> {
    let root = layout.resolve(content_dir);
    let unrepresentable = |path: &Path| VendorError::PathUnrepresentable {
        path: path.display().to_string(),
    };

    // Physical, so a link chained through another link cannot climb out; the walk below only reads link targets.
    let link_root = root.clone();
    tokio::task::spawn_blocking(move || ocx_util::fs::path::validate_symlinks_in_dir(&link_root, &link_root))
        .await
        .map_err(|error| VendorError::Package(crate::Error::InternalFile(root.clone(), std::io::Error::other(error))))?
        .map_err(|error| match error {
            ocx_util::archive::Error::SymlinkEscape { link, target } => VendorError::PathUnrepresentable {
                path: format!("{} -> {}", link.display(), target.display()),
            },
            other => VendorError::Package(other.into()),
        })?;

    let directories = DirWalker::new(root.clone(), |dir: &Path, _| {
        WalkDecision::collect_and_descend(vec![dir.to_path_buf()])
    })
    .walk()
    .await
    .map_err(|error| VendorError::Package(error.into()))?;

    let mut files = Vec::new();
    let mut links = Vec::new();
    let mut directory_paths = BTreeSet::new();
    let mut directories_with_links = BTreeSet::new();
    for directory in &directories {
        let relative_directory = relative_path(layout, directory, platform_is_windows)?;
        let mut entries = tokio::fs::read_dir(directory)
            .await
            .map_err(|error| VendorError::Package(crate::Error::InternalFile(directory.clone(), error)))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| VendorError::Package(crate::Error::InternalFile(directory.clone(), error)))?
        {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .await
                .map_err(|error| VendorError::Package(crate::Error::InternalFile(path.clone(), error)))?;
            if file_type.is_dir() {
                continue;
            }
            let relative = relative_path(layout, &path, platform_is_windows)?;
            if file_type.is_symlink() {
                let target = tokio::fs::read_link(&path)
                    .await
                    .map_err(|error| VendorError::Package(crate::Error::InternalFile(path.clone(), error)))?;
                let target = target.to_str().ok_or_else(|| unrepresentable(&target))?.to_owned();
                // `..` and `.` are the escape check's; every other segment must be a legal Windows name,
                // so `..\x`, `C:\x` and device names cannot hide in a single Unix-legal segment.
                if platform_is_windows
                    && target
                        .split('/')
                        .filter(|segment| !matches!(*segment, "" | "." | ".."))
                        .any(|segment| ocx_env_apply::windows_path::check_segment(segment).is_err())
                {
                    return Err(unrepresentable(Path::new(&target)));
                }
                directories_with_links.insert(relative_directory.clone());
                links.push(LinkEntry { path: relative, target });
            } else if file_type.is_file() {
                let metadata = entry
                    .metadata()
                    .await
                    .map_err(|error| VendorError::Package(crate::Error::InternalFile(path.clone(), error)))?;
                files.push(FileEntry {
                    path: relative,
                    executable: ocx_package::bin_scan::unix_is_executable(&metadata),
                });
            } else {
                return Err(unrepresentable(&path));
            }
        }
        directory_paths.insert(relative_directory);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    links.sort_by(|a, b| a.path.cmp(&b.path));

    // A link taints its directory and every ancestor up to `content_dir`.
    let tainted: BTreeSet<&str> = directories_with_links
        .iter()
        .flat_map(|directory| {
            let mut ancestors = vec![directory.as_str()];
            let mut current = directory.as_str();
            while current != content_dir
                && let Some((parent, _)) = current.rsplit_once('/')
            {
                ancestors.push(parent);
                current = parent;
            }
            ancestors
        })
        .collect();
    // The set iterates in byte order, so the list is sorted without a second pass.
    let link_free_dirs = directory_paths
        .iter()
        .filter(|directory| !tainted.contains(directory.as_str()))
        .filter(|directory| directory.as_str() != content_dir)
        .filter(|directory| {
            directory
                .rsplit_once('/')
                .is_some_and(|(parent, _)| parent == content_dir || tainted.contains(parent))
        })
        .cloned()
        .collect();

    Ok(Tree {
        files,
        links,
        link_free_dirs,
    })
}

/// `path` relative to `layout`'s root, `/`-separated and checked against the manifest's path rules.
fn relative_path(layout: &VendorLayout, path: &Path, platform_is_windows: bool) -> Result<String, VendorError> {
    let unrepresentable = |detail: String| VendorError::PathUnrepresentable { path: detail };
    let relative = path
        .strip_prefix(layout.root())
        .map_err(|_| unrepresentable(path.display().to_string()))?;
    let mut segments = Vec::new();
    for component in relative.components() {
        let segment = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| unrepresentable(path.display().to_string()))?;
        segments.push(segment);
    }
    let relative = segments.join("/");
    VendorLayout::check_relative_path(&relative, platform_is_windows)
        .map_err(|violation| unrepresentable(format!("{relative} ({violation:?})")))?;
    Ok(relative)
}

#[cfg(test)]
mod tests {
    use ocx_exit::{ClassifyExitCode, ExitCode};
    use ocx_project::LockedTool;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn host_paths_refuse_a_non_utf8_output_directory_and_home() {
        use std::os::unix::ffi::OsStrExt;
        let bad = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff"));

        for (home, output) in [(Path::new("/home/ocx"), bad), (bad, Path::new("/tmp/out"))] {
            let error = host_paths(home, output).unwrap_err();
            assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
            assert_eq!(error.classify(), Some(ExitCode::DataError));
        }
        assert_eq!(
            host_paths(Path::new("/home/ocx"), Path::new("/tmp/out")).unwrap(),
            ["/home/ocx", "/tmp/out"]
        );
    }

    #[test]
    fn forbidden_paths_match_whole_segments() {
        let forbidden = ["/tmp/o".to_owned()];
        assert!(check_value("/tmp/opt/x", &forbidden).is_ok());
        assert!(check_value("/tmp/o.d", &forbidden).is_ok());
        assert!(check_value("/tmp/o/x", &["/tmp/o/".to_owned()]).is_err());
        for value in [
            "/tmp/o",
            "/tmp/o/x",
            "/bin:/tmp/o:/usr",
            "/tmp/opt/x:/tmp/o/bin",
            r"/tmp/o\x",
        ] {
            assert!(check_value(value, &forbidden).is_err(), "{value}");
        }
    }

    #[test]
    fn output_not_empty_is_a_usage_error_with_its_slug() {
        let error = VendorError::OutputNotEmpty {
            path: PathBuf::from("/out"),
        };
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
        assert_eq!(
            error.to_string(),
            "vendor output '/out' exists and is not an empty directory; name a new or empty directory"
        );
    }

    #[test]
    fn unrepresentable_errors_are_data_errors() {
        let value = VendorError::ValueUnrepresentable { reason: "x".into() };
        let path = VendorError::PathUnrepresentable { path: "/x".into() };
        assert_eq!(value.classify(), Some(ExitCode::DataError));
        assert_eq!(path.classify(), Some(ExitCode::DataError));
    }

    const CONTENT: &str = "packages/r/sha256/ab/cd/content";

    /// A vendor root holding `CONTENT` with the given entries; a name ending in `/` is an empty directory.
    fn tree_root(entries: &[&str]) -> (tempfile::TempDir, VendorLayout) {
        let root = tempfile::tempdir().expect("tempdir");
        let layout = VendorLayout::new(dunce::canonicalize(root.path()).expect("canonical root"));
        for entry in entries {
            let path = layout.resolve(&format!("{CONTENT}/{entry}"));
            if entry.ends_with('/') {
                std::fs::create_dir_all(&path).expect("directory");
            } else {
                std::fs::create_dir_all(path.parent().expect("parent")).expect("parent directory");
                std::fs::write(&path, entry).expect("file");
            }
        }
        (root, layout)
    }

    #[cfg(unix)]
    fn link(layout: &VendorLayout, at: &str, target: &str) {
        std::os::unix::fs::symlink(target, layout.resolve(&format!("{CONTENT}/{at}"))).expect("symlink");
    }

    #[cfg(unix)]
    fn make_executable(layout: &VendorLayout, at: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(
            layout.resolve(&format!("{CONTENT}/{at}")),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_tree_lists_files_links_and_the_top_most_link_free_directories() {
        let (_root, layout) = tree_root(&["bin/tool", "lib/a/x", "lib/l", "share/doc/readme.txt", "share/empty/"]);
        std::fs::remove_file(layout.resolve(&format!("{CONTENT}/lib/l"))).expect("replace the file by a link");
        link(&layout, "lib/l", "../share/doc/readme.txt");
        link(&layout, "bin/tool2", "tool");
        make_executable(&layout, "bin/tool");

        let tree = list_tree(&layout, CONTENT, false).await.expect("the tree lists");

        let paths: Vec<_> = tree
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.executable))
            .collect();
        assert_eq!(
            paths,
            [
                ("packages/r/sha256/ab/cd/content/bin/tool", true),
                ("packages/r/sha256/ab/cd/content/lib/a/x", false),
                ("packages/r/sha256/ab/cd/content/share/doc/readme.txt", false),
            ]
        );
        let links: Vec<_> = tree
            .links
            .iter()
            .map(|l| (l.path.as_str(), l.target.as_str()))
            .collect();
        assert_eq!(
            links,
            [
                ("packages/r/sha256/ab/cd/content/bin/tool2", "tool"),
                ("packages/r/sha256/ab/cd/content/lib/l", "../share/doc/readme.txt"),
            ]
        );
        // `bin` and `lib` hold links; `lib/a` and `share` are the top-most clean directories below them.
        assert_eq!(
            tree.link_free_dirs,
            [
                "packages/r/sha256/ab/cd/content/lib/a",
                "packages/r/sha256/ab/cd/content/share"
            ]
        );
    }

    #[tokio::test]
    async fn a_package_without_links_lists_the_child_directories_of_its_content_directory() {
        let (_root, layout) = tree_root(&["bin/tool", "share/doc/readme.txt"]);

        let tree = list_tree(&layout, CONTENT, false).await.expect("the tree lists");

        assert_eq!(
            tree.link_free_dirs,
            [
                "packages/r/sha256/ab/cd/content/bin",
                "packages/r/sha256/ab/cd/content/share"
            ]
        );
        assert!(tree.links.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_absolute_link_target_is_unrepresentable() {
        let (_root, layout) = tree_root(&["bin/tool"]);
        link(&layout, "bin/passwd", "/etc/passwd");

        let error = list_tree(&layout, CONTENT, false)
            .await
            .expect_err("an absolute target");

        assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_escaping_link_target_is_unrepresentable() {
        let (_root, layout) = tree_root(&["bin/tool"]);
        link(&layout, "bin/up", "../../../../x");

        let error = list_tree(&layout, CONTENT, false)
            .await
            .expect_err("an escaping target");

        assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
    }

    /// A host with no mode bits lists a declared binary as not executable; the manifest says executable anyway.
    #[test]
    fn a_declared_binary_is_executable_whatever_the_host_mode_bits_say() {
        let file = |path: &str| FileEntry {
            path: path.to_owned(),
            executable: false,
        };
        let mut packages = vec![Package {
            identifier: "r/p@sha256:0".to_owned(),
            digest: "sha256:0".to_owned(),
            index_digest: None,
            package_dir: "p/0".to_owned(),
            content_dir: "p/0/c".to_owned(),
            dependencies: Vec::new(),
            names_declared: true,
            entrypoints: Vec::new(),
            tree: Some(Tree {
                files: vec![file("p/0/c/bin/tool.exe"), file("p/0/c/share/readme")],
                links: Vec::new(),
                link_free_dirs: Vec::new(),
            }),
        }];
        let tools = [binary_tool("p/0/c/bin/tool.exe", ToolKind::Binary)];

        mark_declared_binaries_executable(&tools, &mut packages);

        let files = &packages[0].tree.as_ref().expect("tree").files;
        assert_eq!(files.iter().map(|f| f.executable).collect::<Vec<_>>(), [true, false]);
    }

    fn binary_tool(target_path: &str, kind: ToolKind) -> Tool {
        Tool {
            name: "hello".to_owned(),
            selection: SELECTION_ID.to_owned(),
            kind,
            package: "sha256:0".to_owned(),
            environment: SELECTION_ENVIRONMENT.to_owned(),
            target_path: target_path.to_owned(),
            launcher_path: None,
            record_path: None,
        }
    }

    /// A binary absent at its manifest path is refused, a batch file beside the `.exe` guess does not stand in
    /// for it, and an entry point is never checked here.
    #[tokio::test]
    async fn a_binary_target_the_content_lacks_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let layout = VendorLayout::new(dir.path());
        std::fs::create_dir_all(layout.resolve("pkg/content/bin")).expect("bin dir");
        std::fs::write(layout.resolve("pkg/content/bin/hello.cmd"), b"@echo off").expect("batch file");
        std::fs::write(layout.resolve("pkg/content/bin/present"), b"x").expect("file");

        let windows = binary_tool("pkg/content/bin/hello.exe", ToolKind::Binary);
        let error = ensure_binary_targets_exist(&layout, &[windows], &[])
            .await
            .expect_err("only hello.cmd exists");
        assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
        let message = error.to_string();
        assert!(
            message.contains("hello")
                && message.contains("pkg/content/bin/hello.exe")
                && message.contains("entry point"),
            "{message}"
        );

        let unix = binary_tool("pkg/content/bin/hello", ToolKind::Binary);
        assert!(ensure_binary_targets_exist(&layout, &[unix], &[]).await.is_err());

        let mut present = binary_tool("pkg/content/bin/present", ToolKind::Binary);
        present.name = "present".to_owned();
        let entrypoint = binary_tool("pkg/content/bin/hello.exe", ToolKind::Entrypoint);
        ensure_binary_targets_exist(&layout, &[present, entrypoint], &[])
            .await
            .expect("control: a present file passes and an entry point is skipped");
    }

    /// The manifest-only path for a binary is the path a full run checks, so both modes agree.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_binary_target_is_the_same_path_the_full_run_accepts() {
        let (_dir, _, manifest) = staged_tool(&[]).await;
        let binary = manifest
            .tools
            .iter()
            .find(|tool| tool.kind == ToolKind::Binary)
            .expect("the cmake binary");
        let (_dir, layout, _) = staged_tool(std::slice::from_ref(&binary.target_path)).await;

        ensure_binary_targets_exist(&layout, &manifest.tools, &manifest.packages)
            .await
            .expect("a file at the manifest-only path passes");
        let (_dir, layout, _) = staged_tool(&[]).await;
        let error = ensure_binary_targets_exist(&layout, &manifest.tools, &manifest.packages)
            .await
            .expect_err("no file at that path");
        assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
    }

    /// `aux.h` is an ordinary file name everywhere but a Windows target.
    #[tokio::test]
    async fn a_windows_device_name_is_refused_only_for_a_windows_target() {
        let (_root, layout) = tree_root(&["include/aux.h"]);

        list_tree(&layout, CONTENT, false)
            .await
            .expect("a Unix target lists the name");
        let error = list_tree(&layout, CONTENT, true).await.expect_err("a Windows target");

        assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
    }

    /// Link targets a Unix host accepts as one plain segment are refused for a Windows target.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_target_breaking_windows_rules_is_refused_only_for_a_windows_target() {
        for target in ["..\\x", "C:\\x", "sub/CON", "a:b"] {
            let (_root, layout) = tree_root(&["bin/tool", "bin/sub/x"]);
            link(&layout, "bin/l", target);

            list_tree(&layout, CONTENT, false)
                .await
                .unwrap_or_else(|error| panic!("a Linux target keeps {target:?}: {error:?}"));
            let error = list_tree(&layout, CONTENT, true)
                .await
                .expect_err("a Windows target refuses it");
            assert!(
                matches!(error, VendorError::PathUnrepresentable { .. }),
                "{target:?}: {error:?}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_plain_relative_link_target_is_accepted_on_both_platforms() {
        for windows in [false, true] {
            let (_root, layout) = tree_root(&["bin/tool", "share/doc/readme.txt"]);
            link(&layout, "bin/up", "../share/doc/readme.txt");
            link(&layout, "bin/same", "tool");

            let tree = list_tree(&layout, CONTENT, windows).await.expect("plain targets list");

            assert_eq!(tree.links.len(), 2, "windows={windows}");
        }
    }

    /// A Windows entry point resolves in the default PATHEXT order (`.bat` before `.cmd`), the bare name last.
    #[tokio::test]
    async fn a_windows_program_resolves_in_the_default_pathext_order() {
        let (_root, layout) = tree_root(&["bin/tool.cmd", "bin/tool.bat", "bin/other"]);
        let items = [EnvironmentItem {
            key: "PATH".to_owned(),
            kind: ModifierKind::Path,
            value: format!("{DIR_TOKEN}/{CONTENT}/bin"),
            separator: None,
        }];

        let both = resolve_program(&layout, &items, "tool", true).await.expect("resolves");
        let bare = resolve_program(&layout, &items, "other", true)
            .await
            .expect("bare name");

        assert_eq!(both, format!("{DIR_TOKEN}/{CONTENT}/bin/tool.bat"));
        assert_eq!(bare, format!("{DIR_TOKEN}/{CONTENT}/bin/other"));
    }

    // ── Selection half: closure, shared platforms, trust ─────────────────────

    use ocx_index::{ChainMode, Index, LocalConfig, LocalIndex};
    use ocx_oci::Algorithm;
    use ocx_oci::media_type::MEDIA_TYPE_PACKAGE_METADATA_V1;
    use ocx_store::file_structure::FileStructure;

    use crate::test_support::manifest_source::FakeManifestSource;

    const REGISTRY: &str = "example.com";
    const ABSENT_CHILD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn linux_amd64() -> Platform {
        "linux/amd64".parse().expect("platform")
    }

    fn manager_over(root: &Path, source: impl ocx_index::IndexImpl + 'static) -> PackageManager {
        let fs = FileStructure::with_root(root.to_path_buf());
        // As the CLI wires it: a fetched blob is kept in the home, which the closure read depends on.
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

    /// Registers a flat package manifest and its config blob; returns the manifest digest.
    fn register_leaf(source: FakeManifestSource, dependencies: serde_json::Value) -> (FakeManifestSource, Digest) {
        register_package(
            source,
            serde_json::json!({
                "type": "bundle", "version": 1, "dependencies": dependencies, "entrypoints": {},
                "env": [{"key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public"}],
            }),
        )
    }

    /// Registers a flat package manifest over the metadata `config`; returns the manifest digest.
    fn register_package(source: FakeManifestSource, config: serde_json::Value) -> (FakeManifestSource, Digest) {
        register_package_typed(source, config, "")
    }

    /// [`register_package`] with `artifact_type` spelled in the manifest, so a pull of it succeeds.
    fn register_package_typed(
        source: FakeManifestSource,
        config: serde_json::Value,
        artifact_type: &str,
    ) -> (FakeManifestSource, Digest) {
        let config = config.to_string();
        let config_digest = Algorithm::Sha256.hash(config.as_bytes());
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",{artifact_type}"config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
            size = config.len(),
            artifact_type = if artifact_type.is_empty() {
                String::new()
            } else {
                format!(r#""artifactType":"{artifact_type}","#)
            },
        );
        let digest = Algorithm::Sha256.hash(manifest.as_bytes());
        let source = source
            .with(&digest.to_string(), manifest.as_bytes())
            .with_blob(&config_digest.to_string(), config.as_bytes());
        (source, digest)
    }

    /// The image index document offering `platforms`; the first platform's leaf is real.
    fn index_document(leaf: &Digest, platforms: &[(&str, &str)]) -> String {
        let entries: Vec<String> = platforms
            .iter()
            .enumerate()
            .map(|(position, (os, arch))| {
                let digest = if position == 0 { leaf.to_string() } else { format!("sha256:{ABSENT_CHILD}") };
                format!(
                    r#"{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{digest}","size":1,"platform":{{"os":"{os}","architecture":"{arch}"}}}}"#
                )
            })
            .collect();
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{}]}}"#,
            entries.join(",")
        )
    }

    /// Registers the tag `tag` as an image index offering `platforms`; the first platform's leaf is real.
    fn register_index(
        source: FakeManifestSource,
        tag: &str,
        leaf: &Digest,
        platforms: &[(&str, &str)],
    ) -> FakeManifestSource {
        source.with(tag, index_document(leaf, platforms).as_bytes())
    }

    fn packages_request(packages: Vec<PackageRef>, platform: Platform) -> VendorRequest {
        VendorRequest {
            output: PathBuf::from("/out"),
            platform,
            selection: VendorSelection::Packages(packages),
            manifest_only: true,
            omit_links: false,
        }
    }

    fn package(repository: &str, tag: &str) -> PackageRef {
        PackageRef::new_registry(repository, REGISTRY).clone_with_tag(tag)
    }

    /// A selected package brings the dependency its edge admits, root last.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_selection_brings_its_dependency_closure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, dependency) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let edge = serde_json::json!([{
            "identifier": format!("{REGISTRY}/dep@{dependency}"), "visibility": "public", "name": "dep",
        }]);
        let (source, root) = register_leaf(source, edge);
        let manager = manager_over(dir.path(), register_index(source, "1.0", &root, &[("linux", "amd64")]));

        let selection = select(&manager, &packages_request(vec![package("tool", "1.0")], linux_amd64()))
            .await
            .expect("the selection reads");

        let selected = &selection.closures.roots[0];
        assert_eq!(
            selected.binding, None,
            "a package named on the command line has no binding"
        );
        assert_eq!(selected.pinned.digest(), root, "the leaf the index resolved to");
        let nodes = selection.closures.nodes(selected);
        let digests: Vec<_> = nodes.iter().map(|node| node.identifier.digest()).collect();
        assert_eq!(digests, [dependency, root], "the dependency, then the root");
        assert!(nodes[1].is_root);
        assert!(
            super::super::inspect::admitted_on_surface(&nodes[0], false),
            "the public edge is admitted by the rule `ocx env` applies"
        );
    }

    /// The platforms every root offers; one root lacking a platform leaves it out.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_available_platforms_leave_out_a_platform_one_root_lacks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, wide) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let (source, narrow) = register_leaf(
            source,
            serde_json::json!([{"identifier": format!("{REGISTRY}/wide@{wide}"), "visibility": "sealed", "name": "wide"}]),
        );
        let source = register_index(source, "wide", &wide, &[("linux", "amd64"), ("darwin", "arm64")]);
        let source = register_index(source, "narrow", &narrow, &[("linux", "amd64")]);
        let manager = manager_over(dir.path(), source);
        let packages = vec![package("wide", "wide"), package("narrow", "narrow")];

        let selection = select(&manager, &packages_request(packages, linux_amd64()))
            .await
            .expect("the selection reads");

        assert_eq!(selection.available_platforms, ["linux/amd64"]);
        // Control: the wide root alone offers both.
        let alone = select(
            &manager,
            &packages_request(vec![package("wide", "wide")], linux_amd64()),
        )
        .await
        .expect("the selection reads");
        assert_eq!(alone.available_platforms, ["darwin/arm64", "linux/amd64"]);
    }

    /// Design § 5.2 is silent on the package tier: a package with no leaf for `-p` keeps the resolution error (79).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_package_without_a_leaf_for_the_platform_keeps_the_resolution_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, leaf) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let manager = manager_over(dir.path(), register_index(source, "1.0", &leaf, &[("linux", "amd64")]));
        let windows: Platform = "windows/amd64".parse().expect("platform");

        let error = select(&manager, &packages_request(vec![package("tool", "1.0")], windows))
            .await
            .expect_err("no windows leaf");
        assert_eq!(error.classify(), Some(ExitCode::NotFound), "{error:?}");

        // Control: the platform the index offers resolves.
        select(&manager, &packages_request(vec![package("tool", "1.0")], linux_amd64()))
            .await
            .expect("linux leaf");
    }

    /// A lock mixing `linux/amd64` and `linux/amd64+libc.glibc` leaves offers the glibc spelling, not the bare one.
    #[test]
    fn locked_platforms_follow_the_host_leaf_match_not_the_literal_keys() {
        let tool = |keys: &[&str]| LockedTool {
            declared: None,
            name: "tool".to_owned(),
            group: "default".to_owned(),
            repository: ocx_oci::Repository::new(REGISTRY, "tool"),
            platforms: keys
                .iter()
                .map(|key| ((*key).to_owned(), Digest::Sha256("a".repeat(64))))
                .collect(),
        };
        let plain = tool(&["linux/amd64", "darwin/arm64"]);
        let glibc = tool(&["linux/amd64+libc.glibc", "darwin/arm64"]);
        // `plain` serves a glibc request, `glibc` does not serve a bare one: only the glibc spelling is common.
        assert_eq!(
            locked_platforms([&plain, &glibc]),
            ["darwin/arm64", "linux/amd64+libc.glibc"]
        );
        assert_eq!(locked_platforms([&plain]), ["darwin/arm64", "linux/amd64"]);
        assert_eq!(
            locked_platforms([&tool(&["any"]), &plain]),
            ["darwin/arm64", "linux/amd64"]
        );
        assert_eq!(locked_platforms([]), ["any"]);
        assert!(locked_platforms([&tool(&["linux/amd64"]), &tool(&["windows/amd64"])]).is_empty());
    }

    #[test]
    fn shared_platforms_intersect_and_an_any_offer_narrows_nothing() {
        let set = |keys: &[&str]| keys.iter().map(|key| (*key).to_owned()).collect::<BTreeSet<_>>();
        assert_eq!(
            shared_platforms([set(&["darwin/arm64", "linux/amd64"]), set(&["linux/amd64"])]),
            ["linux/amd64"]
        );
        assert_eq!(
            shared_platforms([set(&["any"]), set(&["linux/amd64", "windows/amd64"])]),
            ["linux/amd64", "windows/amd64"]
        );
        assert_eq!(shared_platforms([set(&["any"])]), ["any"]);
        assert!(shared_platforms([set(&["linux/amd64"]), set(&["windows/amd64"])]).is_empty());
    }

    /// A bare platform manifest identifier offers `any`, so `-p P` is accepted with `available_platforms = ["any"]`.
    #[test]
    fn a_flat_manifest_offers_any_and_an_index_offers_its_candidates() {
        let digest = Digest::Sha256("0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a".to_owned());
        let pinned = closure_node(&digest).identifier;
        let metadata = serde_json::from_str::<ocx_package::metadata::Metadata>(r#"{"type":"bundle","version":1}"#)
            .expect("bundle metadata");
        let flat = InspectResult::Manifest {
            pinned: pinned.clone(),
            metadata: ocx_package::metadata::ValidMetadata::try_from(metadata).expect("valid metadata"),
            layers: Vec::new(),
            closure: None,
        };
        assert_eq!(offered_platforms(flat), BTreeSet::from(["any".to_owned()]));
        let index = InspectResult::Candidates {
            pinned,
            candidates: Vec::new(),
        };
        assert!(offered_platforms(index).is_empty());
    }

    /// What `ocx exec` selects for one default-group tool: `select_tool_set` over a config and the lock that binds it.
    fn project_selection(name: &str, leaves: &[(&str, &Digest)]) -> VendorSelection {
        let declared = PackageRef::new_registry(name, REGISTRY).clone_with_tag("1.0");
        let config =
            ocx_project::ProjectConfig::from_parts(BTreeMap::from([(name.to_owned(), declared)]), BTreeMap::new());
        let lock = ocx_project::ProjectLock {
            metadata: ocx_project::LockMetadata {
                lock_version: ocx_project::LockVersion::V3,
                declaration_hash_version: 1,
                declaration_hash: config.declaration_hash_cached().to_owned(),
                generated_by: "ocx test".into(),
                generated_at: "2026-04-24T00:00:00Z".into(),
            },
            tools: vec![LockedTool {
                declared: None,
                name: name.to_owned(),
                group: "default".to_owned(),
                repository: ocx_oci::Repository::new(REGISTRY, name),
                platforms: leaves
                    .iter()
                    .map(|(platform, digest)| ((*platform).to_owned(), (*digest).clone()))
                    .collect(),
            }],
        };
        let groups = vec!["default".to_owned()];
        let selected = ocx_project::select_tool_set(&config, Some(&lock), &groups, &[]).expect("the lock binds");
        VendorSelection::Project { selected, groups }
    }

    fn project_request(selection: VendorSelection, platform: Platform) -> VendorRequest {
        VendorRequest {
            selection,
            ..packages_request(Vec::new(), platform)
        }
    }

    /// A platform outside the lock's keys is refused with the project tier's own `no_host_leaf` (78), before the network.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_platform_outside_the_lock_is_refused_with_no_host_leaf() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, leaf) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let manager = manager_over(dir.path(), source);
        let windows: Platform = "windows/amd64".parse().expect("platform");
        let selection = project_selection("cmake", &[("linux/amd64", &leaf)]);

        let error = select(&manager, &project_request(selection.clone(), windows))
            .await
            .expect_err("no windows leaf");
        assert_eq!(error.classify(), Some(ExitCode::ConfigError), "{error:?}");

        // Control: the platform the lock holds resolves, under the declaration's tag, as a bound tool.
        let selected = select(&manager, &project_request(selection, linux_amd64()))
            .await
            .expect("linux leaf");
        let root = &selected.closures.roots[0];
        assert_eq!(root.binding.as_deref(), Some("cmake"));
        assert_eq!(root.requested.tag(), Some("1.0"));
        assert_eq!(root.requested.digest(), Some(leaf));
    }

    /// A lock holding only `windows/amd64` selects for it on any host; the lock's keys are the available set.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_foreign_only_lock_selects_for_its_platform() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, leaf) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let manager = manager_over(dir.path(), source);
        let windows: Platform = "windows/amd64".parse().expect("platform");
        let selection = project_selection("cmake", &[("windows/amd64", &leaf)]);

        let selected = select(&manager, &project_request(selection, windows))
            .await
            .expect("the foreign leaf selects");

        assert_eq!(selected.available_platforms, ["windows/amd64"]);
    }

    /// The manifest-only path runs the auto-verify hook; `required` with no publisher refuses it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_manifest_only_path_runs_auto_verify() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, leaf) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let source = register_index(source, "1.0", &leaf, &[("linux", "amd64")]);
        let manager = manager_over(dir.path(), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());

        select(&manager, &request)
            .await
            .expect("control: no verify hook, no refusal");

        let config: ocx_config::Config =
            toml::from_str(&format!("[registries.\"{REGISTRY}\"]\nsignatures = \"required\"\n")).expect("config");
        let verifying = manager
            .clone()
            .with_auto_verify(Some(crate::AutoVerify::new(crate::AutoVerifyInput {
                operator_policies: Vec::new(),
                project_policies: Vec::new(),
                config,
                registry_client: ocx_oci::Client::with_transport(Box::new(
                    ocx_oci::client::test_transport::StubTransport::new(
                        ocx_oci::client::test_transport::StubTransportData::new(),
                    ),
                )),
                rekor_url: "http://127.0.0.1:3000".parse().expect("url"),
                offline: true,
                state: ocx_store::file_structure::StateStore::new(dir.path().join("state")),
                trusted_root_env: None,
                sigstore_trust: None,
                home_trusted_root: None,
                user_opted_out: false,
            })));
        let error = select(&verifying, &request)
            .await
            .expect_err("`required` with no publisher refuses");

        // The refusal the install path raises, carried whole: the CLI's chain walk classifies it 81.
        let VendorError::Package(crate::Error::ResolveFailed(failures)) = &error else {
            panic!("expected a resolve failure, got {error:?}");
        };
        assert!(
            matches!(
                failures[0].kind,
                PackageErrorKind::Internal(crate::Error::Rounds(
                    crate::tasks::signer_rounds::RoundsError::PublishersRequired { .. }
                ))
            ),
            "{error:?}"
        );
    }

    /// A source that declares publishers for the repository `dep` only, as a fetched index root does.
    #[derive(Clone)]
    struct DeclaringSource(FakeManifestSource);

    #[async_trait::async_trait]
    impl ocx_index::IndexImpl for DeclaringSource {
        async fn list_repositories(&self, registry: &str) -> ocx_index::error::Result<Vec<String>> {
            self.0.list_repositories(registry).await
        }
        async fn list_tags(&self, identifier: &PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            self.0.list_tags(identifier).await
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            op: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<(Digest, ocx_oci::Manifest)>> {
            self.0.fetch_manifest(identifier, op).await
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &PackageRef,
            op: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<Digest>> {
            self.0.fetch_manifest_digest(identifier, op).await
        }
        async fn fetch_blob(&self, blob_ref: &PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            self.0.fetch_blob(blob_ref).await
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &PackageRef,
        ) -> ocx_index::error::Result<Option<(Vec<u8>, Digest, ocx_oci::Manifest)>> {
            self.0.fetch_manifest_raw_bytes(identifier).await
        }
        async fn declared_publishers(
            &self,
            identifier: &PackageRef,
        ) -> ocx_index::error::Result<ocx_index::DeclaredPublishers> {
            if identifier.repository() != "dep" {
                return Ok(ocx_index::DeclaredPublishers::Undeclared);
            }
            Ok(ocx_index::DeclaredPublishers::Declared(serde_json::json!([{
                "kind": "keyless", "identity": "publisher@example.com",
                "oidc_issuer": "https://issuer.example.com",
            }])))
        }
        fn serves_registry(&self, registry: &str) -> bool {
            registry == REGISTRY
        }
        fn source_kind(&self) -> ocx_index::SourceKind {
            ocx_index::SourceKind::Published
        }
        fn box_clone(&self) -> Box<dyn ocx_index::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A package reached both as a pinned dependency and as a tagged root still runs the publisher check its tag
    /// owes, whichever the closure walk meets first.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tagged_root_is_verified_though_it_is_also_a_pinned_dependency() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, dependency) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let edge = serde_json::json!([{
            "identifier": format!("{REGISTRY}/dep@{dependency}"), "visibility": "public", "name": "dep",
        }]);
        let (source, tool) = register_leaf(source, edge);
        let source = register_index(source, "1.0", &tool, &[("linux", "amd64")]);
        let source = DeclaringSource(register_index(source, "dep-1.0", &dependency, &[("linux", "amd64")]));
        let manager = manager_over(dir.path(), source);
        let config: ocx_config::Config = toml::from_str("").expect("config");
        let verifying = manager
            .clone()
            .with_auto_verify(Some(crate::AutoVerify::new(crate::AutoVerifyInput {
                operator_policies: Vec::new(),
                project_policies: Vec::new(),
                config,
                registry_client: ocx_oci::Client::with_transport(Box::new(
                    ocx_oci::client::test_transport::StubTransport::new(
                        ocx_oci::client::test_transport::StubTransportData::new(),
                    ),
                )),
                rekor_url: "http://127.0.0.1:3000".parse().expect("url"),
                offline: true,
                state: ocx_store::file_structure::StateStore::new(dir.path().join("state")),
                trusted_root_env: None,
                sigstore_trust: None,
                home_trusted_root: None,
                user_opted_out: false,
            })));
        let tagged_dep = || package("dep", "dep-1.0");

        select(
            &verifying,
            &packages_request(vec![package("tool", "1.0")], linux_amd64()),
        )
        .await
        .expect("control: a pinned dependency declares nothing, so the dependency alone passes");
        select(&verifying, &packages_request(vec![tagged_dep()], linux_amd64()))
            .await
            .expect_err("control: the tagged request adopts the declared publishers and finds no signature");

        for (order, roots) in [
            ("dependency first", vec![package("tool", "1.0"), tagged_dep()]),
            ("tagged root first", vec![tagged_dep(), package("tool", "1.0")]),
        ] {
            select(&verifying, &packages_request(roots, linux_amd64()))
                .await
                .expect_err(&format!("{order}: the tagged root's publisher check must run"));
        }
    }

    /// A vendor run into an absent directory writes its manifest.
    #[tokio::test]
    async fn vendor_manifest_only_writes_manifest_json() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fs = ocx_store::file_structure::FileStructure::with_root(tmp.path().join("home"));
        let index = ocx_index::Index::from_chained(
            ocx_index::LocalIndex::new(ocx_index::LocalConfig {
                index_store: ocx_index::IndexStore::new(tmp.path().join("home/index")),
            }),
            vec![],
            ocx_index::ChainMode::Offline,
        );
        let manager = PackageManager::new(fs, index, None, "localhost:5000");
        let output = tmp.path().join("out");
        let outcome = manager
            .vendor(VendorRequest {
                output: output.clone(),
                platform: Platform::any(),
                selection: VendorSelection::Packages(Vec::new()),
                manifest_only: true,
                omit_links: false,
            })
            .await
            .expect("vendor writes a manifest");
        assert!(outcome.manifest_path.is_file());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let metadata = std::fs::metadata(&outcome.manifest_path).expect("metadata");
            assert_eq!(metadata.permissions().mode() & 0o777, 0o644, "manifest mode");
            let epoch = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(315_532_800);
            assert_eq!(metadata.modified().expect("mtime"), epoch, "manifest mtime");
        }
        assert_eq!(outcome.content, VendorContent::Absent);
    }

    // ── Document half: the manifest from the selection alone ─────────────────

    /// A dependency with a `PATH` entry, and a root declaring a binary, an entry point and a constant, both public.
    fn registered_tool() -> (FakeManifestSource, Digest, Digest) {
        registered_tool_on(("linux", "amd64"))
    }

    /// [`registered_tool`] with the index offering `platform`.
    fn registered_tool_on(platform: (&str, &str)) -> (FakeManifestSource, Digest, Digest) {
        let (source, dependency) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let (source, root) = register_package(
            source,
            serde_json::json!({
                "type": "bundle", "version": 1, "binaries": ["cmake"], "entrypoints": {"hello": {}},
                "dependencies": [{"identifier": format!("{REGISTRY}/dep@{dependency}"), "visibility": "public", "name": "dep"}],
                "env": [
                    {"key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public"},
                    {"key": "CMAKE_ROOT", "type": "constant", "value": "${installPath}/share/cmake", "visibility": "public"},
                ],
            }),
        );
        (register_index(source, "1.0", &root, &[platform]), dependency, root)
    }

    async fn manifest_of(manager: &PackageManager, request: &VendorRequest) -> (Selection, VendorManifest) {
        let selection = select(manager, request).await.expect("the selection reads");
        let manifest = build_manifest(manager, request, &selection, &|_| true)
            .await
            .expect("the manifest builds");
        (selection, manifest)
    }

    fn closure_node(digest: &Digest) -> ClosureNode {
        ClosureNode {
            identifier: PinnedPackageRef::try_from(
                PackageRef::new_registry(REGISTRY, "tool").clone_with_digest(digest.clone()),
            )
            .expect("pinned identifier"),
            config_digest: digest.clone(),
            effective_visibility: None,
            binaries: None,
            entrypoints: Vec::new(),
            env: Vec::new(),
            integrations: Vec::new(),
            dependencies: Vec::new(),
            is_root: true,
        }
    }

    /// Two digests sharing their first 16 hex characters are refused as `vendor_path_unrepresentable`, naming both.
    #[test]
    fn two_packages_sharing_a_directory_name_are_unrepresentable_naming_both_digests() {
        let first = Digest::Sha256(format!("{}{}", "ab".repeat(8), "0".repeat(48)));
        let second = Digest::Sha256(format!("{}{}", "ab".repeat(8), "1".repeat(48)));

        let error = package_keys_of(&[closure_node(&first), closure_node(&second)]).expect_err("a shared name");

        let VendorError::PathUnrepresentable { path } = &error else {
            panic!("{error:?}");
        };
        assert!(
            path.contains(&first.to_string()) && path.contains(&second.to_string()),
            "{path}"
        );
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    fn package_key(digest: &Digest) -> PackageKey {
        VendorLayout::package_keys([digest]).expect("one key")[digest].clone()
    }

    fn items(manifest: &VendorManifest, id: &str) -> Vec<(String, String)> {
        let environment = manifest.environments.iter().find(|e| e.id == id).expect("environment");
        environment
            .items
            .iter()
            .map(|item| (item.key.clone(), item.value.clone()))
            .collect()
    }

    /// The selection's environment is the composer's for the same roots, the install root swapped for the token.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_selection_environment_is_the_composers_with_the_install_root_swapped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, dependency, root) = registered_tool();
        let manager = manager_over(dir.path(), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let (selection, manifest) = manifest_of(&manager, &request).await;

        // Control: the digest lane over the same roots, minus the deferred root's shim slot, which is a host path.
        let roots = compose_roots_of(&manager, &selection.closures).await.expect("roots");
        let shim = manager
            .file_structure()
            .shims
            .shim_dir(&selection.closures.roots[0].pinned);
        let generation = ocx_store::launcher_generation::LauncherGeneration::host().expect("host launcher blob");
        // Each store directory the digest lane names becomes the vendored one `VendorLayout` names.
        let swaps: Vec<(String, String)> = selection
            .closures
            .surface
            .iter()
            .flat_map(|node| {
                let store = manager.file_structure().packages.path(&node.identifier);
                let key = &selection.keys[&node.identifier.digest()];
                [
                    (store.join("content"), VendorLayout::relative_content_dir(key)),
                    (
                        store.join("launchers").join(generation.as_str()),
                        VendorLayout::relative_entrypoint_dir(key),
                    ),
                ]
            })
            .map(|(store, vendored)| (store.to_string_lossy().into_owned(), format!("{DIR_TOKEN}/{vendored}")))
            .collect();
        let expected: Vec<(String, String)> = manager
            .resolve_env(&roots, false, EnvScope::package_tier(), &linux_amd64())
            .await
            .expect("digest lane")
            .into_iter()
            .filter(|entry| entry.value != shim.bin().to_string_lossy())
            .map(|entry| {
                let value = swaps
                    .iter()
                    .fold(entry.value, |value, (store, vendored)| value.replace(store, vendored));
                // The digest lane spells host paths (`\` on Windows); the manifest stores `/` on every host.
                (entry.key, value.replace('\\', "/"))
            })
            .collect();

        let actual = items(&manifest, "selection:default");
        assert_eq!(actual, expected);
        let package_dir = |digest: &Digest| VendorLayout::relative_content_dir(&package_key(digest));
        assert_eq!(
            actual,
            [
                (
                    "PATH".to_owned(),
                    format!("{DIR_TOKEN}/{}/bin", package_dir(&dependency))
                ),
                ("PATH".to_owned(), format!("{DIR_TOKEN}/{}/bin", package_dir(&root))),
                (
                    "CMAKE_ROOT".to_owned(),
                    format!("{DIR_TOKEN}/{}/share/cmake", package_dir(&root))
                ),
                (
                    "PATH".to_owned(),
                    format!(
                        "{DIR_TOKEN}/{}",
                        VendorLayout::relative_entrypoint_dir(&package_key(&root))
                    )
                ),
            ],
            "the dependency's PATH first, then the root's own entries, its entry points last"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_manifest_names_packages_entrypoints_and_tools() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, dependency, root) = registered_tool();
        let manager = manager_over(dir.path(), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let (_, manifest) = manifest_of(&manager, &request).await;

        assert_eq!(manifest.content, Content::Absent);
        assert_eq!(manifest.platform, "linux/amd64");
        assert_eq!(manifest.dir_token, DIR_TOKEN);
        let (dependency, root) = (dependency.to_string(), root.to_string());
        let mut digests = [dependency.clone(), root.clone()];
        digests.sort();
        assert_eq!(
            manifest.packages.iter().map(|p| p.digest.as_str()).collect::<Vec<_>>(),
            digests,
            "every package of the closure once, sorted by digest"
        );
        let tool = manifest
            .packages
            .iter()
            .find(|p| p.digest == root)
            .expect("root package");
        assert_eq!(tool.dependencies, std::slice::from_ref(&dependency));
        assert!(tool.names_declared);
        assert!(tool.tree.is_none(), "a manifest-only run lists no tree");
        let dep = manifest
            .packages
            .iter()
            .find(|p| p.digest == dependency)
            .expect("dependency");
        assert!(
            !dep.names_declared,
            "the dependency claims neither binaries nor entry points"
        );
        assert_eq!(tool.entrypoints.len(), 1);
        assert_eq!(tool.entrypoints[0].name, "hello");
        let key = package_key(&Digest::try_from(root.as_str()).expect("digest"));
        assert_eq!(tool.package_dir, VendorLayout::relative_package_dir(&key));
        assert_eq!(tool.content_dir, VendorLayout::relative_content_dir(&key));
        assert_eq!(
            tool.entrypoints[0].launcher_path,
            VendorLayout::relative_entrypoint_launcher(&key, "hello", false)
        );
        assert_eq!(
            tool.entrypoints[0].launcher_path,
            format!("{}/e/hello", tool.package_dir)
        );
        assert_eq!(tool.entrypoints[0].environment, format!("entrypoint:{root}:hello"));

        // The entry point's private environment is the package's own surface: its dependency's `PATH`, then its own.
        let private = items(&manifest, &format!("entrypoint:{root}:hello"));
        assert_eq!(private.len(), 3, "{private:?}");
        assert_eq!(
            manifest.environments[0].id, "selection:default",
            "the selection's environment is first"
        );

        let selection = &manifest.selections[0];
        assert_eq!(
            (selection.id.as_str(), selection.roots.as_slice()),
            ("default", [root.clone()].as_slice())
        );
        assert_eq!(selection.launcher_dir, "l");
        assert!(selection.groups.is_empty() && selection.bindings.is_empty());

        let names: Vec<_> = manifest
            .tools
            .iter()
            .map(|t| (t.name.as_str(), t.kind.clone()))
            .collect();
        assert_eq!(names, [("cmake", ToolKind::Binary), ("hello", ToolKind::Entrypoint)]);
        let cmake = &manifest.tools[0];
        assert_eq!(cmake.package, root);
        assert_eq!(cmake.target_path, format!("{}/bin/cmake", tool.content_dir));
        assert_eq!(cmake.launcher_path.as_deref(), Some("l/cmake"));
        assert_eq!(manifest.tools[1].target_path, tool.entrypoints[0].launcher_path);

        assert_eq!(
            manifest.vendor_id,
            vendor_id("linux/amd64", &[dependency.as_str(), root.as_str()])
        );
        assert_eq!(manifest.available_platforms, ["linux/amd64"]);
    }

    /// A manifest lists the record beside every launcher, and only the root an index selected names that index.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_manifest_lists_record_paths_and_the_selecting_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, dependency, root) = registered_tool();
        let manager = manager_over(dir.path(), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let (_, manifest) = manifest_of(&manager, &request).await;
        let index = Algorithm::Sha256.hash(index_document(&root, &[("linux", "amd64")]).as_bytes());

        let tool = manifest
            .packages
            .iter()
            .find(|p| p.digest == root.to_string())
            .expect("root");
        let dep = manifest
            .packages
            .iter()
            .find(|p| p.digest == dependency.to_string())
            .expect("dependency");
        assert_eq!(
            tool.index_digest,
            Some(index.to_string()),
            "the root was resolved through an index"
        );
        assert_eq!(dep.index_digest, None, "a dependency never names an index");

        let key = package_key(&root);
        assert_eq!(
            tool.entrypoints[0].record_path.as_deref(),
            Some(VendorLayout::relative_entrypoint_record(&key, "hello").as_str())
        );
        assert_eq!(
            tool.entrypoints[0].record_path.as_deref(),
            Some(format!("{}/e/hello.launch", tool.package_dir).as_str())
        );
        let records: Vec<_> = manifest
            .tools
            .iter()
            .map(|t| (t.name.as_str(), t.record_path.as_deref()))
            .collect();
        assert_eq!(
            records,
            [("cmake", Some("l/cmake.launch")), ("hello", Some("l/hello.launch"))]
        );
    }

    /// A platform-manifest identifier skips the index, so no index digest exists to name.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_root_named_by_its_platform_manifest_has_no_index_digest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, leaf) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let manager = manager_over(dir.path(), source);
        let pinned = PackageRef::new_registry("tool", REGISTRY).clone_with_digest(leaf.clone());
        let request = packages_request(vec![pinned], linux_amd64());

        let (_, manifest) = manifest_of(&manager, &request).await;

        assert_eq!(manifest.packages.len(), 1);
        assert_eq!(manifest.packages[0].digest, leaf.to_string());
        assert_eq!(manifest.packages[0].index_digest, None);
    }

    /// `record_path` is exactly the file the writers place beside each launcher, on a Windows target too.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_record_path_is_the_file_the_writers_place() {
        for (os, windows) in [("linux", false), ("windows", true)] {
            let dir = tempfile::tempdir().expect("tempdir");
            let (source, _, root) = registered_tool_on((os, "amd64"));
            let manager = manager_over(&dir.path().join("home"), source);
            let platform: Platform = format!("{os}/amd64").parse().expect("platform");
            let request = packages_request(vec![package("tool", "1.0")], platform);
            let (_, manifest) = manifest_of(&manager, &request).await;
            let package = manifest
                .packages
                .iter()
                .find(|p| p.digest == root.to_string())
                .expect("root");
            let layout = VendorLayout::new(dunce::canonicalize(dir.path()).expect("canonical"));
            let program = format!("{}/bin/hello{}", package.content_dir, if windows { ".exe" } else { "" });
            std::fs::create_dir_all(layout.resolve(&program).parent().expect("parent")).expect("directory");
            std::fs::write(layout.resolve(&program), b"x").expect("file");
            let declared = HashMap::from([(
                package.digest.clone(),
                serde_json::from_str(r#"{"hello":{}}"#).expect("entrypoints"),
            )]);

            write_entrypoint_launchers(&layout, &manifest, &declared, &[], b"blob", windows)
                .await
                .expect("the entry-point launchers write");
            write_selection_launchers(&layout, &manifest, b"blob", windows)
                .await
                .expect("the selection launchers write");

            let mut listed = vec![(
                package.entrypoints[0].launcher_path.as_str(),
                package.entrypoints[0].record_path.as_deref(),
            )];
            listed.extend(
                manifest
                    .tools
                    .iter()
                    .map(|t| (t.launcher_path.as_deref().expect("launcher"), t.record_path.as_deref())),
            );
            assert_eq!(listed.len(), 3, "{os}");
            for (launcher, record) in listed {
                let record = record.unwrap_or_else(|| panic!("{os}: {launcher} lists no record"));
                assert!(layout.resolve(launcher).is_file(), "{os}: {launcher}");
                assert!(layout.resolve(record).is_file(), "{os}: the writer placed no {record}");
                assert!(!record.ends_with(".exe.launch"), "{record}");
            }
        }
    }

    /// A binary name ending in `.launch` would be a launcher's record file: refused with 65 on every platform.
    ///
    /// Entry-point names cannot reach this: the metadata grammar already refuses a `.`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_name_ending_in_the_record_suffix_is_refused() {
        for name in ["cmake.launch", "cmake.LAUNCH"] {
            for platform in ["linux/amd64", "windows/amd64"] {
                let dir = tempfile::tempdir().expect("tempdir");
                let (source, root) = register_package(
                    FakeManifestSource::default(),
                    serde_json::json!({"type": "bundle", "version": 1, "binaries": [name], "entrypoints": {}, "env": []}),
                );
                let (os, arch) = platform.split_once('/').expect("platform");
                let manager = manager_over(dir.path(), register_index(source, "1.0", &root, &[(os, arch)]));
                let request = packages_request(vec![package("tool", "1.0")], platform.parse().expect("platform"));
                let selection = select(&manager, &request).await.expect("the selection reads");

                let error = build_manifest(&manager, &request, &selection, &|_| true)
                    .await
                    .expect_err(name);

                assert!(
                    matches!(error, VendorError::PathUnrepresentable { .. }),
                    "{name} {platform}: {error:?}"
                );
                assert_eq!(error.classify(), Some(ExitCode::DataError), "{name} {platform}");
            }
        }
    }

    /// Names equal after ASCII folding share one launcher and one record on a Windows or macOS target.
    #[tokio::test(flavor = "multi_thread")]
    async fn names_equal_after_folding_collapse_to_one_record_on_a_folding_target() {
        for (platform, expected) in [("linux/amd64", 2), ("windows/amd64", 1), ("darwin/arm64", 1)] {
            let dir = tempfile::tempdir().expect("tempdir");
            // One package cannot claim both spellings, so the root and its dependency each claim one.
            let (source, dependency) = register_package(
                FakeManifestSource::default(),
                serde_json::json!({"type": "bundle", "version": 1, "binaries": ["tool"], "entrypoints": {}, "env": []}),
            );
            let (source, root) = register_package(
                source,
                serde_json::json!({
                    "type": "bundle", "version": 1, "binaries": ["Tool"], "entrypoints": {}, "env": [],
                    "dependencies": [{"identifier": format!("{REGISTRY}/dep@{dependency}"), "visibility": "public", "name": "dep"}],
                }),
            );
            let (os, arch) = platform.split_once('/').expect("platform");
            let manager = manager_over(dir.path(), register_index(source, "1.0", &root, &[(os, arch)]));
            let request = packages_request(vec![package("tool", "1.0")], platform.parse().expect("platform"));

            let (_, manifest) = manifest_of(&manager, &request).await;

            assert_eq!(manifest.tools.len(), expected, "{platform}");
            let records: BTreeSet<_> = manifest.tools.iter().map(|t| t.record_path.clone()).collect();
            assert_eq!(records.len(), expected, "{platform}: every record is its own file");
        }
    }

    /// A package declaring an empty `binaries` list claims no name, the same as one declaring nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_binaries_claim_is_not_a_declared_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, root) = register_package(
            FakeManifestSource::default(),
            serde_json::json!({"type": "bundle", "version": 1, "binaries": [], "env": []}),
        );
        let source = register_index(source, "1.0", &root, &[("linux", "amd64")]);
        let manager = manager_over(dir.path(), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());

        let (_, manifest) = manifest_of(&manager, &request).await;

        assert!(!manifest.packages[0].names_declared);
        assert!(manifest.tools.is_empty());
    }

    /// A platform with no launcher blob lists its tools without a `launcher_path`.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_launcher_blob_leaves_launcher_path_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, _, _) = registered_tool();
        let manager = manager_over(dir.path(), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let selection = select(&manager, &request).await.expect("the selection reads");

        let manifest = build_manifest(&manager, &request, &selection, &|_| false)
            .await
            .expect("the manifest builds");

        assert!(manifest.tools.iter().all(|tool| tool.launcher_path.is_none()));
        assert!(!manifest.tools.is_empty());
    }

    /// A package that spells the token itself is refused, where the token the composer writes is not.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_package_spelling_the_token_is_unrepresentable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, leaf) = register_package(
            FakeManifestSource::default(),
            serde_json::json!({
                "type": "bundle", "version": 1, "dependencies": [], "entrypoints": {},
                "env": [{"key": "ROOT", "type": "constant", "value": "@VENDOR_DIR@/x", "visibility": "public"}],
            }),
        );
        let manager = manager_over(dir.path(), register_index(source, "1.0", &leaf, &[("linux", "amd64")]));
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let selection = select(&manager, &request).await.expect("the selection reads");

        let error = build_manifest(&manager, &request, &selection, &|_| true)
            .await
            .expect_err("the token in a package value");

        assert!(matches!(error, VendorError::ValueUnrepresentable { .. }), "{error:?}");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn a_host_path_or_unresolved_template_in_a_value_is_unrepresentable() {
        let item = |value: &str| EnvironmentItem {
            key: "K".to_owned(),
            value: value.to_owned(),
            kind: ocx_package::metadata::env::modifier::ModifierKind::Constant,
            separator: None,
        };
        let forbidden = ["/home/me/.ocx".to_owned()];

        check_item(&item("@VENDOR_DIR@/packages/x"), &forbidden).expect("control: the token is allowed");
        for value in ["${deps.x.installPath}", "/home/me/.ocx/packages/x", "a\0b"] {
            let error = check_item(&item(value), &forbidden).expect_err(value);
            assert!(
                matches!(error, VendorError::ValueUnrepresentable { .. }),
                "{value}: {error:?}"
            );
        }
    }

    /// The bytes are the manifest and a newline, and equal on a second run.
    #[tokio::test(flavor = "multi_thread")]
    async fn manifest_only_writes_exactly_one_deterministic_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = "application/vnd.sh.ocx.package.v1";
        let (source, dependency) = register_package_typed(
            FakeManifestSource::default(),
            serde_json::json!({"type": "bundle", "version": 1, "dependencies": [], "entrypoints": {}, "env": []}),
            artifact,
        );
        let (source, root) = register_package_typed(
            source,
            serde_json::json!({
                "type": "bundle", "version": 1, "binaries": ["cmake"], "entrypoints": {},
                "dependencies": [{"identifier": format!("{REGISTRY}/dep@{dependency}"), "visibility": "public", "name": "dep"}],
                "env": [{"key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public"}],
            }),
            artifact,
        );
        let source = register_index(source, "1.0", &root, &[("linux", "amd64")]);
        let manager = manager_over(&dir.path().join("home"), source);
        let mut request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let selection = select(&manager, &request).await.expect("the selection reads");
        let manifest = build_manifest(&manager, &request, &selection, &|_| true)
            .await
            .expect("manifest");

        let mut written = Vec::new();
        for run in ["first", "second"] {
            let output = dir.path().join(run);
            request.output = output.clone();
            publish(&output, stage(&output).await.expect("stages"), &manifest)
                .await
                .expect("publishes");
            let entries: Vec<_> = std::fs::read_dir(&output)
                .expect("dir")
                .map(|e| e.expect("entry").file_name())
                .collect();
            assert_eq!(entries, [std::ffi::OsString::from("manifest.json")]);
            written.push(std::fs::read(output.join("manifest.json")).expect("read"));
        }

        assert_eq!(written[0], written[1]);
        assert_eq!(written[0].last(), Some(&b'\n'));
        let parsed: VendorManifest = serde_json::from_slice(&written[0]).expect("parses back");
        assert_eq!(parsed, manifest);
    }

    /// A non-empty `DIR` is refused before the selection reads anything.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_non_empty_output_is_refused_before_the_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manager = manager_over(&dir.path().join("home"), FakeManifestSource::default());
        let output = dir.path().join("out");
        std::fs::create_dir(&output).expect("dir");
        std::fs::write(output.join("keep"), "x").expect("file");
        let mut request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        request.output = output.clone();

        let error = manager.vendor(request).await.expect_err("non-empty output");

        assert!(matches!(error, VendorError::OutputNotEmpty { .. }), "{error:?}");
        assert!(output.join("keep").is_file(), "DIR is left as it was");
    }
    async fn manifest_for_publish(dir: &Path) -> VendorManifest {
        let (source, _, _) = registered_tool();
        let manager = manager_over(&dir.join("home"), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let selection = select(&manager, &request).await.expect("the selection reads");
        build_manifest(&manager, &request, &selection, &|_| true)
            .await
            .expect("manifest")
    }

    /// A `DIR` that gained content after the early check is the user's output-not-empty refusal and stays as it was.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dir_that_gains_a_file_before_publish_is_refused_and_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_for_publish(dir.path()).await;
        let output = dir.path().join("out");
        std::fs::create_dir(&output).expect("empty DIR passes the early check");
        let staged = stage(&output).await.expect("stages");
        std::fs::write(output.join("late"), "late").expect("the file arrives after the check");

        let error = publish(&output, staged, &manifest)
            .await
            .expect_err("non-empty at publish");

        assert!(matches!(error, VendorError::OutputNotEmpty { .. }), "{error:?}");
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
        assert_eq!(std::fs::read(output.join("late")).expect("kept"), b"late");
        assert_eq!(std::fs::read_dir(&output).expect("dir").count(), 1);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("parent")
            .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".ocx-vendor-"))
            .collect();
        assert!(leftovers.is_empty(), "staging is removed: {leftovers:?}");
    }

    /// A file at `DIR` is the same refusal.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_file_at_dir_at_publish_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_for_publish(dir.path()).await;
        let output = dir.path().join("out");
        let staged = stage(&output).await.expect("stages");
        std::fs::write(&output, "a file").expect("a file takes the name");

        let error = publish(&output, staged, &manifest).await.expect_err("a file at DIR");

        assert!(matches!(error, VendorError::OutputNotEmpty { .. }), "{error:?}");
        assert_eq!(std::fs::read(&output).expect("kept"), b"a file");
    }

    /// An absent or empty `DIR` is published, and the empty one is replaced.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn an_absent_or_empty_dir_is_published() {
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = manifest_for_publish(dir.path()).await;
        for name in ["absent", "empty"] {
            let output = dir.path().join(name);
            if name == "empty" {
                std::fs::create_dir(&output).expect("dir");
            }
            publish(&output, stage(&output).await.expect("stages"), &manifest)
                .await
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            assert!(output.join(MANIFEST_FILE).is_file(), "{name}");
        }
    }

    /// A symlink at `DIR` is refused even when it points at an empty directory; an absent `DIR` is free.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_at_the_output_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).expect("dir");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&empty, &link).expect("symlink");

        let error = ensure_output_is_free(&link).await.expect_err("a symlink is refused");
        assert!(matches!(error, VendorError::OutputNotEmpty { .. }), "{error:?}");
        assert_eq!(error.classify(), Some(ExitCode::UsageError));

        ensure_output_is_free(&empty)
            .await
            .expect("control: an empty directory is free");
        ensure_output_is_free(&dir.path().join("absent"))
            .await
            .expect("control: an absent path is free");
    }

    // ── Content half: launchers and sidecars over a staged tree ──────────────

    use ocx_env_apply::launch::DirectRecord as WrittenSidecar;

    /// A staging tree holding `files` (paths relative to the root), and the manifest of the registered tool.
    async fn staged_tool(files: &[String]) -> (tempfile::TempDir, VendorLayout, VendorManifest) {
        let dir = tempfile::tempdir().expect("tempdir");
        let (source, _, _) = registered_tool();
        let manager = manager_over(&dir.path().join("home"), source);
        let request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        let (_, manifest) = manifest_of(&manager, &request).await;
        let layout = VendorLayout::new(dunce::canonicalize(dir.path()).expect("canonical"));
        for file in files {
            let path = layout.resolve(file);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
            std::fs::write(path, file).expect("file");
        }
        (dir, layout, manifest)
    }

    fn root_package(manifest: &VendorManifest) -> &Package {
        manifest
            .packages
            .iter()
            .find(|package| !package.entrypoints.is_empty())
            .expect("the package with the entry point")
    }

    fn sidecar_at(layout: &VendorLayout, launcher: &str) -> WrittenSidecar {
        serde_json::from_slice(&std::fs::read(layout.resolve(&format!("{launcher}.launch"))).expect("sidecar"))
            .expect("decodes")
    }

    /// The entry point's launcher runs the command found on its own environment's `PATH`, the last-applied entry first.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_entrypoint_launcher_starts_the_command_its_environment_finds_first() {
        let (_dir, _, manifest) = staged_tool(&[]).await;
        let package = root_package(&manifest);
        let dependency = manifest
            .packages
            .iter()
            .find(|p| p.digest != package.digest)
            .expect("dependency");
        let files = [
            format!("{}/bin/hello", dependency.content_dir),
            format!("{}/bin/hello", package.content_dir),
        ];
        let (_dir, layout, manifest) = staged_tool(&files).await;
        let declared: Entrypoints =
            serde_json::from_str(r#"{"hello":{"args":["--root","${installPath}/share"]}}"#).expect("entrypoints");
        let declared = HashMap::from([(package.digest.clone(), declared)]);

        write_entrypoint_launchers(&layout, &manifest, &declared, &[], b"blob", false)
            .await
            .expect("the launchers write");

        let launcher = &package.entrypoints[0].launcher_path;
        assert_eq!(std::fs::read(layout.resolve(launcher)).expect("launcher"), b"blob");
        let sidecar = sidecar_at(&layout, launcher);
        assert_eq!(sidecar.self_path, *launcher);
        assert_eq!(sidecar.vendor_id, manifest.vendor_id);
        assert_eq!(
            sidecar.program,
            format!("{DIR_TOKEN}/{}/bin/hello", package.content_dir),
            "the package's own PATH entry is applied last, so it is searched first"
        );
        assert_eq!(
            sidecar.args,
            [
                "--root".to_owned(),
                format!("{DIR_TOKEN}/{}/share", package.content_dir)
            ]
        );
        let private = &manifest
            .environments
            .iter()
            .find(|e| e.id == package.entrypoints[0].environment)
            .expect("environment")
            .items;
        assert_eq!(sidecar.env.len(), private.len());
        assert!(
            sidecar
                .env
                .iter()
                .zip(private)
                .all(|(record, item)| record.key == item.key && record.value == item.value)
        );
        assert!(!sidecar.unset.is_empty(), "the writer fills the credential keys");
    }

    /// A baked argument is held to the rules of an environment value: it never spells the vendor token itself.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_entrypoint_argument_spelling_the_vendor_token_is_unrepresentable() {
        let files = {
            let (_dir, _, manifest) = staged_tool(&[]).await;
            let package = root_package(&manifest);
            vec![format!("{}/bin/hello", package.content_dir)]
        };
        let (_dir, layout, manifest) = staged_tool(&files).await;
        let package = root_package(&manifest);
        let declared: Entrypoints =
            serde_json::from_str(r#"{"hello":{"args":["--root=@VENDOR_DIR@/x"]}}"#).expect("entrypoints");
        let declared = HashMap::from([(package.digest.clone(), declared)]);

        let error = write_entrypoint_launchers(&layout, &manifest, &declared, &[], b"blob", false)
            .await
            .expect_err("the token is the writer's, not the author's");

        assert!(matches!(error, VendorError::ValueUnrepresentable { .. }), "{error:?}");
        assert!(!layout.resolve(&package.entrypoints[0].launcher_path).exists());
    }

    /// A baked argument naming this machine's home or `DIR` would leave the directory non-relocatable.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_entrypoint_argument_holding_a_host_path_is_unrepresentable() {
        let files = {
            let (_dir, _, manifest) = staged_tool(&[]).await;
            let package = root_package(&manifest);
            vec![format!("{}/bin/hello", package.content_dir)]
        };
        let (_dir, layout, manifest) = staged_tool(&files).await;
        let package = root_package(&manifest);
        let declared: Entrypoints =
            serde_json::from_str(r#"{"hello":{"args":["--home=/home/me/.ocx/x"]}}"#).expect("entrypoints");
        let declared = HashMap::from([(package.digest.clone(), declared)]);
        let forbidden = ["/home/me/.ocx".to_owned()];

        let error = write_entrypoint_launchers(&layout, &manifest, &declared, &forbidden, b"blob", false)
            .await
            .expect_err("the host path survives the replacement");

        assert!(matches!(error, VendorError::ValueUnrepresentable { .. }), "{error:?}");
    }

    /// A command no `PATH` entry of the environment holds has no program to start.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_entrypoint_command_on_no_path_entry_is_unrepresentable() {
        let (_dir, layout, manifest) = staged_tool(&[]).await;
        let package = root_package(&manifest);
        let declared: Entrypoints = serde_json::from_str(r#"{"hello":{}}"#).expect("entrypoints");
        let declared = HashMap::from([(package.digest.clone(), declared)]);

        let error = write_entrypoint_launchers(&layout, &manifest, &declared, &[], b"blob", false)
            .await
            .expect_err("nothing to start");

        assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
        assert!(!layout.resolve(&package.entrypoints[0].launcher_path).exists());
    }

    /// Each exposed name gets a launcher under the selection's directory, starting its `target_path` in the selection's environment.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_selection_launcher_starts_the_tools_target_in_the_selection_environment() {
        let (_dir, layout, manifest) = staged_tool(&[]).await;

        write_selection_launchers(&layout, &manifest, b"blob", false)
            .await
            .expect("the launchers write");

        for tool in &manifest.tools {
            let launcher = tool.launcher_path.as_deref().expect("a launcher path");
            assert_eq!(std::fs::read(layout.resolve(launcher)).expect("launcher"), b"blob");
            let sidecar = sidecar_at(&layout, launcher);
            assert_eq!(sidecar.program, format!("{DIR_TOKEN}/{}", tool.target_path));
            assert_eq!(sidecar.self_path, launcher);
            assert_eq!(sidecar.vendor_id, manifest.vendor_id);
            assert_eq!(sidecar.env.len(), manifest.environments[0].items.len());
        }
        let written = std::fs::read_dir(layout.resolve("l")).expect("dir").count();
        assert_eq!(written, 2 * manifest.tools.len(), "a launcher and a sidecar per name");
    }

    /// A tool without a `launcher_path` (no blob for the platform) writes nothing, not even the directory.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tool_without_a_launcher_path_writes_nothing() {
        let (_dir, layout, mut manifest) = staged_tool(&[]).await;
        for tool in &mut manifest.tools {
            tool.launcher_path = None;
        }

        write_selection_launchers(&layout, &manifest, b"blob", false)
            .await
            .expect("nothing to write");

        assert!(!layout.resolve("l").exists());
    }

    #[test]
    fn windows_and_macos_targets_fold_case_and_linux_does_not() {
        let platform = |text: &str| text.parse::<Platform>().expect("platform");
        assert!(folds_case(&platform("windows/amd64")));
        assert!(folds_case(&platform("darwin/arm64")));
        assert!(!folds_case(&platform("linux/amd64")));
    }

    // ── A vendor run writes nothing into the index directory ─────────────────

    /// Every file under `root` with its bytes; an absent `root` is empty.
    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let bytes = std::fs::read(&path).expect("file");
                    files.insert(path, bytes);
                }
            }
        }
        files
    }

    /// Placing a digest-addressed package over a seeded, writable index leaves the index directory as it was:
    /// resolving an image index through a writable chain persists a dispatch object, so this fails
    /// when `place_package` resolves with the manager it was given.
    #[tokio::test(flavor = "multi_thread")]
    async fn placing_a_package_writes_nothing_into_the_index_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let (source, leaf) = register_leaf(FakeManifestSource::default(), serde_json::json!([]));
        let document = index_document(&leaf, &[("linux", "amd64")]);
        let index_digest = Algorithm::Sha256.hash(document.as_bytes());
        let source = source
            .with("1.0", document.as_bytes())
            .with(&index_digest.to_string(), document.as_bytes());
        let manager = manager_over(&home, source);
        let store = ocx_index::IndexStore::machine_local(&FileStructure::with_root(home));
        std::fs::create_dir_all(store.root()).expect("index directory");
        std::fs::write(store.root().join("seed.json"), b"{}").expect("seed");
        let before = snapshot(store.root());
        let layout = VendorLayout::new(dir.path().join("out"));

        // The fake registry serves no layers, so the pull ends in an error; the resolve it follows has run.
        let result = place_package(
            &manager,
            &linux_amd64(),
            &layout,
            SetupGroups::new(),
            index_digest.to_string(),
            &closure_node(&index_digest),
            &package_key(&index_digest),
        )
        .await;

        assert!(
            result.is_err(),
            "the fake registry serves no layers: {:?}",
            result.map(|_| ())
        );
        let after = snapshot(store.root());
        assert_eq!(
            after.keys().collect::<Vec<_>>(),
            before.keys().collect::<Vec<_>>(),
            "no file was added to the index directory"
        );
        assert_eq!(after, before, "the index directory is byte-identical");
    }

    /// An index that tells the writable instance from its read-only view: any read of the writable one is a
    /// write path a vendor run must not take; the view counts the reads it served.
    #[derive(Clone)]
    struct ViewProbe {
        view: bool,
        writable_reads: Arc<std::sync::atomic::AtomicUsize>,
        view_reads: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ViewProbe {
        fn touch(&self) {
            let counter = if self.view {
                &self.view_reads
            } else {
                &self.writable_reads
            };
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl ocx_index::IndexImpl for ViewProbe {
        async fn list_repositories(&self, _: &str) -> ocx_index::error::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> ocx_index::error::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<(Digest, ocx_oci::Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(
            &self,
            _: &PackageRef,
            _: ocx_index::IndexOperation,
        ) -> ocx_index::error::Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &PinnedPackageRef) -> ocx_index::error::Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn declared_publishers(&self, _: &PackageRef) -> ocx_index::error::Result<ocx_index::DeclaredPublishers> {
            self.touch();
            Ok(ocx_index::DeclaredPublishers::Undeclared)
        }
        async fn committed_publishers(
            &self,
            _: &PackageRef,
        ) -> ocx_index::error::Result<ocx_index::DeclaredPublishers> {
            self.touch();
            Ok(ocx_index::DeclaredPublishers::Undeclared)
        }
        fn box_clone(&self) -> Box<dyn ocx_index::IndexImpl> {
            Box::new(self.clone())
        }
        fn read_only_view(&self) -> Box<dyn ocx_index::IndexImpl> {
            Box::new(Self {
                view: true,
                ..self.clone()
            })
        }
    }

    /// The trust checks of a vendor run ask the index through its read-only view only: with a covering
    /// `signatures = "required"` registry the check reads the index, and every read lands on the view.
    /// Fails when `verify_selection` uses the manager it was given.
    #[tokio::test(flavor = "multi_thread")]
    async fn verifying_a_selection_reads_the_index_only_through_the_read_only_view() {
        use std::sync::atomic::Ordering::SeqCst;

        let dir = tempfile::tempdir().expect("tempdir");
        let probe = ViewProbe {
            view: false,
            writable_reads: Arc::default(),
            view_reads: Arc::default(),
        };
        let config: ocx_config::Config =
            toml::from_str(&format!("[registries.\"{REGISTRY}\"]\nsignatures = \"required\"\n")).expect("config");
        let auto_verify = super::super::auto_verify::AutoVerify::new(super::super::auto_verify::AutoVerifyInput {
            operator_policies: Vec::new(),
            project_policies: Vec::new(),
            config,
            registry_client: ocx_oci::Client::with_transport(Box::new(
                ocx_oci::client::test_transport::StubTransport::new(
                    ocx_oci::client::test_transport::StubTransportData::new(),
                ),
            )),
            rekor_url: "http://127.0.0.1:3000".parse().expect("url"),
            offline: true,
            state: ocx_store::file_structure::StateStore::new(dir.path().join("state")),
            trusted_root_env: None,
            sigstore_trust: None,
            home_trusted_root: None,
            user_opted_out: false,
        });
        let manager = PackageManager::new(
            FileStructure::with_root(dir.path().join("home")),
            Index::from_impl(probe.clone()),
            None,
            REGISTRY,
        )
        .with_auto_verify(Some(auto_verify));
        let digest = Algorithm::Sha256.hash(b"leaf");
        let node = closure_node(&digest);
        let closures = ClosureSet {
            roots: vec![SelectedRoot {
                binding: None,
                requested: node.identifier.as_identifier().clone(),
                pinned: node.identifier.clone(),
                index_digest: None,
                nodes: 0..1,
            }],
            surface: vec![node],
        };

        // The registry requires signatures and declares no publisher, so the check refuses; the read it made is the subject.
        let _ = verify_selection(&manager, &closures).await;

        assert!(probe.view_reads.load(SeqCst) > 0, "the check read the index (control)");
        assert_eq!(
            probe.writable_reads.load(SeqCst),
            0,
            "no read went to the writable index"
        );
    }

    /// The call in `write_content`, not the helper: a declared binary on disk without any executable mode bit
    /// is listed `executable: true`, a file nothing declares keeps `false`.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn write_content_marks_the_declared_binary_executable_though_its_mode_bits_are_clear() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = "application/vnd.sh.ocx.package.v1";
        let (source, dependency) = register_package_typed(
            FakeManifestSource::default(),
            serde_json::json!({"type": "bundle", "version": 1, "dependencies": [], "entrypoints": {}, "env": []}),
            artifact,
        );
        let (source, root) = register_package_typed(
            source,
            serde_json::json!({
                "type": "bundle", "version": 1, "binaries": ["cmake"], "entrypoints": {},
                "dependencies": [{"identifier": format!("{REGISTRY}/dep@{dependency}"), "visibility": "public", "name": "dep"}],
                "env": [{"key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public"}],
            }),
            artifact,
        );
        let source = register_index(source, "1.0", &root, &[("linux", "amd64")]);
        let manager = manager_over(&dir.path().join("home"), source);
        let mut request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        request.output = dir.path().join("out");
        let (selection, mut manifest) = manifest_of(&manager, &request).await;
        let staging = dunce::canonicalize(dir.path()).expect("canonical");
        let layout = VendorLayout::new(&staging);
        let binary = manifest
            .tools
            .iter()
            .find(|tool| tool.kind == ToolKind::Binary)
            .expect("the cmake binary")
            .target_path
            .clone();
        let content = binary.rsplit_once("/bin/").expect("a binary under bin/").0.to_owned();
        for path in [binary.clone(), format!("{content}/share/readme")] {
            let file = layout.resolve(&path);
            std::fs::create_dir_all(file.parent().expect("parent")).expect("directory");
            std::fs::write(&file, &path).expect("file");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        }

        write_content(&manager, &request, &selection, &mut manifest, &staging)
            .await
            .expect("the content is written");

        let files: Vec<_> = manifest
            .packages
            .iter()
            .filter_map(|package| package.tree.as_ref())
            .flat_map(|tree| &tree.files)
            .map(|file| (file.path.as_str(), file.executable))
            .collect();
        assert!(files.contains(&(binary.as_str(), true)), "{files:?}");
        assert!(
            files
                .iter()
                .any(|(path, executable)| path.ends_with("/share/readme") && !executable),
            "{files:?}"
        );
    }

    /// The refusal names the package with its tag, the claim, the program and who must act.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_declared_binary_the_content_lacks_names_the_package_and_the_claim() {
        let (_dir, layout, manifest) = staged_tool(&[]).await;
        let binary = manifest
            .tools
            .iter()
            .find(|tool| tool.kind == ToolKind::Binary)
            .expect("the cmake binary");
        let package = manifest
            .packages
            .iter()
            .find(|package| package.digest == binary.package)
            .expect("the owner");

        let error = ensure_binary_targets_exist(&layout, &manifest.tools, &manifest.packages)
            .await
            .expect_err("no file at that path");

        assert!(matches!(error, VendorError::PathUnrepresentable { .. }), "{error:?}");
        let message = error.to_string();
        for fragment in [
            package.identifier.as_str(),
            "tool:1.0",
            "`binaries`",
            "`cmake`",
            "does not hold",
            "publisher",
        ] {
            assert!(message.contains(fragment), "{fragment:?} missing from: {message}");
        }
    }

    /// `--index` lists the paths resolution read, once and sorted; without it the field is absent.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_manifest_lists_index_lookups_only_with_a_recorder() {
        let (source, _, _) = registered_tool();
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = FileStructure::with_root(dir.path().join("home"));
        let recorder = ocx_index::LookupRecorder::new();
        recorder.record("b.example/catalog.json", false);
        recorder.record("a.example/catalog.json", true);
        recorder.record("b.example/catalog.json", false);
        let store = ocx_index::IndexStore::machine_local(&fs).with_lookup_recorder(recorder);
        let index = Index::from_chained_with_content_store(
            LocalIndex::new(LocalConfig {
                index_store: store.clone(),
            }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
            fs.blobs.clone(),
        );
        let manager = PackageManager::new(fs, index, None, REGISTRY).with_index(store);
        let mut request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        request.output = dir.path().join("out");

        let outcome = manager.vendor(request).await.expect("the manifest-only run");

        let written: VendorManifest =
            serde_json::from_slice(&std::fs::read(&outcome.manifest_path).expect("manifest")).expect("decodes");
        let lookups = written.index_lookups.expect("the field is present with a recorder");
        assert_eq!(
            Some(lookups.clone()),
            manager.index_lookups(),
            "as the manager reports them"
        );
        let paths: Vec<&str> = lookups.iter().map(|lookup| lookup.path.as_str()).collect();
        assert!(
            paths.windows(2).all(|pair| pair[0] < pair[1]),
            "sorted, each once: {paths:?}"
        );
        assert!(
            paths.contains(&"a.example/catalog.json") && paths.contains(&"b.example/catalog.json"),
            "{paths:?}"
        );

        let (source, _, _) = registered_tool();
        let plain_dir = tempfile::tempdir().expect("tempdir");
        let plain = manager_over(&plain_dir.path().join("home"), source);
        let mut request = packages_request(vec![package("tool", "1.0")], linux_amd64());
        request.output = plain_dir.path().join("out");
        let outcome = plain.vendor(request).await.expect("the manifest-only run");
        let text = std::fs::read_to_string(&outcome.manifest_path).expect("manifest");
        assert!(!text.contains("index_lookups"), "absent without --index");
    }
}
