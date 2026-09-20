// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Single canonical primitive for in-place locked file I/O. F2-safe by
//! construction (all I/O routes through the lock-owning handle, never opens a
//! second handle on the locked range).
//!
//! Owns the file handle and the advisory lock together; every in-place read
//! or write routes through the lock-owning handle. F2-safe by construction
//! (cannot accidentally open a second handle on the locked range). F1-safe by
//! use: only used on files that have no external concurrent reader (sentinels,
//! `ocx.toml`, tag JSON, `install_status`).

use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::file_lock::FileLock;
use crate::error::{FileError, SerializationError};

const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// A file opened with an advisory lock held for the lifetime of this guard.
///
/// All in-place reads and writes are routed through the lock-owning handle,
/// making this type F2-safe by construction. Dropping the guard releases the
/// OS advisory lock.
#[derive(Debug)]
pub struct LockedFile {
    lock: FileLock,
    path: PathBuf,
}

/// Returns `true` when the locked handle still refers to the file currently at
/// `path` — i.e. the file was not unlinked or replaced while we waited to lock.
///
/// This closes the flock+unlink-by-path race: advisory locks live on the inode,
/// but a lock *file* is addressed by path, so a holder that removes it by path
/// (then releases) can let an opener that was blocked on the same inode acquire
/// a lock on a now-dangling inode while the path no longer exists. Comparing the
/// held handle's identity against the path's detects exactly that.
///
/// A file that is *replaced* rather than unlinked — by an atomic publish, as
/// `auth::store` does — is the same race seen from the other side, and Windows
/// is not exempt from it: `rename` over a target Rust holds open succeeds,
/// because std opens with `FILE_SHARE_DELETE`. So this is deliberately not
/// `cfg`-split. `same_file::Handle` is `(dev, ino)` on Unix and
/// `(volume serial, file index)` on Windows; std's own accessors for the latter
/// are still unstable (`windows_by_handle`, rust#63010).
///
/// Path gone (unlinked mid-acquire) or unreadable: a mismatch, so the caller
/// reopens and re-materializes the lock file.
fn lock_matches_path(lock: &mut FileLock, path: &Path) -> bool {
    // `Handle::from_file` consumes the file, so it gets a duplicate. Duplicating
    // is what makes that safe: `dup(2)` shares the open file description, and
    // `flock(2)` releases on the last close of *that* description, not of a
    // descriptor — so dropping the clone leaves the lock held. The same holds
    // for `DuplicateHandle` and `LockFileEx`, whose locks live on the file
    // object the duplicate still refers to.
    let Ok(duplicate) = lock.file_mut().try_clone() else {
        return false;
    };
    match (
        same_file::Handle::from_file(duplicate),
        same_file::Handle::from_path(path),
    ) {
        (Ok(held), Ok(current)) => held == current,
        _ => false,
    }
}

impl LockedFile {
    /// Acquire an exclusive lock on `path`. Creates the file (and parents)
    /// if absent. Blocks until acquired or [`DEFAULT_LOCK_TIMEOUT`] elapses.
    pub async fn open_exclusive(path: impl Into<PathBuf>) -> Result<Self, FileError> {
        Self::open_exclusive_with_timeout(path, DEFAULT_LOCK_TIMEOUT).await
    }

    /// Acquire an exclusive lock on `path` with a caller-supplied timeout.
    ///
    /// Creates the file (and all parent directories) if absent. Blocks until
    /// the lock is acquired or `timeout` elapses.
    ///
    /// Runs the whole open→lock→inode-verify dance on a blocking thread via the
    /// synchronous [`Self::open_exclusive_blocking_with_timeout`], so both the
    /// async and sync exclusive-acquire paths share one flock+unlink-safe
    /// implementation (see that method for the verify rationale).
    pub async fn open_exclusive_with_timeout(path: impl Into<PathBuf>, timeout: Duration) -> Result<Self, FileError> {
        let path = path.into();
        let error_path = path.clone();
        match tokio::task::spawn_blocking(move || Self::open_exclusive_blocking_with_timeout(path, timeout)).await {
            Ok(result) => result,
            Err(join_error) => Err(FileError::new(&error_path, std::io::Error::other(join_error))),
        }
    }

