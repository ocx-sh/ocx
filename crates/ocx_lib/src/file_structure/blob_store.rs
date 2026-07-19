// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::{Result, log, oci};

/// Represents a single content-addressed blob directory within the blob store.
///
/// A blob directory has a fixed layout:
/// - `data`   -- the raw blob content (single file)
/// - `digest` -- full digest string for recovery
pub struct BlobDir {
    /// The root directory of this blob (parent of `data`, `digest`).
    pub dir: PathBuf,
}

impl BlobDir {
    /// Path to the raw blob data file.
    pub fn data(&self) -> PathBuf {
        self.dir.join("data")
    }

    /// Path to the digest marker file.
    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }
}

/// Manages the content-addressed blob store on the local filesystem.
///
/// All blobs are stored under a single `root` directory, sharded by
/// registry and digest (via [`super::cas_path::cas_shard_path`]) to avoid
/// filesystem limits in any single directory.
///
/// Layout:
/// ```text
/// {root}/
///   {registry_slug}/
///     {algorithm}/        e.g. sha256
///       {2hex}/           first 2 hex chars of digest
///         {30hex}/        next 30 hex chars
///           data
///           digest
/// ```
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the blob store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the blob directory path for the given registry and digest.
    pub fn path(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.root
            .join(super::slugify(registry))
            .join(super::cas_path::cas_shard_path(digest))
    }

    /// Returns the `data` file path for the given registry and digest.
    ///
    /// # Invariant
    /// All writes to this path go through `BlobStore::write_blob`, which uses
    /// `tempfile::NamedTempFile` + atomic rename. The content-addressed
    /// invariant (same digest → same bytes) makes concurrent writers safe:
    /// each writes byte-equivalent content, and the rename is idempotent.
    pub(crate) fn data(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.path(registry, digest).join("data")
    }

    /// Returns the `digest` file path for the given registry and digest.
    pub fn digest_file(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        self.path(registry, digest).join(super::cas_path::DIGEST_FILENAME)
    }

    /// Idempotently write `bytes` to the CAS `data` path for `(registry, digest)`.
    ///
    /// Caller MUST have verified `digest == sha256(bytes)` upstream — this
    /// function does not re-hash.
    ///
    /// Behavior:
    /// 1. If the CAS `data` path already exists **and is non-empty**, return
    ///    `Ok(())` (idempotent). A zero-byte file is treated as absent — it
    ///    is a crash artifact from a kill-9 during a previous write and must
    ///    be overwritten via the tempfile+rename path.
    /// 2. Otherwise: `tempfile::NamedTempFile::new_in(cas_parent_dir)` →
    ///    `write_all(bytes)` → `sync_data` → `persist(cas_path)`.
    /// 3. On Windows `ERROR_SHARING_VIOLATION` (32) or `ERROR_ACCESS_DENIED`
    ///    (5) — caused by concurrent non-sharing readers or AV scanning —
    ///    retry the persist with exponential backoff (3 retries:
    ///    100ms / 400ms / 800ms with ±25% jitter). After exhausting retries,
    ///    re-check the CAS path; if it now exists, return `Ok(())`
    ///    (the writer that won the race is byte-equivalent by content-addressing).
    ///    Matches rattler's `rename_with_retry` precedent for the same hazard.
    ///
    /// # Errors
    ///
    /// Returns `crate::Error::InternalFile(cas_path, io::Error)` on disk
    /// failure after retry exhaustion.
    ///
    /// No `BlobGuard` is acquired — there is no advisory lock on the `data`
    /// file. F1 (cross-process read of locked blob data) cannot recur because
    /// the lock has been removed entirely, not relocated.
    pub(crate) async fn write_blob(&self, registry: &str, digest: &oci::Digest, bytes: &[u8]) -> crate::Result<()> {
        let target = self.data(registry, digest);
        // Check-first: idempotent fast path (content-addressed invariant).
        // A zero-byte file is a crash artifact (kill-9 recovery window); treat
        // it as absent so the tempfile+rename path overwrites it correctly.
        if tokio::fs::metadata(&target).await.map(|m| m.len() > 0).unwrap_or(false) {
            return Ok(());
        }
        self.persist_bytes(&target, bytes).await
    }

    /// Unconditionally replaces the CAS `data` file for `(registry, digest)`
    /// via the same tempfile + atomic-rename publish [`Self::write_blob`]
    /// uses, but WITHOUT its check-first existence fast path: a fresh
    /// tempfile is always written and renamed over whatever is at `target` —
    /// corrupt bytes or nothing — so there is no absence window and no
    /// separate removal step that could itself fail and leave the corrupt
    /// bytes in place.
    ///
    /// Caller MUST have verified `digest == sha256(bytes)` upstream, same
    /// contract as [`Self::write_blob`].
    ///
    /// Exists for a caller that has already discovered a present-but-corrupt
    /// entry (a digest-verify failure on read) and wants to heal it in place:
    /// `write_blob`'s check-first fast path would short-circuit on the
    /// still-present tampered file and never actually replace it (this was a
    /// real regression caught in review — a remove-then-`write_blob` two-step
    /// left a failure window where a removal error left the corrupt bytes in
    /// place while `write_blob`'s own fast path would then re-accept them). A
    /// single atomic replace has no such window.
    pub(crate) async fn replace_blob(&self, registry: &str, digest: &oci::Digest, bytes: &[u8]) -> crate::Result<()> {
        let target = self.data(registry, digest);
        self.persist_bytes(&target, bytes).await
    }

    /// Shared tempfile + atomic-rename publish body for [`Self::write_blob`]
    /// (behind its check-first fast path) and [`Self::replace_blob`]
    /// (unconditional) — one write body, no copy-pasted logic. Increments
    /// [`WRITE_BLOB_CALL_COUNT`] (test-only) once per genuine write attempt,
    /// whichever public entry point triggered it.
    async fn persist_bytes(&self, target: &Path, bytes: &[u8]) -> crate::Result<()> {
        #[cfg(test)]
        WRITE_BLOB_CALL_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let parent = target
            .parent()
            .ok_or_else(|| crate::error::file_error(target, std::io::Error::other("blob data path has no parent")))?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|e| crate::error::file_error(&parent, e))?;
        let bytes_owned = bytes.to_vec();
        let target_for_blocking = target.to_path_buf();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut tmp = tempfile::NamedTempFile::new_in(&parent)?;
            std::io::Write::write_all(&mut tmp, &bytes_owned)?;
            tmp.as_file().sync_data()?;
            match crate::utility::fs::persist_temp_file(tmp, &target_for_blocking) {
                Ok(()) => Ok(()),
                // A failed persist is success ONLY when the target now holds the
                // exact bytes we meant to publish. `write_blob` reaches here past
                // its check-first fast path (target absent/zero-byte) and
                // `replace_blob` reaches it unconditionally to heal a
                // present-but-corrupt entry — in the heal case the corrupt bytes
                // are still on disk after a failed rename, so a bare `exists()`
                // check would report success while leaving corruption behind.
                // Re-read and byte-compare against the content we wrote: a
                // genuine concurrent CAS writer published byte-equivalent content
                // (same digest ⇒ same bytes) and matches; corrupt or absent
                // content propagates the original error. Byte-compare rather than
                // re-hash because `persist_bytes` has the intended bytes in hand
                // but no digest — the check is equivalent under content
                // addressing and needs no extra parameter. This re-check is valid
                // ONLY because the path is content-addressed; it is deliberately
                // NOT baked into the generic `persist_temp_file`.
                Err(err) => match std::fs::read(&target_for_blocking) {
                    Ok(current) if current == bytes_owned => Ok(()),
                    _ => Err(err),
                },
            }
        })
        .await
        .map_err(|join_err| crate::error::file_error(target, std::io::Error::other(join_err)))?
        .map_err(|io_err| crate::error::file_error(target, io_err))?;
        Ok(())
    }

    /// Read the full blob bytes from the CAS `data` path.
    ///
    /// Returns `Ok(None)` if the path does not exist. No lock taken — the blob
    /// is immutable by digest, race-free.
    ///
    /// # Trust
    ///
    /// Bytes are returned without re-hashing against `digest`. Integrity rests
    /// on the write-side contract: [`Self::write_blob`] requires the caller to
    /// have verified `digest == sha256(bytes)` upstream. A future code path
    /// that writes to the CAS without that pre-verification would silently
    /// break the integrity guarantee this method depends on. Any new writer
    /// MUST honor the upstream-verification contract; this is enforced by
    /// convention and code review (the audit in
    /// `.claude/artifacts/discovery_file_lock_unification.md` §"Blob-store
    /// content-addressed audit" validates today's three production writers).
    pub(crate) async fn read_blob(&self, registry: &str, digest: &oci::Digest) -> crate::Result<Option<Vec<u8>>> {
        let target = self.data(registry, digest);
        match tokio::fs::read(&target).await {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(crate::error::file_error(&target, e)),
        }
    }

    /// Removes the CAS `data` file for `(registry, digest)`, tolerating an
    /// already-absent target (`Ok(())` when the file does not exist).
    ///
    /// A present-but-corrupt entry (bytes that no longer hash to `digest`) must
    /// be removed before a re-fetch: [`Self::write_blob`]'s check-first fast
    /// path would otherwise re-accept the corrupt file untouched. Callers that
    /// discover corruption on a digest-verified read heal by remove-then-refetch
    /// — the chain's leaf recovery ([`ChainedIndex::recover_absent_leaf`](crate::oci::index))
    /// and the install-staging shortcut (`stage_and_link_chain_blobs`).
    ///
    /// Removes only the `data` file — the write path stores no sibling `digest`
    /// file, and the next write repopulates `data` in place; an orphaned shard
    /// directory is reaped by GC.
    pub(crate) async fn remove_blob(&self, registry: &str, digest: &oci::Digest) -> crate::Result<()> {
        let target = self.data(registry, digest);
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(crate::error::file_error(&target, e)),
        }
    }

    /// Lists all blob directories currently present in the store.
    ///
    /// A blob directory is identified by the presence of a `data` child file.
    /// Returns an empty vec if the store root does not exist yet.
    pub async fn list_all(&self) -> Result<Vec<BlobDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        crate::utility::fs::DirWalker::new(self.root.clone(), classify_blob_dir)
            .max_depth(MAX_WALK_DEPTH)
            .walk()
            .await
    }
}

