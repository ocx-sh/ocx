// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The tree `ocx vendor` writes: where each package, launcher and the manifest sit under `DIR`, and the staged publication of `DIR`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ocx_env_apply::windows_path::{SegmentViolation, check_segment};
use ocx_oci::Digest;
use ocx_util::error::FileError;

/// 1980-01-01T00:00:00Z: the mtime of every directory under `DIR`, the earliest a zip can store.
const NORMALISED_MTIME_SECS: u64 = 315_532_800;

/// Hex characters of a digest that name its package under `DIR/p/`.
const PACKAGE_KEY_LEN: usize = 16;

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
    /// Windows targets only: a control character or one of `<>"|?*`.
    ReservedCharacter,
    /// Windows targets only: `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9`, `LPT0`-`LPT9`, with any extension.
    DeviceName,
    /// Windows targets only: a segment ending in `.` or a space.
    TrailingDotOrSpace,
    /// A segment equal to a [`VendorLayout::RESERVED_SEGMENTS`] entry, ASCII case-insensitive.
    ReservedSegment,
}

/// The 16 hex characters that name a package under `DIR/p/`: the first 16 of its digest's hex part.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageKey(String);

impl PackageKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Two package digests of one run share a [`PackageKey`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageKeyClash {
    pub first: Digest,
    pub second: Digest,
}

/// Path arithmetic for a vendor directory; every relative path is `/`-separated, as the manifest stores it.
#[derive(Debug, Clone)]
pub struct VendorLayout {
    root: PathBuf,
}

impl VendorLayout {
    /// Names ocx never writes, so a consumer may use them; `check_relative_path` refuses them as a segment.
    pub const RESERVED_SEGMENTS: &'static [&'static str] = &["_ocx_reserved"];

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

    /// `l/<name>.launch`: the record beside a selection launcher.
    pub fn relative_record(name: &str) -> String {
        format!("l/{name}.launch")
    }

    /// `p/<HEX16>/e/<name>.launch`: the record beside a package entry-point launcher.
    pub fn relative_entrypoint_record(key: &PackageKey, name: &str) -> String {
        format!("p/{}/e/{name}.launch", key.as_str())
    }

    /// `l`: the directory of the selection launchers.
    pub fn relative_launcher_dir() -> &'static str {
        "l"
    }

    /// `p/<HEX16>`.
    pub fn relative_package_dir(key: &PackageKey) -> String {
        format!("p/{}", key.as_str())
    }

    /// `p/<HEX16>/c`.
    pub fn relative_content_dir(key: &PackageKey) -> String {
        format!("p/{}/c", key.as_str())
    }

    /// `l/<name>`, plus `.exe` for a Windows platform.
    pub fn relative_launcher(name: &str, windows: bool) -> String {
        format!("l/{name}{}", exe_suffix(windows))
    }

    /// `p/<HEX16>/e`.
    pub fn relative_entrypoint_dir(key: &PackageKey) -> String {
        format!("p/{}/e", key.as_str())
    }

    /// `p/<HEX16>/e/<name>`, plus `.exe` for a Windows platform.
    pub fn relative_entrypoint_launcher(key: &PackageKey, name: &str, windows: bool) -> String {
        format!("p/{}/e/{name}{}", key.as_str(), exe_suffix(windows))
    }

    /// Keys every digest by the first 16 hex characters of its hex part.
    ///
    /// # Errors
    ///
    /// [`PackageKeyClash`] naming two different digests that share a key.
    pub fn package_keys<'a>(
        digests: impl IntoIterator<Item = &'a Digest>,
    ) -> Result<BTreeMap<Digest, PackageKey>, PackageKeyClash> {
        let mut owner_of_key: BTreeMap<String, &Digest> = BTreeMap::new();
        let mut keys = BTreeMap::new();
        for digest in digests {
            let hex = digest.hex();
            let key = hex.get(..PACKAGE_KEY_LEN).unwrap_or(hex).to_string();
            // The same digest twice is one package; only two different digests can clash.
            match owner_of_key.get(&key) {
                Some(first) if *first != digest => {
                    return Err(PackageKeyClash {
                        first: (*first).clone(),
                        second: digest.clone(),
                    });
                }
                Some(_) => {}
                None => {
                    owner_of_key.insert(key.clone(), digest);
                }
            }
            keys.insert(digest.clone(), PackageKey(key));
        }
        Ok(keys)
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
            if Self::RESERVED_SEGMENTS
                .iter()
                .any(|reserved| segment.eq_ignore_ascii_case(reserved))
            {
                return Err(PathViolation::ReservedSegment);
            }
        }
        Ok(())
    }
}

