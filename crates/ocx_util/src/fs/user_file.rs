// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publishing a file the user owns (shell profile, `ocx.toml`, docker `config.json`).
//!
//! Unlike the private-file publish, the user's symlinks, mode and ownership survive the write.

use std::io;
use std::path::{Path, PathBuf};

/// Mode of a file created because nothing was at the path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewFileMode {
    /// `0o666` clipped by the process umask, as a plain `File::create` would.
    UmaskDefault,
    /// `0o600`.
    Private,
}

/// Why a user-owned path was refused rather than written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UserFileRefusal {
    /// A symlink whose chain ends at nothing, loops, or runs past the hop limit.
    #[error("symlink does not resolve to an existing file")]
    DanglingSymlink,
    /// The path, or the end of its symlink chain, is not a regular file.
    #[error("not a regular file")]
    NotRegularFile,
    /// A link in the chain is owned by neither the effective user nor root.
    #[error("symlink is owned by another user")]
    ForeignSymlink,
}

fn refusal(r: UserFileRefusal) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, r)
}

/// The refusal an error from this module carries, if it is one.
pub fn user_file_refusal(err: &io::Error) -> Option<UserFileRefusal> {
    err.get_ref()?.downcast_ref::<UserFileRefusal>().copied()
}

/// A link may be followed when the effective user or root made it.
#[cfg(any(unix, test))]
fn link_owner_allowed(link_uid: u32, effective_uid: u32) -> bool {
    link_uid == effective_uid || link_uid == 0
}

/// Hops followed before a chain counts as a loop.
const MAX_LINK_HOPS: usize = 40;

/// Walk `path`'s symlink chain to the regular file it names; a non-link comes back unchanged. Blocking.
///
/// # Errors
///
/// A [`UserFileRefusal`] payload for a dangling, looping, foreign-owned or non-file chain.
pub fn resolve_user_file(path: &Path) -> io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {}
        Ok(_) => return Ok(path.to_path_buf()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(path.to_path_buf()),
        Err(err) => return Err(err),
    }

    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINK_HOPS {
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(refusal(UserFileRefusal::DanglingSymlink)),
            Err(err) => return Err(err),
        };
        if !meta.file_type().is_symlink() {
            if !meta.is_file() {
                return Err(refusal(UserFileRefusal::NotRegularFile));
            }
            return dunce::canonicalize(&current);
        }
        // Only the leaf chain is checked; a symlinked directory higher up the path is followed unchecked.
        check_link_owner(&meta)?;
        let target = std::fs::read_link(&current)?;
        current = match current.parent() {
            Some(parent) => parent.join(target),
            None => target,
        };
    }
    Err(refusal(UserFileRefusal::DanglingSymlink))
}

#[cfg(unix)]
fn check_link_owner(link: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    // SAFETY: `geteuid` takes no arguments, touches no memory, and always succeeds.
    let effective_uid = unsafe { libc::geteuid() };
    // Stops root or another user being steered through a link someone else planted; uid 0 links stay
    // followable because system-managed link farms (/nix/store) are root-owned.
    if link_owner_allowed(link.uid(), effective_uid) {
        Ok(())
    } else {
        Err(refusal(UserFileRefusal::ForeignSymlink))
    }
}

#[cfg(not(unix))]
fn check_link_owner(_link: &std::fs::Metadata) -> io::Result<()> {
    Ok(())
}

