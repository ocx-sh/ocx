// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod bounded_read;
mod dir_walker;
mod drop_file;
mod empty_or_absent;
mod file_lock;
mod locked_file;
pub mod path;
mod same_dir;
mod same_filesystem;
mod scoped_lock;
pub mod symlink;
mod symlink_walk;

pub use bounded_read::{BoundedReadError, read_bounded, read_bounded_async};
pub use dir_walker::{DirWalker, WalkDecision};
pub use drop_file::DropFile;
pub use empty_or_absent::{EmptyOrAbsentError, ensure_empty_or_absent};
pub use file_lock::FileLock;
pub use locked_file::{LockedFile, LockedJsonFile, LockedTomlFile};
pub use same_dir::same_dir;
pub use same_filesystem::{SameFilesystemError, same_filesystem};
pub use scoped_lock::lock_scoped;
pub use symlink_walk::{SymlinkWalkError, refuse_if_symlink_in_path};
// The blocking arm is used only by this crate's own archive extractors.
pub(crate) use symlink_walk::refuse_if_symlink_in_path_sync;

use crate::error::FileError;

/// Returns whether `path` exists, logging any I/O error at debug and answering `false`.
pub async fn path_exists_lossy(path: &std::path::Path) -> bool {
    match tokio::fs::try_exists(path).await {
        Ok(exists) => exists,
        Err(e) => {
            log::debug!("path_exists_lossy probe failed for {}: {}", path.display(), e);
            false
        }
    }
}

/// Moves directory `src` to `dst` by same-filesystem rename, creating `dst`'s parent and replacing an existing `dst`.
pub async fn move_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<(), FileError> {
    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| FileError::new(parent, e))?;
    }
    if dst.exists() {
        tokio::fs::remove_dir_all(dst)
            .await
            .map_err(|e| FileError::new(dst, e))?;
    }
    rename_with_windows_retry(src, dst)
        .await
        .map_err(|e| FileError::new(src, e))?;
    Ok(())
}

/// Backoff schedule for Windows transient sharing/access retries.
#[cfg(windows)]
const WINDOWS_TRANSIENT_BACKOFF: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(100),
    std::time::Duration::from_millis(400),
    std::time::Duration::from_millis(800),
];

/// Scales `backoff` by ±25% jitter so concurrent retriers do not re-collide in lockstep.
#[cfg(windows)]
fn jittered_backoff(backoff: std::time::Duration) -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter_scale = 0.75 + (f64::from(nanos % 1024) / 1023.0) * 0.5;
    std::time::Duration::from_secs_f64(backoff.as_secs_f64() * jitter_scale)
}

/// `ERROR_ACCESS_DENIED` (5) or `ERROR_SHARING_VIOLATION` (32): another handle, typically gone within milliseconds.
#[cfg(windows)]
fn is_transient_windows_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5) | Some(32))
}

/// Renames `src` to `dst`, retrying Windows transient errors (Defender opens freshly written files).
///
/// An already-present `dst` is not success; what it means is the caller's call.
pub async fn rename_with_windows_retry(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        retry_windows_transient(|| tokio::fs::rename(src, dst)).await
    }
    #[cfg(not(windows))]
    {
        tokio::fs::rename(src, dst).await
    }
}

/// Drives `attempt` through the transient-retry schedule, retrying only the transient class.
#[cfg(windows)]
async fn retry_windows_transient<Attempt, AttemptFuture>(mut attempt: Attempt) -> std::io::Result<()>
where
    Attempt: FnMut() -> AttemptFuture,
    AttemptFuture: std::future::Future<Output = std::io::Result<()>>,
{
    let mut last_error: Option<std::io::Error> = None;
    for backoff in std::iter::once(std::time::Duration::ZERO).chain(WINDOWS_TRANSIENT_BACKOFF) {
        if !backoff.is_zero() {
            tokio::time::sleep(jittered_backoff(backoff)).await;
        }
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(attempt_error) if is_transient_windows_error(&attempt_error) => {
                last_error = Some(attempt_error);
            }
            Err(attempt_error) => return Err(attempt_error),
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("rename retries exhausted")))
}