    /// Acquire a shared lock on `path`. Returns `Ok(None)` if the file does
    /// not exist (reader sees "no content yet" without racing a writer).
    pub async fn open_shared(path: impl Into<PathBuf>) -> Result<Option<Self>, FileError> {
        Self::open_shared_with_timeout(path, DEFAULT_LOCK_TIMEOUT).await
    }

    /// Acquire a shared lock on `path` with a caller-supplied timeout.
    ///
    /// Returns `Ok(None)` if the file does not exist.
    pub async fn open_shared_with_timeout(
        path: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<Option<Self>, FileError> {
        let path = path.into();
        let open_path = path.clone();
        let open_result = tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().read(true).open(&open_path))
            .await
            .map_err(std::io::Error::other)
            .and_then(std::convert::identity);

        let file = match open_result {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(FileError::new(&path, error)),
        };

        let lock = FileLock::lock_shared_with_timeout(file, timeout)
            .await
            .map_err(|e| FileError::new(&path, e))?;
        Ok(Some(Self { lock, path }))
    }

    /// Try to acquire an exclusive lock without blocking.
    ///
    /// Returns `Ok(None)` on contention (another process holds the lock).
    /// Creates the file (and parents) if absent.
    ///
    /// Runs the synchronous [`Self::try_exclusive_blocking`] on a blocking
    /// thread, so the async and sync non-blocking paths share the one
    /// flock+unlink-safe implementation — the `(dev, ino)` re-verify lived only
    /// on the sync side until a cross-model gate noticed this one lacked it.
    pub async fn try_exclusive(path: impl Into<PathBuf>) -> Result<Option<Self>, FileError> {
        let path = path.into();
        let error_path = path.clone();
        match tokio::task::spawn_blocking(move || Self::try_exclusive_blocking(path)).await {
            Ok(result) => result,
            Err(join_error) => Err(FileError::new(&error_path, std::io::Error::other(join_error))),
        }
    }

