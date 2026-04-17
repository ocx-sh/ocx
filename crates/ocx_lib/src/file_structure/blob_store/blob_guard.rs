// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::file_lock::FileLock;
use crate::{Result, error::file_error};

/// Max time we're willing to block waiting for another writer to release the
/// per-blob `data` file lock. Mirrors `TagGuard::LOCK_TIMEOUT`.
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-blob reader/writer guard over a content-addressed `data` file.
///
/// Holds an `fs2` advisory lock — shared for reads, exclusive for writes —
/// directly on the `data` file itself. No sidecar `.lock`, no temp sibling,
/// no atomic rename: writers lock the file and update it in place
/// (truncate + write + `sync_all`).
///
/// Modelled exactly on
/// [`crate::oci::index::local_index::tag_guard::TagGuard`].
/// Crash trade: a `kill -9` mid-write can leave the `data` file truncated;
/// the next `Manifest::read_json` attempt will fail to parse,
/// `LocalIndex::get_manifest` logs at warn and returns `None`, and
/// `ChainedIndex::fetch_manifest`'s cache-miss path re-fetches the blob.
/// Safe because blob content is immutable by digest — any re-fetch produces
/// identical bytes.
pub struct BlobGuard {
    _lock: FileLock,
    target_path: PathBuf,
}

impl BlobGuard {
    /// Acquires an exclusive (writer) lock on the blob `data` file at
    /// `target_path`, creating the file and its parent directories on first
    /// use. Blocks until the lock is available or [`LOCK_TIMEOUT`] elapses.
    pub async fn acquire_exclusive(target_path: PathBuf) -> Result<Self> {
        if let Some(parent) = target_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| file_error(parent, e))?;
        }
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

    /// Acquires a shared (reader) lock on the blob `data` file at
    /// `target_path`. Returns `Ok(None)` if the file does not exist (so
    /// callers can treat absence as "no blob yet" without racing an
    /// exclusive writer).
    pub async fn acquire_shared(target_path: PathBuf) -> Result<Option<Self>> {
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

    /// Truncates the blob `data` file in place and writes `bytes`,
    /// `sync_all`-ing for durability. Concurrent writers are serialised by
    /// the exclusive lock held by the caller.
    pub async fn write_bytes(&self, bytes: &[u8]) -> Result<()> {
        // Second fd: the lock fd from acquire_exclusive cannot portably seek/truncate for a
        // fresh write, so we open a new fd. fs2 serialization on the lock fd ensures concurrent
        // writers are sequential, so the second-fd truncate is safe.
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&self.target_path)
            .await
            .map_err(|e| file_error(&self.target_path, e))?;
        file.write_all(bytes)
            .await
            .map_err(|e| file_error(&self.target_path, e))?;
        file.sync_all().await.map_err(|e| file_error(&self.target_path, e))?;
        Ok(())
    }

    /// Reads the full blob `data` file under the lock.
    pub async fn read_bytes(&self) -> Result<Vec<u8>> {
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .open(&self.target_path)
            .await
            .map_err(|e| file_error(&self.target_path, e))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .await
            .map_err(|e| file_error(&self.target_path, e))?;
        Ok(buf)
    }

    /// Returns the path this guard was opened on (for inspection in tests).
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn target_path(&self) -> &std::path::Path {
        &self.target_path
    }
}

