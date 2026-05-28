// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Low-level cross-process advisory file lock.
//!
//! Consumers should reach for [`super::locked_file::LockedFile`],
//! [`super::locked_file::LockedJsonFile<T>`], or
//! [`super::locked_file::LockedTomlFile<T>`] for the canonical async,
//! F2-safe API. `FileLock` itself is the underlying primitive — callers
//! that must acquire from synchronous contexts (e.g. `auth::store` inside
//! a `spawn_blocking` body) reach it via
//! [`FileLock::lock_exclusive_blocking_with_timeout`].

#[derive(Debug)]
pub struct FileLock {
    _lock_file: std::fs::File,
}

impl FileLock {
    /// The file handle that owns the lock.
    ///
    /// Windows `LockFileEx` locks a byte range on a specific handle. Other
    /// handles in the same process that touch the locked range get
    /// `ERROR_LOCK_VIOLATION` (os error 33). In-place reads or writes against
    /// a directly-locked file MUST go through this handle —
    /// [`super::locked_file::LockedFile`] does so by construction.
    pub fn file_mut(&mut self) -> &mut std::fs::File {
        &mut self._lock_file
    }

    /// Try to acquire an exclusive lock without blocking.
    ///
    /// Returns `Ok(Some(guard))` if the lock was acquired, `Ok(None)` if another
    /// process already holds it (contention), or `Err` on a real I/O error.
    pub fn try_exclusive(file: std::fs::File) -> std::io::Result<Option<Self>> {
        match <std::fs::File as fs4::fs_std::FileExt>::try_lock_exclusive(&file) {
            Ok(true) => Ok(Some(FileLock { _lock_file: file })),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Acquire an exclusive lock; block until acquired or `duration` elapses.
    pub async fn lock_exclusive_with_timeout(
        file: std::fs::File,
        duration: std::time::Duration,
    ) -> std::io::Result<FileLock> {
        let blocking = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::fs_std::FileExt>::lock_exclusive(&file)?;
            Ok::<_, std::io::Error>(file)
        });

        match tokio::time::timeout(duration, blocking).await {
            Ok(join_result) => {
                let file = join_result.map_err(std::io::Error::other)??;
                Ok(FileLock { _lock_file: file })
            }
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "lock timed out")),
        }
    }

    /// Acquire a shared lock; block until acquired or `duration` elapses.
    pub async fn lock_shared_with_timeout(
        file: std::fs::File,
        duration: std::time::Duration,
    ) -> std::io::Result<FileLock> {
        let blocking = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::fs_std::FileExt>::lock_shared(&file)?;
            Ok::<_, std::io::Error>(file)
        });

        match tokio::time::timeout(duration, blocking).await {
            Ok(join_result) => {
                let file = join_result.map_err(std::io::Error::other)??;
                Ok(FileLock { _lock_file: file })
            }
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "lock timed out")),
        }
    }

    /// Synchronous sibling of [`Self::lock_exclusive_with_timeout`] for callers
    /// inside `tokio::task::spawn_blocking` that cannot `.await`.
    ///
    /// Polls for the lock in a 25 ms tick loop until either the lock is
    /// acquired or `timeout` elapses. Returns `io::ErrorKind::TimedOut` on
    /// expiry.
    pub fn lock_exclusive_blocking_with_timeout(
        file: std::fs::File,
        timeout: std::time::Duration,
    ) -> std::io::Result<Self> {
        const TICK: std::time::Duration = std::time::Duration::from_millis(25);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match <std::fs::File as fs4::fs_std::FileExt>::try_lock_exclusive(&file) {
                Ok(true) => return Ok(FileLock { _lock_file: file }),
                Ok(false) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "lock timed out"));
                    }
                    std::thread::sleep(TICK);
                }
                Err(error) => return Err(error),
            }
        }
    }

    // ── Test-only acquisition primitives ────────────────────────────────────
    //
    // Production callers reach the lock through `LockedFile::open_*` or
    // `try_exclusive`. The fully-blocking acquisition variants below have no
    // production callers — they exist to exercise the wait/wake semantics in
    // the inline regression test. Kept `#[cfg(test)]` so they cannot drift
    // into production paths without an explicit module-internal need.

    #[cfg(test)]
    fn try_shared(file: std::fs::File) -> std::io::Result<Option<Self>> {
        match <std::fs::File as fs4::fs_std::FileExt>::try_lock_shared(&file) {
            Ok(true) => Ok(Some(FileLock { _lock_file: file })),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }

    #[cfg(test)]
    async fn lock_exclusive(file: std::fs::File) -> std::io::Result<Self> {
        let handle = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::fs_std::FileExt>::lock_exclusive(&file)?;
            Ok::<_, std::io::Error>(file)
        });
        let file = handle.await.map_err(std::io::Error::other)??;
        Ok(FileLock { _lock_file: file })
    }

    #[cfg(test)]
    async fn lock_shared(file: std::fs::File) -> std::io::Result<Self> {
        let handle = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::fs_std::FileExt>::lock_shared(&file)?;
            Ok::<_, std::io::Error>(file)
        });
        let file = handle.await.map_err(std::io::Error::other)??;
        Ok(FileLock { _lock_file: file })
    }
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;

    use super::*;

    #[tokio::test]
    async fn test_file_lock() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let lock_path = temp_dir.path().join("test.lock");
        std::fs::File::create(&lock_path)?;
        let lock = FileLock::try_exclusive(std::fs::File::open(&lock_path)?)?.expect("acquired exclusive");
        assert!(FileLock::try_exclusive(std::fs::File::open(&lock_path)?)?.is_none());
        assert!(FileLock::try_shared(std::fs::File::open(&lock_path)?)?.is_none());
        drop(lock);
        let lock_one = FileLock::try_shared(std::fs::File::open(&lock_path)?)?.expect("acquired shared one");
        let lock_two = FileLock::try_shared(std::fs::File::open(&lock_path)?)?.expect("acquired shared two");
        assert!(FileLock::try_exclusive(std::fs::File::open(&lock_path)?)?.is_none());
        drop(lock_one);
        assert!(FileLock::try_exclusive(std::fs::File::open(&lock_path)?)?.is_none());
        let lock_future = FileLock::lock_exclusive(std::fs::File::open(&lock_path)?);
        tokio::pin!(lock_future);
        assert!(lock_future.as_mut().now_or_never().is_none());
        drop(lock_two);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let lock = match lock_future.as_mut().now_or_never() {
            Some(result) => result?,
            None => panic!("Lock future should be ready after dropping shared lock"),
        };
        let lock_future = FileLock::lock_shared(std::fs::File::open(&lock_path)?);
        tokio::pin!(lock_future);
        assert!(lock_future.as_mut().now_or_never().is_none());
        drop(lock);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let lock = match lock_future.as_mut().now_or_never() {
            Some(result) => result?,
            None => panic!("Lock future should be ready after dropping exclusive lock"),
        };
        drop(lock);
        Ok(())
    }
}
