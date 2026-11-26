// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx vendor` and `ocx package vendor`: write a relocatable directory of packages, launchers and a manifest.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ocx_oci::{Digest, PackageRef, Platform};
use ocx_package::vendor::{FileEntry, LinkEntry, Tree};
use ocx_project::LockedTool;
use ocx_store::file_structure::VendorLayout;
use ocx_util::fs::{DirWalker, WalkDecision};

use crate::PackageManager;

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
        let _ = request;
        unimplemented!("PackageManager::vendor")
    }
}

/// The files, links and link-free directories under `content_dir`, every path relative to `layout`'s root.
///
/// A link target must be relative and resolve inside `content_dir`; every path must pass the manifest's path
/// rules. `link_free_dirs` are the top-most directories with no link at or below them, `content_dir` itself
/// when the whole package has none.
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
        .filter(|directory| {
            directory.as_str() == content_dir
                || directory
                    .rsplit_once('/')
                    .is_some_and(|(parent, _)| tainted.contains(parent))
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
    async fn a_package_without_links_lists_its_content_directory_alone() {
        let (_root, layout) = tree_root(&["bin/tool", "share/doc/readme.txt"]);

        let tree = list_tree(&layout, CONTENT, false).await.expect("the tree lists");

        assert_eq!(tree.link_free_dirs, [CONTENT]);
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
