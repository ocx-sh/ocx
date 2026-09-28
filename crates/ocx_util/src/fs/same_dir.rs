// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Device + inode directory-identity check.
//!
//! Identity, not canonical-path equality: `canonicalize` does not case-fold, and a missed match
//! breaks `ProjectRegistry::register`'s no-self-link invariant (`adr_project_gc_symlink_ledger.md` § No self-link invariant).

use std::path::Path;

/// Windows file identity: volume serial number + 64-bit file index.
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

    // Opens a directory handle; adding `FILE_FLAG_OPEN_REPARSE_POINT` would stop a junction resolving to its target.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;

    let file = std::fs::OpenOptions::new()
        .access_mode(0) // no read/write, identity probe only
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file` holds a valid handle across the call and `info` is a valid, aligned out-pointer.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(FileId {
        volume_serial: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

/// Returns `true` when `a` and `b` denote the same directory by filesystem identity.
///
/// # Errors
///
/// When either path cannot be stat'd or opened, including when it does not exist.
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
        let ia = file_id(a)?;
        let ib = file_id(b)?;
        Ok(ia == ib)
    }
    #[cfg(not(any(unix, windows)))]
    {
        // No identity API here; canonical-path equality is the weaker fallback.
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
