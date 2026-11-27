// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publication primitives for directories built complete in a staging directory: one rename or one link switch,
//! never an edit of what is already published.

use std::path::{Path, PathBuf};

/// What [`publish_dir`] found at the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    /// The staged directory is now the target.
    New,
    /// Another directory already holds the target; the staged one is untouched.
    AlreadyThere,
}

/// What [`retire_dir`] did with a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retired {
    /// The directory is gone.
    Removed,
    /// The directory was not removed and is whole; a later call may retire it.
    ///
    /// Also the answer for a path that exists but is not a real directory: it is never acted on.
    /// One exception to "whole": the EXDEV fallback removes in place, so a busy entry can leave
    /// the directory partial under its own name. A completeness check treats it as retire-and-rebuild.
    LeftForLater,
}

/// `rename(2)` of `staged` onto `target`.
///
/// Unix replaces an absent or empty target and maps `ENOTEMPTY` and `EEXIST` to [`Published::AlreadyThere`].
/// Windows publishes onto an absent target only; an existing one, empty or not, is `AlreadyThere`.
/// `staged` stays for the caller to discard.
///
/// # Errors
///
/// The rename fails for a reason other than an occupied target.
pub async fn publish_dir(staged: &Path, target: &Path) -> std::io::Result<Published> {
    #[cfg(unix)]
    {
        match super::rename_with_windows_retry(staged, target).await {
            Ok(()) => Ok(Published::New),
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) => {
                Ok(Published::AlreadyThere)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(windows)]
    {
        // ERROR_FILE_EXISTS / ERROR_ALREADY_EXISTS: without MOVEFILE_REPLACE_EXISTING an occupied target is refused.
        const OCCUPIED: [i32; 2] = [80, 183];
        let (staged, target) = (staged.to_path_buf(), target.to_path_buf());
        let result = super::retry_windows_transient(|| {
            let (staged, target) = (staged.clone(), target.clone());
            async move {
                tokio::task::spawn_blocking(move || move_without_replace(&staged, &target))
                    .await
                    .map_err(std::io::Error::other)?
            }
        })
        .await;
        match result {
            Ok(()) => Ok(Published::New),
            Err(error) if error.raw_os_error().is_some_and(|code| OCCUPIED.contains(&code)) => {
                Ok(Published::AlreadyThere)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(windows)]
fn move_without_replace(staged: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (source, destination) = (wide(staged), wide(target));
    // SAFETY: both buffers are NUL-terminated and outlive the call; flags 0 never replaces an existing target.
    let moved = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Name prefix of the sibling link a retarget stages before publishing it.
const RETARGET_PREFIX: &str = ".tmp-retarget-";

/// Points `link` at `target`.
///
/// Unix stages a sibling link by exclusive create and renames it over `link`. Windows sets the junction's
/// mount-point data in place; an absent `link` is staged as a sibling junction and published by one
/// no-replace rename, so no empty real directory is ever visible under `link`.
///
/// Blocking I/O: an async caller wraps it in `spawn_blocking`. The caller holds the lock that excludes other
/// retargeters of `link`'s parent (the toolchain-render lock): staging first sweeps stale `.tmp-retarget-*`
/// links a crashed retarget left, which would also remove a live retargeter's staged link.
///
/// # Errors
///
/// `link` exists and is neither a symlink nor a junction, or the switch fails.
pub fn retarget_link(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        retarget_symlink(target, link)
    }
    #[cfg(windows)]
    {
        retarget_junction(target, link)
    }
}

/// Removes sibling `.tmp-retarget-*` entries that are links; lstat only, never followed, no other name touched.
fn sweep_retarget_debris(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(RETARGET_PREFIX) {
            continue;
        }
        let path = entry.path();
        let removal = match std::fs::symlink_metadata(&path) {
            #[cfg(unix)]
            Ok(metadata) if metadata.file_type().is_symlink() => std::fs::remove_file(&path),
            #[cfg(windows)]
            Ok(metadata) if is_reparse_point(&metadata) => std::fs::remove_dir(&path),
            _ => continue,
        };
        if let Err(error) = removal {
            tracing::debug!(path = %path.display(), %error, "stale retarget link not removed; left for the next call");
        }
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(unix)]
fn retarget_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(link) {
        Ok(metadata) if !metadata.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a link", link.display()),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = link.parent().unwrap_or_else(|| Path::new("."));
    sweep_retarget_debris(parent);
    // A name taken by a concurrent retargeter is retried under a fresh one, never overwritten.
    let mut attempts = 0;
    let staged = loop {
        let staged = parent.join(format!("{RETARGET_PREFIX}{}", random_suffix()));
        match std::os::unix::fs::symlink(target, &staged) {
            Ok(()) => break staged,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempts < 8 => attempts += 1,
            Err(error) => return Err(error),
        }
    };
    if let Err(error) = rename_staged_link(&staged, link) {
        // Best effort: the rename error is the one that matters.
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
#[expect(
    clippy::disallowed_methods,
    reason = "renames ocx's own freshly staged symlink over its link path"
)]
fn rename_staged_link(staged: &Path, link: &Path) -> std::io::Result<()> {
    std::fs::rename(staged, link)
}

#[cfg(any(windows, test))]
const REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
#[cfg(windows)]
const REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
#[cfg(any(windows, test))]
const MAX_REPARSE_BUFFER: usize = 16 * 1024;
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Sets the mount-point data of an existing junction in place, so `link` is never absent to a concurrent lookup.
///
/// An absent `link` is created by [`publish_new_junction`]. A directory symlink at `link` is the one exception to
/// "never absent": it is removed and recreated as a junction (the heal). A real directory or a file is refused.
// shortcut: only drive-letter absolute targets (`\??\C:\...`) are written, a UNC target (`\??\UNC\...`) is not; upgrade when a home may live on a network share.
#[cfg(windows)]
fn retarget_junction(target: &Path, link: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;
    const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    const OPEN_REPARSE_POINT_BACKUP: u32 = 0x0020_0000 | 0x0200_0000;

    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or_else(|| Path::new(".")).join(target)
    };
    let target = std::path::absolute(&target)?;
    let target = dunce::simplified(&target);

    // A concurrent creator or the symlink heal sends the loop round once more, onto the in-place path.
    for _ in 0..3 {
        match std::fs::symlink_metadata(link) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if publish_new_junction(target, link)? {
                    return Ok(());
                }
                continue;
            }
            Err(error) => return Err(error),
            Ok(metadata) if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} exists and is not a link", link.display()),
                ));
            }
            Ok(_) => {}
        }
        // Full sharing: a concurrent lookup through `link` must never see a sharing violation.
        let handle = std::fs::OpenOptions::new()
            .access_mode(GENERIC_READ_WRITE)
            .share_mode(SHARE_ALL)
            .custom_flags(OPEN_REPARSE_POINT_BACKUP)
            .open(link)?;
        match reparse_tag(&handle)? {
            REPARSE_TAG_MOUNT_POINT => return set_mount_point(&handle, target),
            REPARSE_TAG_SYMLINK => {
                // The one heal gap: `link` is absent until the next iteration publishes the junction.
                drop(handle);
                std::fs::remove_dir(link)?;
            }
            tag => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{} is a reparse point of unsupported tag {tag:#x}", link.display()),
                ));
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("{} is being created concurrently", link.display()),
    ))
}