    /// Read the full file contents under the lock, through the lock-owning
    /// handle. Seeks to position 0 first. Empty file returns an empty `Vec`.
    ///
    /// Uses `tokio::task::block_in_place` so the blocking syscalls do not
    /// starve other async tasks on the current thread without requiring
    /// ownership transfer to a separate blocking thread (which would prevent
    /// routing through the lock-owning handle).
    ///
    /// # Panics
    ///
    /// `tokio::task::block_in_place` requires a multi-thread Tokio runtime.
    /// Calling this method from a `current_thread` runtime will panic with
    /// "can call blocking only when running on the multi-threaded runtime".
    /// Tests that exercise `LockedFile` must use `#[tokio::test(flavor = "multi_thread")]`.
    pub async fn read_bytes(&mut self) -> Result<Vec<u8>, FileError> {
        let path = &self.path;
        let file = self.lock.file_mut();
        tokio::task::block_in_place(|| {
            file.seek(SeekFrom::Start(0)).map_err(|e| FileError::new(path, e))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).map_err(|e| FileError::new(path, e))?;
            Ok(buf)
        })
    }

    /// Truncate to zero, write `bytes`, and `sync_data` for durability — all
    /// through the lock-owning handle. Order: `set_len(0)` → `seek(0)` →
    /// `write_all` → `sync_data`. Caller must hold an exclusive lock.
    ///
    /// Uses `tokio::task::block_in_place` so the blocking syscalls do not
    /// starve other async tasks without requiring ownership transfer.
    ///
    /// # Panics
    ///
    /// `tokio::task::block_in_place` requires a multi-thread Tokio runtime.
    /// Calling this method from a `current_thread` runtime will panic with
    /// "can call blocking only when running on the multi-threaded runtime".
    /// Tests that exercise `LockedFile` must use `#[tokio::test(flavor = "multi_thread")]`.
    pub async fn replace_bytes(&mut self, bytes: &[u8]) -> Result<(), FileError> {
        let path = &self.path;
        let file = self.lock.file_mut();
        tokio::task::block_in_place(|| {
            file.set_len(0).map_err(|e| FileError::new(path, e))?;
            file.seek(SeekFrom::Start(0)).map_err(|e| FileError::new(path, e))?;
            file.write_all(bytes).map_err(|e| FileError::new(path, e))?;
            file.sync_data().map_err(|e| FileError::new(path, e))?;
            Ok(())
        })
    }

    /// Returns the path of the locked file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Test-only: does the held handle still name the file at `self.path`?
    /// Exposes [`lock_matches_path`] so a test can drive both outcomes of the
    /// flock+unlink verify predicate directly.
    #[cfg(all(test, unix))] // its only callers are the unix-gated predicate tests
    fn handle_matches_path(&mut self) -> bool {
        let path = self.path.clone();
        lock_matches_path(&mut self.lock, &path)
    }

    // ── Synchronous API ───────────────────────────────────────────────────
    //
    // The async constructors and `read_bytes` / `replace_bytes` route through
    // `spawn_blocking` / `block_in_place` so an async runtime can hand the
    // blocking syscalls to a blocking thread without starving other tasks.
    // Sync callers (already running on a blocking thread — e.g. inside a
    // `tokio::task::spawn_blocking` body, or a non-async test) cannot await
    // those wrappers. The blocking variants below run the same logic directly
    // so callers do not need a runtime context.

    /// Synchronous sibling of [`Self::open_exclusive_with_timeout`].
    ///
    /// Creates the file (and parent directories) if absent, then acquires the
    /// exclusive advisory lock by polling in a 25 ms tick loop until either
    /// the lock is acquired or `timeout` elapses. Use from inside
    /// `tokio::task::spawn_blocking` or from a non-async test.
    pub fn open_exclusive_blocking_with_timeout(
        path: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<Self, FileError> {
        let path = path.into();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| FileError::new(parent, e))?;
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| FileError::new(&path, e))?;
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let mut lock = FileLock::lock_exclusive_blocking_with_timeout(file, remaining)
                .map_err(|e| FileError::new(&path, e))?;
            // The flock+unlink race (a concurrent holder that removes the lock
            // file by path while we waited — e.g. `ocx clean`'s temp sweep) would
            // otherwise leave us holding a lock on a dangling inode with no file
            // at `path`, so a *later* scan sees the guarded directory as an
            // unlocked orphan and removes it out from under us. Confirm the handle
            // still names the file at `path`; on mismatch drop it and reopen so we
            // lock the live inode. `create(true)` on reopen re-materializes the
            // lock file when it was unlinked, so the guard is present again.
            if lock_matches_path(&mut lock, &path) {
                return Ok(Self { lock, path });
            }
            drop(lock);
            if std::time::Instant::now() >= deadline {
                return Err(FileError::new(
                    &path,
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "lock file was unlinked during acquisition",
                    ),
                ));
            }
        }
    }

    /// Synchronous, non-blocking sibling of [`Self::try_exclusive`].
    ///
    /// Returns `Ok(None)` on contention. Creates the file (and parent
    /// directories) if absent.
    pub fn try_exclusive_blocking(path: impl Into<PathBuf>) -> Result<Option<Self>, FileError> {
        let path = path.into();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| FileError::new(parent, e))?;
        }
        // Bounded verify-and-retry: a genuine contention returns `None`
        // immediately, but an inode mismatch means the file was unlinked while we
        // held the lock (the flock+unlink race — see
        // `open_exclusive_blocking_with_timeout`). Reopening re-creates a fresh
        // inode we can lock at once, so a few iterations always suffice; the
        // bound stops a pathological churn from spinning. Falling back to `None`
        // on exhaustion is safe — the caller (`clean`) treats it as "busy, skip".
        for _ in 0..8 {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| FileError::new(&path, e))?;
            let Some(mut lock) = FileLock::try_exclusive(file).map_err(|e| FileError::new(&path, e))? else {
                return Ok(None);
            };
            if lock_matches_path(&mut lock, &path) {
                return Ok(Some(Self { lock, path }));
            }
            drop(lock);
        }
        Ok(None)
    }

    /// Synchronous sibling of [`Self::read_bytes`]. Same semantics, no
    /// `block_in_place` wrapping — caller is already on a blocking thread.
    pub fn read_bytes_blocking(&mut self) -> Result<Vec<u8>, FileError> {
        let path = &self.path;
        let file = self.lock.file_mut();
        file.seek(SeekFrom::Start(0)).map_err(|e| FileError::new(path, e))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).map_err(|e| FileError::new(path, e))?;
        Ok(buf)
    }
}

// ── Codec wrappers ────────────────────────────────────────────────────────────