/// Atomically publish a written temp file to `target` via `persist`, retrying Windows transient errors. Blocking.
///
/// An already-present `target` is not success: a mutable destination may hold stale content.
pub fn persist_temp_file(tmp: tempfile::NamedTempFile, target: &std::path::Path) -> std::io::Result<()> {
    persist_with_retry(tmp, |tmp| tmp.persist(target).map(|_| ()))
}

/// [`persist_temp_file`], except an existing `target` is left alone and reported as `AlreadyExists`. Blocking.
///
/// For a destination whose file identity matters: a replacing racer orphans what hardlinks the winner's file.
pub fn persist_temp_file_if_absent(tmp: tempfile::NamedTempFile, target: &std::path::Path) -> std::io::Result<()> {
    persist_with_retry(tmp, |tmp| tmp.persist_noclobber(target).map(|_| ()))
}

/// Outcome of a no-clobber publish attempt — see [`persist_temp_file_noclobber`].
#[derive(Debug)]
#[must_use]
pub enum PersistOutcome {
    Published,

    /// The target is occupied and untouched; the temp file comes back for a retry under another name.
    Occupied(tempfile::NamedTempFile),
}

/// Publish a temp file to `target` without replacing anything there; an occupied target is a value. Blocking.
///
/// For append-only destinations whose names are chosen, where a replacing publish lets one write destroy another.
pub fn persist_temp_file_noclobber(
    tmp: tempfile::NamedTempFile,
    target: &std::path::Path,
) -> std::io::Result<PersistOutcome> {
    persist_with_retry(tmp, |tmp| attempt_noclobber(tmp, target))
}

/// One no-clobber publish attempt, resolving the NFS lost-reply case.
fn attempt_noclobber(
    tmp: tempfile::NamedTempFile,
    target: &std::path::Path,
) -> Result<PersistOutcome, tempfile::PersistError> {
    match tmp.persist_noclobber(target) {
        Ok(_) => Ok(PersistOutcome::Published),
        Err(persist_err) if persist_err.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if published_at(target, &persist_err.file) {
                // Our link landed and only the NFS reply was lost; no `keep()`, or a `.tmp*` is stranded.
                Ok(PersistOutcome::Published)
            } else {
                Ok(PersistOutcome::Occupied(persist_err.file))
            }
        }
        Err(persist_err) => Err(persist_err),
    }
}

/// Whether the file at `target` is the one `tmp` holds, which only the NFS `link` fallback can produce.
///
/// Device and inode, never link count: `nlink == 2` can be a foreign hardlink, losing a record.
/// `symlink_metadata`, so a symlink at `target` is never taken for our hardlink.
#[cfg(unix)]
fn published_at(target: &std::path::Path, tmp: &tempfile::NamedTempFile) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let Ok(ours) = tmp.as_file().metadata() else {
        return false;
    };
    std::fs::symlink_metadata(target).is_ok_and(|there| there.dev() == ours.dev() && there.ino() == ours.ino())
}

/// No device/inode pair here, so `AlreadyExists` is always a genuine collision.
#[cfg(not(unix))]
fn published_at(_target: &std::path::Path, _tmp: &tempfile::NamedTempFile) -> bool {
    false
}

