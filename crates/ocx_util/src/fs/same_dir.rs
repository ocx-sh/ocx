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

/// Windows file-identity triple: volume serial number + 64-bit file index.
///
/// `BY_HANDLE_FILE_INFORMATION` splits the file index into two 32-bit
/// halves; collapsing them into a single `u64` keeps the equality check
/// readable.
#[cfg(windows)]
#[derive(PartialEq, Eq)]
struct FileId {
    volume_serial: u32,
    file_index: u64,
}

#[cfg(windows)]
fn file_id(path: &Path) -> std::io::Result<FileId> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};

    // `FILE_FLAG_BACKUP_SEMANTICS` (0x02000000) — see module doc above.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;

    let file = std::fs::OpenOptions::new()
        .access_mode(0) // no read/write, identity probe only
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` owns a valid open handle for the duration of this
    // call (closed on drop after we return). `&mut info` is a valid
    // writable pointer to a properly aligned `BY_HANDLE_FILE_INFORMATION`.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(FileId {
        volume_serial: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

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
        // The Windows inode equivalent is the (volume serial number, file
        // index) pair from `BY_HANDLE_FILE_INFORMATION`. We open each path
        // and pull that pair via `GetFileInformationByHandle` — the stable
        // counterpart to the nightly `MetadataExt::{volume_serial_number,
        // file_index}` accessors (rust-lang/rust#63010, gated behind the
        // `windows_by_handle` feature).
        //
        // `FILE_FLAG_BACKUP_SEMANTICS` is required to open a *directory*
        // handle on Windows. Opening with default flags (i.e. via
        // `File::open`) fails on directories. We deliberately *do not* set
        // `FILE_FLAG_OPEN_REPARSE_POINT`: a directory junction or symlink
        // must resolve to its target so identity matches the Unix
        // `dev`/`ino` semantics above.
        let ia = file_id(a)?;
        let ib = file_id(b)?;
        Ok(ia == ib)
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

    /// Windows mirror of `dir_and_symlink_to_it_are_same`: a directory and a
    /// junction pointing at it must compare equal by identity — the Win32
    /// `GetFileInformationByHandle` probe follows the reparse point because
    /// we do not pass `FILE_FLAG_OPEN_REPARSE_POINT`.
    #[test]
    #[cfg(windows)]
    fn dir_and_junction_to_it_are_same() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).expect("create real");
        let link = tmp.path().join("link");
        junction::create(&real, &link).expect("junction");

        assert_ne!(real.as_os_str(), link.as_os_str());
        assert!(
            same_dir(&real, &link).expect("same_dir on dir + junction-to-dir"),
            "a directory and a junction resolving to it must compare equal by identity"
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
