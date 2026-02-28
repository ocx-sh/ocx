use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{file_structure::FileStructure, log, symlink, Error, Result};

/// Manages forward symlinks and their back-references inside the object store.
///
/// Every forward symlink (candidate, current, or user-defined link) is paired
/// with a back-reference symlink stored in `{object}/refs/`.  The back-ref
/// points from the object back to the forward symlink, enabling:
///
/// - **Safe removal**: an object can be deleted only when its `refs/` dir is empty.
/// - **Garbage collection**: scanning `refs/` for stale or broken entries
///   identifies objects that are no longer reachable.
///
/// Back-reference names are derived by hashing the canonical forward-symlink
/// path (16 hex chars of SHA-256).  The symlink's target IS the forward path,
/// so `readlink refs/<hash>` is the human-readable audit entry.
///
/// All operations are atomic at the individual symlink level.  No locking is
/// required: symlink creation and deletion are atomic POSIX operations, and each
/// ref entry maps to a unique path (its hash), so concurrent calls do not race.
pub struct ReferenceManager {
    file_structure: FileStructure,
}

impl ReferenceManager {
    pub fn new(file_structure: FileStructure) -> Self {
        Self { file_structure }
    }

    /// Derives the back-reference name for `forward_path`.
    ///
    /// The name is the first 16 hex characters of the SHA-256 hash of the
    /// forward path bytes — unique and fixed-length regardless of path length.
    fn ref_name(forward_path: &Path) -> String {
        let mut hasher = Sha256::new();
        hasher.update(forward_path.as_os_str().as_encoded_bytes());
        hex::encode(&hasher.finalize()[..8])
    }

    /// Returns the back-reference path inside the object that `content_path`
    /// belongs to, keyed by `forward_path`.
    fn back_ref_path(&self, content_path: &Path, forward_path: &Path) -> Result<PathBuf> {
        Ok(self.file_structure.objects.refs_dir_for_content(content_path)?.join(Self::ref_name(forward_path)))
    }

