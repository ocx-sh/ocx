// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod assemble;
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
mod symlink_walk;

pub use assemble::{
    AssemblyError, AssemblyStats, LayerPlacement, assemble_from_layer, assemble_from_layers,
    assemble_from_layers_with_layouts,
};
pub use bounded_read::{BoundedReadError, read_bounded};
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
    persist_with_retry(tmp, |tmp| tmp.persist(target).map(|_| ()))
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
    persist_with_retry(tmp, |tmp| tmp.persist_noclobber(target).map(|_| ()))
}

/// Outcome of a no-clobber publish attempt — see [`persist_temp_file_noclobber`].
#[derive(Debug)]
#[must_use]
pub enum PersistOutcome {
    /// The temp file now lives at the target path.
    Published,

    /// Something already occupies the target, which was left untouched. The temp
    /// file comes back unconsumed so the caller can retry under a different name
    /// without re-writing its content.
    Occupied(tempfile::NamedTempFile),
}

/// Publish a written [`tempfile::NamedTempFile`] to `target` **without replacing**
/// an existing file there, reporting an occupied target as a *value*.
///
/// The counterpart to [`persist_temp_file`] for append-only destinations — an
/// audit trail, an execution-record sink — where the target name is *chosen*
/// rather than content-addressed, so a replacing publish would let one write
/// silently destroy another. `persist_temp_file` remains correct for a mutable
/// destination whose whole point is to be overwritten.
///
/// An occupied target is [`PersistOutcome::Occupied`], not an error: the caller
/// draws a fresh name and calls again, and gets its unconsumed temp file back to
/// do it with. Only genuine I/O failures return `Err`. That is the whole
/// difference from [`persist_temp_file_if_absent`], which reports the same
/// collision as an `AlreadyExists` error for a caller that converges on the
/// winner instead of renaming around it.
///
/// # The NFS lost-reply case
///
/// `persist_noclobber` publishes with `renameat2(RENAME_NOREPLACE)` where the
/// kernel supports it and falls back to `link` + `unlink` on `ENOSYS`/`EINVAL`.
/// The Linux NFS client rejects `renameat2` flags with `EINVAL`, so **NFS is
/// exactly the hardlink path** — and there `link()` is atomic server-side but a
/// lost reply plus a client retry reports `EEXIST` even though the first attempt
/// succeeded. On `EEXIST` this therefore compares the *identity* of the file now
/// at `target` with the temp file's: same device and inode means the occupant is
/// our own link under that very name, so the result is
/// [`PersistOutcome::Published`] and the temp file is *dropped*, taking the link
/// count 2 → 1 and leaving only the published name. Keeping it would strand a
/// `.tmp*` in the caller's directory on every spurious report.
///
/// Identity, not link count, is what makes [`PersistOutcome::Published`] name a
/// path this call actually wrote — and on a shared sink the difference is a lost
/// record, not a cosmetic one. A count says only that *some* second link to the
/// temp file exists, which two ordinary situations produce without the target
/// being ours: a caller retrying under fresh names after a stale attribute cache
/// under-reported an earlier landing, and another same-UID process hardlinking
/// our visible `.tmp` under a name of its own. Either way `nlink == 2` would
/// report a publish over a foreign occupant, drop the temp path, and leave the
/// caller believing a record landed that did not. A device+inode match cannot
/// say that. On the `renameat2` path the check is inert, because a genuine
/// `EEXIST` there leaves a foreign file at `target`. Windows has no comparable
/// identity in `Metadata` and gets no such check.
///
/// The Windows transient-lock backoff of [`persist_temp_file`] applies here too:
/// without it, a Defender lock would surface as a hard publish failure rather
/// than the retry it is.
///
/// Blocking — `NamedTempFile` is synchronous; call from `spawn_blocking` inside
/// async code.
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
                // Our own link landed under this name and only the reply was
                // lost. Returning WITHOUT `keep()` drops the temp path here,
                // leaving just the published name behind.
                Ok(PersistOutcome::Published)
            } else {
                Ok(PersistOutcome::Occupied(persist_err.file))
            }
        }
        Err(persist_err) => Err(persist_err),
    }
}

/// Whether the file occupying `target` *is* the file `tmp` holds — the one
/// answer that makes a `Published` outcome name a path this call wrote.
///
/// Meaningful only on the `link` + `unlink` fallback, which is the NFS path; see
/// [`persist_temp_file_noclobber`]. `symlink_metadata` deliberately does not
/// follow: a symlink standing at `target` is never the hardlink we just made.
#[cfg(unix)]
fn published_at(target: &std::path::Path, tmp: &tempfile::NamedTempFile) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    let Ok(ours) = tmp.as_file().metadata() else {
        return false;
    };
    std::fs::symlink_metadata(target).is_ok_and(|there| there.dev() == ours.dev() && there.ino() == ours.ino())
}

/// Windows `Metadata` exposes no device/inode pair, so an `AlreadyExists` there
/// is always treated as a genuine collision.
#[cfg(not(unix))]
fn published_at(_target: &std::path::Path, _tmp: &tempfile::NamedTempFile) -> bool {
    false
}

/// The one publish implementation behind [`persist_temp_file`],
/// [`persist_temp_file_if_absent`] and [`persist_temp_file_noclobber`], carrying
/// the Windows transient-retry schedule all three share.
///
/// Generic over what an attempt yields, because the third caller needs its temp
/// file back: `persist_temp_file_noclobber` reports an occupied target as a
/// value rather than an error, and only `PersistError` hands the unconsumed file
/// over. A retry therefore republishes the same content instead of re-writing it.
fn persist_with_retry<Published>(
    tmp: tempfile::NamedTempFile,
    mut publish: impl FnMut(tempfile::NamedTempFile) -> Result<Published, tempfile::PersistError>,
) -> std::io::Result<Published> {
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
        // Retry exhausted. Return the last transient error — no idempotency
        // re-check here (see the doc comment): an already-present target may
        // hold stale content for a mutable destination. Content-addressed
        // callers re-check existence themselves.
        Err(last_err.unwrap_or_else(|| std::io::Error::other("persist retries exhausted")))
    }
    #[cfg(not(windows))]
    {
        publish(tmp).map_err(|e| e.error)
    }
}

/// Atomically write `bytes` to `target` as a private file (`0o600` on Unix).
///
/// Fills a [`tempfile::NamedTempFile`] created in `target`'s parent directory,
/// then publishes it over `target` via [`persist_temp_file`] (replace-existing
/// on every platform, with the Windows transient-lock retry). A concurrent
/// reader therefore never observes a partially-written file, and a repeated
/// write replaces the previous content atomically.
///
/// The temp file is created `0o600` — owner read/write only — making the
/// private-file contract explicit at the call site (trust material, capability
/// caches, managed-config state, shell-integration files). This matches the
/// `tempfile` crate's Unix default, so routing an existing plain
/// `NamedTempFile::new_in` write through this helper does not change the
/// published file's mode. On non-Unix platforms the `tempfile` default applies.
///
/// The parent directory must already exist — this helper does not create it
/// (callers needing it call `create_dir_all` first, so the single-responsibility
/// "publish these bytes" contract stays sharp). `target` must have a parent
/// component.
///
/// Blocking — `NamedTempFile` I/O and `persist` are synchronous; call from
/// `spawn_blocking` inside async code.
///
/// # Errors
///
/// Any I/O failure creating, writing, or publishing the temp file. Returns
/// [`std::io::ErrorKind::InvalidInput`] when `target` has no parent component.
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
