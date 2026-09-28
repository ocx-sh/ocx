// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Low-level cross-process advisory file lock; consumers use [`super::locked_file::LockedFile`].

#[derive(Debug)]
pub struct FileLock {
    _lock_file: std::fs::File,
}

/// Collapse an `fs4` try-lock outcome: contention is `Ok(false)`, a real I/O failure is `Err`.
fn acquired(outcome: Result<(), fs4::TryLockError>) -> std::io::Result<bool> {
    match outcome {
        Ok(()) => Ok(true),
        Err(fs4::TryLockError::WouldBlock) => Ok(false),
        Err(fs4::TryLockError::Error(error)) => Err(error),
    }
}

impl FileLock {
    /// The handle that owns the lock; in-place I/O must use it, or Windows fails with `ERROR_LOCK_VIOLATION`.
    pub(crate) fn file_mut(&mut self) -> &mut std::fs::File {
        &mut self._lock_file
    }

    /// Try to acquire an exclusive lock without blocking; `Ok(None)` means another holder has it.
    pub fn try_exclusive(file: std::fs::File) -> std::io::Result<Option<Self>> {
        match acquired(<std::fs::File as fs4::FileExt>::try_lock(&file)) {
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
            <std::fs::File as fs4::FileExt>::lock(&file)?;
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
    pub(crate) async fn lock_shared_with_timeout(
        file: std::fs::File,
        duration: std::time::Duration,
    ) -> std::io::Result<FileLock> {
        let blocking = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::FileExt>::lock_shared(&file)?;
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

    /// Synchronous sibling of [`Self::lock_exclusive_with_timeout`] for callers inside `spawn_blocking`.
    pub(crate) fn lock_exclusive_blocking_with_timeout(
        file: std::fs::File,
        timeout: std::time::Duration,
    ) -> std::io::Result<Self> {
        const TICK: std::time::Duration = std::time::Duration::from_millis(25);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match acquired(<std::fs::File as fs4::FileExt>::try_lock(&file)) {
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

    // Test-only: fully-blocking variants for the inline wait/wake tests.

    #[cfg(test)]
    fn try_shared(file: std::fs::File) -> std::io::Result<Option<Self>> {
        match acquired(<std::fs::File as fs4::FileExt>::try_lock_shared(&file)) {
            Ok(true) => Ok(Some(FileLock { _lock_file: file })),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }

    #[cfg(test)]
    async fn lock_exclusive(file: std::fs::File) -> std::io::Result<Self> {
        let handle = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::FileExt>::lock(&file)?;
            Ok::<_, std::io::Error>(file)
        });
        let file = handle.await.map_err(std::io::Error::other)??;
        Ok(FileLock { _lock_file: file })
    }

    #[cfg(test)]
    async fn lock_shared(file: std::fs::File) -> std::io::Result<Self> {
        let handle = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::FileExt>::lock_shared(&file)?;
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
