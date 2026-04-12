// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::file_lock::FileLock;
use crate::{Result, error::file_error, log, oci, prelude::*};

use super::tag_lock::TagLock;

/// Max time we're willing to block waiting for another writer to release the
/// per-repo tag lock. Long enough to survive a concurrent `ocx index update`
/// against a slow registry, short enough that a stuck holder surfaces instead
/// of hanging the CLI forever.
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-repository reader/writer guard over the local tag lock file.
///
/// Holds an `fs2` advisory lock — shared for reads, exclusive for writes —
/// directly on the canonical tag file itself. No sidecar `.lock` file, no
/// temp sibling, no atomic rename: writers lock the tag file and update it
/// in place (truncate + write + `sync_all`).
///
/// This is deliberately different from the classic "sidecar lock + atomic
/// rename" pattern. Atomic rename rotates the tag file's inode, which would
/// strand our advisory lock on the old inode and let a second writer race
/// us on the new one — which is why that pattern needs a stable-inode
/// sidecar. Locking the tag file directly and writing in place sidesteps
/// the inode rotation entirely, at the cost of crash atomicity: a
/// `kill -9` during `write_disk` can leave the tag file truncated and the
/// next read needs an `ocx index update` to rebuild it. That trade is
/// deliberate — concurrency safety is what matters for fallback writes,
/// crash atomicity is a rare edge case, and the sidecar file was never
/// cleaned up.
pub(super) struct TagGuard {
    _lock: FileLock,
    target_path: PathBuf,
}

