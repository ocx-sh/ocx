// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The tree `ocx vendor` writes: where each package, launcher and the manifest sit under `DIR`, and the staged publication of `DIR`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ocx_oci::Digest;
use ocx_oci::cas_path::{CAS_SHARD_PREFIX_LEN, CAS_SHARD_TOTAL_LEN};
use ocx_util::error::FileError;
use ocx_util::prelude::*;

/// 1980-01-01T00:00:00Z: the mtime of every directory under `DIR`, the earliest a zip can store.
const NORMALISED_MTIME_SECS: u64 = 315_532_800;

/// Why a manifest path field is refused; the writer maps it to `vendor_path_unrepresentable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathViolation {
    Empty,
    Absolute,
    EmptySegment,
    DotSegment,
    ParentSegment,
    /// Windows targets only: `\`.
    Backslash,
    /// Windows targets only: `:`.
    Colon,
    /// Windows targets only: `CON`, `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, with any extension.
    DeviceName,
    /// Windows targets only: a segment ending in `.` or a space.
    TrailingDotOrSpace,
}

/// Path arithmetic for a vendor directory; every relative path is `/`-separated, as the manifest stores it.
#[derive(Debug, Clone)]
pub struct VendorLayout {
    root: PathBuf,
}

impl VendorLayout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `DIR/manifest.json`.
    pub fn manifest_file(&self) -> PathBuf {
        self.root.join("manifest.json")
    }

    /// `packages/<registry slug>/<algorithm>/<2 hex>/<30 hex>`.
    pub fn relative_package_dir(registry: &str, digest: &Digest) -> String {
        let (algorithm, hex) = digest.parts();
        format!(
            "packages/{}/{algorithm}/{}/{}",
            registry.to_relaxed_slug(),
            &hex[..CAS_SHARD_PREFIX_LEN],
            &hex[CAS_SHARD_PREFIX_LEN..CAS_SHARD_TOTAL_LEN]
        )
    }

    /// `<package_dir>/content`.
    pub fn relative_content_dir(registry: &str, digest: &Digest) -> String {
        format!("{}/content", Self::relative_package_dir(registry, digest))
    }

    /// `selections/<id>/shells/default/bin`.
    pub fn relative_launcher_dir(selection: &str) -> String {
        format!("selections/{selection}/shells/default/bin")
    }

    /// `DIR` joined with a validated relative path.
    pub fn resolve(&self, relative: &str) -> PathBuf {
        relative
            .split('/')
            .fold(self.root.clone(), |path, segment| path.join(segment))
    }

    /// Applies the manifest's path rules; the Windows-only rules apply when the target platform is `windows/*`.
    ///
    /// # Errors
    ///
    /// The first [`PathViolation`] found.
    pub fn check_relative_path(path: &str, platform_is_windows: bool) -> Result<(), PathViolation> {
        if path.is_empty() {
            return Err(PathViolation::Empty);
        }
        if path.starts_with('/') {
            return Err(PathViolation::Absolute);
        }
        for segment in path.split('/') {
            match segment {
                "" => return Err(PathViolation::EmptySegment),
                "." => return Err(PathViolation::DotSegment),
                ".." => return Err(PathViolation::ParentSegment),
                _ => {}
            }
            if platform_is_windows {
                check_windows_segment(segment)?;
            }
        }
        Ok(())
    }
}

fn check_windows_segment(segment: &str) -> Result<(), PathViolation> {
    if segment.contains('\\') {
        return Err(PathViolation::Backslash);
    }
    if segment.contains(':') {
        return Err(PathViolation::Colon);
    }
    // Windows resolves `NUL.txt` to the device too, so only the part before the first dot names it.
    let stem = segment.split('.').next().unwrap_or(segment).to_ascii_uppercase();
    let is_numbered = |prefix: &str| {
        stem.strip_prefix(prefix)
            .is_some_and(|digit| matches!(digit.as_bytes(), [b'1'..=b'9']))
    };
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || is_numbered("COM") || is_numbered("LPT") {
        return Err(PathViolation::DeviceName);
    }
    if segment.ends_with(['.', ' ']) {
        return Err(PathViolation::TrailingDotOrSpace);
    }
    Ok(())
}

/// A fresh sibling directory of `DIR` that is built whole, then renamed onto `DIR` or removed.
///
/// Dropping it without [`publish`](Self::publish) removes the staging directory.
#[derive(Debug)]
pub struct StagedTree {
    staging: tempfile::TempDir,
    target: PathBuf,
}

impl StagedTree {
    /// Creates `.ocx-vendor-<random>` (mode 0700) in `target`'s parent.
    ///
    /// # Errors
    ///
    /// The parent is missing or not writable.
    pub fn create(target: &Path) -> Result<Self, FileError> {
        let parent = match target.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let mut builder = tempfile::Builder::new();
        builder.prefix(".ocx-vendor-");
        // Created 0700 by `mkdir` itself, so no umask-wide window precedes a `chmod`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let staging = builder
            .tempdir_in(parent)
            .map_err(|error| FileError::new(parent, error))?;
        Ok(Self {
            staging,
            target: target.to_path_buf(),
        })
    }

