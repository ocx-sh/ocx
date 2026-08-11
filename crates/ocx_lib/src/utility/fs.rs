// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod assemble;
mod dir_walker;
mod drop_file;
mod empty_or_absent;
mod file_lock;
mod locked_file;
pub mod path;
mod same_dir;
mod same_filesystem;
mod scoped_lock;
mod symlink_walk;

pub use assemble::{
    AssemblyError, AssemblyStats, LayerPlacement, assemble_from_layer, assemble_from_layers,
    assemble_from_layers_with_layouts,
};
pub use dir_walker::{DirWalker, WalkDecision};
pub use drop_file::DropFile;
pub use empty_or_absent::{EmptyOrAbsentError, ensure_empty_or_absent};
// `FileLock` is the underlying primitive; consumers prefer the
// `LockedFile` / `LockedJsonFile` / `LockedTomlFile` API for in-place
// F2-safe I/O. `FileLock` itself is re-exported for the synchronous
// acquisition path (`lock_exclusive_blocking_with_timeout`) needed by
// `auth::store` inside a `spawn_blocking` body, and for `temp_store`
// which acquires synchronously from `stale_entries`.
pub use file_lock::FileLock;
pub use locked_file::{LockedFile, LockedJsonFile, LockedTomlFile};
pub use same_dir::same_dir;
pub use same_filesystem::{SameFilesystemError, same_filesystem};
pub use scoped_lock::lock_scoped;
pub use symlink_walk::{SymlinkWalkError, refuse_if_symlink_in_path};

/// Returns whether `path` exists, swallowing any I/O error as `false`.
///
/// Wraps [`tokio::fs::try_exists`] and emits a `debug!` log whenever
/// the probe fails (permission denied, transient I/O, etc.) so the
/// swallow is still observable in diagnostic output. Use when the
/// caller is tolerant of a missing path — either because a follow-up
/// fallible operation will naturally surface the same error with
/// better context, or because absence and I/O failure are handled
/// identically at the call site.
pub async fn path_exists_lossy(path: &std::path::Path) -> bool {
    match tokio::fs::try_exists(path).await {
        Ok(exists) => exists,
        Err(e) => {
            crate::log::debug!("path_exists_lossy probe failed for {}: {}", path.display(), e);
            false
        }
    }
}

/// Moves `src` directory to `dst` via same-filesystem rename.
///
/// Creates parent directories of `dst` if needed. If `dst` already exists
/// (e.g., from a crashed previous attempt), it is removed first.
///
/// Renames via [`rename_with_windows_retry`] — `src` and `dst` must reside
/// on the same filesystem (cross-device moves return an OS error), and on
/// Windows a transiently locked tree is retried for up to ~1.6 s before the
/// access-denied error surfaces.
pub async fn move_dir(src: &std::path::Path, dst: &std::path::Path) -> Result<(), crate::Error> {
    if let Some(parent) = dst.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| crate::error::file_error(parent, e))?;
    }
    if dst.exists() {
        tokio::fs::remove_dir_all(dst)
            .await
            .map_err(|e| crate::error::file_error(dst, e))?;
    }
    rename_with_windows_retry(src, dst)
        .await
        .map_err(|e| crate::error::file_error(src, e))?;
    Ok(())
}

/// Backoff schedule for a Windows transient sharing/access retry loop, shared by
/// [`persist_temp_file`] and [`rename_with_windows_retry`] (rattler
/// `rename_with_retry` precedent).
#[cfg(windows)]
const WINDOWS_TRANSIENT_BACKOFF: [std::time::Duration; 3] = [
    std::time::Duration::from_millis(100),
    std::time::Duration::from_millis(400),
    std::time::Duration::from_millis(800),
];

/// Scales `backoff` by ±25% jitter so concurrent retriers do not re-collide on
/// the same handle in lockstep. Derived from `SystemTime` subsecond nanos to
/// keep the retry path free of a `rand` dependency.
#[cfg(windows)]
fn jittered_backoff(backoff: std::time::Duration) -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter_scale = 0.75 + (f64::from(nanos % 1024) / 1023.0) * 0.5;
    std::time::Duration::from_secs_f64(backoff.as_secs_f64() * jitter_scale)
}

/// Whether `error` is the Windows transient class worth retrying:
/// `ERROR_ACCESS_DENIED` (5) or `ERROR_SHARING_VIOLATION` (32) — another handle
/// on the path, typically released within milliseconds.
#[cfg(windows)]
fn is_transient_windows_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5) | Some(32))
}

/// Renames `src` to `dst`, retrying on Windows transient lock/access errors.
///
/// The directory sibling of [`persist_temp_file`]. A Windows directory rename
/// fails with `ERROR_ACCESS_DENIED` (5) or `ERROR_SHARING_VIOLATION` (32) while
/// **any** file anywhere inside the tree is still held open — and Windows
/// Defender real-time scanning opens exactly the files that were just written,
/// milliseconds after they land. A freshly populated temp directory renamed into
/// its final store location therefore sits squarely in that hazard window; the
/// motivating report is issue #285 (`ocx install` failing with
/// "Access is denied. (os error 5)" on the temp-to-store rename).
///
/// The first attempt runs with no delay; up to three retries follow
/// (100/400/800 ms ±25% jitter). Any other error returns immediately, and after
/// retry exhaustion the last transient error is returned. On non-Windows this is
/// a single [`tokio::fs::rename`].
///
/// Makes **no idempotency assumption** — an already-present `dst` is NOT treated
/// as success. What an existing destination means is the caller's call: a
/// content-addressed destination may read it as a concurrent winner, a mutable
/// one as stale content that must not be silently kept.
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

