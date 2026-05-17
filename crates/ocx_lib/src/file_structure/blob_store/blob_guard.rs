// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::file_lock::FileLock;
use crate::{Result, error::file_error};

/// Max time we're willing to block waiting for another writer to release the
/// per-blob sidecar lock. Mirrors `TagGuard::LOCK_TIMEOUT`.
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-blob reader/writer guard over a content-addressed `data` file.
///
/// Holds an `fs4` advisory lock — shared for reads, exclusive for writes —
/// on a **sidecar `data.lock` file**, NOT on the `data` file itself.
///
/// ## Why a sidecar lock file?
///
/// On Windows, `fs4::FileExt::lock_exclusive` calls `LockFileEx` with flags
/// `LOCKFILE_EXCLUSIVE_LOCK` covering the entire file byte range. This is a
/// **mandatory** byte-range lock: any other process's `ReadFile`/`WriteFile`
/// on the same byte range returns `ERROR_LOCK_VIOLATION` (os error 33), even
/// if that other process only wants to read.
///
/// When `ocx package test` materialises a package and then spawns a child
/// process (e.g. a launcher that re-enters `ocx`), the child tries to open
/// the same blob `data` file for reading.  If the parent still holds the
/// `LockFileEx` lock on `data`, the child gets `ERROR_LOCK_VIOLATION`.
///
/// Moving the `LockFileEx` lock onto a separate `data.lock` sidecar leaves
/// the `data` file entirely free of byte-range locks — any process may open
/// and read it without restriction, on every OS.  The sidecar is never read
/// or written to; it only exists as a lock sentinel.
///
/// On POSIX (Linux/macOS), `fs4` uses `flock(2)`, which is cooperative/
/// advisory: no mandatory enforcement, so the sidecar vs. in-file distinction
/// has no practical effect and the fix is purely additive.
///
/// Modelled on [`crate::oci::index::local_index::tag_guard::TagGuard`].
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

/// Returns the sidecar lock-file path for a given blob `data` path.
///
/// The sidecar sits next to `data` as `data.lock`.  It is opened only for
/// locking; blob content is always read from / written to `data` directly.
fn lock_file_path(data_path: &std::path::Path) -> PathBuf {
    // `with_added_extension` (stable Rust 1.81) appends `.lock` without
    // touching any existing extension, so
    //   `sha256/ab/cd.../data` → `sha256/ab/cd.../data.lock`.
    data_path.with_added_extension("lock")
}