/// Registry directory + CAS shard depth (algorithm/prefix/suffix).
const MAX_WALK_DEPTH: usize = 1 + super::cas_path::CAS_SHARD_DEPTH;

/// Directory names that are part of the blob layout and must not be
/// recursed into during the store walk.
const BLOB_SKIP_NAMES: &[&str] = &[];

/// Classifies a directory for the generic walker.
///
/// - If a `data` file exists and the path is valid CAS → [`WalkDecision::leaf`]
///   with a [`BlobDir`].
/// - If `data` exists but the path is invalid → [`WalkDecision::skip`].
/// - Otherwise → [`WalkDecision::descend`].
fn classify_blob_dir(dir: &Path, _depth: usize) -> crate::utility::fs::WalkDecision<BlobDir> {
    if dir.join("data").is_file() {
        if super::cas_path::is_valid_cas_path(dir) {
            return crate::utility::fs::WalkDecision::leaf(BlobDir { dir: dir.to_path_buf() });
        }
        log::warn!("Skipping data file in dir not matching CAS layout: {}", dir.display());
        return crate::utility::fs::WalkDecision::skip();
    }
    crate::utility::fs::WalkDecision::descend_skip(BLOB_SKIP_NAMES)
}

/// Test-only call counter on `BlobStore::write_blob`.
///
/// Used by `PullCoordinator::stage_blob_bytes` coalescing tests to assert
/// the singleflight dedup actually fires (the leader executes exactly once,
/// waiters short-circuit). Without this instrumentation, content-addressing
/// alone makes "both calls return Ok" a passing condition even when no
/// dedup happens — masking a regression that would otherwise cost a
/// duplicate download per concurrent caller.
///
/// The counter is process-global. Tests that read it for assertion MUST
/// acquire [`WRITE_BLOB_TEST_LOCK`] to serialise against sibling tests
/// that also call `write_blob` (e.g. the Windows-cfg `write_blob_retries_*`
/// tests). `cargo test` parallelises within a single test binary, so the
/// static would otherwise be racy.
#[cfg(test)]
pub(crate) static WRITE_BLOB_CALL_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test-binary-local serializer for the `WRITE_BLOB_CALL_COUNT` static. Held
/// for the duration of any test that reads the counter as an assertion, or
/// that calls `write_blob` while such a test might be running.
///
/// Uses `tokio::sync::Mutex` (not `std::sync::Mutex`) because every consumer
/// is a `#[tokio::test]` that awaits blob I/O while holding the guard. A
/// std-sync guard across `.await` is a Block-tier anti-pattern per
/// `quality-rust.md` (`clippy::await_holding_lock`).
#[cfg(test)]
pub(crate) static WRITE_BLOB_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    // ---- path construction ------------------------------------------------

    #[test]
    fn path_flat_registry() {
        let store = BlobStore::new("/blobs");
        let p = store.path("example.com", &digest());
        let expected = Path::new("/blobs")
            .join("example.com")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn path_port_containing_registry_is_slugified() {
        let store = BlobStore::new("/blobs");
        let p = store.path("localhost:5000", &digest());
        let expected = Path::new("/blobs")
            .join("localhost_5000")
            .join("sha256")
            .join("43")
            .join("567c07f1a6b07b5e8dc052108c9d4c");
        assert_eq!(p, expected);
    }

    #[test]
    fn data_is_path_join_data() {
        let store = BlobStore::new("/blobs");
        let p = store.data("example.com", &digest());
        assert_eq!(p.file_name().unwrap(), "data");
        assert_eq!(p.parent().unwrap(), store.path("example.com", &digest()));
    }

    #[test]
    fn digest_file_is_path_join_digest() {
        let store = BlobStore::new("/blobs");
        let p = store.digest_file("example.com", &digest());
        assert_eq!(p.file_name().unwrap(), "digest");
        assert_eq!(p.parent().unwrap(), store.path("example.com", &digest()));
    }

    // ---- BlobDir accessors ------------------------------------------------

    #[test]
    fn blob_dir_accessors() {
        let blob = BlobDir {
            dir: PathBuf::from("/blobs/reg/sha256/43/rest"),
        };
        assert_eq!(blob.data(), PathBuf::from("/blobs/reg/sha256/43/rest/data"));
        assert_eq!(blob.digest_file(), PathBuf::from("/blobs/reg/sha256/43/rest/digest"));
    }

    // ---- list_all ---------------------------------------------------------

    #[tokio::test]
    async fn list_all_returns_empty_when_root_absent() {
        let store = BlobStore::new("/nonexistent/path/that/does/not/exist");
        assert_eq!(store.list_all().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_all_finds_single_blob() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::fs::write(blob_dir.join("data"), b"blob content").unwrap();

        let store = BlobStore::new(dir.path());
        let blobs = store.list_all().await.unwrap();
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].data(), blob_dir.join("data"));
    }

    #[tokio::test]
    async fn list_all_skips_invalid_cas_path() {
        let dir = tempfile::tempdir().unwrap();

        // Valid blob
        let valid = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&valid).unwrap();
        std::fs::write(valid.join("data"), b"valid").unwrap();

        // Invalid: wrong algorithm
        let invalid = dir.path().join("example.com/md5/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&invalid).unwrap();
        std::fs::write(invalid.join("data"), b"invalid").unwrap();

        let store = BlobStore::new(dir.path());
        let blobs = store.list_all().await.unwrap();
        assert_eq!(blobs.len(), 1);
    }

    #[tokio::test]
    async fn list_all_skips_directory_without_data_file() {
        let dir = tempfile::tempdir().unwrap();
        // Directory with correct structure but no `data` file
        let no_data = dir.path().join("example.com/sha256/43/567c07f1a6b07b5e8dc052108c9d4c");
        std::fs::create_dir_all(&no_data).unwrap();
        std::fs::write(no_data.join("digest"), b"sha256:43567c...").unwrap();

        let store = BlobStore::new(dir.path());
        let blobs = store.list_all().await.unwrap();
        assert_eq!(blobs.len(), 0);
    }

    // ---- write_blob / read_blob -------------------------------------------

    /// write_blob is idempotent: calling it again when the target already
    /// exists returns Ok(()) and does not overwrite the existing file.
    #[tokio::test]
    async fn write_blob_idempotent_when_target_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let d = digest();

        let first_bytes = b"first write";
        store.write_blob("example.com", &d, first_bytes).await.unwrap();
        assert_eq!(std::fs::read(store.data("example.com", &d)).unwrap(), first_bytes);

        // A second write with different bytes must be a no-op: the target
        // already exists, so the check-first path returns Ok(()).
        store.write_blob("example.com", &d, b"second write").await.unwrap();
        assert_eq!(
            std::fs::read(store.data("example.com", &d)).unwrap(),
            first_bytes,
            "write_blob must be idempotent when the target data file already exists"
        );
    }

    /// N concurrent writers on the same digest all succeed and the final
    /// file is non-empty (atomic rename is idempotent under concurrent writes).
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_write_blob_on_same_digest_atomic_rename_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(BlobStore::new(dir.path()));
        let d = digest();
        let payload = b"content-addressed payload";

        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let store_clone = store.clone();
            let d_clone = d.clone();
            tasks.spawn(async move {
                store_clone.write_blob("example.com", &d_clone, payload).await.unwrap();
            });
        }
        while let Some(joined) = tasks.join_next().await {
            joined.expect("task panicked");
        }

        // All writers produced byte-equivalent content; the file must exist
        // and contain the correct bytes.
        let on_disk = std::fs::read(store.data("example.com", &d)).unwrap();
        assert_eq!(
            on_disk, payload,
            "concurrent write_blob must leave the correct content on disk"
        );
    }

    /// read_blob returns None when the blob has not been written yet.
    #[tokio::test]
    async fn read_blob_returns_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let result = store.read_blob("example.com", &digest()).await.unwrap();
        assert!(
            result.is_none(),
            "read_blob must return None when the data file is absent"
        );
    }

    /// write_blob then read_blob round-trips the bytes.
    #[tokio::test]
    async fn write_then_read_blob_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let d = digest();
        let payload = b"round-trip payload";

        store.write_blob("example.com", &d, payload).await.unwrap();
        let read_back = store.read_blob("example.com", &d).await.unwrap().unwrap();
        assert_eq!(read_back, payload);
    }

    /// write_blob overwrites a zero-byte crash artifact — a zero-byte `data`
    /// file from a previous kill-9 must not be treated as a valid completed
    /// write (content-addressed invariant only holds for non-empty files).
    #[tokio::test]
    async fn write_blob_overwrites_zero_byte_crash_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let d = digest();

        // Simulate a kill-9 mid-write: create a zero-byte data file.
        let data_path = store.data("example.com", &d);
        std::fs::create_dir_all(data_path.parent().unwrap()).unwrap();
        std::fs::write(&data_path, b"").unwrap();
        assert_eq!(std::fs::metadata(&data_path).unwrap().len(), 0);

        // write_blob must overwrite the zero-byte file.
        let payload = b"recovered content";
        store.write_blob("example.com", &d, payload).await.unwrap();
        let on_disk = std::fs::read(&data_path).unwrap();
        assert_eq!(
            on_disk, payload,
            "write_blob must overwrite a zero-byte crash artifact with the correct content"
        );
    }

    /// No data.lock sidecar file is created alongside the data file — the
    /// tempfile+rename write path leaves no sentinel.
    #[tokio::test]
    async fn write_blob_leaves_no_lock_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let d = digest();

        store.write_blob("example.com", &d, b"payload").await.unwrap();

        let data_dir = store.path("example.com", &d);
        let lock_file = data_dir.join("data.lock");
        assert!(
            !lock_file.exists(),
            "write_blob must not create a data.lock sidecar; BlobGuard has been deleted"
        );
    }

    /// `replace_blob` heals a present-but-corrupt entry, but a FAILED persist
    /// must NOT report success while the corrupt bytes stay on disk (CWE-345).
    /// The target `data` path is pre-seeded as a non-empty directory: renaming
    /// the tempfile over it fails deterministically and the re-read byte-compare
    /// sees no match — the old `Err(_) if target.exists() => Ok(())` arm would
    /// have masked this heal failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn replace_blob_propagates_a_failed_heal_persist() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let d = digest();

        // Pre-seed the exact `data` path as a NON-EMPTY directory so a file→dir
        // rename fails while the target still "exists".
        let data_path = store.data("example.com", &d);
        std::fs::create_dir_all(&data_path).unwrap();
        std::fs::write(data_path.join("occupant"), b"blocks the rename").unwrap();

        let result = store.replace_blob("example.com", &d, b"healed content").await;
        assert!(
            result.is_err(),
            "a failed heal persist must propagate an error, not report success; got {result:?}"
        );
        assert!(
            data_path.is_dir(),
            "the corrupt target must be left untouched, never silently 'healed'"
        );
    }

    // ── Windows cfg-gated retry behavior tests ──────────────────────────────
    //
    // These tests verify the Windows-specific retry-with-backoff logic in
    // `utility::fs::persist_temp_file` (reached via `write_blob`). They are
    // gated on `#[cfg(target_os = "windows")]` and compile but do not run on
    // Linux/macOS.

    #[cfg(target_os = "windows")]
    #[tokio::test(flavor = "multi_thread")]
    async fn write_blob_retries_on_sharing_violation_then_succeeds() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // which reads `WRITE_BLOB_CALL_COUNT` as an assertion. See the
        // `WRITE_BLOB_TEST_LOCK` doc-comment.
        let _serialize = super::WRITE_BLOB_TEST_LOCK.lock().await;
        // Open the eventual CAS path with std::fs::File::open (no FILE_SHARE_DELETE)
        // so that a rename over it will trigger ERROR_SHARING_VIOLATION (32).
        // Then race a write_blob. The retry-with-backoff loop should eventually
        // succeed once we close our blocking handle.
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(BlobStore::new(dir.path()));
        let d = digest();
        let target = store.data("example.com", &d);

        // Pre-create the parent directory and an empty data file, then re-open
        // read-only. `OpenOptions::create(true)` requires `write` or `append`
        // access on Windows, so the create + reopen split keeps the blocker
        // handle read-only (no FILE_SHARE_DELETE) as the test intends.
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let _ = std::fs::File::create(&target).unwrap();
        let blocker = std::fs::OpenOptions::new().read(true).open(&target).unwrap();

        let store_clone = store.clone();
        let d_clone = d.clone();
        let handle = tokio::task::spawn(async move {
            // We expect this to succeed via the retry loop once the blocker
            // is dropped below.
            store_clone.write_blob("example.com", &d_clone, b"retry-payload").await
        });

        // Hold the blocking handle briefly, then release.
        std::thread::sleep(std::time::Duration::from_millis(150));
        drop(blocker);

        // write_blob should eventually succeed.
        handle.await.unwrap().unwrap();
    }

    #[cfg(target_os = "windows")]
    #[tokio::test(flavor = "multi_thread")]
    async fn write_blob_returns_ok_when_target_exists_after_retry_exhaustion() {
        let _serialize = super::WRITE_BLOB_TEST_LOCK.lock().await;
        // Simulate the scenario where retry exhaustion occurs but the target
        // file is created by a concurrent writer. The idempotent re-check
        // should return Ok(()).
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path());
        let d = digest();
        let target = store.data("example.com", &d);

        // Pre-create the target file so the existence check succeeds on the
        // very first call (the check-first fast path in write_blob).
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"pre-existing content").unwrap();

        // write_blob must return Ok(()) without overwriting.
        store.write_blob("example.com", &d, b"newer bytes").await.unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"pre-existing content");
    }

    #[cfg(target_os = "windows")]
    #[tokio::test(flavor = "multi_thread")]
    async fn launcher_child_can_read_blob_data_during_concurrent_other_write() {
        let _serialize = super::WRITE_BLOB_TEST_LOCK.lock().await;
        // F1 cannot-recur proof: spawn a writer for digest A and simultaneously
        // open blob A's data file (if already written) with bare File::open.
        // There is no LockFileEx on the data file after BlobGuard removal, so
        // the reader must never see ERROR_LOCK_VIOLATION (os error 33).
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(BlobStore::new(dir.path()));
        let d = digest();

        // Pre-write so there is a data file to read.
        store.write_blob("example.com", &d, b"f1-proof content").await.unwrap();

        let target = store.data("example.com", &d);
        let read_result = std::fs::File::open(&target);
        assert!(
            read_result.is_ok(),
            "F1 cannot-recur: data file must be openable without ERROR_LOCK_VIOLATION; \
             no LockFileEx lock exists on the data file after BlobGuard removal: {:?}",
            read_result.err()
        );

        use std::io::Read;
        let mut content = Vec::new();
        read_result.unwrap().read_to_end(&mut content).unwrap();
        assert_eq!(content, b"f1-proof content");
    }
}