/// Replace the content of a user-owned file with `bytes`, keeping its symlinks, mode and owner. Blocking.
///
/// On Unix a file with one name is replaced by rename; a file with other hard links, or whose owner or
/// group the replacement cannot take, is overwritten in place. The rename path does not carry POSIX ACLs
/// or extended attributes (SELinux labels included). On Unix a read-only file in a directory this process
/// can write is replaced like any other and keeps its mode. On Windows only the content and the link
/// survive, and a file with the read-only attribute is refused with `PermissionDenied`.
///
/// # Errors
///
/// A [`UserFileRefusal`] payload for a path that must not be written; any other I/O failure as is.
pub fn replace_user_file(path: &Path, bytes: &[u8], new_file: NewFileMode) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let resolved = resolve_user_file(path)?;
            // Stat the end of the chain itself: a link swapped in since the walk must not lend its target's identity.
            let resolved_meta = std::fs::symlink_metadata(&resolved)?;
            if !resolved_meta.is_file() {
                return Err(refusal(UserFileRefusal::NotRegularFile));
            }
            replace_regular(&resolved, bytes, &resolved_meta)
        }
        Ok(meta) if meta.is_file() => replace_regular(path, bytes, &meta),
        Ok(_) => Err(refusal(UserFileRefusal::NotRegularFile)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let mode = match new_file {
                NewFileMode::UmaskDefault => 0o666,
                NewFileMode::Private => 0o600,
            };
            let dir = parent_dir(path);
            let tmp = stage(dir, bytes, mode)?;
            super::persist_temp_file(tmp, path)?;
            // Best-effort: the content is already published, only crash-durability of the rename is at stake.
            let _ = fsync_dir(dir);
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Replace an existing regular file, carrying its mode and owner onto the new content.
fn replace_regular(path: &Path, bytes: &[u8], existing: &std::fs::Metadata) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // A rename gives the path a new inode, so the file's other names would keep the old content.
        if existing.nlink() > 1 {
            return overwrite_in_place(path, bytes, existing);
        }
    }
    let dir = parent_dir(path);
    let tmp = stage_replacement(dir, bytes)?;
    #[cfg(unix)]
    if !carry_identity(&tmp, existing)? {
        drop(tmp);
        // A rename would change the file's owner or group, so the bytes go through the existing inode instead.
        return overwrite_in_place(path, bytes, existing);
    }
    #[cfg(not(unix))]
    carry_identity(&tmp, existing)?;
    // ponytail: the rename drops POSIX ACLs and xattrs; write in place when the file carries one.
    super::persist_temp_file(tmp, path)?;
    // Best-effort: the content is already published, only crash-durability of the rename is at stake.
    let _ = fsync_dir(dir);
    Ok(())
}

/// Staged content of a replacement, still unpublished and private.
fn stage_replacement(dir: &Path, bytes: &[u8]) -> io::Result<tempfile::NamedTempFile> {
    // Private until the mode is carried: the new bytes must not be readable more widely than the file they replace.
    stage(dir, bytes, 0o600)
}

/// Write `bytes` to a synced temp file in `dir`, created with `mode` (clipped by the umask).
fn stage(dir: &Path, bytes: &[u8], mode: u32) -> io::Result<tempfile::NamedTempFile> {
    use std::io::Write as _;

    #[cfg(unix)]
    let mut tmp = {
        use std::os::unix::fs::PermissionsExt as _;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(mode))
            .tempfile_in(dir)?
    };
    #[cfg(not(unix))]
    let mut tmp = {
        let _ = mode;
        tempfile::NamedTempFile::new_in(dir)?
    };
    tmp.write_all(bytes)?;
    tmp.as_file().sync_data()?;
    Ok(tmp)
}

/// Give the staged file the existing file's owner, group and mode; `false` when owner or group did not follow.
#[cfg(unix)]
fn carry_identity(tmp: &tempfile::NamedTempFile, existing: &std::fs::Metadata) -> io::Result<bool> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    // A non-owner cannot give the file away but may set a group it belongs to, so a refusal is not an error:
    // the metadata read below reports what was carried. The foreign-owner branch needs a second user, so it has
    // no unit test here; the acceptance suite covers it under `sudo`.
    if std::os::unix::fs::fchown(tmp.as_file(), Some(existing.uid()), Some(existing.gid())).is_err() {
        let _ = std::os::unix::fs::fchown(tmp.as_file(), None, Some(existing.gid()));
    }
    let staged = tmp.as_file().metadata()?;
    if staged.uid() != existing.uid() || staged.gid() != existing.gid() {
        return Ok(false);
    }
    // fchmod on the open file: a mode passed at creation would be clipped by the umask (0664 -> 0644).
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(existing.permissions().mode() & 0o777))?;
    Ok(true)
}

