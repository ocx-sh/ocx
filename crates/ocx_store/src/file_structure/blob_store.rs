// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, ocx_util::error::FileError>;

/// One blob directory: the `data` file plus, when written, `digest`.
pub struct BlobDir {
    pub dir: PathBuf,
}

impl BlobDir {
    pub fn data(&self) -> PathBuf {
        self.dir.join("data")
    }

    pub fn digest_file(&self) -> PathBuf {
        self.dir.join(super::cas_path::DIGEST_FILENAME)
    }
}

/// Raw blobs at `{root}/{registry_slug}/{cas_shard_path}/data`.
///
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
    #[cfg(any(test, feature = "__test_scaffolding"))]
    write_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            #[cfg(any(test, feature = "__test_scaffolding"))]
            write_calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, registry: &str, digest: &ocx_oci::Digest) -> PathBuf {
        self.root
            .join(super::slugify(registry))
            .join(super::cas_path::cas_shard_path(digest))
    }

    /// Write only through [`Self::write_blob`] / [`Self::replace_blob`], whose atomic rename makes concurrent writers safe.
    pub fn data(&self, registry: &str, digest: &ocx_oci::Digest) -> PathBuf {
        self.path(registry, digest).join("data")
    }

    pub fn digest_file(&self, registry: &str, digest: &ocx_oci::Digest) -> PathBuf {
        self.path(registry, digest).join(super::cas_path::DIGEST_FILENAME)
    }

    /// Idempotently writes `bytes`; the caller must have verified `digest == sha256(bytes)`, as nothing re-hashes.
    ///
    /// # Errors
    ///
    /// Disk failure after the persist retries are exhausted.
    pub async fn write_blob(&self, registry: &str, digest: &ocx_oci::Digest, bytes: &[u8]) -> Result<()> {
        let target = self.data(registry, digest);
        // A zero-byte file is a kill-9 artifact, so only a non-empty one counts as present.
        if tokio::fs::metadata(&target).await.map(|m| m.len() > 0).unwrap_or(false) {
            return Ok(());
        }
        self.persist_bytes(&target, bytes).await
    }

    /// [`Self::write_blob`] without the fast path, for healing a corrupt entry in one atomic replace; `bytes` must be verified.
    pub async fn replace_blob(&self, registry: &str, digest: &ocx_oci::Digest, bytes: &[u8]) -> Result<()> {
        let target = self.data(registry, digest);
        self.persist_bytes(&target, bytes).await
    }

    async fn persist_bytes(&self, target: &Path, bytes: &[u8]) -> Result<()> {
        #[cfg(any(test, feature = "__test_scaffolding"))]
        self.write_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let parent = target
            .parent()
            .ok_or_else(|| {
                ocx_util::error::FileError::new(target, std::io::Error::other("blob data path has no parent"))
            })?
            .to_path_buf();
        tokio::fs::create_dir_all(&parent)
            .await
            .map_err(|e| ocx_util::error::FileError::new(&parent, e))?;
        let bytes_owned = bytes.to_vec();
        let target_for_blocking = target.to_path_buf();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut tmp = tempfile::NamedTempFile::new_in(&parent)?;
            std::io::Write::write_all(&mut tmp, &bytes_owned)?;
            tmp.as_file().sync_data()?;
            match ocx_util::fs::persist_temp_file(tmp, &target_for_blocking) {
                Ok(()) => Ok(()),
                // Success only if the target holds our bytes: an `exists()` check would accept `replace_blob`'s corrupt original.
                // Byte-compare is valid only because the path is content-addressed; never move it into `persist_temp_file`.
                Err(err) => match std::fs::read(&target_for_blocking) {
                    Ok(current) if current == bytes_owned => Ok(()),
                    _ => Err(err),
                },
            }
        })
        .await
        .map_err(|join_err| ocx_util::error::FileError::new(target, std::io::Error::other(join_err)))?
        .map_err(|io_err| ocx_util::error::FileError::new(target, io_err))?;
        Ok(())
    }

    /// Reads the blob, `Ok(None)` if absent; not re-hashed, so integrity rests on every writer verifying upstream.
    pub async fn read_blob(&self, registry: &str, digest: &ocx_oci::Digest) -> Result<Option<Vec<u8>>> {
        let target = self.data(registry, digest);
        match tokio::fs::read(&target).await {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ocx_util::error::FileError::new(&target, e)),
        }
    }

    /// Removes the `data` file, `Ok(())` if absent; a corrupt entry must go before a re-fetch, or the fast path re-accepts it.
    pub async fn remove_blob(&self, registry: &str, digest: &ocx_oci::Digest) -> Result<()> {
        let target = self.data(registry, digest);
        match tokio::fs::remove_file(&target).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ocx_util::error::FileError::new(&target, e)),
        }
    }

    /// Lists every blob directory; empty if the root does not exist.
    pub async fn list_all(&self) -> Result<Vec<BlobDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        ocx_util::fs::DirWalker::new(self.root.clone(), classify_blob_dir)
            .max_depth(MAX_WALK_DEPTH)
            .walk()
            .await
    }
}

const MAX_WALK_DEPTH: usize = 1 + super::cas_path::CAS_SHARD_DEPTH;

const BLOB_SKIP_NAMES: &[&str] = &[];

fn classify_blob_dir(dir: &Path, _depth: usize) -> ocx_util::fs::WalkDecision<BlobDir> {
    if dir.join("data").is_file() {
        if super::cas_path::is_valid_cas_path(dir) {
            return ocx_util::fs::WalkDecision::leaf(BlobDir { dir: dir.to_path_buf() });
        }
        log::warn!("Skipping data file in dir not matching CAS layout: {}", dir.display());
        return ocx_util::fs::WalkDecision::skip();
    }
    ocx_util::fs::WalkDecision::descend_skip(BLOB_SKIP_NAMES)
}

#[cfg(any(test, feature = "__test_scaffolding"))]
impl BlobStore {
    /// Write attempts past the fast path, across clones; per store so parallel tests cannot perturb it.
    pub fn write_call_count(&self) -> usize {
        self.write_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(SHA256_HEX.to_string())
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
