// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx vendor` and `ocx package vendor`: write a relocatable directory of packages, launchers and a manifest.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ocx_oci::{Digest, PackageRef, PinnedPackageRef, Platform};
use ocx_package::metadata::ValidMetadata;
use ocx_package::vendor::{FileEntry, LinkEntry, Tree};
use ocx_project::{LockedTool, ProjectError, ProjectErrorKind};
use ocx_store::file_structure::VendorLayout;
use ocx_util::fs::{DirWalker, WalkDecision};

use super::common::ClosureNode;
use super::inspect::{InspectOptions, InspectResult};
use crate::PackageManager;
use crate::error::{PackageError, PackageErrorKind};

/// What to vendor.
#[derive(Debug, Clone)]
pub enum VendorSelection {
    /// The project lock's tools in `groups`, narrowed to `names` when it is not empty.
    Project {
        tools: Vec<LockedTool>,
        groups: Vec<String>,
        names: Vec<String>,
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
    #[error("vendor output '{}' exists and is not an empty directory", .path.display())]
    #[exit(
        UsageError,
        slug = "vendor_output_not_empty",
        summary = "The vendor output directory exists and is not an empty directory"
    )]
    OutputNotEmpty { path: PathBuf },
    /// A named tool is declared in none of the selected groups.
    #[error("binding '{name}' not found in the selected groups ({}); pass `-g` to select another group", .groups.join(", "))]
    #[exit(
        UsageError,
        slug = "vendor_binding_not_selected",
        summary = "A named tool is declared in no selected group"
    )]
    BindingNotSelected { name: String, groups: Vec<String> },
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
        let _selection = select(self, &request).await?;
        unimplemented!("PackageManager::vendor")
    }
}

/// The selected packages, each with its metadata-only closure, and the platforms they all offer.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the document half of `PackageManager::vendor` reads every field once wired"
    )
)]
#[derive(Debug)]
struct Selection {
    roots: Vec<SelectedRoot>,
    /// C-023: the platforms on which every root has a leaf, computed before any leaf was chosen.
    available_platforms: Vec<String>,
}

/// One package the run vendors, resolved for the requested platform.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the document half of `PackageManager::vendor` reads every field once wired"
    )
)]
#[derive(Debug)]
struct SelectedRoot {
    /// The `ocx.toml` binding; `None` for a package named on the command line.
    binding: Option<String>,
    /// As the selection named it: a lock leaf, or the identifier on the command line.
    requested: PackageRef,
    pinned: PinnedPackageRef,
    metadata: ValidMetadata,
    /// The whole dependency closure, dependencies first and the root last (C-022).
    nodes: Vec<ClosureNode>,
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
        VendorSelection::Project { tools, groups, names } => {
            let tools = pick_project_tools(tools, groups, names)?;
            // From the lock's keys, before a leaf is chosen for `request.platform`, so every `-p` sees the same set.
            let available = shared_platforms(
                tools
                    .iter()
                    .map(|tool| tool.platforms.keys().cloned().collect::<BTreeSet<_>>()),
            );
            let picked = tools
                .iter()
                .map(|tool| {
                    Ok(PickedRoot {
                        binding: Some(tool.name.clone()),
                        requested: locked_identifier(tool, &request.platform)?,
                    })
                })
                .collect::<Result<Vec<_>, VendorError>>()?;
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
    let roots = read_closures(manager, picked, &request.platform).await?;
    verify_selection(manager, &roots).await?;
    Ok(Selection {
        roots,
        available_platforms,
    })
}

/// The lock entries `groups` selects, narrowed to `names` when it is not empty.
///
/// `ocx exec`'s selection over the lock alone: the request carries no `ocx.toml`, and `ocx vendor` has already
/// refused a stale lock. Resolution-free, so a sibling with no leaf for the platform cannot fail a narrower run.
///
/// # Errors
///
/// [`VendorError::BindingNotSelected`] for a name in no selected group; the duplicate-binding refusal of the
/// project tier when two selected groups lock one binding differently.
fn pick_project_tools<'a>(
    tools: &'a [LockedTool],
    groups: &[String],
    names: &[String],
) -> Result<Vec<&'a LockedTool>, VendorError> {
    let mut selected: Vec<&LockedTool> = Vec::new();
    let mut seen_groups: Vec<&String> = Vec::new();
    for group in groups {
        if seen_groups.contains(&group) {
            continue;
        }
        seen_groups.push(group);
        for tool in tools.iter().filter(|tool| tool.group == *group) {
            // Identical content under another selected group collapses; differing content stays for the duplicate check.
            let repeated = selected
                .iter()
                .any(|known| known.name == tool.name && ocx_project::locked_tool_content_equal(known, tool));
            if !repeated {
                selected.push(tool);
            }
        }
    }

    if !names.is_empty() {
        let mut narrowed: Vec<&LockedTool> = Vec::new();
        for (position, name) in names.iter().enumerate() {
            if names[..position].contains(name) {
                continue;
            }
            let before = narrowed.len();
            narrowed.extend(selected.iter().filter(|tool| tool.name == *name));
            if narrowed.len() == before {
                return Err(VendorError::BindingNotSelected {
                    name: name.clone(),
                    groups: groups.to_vec(),
                });
            }
        }
        selected = narrowed;
    }

    // After the name filter, so a binding the run does not name cannot fail it.
    for (position, tool) in selected.iter().enumerate() {
        if let Some(first) = selected[..position].iter().find(|known| known.name == tool.name) {
            return Err(VendorError::Package(crate::Error::Project(ocx_project::Error::from(
                ProjectError::new(
                    PathBuf::new(),
                    ProjectErrorKind::DuplicateToolAcrossSelectedGroups {
                        name: tool.name.clone(),
                        group_a: first.group.clone(),
                        group_b: tool.group.clone(),
                    },
                ),
            ))));
        }
    }
    Ok(selected)
}

