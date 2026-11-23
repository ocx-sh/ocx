// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! One verified read order for every digest-addressed blob: blob directories, then a shared cache.
//!
//! A blob directory and the cache share one on-disk grammar, `<root>/<registry slug>/<shard>/data`, so a cache is
//! itself a valid blob directory. Every read is verified; a bad copy never ends a walk early.

pub mod export_marker;
pub mod markers;
pub mod stream;
pub mod transport;
pub mod writer;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ocx_util::prelude::StringExt as _;
use tokio::io::AsyncReadExt as _;

use crate::Digest;

pub use stream::{LookupOrigin, LookupStream, LookupWalk, WalkEnd};
pub use writer::{BlobCacheDir, BlobWriter, WriteOutcome};

/// Ceiling on referrer manifests followed per subject, across the whole chain.
pub const MAX_REFERRERS_PER_SUBJECT: usize = 256;

/// Ceiling on marker names read per root; a larger listing is truncated with one warning.
pub const MAX_MARKER_NAMES_PER_ROOT: usize = 4096;

/// Why a lookup read or write failed.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum LookupError {
    /// The bytes read do not hash to the digest they were looked up by.
    #[error("blob at '{}' does not hash to '{expected}' (got '{actual}')", path.display())]
    #[exit(
        DataError,
        slug = "digest_mismatch",
        summary = "Downloaded content does not hash to its expected digest"
    )]
    DigestMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    /// A metadata blob is larger than the cap its caller allows.
    #[error("blob at '{}' is over the {cap}-byte cap", path.display())]
    #[exit(DataError, slug = "invalid_manifest", summary = "A manifest is invalid")]
    OverCap { path: PathBuf, cap: u64 },
    /// Two registries, or two digests, share one on-disk name.
    #[error("'{}' already holds the content of '{existing}', not '{digest}'", path.display())]
    #[exit(
        DataError,
        slug = "shard_collision",
        summary = "Two digests share one blob directory"
    )]
    ShardCollision {
        path: PathBuf,
        existing: String,
        digest: String,
    },
    /// A read or write under a blob root failed.
    #[error("I/O error for '{}': {source}", path.display())]
    #[exit(IoError, slug = "lookup_io", summary = "Reading or writing a blob directory failed")]
    Io { path: PathBuf, source: std::io::Error },
}

/// `<root>/<registry slug>/<alg>/<2hex>/<30hex>`: the directory that holds one blob's `data`, `digest` and `referrers/`.
///
/// `registry` is the logical registry as written; every blob path in this crate and in the home store comes from here.
pub fn blob_dir_path(root: &Path, registry: &str, digest: &Digest) -> PathBuf {
    root.join(registry.to_relaxed_slug())
        .join(crate::cas_path::cas_shard_path(digest))
}

/// False for a registry whose directory name would escape the root or collide with the staging directory.
pub(crate) fn registry_slug_is_safe(registry: &str) -> bool {
    !matches!(registry.to_relaxed_slug().as_str(), "" | "." | ".." | writer::TMP_DIR)
}

/// The `data` file of one blob; see [`blob_dir_path`].
pub fn blob_data_path(root: &Path, registry: &str, digest: &Digest) -> PathBuf {
    blob_dir_path(root, registry, digest).join("data")
}

/// The blob directories and optional cache a digest-addressed read consults after the home and before the network.
///
/// Empty (no directory, no cache) behaves as no lookup at all, so every constructor and accessor is safe to call
/// from a path that never configured one.
#[derive(Debug, Clone, Default)]
pub struct BlobLookup {
    dirs: Vec<PathBuf>,
    cache: Option<BlobCacheDir>,
    /// Directories already reported missing, shared by every clone so each is warned about once.
    warned_missing: Arc<Mutex<HashSet<PathBuf>>>,
}

impl BlobLookup {
    pub fn new(dirs: Vec<PathBuf>, cache: Option<BlobCacheDir>) -> Self {
        Self {
            dirs,
            cache,
            warned_missing: Arc::default(),
        }
    }