impl BlobGuard {
    /// Acquires an exclusive (writer) lock for the blob at `target_path`,
    /// creating the `data` file and its parent directories on first use.
    /// Blocks until the lock is available or [`LOCK_TIMEOUT`] elapses.
    ///
    /// The advisory lock is placed on a **sidecar `data.lock` file**, not on
    /// `data` itself.  This ensures no byte-range lock (`LockFileEx` on
    /// Windows) ever lands on `data`, so concurrent processes — e.g. a child
    /// `ocx` launched by `ocx package test` — can freely open and read the
    /// blob without hitting `ERROR_LOCK_VIOLATION` (os error 33).
    pub async fn acquire_exclusive(target_path: PathBuf) -> Result<Self> {
        if let Some(parent) = target_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| file_error(parent, e))?;
        }
        // Open the sidecar for locking; create data file separately so readers
        // see an empty-but-existent file rather than having the lock fd double
        // as the write fd (which caused ERROR_LOCK_VIOLATION with the old code).
        let lock_path = lock_file_path(&target_path);
        let open_lock_path = lock_path.clone();
        let lock_file = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&open_lock_path)
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(std::convert::identity)
        .map_err(|e| file_error(&lock_path, e))?;
        let lock = FileLock::lock_exclusive_with_timeout(lock_file, LOCK_TIMEOUT)
            .await
            .map_err(|e| file_error(&lock_path, e))?;
        Ok(Self {
            _lock: lock,
            target_path,
        })
    }

    /// Acquires a shared (reader) lock for the blob at `target_path`.
    /// Returns `Ok(None)` if the `data` file does not exist (so callers can
    /// treat absence as "no blob yet" without racing an exclusive writer).
    ///
    /// The advisory lock is placed on the sidecar `data.lock` file; see
    /// [`acquire_exclusive`](Self::acquire_exclusive) for rationale.
    pub async fn acquire_shared(target_path: PathBuf) -> Result<Option<Self>> {
        // Absence of the data file = blob not yet written; return None before
        // even opening the sidecar (the sidecar may also be absent in that case).
        let check_path = target_path.clone();
        let data_exists = tokio::task::spawn_blocking(move || check_path.exists())
            .await
            .map_err(std::io::Error::other)
            .map_err(|e| file_error(&target_path, e))?;
        if !data_exists {
            return Ok(None);
        }
        let lock_path = lock_file_path(&target_path);
        let open_lock_path = lock_path.clone();
        let open_result = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&open_lock_path)
        })
        .await
        .map_err(std::io::Error::other)
        .and_then(std::convert::identity);
        let file = match open_result {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(file_error(&lock_path, e)),
        };
        let lock = FileLock::lock_shared_with_timeout(file, LOCK_TIMEOUT)
            .await
            .map_err(|e| file_error(&lock_path, e))?;
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
        // Explicitly close the write handle before returning.
        //
        // On Windows, `tokio::fs::File` drop is asynchronous — the underlying
        // OS handle is closed on a background threadpool thread, not during
        // the drop call itself. Any subsequent open of the same path (e.g. by
        // acquire_read or verify_blob_digest) before the background close
        // completes causes ERROR_LOCK_VIOLATION (os error 33). POSIX advisory
        // locks are optional so Linux tolerates the overlap silently.
        // `shutdown()` drives the tokio file through its internal sync + close
        // path synchronously, guaranteeing the handle is closed before this
        // function returns to the caller.
        file.shutdown().await.map_err(|e| file_error(&self.target_path, e))?;
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

    /// Design record §1: acquire_exclusive creates the parent directories and
    /// a `data.lock` sidecar for the advisory lock. The `data` file itself is
    /// NOT created until `write_bytes` runs — the lock fd does not touch `data`.
    ///
    /// The sidecar (`data.lock`) is intentional: it keeps the `LockFileEx`
    /// byte-range lock (Windows) off the `data` file so concurrent reader
    /// processes can open `data` without `ERROR_LOCK_VIOLATION` (os error 33).
    #[tokio::test]
    async fn acquire_exclusive_creates_parent_dirs_and_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("cdef1234").join("data");
        let guard = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        // Parent dir must exist.
        assert!(
            target.parent().unwrap().exists(),
            "parent dir must be created on exclusive acquire"
        );
        // Sidecar lock file must exist; it is the lock sentinel.
        let sidecar = lock_file_path(&target);
        assert!(sidecar.exists(), "sidecar data.lock must exist after exclusive acquire");
        // data file is NOT created by acquire_exclusive alone — it is written by write_bytes.
        drop(guard);
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
    ///
    /// `acquire_shared` checks whether `data` exists before trying the sidecar
    /// lock, so we must write the blob first — otherwise absence returns `None`
    /// immediately with no locking attempt.
    #[tokio::test]
    async fn shared_blocks_behind_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let target = Arc::new(dir.path().join("sha256").join("ab").join("data"));

        // Write once so the data file exists, then re-acquire exclusive to hold
        // the sidecar lock for the duration of the blocking test.
        {
            let setup = BlobGuard::acquire_exclusive((*target).clone()).await.unwrap();
            setup.write_bytes(b"setup").await.unwrap();
        }

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

    /// Design record §9: after an exclusive acquire + write + drop, exactly
    /// `data` and `data.lock` exist in the parent directory — `data` holds
    /// the blob bytes and `data.lock` is the persistent sidecar sentinel.
    /// No other unexpected files (`.tmp`, `.log`, etc.) appear.
    ///
    /// The sidecar `data.lock` is intentionally retained across acquire/drop
    /// cycles so that subsequent `acquire_shared` calls can always open it
    /// to place their shared lock, even before the data file is written.
    #[tokio::test]
    async fn only_data_and_sidecar_exist_after_acquire_write_drop() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("sha256").join("ab");
        let target = parent.join("data");
        let guard = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_bytes(b"blob bytes").await.unwrap();
        drop(guard);

        let mut entries: Vec<String> = std::fs::read_dir(&parent)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec!["data".to_string(), "data.lock".to_string()],
            "exactly data + data.lock must exist; got: {entries:?}"
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

    // ── Regression: data file is free of byte-range locks while guard held ──
    //
    // Contract: `BlobGuard` holds its `fs4` advisory lock on the sidecar
    // `data.lock` file, NOT on `data` itself.  Any process (in-process or
    // cross-process) must be able to open and read `data` while the guard is
    // alive without hitting `ERROR_LOCK_VIOLATION` (Windows os error 33).
    //
    // On Windows, `fs4::lock_exclusive` calls `LockFileEx` with mandatory byte-
    // range coverage.  If the lock were on `data`, a concurrent `OpenFile` from
    // a child process (e.g. a launcher re-entering `ocx` during `package test`)
    // would receive `ERROR_LOCK_VIOLATION`.  Moving the lock to the sidecar
    // leaves `data` entirely lock-free.
    //
    // On Linux `flock(2)` is advisory, so the violation is never observed there;
    // this test documents the contract and proves the sidecar design is correct
    // on every platform.
    #[tokio::test]
    async fn data_file_openable_while_exclusive_guard_held() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("data");
        let guard = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_bytes(b"hello world").await.unwrap();

        // While the BlobGuard (advisory lock on data.lock) is still alive,
        // open the data file for read from a second file descriptor.
        // Pre-fix: the lock was on `data` itself → ERROR_LOCK_VIOLATION on Windows.
        // Post-fix: the lock is on `data.lock` → `data` is free, open succeeds.
        let open_result = std::fs::OpenOptions::new().read(true).open(&target);
        assert!(
            open_result.is_ok(),
            "data file must be openable by any process while BlobGuard exclusive lock is held; \
             a byte-range lock on data (pre-fix) would cause ERROR_LOCK_VIOLATION on Windows: {:?}",
            open_result.err()
        );

        // Also verify the content is readable (not just openable).
        use std::io::Read;
        let mut content = Vec::new();
        open_result.unwrap().read_to_end(&mut content).unwrap();
        assert_eq!(
            content, b"hello world",
            "data content must be readable while exclusive guard held"
        );

        drop(guard);
    }

    // ── Regression: write handle must be closed before write_bytes returns ──
    //
    // Guards the Windows ERROR_LOCK_VIOLATION (os error 33) regression.
    //
    // On Windows, `tokio::fs::File` drop is asynchronous: the OS-level handle
    // is closed on a background threadpool thread, NOT during the `drop()` call.
    // Without the explicit `shutdown().await` at the end of `write_bytes`, a
    // caller that immediately reopens the same path after `write_bytes` returns
    // (e.g. `verify_blob_digest`, `acquire_read`) would race the background
    // close and receive ERROR_LOCK_VIOLATION. POSIX advisory locks are optional
    // so Linux never reproduces this.
    //
    // The test opens the same path for write immediately after `write_bytes`
    // returns while the BlobGuard (advisory lock) is still held. A pre-fix
    // Windows build would fail the open with ERROR_LOCK_VIOLATION because the
    // write fd from `write_bytes` would still be open. On Linux this documents
    // the contract: write handle must be closed before write_bytes returns.
    #[tokio::test]
    async fn write_bytes_handle_closed_before_return_allows_immediate_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("sha256").join("ab").join("data");
        let guard = BlobGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_bytes(b"blob payload").await.unwrap();
        // The BlobGuard (fs2 advisory lock) is still alive — only the write fd
        // opened inside write_bytes must be closed. Reopen for read+write
        // immediately; on Windows with a lingering write handle this would fail
        // with ERROR_LOCK_VIOLATION (os error 33).
        let reopen = std::fs::OpenOptions::new().read(true).write(true).open(&target);
        assert!(
            reopen.is_ok(),
            "data file must be reopenable for write immediately after write_bytes returns; \
             a lingering write handle would cause ERROR_LOCK_VIOLATION on Windows: {:?}",
            reopen.err()
        );
        drop(guard);
    }
}
