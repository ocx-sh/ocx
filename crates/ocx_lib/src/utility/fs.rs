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
/// Uses `tokio::fs::rename` which requires `src` and `dst` to reside on
/// the same filesystem. Cross-device moves return an OS error.
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
    tokio::fs::rename(src, dst)
        .await
        .map_err(|e| crate::error::file_error(src, e))?;
    Ok(())
}

/// Atomically publish a written [`tempfile::NamedTempFile`] to `target` via
/// `persist`, retrying on Windows transient lock/access errors.
///
/// The single cross-platform atomic-publish primitive: callers write content
/// into a `NamedTempFile` in the destination directory, then hand it here to
/// rename it into place. Used by [`BlobStore::write_blob`](crate::file_structure::BlobStore)
/// (content-addressed blobs) and `ocx self activate` (the version-stamped
/// completion file).
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
    #[cfg(windows)]
    {
        use std::time::Duration;

        const BACKOFF: [Duration; 3] = [
            Duration::from_millis(100),
            Duration::from_millis(400),
            Duration::from_millis(800),
        ];

        let mut tmp_opt = Some(tmp);
        let mut last_err: Option<std::io::Error> = None;
        // First attempt with no backoff, then up to 3 retries with jitter.
        for backoff in std::iter::once(Duration::ZERO).chain(BACKOFF) {
            if !backoff.is_zero() {
                // ±25% jitter from SystemTime subsecond nanos (no `rand` dep).
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos();
                let jitter_scale = 0.75 + (f64::from(nanos % 1024) / 1023.0) * 0.5;
                std::thread::sleep(Duration::from_secs_f64(backoff.as_secs_f64() * jitter_scale));
            }
            let temp_file = tmp_opt.take().expect("tmp_opt is always Some at loop entry");
            match temp_file.persist(target) {
                Ok(_) => return Ok(()),
                Err(persist_err) => {
                    // ERROR_ACCESS_DENIED (5) / ERROR_SHARING_VIOLATION (32) — transient.
                    if matches!(persist_err.error.raw_os_error(), Some(5) | Some(32)) {
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
        tmp.persist(target).map(|_| ()).map_err(|e| e.error)
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

    use super::{persist_temp_file, write_bytes_atomic};

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
}