/// Creates a junction to `target` at a sibling staging name, then publishes it at the absent `link` with one
/// `MoveFileExW` without replace: that rename is the commit point, so a crash leaves only a staged junction.
///
/// `Ok(false)` means a concurrent creator won the name; the staged junction is removed (`remove_dir` on the
/// junction itself, never recursive) and the caller retargets `link` in place.
#[cfg(windows)]
fn publish_new_junction(target: &Path, link: &Path) -> std::io::Result<bool> {
    // ERROR_FILE_EXISTS / ERROR_ALREADY_EXISTS: the same occupied-target pair `publish_dir` maps.
    const OCCUPIED: [i32; 2] = [80, 183];
    let parent = link.parent().unwrap_or_else(|| Path::new("."));
    sweep_retarget_debris(parent);
    let mut attempts = 0;
    let staged = loop {
        let staged = parent.join(format!("{RETARGET_PREFIX}{}", random_suffix()));
        match junction::create(target, &staged) {
            Ok(()) => break staged,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && attempts < 8 => attempts += 1,
            Err(error) => return Err(error),
        }
    };
    match move_without_replace(&staged, link) {
        Ok(()) => Ok(true),
        Err(error) => {
            // Best effort: the publish error is the one that matters.
            let _ = std::fs::remove_dir(&staged);
            if error.raw_os_error().is_some_and(|code| OCCUPIED.contains(&code)) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(windows)]
fn device_control(handle: &std::fs::File, code: u32, input: &[u8], output: &mut [u8]) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::IO::DeviceIoControl;

    let mut returned = 0u32;
    // SAFETY: `handle` is open for the call; both buffers are valid for the lengths passed and `returned` outlives it.
    let ok = unsafe {
        DeviceIoControl(
            handle.as_raw_handle(),
            code,
            input.as_ptr().cast(),
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The reparse tag is the first `u32` of the `REPARSE_DATA_BUFFER`.
#[cfg(windows)]
fn reparse_tag(handle: &std::fs::File) -> std::io::Result<u32> {
    use windows_sys::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

    let mut buffer = vec![0u8; MAX_REPARSE_BUFFER];
    device_control(handle, FSCTL_GET_REPARSE_POINT, &[], &mut buffer)?;
    Ok(u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]))
}

/// Replaces the mount-point data on an open junction handle.
#[cfg(windows)]
fn set_mount_point(handle: &std::fs::File, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Ioctl::FSCTL_SET_REPARSE_POINT;

    let target: Vec<u16> = target.as_os_str().encode_wide().collect();
    let buffer = mount_point_buffer(&target)?;
    device_control(handle, FSCTL_SET_REPARSE_POINT, &buffer, &mut [])
}

/// The `REPARSE_DATA_BUFFER` of a mount point to `target` (UTF-16 units).
///
/// Layout, all little-endian: `ReparseTag u32`, `ReparseDataLength u16` (bytes after the 8-byte header), `Reserved u16`,
/// then `SubstituteNameOffset/Length`, `PrintNameOffset/Length` (4 x `u16`, bytes) and the UTF-16 path buffer
/// `\??\<target>\0<target>\0`. A verbatim `\\?\` prefix is dropped first, because the substitute name already
/// carries the NT `\??\` prefix: `\\?\C:\x` becomes `\??\C:\x`, `\\?\UNC\s\r` becomes `\??\UNC\s\r`.
#[cfg(any(windows, test))]
fn mount_point_buffer(target: &[u16]) -> std::io::Result<Vec<u8>> {
    let units = |text: &str| text.encode_utf16().collect::<Vec<u16>>();
    let (verbatim, verbatim_unc) = (units(r"\\?\"), units(r"\\?\UNC\"));
    let (substitute, print): (Vec<u16>, Vec<u16>) = if let Some(rest) = target.strip_prefix(verbatim_unc.as_slice()) {
        (
            units(r"\??\UNC\").into_iter().chain(rest.iter().copied()).collect(),
            units(r"\\").into_iter().chain(rest.iter().copied()).collect(),
        )
    } else {
        let plain = target.strip_prefix(verbatim.as_slice()).unwrap_or(target);
        (
            units(r"\??\").into_iter().chain(plain.iter().copied()).collect(),
            plain.to_vec(),
        )
    };
    let (substitute_bytes, print_bytes) = (substitute.len() * 2, print.len() * 2);
    let data_length = 8 + substitute_bytes + 2 + print_bytes + 2;
    if 8 + data_length > MAX_REPARSE_BUFFER {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "junction target is too long",
        ));
    }
    let mut buffer = Vec::with_capacity(8 + data_length);
    buffer.extend_from_slice(&REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer.extend_from_slice(&(data_length as u16).to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    for (offset, length) in [(0, substitute_bytes), (substitute_bytes + 2, print_bytes)] {
        buffer.extend_from_slice(&(offset as u16).to_le_bytes());
        buffer.extend_from_slice(&(length as u16).to_le_bytes());
    }
    for unit in substitute.iter().chain(&[0]).chain(&print).chain(&[0]) {
        buffer.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(buffer)
}

/// Acts only on a real directory, never a symlink or reparse point: renames it to a sibling `.gc-<random>`,
/// then removes that. A refused rename leaves `dir` whole. Also sweeps sibling `.gc-*` and `.staging-*` debris.
///
/// The caller holds the lock that excludes stagers in `dir`'s parent: a `.staging-*` sibling of a live stager is
/// swept as debris, and a symlink debris entry is removed as a link, never followed. An absent `dir` is `Removed`.
/// The EXDEV fallback removes in place, so its `LeftForLater` can leave `dir` partial (see [`Retired::LeftForLater`]).
///
/// # Errors
///
/// The directory cannot be inspected or removed for a reason other than being busy or non-empty.
pub async fn retire_dir(dir: &Path) -> std::io::Result<Retired> {
    let outcome = match tokio::fs::symlink_metadata(dir).await {
        // Already absent: the name is gone, which is what the caller asked for.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Retired::Removed,
        Err(error) => return Err(error),
        Ok(metadata) if !is_real_directory(&metadata) => Retired::LeftForLater,
        Ok(_) => retire_real_directory(dir).await?,
    };
    if let Some(parent) = dir.parent() {
        sweep_debris(parent).await;
    }
    Ok(outcome)
}

fn is_real_directory(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_dir()
    }
}

async fn retire_real_directory(dir: &Path) -> std::io::Result<Retired> {
    let Some(parent) = dir.parent() else {
        return Ok(Retired::LeftForLater);
    };
    let graveyard = loop {
        let candidate = parent.join(format!(".gc-{}", random_suffix()));
        match tokio::fs::symlink_metadata(&candidate).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break candidate,
            Err(error) => return Err(error),
            Ok(_) => {}
        }
    };
    match super::rename_with_windows_retry(dir, &graveyard).await {
        Ok(()) => {}
        // Another process retired `dir` after the caller's lstat: the name is gone.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Retired::Removed),
        // overlayfs refuses directory renames across layers: remove in place.
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            return match tokio::fs::remove_dir_all(dir).await {
                Ok(()) => Ok(Retired::Removed),
                Err(error) if is_busy_or_not_empty(&error) => Ok(Retired::LeftForLater),
                Err(error) => Err(error),
            };
        }
        Err(error) if is_refused(&error) => return Ok(Retired::LeftForLater),
        Err(error) => return Err(error),
    }
    // The name is gone after the rename; a failed removal is debris for the next sweep.
    if let Err(error) = tokio::fs::remove_dir_all(&graveyard).await {
        tracing::debug!(path = %graveyard.display(), %error, "retired directory not fully removed; left for the next sweep");
    }
    Ok(Retired::Removed)
}

fn is_refused(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied || is_busy_or_not_empty(error)
}

fn is_busy_or_not_empty(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(libc::EBUSY | libc::ENOTEMPTY | libc::EPERM | libc::EACCES)
        )
    }
    #[cfg(windows)]
    {
        // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_DIR_NOT_EMPTY
        matches!(error.raw_os_error(), Some(5 | 32 | 145))
    }
}

fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u64(SEQUENCE.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("{:016x}", hasher.finish())
}

/// Removes `.gc-*` and `.staging-*` siblings; failures wait for the next call.
async fn sweep_debris(parent: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(".gc-") || name.starts_with(".staging-")) {
            continue;
        }
        let path: PathBuf = entry.path();
        let removal = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if is_real_directory(&metadata) => tokio::fs::remove_dir_all(&path).await,
            // A symlink, reparse point or file goes as itself, never followed.
            Ok(_) => remove_link_or_file(&path).await,
            Err(error) => Err(error),
        };
        if let Err(error) = removal {
            tracing::debug!(path = %path.display(), %error, "debris not removed; left for the next sweep");
        }
    }
}

