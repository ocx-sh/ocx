// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The tree `ocx vendor` writes: where each package, launcher and the manifest sit under `DIR`, and the staged publication of `DIR`.

use std::path::{Path, PathBuf};

use ocx_oci::Digest;
use ocx_util::error::FileError;

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
    pub fn relative_package_dir(_registry: &str, _digest: &Digest) -> String {
        unimplemented!("vendor layout paths are implemented by the vendor copy-assembly step")
    }

    /// `<package_dir>/content`.
    pub fn relative_content_dir(_registry: &str, _digest: &Digest) -> String {
        unimplemented!("vendor layout paths are implemented by the vendor copy-assembly step")
    }

    /// `selections/<id>/shells/default/bin`.
    pub fn relative_launcher_dir(_selection: &str) -> String {
        unimplemented!("vendor layout paths are implemented by the vendor copy-assembly step")
    }

    /// `DIR` joined with a validated relative path.
    pub fn resolve(&self, _relative: &str) -> PathBuf {
        unimplemented!("vendor layout paths are implemented by the vendor copy-assembly step")
    }

    /// Applies the manifest's path rules; the Windows-only rules apply when the target platform is `windows/*`.
    ///
    /// # Errors
    ///
    /// The first [`PathViolation`] found.
    pub fn check_relative_path(_path: &str, _platform_is_windows: bool) -> Result<(), PathViolation> {
        unimplemented!("vendor layout paths are implemented by the vendor copy-assembly step")
    }
}

/// A fresh sibling directory of `DIR` that is built whole, then renamed onto `DIR` or removed.
#[derive(Debug)]
pub struct StagedTree {
    staging: PathBuf,
    target: PathBuf,
}

impl StagedTree {
    /// Creates `.ocx-vendor-<random>` (mode 0700) in `target`'s parent.
    ///
    /// # Errors
    ///
    /// The parent is missing or not writable.
    pub fn create(_target: &Path) -> Result<Self, FileError> {
        unimplemented!("staged publication is implemented by the vendor copy-assembly step")
    }

    /// The directory to build into.
    pub fn path(&self) -> &Path {
        &self.staging
    }

    /// Sets the top directory to 0755 and renames it onto `target`, absent or empty.
    ///
    /// # Errors
    ///
    /// The rename failed; the staging directory is removed and `target` is as it was.
    pub fn publish(self) -> Result<(), FileError> {
        let _ = &self.target;
        unimplemented!("staged publication is implemented by the vendor copy-assembly step")
    }

    /// Removes the staging directory.
    ///
    /// # Errors
    ///
    /// The removal failed.
    pub fn abort(self) -> Result<(), FileError> {
        unimplemented!("staged publication is implemented by the vendor copy-assembly step")
    }
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
}