    /// The directory to build into.
    pub fn path(&self) -> &Path {
        self.staging.path()
    }

    /// Normalises the tree (C-008), then renames it onto `target`, absent or empty.
    ///
    /// # Errors
    ///
    /// The target is a non-empty directory, a symlink or a file, or the rename failed; the
    /// staging directory is removed and `target` is as it was.
    pub fn publish(self) -> Result<(), FileError> {
        normalise_tree(self.staging.path())?;
        // `remove_dir` is the emptiness check and the removal in one call: a non-empty or
        // non-directory target fails here untouched, where list-then-remove would race.
        match std::fs::remove_dir(&self.target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(FileError::new(&self.target, error)),
        }
        let staging = self.staging.keep();
        std::fs::rename(&staging, &self.target).map_err(|error| {
            // The tree is ours alone; leaving it would strand a full copy beside `DIR`.
            let _ = std::fs::remove_dir_all(&staging);
            FileError::new(&self.target, error)
        })
    }

    /// Removes the staging directory.
    ///
    /// # Errors
    ///
    /// The removal failed.
    pub fn abort(self) -> Result<(), FileError> {
        let staging = self.staging.path().to_path_buf();
        self.staging.close().map_err(|error| FileError::new(staging, error))
    }
}

/// Sets every directory under `root`, `root` included, to 0755 and mtime 1980-01-01 (C-008), children first.
///
/// Regular files keep what the copy gave them. shortcut: a symlink keeps its write-time mtime,
/// since std cannot set one without following the link; add `utimensat` when `ocx_store` takes `libc`.
///
/// # Errors
///
/// An entry cannot be read or its mode or mtime cannot be set.
fn normalise_tree(root: &Path) -> Result<(), FileError> {
    let entries = std::fs::read_dir(root).map_err(|error| FileError::new(root, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| FileError::new(root, error))?;
        let file_type = entry.file_type().map_err(|error| FileError::new(entry.path(), error))?;
        if file_type.is_dir() {
            normalise_tree(&entry.path())?;
        }
    }
    normalise_dir(root).map_err(|error| FileError::new(root, error))
}