// ── Specification tests ───────────────────────────────────────────────────
//
// Written from the design record (plan_resolution_chain_refs.md §Testing
// Strategy, tests 1-10). These mirror tag_guard.rs tests structurally.
#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::time::Duration;

    // ── test 1 ────────────────────────────────────────────────────────────

    /// Design record §1: acquire_exclusive creates the blob data file and its
    /// parent directories. No sidecar `.lock` file may appear alongside it.
    #[tokio::test]
    async fn acquire_exclusive_creates_blob_file_and_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("cdef1234").join("data");
        let guard = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        assert!(target.exists(), "data file must be created on exclusive acquire");
        drop(guard);
        // No sidecar .lock file.
        let sidecar = target.with_extension("data.lock");
        assert!(!sidecar.exists(), "no sidecar .lock file may be created");
    }

    // ── test 2 ────────────────────────────────────────────────────────────

    /// Design record §2: acquire_shared on a missing file returns None.
    #[tokio::test]
    async fn acquire_shared_on_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("cdef1234").join("data");
        let result = BlobGuard::acquire_shared(target).await.unwrap();
        assert!(result.is_none(), "shared acquire on missing file must return None");
    }

    // ── test 3 ────────────────────────────────────────────────────────────

    /// Design record §3: acquire_shared returns Some when the file exists
    /// (after an exclusive acquire created it).
    #[tokio::test]
    async fn acquire_shared_returns_some_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("cdef1234").join("data");
        // Create the file via an exclusive acquire first.
        let writer = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        writer.write_bytes(b"blob content").await.unwrap();
        drop(writer);
        // Now a shared acquire must succeed.
        let guard = BlobGuard::acquire_shared(target).await.unwrap();
        assert!(guard.is_some(), "shared acquire must return Some when file exists");
    }

    // ── test 4 ────────────────────────────────────────────────────────────

    /// Design record §4: multiple shared locks can coexist simultaneously.
    #[tokio::test]
    async fn shared_locks_can_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("data");
        // Create the file first.
        let writer = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        writer.write_bytes(b"shared content").await.unwrap();
        drop(writer);
        // Acquire two shared locks simultaneously — must not block each other.
        let a = BlobGuard::acquire_shared(target.clone()).await.unwrap().unwrap();
        let b = BlobGuard::acquire_shared(target.clone()).await.unwrap().unwrap();
        drop(a);
        drop(b);
    }

    // ── test 5 ────────────────────────────────────────────────────────────

    /// Design record §5: a shared lock blocks behind an exclusive lock and
    /// unblocks when the exclusive lock is dropped.
    #[tokio::test]
    async fn shared_blocks_behind_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let target = Arc::new(dir.path().join("sha256").join("ab").join("data"));

        let exclusive = BlobGuard::acquire_exclusive((*target).clone()).await.unwrap();

        let target_clone = target.clone();
        let waiter = tokio::spawn(async move { BlobGuard::acquire_shared((*target_clone).clone()).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "shared acquire must block behind exclusive");

        drop(exclusive);
        waiter
            .await
            .unwrap()
            .unwrap()
            .expect("shared guard must be Some after file exists");
    }

    // ── test 6 ────────────────────────────────────────────────────────────

    /// Design record §6: a second exclusive acquire blocks behind the first
    /// and succeeds once the first is dropped.
    #[tokio::test]
    async fn second_exclusive_blocks_behind_first() {
        let dir = tempfile::tempdir().unwrap();
        let target = Arc::new(dir.path().join("sha256").join("ab").join("data"));

        let first = BlobGuard::acquire_exclusive((*target).clone()).await.unwrap();

        let target_clone = target.clone();
        let waiter = tokio::spawn(async move { BlobGuard::acquire_exclusive((*target_clone).clone()).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiter.is_finished(),
            "second exclusive acquire must block behind first"
        );

        drop(first);
        waiter.await.unwrap().unwrap();
    }

    // ── test 7 ────────────────────────────────────────────────────────────

    /// Design record §7: write_bytes truncates the file and syncs to disk.
    /// A second write_bytes replaces the first content entirely.
    #[tokio::test]
    async fn write_bytes_truncates_and_syncs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("data");
        let guard = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_bytes(b"first write content").await.unwrap();
        guard.write_bytes(b"replaced").await.unwrap();
        drop(guard);
        // Verify truncation: second write must have replaced first.
        let content = std::fs::read(&target).unwrap();
        assert_eq!(content, b"replaced", "write_bytes must truncate before writing");
    }

    // ── test 8 ────────────────────────────────────────────────────────────

    /// Design record §8: write_bytes then read_bytes round-trips the content.
    #[tokio::test]
    async fn read_bytes_round_trips_written_content() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("data");
        let writer = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        let payload = b"manifest json content here";
        writer.write_bytes(payload).await.unwrap();
        drop(writer);

        let reader = BlobGuard::acquire_shared(target).await.unwrap().unwrap();
        let read_back = reader.read_bytes().await.unwrap();
        assert_eq!(
            read_back, payload,
            "read_bytes must return exactly what write_bytes wrote"
        );
    }

    // ── test 9 ────────────────────────────────────────────────────────────

    /// Design record §9: after an exclusive acquire + write + drop, no sidecar
    /// files (.lock, .tmp, .log) remain in the parent directory.
    #[tokio::test]
    async fn no_sidecar_lock_file_created_after_acquire_write_drop() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("sha256").join("ab");
        let target = parent.join("data");
        let guard = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_bytes(b"blob bytes").await.unwrap();
        drop(guard);

        let entries: Vec<String> = std::fs::read_dir(&parent)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            entries,
            vec!["data".to_string()],
            "only the data file itself must exist; got: {entries:?}"
        );
    }

    // ── T4 ───────────────────────────────────────────────────────────────
    //
    // T4 (plan review): the old "kill-9 simulation" test here targeted the
    // wrong contract — it tested that serde_json rejects partial JSON, which
    // is a library property not worth pinning here.  The production contract
    // (truncated blob → LocalIndex returns None + logs warn → ChainedIndex
    // re-fetches) is fully covered by:
    //   `crates/ocx_lib/src/oci/index/local_index.rs`
    //   `::get_manifest_on_truncated_blob_file_returns_none_and_logs_warn`
    // Keeping this test would assert serde_json internals, not our API.
    // Deleted (Option A from the review plan).
}