fn exe_suffix(windows: bool) -> &'static str {
    if windows { ".exe" } else { "" }
}

fn check_windows_segment(segment: &str) -> Result<(), PathViolation> {
    check_segment(segment).map_err(|violation| match violation {
        SegmentViolation::Backslash => PathViolation::Backslash,
        SegmentViolation::Colon => PathViolation::Colon,
        SegmentViolation::ReservedCharacter => PathViolation::ReservedCharacter,
        SegmentViolation::DeviceName => PathViolation::DeviceName,
        SegmentViolation::TrailingDotOrSpace => PathViolation::TrailingDotOrSpace,
    })
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

    /// Normalises the tree, then renames it onto `target`, absent or empty.
    ///
    /// # Errors
    ///
    /// The target is a non-empty directory, a symlink or a file, or the rename failed; the
    /// staging directory is removed and `target` is as it was. A refused target carries the
    /// io kind `DirectoryNotEmpty`, `NotADirectory` or `AlreadyExists`.
    pub fn publish(self) -> Result<(), FileError> {
        normalise_tree(self.staging.path())?;
        let staging = self.staging.keep();
        replace_target(&staging, &self.target).map_err(|error| {
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

/// `rename(2)` replaces an empty directory atomically and refuses a non-empty one, a file or a link,
/// so nothing is removed first and a `DIR` created after the caller's check is never lost.
#[cfg(not(windows))]
fn replace_target(from: &Path, to: &Path) -> std::io::Result<()> {
    rename_tree(from, to)
}

/// Windows cannot rename onto an existing directory, so an empty one is removed first; `remove_dir`
/// is the emptiness check, and a link is refused beforehand because it would remove the link itself.
#[cfg(windows)]
fn replace_target(from: &Path, to: &Path) -> std::io::Result<()> {
    if ocx_util::fs::symlink::is_link(to) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "the target is a symlink or junction",
        ));
    }
    match std::fs::remove_dir(to) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    rename_tree(from, to)
}

#[expect(
    clippy::disallowed_methods,
    reason = "publishes a tree ocx just built in a private sibling of the empty or absent destination"
)]
fn rename_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Sets every directory under `root`, `root` included, to 0755 and mtime 1980-01-01, children first.
///
/// A symlink gets the same mtime on the link itself, never its target. Regular files keep what the copy gave them.
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
        } else if file_type.is_symlink() {
            let mtime = filetime::FileTime::from_unix_time(NORMALISED_MTIME_SECS as i64, 0);
            filetime::set_symlink_file_times(entry.path(), mtime, mtime)
                .map_err(|error| FileError::new(entry.path(), error))?;
        }
    }
    normalise_dir(root).map_err(|error| FileError::new(root, error))
}

