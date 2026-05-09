// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

// Option A: `Ok(None)` = contended, `Ok(Some(guard))` = acquired, `Err` = real I/O error.
// This models the three states correctly without introducing a new error type.

#[derive(Debug)]
pub struct FileLock {
    _lock_file: std::fs::File,
}

impl FileLock {
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

    /// Try to acquire a shared lock without blocking.
    ///
    /// Returns `Ok(Some(guard))` if the lock was acquired, `Ok(None)` if another
    /// process already holds an exclusive lock (contention), or `Err` on a real I/O error.
    pub fn try_shared(file: std::fs::File) -> std::io::Result<Option<Self>> {
        match <std::fs::File as fs4::fs_std::FileExt>::try_lock_shared(&file) {
            Ok(true) => Ok(Some(FileLock { _lock_file: file })),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn lock_exclusive(file: std::fs::File) -> std::io::Result<Self> {
        let handle = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::fs_std::FileExt>::lock_exclusive(&file)?;
            Ok::<_, std::io::Error>(file)
        });
        let file = handle.await.map_err(std::io::Error::other)??;
        Ok(FileLock { _lock_file: file })
    }

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

    pub async fn lock_shared(file: std::fs::File) -> std::io::Result<Self> {
        let handle = tokio::task::spawn_blocking(move || {
            <std::fs::File as fs4::fs_std::FileExt>::lock_shared(&file)?;
            Ok::<_, std::io::Error>(file)
        });
        let file = handle.await.map_err(std::io::Error::other)??;
        Ok(FileLock { _lock_file: file })
    }

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