/// The shared publish loop with the Windows retry schedule, generic so a caller can get its temp file back.
fn persist_with_retry<Published>(
    tmp: tempfile::NamedTempFile,
    mut publish: impl FnMut(tempfile::NamedTempFile) -> Result<Published, tempfile::PersistError>,
) -> std::io::Result<Published> {
    #[cfg(windows)]
    {
        let mut tmp_opt = Some(tmp);
        let mut last_err: Option<std::io::Error> = None;
        for backoff in std::iter::once(std::time::Duration::ZERO).chain(WINDOWS_TRANSIENT_BACKOFF) {
            if !backoff.is_zero() {
                std::thread::sleep(jittered_backoff(backoff));
            }
            let temp_file = tmp_opt.take().expect("tmp_opt is always Some at loop entry");
            match publish(temp_file) {
                Ok(published) => return Ok(published),
                Err(persist_err) => {
                    if is_transient_windows_error(&persist_err.error) {
                        tmp_opt = Some(persist_err.file);
                        last_err = Some(persist_err.error);
                        continue;
                    }
                    return Err(persist_err.error);
                }
            }
        }
        // No existence re-check: an already-present target may hold stale content.
        Err(last_err.unwrap_or_else(|| std::io::Error::other("persist retries exhausted")))
    }
    #[cfg(not(windows))]
    {
        publish(tmp).map_err(|e| e.error)
    }
}