impl TagGuard {
    /// Acquires an exclusive (writer) lock on the tag file at `target_path`,
    /// creating the file and its parent directories on first use. Blocks
    /// until the lock is available or [`LOCK_TIMEOUT`] elapses.
    pub async fn acquire_exclusive(target_path: PathBuf) -> Result<Self> {
        if let Some(parent) = target_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| file_error(parent, e))?;
        }
        // Sync handle: `fs2::FileExt` advisory-locks on the raw fd, and the
        // handle must outlive the lock (owned by `FileLock`). Run the blocking
        // `open` on the blocking pool so the reactor thread never blocks on a
        // filesystem syscall.
        let open_path = target_path.clone();
        let file = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&open_path)
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(std::convert::identity)
        .map_err(|e| file_error(&target_path, e))?;
        let lock = FileLock::lock_exclusive_with_timeout(file, LOCK_TIMEOUT)
            .await
            .map_err(|e| file_error(&target_path, e))?;
        Ok(Self {
            _lock: lock,
            target_path,
        })
    }

    /// Acquires a shared (reader) lock on the tag file at `target_path`.
    ///
    /// Readers never create the file — if it does not exist, returns
    /// `Ok(None)` so callers can treat absence as "no tags yet" without
    /// racing an exclusive writer. Multiple readers may hold the shared
    /// lock concurrently; an exclusive writer waits until the last reader
    /// drops.
    pub async fn acquire_shared(target_path: PathBuf) -> Result<Option<Self>> {
        // Run the blocking `open` on the blocking pool; matches the
        // off-reactor pattern in `acquire_exclusive`.
        let open_path = target_path.clone();
        let open_result = tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().read(true).open(&open_path))
            .await
            .map_err(std::io::Error::other)
            .and_then(std::convert::identity);
        let file = match open_result {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(file_error(&target_path, e)),
        };
        let lock = FileLock::lock_shared_with_timeout(file, LOCK_TIMEOUT)
            .await
            .map_err(|e| file_error(&target_path, e))?;
        Ok(Some(Self {
            _lock: lock,
            target_path,
        }))
    }

    /// Reads and parses the current tag file under the lock. Returns an
    /// empty map when the file is missing, was freshly created by an
    /// exclusive acquire and not yet written, **or** is unparseable.
    ///
    /// The unparseable case is the documented kill-9 recovery window: a
    /// writer killed mid-`write_disk` can leave the file truncated or
    /// corrupt. Treat that the same as "no tags yet" so the next chain walk
    /// or `ocx index update` can rewrite it cleanly, and log a warn so the
    /// recovery is observable.
    pub async fn read_disk(&self, identifier: &oci::Identifier) -> Result<HashMap<String, oci::Digest>> {
        match tokio::fs::metadata(&self.target_path).await {
            Ok(m) if m.len() == 0 => return Ok(HashMap::new()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(e) => return Err(file_error(&self.target_path, e)),
        }
        let parsed = TagLock::read_json(&self.target_path)
            .await
            .and_then(|tag_lock| tag_lock.into_tags(identifier, &self.target_path));
        match parsed {
            Ok(tags) => Ok(tags),
            Err(e) => {
                log::warn!(
                    "Tag file '{}' is unparseable ({e}) — treating as empty for recovery.",
                    self.target_path.display()
                );
                Ok(HashMap::new())
            }
        }
    }

    /// Overwrites the tag file in place with a `TagLock` containing `tags`.
    /// Truncates the existing file (same inode), writes the full JSON, and
    /// `sync_all`s for durability. Concurrent writers are serialised by the
    /// exclusive lock held by the caller.
    pub async fn write_disk(&self, identifier: &oci::Identifier, tags: &HashMap<String, oci::Digest>) -> Result<()> {
        let tag_lock = TagLock::new(identifier, tags.clone());
        let bytes = serde_json::to_vec_pretty(&tag_lock)?;

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.target_path)
            .await
            .map_err(|e| file_error(&self.target_path, e))?;
        file.write_all(&bytes)
            .await
            .map_err(|e| file_error(&self.target_path, e))?;
        file.sync_all().await.map_err(|e| file_error(&self.target_path, e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    fn make_id() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "ghcr.io").clone_with_tag("3.28")
    }

    fn tags_with(entries: &[(&str, char)]) -> HashMap<String, oci::Digest> {
        entries
            .iter()
            .map(|(t, c)| (t.to_string(), oci::Digest::Sha256(c.to_string().repeat(64))))
            .collect()
    }

    #[tokio::test]
    async fn acquire_exclusive_creates_tag_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let guard = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
        assert!(target.exists(), "tag file itself should be created on acquire");
        drop(guard);

        // No sidecar .lock file — the lock lives on the tag file's own fd.
        let sidecar = dir.path().join("ghcr.io").join("cmake.json.lock");
        assert!(!sidecar.exists(), "no sidecar .lock file must be created");
    }

    #[tokio::test]
    async fn acquire_shared_on_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let result = TagGuard::acquire_shared(target).await.unwrap();
        assert!(result.is_none(), "shared acquire on missing file must return None");
    }

    #[tokio::test]
    async fn read_disk_returns_empty_when_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let guard = TagGuard::acquire_exclusive(target).await.unwrap();
        let tags = guard.read_disk(&make_id()).await.unwrap();
        assert!(tags.is_empty());
    }

    #[tokio::test]
    async fn write_then_read_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let id = make_id();
        let tags = tags_with(&[("3.28", 'a'), ("latest", 'b')]);

        let guard = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_disk(&id, &tags).await.unwrap();
        let readback = guard.read_disk(&id).await.unwrap();
        assert_eq!(readback, tags);
    }

    #[tokio::test]
    async fn write_leaves_no_sidecar_or_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("ghcr.io");
        let target = parent.join("cmake.json");
        let guard = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard
            .write_disk(&make_id(), &tags_with(&[("3.28", 'a')]))
            .await
            .unwrap();
        drop(guard);

        let entries: Vec<_> = std::fs::read_dir(&parent)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            entries,
            vec!["cmake.json".to_string()],
            "only the tag file itself may exist"
        );
    }

    #[tokio::test]
    async fn second_exclusive_blocks_behind_first() {
        let dir = tempfile::tempdir().unwrap();
        let target = Arc::new(dir.path().join("ghcr.io").join("cmake.json"));

        let first = TagGuard::acquire_exclusive((*target).clone()).await.unwrap();

        let target_clone = target.clone();
        let waiter = tokio::spawn(async move { TagGuard::acquire_exclusive((*target_clone).clone()).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "second exclusive acquire must block behind first"
        );

        drop(first);
        waiter.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shared_blocks_behind_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let target = Arc::new(dir.path().join("ghcr.io").join("cmake.json"));

        let exclusive = TagGuard::acquire_exclusive((*target).clone()).await.unwrap();

        let target_clone = target.clone();
        let waiter = tokio::spawn(async move { TagGuard::acquire_shared((*target_clone).clone()).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "shared acquire must block behind exclusive");

        drop(exclusive);
        waiter
            .await
            .unwrap()
            .unwrap()
            .expect("shared guard must be Some after file exists");
    }

    #[tokio::test]
    async fn shared_locks_can_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");

        // Establish the file with an exclusive acquire + write + drop so the
        // subsequent shared acquires find an existing file to lock.
        let writer = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
        writer
            .write_disk(&make_id(), &tags_with(&[("3.28", 'a')]))
            .await
            .unwrap();
        drop(writer);

        let a = TagGuard::acquire_shared(target.clone()).await.unwrap().unwrap();
        let b = TagGuard::acquire_shared(target.clone()).await.unwrap().unwrap();
        drop(a);
        drop(b);
    }
}