/// `serde_json` codec wrapper over [`LockedFile`].
///
/// Bound: `T: Serialize + DeserializeOwned` — no additional bounds.
///
/// Empty file → `Ok(None)`. Unparseable → `Ok(None)` + `warn` log
/// (kill-9 recovery contract, mirrors `TagGuard::read_disk`).
pub struct LockedJsonFile<T> {
    inner: LockedFile,
    _marker: PhantomData<T>,
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> LockedJsonFile<T> {
    /// Acquire an exclusive lock on `path`. Creates the file (and parents) if
    /// absent.
    pub async fn open_exclusive(path: impl Into<PathBuf>) -> Result<Self, FileError> {
        let inner = LockedFile::open_exclusive(path).await?;
        Ok(Self {
            inner,
            _marker: PhantomData,
        })
    }

    /// Acquire an exclusive lock on `path` with a caller-supplied timeout.
    ///
    /// Creates the file (and all parent directories) if absent.
    pub async fn open_exclusive_with_timeout(path: impl Into<PathBuf>, timeout: Duration) -> Result<Self, FileError> {
        let inner = LockedFile::open_exclusive_with_timeout(path, timeout).await?;
        Ok(Self {
            inner,
            _marker: PhantomData,
        })
    }

    /// Acquire a shared lock on `path`. Returns `Ok(None)` if the file does
    /// not exist.
    pub async fn open_shared(path: impl Into<PathBuf>) -> Result<Option<Self>, FileError> {
        let maybe_inner = LockedFile::open_shared(path).await?;
        Ok(maybe_inner.map(|inner| Self {
            inner,
            _marker: PhantomData,
        }))
    }

    /// Acquire a shared lock on `path` with a caller-supplied timeout.
    ///
    /// Returns `Ok(None)` if the file does not exist.
    pub async fn open_shared_with_timeout(
        path: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<Option<Self>, FileError> {
        let maybe_inner = LockedFile::open_shared_with_timeout(path, timeout).await?;
        Ok(maybe_inner.map(|inner| Self {
            inner,
            _marker: PhantomData,
        }))
    }

    /// Read and parse the file under the lock.
    ///
    /// - Empty file → `Ok(None)`
    /// - Unparseable → `Ok(None)` + `warn` log (kill-9 recovery contract)
    pub async fn read(&mut self) -> Result<Option<T>, FileError> {
        let bytes = self.inner.read_bytes().await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                log::warn!(
                    "JSON file '{}' is unparseable ({error}) — treating as empty for recovery.",
                    self.inner.path().display()
                );
                Ok(None)
            }
        }
    }

    /// Serialize `value` as pretty-printed JSON and write it to the file via
    /// [`LockedFile::replace_bytes`].
    pub async fn write(&mut self, value: &T) -> crate::error::Result<()> {
        let bytes = serde_json::to_vec_pretty(value).map_err(SerializationError::from)?;
        Ok(self.inner.replace_bytes(&bytes).await?)
    }
}

/// `toml` codec wrapper over [`LockedFile`].
///
/// Bound: `T: Serialize + DeserializeOwned` — no additional bounds.
///
/// Empty file → `Ok(None)`. Unparseable → `Ok(None)` + `warn` log
/// (kill-9 recovery contract, mirrors `TagGuard::read_disk`).
pub struct LockedTomlFile<T> {
    inner: LockedFile,
    _marker: PhantomData<T>,
}

impl<T: serde::Serialize + serde::de::DeserializeOwned> LockedTomlFile<T> {
    /// Acquire an exclusive lock on `path`. Creates the file (and parents) if
    /// absent.
    pub async fn open_exclusive(path: impl Into<PathBuf>) -> Result<Self, FileError> {
        let inner = LockedFile::open_exclusive(path).await?;
        Ok(Self {
            inner,
            _marker: PhantomData,
        })
    }

    /// Acquire an exclusive lock on `path` with a caller-supplied timeout.
    ///
    /// Creates the file (and all parent directories) if absent.
    pub async fn open_exclusive_with_timeout(path: impl Into<PathBuf>, timeout: Duration) -> Result<Self, FileError> {
        let inner = LockedFile::open_exclusive_with_timeout(path, timeout).await?;
        Ok(Self {
            inner,
            _marker: PhantomData,
        })
    }

