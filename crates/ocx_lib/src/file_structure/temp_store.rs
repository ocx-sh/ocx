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

use crate::{Error, Result, file_lock, oci};

const LOCK_EXTENSION: &str = "lock";

/// Deterministic temporary directory for in-progress downloads.
///
/// Each identifier with a digest maps to a unique, flat directory under the
/// root. The directory name is a truncated SHA-256 hash of the full
/// identifier (registry + repository + digest), making paths both
/// deterministic and compact.
///
/// Layout:
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
/// The lock file lives as a sibling of the temp directory so the directory
/// can be atomically moved (renamed) while the lock is still held.
///
/// Both the `install` task and the `clean` command use [`TempStore::try_acquire`]
/// to lock and prepare temp directories. A successful acquire clears any
/// leftover artifacts from a previous interrupted download.
#[derive(Debug, Clone)]
pub struct TempStore {
    root: PathBuf,
}

impl TempStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the temp store.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the temp directory path for the given identifier.
    ///
    /// Requires the identifier to carry a digest; returns an error otherwise.
    pub fn path(&self, identifier: &oci::Identifier) -> Result<PathBuf> {
        let digest = identifier
            .digest()
            .ok_or_else(|| super::error::Error::MissingDigest(identifier.to_string()))?;
        Ok(self.root.join(Self::dir_name(identifier, &digest)))
    }

    /// Returns the sibling lock file path for a given temp directory path.
    pub fn lock_path_for(dir: &Path) -> PathBuf {
        dir.with_extension(LOCK_EXTENSION)
    }

    /// Lists all temp entries currently present.
    ///
    /// Discovers entries from both `.lock` files and directories in the root.
    /// Returns an empty vec if the root does not exist.
    pub fn list_all(&self) -> Result<Vec<TempEntry>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
        };

        // Collect unique base names from both .lock files and directories.
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

    /// Tries to exclusively lock a temp directory (non-blocking).
    ///
    /// If the lock is acquired, any leftover artifacts (from a previous
    /// interrupted download) are cleared. Returns `None` if the lock is
    /// held by another process.
    ///
    /// Used by both `install` (to prepare a clean temp dir before downloading)
    /// and `clean` (to detect and remove stale dirs).
    pub fn try_acquire(&self, path: &Path) -> Result<Option<TempAcquireResult>> {
        let file = Self::prepare_lock_file(path)?;
        match file_lock::FileLock::try_exclusive(file) {
            Ok(lock) => Ok(Some(Self::finish_acquire(path, lock)?)),
            Err(_) => Ok(None),
        }
    }

    /// Like [`try_acquire`](Self::try_acquire) but blocks until the lock is
    /// available or `timeout` expires.
    pub async fn acquire_with_timeout(&self, path: &Path, timeout: std::time::Duration) -> Result<TempAcquireResult> {
        let file = Self::prepare_lock_file(path)?;
        let lock_path = Self::lock_path_for(path);
        let lock = file_lock::FileLock::lock_exclusive_with_timeout(file, timeout)
            .await
            .map_err(|e| Error::InternalFile(lock_path, e))?;
        Self::finish_acquire(path, lock)
    }

    /// Creates the sibling lock file and returns the file handle.
    ///
    /// Ensures the temp root exists but does NOT create the temp directory
    /// itself — that happens in [`finish_acquire`] after the lock is held.
    fn prepare_lock_file(dir_path: &Path) -> Result<std::fs::File> {
        if let Some(parent) = dir_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::InternalFile(parent.to_path_buf(), e))?;
        }
        let lock_path = Self::lock_path_for(dir_path);
        std::fs::File::create(&lock_path).map_err(|e| Error::InternalFile(lock_path, e))
    }

    /// Shared post-lock logic: create the content directory, check for and
    /// clean leftover artifacts.
    fn finish_acquire(dir_path: &Path, lock: file_lock::FileLock) -> Result<TempAcquireResult> {
        std::fs::create_dir_all(dir_path).map_err(|e| Error::InternalFile(dir_path.to_path_buf(), e))?;
        let dir = TempDir {
            dir: dir_path.to_path_buf(),
        };
        let was_cleaned = dir.has_artifacts()?;
        if was_cleaned {
            dir.clear()?;
        }
        Ok(TempAcquireResult { lock, dir, was_cleaned })
    }

    /// Returns all stale temp entries (those whose lock is not held, plus orphans).
    ///
    /// For entries with a lock file, acquires the lock to prevent races with
    /// concurrent installs. Dirs that are actively locked are skipped.
    /// Directories without a lock file are returned as orphans.
    pub fn stale_entries(&self) -> Result<Vec<StaleEntry>> {
        let entries = self.list_all()?;
        let mut result = Vec::new();
        for entry in entries {
            if entry.has_lock_file {
                if let Some(acquired) = self.try_acquire(&entry.dir)? {
                    result.push(StaleEntry::Locked(acquired));
                }
                // else: another process holds the lock, skip
            } else {
                // No lock file → orphan directory, safe to clean directly.
                result.push(StaleEntry::Orphan(entry.dir));
            }
        }
        Ok(result)
    }

    /// Returns the temp directory path for a layer extraction.
    ///
    /// Layers are not repository-scoped at the CAS level — two packages in
    /// different repositories may share the same layer digest. The middle
    /// component is therefore a fixed `__layer__` sentinel rather than a
    /// repository name. Null-byte delimiters keep the keyspace disjoint from
    /// any legitimate repository path (which cannot contain NUL).
    pub fn layer_path(&self, registry: &str, digest: &oci::Digest) -> PathBuf {
        use sha2::{Digest as _, Sha256};
        let input = format!("{registry}\0__layer__\0{digest}");
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        self.root.join(&hash[..32])
    }

    /// Hash of the CAS identity into a flat 32-char hex directory name.
    ///
    /// Keyed by `registry + digest` only (no repository) so the temp lock
    /// matches the final `PackageStore::path` which is also repo-agnostic.
    /// Two processes installing the same digest from different repositories
    /// must serialize on the same lock to avoid the late finisher clobbering
    /// the early finisher's `refs/` back-references via `move_dir`.
    fn dir_name(identifier: &oci::Identifier, digest: &oci::Digest) -> String {
        use sha2::{Digest as _, Sha256};
        let input = format!("{}\0{}", identifier.registry(), digest);
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        hash[..32].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    const SHA256_HEX: &str = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";

    fn digest() -> oci::Digest {
        oci::Digest::Sha256(SHA256_HEX.to_string())
    }

    fn id_with_digest() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_digest(digest())
    }

    fn id_tag_only() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "example.com").clone_with_tag("3.28")
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
        let id_a = oci::Identifier::new_registry("cmake", "a.com").clone_with_digest(digest());
        let id_b = oci::Identifier::new_registry("cmake", "b.com").clone_with_digest(digest());
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
        let _lock =
            file_lock::FileLock::try_exclusive(std::fs::File::open(TempStore::lock_path_for(&a)).unwrap()).unwrap();

        let stale = store.stale_entries().unwrap();
        assert_eq!(stale.len(), 1);
        match &stale[0] {
            StaleEntry::Locked(acquired) => assert_eq!(acquired.dir.dir, b),
            StaleEntry::Orphan(_) => panic!("expected Locked, got Orphan"),
        }
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
}