async fn remove_link_or_file(path: &Path) -> std::io::Result<()> {
    let removed = tokio::fs::remove_file(path).await;
    // A Windows directory link or junction needs `remove_dir`, which never follows it.
    #[cfg(windows)]
    if removed.is_err() {
        return tokio::fs::remove_dir(path).await;
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[cfg(unix)]
    fn staged_with_file(root: &Path) -> std::path::PathBuf {
        let staged = root.join("staged");
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("NAME"), b"x").unwrap();
        staged
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contract_publish_onto_absent_target_is_new() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = staged_with_file(tmp.path());
        let target = tmp.path().join("target");

        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::New);
        assert!(target.join("NAME").is_file());
        assert!(!staged.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contract_publish_onto_empty_dir_is_new() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = staged_with_file(tmp.path());
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();

        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::New);
        assert!(target.join("NAME").is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contract_publish_onto_non_empty_dir_is_already_there_and_keeps_staged() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = staged_with_file(tmp.path());
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("theirs"), b"y").unwrap();

        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::AlreadyThere);
        assert!(staged.join("NAME").is_file(), "staged stays for the caller");
        assert!(target.join("theirs").is_file());
        assert!(!target.join("NAME").exists());
    }

    #[cfg(unix)]
    #[test]
    fn contract_retarget_creates_an_absent_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        let link = tmp.path().join("active");

        retarget_link(&dir, &link).unwrap();

        assert_eq!(std::fs::read_link(&link).unwrap(), dir);
    }

    #[cfg(unix)]
    #[test]
    fn contract_retarget_replaces_an_existing_link() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let link = tmp.path().join("active");
        retarget_link(&a, &link).unwrap();

        retarget_link(&b, &link).unwrap();

        assert_eq!(std::fs::read_link(&link).unwrap(), b);
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            3,
            "a, b, active: no staged link left behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn contract_retarget_refuses_a_real_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a");
        let link = tmp.path().join("active");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&link).unwrap();

        assert!(retarget_link(&target, &link).is_err());
        assert!(link.is_dir() && !link.is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn contract_retarget_refuses_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a");
        let link = tmp.path().join("active");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(&link, b"x").unwrap();

        assert!(retarget_link(&target, &link).is_err());
        assert!(link.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn retarget_leaves_no_staged_link_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let link = tmp.path().join("active");

        for round in 0..50 {
            retarget_link(if round % 2 == 0 { &a } else { &b }, &link).unwrap();
        }

        let leftovers = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tmp-retarget-")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[cfg(unix)]
    #[test]
    fn retarget_sweeps_a_stale_staged_link_and_leaves_bystanders() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        let stale = tmp.path().join(".tmp-retarget-x");
        std::os::unix::fs::symlink("nowhere", &stale).unwrap();
        let bystander = tmp.path().join(".tmp-other");
        std::os::unix::fs::symlink("nowhere", &bystander).unwrap();
        let link = tmp.path().join("active");

        retarget_link(&dir, &link).unwrap();

        assert!(std::fs::symlink_metadata(&stale).is_err(), "stale staged link swept");
        assert!(std::fs::symlink_metadata(&bystander).is_ok(), "other names untouched");
        assert_eq!(std::fs::read_link(&link).unwrap(), dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contract_retire_removes_a_real_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = staged_with_file(tmp.path());

        assert_eq!(retire_dir(&dir).await.unwrap(), Retired::Removed);
        assert!(!dir.exists());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "no .gc- debris");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contract_retire_leaves_a_symlink_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let real = staged_with_file(tmp.path());
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(retire_dir(&link).await.unwrap(), Retired::LeftForLater);
        assert!(link.is_symlink());
        assert!(real.join("NAME").is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn contract_retire_sweeps_sibling_debris() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = staged_with_file(tmp.path());
        let gc = tmp.path().join(".gc-0123456789abcdef");
        let staging = tmp.path().join(".staging-0123456789abcdef");
        let bystander = tmp.path().join("keep");
        for debris in [&gc, &staging, &bystander] {
            std::fs::create_dir(debris).unwrap();
            std::fs::write(debris.join("f"), b"z").unwrap();
        }

        retire_dir(&dir).await.unwrap();

        assert!(!gc.exists(), ".gc-* swept");
        assert!(!staging.exists(), ".staging-* swept");
        assert!(bystander.join("f").is_file(), "other siblings untouched");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn publish_two_concurrent_stagers_converge_on_one_whole_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let mut staged = Vec::new();
        for name in ["a", "b"] {
            let dir = tmp.path().join(format!("staged-{name}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("NAME"), name).unwrap();
            std::fs::write(dir.join("EXTRA"), name).unwrap();
            staged.push(dir);
        }

        let (first, second) = tokio::join!(publish_dir(&staged[0], &target), publish_dir(&staged[1], &target));
        let outcomes = [first.unwrap(), second.unwrap()];

        assert_eq!(outcomes.iter().filter(|o| **o == Published::New).count(), 1);
        assert_eq!(outcomes.iter().filter(|o| **o == Published::AlreadyThere).count(), 1);
        let winner = std::fs::read_to_string(target.join("NAME")).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("EXTRA")).unwrap(),
            winner,
            "one staged set, whole"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn publish_failure_leaves_target_absent_and_retry_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let missing = tmp.path().join("missing");

        assert!(publish_dir(&missing, &target).await.is_err());
        assert!(!target.exists(), "no partial published directory");

        let staged = staged_with_file(tmp.path());
        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::New);
        assert!(target.join("NAME").is_file());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retire_leaves_dir_whole_when_rename_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: euid 0 ignores directory permissions");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let dir = staged_with_file(&parent);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

        let outcome = retire_dir(&dir).await;

        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(outcome.unwrap(), Retired::LeftForLater);
        assert!(dir.join("NAME").is_file(), "dir stays whole");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retire_sweep_removes_a_gc_symlink_as_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = staged_with_file(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("f"), b"z").unwrap();
        let link = tmp.path().join(".gc-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        retire_dir(&dir).await.unwrap();

        assert!(std::fs::symlink_metadata(&link).is_err(), "link removed");
        assert!(outside.join("f").is_file(), "link target untouched");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retire_absent_dir_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(retire_dir(&tmp.path().join("gone")).await.unwrap(), Retired::Removed);
    }

    #[tokio::test]
    async fn retire_real_directory_gone_after_the_lstat_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        // The state a second retirer leaves between the caller's lstat and the rename.
        assert_eq!(
            retire_real_directory(&tmp.path().join("gone")).await.unwrap(),
            Retired::Removed
        );
    }

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// The substitute and print names of a mount-point buffer, as text.
    fn mount_point_names(target: &str) -> (String, String) {
        let buffer = mount_point_buffer(&utf16(target)).unwrap();
        let field = |at: usize| usize::from(u16::from_le_bytes([buffer[at], buffer[at + 1]]));
        let (sub_offset, sub_length, print_offset, print_length) = (field(8), field(10), field(12), field(14));
        let path_buffer = &buffer[16..];
        let text = |offset: usize, length: usize| {
            let units: Vec<u16> = path_buffer[offset..offset + length]
                .chunks(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16(&units).unwrap()
        };
        (text(sub_offset, sub_length), text(print_offset, print_length))
    }

    #[test]
    fn mount_point_buffer_plain_target_gets_the_nt_prefix() {
        let (substitute, print) = mount_point_names(r"C:\Users\x");
        assert_eq!(substitute, r"\??\C:\Users\x");
        assert_eq!(print, r"C:\Users\x");
    }

    #[test]
    fn mount_point_buffer_drops_a_verbatim_prefix() {
        let long = format!(r"\\?\C:\{}", "d".repeat(240));
        let (substitute, print) = mount_point_names(&long);
        assert!(substitute.starts_with(r"\??\C:\"), "substitute: {substitute}");
        assert!(print.starts_with(r"C:\"), "print: {print}");
        assert!(!substitute.contains(r"\\?\"), "substitute: {substitute}");
    }

    #[test]
    fn mount_point_buffer_maps_a_verbatim_unc_target() {
        let (substitute, print) = mount_point_names(r"\\?\UNC\server\share\x");
        assert_eq!(substitute, r"\??\UNC\server\share\x");
        assert_eq!(print, r"\\server\share\x");
    }

    #[test]
    fn mount_point_buffer_refuses_a_target_past_the_reparse_limit() {
        assert!(mount_point_buffer(&utf16(&format!(r"C:\{}", "d".repeat(9000)))).is_err());
    }

    #[cfg(windows)]
    fn dir_with_marker(root: &Path, name: &str) -> std::path::PathBuf {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("marker"), name).unwrap();
        dir
    }

    #[cfg(windows)]
    struct LookupTally {
        reads: usize,
        failures: usize,
        saw_a: bool,
        saw_b: bool,
    }

    /// Reads through `link` until `stop`, counting every read, every failed read and which marker each returned.
    #[cfg(windows)]
    fn lookup_tally(link: &Path, stop: &AtomicBool) -> LookupTally {
        let marker = link.join("marker");
        let mut tally = LookupTally {
            reads: 0,
            failures: 0,
            saw_a: false,
            saw_b: false,
        };
        while !stop.load(Ordering::Relaxed) {
            match std::fs::read(&marker) {
                Ok(content) => {
                    tally.reads += 1;
                    tally.saw_a |= content == b"a";
                    tally.saw_b |= content == b"b";
                }
                Err(_) => tally.failures += 1,
            }
        }
        tally
    }

    /// A reader that stopped early would report zero failures too, so each phase must have read this many times.
    #[cfg(windows)]
    const READ_FLOOR: usize = 1000;

    #[cfg(windows)]
    #[test]
    #[ignore = "junction-proof CI job: run from the junction-proof workflow, elevated and unelevated"]
    fn junction_proof_retarget_under_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (dir_with_marker(tmp.path(), "a"), dir_with_marker(tmp.path(), "b"));
        let active = tmp.path().join("active");
        retarget_link(&a, &active).unwrap();

        // Phase 1: one retargeter against one lookup loop.
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| lookup_tally(&active, &stop));
            for round in 0..10_000 {
                retarget_link(if round % 2 == 0 { &b } else { &a }, &active).unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            let tally = reader.join().unwrap();
            assert_eq!(tally.failures, 0, "lookup failed during a retarget");
            assert!(tally.reads >= READ_FLOOR, "reader read only {} times", tally.reads);
            assert!(tally.saw_a && tally.saw_b, "reader never saw both targets");
        });

        // Phase 2: two retargeters at once against the lookup loop.
        let stop = AtomicBool::new(false);
        let retarget_errors = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let reader = scope.spawn(|| lookup_tally(&active, &stop));
            let writers: Vec<_> = (0..2)
                .map(|writer| {
                    let (a, b, active, errors) = (&a, &b, &active, &retarget_errors);
                    scope.spawn(move || {
                        for round in 0..5_000 {
                            let to = if (round + writer) % 2 == 0 { b } else { a };
                            if retarget_link(to, active).is_err() {
                                errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    })
                })
                .collect();
            for writer in writers {
                writer.join().unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            let tally = reader.join().unwrap();
            assert_eq!(tally.failures, 0, "lookup failed during concurrent retargets");
            assert!(tally.reads >= READ_FLOOR, "reader read only {} times", tally.reads);
        });
        assert_eq!(
            retarget_errors.load(Ordering::Relaxed),
            0,
            "a concurrent retarget failed"
        );

        // Heal: a directory symlink at `active` becomes a junction at the requested target.
        std::fs::remove_dir(&active).unwrap();
        match std::os::windows::fs::symlink_dir(&a, &active) {
            Ok(()) => {
                retarget_link(&b, &active).unwrap();
                assert!(junction::exists(&active).unwrap(), "active is a junction again");
                assert_eq!(std::fs::read_to_string(active.join("marker")).unwrap(), "b");
            }
            Err(error) if error.raw_os_error() == Some(1314) => {
                eprintln!("symlink privilege missing (os error 1314): heal case not exercised");
            }
            Err(error) => panic!("symlink_dir failed: {error}"),
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "junction-proof CI job: run from the junction-proof workflow, elevated and unelevated"]
    #[expect(
        clippy::disallowed_types,
        reason = "runs a copy of a system exe to hold it open; no package launch is involved"
    )]
    fn junction_proof_retire_running_exe() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("launcher");
        std::fs::create_dir(&dir).unwrap();
        let exe = dir.join("ping.exe");
        std::fs::copy(r"C:\Windows\System32\ping.exe", &exe).unwrap();
        std::fs::write(dir.join("NAME.launch"), b"{}").unwrap();
        let mut child = std::process::Command::new(&exe)
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let outcome = runtime.block_on(retire_dir(&dir)).unwrap();

        match outcome {
            Retired::Removed => assert!(!dir.exists(), "Removed means the directory is gone"),
            Retired::LeftForLater => {
                assert!(
                    exe.is_file() && dir.join("NAME.launch").is_file(),
                    "left whole, never partial"
                );
            }
        }
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(runtime.block_on(retire_dir(&dir)).unwrap(), Retired::Removed);
        assert!(!dir.exists());
        let graveyards = || {
            std::fs::read_dir(tmp.path())
                .unwrap()
                .filter(|entry| {
                    entry
                        .as_ref()
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".gc-")
                })
                .count()
        };
        // The exe is released now, so the sweep inside the second retire has cleared any `.gc-*` the first left.
        assert_eq!(
            graveyards(),
            0,
            "no .gc-* debris after the child exited and retire ran again"
        );
    }
}