/// Write `bytes` through the existing inode, so owner, group, mode, ACLs and hard links all stay.
///
/// Not crash-atomic: a kill between the truncate and the last write leaves a torn file. A write that
/// fails puts the old bytes back and returns the write's error.
#[cfg(unix)]
fn overwrite_in_place(path: &Path, bytes: &[u8], existing: &std::fs::Metadata) -> io::Result<()> {
    use std::io::Write as _;

    overwrite_in_place_with(path, existing, |file| {
        file.write_all(bytes)?;
        file.sync_data()
    })
}

/// [`overwrite_in_place`] with the "write the new bytes" step supplied by the caller.
#[cfg(unix)]
fn overwrite_in_place_with(
    path: &Path,
    existing: &std::fs::Metadata,
    write_new: impl FnOnce(&mut std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    // No truncate and no create: the identity check below must run before any byte is destroyed.
    // O_NOFOLLOW: a link swapped in since the chain was resolved must not redirect the write to its target.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let opened = file.metadata()?;
    if opened.dev() != existing.dev() || opened.ino() != existing.ino() {
        return Err(refusal(UserFileRefusal::NotRegularFile));
    }
    // Read before the truncate: afterwards the old bytes exist nowhere, and a failed write would leave the user's file empty.
    let mut old_bytes = Vec::new();
    file.read_to_end(&mut old_bytes)?;
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    if let Err(write_error) = write_new(&mut file) {
        // The restore's own error is dropped: the caller needs the failure that started it, not a follow-on one.
        let _ = file
            .seek(SeekFrom::Start(0))
            .and_then(|_| file.set_len(0))
            .and_then(|()| file.write_all(&old_bytes))
            .and_then(|()| file.sync_data());
        return Err(write_error);
    }
    Ok(())
}

#[cfg(not(unix))]
fn carry_identity(_tmp: &tempfile::NamedTempFile, _existing: &std::fs::Metadata) -> io::Result<()> {
    Ok(())
}

fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Fsync `dir` so the rename itself survives a crash.
#[cfg(unix)]
fn fsync_dir(dir: &Path) -> io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// No-op off Unix: Windows cannot open a directory as a `File`.
#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusal_round_trips_through_io_error() {
        for r in [
            UserFileRefusal::DanglingSymlink,
            UserFileRefusal::NotRegularFile,
            UserFileRefusal::ForeignSymlink,
        ] {
            assert_eq!(user_file_refusal(&refusal(r)), Some(r));
            assert_eq!(refusal(r).kind(), io::ErrorKind::InvalidInput);
        }
        assert_eq!(user_file_refusal(&io::Error::other("plain")), None);
    }

    #[test]
    fn link_owner_table() {
        assert!(link_owner_allowed(1000, 1000));
        assert!(link_owner_allowed(0, 1000));
        assert!(!link_owner_allowed(2000, 1000));
    }

    #[test]
    fn resolve_returns_a_regular_file_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(resolve_user_file(&file).unwrap(), file);
    }

    #[test]
    fn resolve_returns_a_missing_path_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        assert_eq!(resolve_user_file(&missing).unwrap(), missing);
    }

    #[test]
    fn replace_creates_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("new");
        replace_user_file(&file, b"hello", NewFileMode::Private).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"hello");
    }

    #[test]
    fn replace_into_a_missing_parent_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = replace_user_file(&dir.path().join("nope/f"), b"x", NewFileMode::Private).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn replace_refuses_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = replace_user_file(dir.path(), b"x", NewFileMode::Private).unwrap_err();
        assert_eq!(user_file_refusal(&err), Some(UserFileRefusal::NotRegularFile));
    }

    #[cfg(windows)]
    #[test]
    fn replace_refuses_a_read_only_file_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        std::fs::write(&file, b"old").unwrap();
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&file, permissions).unwrap();

        let result = replace_user_file(&file, b"new", NewFileMode::Private);
        let content = std::fs::read(&file).unwrap();

        // Clear the attribute first so the tempdir can be removed whatever the assertions say.
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&file, permissions).unwrap();

        match result {
            Ok(()) => panic!(
                "Windows replaced a read-only file instead of refusing it; content now {:?}",
                String::from_utf8_lossy(&content)
            ),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::PermissionDenied),
        }
        assert_eq!(content, b"old");
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        use super::*;

        fn mode_of(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
        }

        fn set_mode(path: &Path, mode: u32) {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }

        fn is_root() -> bool {
            // SAFETY: geteuid has no preconditions and cannot fail.
            unsafe { libc::geteuid() == 0 }
        }

        #[test]
        fn replace_follows_a_single_link_and_leaves_it_in_place() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            let link = dir.path().join("link");
            std::fs::write(&target, b"old").unwrap();
            symlink(&target, &link).unwrap();

            replace_user_file(&link, b"new", NewFileMode::Private).unwrap();

            assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
            assert_eq!(std::fs::read(&target).unwrap(), b"new");
        }

        #[test]
        fn replace_follows_a_chain_of_two_links() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            let inner = dir.path().join("inner");
            let outer = dir.path().join("outer");
            std::fs::write(&target, b"old").unwrap();
            symlink(&target, &inner).unwrap();
            symlink(&inner, &outer).unwrap();

            replace_user_file(&outer, b"new", NewFileMode::Private).unwrap();

            assert_eq!(std::fs::read_link(&outer).unwrap(), inner);
            assert_eq!(std::fs::read_link(&inner).unwrap(), target);
            assert_eq!(std::fs::read(&target).unwrap(), b"new");
        }

        #[test]
        fn replace_follows_a_relative_link() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("sub")).unwrap();
            let target = dir.path().join("sub/target");
            let link = dir.path().join("link");
            std::fs::write(&target, b"old").unwrap();
            symlink("sub/target", &link).unwrap();

            replace_user_file(&link, b"new", NewFileMode::Private).unwrap();

            assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("sub/target"));
            assert_eq!(std::fs::read(&target).unwrap(), b"new");
        }

        #[test]
        fn replace_refuses_a_dangling_link() {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("link");
            symlink(dir.path().join("missing"), &link).unwrap();

            let err = replace_user_file(&link, b"x", NewFileMode::Private).unwrap_err();
            assert_eq!(user_file_refusal(&err), Some(UserFileRefusal::DanglingSymlink));
        }

        #[test]
        fn replace_refuses_a_looped_link() {
            let dir = tempfile::tempdir().unwrap();
            let a = dir.path().join("a");
            let b = dir.path().join("b");
            symlink(&b, &a).unwrap();
            symlink(&a, &b).unwrap();

            let err = replace_user_file(&a, b"x", NewFileMode::Private).unwrap_err();
            assert_eq!(user_file_refusal(&err), Some(UserFileRefusal::DanglingSymlink));
        }

        #[test]
        fn replace_refuses_a_link_to_a_directory() {
            let dir = tempfile::tempdir().unwrap();
            let sub = dir.path().join("sub");
            let link = dir.path().join("link");
            std::fs::create_dir(&sub).unwrap();
            symlink(&sub, &link).unwrap();

            let err = replace_user_file(&link, b"x", NewFileMode::Private).unwrap_err();
            assert_eq!(user_file_refusal(&err), Some(UserFileRefusal::NotRegularFile));
        }

        #[test]
        fn replace_carries_the_existing_mode() {
            for mode in [0o644, 0o664, 0o600, 0o755] {
                let dir = tempfile::tempdir().unwrap();
                let file = dir.path().join("f");
                std::fs::write(&file, b"old").unwrap();
                set_mode(&file, mode);

                replace_user_file(&file, b"new", NewFileMode::Private).unwrap();

                assert_eq!(mode_of(&file), mode, "mode {mode:o}");
                assert_eq!(std::fs::read(&file).unwrap(), b"new");
            }
        }

        #[test]
        fn replace_drops_setuid_from_the_existing_mode() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old").unwrap();
            set_mode(&file, 0o4755);

            replace_user_file(&file, b"new", NewFileMode::Private).unwrap();

            assert_eq!(mode_of(&file), 0o755);
        }

        #[test]
        fn replace_creates_a_private_file() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            replace_user_file(&file, b"x", NewFileMode::Private).unwrap();
            assert_eq!(mode_of(&file), 0o600);
        }

        #[test]
        fn replace_creates_an_umask_default_file() {
            let dir = tempfile::tempdir().unwrap();
            let probe = dir.path().join("probe");
            std::fs::File::create(&probe).unwrap();
            let file = dir.path().join("f");

            replace_user_file(&file, b"x", NewFileMode::UmaskDefault).unwrap();

            assert_eq!(mode_of(&file), mode_of(&probe));
        }

        #[test]
        fn resolve_returns_the_canonical_target_of_a_link() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            let link = dir.path().join("link");
            std::fs::write(&target, b"x").unwrap();
            symlink(&target, &link).unwrap();

            assert_eq!(resolve_user_file(&link).unwrap(), dunce::canonicalize(&target).unwrap());
        }

        #[test]
        fn a_write_into_a_read_only_directory_fails_and_keeps_the_old_bytes() {
            if is_root() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old").unwrap();
            set_mode(dir.path(), 0o555);

            let result = replace_user_file(&file, b"new", NewFileMode::Private);
            set_mode(dir.path(), 0o755);

            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
            assert_eq!(std::fs::read(&file).unwrap(), b"old");
        }

        #[test]
        fn an_unreadable_directory_does_not_fail_a_published_write() {
            if is_root() {
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old").unwrap();
            // Write+search without read: the directory fsync cannot open it.
            set_mode(dir.path(), 0o300);

            let result = replace_user_file(&file, b"new", NewFileMode::Private);
            set_mode(dir.path(), 0o755);

            result.unwrap();
            assert_eq!(std::fs::read(&file).unwrap(), b"new");
        }

        #[test]
        fn a_chain_longer_than_the_hop_limit_is_dangling() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            std::fs::write(&target, b"x").unwrap();
            let mut previous = target;
            for hop in 0..=MAX_LINK_HOPS {
                let link = dir.path().join(format!("link{hop}"));
                symlink(&previous, &link).unwrap();
                previous = link;
            }

            let err = replace_user_file(&previous, b"y", NewFileMode::Private).unwrap_err();
            assert_eq!(user_file_refusal(&err), Some(UserFileRefusal::DanglingSymlink));
        }

        #[test]
        fn the_staged_temp_file_of_an_existing_0644_file_is_private_until_the_mode_is_carried() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old").unwrap();
            set_mode(&file, 0o644);
            let existing = std::fs::metadata(&file).unwrap();

            let tmp = stage_replacement(dir.path(), b"new").unwrap();
            assert_eq!(mode_of(tmp.path()), 0o600);

            carry_identity(&tmp, &existing).unwrap();
            assert_eq!(mode_of(tmp.path()), 0o644);
        }

        #[test]
        fn replace_keeps_the_owner() {
            use std::os::unix::fs::MetadataExt as _;

            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old").unwrap();
            let before = std::fs::metadata(&file).unwrap();

            replace_user_file(&file, b"new", NewFileMode::Private).unwrap();

            let after = std::fs::metadata(&file).unwrap();
            assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
            assert_eq!(std::fs::read(&file).unwrap(), b"new");
        }

        #[test]
        fn replace_of_an_own_file_rotates_the_inode_and_keeps_the_mode() {
            use std::os::unix::fs::MetadataExt as _;

            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old").unwrap();
            set_mode(&file, 0o640);
            let before = std::fs::metadata(&file).unwrap().ino();

            replace_user_file(&file, b"new", NewFileMode::Private).unwrap();

            assert_ne!(std::fs::metadata(&file).unwrap().ino(), before);
            assert_eq!(mode_of(&file), 0o640);
        }

        #[test]
        fn a_hard_linked_file_is_written_in_place_so_every_name_sees_the_bytes() {
            use std::os::unix::fs::MetadataExt as _;

            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            let other_name = dir.path().join("other");
            std::fs::write(&file, b"old").unwrap();
            std::fs::hard_link(&file, &other_name).unwrap();
            let before = std::fs::metadata(&file).unwrap().ino();

            replace_user_file(&file, b"new", NewFileMode::Private).unwrap();

            assert_eq!(std::fs::metadata(&file).unwrap().ino(), before);
            assert_eq!(std::fs::read(&file).unwrap(), b"new");
            assert_eq!(std::fs::read(&other_name).unwrap(), b"new");
        }

        #[test]
        fn a_dot_dot_target_under_a_symlinked_directory_resolves_as_the_kernel_does() {
            let dir = tempfile::tempdir().unwrap();
            let root = dunce::canonicalize(dir.path()).unwrap();
            std::fs::create_dir_all(root.join("real/sub")).unwrap();
            std::fs::write(root.join("real/file"), b"x").unwrap();
            symlink("real/sub", root.join("alias")).unwrap();
            symlink("../file", root.join("real/sub/link")).unwrap();

            let resolved = resolve_user_file(&root.join("alias/link")).unwrap();

            assert_eq!(resolved, root.join("real/file"));
        }

        #[test]
        fn overwrite_in_place_keeps_the_inode_the_mode_and_hard_links() {
            use std::os::unix::fs::MetadataExt as _;

            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            let linked = dir.path().join("linked");
            std::fs::write(&file, b"old content").unwrap();
            set_mode(&file, 0o640);
            std::fs::hard_link(&file, &linked).unwrap();
            let existing = std::fs::metadata(&file).unwrap();

            overwrite_in_place(&file, b"new", &existing).unwrap();

            let after = std::fs::metadata(&file).unwrap();
            assert_eq!(after.ino(), existing.ino());
            assert_eq!(mode_of(&file), 0o640);
            assert_eq!(std::fs::read(&file).unwrap(), b"new");
            assert_eq!(std::fs::read(&linked).unwrap(), b"new");
        }

        #[test]
        fn overwrite_in_place_grows_a_file() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old").unwrap();
            let existing = std::fs::metadata(&file).unwrap();

            overwrite_in_place(&file, b"much longer content", &existing).unwrap();

            assert_eq!(std::fs::read(&file).unwrap(), b"much longer content");
        }

        #[test]
        fn a_failed_in_place_write_puts_the_old_bytes_back() {
            use std::io::Write as _;
            use std::os::unix::fs::MetadataExt as _;

            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("f");
            std::fs::write(&file, b"old content").unwrap();
            let existing = std::fs::metadata(&file).unwrap();

            let err = overwrite_in_place_with(&file, &existing, |handle| {
                handle.write_all(b"torn")?;
                Err(io::Error::from(io::ErrorKind::StorageFull))
            })
            .unwrap_err();

            assert_eq!(err.kind(), io::ErrorKind::StorageFull);
            assert_eq!(std::fs::read(&file).unwrap(), b"old content");
            assert_eq!(std::fs::metadata(&file).unwrap().ino(), existing.ino());
        }

        #[test]
        fn overwrite_in_place_refuses_a_different_file_and_leaves_its_bytes() {
            let dir = tempfile::tempdir().unwrap();
            let swapped_in = dir.path().join("a");
            let inspected = dir.path().join("b");
            std::fs::write(&swapped_in, b"keep me").unwrap();
            std::fs::write(&inspected, b"other").unwrap();
            let existing = std::fs::metadata(&inspected).unwrap();

            let err = overwrite_in_place(&swapped_in, b"new", &existing).unwrap_err();

            assert_eq!(user_file_refusal(&err), Some(UserFileRefusal::NotRegularFile));
            assert_eq!(std::fs::read(&swapped_in).unwrap(), b"keep me");
        }

        #[test]
        fn overwrite_in_place_does_not_follow_a_symlink() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("target");
            let link = dir.path().join("link");
            std::fs::write(&target, b"keep me").unwrap();
            symlink(&target, &link).unwrap();
            let existing = std::fs::metadata(&target).unwrap();

            assert!(overwrite_in_place(&link, b"new", &existing).is_err());

            assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
        }
    }
}