    /// Creates or updates a forward symlink from `forward_path` to `content_path`,
    /// maintaining the corresponding back-reference.
    ///
    /// If `forward_path` already exists and points to a different object, the old
    /// back-reference is removed before the new one is created (re-link).  If it
    /// already points to `content_path`, the call is a no-op.
    pub fn link(&self, forward_path: &Path, content_path: &Path) -> Result<()> {
        if forward_path.is_symlink() {
            if let Ok(current_target) = std::fs::read_link(forward_path) {
                if current_target == content_path {
                    log::trace!(
                        "link '{}' → '{}': already up to date, skipping.",
                        forward_path.display(),
                        content_path.display(),
                    );
                    return Ok(());
                }
                log::debug!(
                    "Re-linking '{}': '{}' → '{}'.",
                    forward_path.display(),
                    current_target.display(),
                    content_path.display(),
                );
                // Remove the old back-ref; tolerate failure (stale ref or GC'd object).
                if let Ok(old_ref) = self.back_ref_path(&current_target, forward_path) {
                    log::trace!("Removing old back-ref '{}'.", old_ref.display());
                    let _ = symlink::remove(&old_ref);
                }
            }
        } else {
            log::debug!(
                "Linking '{}' → '{}'.",
                forward_path.display(),
                content_path.display(),
            );
        }

        symlink::update(content_path, forward_path)?;

        let ref_path = self.back_ref_path(content_path, forward_path)?;
        if let Some(parent) = ref_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::InternalFile(parent.to_path_buf(), e))?;
        }
        // Idempotent: recreate if a stale back-ref already exists at this path.
        if ref_path.is_symlink() {
            log::trace!("Replacing stale back-ref '{}'.", ref_path.display());
            symlink::remove(&ref_path)?;
        }
        symlink::create(forward_path, &ref_path)?;
        log::trace!("Created back-ref '{}' → '{}'.", ref_path.display(), forward_path.display());

        Ok(())
    }

    /// Removes the forward symlink at `forward_path` and its back-reference.
    ///
    /// If `forward_path` does not exist the call is a no-op.  Failure to remove
    /// a stale back-reference is tolerated — the broken ref will be reported by
    /// [`broken_refs`] and can be cleaned up separately.
    pub fn unlink(&self, forward_path: &Path) -> Result<()> {
        if !forward_path.is_symlink() {
            log::trace!("unlink '{}': path is not a symlink, skipping.", forward_path.display());
            return Ok(());
        }

        if let Ok(target) = std::fs::read_link(forward_path) {
            log::debug!("Unlinking '{}' (was → '{}').", forward_path.display(), target.display());
            if let Ok(ref_path) = self.back_ref_path(&target, forward_path) {
                log::trace!("Removing back-ref '{}'.", ref_path.display());
                let _ = symlink::remove(&ref_path);
            }
        }

        symlink::remove(forward_path)
    }

    /// Returns the paths of all broken back-references found in the object store.
    ///
    /// A back-reference is broken when:
    /// - Its target (the forward path) no longer exists, or
    /// - The forward path no longer points to the expected content (the object
    ///   was re-linked without going through [`ReferenceManager`]).
    ///
    /// Uses [`ObjectStore::list_all`] to enumerate object directories, so only
    /// `refs/` entries inside known object dirs are inspected.  Package-installed
    /// files under `content/` are never traversed.
    pub fn broken_refs(&self) -> Result<Vec<PathBuf>> {
        let object_dirs = self.file_structure.objects.list_all()?;
        if object_dirs.is_empty() {
            log::trace!("broken_refs: no objects found in store.");
            return Ok(Vec::new());
        }
        let mut broken = Vec::new();
        for obj in &object_dirs {
            let refs_dir = obj.refs_dir();
            if !refs_dir.is_dir() {
                continue;
            }
            self.check_refs_dir(&refs_dir, &obj.content(), &mut broken)?;
        }
        if broken.is_empty() {
            log::debug!("broken_refs: no broken back-refs found.");
        } else {
            log::debug!("broken_refs: found {} broken back-ref(s).", broken.len());
        }
        Ok(broken)
    }

    fn check_refs_dir(
        &self,
        refs_dir: &Path,
        expected_content: &Path,
        broken: &mut Vec<PathBuf>,
    ) -> Result<()> {
        let entries = std::fs::read_dir(refs_dir)
            .map_err(|e| Error::InternalFile(refs_dir.to_path_buf(), e))?;

        for entry in entries.flatten() {
            let back_ref = entry.path();
            if !back_ref.is_symlink() {
                continue;
            }
            let Ok(forward_path) = std::fs::read_link(&back_ref) else {
                log::trace!("Broken back-ref '{}': could not read symlink target.", back_ref.display());
                broken.push(back_ref);
                continue;
            };
            if !forward_path.is_symlink() {
                // Forward symlink no longer exists.
                log::trace!(
                    "Broken back-ref '{}': forward symlink '{}' no longer exists.",
                    back_ref.display(),
                    forward_path.display(),
                );
                broken.push(back_ref);
                continue;
            }
            // Verify the forward symlink still points to this object's content.
            let Ok(actual) = std::fs::read_link(&forward_path) else {
                log::trace!(
                    "Broken back-ref '{}': could not read target of forward symlink '{}'.",
                    back_ref.display(),
                    forward_path.display(),
                );
                broken.push(back_ref);
                continue;
            };
            let actual_canon = std::fs::canonicalize(&actual).unwrap_or(actual);
            let expected_canon = std::fs::canonicalize(expected_content).ok();
            if Some(actual_canon) != expected_canon {
                log::trace!(
                    "Broken back-ref '{}': forward symlink '{}' points to wrong content.",
                    back_ref.display(),
                    forward_path.display(),
                );
                broken.push(back_ref);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::file_structure::FileStructure;

    fn setup() -> (TempDir, ReferenceManager) {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let rm = ReferenceManager::new(fs);
        (dir, rm)
    }

    /// Creates a real content directory at `{root}/objects/reg/repo/{id}/content`.
    fn make_content(root: &Path, id: &str) -> PathBuf {
        let p = root.join("objects").join("reg").join("repo").join(id).join("content");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Returns the path for a forward symlink at `{root}/fwd/{name}` (parent created).
    fn fwd(root: &Path, name: &str) -> PathBuf {
        let p = root.join("fwd").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        p
    }

    fn back_ref_for(content: &Path, forward: &Path) -> PathBuf {
        content
            .parent()
            .unwrap()
            .join("refs")
            .join(ReferenceManager::ref_name(forward))
    }

    // ── ref_name ──────────────────────────────────────────────────────────────

    #[test]
    fn ref_name_is_deterministic_and_16_hex_chars() {
        let path = Path::new("/home/user/.ocx/installs/ocx.sh/cmake/candidates/3.28");
        let name = ReferenceManager::ref_name(path);
        assert_eq!(name, ReferenceManager::ref_name(path));
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ref_name_differs_for_different_paths() {
        assert_ne!(
            ReferenceManager::ref_name(Path::new("/link/a")),
            ReferenceManager::ref_name(Path::new("/link/b")),
        );
    }

    // ── link ──────────────────────────────────────────────────────────────────

    #[test]
    fn link_creates_forward_symlink_and_back_ref() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");
        let forward = fwd(dir.path(), "link1");

        rm.link(&forward, &content).unwrap();

        assert_eq!(std::fs::read_link(&forward).unwrap(), content);
        let back_ref = back_ref_for(&content, &forward);
        assert!(back_ref.is_symlink());
        assert_eq!(std::fs::read_link(&back_ref).unwrap(), forward);
    }

    #[test]
    fn link_is_noop_when_already_pointing_to_same_content() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");
        let forward = fwd(dir.path(), "link1");

        rm.link(&forward, &content).unwrap();
        let refs_dir = content.parent().unwrap().join("refs");
        let count_before = std::fs::read_dir(&refs_dir).unwrap().count();

        rm.link(&forward, &content).unwrap();
        assert_eq!(std::fs::read_dir(&refs_dir).unwrap().count(), count_before);
    }

    #[test]
    fn link_updates_forward_and_moves_back_ref_on_relink() {
        let (dir, rm) = setup();
        let content_a = make_content(dir.path(), "d_a");
        let content_b = make_content(dir.path(), "d_b");
        let forward = fwd(dir.path(), "link1");

        rm.link(&forward, &content_a).unwrap();
        rm.link(&forward, &content_b).unwrap();

        assert_eq!(std::fs::read_link(&forward).unwrap(), content_b);
        // New back-ref present.
        assert!(back_ref_for(&content_b, &forward).is_symlink());
        // Old back-ref removed.
        assert!(!back_ref_for(&content_a, &forward).exists());
    }

    #[test]
    fn link_tolerates_missing_old_content_on_relink() {
        // Forward symlink already points to a GC'd (non-existent) content path.
        let (dir, rm) = setup();
        let content_b = make_content(dir.path(), "d_b");
        let forward = fwd(dir.path(), "link1");

        let gone = dir.path().join("objects").join("reg").join("repo").join("gone").join("content");
        crate::symlink::create(&gone, &forward).unwrap();

        rm.link(&forward, &content_b).unwrap();
        assert_eq!(std::fs::read_link(&forward).unwrap(), content_b);
    }

    #[test]
    fn link_replaces_stale_back_ref_at_target_location() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");
        let forward = fwd(dir.path(), "link1");

        // Pre-plant a stale symlink at the exact back-ref path.
        let back_ref = back_ref_for(&content, &forward);
        std::fs::create_dir_all(back_ref.parent().unwrap()).unwrap();
        crate::symlink::create(&dir.path().join("nowhere"), &back_ref).unwrap();

        rm.link(&forward, &content).unwrap();

        // Stale target replaced with the correct forward path.
        assert_eq!(std::fs::read_link(&back_ref).unwrap(), forward);
    }

    // ── unlink ────────────────────────────────────────────────────────────────

    #[test]
    fn unlink_is_noop_when_path_is_not_a_symlink() {
        let (dir, rm) = setup();
        rm.unlink(&dir.path().join("nonexistent")).unwrap();
    }

    #[test]
    fn unlink_removes_forward_symlink_and_back_ref() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");
        let forward = fwd(dir.path(), "link1");

        rm.link(&forward, &content).unwrap();
        let back_ref = back_ref_for(&content, &forward);
        assert!(back_ref.is_symlink());

        rm.unlink(&forward).unwrap();

        assert!(!forward.is_symlink());
        assert!(!back_ref.exists());
    }

    #[test]
    fn unlink_tolerates_dangling_forward_symlink() {
        // Forward points to a non-existent path — back_ref_path cannot canonicalize it.
        let (dir, rm) = setup();
        let forward = fwd(dir.path(), "link1");

        let gone = dir.path().join("objects").join("reg").join("repo").join("gone").join("content");
        crate::symlink::create(&gone, &forward).unwrap();

        rm.unlink(&forward).unwrap();
        assert!(!forward.is_symlink());
    }

    // ── broken_refs ───────────────────────────────────────────────────────────

    #[test]
    fn broken_refs_empty_when_objects_dir_absent() {
        let (_, rm) = setup();
        assert_eq!(rm.broken_refs().unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn broken_refs_empty_when_all_refs_valid() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");
        let forward = fwd(dir.path(), "link1");
        rm.link(&forward, &content).unwrap();

        assert_eq!(rm.broken_refs().unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn broken_refs_detects_missing_forward_symlink() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");

        // Back-ref points to a forward path that does not exist.
        let ghost_fwd = dir.path().join("fwd").join("ghost");
        let refs_dir = content.parent().unwrap().join("refs");
        std::fs::create_dir_all(&refs_dir).unwrap();
        let back_ref = refs_dir.join(ReferenceManager::ref_name(&ghost_fwd));
        crate::symlink::create(&ghost_fwd, &back_ref).unwrap();

        assert_eq!(rm.broken_refs().unwrap(), vec![back_ref]);
    }

    #[test]
    fn broken_refs_detects_forward_pointing_to_wrong_content() {
        let (dir, rm) = setup();
        let content_a = make_content(dir.path(), "d_a");
        let content_b = make_content(dir.path(), "d_b");
        let forward = fwd(dir.path(), "link1");

        // forward → content_a, but back-ref lives in content_b's refs/.
        crate::symlink::create(&content_a, &forward).unwrap();
        let refs_dir = content_b.parent().unwrap().join("refs");
        std::fs::create_dir_all(&refs_dir).unwrap();
        let back_ref = refs_dir.join(ReferenceManager::ref_name(&forward));
        crate::symlink::create(&forward, &back_ref).unwrap();

        assert_eq!(rm.broken_refs().unwrap(), vec![back_ref]);
    }

    #[test]
    fn broken_refs_does_not_recurse_into_content_dirs() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");

        // A "refs" dir inside the content dir must never be inspected.
        std::fs::create_dir_all(content.join("refs")).unwrap();

        assert_eq!(rm.broken_refs().unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn broken_refs_skips_non_symlinks_in_refs_dir() {
        let (dir, rm) = setup();
        let content = make_content(dir.path(), "d1");
        let refs_dir = content.parent().unwrap().join("refs");
        std::fs::create_dir_all(&refs_dir).unwrap();
        std::fs::write(refs_dir.join("not_a_symlink"), b"data").unwrap();

        assert_eq!(rm.broken_refs().unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn broken_refs_skips_non_dir_entries_in_objects() {
        let (dir, rm) = setup();
        let objects = dir.path().join("objects");
        std::fs::create_dir_all(&objects).unwrap();
        std::fs::write(objects.join("stray_file"), b"junk").unwrap();

        assert_eq!(rm.broken_refs().unwrap(), Vec::<PathBuf>::new());
    }
}