/// Atomically write `bytes` to `target` as a private file (`0o600` on Unix); `target`'s parent must exist. Blocking.
///
/// # Errors
///
/// Any I/O failure; `InvalidInput` when `target` has no parent.
pub fn write_bytes_atomic(target: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    let dir = target
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "target path has no parent"))?;

    #[cfg(unix)]
    let mut tmp = {
        use std::os::unix::fs::PermissionsExt as _;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o600))
            .tempfile_in(dir)?
    };
    #[cfg(not(unix))]
    let mut tmp = tempfile::Builder::new().tempfile_in(dir)?;

    tmp.write_all(bytes)?;
    persist_temp_file(tmp, target)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{
        PersistOutcome, persist_temp_file, persist_temp_file_noclobber, rename_with_windows_retry, write_bytes_atomic,
    };

    /// Baseline (all platforms): a written tempfile is published to the target.
    #[test]
    fn persist_temp_file_publishes_to_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(b"payload").unwrap();

        persist_temp_file(tmp, &target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
    }

    /// `write_bytes_atomic` publishes the bytes and, on Unix, the resulting
    /// file is private (`0o600`) — the contract the referrers/trust-root caches
    /// and managed-config state depend on.
    #[test]
    fn write_bytes_atomic_publishes_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("cache.json");

        write_bytes_atomic(&target, b"{\"ok\":true}").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"{\"ok\":true}");

        // A second write replaces the content atomically (overwrite path).
        write_bytes_atomic(&target, b"replaced").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"replaced");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "published cache file must be owner-only");
        }
    }

    /// A target with no parent component is rejected rather than silently
    /// writing to the current directory.
    #[test]
    fn write_bytes_atomic_rejects_parentless_target() {
        let err = write_bytes_atomic(std::path::Path::new("/"), b"x").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// Windows: a non-sharing reader holding the destination open makes the
    /// first persist fail with `ERROR_ACCESS_DENIED`/`ERROR_SHARING_VIOLATION`;
    /// the retry loop must succeed once the handle is released — exactly the
    /// "a process holds a just-published file open" hazard any atomic publish hits.
    /// Mirrors `blob_store::tests::write_blob_retries_on_sharing_violation_then_succeeds`.
    /// Linux/macOS skip it: persist/rename has no sharing-violation semantics there.
    #[cfg(windows)]
    #[test]
    fn persist_temp_file_succeeds_after_blocking_reader_released() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.ps1");

        // Pre-create the destination and hold it open read-only (no
        // FILE_SHARE_DELETE) so a persist over it triggers a sharing violation.
        let _ = std::fs::File::create(&target).unwrap();
        let blocker = std::fs::OpenOptions::new().read(true).open(&target).unwrap();

        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(b"new-content").unwrap();
        let target_clone = target.clone();
        let handle = std::thread::spawn(move || persist_temp_file(tmp, &target_clone));

        // Hold the handle past the first (no-backoff) attempt, then release so a
        // subsequent retry wins.
        std::thread::sleep(std::time::Duration::from_millis(150));
        drop(blocker);

        handle.join().unwrap().unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-content");
    }

    /// Baseline (all platforms): a populated directory tree lands at an absent
    /// destination and the source is consumed.
    #[tokio::test]
    async fn rename_with_windows_retry_moves_populated_directory() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("nested").join("payload"), b"content").unwrap();
        let destination = dir.path().join("destination");

        rename_with_windows_retry(&source, &destination).await.unwrap();

        assert_eq!(
            std::fs::read(destination.join("nested").join("payload")).unwrap(),
            b"content"
        );
        assert!(!source.exists(), "source dir should be consumed by rename");
    }

    /// Windows regression for issue #285: a handle on a file *inside* the source
    /// tree makes the parent-directory rename fail with `ERROR_ACCESS_DENIED` —
    /// the same hazard Defender's real-time scan creates against a just-written
    /// store directory. The retry loop must succeed once the handle is released.
    ///
    /// Deterministic by construction: the blocking handle is released *inside*
    /// the second attempt closure, so the first attempt provably ran against the
    /// held handle and the attempt count discriminates every failure mode — a
    /// bare single-attempt rename reds on the unwrap, and a blocker that failed
    /// to block reds on the count. No wall-clock coupling.
    /// Linux/macOS skip it: rename has no sharing-violation semantics there.
    #[cfg(windows)]
    #[tokio::test]
    async fn rename_with_windows_retry_succeeds_after_blocking_reader_released() {
        use std::cell::{Cell, RefCell};
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        let child = source.join("held.bin");
        std::fs::write(&child, b"payload").unwrap();
        let destination = dir.path().join("destination");

        // Rust std's default share mode grants FILE_SHARE_DELETE, which would
        // let the parent-directory rename through. Narrow to FILE_SHARE_READ so
        // the handle denies the rename — the same restrictive mode an AV scan
        // handle holds.
        let blocker = RefCell::new(Some(
            std::fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(&child)
                .unwrap(),
        ));
        let attempts = Cell::new(0usize);

        super::retry_windows_transient(|| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 2 {
                // First attempt ran against the held handle; release before
                // the retry so it wins.
                drop(blocker.borrow_mut().take());
            }
            tokio::fs::rename(&source, &destination)
        })
        .await
        .unwrap();

        assert_eq!(
            attempts.get(),
            2,
            "first attempt must fail against the held handle, second must win"
        );
        assert_eq!(std::fs::read(destination.join("held.bin")).unwrap(), b"payload");
    }

    /// Baseline (all platforms): an absent target accepts the publish.
    #[test]
    fn persist_noclobber_publishes_to_an_absent_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("record.json");
        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(b"payload").unwrap();

        match persist_temp_file_noclobber(tmp, &target).unwrap() {
            PersistOutcome::Published => {}
            PersistOutcome::Occupied(_) => panic!("an absent target must accept the publish"),
        }

        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
    }

    /// The defect this primitive exists for (all platforms): an occupied target
    /// is left byte-for-byte alone, and the caller gets its content back to
    /// publish elsewhere — so no record can be destroyed by another record
    /// landing on its name.
    #[test]
    fn persist_noclobber_leaves_an_occupied_target_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("record.json");
        std::fs::write(&target, b"first-record").unwrap();

        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(b"second-record").unwrap();

        let returned = match persist_temp_file_noclobber(tmp, &target).unwrap() {
            PersistOutcome::Occupied(returned) => returned,
            PersistOutcome::Published => panic!("an occupied target must never be replaced"),
        };
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"first-record",
            "the occupant must survive untouched"
        );

        // The unconsumed temp file still carries its content, so a retry under a
        // fresh name loses nothing.
        let second = dir.path().join("record-2.json");
        match persist_temp_file_noclobber(returned, &second).unwrap() {
            PersistOutcome::Published => {}
            PersistOutcome::Occupied(_) => panic!("the fresh name is free"),
        }
        assert_eq!(std::fs::read(&second).unwrap(), b"second-record");
        assert_eq!(std::fs::read(&target).unwrap(), b"first-record");
    }

    /// The NFS lost-reply case: `link()` landed server-side but the client never
    /// heard, so the retry reports `EEXIST` over a target that is *our own*
    /// content. Hard-linking the temp file into place reproduces that exact
    /// on-disk state — two links to one inode — on any Unix. The publish must
    /// count as done, and the temp name must be dropped rather than kept, or a
    /// `.tmp*` is stranded in the operator's sink on every spurious report.
    #[cfg(unix)]
    #[test]
    fn persist_noclobber_treats_our_own_landed_link_as_published() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("record.json");
        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(b"payload").unwrap();
        std::fs::hard_link(tmp.path(), &target).unwrap();
        let tmp_path = tmp.path().to_path_buf();

        match persist_temp_file_noclobber(tmp, &target).unwrap() {
            PersistOutcome::Published => {}
            PersistOutcome::Occupied(_) => panic!("our own landed link must count as published"),
        }

        assert_eq!(std::fs::read(&target).unwrap(), b"payload");
        assert!(
            !tmp_path.exists(),
            "the temp name must be dropped, never kept: {} still present",
            tmp_path.display()
        );
    }

    /// Discriminates the check above from "any `EEXIST` is ours": a *foreign*
    /// occupant has one link, so it stays `Occupied` and survives.
    #[cfg(unix)]
    #[test]
    fn persist_noclobber_does_not_mistake_a_foreign_occupant_for_its_own_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("record.json");
        std::fs::write(&target, b"someone-elses-record").unwrap();

        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(b"ours").unwrap();

        match persist_temp_file_noclobber(tmp, &target).unwrap() {
            PersistOutcome::Occupied(_) => {}
            PersistOutcome::Published => panic!("a single-link occupant is not our landed link"),
        }
        assert_eq!(std::fs::read(&target).unwrap(), b"someone-elses-record");
    }

    /// A second link on the temp file plus a foreign occupant at the target is
    /// the case a link *count* gets wrong, and getting it wrong loses a record.
    ///
    /// The count reads 2 here and would report `Published`, dropping the temp
    /// path — while the target still holds someone else's record and ours
    /// survives, if at all, only under whatever name made the second link. Two
    /// provenances produce this same on-disk state: our own earlier attempt
    /// landed under a name a stale attribute cache under-reported, or another
    /// same-UID process on a shared sink hardlinked our visible `.tmp`. Neither
    /// makes the target ours, and file identity is what says so.
    #[cfg(unix)]
    #[test]
    fn persist_noclobber_never_reports_a_publish_it_did_not_make() {
        let dir = tempfile::tempdir().unwrap();
        let mut tmp = tempfile::NamedTempFile::new_in(dir.path()).unwrap();
        tmp.write_all(b"ours").unwrap();
        // A second link on our temp file, under a name that is NOT the target.
        let elsewhere = dir.path().join("linked-by-someone-else.json");
        std::fs::hard_link(tmp.path(), &elsewhere).unwrap();

        let target = dir.path().join("record.json");
        std::fs::write(&target, b"someone-elses-record").unwrap();

        let returned = match persist_temp_file_noclobber(tmp, &target).unwrap() {
            PersistOutcome::Occupied(returned) => returned,
            PersistOutcome::Published => panic!("a name we never wrote must never be reported as published"),
        };
        assert_eq!(std::fs::read(&target).unwrap(), b"someone-elses-record");
        // The record is not lost: it comes back to be published under a fresh
        // name, which is the failure mode this primitive promises.
        assert_eq!(std::fs::read(returned.path()).unwrap(), b"ours");
    }
}
