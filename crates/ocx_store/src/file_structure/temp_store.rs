// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod acquire_result;
mod stale_entry;
mod temp_dir;

pub use acquire_result::TempAcquireResult;
pub use stale_entry::{StaleEntry, TempEntry};
pub use temp_dir::TempDir;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ocx_util::fs::LockedFile;

type Result<T> = std::result::Result<T, ocx_util::error::FileError>;

const LOCK_EXTENSION: &str = "lock";

/// Deterministic staging directories for in-progress downloads.
///
/// ```text
/// {root}/
///   {32-hex-char-hash}.lock   ← sibling lock file (outside the dir)
///   {32-hex-char-hash}/        ← temp content directory
///     metadata.json
///     content.{ext}
///     content/
///     manifest.json
/// ```
///
/// The `.lock` file sits beside its directory, never inside, so the directory can be renamed away while locked.
#[derive(Debug, Clone)]
pub struct TempStore {
    root: PathBuf,
}

impl TempStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Scratch root `ocx package test` materialises into; `ocx launcher exec` allow-lists it, so never re-spell the join.
    pub fn package_test_root(&self) -> PathBuf {
        self.root.join("test")
    }

    /// Scratch root `ocx patch test` composes into; allow-listed like [`Self::package_test_root`].
    pub fn patch_test_root(&self) -> PathBuf {
        self.root.join("patch-test")
    }

    /// # Errors
    ///
    /// `identifier` carries no digest.
    pub fn path(&self, identifier: &ocx_oci::PackageRef) -> std::result::Result<PathBuf, super::error::Error> {
        let digest = identifier
            .digest()
            .ok_or_else(|| super::error::Error::MissingDigest(identifier.to_string()))?;
        Ok(self.root.join(Self::dir_name(identifier, &digest)))
    }

    pub fn lock_path_for(dir: &Path) -> PathBuf {
        dir.with_extension(LOCK_EXTENSION)
    }

    /// Lists entries found as a `.lock` file, a directory, or both; empty if the root does not exist.
    pub fn list_all(&self) -> Result<Vec<TempEntry>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
        };

        let mut bases = HashSet::new();
        let mut lock_bases = HashSet::new();
        let mut dir_bases = HashSet::new();

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some(LOCK_EXTENSION) {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let base = stem.to_string();
                    lock_bases.insert(base.clone());
                    bases.insert(base);
                }
            } else if path.is_dir()
                && let Some(name) = path.file_name().and_then(|s| s.to_str())
            {
                let base = name.to_string();
                dir_bases.insert(base.clone());
                bases.insert(base);
            }
        }

        let result = bases
            .into_iter()
            .map(|base| TempEntry {
                dir: self.root.join(&base),
                has_lock_file: lock_bases.contains(&base),
            })
            .collect();

        Ok(result)
    }

    /// Non-blocking exclusive lock that clears leftovers on success; `None` when another process holds it.
    ///
    /// Sync on purpose: `stale_entries` runs outside a Tokio runtime.
    pub fn try_acquire(&self, path: &Path) -> Result<Option<TempAcquireResult>> {
        let lock_path = Self::lock_path_for(path);
        match LockedFile::try_exclusive_blocking(&lock_path) {
            Ok(Some(lock)) => Ok(Some(Self::finish_acquire(path, lock)?)),
            Ok(None) => Ok(None),
            Err(_) => Ok(None),
        }
    }

    /// Like [`try_acquire`](Self::try_acquire) but blocks until the lock is
    /// available or `timeout` expires.
    pub async fn acquire_with_timeout(&self, path: &Path, timeout: std::time::Duration) -> Result<TempAcquireResult> {
        let lock_path = Self::lock_path_for(path);
        let lock = LockedFile::open_exclusive_with_timeout(lock_path, timeout).await?;
        Self::finish_acquire(path, lock)
    }

    fn finish_acquire(dir_path: &Path, lock: LockedFile) -> Result<TempAcquireResult> {
        std::fs::create_dir_all(dir_path).map_err(|e| ocx_util::error::FileError::new(dir_path, e))?;
        let dir = TempDir {
            dir: dir_path.to_path_buf(),
        };
        let was_cleaned = dir.has_artifacts()?;
        if was_cleaned {
            dir.clear()?;
        }
        Ok(TempAcquireResult { lock, dir, was_cleaned })
    }

    /// Entries whose lock this call acquired, plus lock-less orphans; a held lock is skipped.
    pub fn stale_entries(&self) -> Result<Vec<StaleEntry>> {
        let entries = self.list_all()?;
        let mut result = Vec::new();
        for entry in entries {
            if entry.has_lock_file {
                if let Some(acquired) = self.try_acquire(&entry.dir)? {
                    result.push(StaleEntry::Locked(acquired));
                }
            } else {
                result.push(StaleEntry::Orphan(entry.dir));
            }
        }
        Ok(result)
    }

    /// Staging directory for a layer extraction; the NUL-delimited `__layer__` key cannot collide with a package's.
    pub fn layer_path(&self, registry: &str, digest: &ocx_oci::Digest) -> PathBuf {
        use sha2::{Digest as _, Sha256};
        let input = format!("{registry}\0__layer__\0{digest}");
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        self.root.join(&hash[..32])
    }

    // No repository in the key: installs of one digest from two repositories must share a lock, or `move_dir` clobbers `refs/`.
    fn dir_name(identifier: &ocx_oci::PackageRef, digest: &ocx_oci::Digest) -> String {
        use sha2::{Digest as _, Sha256};
        let input = format!("{}\0{}", identifier.registry(), digest);
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        hash[..32].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use ocx_util::fs::FileLock;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn id_with_digest() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("cmake", "example.com").clone_with_digest(digest())
    }

    fn id_tag_only() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("cmake", "example.com").clone_with_tag("3.28")
    }

    #[test]
    fn path_is_flat_32_char_hash() {
        let store = TempStore::new("/temp");
        let p = store.path(&id_with_digest()).unwrap();
        let dir_name = p.file_name().unwrap().to_str().unwrap();
        assert_eq!(dir_name.len(), 32);
        assert!(dir_name.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(p.parent().unwrap(), Path::new("/temp"));
    }

    #[test]
    fn path_is_deterministic() {
        let store = TempStore::new("/temp");
        let p1 = store.path(&id_with_digest()).unwrap();
        let p2 = store.path(&id_with_digest()).unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn path_differs_for_different_registries() {
        let store = TempStore::new("/temp");
        let id_a = ocx_oci::PackageRef::new_registry("cmake", "a.com").clone_with_digest(digest());
        let id_b = ocx_oci::PackageRef::new_registry("cmake", "b.com").clone_with_digest(digest());
        assert_ne!(store.path(&id_a).unwrap(), store.path(&id_b).unwrap());
    }

    #[test]
    fn layer_path_differs_from_identifier_path() {
        // A layer path must never collide with a package path derived from
        // the same digest — the keyspace separators (`__layer__` vs a real
        // repository name) prevent this at the hash-input level.
        let store = TempStore::new("/temp");
        let id = id_with_digest();
        let layer_path = store.layer_path(id.registry(), &digest());
        let pkg_path = store.path(&id).unwrap();
        assert_ne!(
            layer_path, pkg_path,
            "layer path must not collide with package path for the same digest"
        );
    }

    #[test]
    fn layer_path_is_deterministic() {
        let store = TempStore::new("/temp");
        let d = digest();
        let p1 = store.layer_path("example.com", &d);
        let p2 = store.layer_path("example.com", &d);
        assert_eq!(p1, p2);
    }

    #[test]
    fn layer_path_differs_across_registries() {
        let store = TempStore::new("/temp");
        let d = digest();
        assert_ne!(
            store.layer_path("a.com", &d),
            store.layer_path("b.com", &d),
            "layer path must separate registry keyspaces"
        );
    }

    #[test]
    fn path_requires_digest() {
        let store = TempStore::new("/temp");
        assert!(store.path(&id_tag_only()).is_err());
    }

    #[test]
    fn list_all_empty_when_root_absent() {
        let store = TempStore::new("/nonexistent/path");
        assert_eq!(store.list_all().unwrap().len(), 0);
    }

    #[test]
    fn list_all_finds_entry_with_lock_and_dir() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("abc12345678901234567890123456789");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(TempStore::lock_path_for(&temp), b"").unwrap();

        let store = TempStore::new(dir.path());
        let entries = store.list_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dir, temp);
        assert!(entries[0].has_lock_file);
    }

    #[test]
    fn list_all_finds_orphan_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("abc12345678901234567890123456789");
        std::fs::write(TempStore::lock_path_for(&temp), b"").unwrap();

        let store = TempStore::new(dir.path());
        let entries = store.list_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dir, temp);
        assert!(entries[0].has_lock_file);
    }

    #[test]
    fn list_all_finds_orphan_directory() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("abc12345678901234567890123456789");
        std::fs::create_dir_all(&temp).unwrap();

        let store = TempStore::new(dir.path());
        let entries = store.list_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].dir, temp);
        assert!(!entries[0].has_lock_file);
    }

    #[test]
    fn list_all_ignores_dirs_without_lock_or_content() {
        let dir = tempfile::tempdir().unwrap();
        let store = TempStore::new(dir.path());
        assert_eq!(store.list_all().unwrap().len(), 0);
    }

    #[test]
    fn try_acquire_returns_some_when_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("test_dir");
        let store = TempStore::new(dir.path());

        let result = store.try_acquire(&temp_path).unwrap();
        assert!(result.is_some());
        let acquired = result.unwrap();
        assert!(!acquired.was_cleaned);
        assert_eq!(acquired.dir.dir, temp_path);
        assert!(TempStore::lock_path_for(&temp_path).exists());
        assert!(temp_path.exists());
    }

    #[test]
    fn try_acquire_returns_none_when_locked() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("test_dir");
        let store = TempStore::new(dir.path());

        let first = store.try_acquire(&temp_path).unwrap().unwrap();
        let second = store.try_acquire(&temp_path).unwrap();
        assert!(second.is_none());
        drop(first);
    }

    #[test]
    fn try_acquire_cleans_stale_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("test_dir");
        std::fs::create_dir_all(&temp_path).unwrap();
        std::fs::write(temp_path.join("metadata.json"), b"{}").unwrap();
        std::fs::create_dir(temp_path.join("content")).unwrap();

        let store = TempStore::new(dir.path());
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();
        assert!(acquired.was_cleaned);
        assert!(!temp_path.join("metadata.json").exists());
        assert!(!temp_path.join("content").exists());
        assert!(TempStore::lock_path_for(&temp_path).exists());
        drop(acquired);
    }

    #[test]
    fn try_acquire_reports_not_cleaned_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("test_dir");

        let store = TempStore::new(dir.path());
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();
        assert!(!acquired.was_cleaned);
        drop(acquired);
    }

    #[test]
    fn drop_cleans_up_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("test_dir");
        let lock_path = TempStore::lock_path_for(&temp_path);

        let store = TempStore::new(dir.path());
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();
        assert!(lock_path.exists());
        drop(acquired);
        assert!(!lock_path.exists());
    }

    #[test]
    fn stale_entries_returns_unlocked_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("aaa");
        let b = dir.path().join("bbb");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(TempStore::lock_path_for(&a), b"").unwrap();
        std::fs::write(TempStore::lock_path_for(&b), b"").unwrap();

        let store = TempStore::new(dir.path());
        let stale = store.stale_entries().unwrap();
        assert_eq!(stale.len(), 2);
        assert!(stale.iter().all(|e| matches!(e, StaleEntry::Locked(_))));
    }

    #[test]
    fn stale_entries_skips_locked_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("aaa");
        let b = dir.path().join("bbb");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(TempStore::lock_path_for(&a), b"").unwrap();
        std::fs::write(TempStore::lock_path_for(&b), b"").unwrap();

        let store = TempStore::new(dir.path());
        let _lock = FileLock::try_exclusive(std::fs::File::open(TempStore::lock_path_for(&a)).unwrap())
            .unwrap()
            .unwrap();

        let stale = store.stale_entries().unwrap();
        assert_eq!(stale.len(), 1);
        match &stale[0] {
            StaleEntry::Locked(acquired) => assert_eq!(acquired.dir.dir, b),
            StaleEntry::Orphan(_) => panic!("expected Locked, got Orphan"),
        }
    }

    /// Codex Finding 6 (temp): a lock held by a live `create --extract` must
    /// SURVIVE a concurrent stale scan — both skipped from the sweep AND its
    /// lock file left on disk, so no later scan re-classes the scratch dir as an
    /// unlocked orphan and removes it out from under the running extraction.
    #[test]
    fn stale_scan_leaves_a_held_lock_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let held_dir = dir.path().join("create");
        std::fs::create_dir_all(&held_dir).unwrap();
        let lock_path = TempStore::lock_path_for(&held_dir);

        let store = TempStore::new(dir.path());
        // Hold the scratch lock exactly as `package_create` does.
        let _held = LockedFile::try_exclusive_blocking(&lock_path)
            .unwrap()
            .expect("hold the scratch lock");

        let stale = store.stale_entries().unwrap();
        assert!(
            stale.is_empty(),
            "a held scratch entry must not be reported stale (got {} entries)",
            stale.len()
        );
        assert!(lock_path.exists(), "the held lock file must survive the stale scan");
        assert!(held_dir.exists(), "the held scratch dir must survive the stale scan");
    }

    #[test]
    fn stale_entries_returns_orphan_directories() {
        let dir = tempfile::tempdir().unwrap();
        let orphan = dir.path().join("orphan_dir");
        std::fs::create_dir_all(&orphan).unwrap();

        let store = TempStore::new(dir.path());
        let stale = store.stale_entries().unwrap();
        assert_eq!(stale.len(), 1);
        match &stale[0] {
            StaleEntry::Orphan(path) => assert_eq!(*path, orphan),
            StaleEntry::Locked(_) => panic!("expected Orphan, got Locked"),
        }
    }

    /// Smoke test: proves `TempAcquireResult.lock` is a `LockedFile` — the
    /// lock field holds the sentinel `.lock` file path.
    #[test]
    fn temp_store_uses_locked_file_primitive() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("test_dir");
        let store = TempStore::new(dir.path());

        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();
        // `LockedFile::path()` returns the sentinel lock file path.
        assert_eq!(
            acquired.lock.path(),
            TempStore::lock_path_for(&temp_path).as_path(),
            "TempAcquireResult.lock must be a LockedFile over the sentinel .lock file"
        );
    }
    /// Acquiring a scratch dir that still holds a crashed run's artifacts
    /// hands back a CLEAN dir, and says so.
    ///
    /// Moved here from `oci/client.rs` by WP-14b: the subject is `TempStore`,
    /// and `oci/client.rs` may no longer name `crate::file_structure`
    /// (boundary `oci_client_does_not_import_workflow`).
    #[tokio::test]
    async fn temp_acquire_cleans_leftover_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let temp_root = dir.path().join("temp_root");
        let temp_path = temp_root.join("some_hash");

        // Simulate leftover artifacts from a crashed download.
        tokio::fs::create_dir_all(&temp_path).await.unwrap();
        tokio::fs::write(temp_path.join("metadata.json"), b"stale")
            .await
            .unwrap();
        tokio::fs::create_dir(temp_path.join("content")).await.unwrap();

        let store = TempStore::new(&temp_root);
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();

        // Verify artifacts were cleaned.
        assert!(acquired.was_cleaned);
        assert!(!temp_path.join("metadata.json").exists());
        assert!(!temp_path.join("content").exists());
        // Lock file is a sibling, not inside the dir.
        assert!(TempStore::lock_path_for(&temp_path).exists());
    }
}