    /// One warning per missing directory; a blob directory named by an environment variable may legitimately be gone.
    async fn warn_if_missing_dir(&self, dir: &Path) {
        if tokio::fs::metadata(dir).await.is_ok_and(|metadata| metadata.is_dir()) {
            return;
        }
        let first = self
            .warned_missing
            .lock()
            .ok()
            .is_some_and(|mut warned| warned.insert(dir.to_path_buf()));
        if first {
            log::warn!("blob directory '{}' does not exist; skipping it", dir.display());
        }
    }

    /// True when neither a directory nor a cache is configured.
    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty() && self.cache.is_none()
    }

    /// A copy with `dir` read first, ahead of every configured directory (import's `DIR/blobs`).
    #[must_use]
    pub fn with_leading_dir(&self, dir: PathBuf) -> Self {
        let mut dirs = Vec::with_capacity(self.dirs.len() + 1);
        dirs.push(dir);
        dirs.extend(self.dirs.iter().cloned());
        Self {
            dirs,
            cache: self.cache.clone(),
            warned_missing: Arc::clone(&self.warned_missing),
        }
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    pub fn cache(&self) -> Option<&BlobCacheDir> {
        self.cache.as_ref()
    }

    /// Starts the ordered walk over every source for one blob; the caller verifies each stream it is handed.
    pub fn walk(&self, registry: &str, digest: &Digest) -> LookupWalk<'_> {
        LookupWalk::new(self, registry, digest)
    }

    /// Opens the first regular `data` file in lookup order, unverified; `None` when no source holds the blob.
    ///
    /// # Errors
    ///
    /// A source that exists but cannot be opened.
    pub async fn open_stream(&self, registry: &str, digest: &Digest) -> Result<Option<LookupStream>, LookupError> {
        self.walk(registry, digest).next_source().await
    }

    /// Reads a metadata blob in full, verified against `digest`, trying every source before giving up.
    ///
    /// A bad copy in a blob directory is remembered; when no source holds a clean copy the verdict is
    /// [`LookupError::DigestMismatch`]. A bad cache entry is removed and counts as a miss.
    ///
    /// # Errors
    ///
    /// [`LookupError::OverCap`] over `cap`, [`LookupError::DigestMismatch`] as above, else I/O.
    pub async fn read_metadata(
        &self,
        registry: &str,
        digest: &Digest,
        cap: u64,
    ) -> Result<Option<Vec<u8>>, LookupError> {
        let mut walk = self.walk(registry, digest);
        let mut over_cap = None;
        while let Some(stream) = walk.next_source().await? {
            let origin = stream.origin;
            let path = match origin {
                stream::LookupOrigin::Dir(position) => self.dirs.get(position).map(PathBuf::as_path),
                stream::LookupOrigin::Cache => self.cache.as_ref().map(BlobCacheDir::root),
            }
            .map_or_else(PathBuf::new, |root| blob_data_path(root, registry, digest));
            let mut bytes = Vec::new();
            // `take(cap + 1)`, not the size alone: a file can grow between stat and read.
            stream
                .reader
                .take(cap.saturating_add(1))
                .read_to_end(&mut bytes)
                .await
                .map_err(|source| LookupError::Io {
                    path: path.clone(),
                    source,
                })?;
            if bytes.len() as u64 > cap {
                // Skipped, not rejected: an honest big blob must survive in the cache. A later source may fit.
                over_cap.get_or_insert(LookupError::OverCap { path, cap });
                continue;
            }
            // Up to the cap of bytes: hashed off the async workers.
            let wanted = digest.clone();
            let (verified, bytes) =
                tokio::task::spawn_blocking(move || (wanted.algorithm().hash(&bytes) == wanted, bytes))
                    .await
                    .map_err(|join| LookupError::Io {
                        path: path.clone(),
                        source: std::io::Error::other(join),
                    })?;
            if verified {
                log::debug!("blob {digest} read from {origin:?}");
                return Ok(Some(bytes));
            }
            walk.reject(origin).await;
        }
        walk.exhausted()?;
        over_cap.map_or(Ok(None), Err)
    }

    /// The referrer markers of `subject`: sorted within each source, sources in lookup order, a digest kept at its
    /// first source. Reads at most [`MAX_MARKER_NAMES_PER_ROOT`] names per source.
    ///
    /// # Errors
    ///
    /// I/O failure reading a marker directory.
    pub async fn referrers(&self, registry: &str, subject: &Digest) -> Result<markers::ReferrerMarkers, LookupError> {
        let mut found = markers::ReferrerMarkers::default();
        let mut seen = HashSet::new();
        let roots = self
            .dirs
            .iter()
            .map(PathBuf::as_path)
            .chain(self.cache.as_ref().map(BlobCacheDir::root));
        for root in roots {
            if self.cache.as_ref().is_some_and(|cache| cache.root() == root) && !BlobCacheDir::holds_registry(registry)
            {
                continue;
            }
            let (root, registry, subject) = (root.to_path_buf(), registry.to_string(), subject.clone());
            let read = tokio::task::spawn_blocking(move || {
                markers::read_markers(&root, &registry, &subject, MAX_MARKER_NAMES_PER_ROOT)
            })
            .await
            .map_err(|join| LookupError::Io {
                path: PathBuf::new(),
                source: std::io::Error::other(join),
            })??;
            found.complete |= read.complete;
            found
                .digests
                .extend(read.digests.into_iter().filter(|digest| seen.insert(digest.clone())));
        }
        Ok(found)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use crate::{Algorithm, Digest};

    pub(crate) fn sha256(bytes: &[u8]) -> Digest {
        Algorithm::Sha256.hash(bytes)
    }

    /// Writes `bytes` as the blob `registry`/`digest` under `root`, as a blob directory or cache holds it.
    pub(crate) fn put_blob(root: &Path, registry: &str, bytes: &[u8]) -> Digest {
        let digest = sha256(bytes);
        put_blob_as(root, registry, &digest, bytes);
        digest
    }

    /// Writes `bytes` under `digest`'s path whether or not they hash to it, for a corrupt copy.
    pub(crate) fn put_blob_as(root: &Path, registry: &str, digest: &Digest, bytes: &[u8]) {
        let dir = super::blob_dir_path(root, registry, digest);
        std::fs::create_dir_all(&dir).expect("blob dir");
        std::fs::write(dir.join("data"), bytes).expect("blob data");
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{put_blob, put_blob_as, sha256};
    use super::*;

    const REG: &str = "registry.test";

    fn lookup(dirs: &[&Path], cache: Option<&Path>) -> BlobLookup {
        BlobLookup::new(
            dirs.iter().map(|d| d.to_path_buf()).collect(),
            cache.map(|c| BlobCacheDir::at(c.to_path_buf())),
        )
    }

    #[test]
    fn data_path_is_slug_then_shard_then_data() {
        let digest = sha256(b"x");
        let path = blob_data_path(Path::new("/r"), "localhost:5000", &digest);
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(rendered.starts_with("/r/localhost_5000/sha256/"), "{rendered}");
        assert!(rendered.ends_with("/data"), "{rendered}");
    }

    #[test]
    fn empty_lookup_is_empty_and_with_leading_dir_is_not() {
        let empty = BlobLookup::default();
        assert!(empty.is_empty());
        assert!(!empty.with_leading_dir(PathBuf::from("/d")).is_empty());
    }

    #[tokio::test]
    async fn with_leading_dir_first() {
        let leading = tempfile::tempdir().unwrap();
        let configured = tempfile::tempdir().unwrap();
        let digest = put_blob(configured.path(), REG, b"configured");
        put_blob_as(leading.path(), REG, &digest, b"leading");

        let base = lookup(&[configured.path()], None);
        let layered = base.with_leading_dir(leading.path().to_path_buf());

        assert_eq!(layered.dirs()[0], leading.path());
        let stream = layered.open_stream(REG, &digest).await.unwrap().expect("a hit");
        assert_eq!(stream.origin, LookupOrigin::Dir(0));
        assert_eq!(stream.size, b"leading".len() as u64);
    }

    #[tokio::test]
    async fn dir_order_wins() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let digest = put_blob(first.path(), REG, b"same");
        put_blob(second.path(), REG, b"same");
        let found = lookup(&[first.path(), second.path()], None)
            .open_stream(REG, &digest)
            .await
            .unwrap()
            .expect("a hit");
        assert_eq!(found.origin, LookupOrigin::Dir(0));
    }

    #[tokio::test]
    async fn bad_dir_copy_continues() {
        let bad = tempfile::tempdir().unwrap();
        let good = tempfile::tempdir().unwrap();
        let digest = put_blob(good.path(), REG, b"payload");
        put_blob_as(bad.path(), REG, &digest, b"tampered");

        let found = lookup(&[bad.path(), good.path()], None)
            .read_metadata(REG, &digest, 1024)
            .await
            .unwrap();
        assert_eq!(found.as_deref(), Some(&b"payload"[..]));
    }

    #[tokio::test]
    async fn bad_dir_copy_verdict_is_65_at_end_of_sources() {
        let bad = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        put_blob_as(bad.path(), REG, &digest, b"tampered");

        let error = lookup(&[bad.path()], None)
            .read_metadata(REG, &digest, 1024)
            .await
            .expect_err("a bad copy with no clean one");
        assert!(matches!(error, LookupError::DigestMismatch { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn short_dir_read_is_65() {
        let dir = tempfile::tempdir().unwrap();
        let digest = sha256(b"complete payload");
        put_blob_as(dir.path(), REG, &digest, b"complete");
        let lookup = lookup(&[dir.path()], None);

        let mut walk = lookup.walk(REG, &digest);
        let stream = walk.next_source().await.unwrap().expect("the short copy is offered");
        walk.reject(stream.origin).await;
        assert!(walk.next_source().await.unwrap().is_none());
        assert!(matches!(walk.exhausted(), Err(LookupError::DigestMismatch { .. })));
    }

    #[tokio::test]
    async fn cache_only_mismatch_is_miss() {
        let cache = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        put_blob_as(cache.path(), REG, &digest, b"tampered");
        let lookup = lookup(&[], Some(cache.path()));

        assert!(lookup.read_metadata(REG, &digest, 1024).await.unwrap().is_none());
        assert!(
            !blob_data_path(cache.path(), REG, &digest).exists(),
            "the bad cache entry is removed"
        );
    }

    #[tokio::test]
    async fn cache_delete_failure_warns() {
        let cache = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        // A directory where `data` should be: `remove` cannot unlink it, and the walk must go on.
        std::fs::create_dir_all(blob_data_path(cache.path(), REG, &digest)).unwrap();
        let lookup = lookup(&[], Some(cache.path()));

        let mut walk = lookup.walk(REG, &digest);
        walk.reject(LookupOrigin::Cache).await;
        let end = walk.exhausted().expect("a cache failure is never the 65 verdict");
        assert_eq!(end.discarded_cache_entries, 1);
    }

    #[tokio::test]
    async fn absent_at_import_is_export_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let digest = sha256(b"absent");
        let lookup = lookup(&[dir.path()], None);

        let mut walk = lookup.walk(REG, &digest);
        assert!(walk.next_source().await.unwrap().is_none());
        let end = walk.exhausted().expect("nothing held, nothing corrupt");
        assert_eq!(end.discarded_cache_entries, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fifo_refused_on_handle() {
        let dir = tempfile::tempdir().unwrap();
        let digest = sha256(b"fifo");
        let blob_dir = blob_dir_path(dir.path(), REG, &digest);
        std::fs::create_dir_all(&blob_dir).unwrap();
        ocx_test_support::fifo::mkfifo(&blob_dir.join("data"));

        // A hang here means the handle check and `O_NONBLOCK` are gone; the timeout turns it into a failure.
        let found = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            lookup(&[dir.path()], None).open_stream(REG, &digest),
        )
        .await
        .expect("open_stream must not block on a FIFO")
        .unwrap();
        assert!(found.is_none(), "a FIFO is absent");
    }

    #[tokio::test]
    async fn over_cap_refused() {
        let dir = tempfile::tempdir().unwrap();
        let digest = put_blob(dir.path(), REG, &[7u8; 64]);
        let error = lookup(&[dir.path()], None)
            .read_metadata(REG, &digest, 16)
            .await
            .expect_err("over the cap");
        assert!(matches!(error, LookupError::OverCap { cap: 16, .. }), "{error:?}");
    }

    #[tokio::test]
    async fn over_cap_copy_does_not_stop_a_good_one() {
        let big = tempfile::tempdir().unwrap();
        let good = tempfile::tempdir().unwrap();
        let digest = put_blob(good.path(), REG, b"payload");
        put_blob_as(big.path(), REG, &digest, &[7u8; 64]);

        let found = lookup(&[big.path(), good.path()], None)
            .read_metadata(REG, &digest, 16)
            .await
            .unwrap();
        assert_eq!(found.as_deref(), Some(&b"payload"[..]));
    }

    #[tokio::test]
    async fn reject_spares_a_replacement() {
        let cache = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        put_blob_as(cache.path(), REG, &digest, b"tampered");
        let lookup = lookup(&[], Some(cache.path()));

        let mut walk = lookup.walk(REG, &digest);
        let stream = walk.next_source().await.unwrap().expect("the bad copy is offered");
        // Another process replaces the bad copy with a verified one before the rejection lands.
        let data = blob_data_path(cache.path(), REG, &digest);
        std::fs::remove_file(&data).unwrap();
        std::fs::write(&data, b"payload").unwrap();
        walk.reject(stream.origin).await;

        assert_eq!(std::fs::read(&data).unwrap(), b"payload", "the replacement survives");
    }

    #[tokio::test]
    async fn tmp_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let digest = put_blob(dir.path(), ".tmp", b"staged");
        let found = lookup(&[dir.path()], None).open_stream(".tmp", &digest).await.unwrap();
        assert!(
            found.is_none(),
            "`.tmp/` holds half-written files and is never a source"
        );
    }

    #[tokio::test]
    async fn missing_env_dir_skipped() {
        let missing = tempfile::tempdir().unwrap().path().join("gone");
        let present = tempfile::tempdir().unwrap();
        let digest = put_blob(present.path(), REG, b"here");
        let found = lookup(&[missing.as_path(), present.path()], None)
            .open_stream(REG, &digest)
            .await
            .unwrap()
            .expect("the second directory answers");
        assert_eq!(found.origin, LookupOrigin::Dir(1));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cache_with_symlinked_slug_dir_is_a_miss() {
        let outside = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let digest = put_blob(outside.path(), REG, b"payload");
        std::os::unix::fs::symlink(outside.path().join(REG), cache.path().join(REG)).unwrap();
        let lookup = lookup(&[], Some(cache.path()));

        let mut walk = lookup.walk(REG, &digest);
        assert!(
            walk.next_source().await.unwrap().is_none(),
            "a link below the cache root is a miss"
        );
        assert_eq!(walk.exhausted().unwrap().discarded_cache_entries, 0);
        assert!(
            blob_data_path(outside.path(), REG, &digest).exists(),
            "the link target is untouched"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn blob_dir_data_symlink_to_a_regular_file_is_served() {
        let elsewhere = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let digest = sha256(b"payload");
        let target = elsewhere.path().join("payload.bin");
        std::fs::write(&target, b"payload").unwrap();
        let blob_dir = blob_dir_path(dir.path(), REG, &digest);
        std::fs::create_dir_all(&blob_dir).unwrap();
        std::os::unix::fs::symlink(&target, blob_dir.join("data")).unwrap();

        let found = lookup(&[dir.path()], None)
            .read_metadata(REG, &digest, 1024)
            .await
            .unwrap();
        assert_eq!(found.as_deref(), Some(&b"payload"[..]));
    }
}