/// `tool`'s host leaf for `platform`, under the tag its declaration carries.
///
/// # Errors
///
/// `no_host_leaf` (78) when the lock pins nothing for `platform`.
fn locked_identifier(tool: &LockedTool, platform: &Platform) -> Result<PackageRef, VendorError> {
    let leaf = tool
        .host_leaf(platform)
        .map_err(|error| VendorError::Package(error.into()))?;
    let untagged = PackageRef::new_registry(tool.repository.repository(), tool.repository.registry());
    let declared = tool
        .declared
        .as_deref()
        .and_then(|spelling| PackageRef::parse_with_default_registry(spelling, tool.repository.registry()).ok())
        .filter(|declared| {
            declared.registry() == untagged.registry() && declared.repository() == untagged.repository()
        });
    Ok(PinnedPackageRef::pin(declared.as_ref().unwrap_or(&untagged), leaf).into())
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

/// The platforms `packages` serve together, from their image indexes; a flat image manifest serves every platform.
async fn index_platforms(manager: &PackageManager, packages: &[PackageRef]) -> Result<Vec<String>, VendorError> {
    let inspected = manager
        .inspect_all(packages.to_vec(), Platform::any(), InspectOptions::default())
        .await
        .map_err(VendorError::Package)?;
    Ok(shared_platforms(inspected.into_iter().map(|result| {
        match result {
            InspectResult::Candidates { candidates, .. } => candidates
                .into_iter()
                .map(|candidate| candidate.platform.to_string())
                .collect(),
            InspectResult::Manifest { .. } | InspectResult::Resolved { .. } => {
                BTreeSet::from([Platform::any().to_string()])
            }
        }
    })))
}

/// Resolves every pick for `platform` and reads its closure from config blobs alone.
async fn read_closures(
    manager: &PackageManager,
    picked: Vec<PickedRoot>,
    platform: &Platform,
) -> Result<Vec<SelectedRoot>, VendorError> {
    let requested: Vec<PackageRef> = picked.iter().map(|root| root.requested.clone()).collect();
    let options = InspectOptions {
        resolve: true,
        closure: true,
    };
    let inspected = manager
        .inspect_all(requested, platform.clone(), options)
        .await
        .map_err(VendorError::Package)?;
    picked
        .into_iter()
        .zip(inspected)
        .map(|(root, result)| match result {
            InspectResult::Resolved {
                pinned,
                metadata,
                closure: Some(closure),
                ..
            } => Ok(SelectedRoot {
                binding: root.binding,
                requested: root.requested,
                pinned,
                metadata,
                nodes: closure.nodes,
            }),
            _ => Err(failed(
                &root.requested,
                PackageErrorKind::Internal(crate::error::file_error(
                    root.requested.to_string(),
                    std::io::Error::other("inspect returned no resolved closure"),
                )),
            )),
        })
        .collect()
}

/// C-024: the trust checks an install runs before it fetches a layer, once per package of the closure.
///
/// Sequential: a closure is a handful of packages and the first refusal must be the same one on every run.
async fn verify_selection(manager: &PackageManager, roots: &[SelectedRoot]) -> Result<(), VendorError> {
    let mut verified: Vec<PinnedPackageRef> = Vec::new();
    for root in roots {
        for node in &root.nodes {
            let leaf = node.identifier.strip_advisory();
            if verified.contains(&leaf) {
                continue;
            }
            // The request that named a root decides which declaration its publisher round reads; a dependency is pinned.
            let requested = if node.is_root {
                &root.requested
            } else {
                node.identifier.as_identifier()
            };
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
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "`PackageManager::vendor` calls it once the full path is wired")
)]
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
                directories_with_links.insert(relative_directory.clone());
                links.push(LinkEntry { path: relative, target });
            } else if file_type.is_file() {
                let metadata = entry
                    .metadata()
                    .await
                    .map_err(|error| VendorError::Package(crate::Error::InternalFile(path.clone(), error)))?;
                files.push(FileEntry {
                    path: relative,
                    executable: is_executable(&metadata),
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

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use ocx_exit::{ClassifyExitCode, ExitCode};

    use super::*;

    #[test]
    fn output_not_empty_is_a_usage_error_with_its_slug() {
        let error = VendorError::OutputNotEmpty {
            path: PathBuf::from("/out"),
        };
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
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

    // ── Selection half: closure, shared platforms, trust ─────────────────────

    use std::collections::BTreeMap;

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

    fn manager_over(root: &Path, source: FakeManifestSource) -> PackageManager {
        let fs = FileStructure::with_root(root.to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(&fs),
            }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// Registers a flat package manifest and its config blob; returns the manifest digest.
    fn register_leaf(source: FakeManifestSource, dependencies: serde_json::Value) -> (FakeManifestSource, Digest) {
        let config = serde_json::json!({
            "type": "bundle", "version": 1, "dependencies": dependencies, "entrypoints": {},
            "env": [{"key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public"}],
        })
        .to_string();
        let config_digest = Algorithm::Sha256.hash(config.as_bytes());
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
            size = config.len(),
        );
        let digest = Algorithm::Sha256.hash(manifest.as_bytes());
        let source = source
            .with(&digest.to_string(), manifest.as_bytes())
            .with_blob(&config_digest.to_string(), config.as_bytes());
        (source, digest)
    }

    /// Registers the tag `tag` as an image index offering `platforms`; the first platform's leaf is real.
    fn register_index(
        source: FakeManifestSource,
        tag: &str,
        leaf: &Digest,
        platforms: &[(&str, &str)],
    ) -> FakeManifestSource {
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
        let index = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{}]}}"#,
            entries.join(",")
        );
        source.with(tag, index.as_bytes())
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

    /// C-022: a selected package brings the dependency its edge admits, root last.
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

        let selected = &selection.roots[0];
        assert_eq!(
            selected.binding, None,
            "a package named on the command line has no binding"
        );
        assert_eq!(selected.pinned.digest(), root, "the leaf the index resolved to");
        assert!(selected.metadata.env().is_some(), "the root's metadata is read");
        let nodes = &selected.nodes;
        let digests: Vec<_> = nodes.iter().map(|node| node.identifier.digest()).collect();
        assert_eq!(digests, [dependency, root], "the dependency, then the root");
        assert!(nodes[1].is_root);
        assert!(
            super::super::inspect::admitted_on_surface(&nodes[0], false),
            "the public edge is admitted by the rule `ocx env` applies"
        );
    }

    /// C-023 and V-54: the platforms every root offers; one root lacking a platform leaves it out.
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

    fn locked(name: &str, group: &str, repository: &str, leaves: &[(&str, char)]) -> LockedTool {
        LockedTool {
            name: name.to_owned(),
            group: group.to_owned(),
            repository: ocx_oci::Repository::new(REGISTRY, repository),
            declared: Some(format!("{REGISTRY}/{repository}:1.0")),
            platforms: leaves
                .iter()
                .map(|(platform, fill)| ((*platform).to_owned(), Digest::Sha256(fill.to_string().repeat(64))))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn names_of(tools: &[&LockedTool]) -> Vec<String> {
        tools.iter().map(|tool| tool.name.clone()).collect()
    }

    /// Every named group, in order; a name narrows; an unnamed sibling is never resolved.
    #[test]
    fn project_selection_follows_groups_and_names() {
        let tools = [
            locked("cmake", "default", "cmake", &[("linux/amd64", 'a')]),
            locked("ccache", "build", "ccache", &[("linux/amd64", 'b')]),
            locked("win-only", "build", "win", &[("windows/amd64", 'c')]),
        ];
        let groups = ["default".to_owned(), "build".to_owned()];

        let all = pick_project_tools(&tools, &groups, &[]).expect("selects");
        assert_eq!(names_of(&all), ["cmake", "ccache", "win-only"]);

        let named = pick_project_tools(&tools, &groups, &["ccache".to_owned()]).expect("selects");
        assert_eq!(names_of(&named), ["ccache"]);

        let error = pick_project_tools(&tools, &groups, &["missing".to_owned()]).expect_err("unknown name");
        assert!(
            matches!(error, VendorError::BindingNotSelected { ref name, .. } if name == "missing"),
            "{error:?}"
        );
        assert_eq!(error.classify(), Some(ExitCode::UsageError));
    }

    /// Identical content under two groups collapses; a binding locked two ways is refused unless a name narrows past it.
    #[test]
    fn project_selection_collapses_equal_content_and_refuses_a_conflict() {
        let same = [
            locked("tool", "default", "tool", &[("linux/amd64", 'a')]),
            locked("tool", "build", "tool", &[("linux/amd64", 'a')]),
        ];
        let groups = ["default".to_owned(), "build".to_owned()];
        assert_eq!(
            names_of(&pick_project_tools(&same, &groups, &[]).expect("collapses")),
            ["tool"]
        );

        let differing = [
            locked("tool", "default", "tool", &[("linux/amd64", 'a')]),
            locked("tool", "build", "tool", &[("linux/amd64", 'b')]),
        ];
        let error = pick_project_tools(&differing, &groups, &[]).expect_err("conflict");
        assert!(
            matches!(error, VendorError::Package(crate::Error::Project(_))),
            "{error:?}"
        );
    }

    /// A platform outside the lock's keys is refused with the project tier's own `no_host_leaf` (78), before the network.
    #[test]
    fn a_platform_outside_the_lock_is_refused_with_no_host_leaf() {
        let tool = locked("cmake", "default", "cmake", &[("linux/amd64", 'a')]);
        let windows: Platform = "windows/amd64".parse().expect("platform");

        let error = locked_identifier(&tool, &windows).expect_err("no windows leaf");

        assert_eq!(error.classify(), Some(ExitCode::ConfigError), "{error:?}");
        // Control: the platform the lock holds resolves, under the declaration's tag.
        let identifier = locked_identifier(&tool, &linux_amd64()).expect("linux leaf");
        assert_eq!(identifier.tag(), Some("1.0"));
        assert_eq!(identifier.digest(), Some(Digest::Sha256("a".repeat(64))));
    }

    /// S-014: a lock holding only `windows/amd64` selects for it on any host; the lock's keys are the available set.
    #[test]
    fn a_foreign_only_lock_selects_for_its_platform() {
        let tool = locked("cmake", "default", "cmake", &[("windows/amd64", 'a')]);
        let windows: Platform = "windows/amd64".parse().expect("platform");

        locked_identifier(&tool, &windows).expect("the foreign leaf selects");
        assert_eq!(
            shared_platforms([tool.platforms.keys().cloned().collect::<BTreeSet<_>>()]),
            ["windows/amd64"]
        );
    }

    /// C-024: the manifest-only path runs the auto-verify hook; `required` with no publisher refuses it.
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

    /// Red until P7: a vendor run into an absent directory writes its manifest.
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
        assert_eq!(outcome.content, VendorContent::Absent);
    }
}