/// Sets a file ocx wrote into `DIR` to `mode` (Unix only) and the normalised mtime, so its bytes do not depend on the host umask or clock.
///
/// # Errors
///
/// The mode or mtime cannot be set.
pub fn normalise_written_file(path: &Path, mode: u32) -> Result<(), FileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|error| FileError::new(path, error))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    let mtime = filetime::FileTime::from_unix_time(NORMALISED_MTIME_SECS as i64, 0);
    filetime::set_file_mtime(path, mtime).map_err(|error| FileError::new(path, error))
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

    /// Digests that differ after the 16th hex character.
    fn sibling_digest() -> Digest {
        Digest::Sha256(format!("{}{}", &HEX[..16], "0".repeat(48)))
    }

    #[test]
    fn contract_package_keys_refuses_a_hex16_clash_naming_both() {
        let (first, second) = (digest(), sibling_digest());
        let clash = VendorLayout::package_keys([&first, &second]).expect_err("a shared HEX16 is refused");
        let mut named = [clash.first.clone(), clash.second.clone()];
        named.sort();
        let mut expected = [first, second];
        expected.sort();
        assert_eq!(named, expected);
    }

    #[test]
    fn contract_package_keys_keys_by_first_16_hex() {
        let (a, b) = (digest(), Digest::Sha256("ab".repeat(32)));
        let keys = VendorLayout::package_keys([&a, &b]).expect("no clash");
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[&a].as_str(), "1f2e3d4c5b6a7988");
        assert_eq!(keys[&b].as_str(), "abababababababab");
    }

    #[test]
    fn contract_check_relative_path_refuses_reserved_segment() {
        for path in ["_ocx_reserved", "p/x/_OCX_RESERVED/y", "l/_Ocx_Reserved"] {
            assert_eq!(
                VendorLayout::check_relative_path(path, false),
                Err(PathViolation::ReservedSegment),
                "{path}"
            );
        }
        assert_eq!(VendorLayout::check_relative_path("p/_ocx_reserved_x/y", false), Ok(()));
    }

    /// One case per path rule of the manifest's path rules, in the order `check_relative_path` applies them.
    #[test]
    fn contract_check_relative_path_agrees_with_every_path_rule() {
        use PathViolation as V;
        // (path, platform_is_windows, expected)
        let cases: &[(&str, bool, Result<(), V>)] = &[
            ("", false, Err(V::Empty)),
            ("", true, Err(V::Empty)),
            ("/a", false, Err(V::Absolute)),
            ("a//b", false, Err(V::EmptySegment)),
            ("a/", false, Err(V::EmptySegment)),
            ("a/./b", false, Err(V::DotSegment)),
            ("a/../b", false, Err(V::ParentSegment)),
            ("a\\b", true, Err(V::Backslash)),
            ("a:b", true, Err(V::Colon)),
            ("a\u{1}b", true, Err(V::ReservedCharacter)),
            ("a\u{1f}b", true, Err(V::ReservedCharacter)),
            ("a<b", true, Err(V::ReservedCharacter)),
            ("a>b", true, Err(V::ReservedCharacter)),
            ("a\"b", true, Err(V::ReservedCharacter)),
            ("a|b", true, Err(V::ReservedCharacter)),
            ("a?b", true, Err(V::ReservedCharacter)),
            ("a*b", true, Err(V::ReservedCharacter)),
            ("CON", true, Err(V::DeviceName)),
            ("prn", true, Err(V::DeviceName)),
            ("Aux", true, Err(V::DeviceName)),
            ("nul.txt", true, Err(V::DeviceName)),
            ("NUL .txt", true, Err(V::DeviceName)),
            ("com1.tar.gz", true, Err(V::DeviceName)),
            ("COM0", true, Err(V::DeviceName)),
            ("LPT9", true, Err(V::DeviceName)),
            ("COM\u{b9}", true, Err(V::DeviceName)),
            ("COM\u{b2}", true, Err(V::DeviceName)),
            ("LPT\u{b3}", true, Err(V::DeviceName)),
            ("CONSOLE", true, Ok(())),
            ("COM10", true, Ok(())),
            ("my.CON", true, Ok(())),
            ("a./b", true, Err(V::TrailingDotOrSpace)),
            ("a /b", true, Err(V::TrailingDotOrSpace)),
            ("_ocx_reserved", false, Err(V::ReservedSegment)),
            ("_ocx_reserved", true, Err(V::ReservedSegment)),
            ("p/_OCX_Reserved/c", true, Err(V::ReservedSegment)),
            // Windows-only rules stay off for other targets; the reserved segment does not.
            ("a\\b", false, Ok(())),
            ("con", false, Ok(())),
            ("a./b", false, Ok(())),
            ("p/0123456789abcdef/c/bin/cmake", true, Ok(())),
        ];
        for (path, windows, expected) in cases {
            assert_eq!(
                VendorLayout::check_relative_path(path, *windows),
                *expected,
                "{path:?} windows={windows}"
            );
        }
    }

    #[test]
    fn rule_order_puts_structure_before_windows_rules_before_the_reserved_segment() {
        assert_eq!(
            VendorLayout::check_relative_path("a/./con", true),
            Err(PathViolation::DotSegment)
        );
        assert_eq!(
            VendorLayout::check_relative_path("con/_ocx_reserved", true),
            Err(PathViolation::DeviceName)
        );
    }

    #[test]
    fn short_layout_names() {
        let key = PackageKey("1f2e3d4c5b6a7988".to_string());
        assert_eq!(VendorLayout::relative_launcher_dir(), "l");
        assert_eq!(VendorLayout::relative_package_dir(&key), "p/1f2e3d4c5b6a7988");
        assert_eq!(VendorLayout::relative_content_dir(&key), "p/1f2e3d4c5b6a7988/c");
        assert_eq!(VendorLayout::relative_launcher("cmake", false), "l/cmake");
        assert_eq!(VendorLayout::relative_launcher("cmake", true), "l/cmake.exe");
        assert_eq!(VendorLayout::relative_entrypoint_dir(&key), "p/1f2e3d4c5b6a7988/e");
        assert_eq!(
            VendorLayout::relative_entrypoint_launcher(&key, "npm", false),
            "p/1f2e3d4c5b6a7988/e/npm"
        );
        assert_eq!(
            VendorLayout::relative_entrypoint_launcher(&key, "npm", true),
            "p/1f2e3d4c5b6a7988/e/npm.exe"
        );
    }

    #[test]
    fn the_same_digest_twice_is_not_a_clash() {
        let digest = digest();
        let keys = VendorLayout::package_keys([&digest, &digest]).expect("one digest, one key");
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn record_paths_sit_beside_their_launchers() {
        let key = PackageKey("1f2e3d4c5b6a7988".to_string());
        assert_eq!(VendorLayout::relative_record("cmake"), "l/cmake.launch");
        assert_eq!(
            VendorLayout::relative_entrypoint_record(&key, "npm"),
            "p/1f2e3d4c5b6a7988/e/npm.launch"
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
            ("bin/to?l", PathViolation::ReservedCharacter),
            ("bin/a\u{1b}b", PathViolation::ReservedCharacter),
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
        for path in ["console", "com10", "lpt", "nul-x"] {
            assert_eq!(VendorLayout::check_relative_path(path, true), Ok(()), "{path:?}");
        }
    }

    #[test]
    fn numbered_device_names_include_zero() {
        for path in ["COM0", "x/lpt0.txt"] {
            assert_eq!(
                VendorLayout::check_relative_path(path, true),
                Err(PathViolation::DeviceName),
                "{path:?}"
            );
            assert_eq!(VendorLayout::check_relative_path(path, false), Ok(()), "{path:?}");
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

    #[cfg(unix)]
    #[test]
    fn publish_refuses_a_non_empty_target_and_a_file_with_a_distinguishable_kind() {
        use std::io::ErrorKind;
        let parent = tempfile::tempdir().unwrap();
        let full = parent.path().join("full");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("keep"), b"keep").unwrap();
        let file = parent.path().join("file");
        std::fs::write(&file, b"a file").unwrap();

        for (target, kind) in [(&full, ErrorKind::DirectoryNotEmpty), (&file, ErrorKind::NotADirectory)] {
            let error = StagedTree::create(target).unwrap().publish().unwrap_err();
            assert_eq!(error.cause.kind(), kind, "{target:?}");
        }

        assert_eq!(std::fs::read(full.join("keep")).unwrap(), b"keep");
        assert_eq!(std::fs::read(&file).unwrap(), b"a file");
        assert!(staging_siblings(parent.path()).is_empty());
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

    #[cfg(unix)]
    #[test]
    fn publish_sets_a_symlinks_own_mtime_and_leaves_its_target() {
        let parent = tempfile::tempdir().unwrap();
        let target = parent.path().join("out");
        let staged = StagedTree::create(&target).unwrap();
        std::fs::write(staged.path().join("file"), b"x").unwrap();
        std::os::unix::fs::symlink("file", staged.path().join("link")).unwrap();
        let file_mtime = std::fs::metadata(staged.path().join("file"))
            .unwrap()
            .modified()
            .unwrap();

        staged.publish().unwrap();

        let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(NORMALISED_MTIME_SECS);
        assert_eq!(
            std::fs::symlink_metadata(target.join("link"))
                .unwrap()
                .modified()
                .unwrap(),
            epoch
        );
        assert_eq!(
            std::fs::metadata(target.join("file")).unwrap().modified().unwrap(),
            file_mtime
        );
    }
}