    /// Acquire a shared lock on `path`. Returns `Ok(None)` if the file does
    /// not exist.
    pub async fn open_shared(path: impl Into<PathBuf>) -> Result<Option<Self>, FileError> {
        let maybe_inner = LockedFile::open_shared(path).await?;
        Ok(maybe_inner.map(|inner| Self {
            inner,
            _marker: PhantomData,
        }))
    }

    /// Acquire a shared lock on `path` with a caller-supplied timeout.
    ///
    /// Returns `Ok(None)` if the file does not exist.
    pub async fn open_shared_with_timeout(
        path: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Result<Option<Self>, FileError> {
        let maybe_inner = LockedFile::open_shared_with_timeout(path, timeout).await?;
        Ok(maybe_inner.map(|inner| Self {
            inner,
            _marker: PhantomData,
        }))
    }

    /// Read and parse the file under the lock.
    ///
    /// - Empty file → `Ok(None)`
    /// - Invalid UTF-8 → `Ok(None)` + `warn` log (kill-9 recovery; TOML is
    ///   defined as UTF-8 so invalid bytes are equivalent to corruption)
    /// - Unparseable TOML → `Ok(None)` + `warn` log (kill-9 recovery contract)
    pub async fn read(&mut self) -> Result<Option<T>, FileError> {
        let bytes = self.inner.read_bytes().await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(error) => {
                log::warn!(
                    "TOML file '{}' contains invalid UTF-8 ({error}) — treating as empty for recovery.",
                    self.inner.path().display()
                );
                return Ok(None);
            }
        };
        match toml::from_str::<T>(text) {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                log::warn!(
                    "TOML file '{}' is unparseable ({error}) — treating as empty for recovery.",
                    self.inner.path().display()
                );
                Ok(None)
            }
        }
    }

    /// Serialize `value` as TOML and write it to the file via
    /// [`LockedFile::replace_bytes`].
    pub async fn write(&mut self, value: &T) -> Result<(), FileError> {
        let text = toml::to_string(value).map_err(|e| FileError::new(self.inner.path(), std::io::Error::other(e)))?;
        self.inner.replace_bytes(text.as_bytes()).await
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    // ── LockedFile tests ──────────────────────────────────────────────────────

    // All tests that call read_bytes / replace_bytes use block_in_place, which
    // requires the multi-threaded Tokio runtime.

    #[tokio::test(flavor = "multi_thread")]
    async fn open_exclusive_acquires_and_replaces_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let mut locked = LockedFile::open_exclusive(&path).await.unwrap();
        locked.replace_bytes(b"hello world").await.unwrap();
        let bytes = locked.read_bytes().await.unwrap();
        assert_eq!(bytes, b"hello world");
    }

    #[tokio::test]
    async fn open_shared_returns_none_when_file_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.lock");
        let result = LockedFile::open_shared(&path).await.unwrap();
        assert!(result.is_none(), "open_shared on absent file must return None");
    }

    // ── flock+unlink verify predicate (Codex Finding 6) ───────────────────────
    //
    // The lock guards against a concurrent holder (e.g. `ocx clean`'s temp
    // sweep) removing the lock file by path while another opener holds it,
    // leaving that opener on a dangling inode and the guarded directory exposed
    // as an orphan. `lock_matches_path` is the predicate that detects the
    // dangling/replaced case; these drive both of its outcomes directly.

    /// GREEN side: a freshly acquired lock's handle names the file at its path.
    #[cfg(unix)]
    #[test]
    fn handle_matches_path_is_true_for_a_live_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.lock");
        let mut lock = LockedFile::try_exclusive_blocking(&path).unwrap().expect("acquire");
        assert!(lock.handle_matches_path(), "a live lock handle must match its own path");
    }

    /// RED side: once the lock file is unlinked (what a racing sweep does), the
    /// held handle points at a dangling inode and no longer matches the path.
    #[cfg(unix)]
    #[test]
    fn handle_matches_path_is_false_after_unlink() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gone.lock");
        let mut lock = LockedFile::try_exclusive_blocking(&path).unwrap().expect("acquire");
        std::fs::remove_file(&path).unwrap();
        assert!(
            !lock.handle_matches_path(),
            "an unlinked lock file must not match — the handle is now dangling"
        );
    }

    /// RED side: if the lock file is replaced by a different inode at the same
    /// path (unlink + recreate, the two-step a racer performs), the old handle
    /// must not match the new file.
    #[cfg(unix)]
    #[test]
    fn handle_matches_path_is_false_after_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replaced.lock");
        let mut old = LockedFile::try_exclusive_blocking(&path).unwrap().expect("acquire");
        std::fs::remove_file(&path).unwrap();
        // A fresh file at the same path — a different inode.
        std::fs::write(&path, b"").unwrap();
        assert!(
            !old.handle_matches_path(),
            "a replaced lock file (new inode) must not match the stale handle"
        );
    }

    /// The verify-and-retry loop must still return `None` on genuine contention
    /// (a live holder in the same process), not spin or falsely re-acquire.
    #[cfg(unix)]
    #[test]
    fn try_exclusive_blocking_returns_none_under_contention() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy.lock");
        let _held = LockedFile::try_exclusive_blocking(&path)
            .unwrap()
            .expect("first holder");
        let second = LockedFile::try_exclusive_blocking(&path).unwrap();
        assert!(second.is_none(), "a contended try-lock must return None");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_bytes_truncates_then_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        let mut locked = LockedFile::open_exclusive(&path).await.unwrap();
        // Write 1KB first.
        locked.replace_bytes(&[b'x'; 1024]).await.unwrap();
        // Replace with 100B.
        locked.replace_bytes(&[b'y'; 100]).await.unwrap();
        let bytes = locked.read_bytes().await.unwrap();
        assert_eq!(
            bytes.len(),
            100,
            "file must be exactly 100 bytes after truncate-then-write"
        );
        assert!(bytes.iter().all(|&b| b == b'y'), "all bytes must be 'y'");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_bytes_returns_empty_for_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.lock");
        let mut locked = LockedFile::open_exclusive(&path).await.unwrap();
        let bytes = locked.read_bytes().await.unwrap();
        assert!(bytes.is_empty(), "freshly created file must read back as empty");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_open_exclusive_serialises() {
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("test.lock"));

        let first = LockedFile::open_exclusive((*path).clone()).await.unwrap();

        let path_clone = path.clone();
        let second_fut = tokio::spawn(async move { LockedFile::open_exclusive((*path_clone).clone()).await });

        // Give the second future a chance to run; it must still be blocked.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !second_fut.is_finished(),
            "second exclusive open must block behind first"
        );

        drop(first);
        second_fut.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_open_shared_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");
        // Create the file first so shared opens can find it.
        {
            let mut writer = LockedFile::open_exclusive(&path).await.unwrap();
            writer.replace_bytes(b"data").await.unwrap();
        }

        let guard_a = LockedFile::open_shared(&path).await.unwrap();
        let guard_b = LockedFile::open_shared(&path).await.unwrap();
        assert!(guard_a.is_some(), "first shared open must succeed");
        assert!(guard_b.is_some(), "second shared open must succeed concurrently");
        drop(guard_a);
        drop(guard_b);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn try_exclusive_returns_none_when_contended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.lock");

        // Hold exclusive lock with first handle.
        let first = LockedFile::open_exclusive(&path).await.unwrap();

        // Non-blocking try must fail while first is held.
        let second = LockedFile::try_exclusive(&path).await.unwrap();
        assert!(second.is_none(), "try_exclusive must return None when contended");

        drop(first);
    }

    // ── LockedJsonFile tests ──────────────────────────────────────────────────

    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    struct JsonData {
        name: String,
        value: i64,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_json_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let original = JsonData {
            name: "hello".to_string(),
            value: 42,
        };

        let mut locked: LockedJsonFile<JsonData> = LockedJsonFile::open_exclusive(&path).await.unwrap();
        locked.write(&original).await.unwrap();
        let read_back = locked.read().await.unwrap();
        assert_eq!(read_back, Some(original));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_json_file_empty_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        let mut locked: LockedJsonFile<JsonData> = LockedJsonFile::open_exclusive(&path).await.unwrap();
        let result = locked.read().await.unwrap();
        assert!(result.is_none(), "empty file must return None");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_json_file_unparseable_recovers_empty_with_warn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.json");
        // Write corrupt content directly to the file.
        std::fs::write(&path, b"not valid json { garbage").unwrap();

        let mut locked: LockedJsonFile<JsonData> = LockedJsonFile::open_exclusive(&path).await.unwrap();
        let result = locked.read().await.unwrap();
        // Must recover to Ok(None) + log warn (not error/panic).
        assert!(result.is_none(), "corrupt JSON must recover to None");
    }

    // ── LockedTomlFile tests ──────────────────────────────────────────────────

    #[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq)]
    struct TomlData {
        name: String,
        count: u32,
        tags: Vec<String>,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_toml_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = TomlData {
            name: "test".to_string(),
            count: 99,
            tags: vec!["a".to_string(), "b".to_string()],
        };

        let mut locked: LockedTomlFile<TomlData> = LockedTomlFile::open_exclusive(&path).await.unwrap();
        locked.write(&original).await.unwrap();
        let read_back = locked.read().await.unwrap();
        assert_eq!(read_back, Some(original));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_toml_file_empty_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut locked: LockedTomlFile<TomlData> = LockedTomlFile::open_exclusive(&path).await.unwrap();
        let result = locked.read().await.unwrap();
        assert!(result.is_none(), "empty file must return None");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_toml_file_unparseable_recovers_empty_with_warn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Write corrupt content directly.
        std::fs::write(&path, b"[broken toml {\n  not = valid").unwrap();

        let mut locked: LockedTomlFile<TomlData> = LockedTomlFile::open_exclusive(&path).await.unwrap();
        let result = locked.read().await.unwrap();
        assert!(result.is_none(), "corrupt TOML must recover to None");
    }

    // ── HashMap codec tests (additional coverage) ─────────────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn locked_json_file_hashmap_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("map.json");

        let mut map = HashMap::<String, String>::new();
        map.insert("key1".to_string(), "value1".to_string());
        map.insert("key2".to_string(), "value2".to_string());

        let mut locked: LockedJsonFile<HashMap<String, String>> = LockedJsonFile::open_exclusive(&path).await.unwrap();
        locked.write(&map).await.unwrap();
        let read_back = locked.read().await.unwrap();
        assert_eq!(read_back, Some(map));
    }

    // ── Windows cfg-gated tests ───────────────────────────────────────────────

    #[cfg(target_os = "windows")]
    mod windows {
        use super::*;
        use crate::fs::file_lock::FileLock;

        /// On Windows `FileLock` takes an advisory byte-range lock
        /// (`LockFileEx`). That lock does NOT prevent a second *open* of the
        /// same path: a raw `OpenOptions::open` still succeeds, handing the
        /// caller an unlocked handle that bypasses the advisory lock. This is
        /// exactly why callers must go through `LockedFile` (which owns the one
        /// lock-bearing handle) instead of opening the file directly.
        ///
        /// The lock itself is still genuinely exclusive: a second
        /// `try_exclusive` on the freshly opened handle reports contention
        /// rather than acquiring.
        #[test]
        fn second_raw_open_succeeds_but_lock_stays_exclusive() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.lock");
            // Create the file.
            std::fs::File::create(&path).unwrap();
            let file = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
            let _lock = FileLock::try_exclusive(file).unwrap().expect("acquired exclusive");

            // A second raw open succeeds — the advisory lock does not block opens.
            let second = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .expect("second raw open must succeed despite the advisory lock");

            // But the lock is still exclusive: locking the second handle is contended.
            let contended = FileLock::try_exclusive(second).unwrap();
            assert!(
                contended.is_none(),
                "second try_exclusive must observe contention while the first lock is held"
            );
        }

        /// Demonstrates that `replace_bytes` via the lock-owning handle does
        /// not hit `ERROR_LOCK_VIOLATION` even after many sequential rewrites.
        #[tokio::test(flavor = "multi_thread")]
        async fn replace_bytes_via_locked_handle_succeeds() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test.lock");
            let mut locked = LockedFile::open_exclusive(&path).await.unwrap();
            for i in 0u32..100 {
                locked
                    .replace_bytes(format!("iteration {i}").as_bytes())
                    .await
                    .expect("replace_bytes must succeed on lock-owning handle");
            }
            let bytes = locked.read_bytes().await.unwrap();
            assert_eq!(bytes, b"iteration 99");
        }
    }
}
