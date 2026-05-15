// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Device + inode directory-identity check.
//!
//! Answers "do these two paths denote the *same directory*?" using filesystem
//! identity (`dev`/`ino` on Unix; canonicalized-handle equivalence on Windows),
//! **not** canonical-path byte equality.
//!
//! Byte equality of `canonicalize` output is unsound on case-insensitive /
//! normalizing filesystems (macOS APFS default, Windows — both first-class
//! platforms per `product-context.md`): `tokio::fs::canonicalize` does not
//! case-fold, so two byte-different paths can denote the same directory.
//! `ProjectRegistry::register`'s no-self-link invariant (ADR
//! `adr_project_gc_symlink_ledger.md` §"No self-link invariant", review
//! correction ARCH-1b — silent-data-loss class) requires true identity, hence
//! this helper.

use std::path::Path;

/// Returns `true` when `a` and `b` denote the same directory by filesystem
/// identity (Unix `dev`+`ino`; Windows canonicalized-handle equivalence).
///
/// Both paths must exist and be directories. A non-existent path or an I/O
/// failure resolving identity surfaces as `Err`; callers decide whether that
/// is fatal (the registry treats it as "not the same dir, proceed").
///
/// # Errors
///
/// Returns the underlying [`std::io::Error`] when either path cannot be
/// stat'd / opened to determine its filesystem identity.
pub fn same_dir(a: &Path, b: &Path) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let ma = std::fs::metadata(a)?;
        let mb = std::fs::metadata(b)?;
        Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        // The Windows inode equivalent is the (volume serial number, file
        // index) pair. `std::fs::metadata` follows reparse points so a
        // directory junction/symlink resolves to its target's identity,
        // matching the Unix `dev`/`ino` semantics above.
        let ma = std::fs::metadata(a)?;
        let mb = std::fs::metadata(b)?;
        Ok(ma.volume_serial_number() == mb.volume_serial_number() && ma.file_index() == mb.file_index())
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No filesystem-identity API on this platform; fall back to
        // canonical-path equality. Documented weaker guarantee — OCX's
        // first-class platforms are all unix or windows.
        Ok(std::fs::canonicalize(a)? == std::fs::canonicalize(b)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two distinct directories are not the same directory by filesystem
    /// identity — distinct `dev`/`ino` (Unix) / file-index (Windows).
    #[test]
    fn distinct_dirs_are_not_same() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir(&a).expect("create a");
        std::fs::create_dir(&b).expect("create b");

        assert!(!same_dir(&a, &b).expect("same_dir on two distinct dirs"));
    }

    /// A directory and a symlink pointing at that directory ARE the same
    /// directory: `std::fs::metadata` follows the link, so identity matches.
    /// This is the regression this helper exists to prevent — path-byte
    /// equality would report `false` here on case-sensitive Linux because the
    /// two path strings differ, even though they denote one directory.
    #[test]
    #[cfg(unix)]
    fn dir_and_symlink_to_it_are_same() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).expect("create real");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        // Path bytes differ (`.../real` vs `.../link`) but both denote the
        // same directory by dev/ino — byte equality alone would fail here.
        assert_ne!(real.as_os_str(), link.as_os_str());
        assert!(
            same_dir(&real, &link).expect("same_dir on dir + symlink-to-dir"),
            "a directory and a symlink resolving to it must compare equal by identity"
        );
    }

    /// A non-existent path cannot be stat'd to determine identity → `Err`
    /// (callers, e.g. the registry, decide whether that is fatal).
    #[test]
    fn nonexistent_path_is_err() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does-not-exist");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).expect("create real");

        assert!(
            same_dir(&missing, &real).is_err(),
            "a non-existent path must surface the stat error as Err"
        );
    }
}