/// Drives `attempt` through the Windows transient-retry schedule: first call
/// with no delay, then up to three retries after the jittered backoff, retrying
/// only the transient class. Parameterized over the operation so the test can
/// count attempts and release its blocking handle deterministically between
/// them — a wall-clock release cannot prove the retry path ran.
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

/// Atomically publish a written [`tempfile::NamedTempFile`] to `target` via
/// `persist`, retrying on Windows transient lock/access errors.
///
/// The cross-platform atomic-publish primitive: callers write content into a
/// `NamedTempFile` in the destination directory, then hand it here to rename it
/// into place. Used by [`BlobStore::write_blob`](crate::file_structure::BlobStore)
/// (content-addressed blobs) and `ocx self activate` (the version-stamped
/// completion file). [`persist_temp_file_if_absent`] is the sibling for a
/// destination whose file identity, not just its content, must survive a race.
///
/// On Windows, `persist` (a rename) over a just-written destination can fail
/// with `ERROR_SHARING_VIOLATION` (32) or `ERROR_ACCESS_DENIED` (5) when
/// Windows Defender real-time scanning or a non-sharing reader holds the target
/// open (rattler `rename_with_retry` precedent). The first attempt runs with no
/// delay; up to three retries follow (100/400/800 ms ±25% jitter). On
/// non-Windows this is a single `persist`.
///
/// After retry exhaustion the last transient error is returned. This helper
/// makes **no idempotency assumption** — an already-present `target` is NOT
/// treated as success, because for a mutable destination it may hold stale or
/// different content (a reader holding the old file open through every retry
/// would leave the old version in place). A caller whose destination is
/// content-addressed / immutable (e.g. [`BlobStore::write_blob`](crate::file_structure::BlobStore))
/// re-checks existence itself and treats a present target as success there.
///
/// Blocking — `NamedTempFile` is synchronous; call from `spawn_blocking` inside
/// async code.
pub fn persist_temp_file(tmp: tempfile::NamedTempFile, target: &std::path::Path) -> std::io::Result<()> {
    persist_with_retry(tmp, target, Existing::Replaced)
}

/// [`persist_temp_file`], except that an already-present `target` is left
/// alone and its presence reported as an error.
///
/// For a destination whose file **identity** matters — not merely its bytes —
/// `persist`'s replace semantics are the wrong primitive: it is a rename with
/// `MOVEFILE_REPLACE_EXISTING` on Windows and a plain `rename(2)` elsewhere, so
/// a losing racer silently swaps in a fresh file record and orphans the one the
/// winner published. Anything already hardlinked from that record keeps pointing
/// at the orphan. [`ShimBinStore`](crate::file_structure::ShimBinStore) is the
/// motivating caller: every generated `<name>.exe` on Windows hardlinks the one
/// published shim blob, so "same bytes" is not enough — they must be the same
/// file.
///
/// The caller decides what an existing target means. A content-addressed
/// destination reads the resulting error as "a concurrent writer won, and its
/// file is byte-identical to mine" and converges on it.
///
/// Blocking, same as [`persist_temp_file`].
pub fn persist_temp_file_if_absent(tmp: tempfile::NamedTempFile, target: &std::path::Path) -> std::io::Result<()> {
    persist_with_retry(tmp, target, Existing::Kept)
}

/// What a publish does when `target` already exists at rename time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Existing {
    /// Replace it (`NamedTempFile::persist`).
    Replaced,
    /// Keep it and fail (`NamedTempFile::persist_noclobber`).
    Kept,
}

/// The one publish implementation behind [`persist_temp_file`] and
/// [`persist_temp_file_if_absent`], carrying the Windows transient-retry
/// schedule both share.
fn persist_with_retry(
    tmp: tempfile::NamedTempFile,
    target: &std::path::Path,
    existing: Existing,
) -> std::io::Result<()> {
    fn publish(
        tmp: tempfile::NamedTempFile,
        target: &std::path::Path,
        existing: Existing,
    ) -> Result<(), tempfile::PersistError> {
        match existing {
            Existing::Replaced => tmp.persist(target).map(|_| ()),
            Existing::Kept => tmp.persist_noclobber(target).map(|_| ()),
        }
    }

    #[cfg(windows)]
    {
        let mut tmp_opt = Some(tmp);
        let mut last_err: Option<std::io::Error> = None;
        // First attempt with no backoff, then up to 3 retries with jitter.
        for backoff in std::iter::once(std::time::Duration::ZERO).chain(WINDOWS_TRANSIENT_BACKOFF) {
            if !backoff.is_zero() {
                std::thread::sleep(jittered_backoff(backoff));
            }
            let temp_file = tmp_opt.take().expect("tmp_opt is always Some at loop entry");
            match publish(temp_file, target, existing) {
                Ok(()) => return Ok(()),
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
        // Retry exhausted. Return the last transient error — no idempotency
        // re-check here (see the doc comment): an already-present target may
        // hold stale content for a mutable destination. Content-addressed
        // callers re-check existence themselves.
        Err(last_err.unwrap_or_else(|| std::io::Error::other("persist retries exhausted")))
    }
    #[cfg(not(windows))]
    {
        publish(tmp, target, existing).map_err(|e| e.error)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{persist_temp_file, rename_with_windows_retry};

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
}