fn normalise_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    }
    let mut options = std::fs::OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // A directory handle needs backup semantics, and `set_modified` needs write access.
        options.write(true).custom_flags(0x0200_0000);
    }
    #[cfg(not(windows))]
    options.read(true);
    options
        .open(dir)?
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(NORMALISED_MTIME_SECS))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988";

    fn digest() -> Digest {
        Digest::Sha256(HEX.to_string())
    }

    #[test]
    fn package_dir_is_registry_algorithm_shard() {
        assert_eq!(
            VendorLayout::relative_package_dir("ocx.sh", &digest()),
            "packages/ocx.sh/sha256/1f/2e3d4c5b6a79881f2e3d4c5b6a7988"
        );
    }

    #[test]
    fn package_dir_slugs_a_registry_with_a_port() {
        assert_eq!(
            VendorLayout::relative_package_dir("localhost:5000", &digest()),
            "packages/localhost_5000/sha256/1f/2e3d4c5b6a79881f2e3d4c5b6a7988"
        );
    }

    #[test]
    fn content_dir_sits_under_package_dir() {
        assert_eq!(
            VendorLayout::relative_content_dir("ocx.sh", &digest()),
            "packages/ocx.sh/sha256/1f/2e3d4c5b6a79881f2e3d4c5b6a7988/content"
        );
    }

    #[test]
    fn launcher_dir_is_the_default_shell_bin() {
        assert_eq!(
            VendorLayout::relative_launcher_dir("default"),
            "selections/default/shells/default/bin"
        );
    }

    #[test]
    fn manifest_file_is_directly_under_root() {
        assert_eq!(
            VendorLayout::new("/out").manifest_file(),
            PathBuf::from("/out/manifest.json")
        );
    }

    #[test]
    fn ordinary_paths_pass_on_every_platform() {
        for windows in [false, true] {
            assert_eq!(
                VendorLayout::check_relative_path("packages/ocx.sh/sha256/1f/2e/content/bin/cmake", windows),
                Ok(())
            );
        }
    }

    #[test]
    fn structural_rules_apply_to_every_platform() {
        for windows in [false, true] {
            for (path, violation) in [
                ("", PathViolation::Empty),
                ("/abs/x", PathViolation::Absolute),
                ("a//b", PathViolation::EmptySegment),
                ("a/", PathViolation::EmptySegment),
                ("a/./b", PathViolation::DotSegment),
                ("a/../b", PathViolation::ParentSegment),
            ] {
                assert_eq!(
                    VendorLayout::check_relative_path(path, windows),
                    Err(violation),
                    "{path:?} windows={windows}"
                );
            }
        }
    }

    #[test]
    fn windows_only_rules_hold_on_windows_targets() {
        for (path, violation) in [
            ("a\\b", PathViolation::Backslash),
            ("C:/x", PathViolation::Colon),
            ("a:b", PathViolation::Colon),
            ("con", PathViolation::DeviceName),
            ("dir/NUL.txt", PathViolation::DeviceName),
            ("AUX.tar.gz", PathViolation::DeviceName),
            ("x/com1", PathViolation::DeviceName),
            ("x/LPT9.log", PathViolation::DeviceName),
            ("a./b", PathViolation::TrailingDotOrSpace),
            ("a /b", PathViolation::TrailingDotOrSpace),
            ("b/name.", PathViolation::TrailingDotOrSpace),
        ] {
            assert_eq!(
                VendorLayout::check_relative_path(path, true),
                Err(violation),
                "{path:?}"
            );
        }
    }

    #[test]
    fn windows_only_rules_do_not_apply_to_other_targets() {
        for path in ["a\\b", "a:b", "con", "dir/NUL.txt", "a./b", "a /b"] {
            assert_eq!(VendorLayout::check_relative_path(path, false), Ok(()), "{path:?}");
        }
    }

    #[test]
    fn similar_names_are_not_device_names() {
        for path in ["console", "com0", "com10", "lpt", "nul-x"] {
            assert_eq!(VendorLayout::check_relative_path(path, true), Ok(()), "{path:?}");
        }
    }

    fn staging_siblings(parent: &Path) -> Vec<String> {
        std::fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".ocx-vendor-"))
            .collect()
    }

    #[test]
    fn create_makes_a_private_sibling_of_the_target() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("out");

        let staged = StagedTree::create(&target).unwrap();

        assert_eq!(staged.path().parent(), Some(parent.path()));
        assert_eq!(staging_siblings(parent.path()).len(), 1);
        assert!(!target.exists(), "create never touches the target");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(staged.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[test]
    fn publish_renames_onto_an_absent_target() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("out");
        let staged = StagedTree::create(&target).unwrap();
        std::fs::write(staged.path().join("manifest.json"), b"{}").unwrap();

        staged.publish().unwrap();

        assert_eq!(std::fs::read(target.join("manifest.json")).unwrap(), b"{}");
        assert!(staging_siblings(parent.path()).is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn publish_replaces_an_empty_target_directory() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("out");
        std::fs::create_dir(&target).unwrap();
        let staged = StagedTree::create(&target).unwrap();
        std::fs::write(staged.path().join("manifest.json"), b"{}").unwrap();

        staged.publish().unwrap();

        assert!(target.join("manifest.json").is_file());
    }

    #[test]
    fn publish_into_a_non_empty_target_fails_and_leaves_it_untouched() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("out");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep"), b"keep").unwrap();
        let staged = StagedTree::create(&target).unwrap();

        assert!(staged.publish().is_err());

        assert_eq!(std::fs::read(target.join("keep")).unwrap(), b"keep");
        assert!(
            staging_siblings(parent.path()).is_empty(),
            "a failed publish removes staging"
        );
    }

    #[test]
    fn abort_removes_staging_and_leaves_the_target_absent() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("out");
        let staged = StagedTree::create(&target).unwrap();
        std::fs::write(staged.path().join("file"), b"x").unwrap();

        staged.abort().unwrap();

        assert!(staging_siblings(parent.path()).is_empty());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_planted_at_a_staging_child_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let outside = parent.path().join("outside");
        std::fs::write(&outside, b"original").unwrap();
        let source = parent.path().join("source");
        std::fs::write(&source, b"payload").unwrap();
        let staged = StagedTree::create(&parent.path().join("out")).unwrap();
        let child = staged.path().join("child");
        std::os::unix::fs::symlink(&outside, &child).unwrap();

        let result = crate::hardlink::LinkMode::Copy.place_in_existing_parent(&source, &child);

        assert!(result.is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn publish_normalises_directory_modes_and_mtimes() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("out");
        let staged = StagedTree::create(&target).unwrap();
        let nested = staged.path().join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(nested.join("file"), b"x").unwrap();

        staged.publish().unwrap();

        let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(NORMALISED_MTIME_SECS);
        for dir in [target.clone(), target.join("a"), target.join("a/b")] {
            let metadata = std::fs::metadata(&dir).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o755, "{dir:?}");
            assert_eq!(metadata.modified().unwrap(), epoch, "{dir:?}");
        }
        assert_ne!(
            std::fs::metadata(target.join("a/b/file")).unwrap().modified().unwrap(),
            epoch
        );
    }

    #[cfg(unix)]
    #[test]
    fn publish_onto_a_symlink_is_refused() {
        let parent = tempfile::tempdir().unwrap();
        let elsewhere = parent.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let target = parent.path().join("out");
        std::os::unix::fs::symlink(&elsewhere, &target).unwrap();
        let staged = StagedTree::create(&target).unwrap();

        assert!(staged.publish().is_err());

        assert!(std::fs::symlink_metadata(&target).unwrap().is_symlink());
        assert!(staging_siblings(parent.path()).is_empty());
    }
}
