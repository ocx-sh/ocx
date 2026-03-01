use std::path::{Path, PathBuf};

use crate::{Error, Result, file_lock, oci};

const LOCK_FILE_NAME: &str = "install.lock";

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
///   {32-hex-char-hash}/
///     metadata.json
///     content.{ext}
///     content/
///     manifest.json
///     install.lock
/// ```
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
        let digest = identifier.digest().ok_or_else(|| {
            Error::UndefinedWithMessage(format!(
                "Temp store requires identifier with digest, got: {}",
                identifier
            ))
        })?;
        Ok(self.root.join(Self::dir_name(identifier, &digest)))
    }

    /// Lists all temp directories currently present.
    ///
    /// A temp directory is identified by containing an `install.lock` file.
    /// Returns an empty vec if the root does not exist.
    pub fn list_all(&self) -> Result<Vec<TempDir>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
        };
        let mut result = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join(LOCK_FILE_NAME).exists() {
                result.push(TempDir { dir: path });
            }
        }
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
    pub async fn acquire_with_timeout(
        &self,
        path: &Path,
        timeout: std::time::Duration,
    ) -> Result<TempAcquireResult> {
        let file = Self::prepare_lock_file(path)?;
        let lock_path = path.join(LOCK_FILE_NAME);
        let lock = file_lock::FileLock::lock_exclusive_with_timeout(file, timeout)
            .await
            .map_err(|e| Error::InternalFile(lock_path, e))?;
        Self::finish_acquire(path, lock)
    }

    /// Creates the temp directory and lock file, returning the file handle.
    fn prepare_lock_file(path: &Path) -> Result<std::fs::File> {
        std::fs::create_dir_all(path)
            .map_err(|e| Error::InternalFile(path.to_path_buf(), e))?;
        let lock_path = path.join(LOCK_FILE_NAME);
        std::fs::File::create(&lock_path)
            .map_err(|e| Error::InternalFile(lock_path, e))
    }

    /// Shared post-lock logic: check for and clean leftover artifacts.
    fn finish_acquire(path: &Path, lock: file_lock::FileLock) -> Result<TempAcquireResult> {
        let dir = TempDir { dir: path.to_path_buf() };
        let was_cleaned = dir.has_artifacts()?;
        if was_cleaned {
            dir.clear()?;
        }
        Ok(TempAcquireResult { dir, lock, was_cleaned })
    }

    /// Returns all stale temp dirs (those whose lock is not held).
    ///
    /// Each returned result holds the exclusive lock, preventing races with
    /// concurrent installs. Dirs that are actively locked are skipped.
    pub fn stale_dirs(&self) -> Result<Vec<TempAcquireResult>> {
        let dirs = self.list_all()?;
        let mut result = Vec::new();
        for temp_dir in dirs {
            if let Some(acquired) = self.try_acquire(&temp_dir.dir)? {
                result.push(acquired);
            }
        }
        Ok(result)
    }

    /// Hash of the full identifier into a flat 32-char hex directory name.
    fn dir_name(identifier: &oci::Identifier, digest: &oci::Digest) -> String {
        use sha2::{Digest as _, Sha256};
        let input = format!(
            "{}\0{}\0{}",
            identifier.registry(),
            identifier.reference.repository(),
            digest,
        );
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        hash[..32].to_string()
    }
}

/// Result of acquiring a temp directory via [`TempStore::try_acquire`] or
/// [`TempStore::acquire_with_timeout`].
///
/// Holds the exclusive lock for the directory's lifetime. When this value
/// is dropped, the lock is released.
pub struct TempAcquireResult {
    pub dir: TempDir,
    pub lock: file_lock::FileLock,
    /// `true` if the directory contained leftover artifacts that were cleaned.
    pub was_cleaned: bool,
}

/// Represents a single temp directory.
pub struct TempDir {
    pub dir: PathBuf,
}

impl TempDir {
    /// Returns `true` if the directory contains any files or subdirectories
    /// besides `install.lock`.
    fn has_artifacts(&self) -> Result<bool> {
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|e| Error::InternalFile(self.dir.clone(), e))?;
        for entry in entries.flatten() {
            if entry.file_name() != LOCK_FILE_NAME {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Removes all files and subdirectories except `install.lock`.
    fn clear(&self) -> Result<()> {
        let entries = std::fs::read_dir(&self.dir)
            .map_err(|e| Error::InternalFile(self.dir.clone(), e))?;
        for entry in entries.flatten() {
            if entry.file_name() == LOCK_FILE_NAME {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                std::fs::remove_dir_all(&path)
                    .map_err(|e| Error::InternalFile(path, e))?;
            } else {
                std::fs::remove_file(&path)
                    .map_err(|e| Error::InternalFile(path, e))?;
            }
        }
        Ok(())
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
    fn list_all_finds_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join("abc12345678901234567890123456789");
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("install.lock"), b"").unwrap();

        let store = TempStore::new(dir.path());
        let dirs = store.list_all().unwrap();
        assert_eq!(dirs.len(), 1);
        assert_eq!(dirs[0].dir, temp);
    }

    #[test]
    fn list_all_ignores_dirs_without_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("abc123")).unwrap();

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
        std::fs::write(temp_path.join(LOCK_FILE_NAME), b"").unwrap();
        std::fs::write(temp_path.join("metadata.json"), b"{}").unwrap();
        std::fs::create_dir(temp_path.join("content")).unwrap();

        let store = TempStore::new(dir.path());
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();
        assert!(acquired.was_cleaned);

        // install.lock should still exist, but artifacts should be gone.
        assert!(temp_path.join(LOCK_FILE_NAME).exists());
        assert!(!temp_path.join("metadata.json").exists());
        assert!(!temp_path.join("content").exists());
        drop(acquired);
    }

    #[test]
    fn try_acquire_reports_not_cleaned_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let temp_path = dir.path().join("test_dir");
        std::fs::create_dir_all(&temp_path).unwrap();
        std::fs::write(temp_path.join(LOCK_FILE_NAME), b"").unwrap();

        let store = TempStore::new(dir.path());
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();
        assert!(!acquired.was_cleaned);
        drop(acquired);
    }

    #[test]
    fn stale_dirs_returns_unlocked_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("aaa");
        let b = dir.path().join("bbb");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("install.lock"), b"").unwrap();
        std::fs::write(b.join("install.lock"), b"").unwrap();

        let store = TempStore::new(dir.path());
        let stale = store.stale_dirs().unwrap();
        assert_eq!(stale.len(), 2);
    }

    #[test]
    fn stale_dirs_skips_locked_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("aaa");
        let b = dir.path().join("bbb");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("install.lock"), b"").unwrap();
        std::fs::write(b.join("install.lock"), b"").unwrap();

        let store = TempStore::new(dir.path());

        // Lock one of them externally.
        let _lock = file_lock::FileLock::try_exclusive(
            std::fs::File::open(a.join("install.lock")).unwrap(),
        )
        .unwrap();

        let stale = store.stale_dirs().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].dir.dir, b);
    }
}
