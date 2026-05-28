// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::{Result, oci, utility::fs::LockedJsonFile};

use super::tag_lock::TagLock;

/// Max time we're willing to block waiting for another writer to release the
/// per-repo tag lock. Long enough to survive a concurrent `ocx index update`
/// against a slow registry, short enough that a stuck holder surfaces instead
/// of hanging the CLI forever.
const LOCK_TIMEOUT: Duration = Duration::from_secs(60);

/// Per-repository reader/writer guard over the local tag lock file.
///
/// Thin typed shim over [`LockedJsonFile<TagLock>`]. The underlying
/// `LockedJsonFile` acquires an `fs2` advisory lock — shared for reads,
/// exclusive for writes — directly on the canonical tag file itself.
/// No sidecar `.lock` file, no temp sibling, no atomic rename: writers lock
/// the tag file and update it in place (truncate + write + `sync_data`).
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
///
/// All reads and writes route through the lock-owning handle (via
/// `LockedJsonFile`), eliminating the F2 `ERROR_LOCK_VIOLATION` class
/// by construction (see ADR §Decision 1).
pub(super) struct TagGuard {
    inner: LockedJsonFile<TagLock>,
    target_path: PathBuf,
}

impl TagGuard {
    /// Acquires an exclusive (writer) lock on the tag file at `target_path`,
    /// creating the file and its parent directories on first use. Blocks
    /// until the lock is available or [`LOCK_TIMEOUT`] elapses.
    pub async fn acquire_exclusive(target_path: PathBuf) -> Result<Self> {
        let inner = LockedJsonFile::open_exclusive_with_timeout(target_path.clone(), LOCK_TIMEOUT).await?;
        Ok(Self { inner, target_path })
    }

    /// Acquires a shared (reader) lock on the tag file at `target_path`.
    ///
    /// Readers never create the file — if it does not exist, returns
    /// `Ok(None)` so callers can treat absence as "no tags yet" without
    /// racing an exclusive writer. Multiple readers may hold the shared
    /// lock concurrently; an exclusive writer waits until the last reader
    /// drops.
    pub async fn acquire_shared(target_path: PathBuf) -> Result<Option<Self>> {
        let maybe_inner = LockedJsonFile::open_shared_with_timeout(target_path.clone(), LOCK_TIMEOUT).await?;
        Ok(maybe_inner.map(|inner| Self { inner, target_path }))
    }

    /// Reads and parses the current tag file under the lock. Returns an
    /// empty map when the file is empty, was freshly created by an exclusive
    /// acquire and not yet written, **or** is unparseable.
    ///
    /// The unparseable case is the documented kill-9 recovery window: a
    /// writer killed mid-`write_disk` can leave the file truncated or
    /// corrupt. Treat that the same as "no tags yet" so the next chain walk
    /// or `ocx index update` can rewrite it cleanly, and log a warn so the
    /// recovery is observable.
    ///
    /// Reads go through the lock-owning handle (via `LockedJsonFile`). Opening
    /// a second handle against the locked range hits `ERROR_LOCK_VIOLATION` on
    /// Windows under an exclusive lock; same-process handles are not exempt.
    pub async fn read_disk(&mut self, identifier: &oci::Identifier) -> Result<HashMap<String, oci::Digest>> {
        match self.inner.read().await? {
            None => Ok(HashMap::new()),
            Some(tag_lock) => match tag_lock.into_tags(identifier, &self.target_path) {
                Ok(tags) => Ok(tags),
                Err(e) => {
                    crate::log::warn!(
                        "Tag file '{}' is unparseable ({e}) — treating as empty for recovery.",
                        self.target_path.display()
                    );
                    Ok(HashMap::new())
                }
            },
        }
    }

    /// Overwrites the tag file in place with a `TagLock` containing `tags`.
    /// Truncates the existing file (same inode), writes the full JSON, and
    /// `sync_data`s for durability. Concurrent writers are serialised by the
    /// exclusive lock held by the caller.
    ///
    /// Writes go through the lock-owning handle (via `LockedJsonFile`). A
    /// second open against the locked range hits `ERROR_LOCK_VIOLATION` on
    /// Windows even from the same process — `LockFileEx` ranges are per-handle,
    /// not per-process.
    pub async fn write_disk(
        &mut self,
        identifier: &oci::Identifier,
        tags: &HashMap<String, oci::Digest>,
    ) -> Result<()> {
        let tag_lock = TagLock::new(identifier, tags.clone());
        self.inner.write(&tag_lock).await
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

    #[tokio::test(flavor = "multi_thread")]
    async fn read_disk_returns_empty_when_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let mut guard = TagGuard::acquire_exclusive(target).await.unwrap();
        let tags = guard.read_disk(&make_id()).await.unwrap();
        assert!(tags.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_then_read_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let id = make_id();
        let tags = tags_with(&[("3.28", 'a'), ("latest", 'b')]);

        let mut guard = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_disk(&id, &tags).await.unwrap();
        let readback = guard.read_disk(&id).await.unwrap();
        assert_eq!(readback, tags);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn write_leaves_no_sidecar_or_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("ghcr.io");
        let target = parent.join("cmake.json");
        let mut guard = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
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

    /// Regression: on Windows `LockFileEx` blocks any other handle in the
    /// same process from writing into the locked byte range, even via
    /// `tokio::fs`. The original implementation re-opened the tag file in
    /// `write_disk` and hit `ERROR_LOCK_VIOLATION` (os error 33), surfacing
    /// as "uncategorized error". The current implementation routes writes
    /// through the lock-owning handle (`FileLock::file_mut`). This test pins
    /// the rewrite path: acquire exclusive, write twice in place under the
    /// same lock, then read back the latest content.
    #[tokio::test(flavor = "multi_thread")]
    async fn merge_under_lock_rewrites_in_place_through_lock_handle() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let id = make_id();

        let mut guard = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
        let initial = tags_with(&[("3.28", 'a')]);
        guard.write_disk(&id, &initial).await.unwrap();
        // Second rewrite under the same lock — must reuse the locked handle,
        // not open a second one.
        let updated = tags_with(&[("3.28", 'a'), ("3.29", 'b')]);
        guard.write_disk(&id, &updated).await.unwrap();
        let readback = guard.read_disk(&id).await.unwrap();
        assert_eq!(readback, updated);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shared_locks_can_coexist() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");

        // Establish the file with an exclusive acquire + write + drop so the
        // subsequent shared acquires find an existing file to lock.
        let mut writer = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
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

    /// Smoke test: proves the TagGuard shim works correctly over the
    /// `LockedJsonFile` internals — constructs a guard, writes a tag map,
    /// reads it back, asserts equality.
    #[tokio::test(flavor = "multi_thread")]
    async fn tag_guard_is_locked_json_file_shim() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("ghcr.io").join("cmake.json");
        let id = make_id();
        let tags = tags_with(&[("3.28", 'a'), ("3.29", 'b'), ("latest", 'b')]);

        let mut guard = TagGuard::acquire_exclusive(target.clone()).await.unwrap();
        guard.write_disk(&id, &tags).await.unwrap();
        let readback = guard.read_disk(&id).await.unwrap();
        assert_eq!(
            readback, tags,
            "TagGuard shim must round-trip tag maps via LockedJsonFile"
        );
    }
}
