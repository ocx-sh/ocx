// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{Error, Result, file_structure::FileStructure, file_structure::cas_ref_name, log, symlink, utility};

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

    /// Derives the reference name for an install symlink's forward path.
    ///
    /// The name is the first 16 hex characters of the SHA-256 hash of the
    /// path bytes — unique and fixed-length regardless of path length.
    /// Used for install back-refs (`refs/symlinks/`) where the ref's identity
    /// is the symlink location, not a content digest.
    pub fn name_for_path(forward_path: &Path) -> String {
        let mut hasher = Sha256::new();
        hasher.update(forward_path.as_os_str().as_encoded_bytes());
        hex::encode(&hasher.finalize()[..8])
    }

    /// Returns the back-reference path inside the object that `content_path`
    /// belongs to, keyed by `forward_path`.
    fn back_ref_path(&self, content_path: &Path, forward_path: &Path) -> Result<PathBuf> {
        Ok(self
            .file_structure
            .packages
            .refs_symlinks_dir_for_content(content_path)?
            .join(Self::name_for_path(forward_path)))
    }

    /// Creates or updates a forward symlink from `forward_path` to `content_path`,
    /// maintaining the corresponding back-reference.
    ///
    /// If `forward_path` already exists and points to a different object, the old
    /// back-reference is removed before the new one is created (re-link).  If it
    /// already points to `content_path`, the call is a no-op.
    pub fn link(&self, forward_path: &Path, content_path: &Path) -> Result<()> {
        if symlink::is_link(forward_path) {
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
            log::debug!("Linking '{}' → '{}'.", forward_path.display(), content_path.display(),);
        }

        symlink::update(content_path, forward_path)?;

        let ref_path = self.back_ref_path(content_path, forward_path)?;
        if let Some(parent) = ref_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::InternalFile(parent.to_path_buf(), e))?;
        }
        // Idempotent: recreate if a stale back-ref already exists at this path.
        if symlink::is_link(&ref_path) {
            log::trace!("Replacing stale back-ref '{}'.", ref_path.display());
            symlink::remove(&ref_path)?;
        }
        symlink::create(forward_path, &ref_path)?;
        log::trace!(
            "Created back-ref '{}' → '{}'.",
            ref_path.display(),
            forward_path.display()
        );

        Ok(())
    }

    /// Removes the forward symlink at `forward_path` and its back-reference.
    ///
    /// If `forward_path` does not exist the call is a no-op.  Failure to remove
    /// a stale back-reference is tolerated — the broken ref will be reported by
    /// [`broken_refs`] and can be cleaned up separately.
    pub fn unlink(&self, forward_path: &Path) -> Result<()> {
        if !symlink::is_link(forward_path) {
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

    /// Creates a dependency forward-reference in the dependent's `deps/` directory.
    ///
    /// The forward-ref allows GC to discover dependencies via filesystem traversal
    /// (instead of parsing metadata digests), which handles Image Index →
    /// platform-specific digest resolution correctly. GC determines reachability
    /// by walking `deps/` edges from root objects (those with install symlink refs
    /// in `refs/` or profile content-mode references).
    ///
    /// The filename is derived from `dependency_digest` via
    /// [`cas_path::cas_ref_name`] so the ref is self-describing: a quick look
    /// at `refs/deps/` tells you exactly which package digests the dependent
    /// points at.
    ///
    /// No back-reference is created in the dependency's `refs/` — that directory
    /// is reserved for install symlink refs (candidate/current).
    pub fn link_dependency(
        &self,
        dependent_content: &Path,
        dependency_content: &Path,
        dependency_digest: &crate::oci::Digest,
    ) -> Result<()> {
        self.create_forward_dep_ref(dependent_content, dependency_content, dependency_digest)
    }

    /// Creates a forward-dependency symlink in the dependent's `deps/` directory
    /// pointing to the dependency's content path.
    fn create_forward_dep_ref(
        &self,
        dependent_content: &Path,
        dependency_content: &Path,
        dependency_digest: &crate::oci::Digest,
    ) -> Result<()> {
        let deps_dir = self
            .file_structure
            .packages
            .refs_deps_dir_for_content(dependent_content)?;
        std::fs::create_dir_all(&deps_dir).map_err(|e| Error::InternalFile(deps_dir.clone(), e))?;

        let dep_path = deps_dir.join(cas_ref_name(dependency_digest));

        if symlink::is_link(&dep_path) {
            if let Ok(target) = std::fs::read_link(&dep_path)
                && target == dependency_content
            {
                log::trace!(
                    "Dependency forward-ref already exists: '{}' → '{}'.",
                    dep_path.display(),
                    dependency_content.display(),
                );
                return Ok(());
            }
            // Stale ref — replace it.
            symlink::remove(&dep_path)?;
        }

        symlink::create(dependency_content, &dep_path)?;
        log::trace!(
            "Created dependency forward-ref '{}' → '{}'.",
            dep_path.display(),
            dependency_content.display(),
        );
        Ok(())
    }

    /// Removes a dependency forward-reference from the dependent's `deps/` directory.
    ///
    /// No-op if the forward-ref does not exist.
    pub fn unlink_dependency(&self, dependent_content: &Path, dependency_digest: &crate::oci::Digest) -> Result<()> {
        if let Ok(deps_dir) = self
            .file_structure
            .packages
            .refs_deps_dir_for_content(dependent_content)
        {
            let dep_path = deps_dir.join(cas_ref_name(dependency_digest));
            if symlink::is_link(&dep_path) {
                log::trace!("Removing dependency forward-ref '{}'.", dep_path.display());
                symlink::remove(&dep_path)?;
            }
        }

        Ok(())
    }

    /// Returns the paths of all broken back-references found in the package store.
    ///
    /// A back-reference is broken when:
    /// - Its target (the forward path) no longer exists, or
    /// - The forward path no longer points to the expected content (the package
    ///   was re-linked without going through [`ReferenceManager`]).
    ///
    /// Uses [`PackageStore::list_all`] to enumerate package directories, so only
    /// `refs/` entries inside known package dirs are inspected.  Package-installed
    /// files under `content/` are never traversed.
    pub async fn broken_refs(&self) -> Result<Vec<PathBuf>> {
        let package_dirs = self.file_structure.packages.list_all().await?;
        if package_dirs.is_empty() {
            log::trace!("broken_refs: no packages found in store.");
            return Ok(Vec::new());
        }

        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(50));
        let mut tasks = tokio::task::JoinSet::new();

        for obj in &package_dirs {
            let refs_dir = obj.refs_symlinks_dir();
            if !utility::fs::path_exists_lossy(&refs_dir).await {
                continue;
            }
            let content = obj.content();
            let sem = std::sync::Arc::clone(&sem);
            tasks.spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore closed");
                check_refs_dir(&refs_dir, &content).await
            });
        }

        let mut broken = Vec::new();
        while let Some(result) = tasks.join_next().await {
            broken.extend(result.expect("task panicked")?);
        }

        broken.sort();

        if broken.is_empty() {
            log::debug!("broken_refs: no broken back-refs found.");
        } else {
            log::debug!("broken_refs: found {} broken back-ref(s).", broken.len());
        }
        Ok(broken)
    }

    /// Idempotently upserts a `refs/blobs/` forward-ref for every entry in
    /// `chain`. The link name is derived from each entry's digest via
    /// `cas_ref_name` so concurrent peers producing the same chain produce
    /// identical symlinks — races resolve to the correct state.
    ///
    /// Eventual consistency: this function does not verify that targets
    /// exist on disk. If a target blob is missing (e.g., a concurrent
    /// `ocx clean` raced the caller), a dangling symlink is written and
    /// the next GC pass collects it. Callers don't need to serialize
    /// against GC — the system converges on its own.
    pub async fn link_blobs(&self, content_path: &Path, chain: &[crate::oci::PinnedIdentifier]) -> Result<()> {
        if chain.is_empty() {
            return Ok(());
        }

        let refs_blobs = self.file_structure.packages.refs_blobs_dir_for_content(content_path)?;
        tokio::fs::create_dir_all(&refs_blobs)
            .await
            .map_err(|e| Error::InternalFile(refs_blobs.clone(), e))?;

        for pinned in chain {
            let digest = pinned.digest();
            let target = self.file_structure.blobs.data(pinned.registry(), &digest);
            let link_path = refs_blobs.join(cas_ref_name(&digest));
            if symlink::is_link(&link_path) {
                if let Ok(existing) = std::fs::read_link(&link_path)
                    && existing == target
                {
                    log::trace!(
                        "refs/blobs/ forward-ref already current: '{}' → '{}'.",
                        link_path.display(),
                        target.display(),
                    );
                    continue;
                }
                log::trace!("Updating stale refs/blobs/ forward-ref '{}'.", link_path.display());
            }

            symlink::update(&target, &link_path)?;
            log::trace!(
                "Created refs/blobs/ forward-ref '{}' → '{}'.",
                link_path.display(),
                target.display(),
            );
        }
        Ok(())
    }
}

async fn check_refs_dir(refs_dir: &Path, expected_content: &Path) -> Result<Vec<PathBuf>> {
    let mut entries = tokio::fs::read_dir(refs_dir)
        .await
        .map_err(|e| Error::InternalFile(refs_dir.to_path_buf(), e))?;
    let mut broken = Vec::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let back_ref = entry.path();
        if !symlink::is_link(&back_ref) {
            continue;
        }
        let Ok(forward_path) = tokio::fs::read_link(&back_ref).await else {
            log::trace!(
                "Broken back-ref '{}': could not read symlink target.",
                back_ref.display()
            );
            broken.push(back_ref);
            continue;
        };
        if !symlink::is_link(&forward_path) {
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
        let Ok(actual) = tokio::fs::read_link(&forward_path).await else {
            log::trace!(
                "Broken back-ref '{}': could not read target of forward symlink '{}'.",
                back_ref.display(),
                forward_path.display(),
            );
            broken.push(back_ref);
            continue;
        };
        let actual_canon = dunce::canonicalize(&actual).unwrap_or(actual);
        let expected_canon = dunce::canonicalize(expected_content).ok();
        if Some(actual_canon) != expected_canon {
            log::trace!(
                "Broken back-ref '{}': forward symlink '{}' points to wrong content.",
                back_ref.display(),
                forward_path.display(),
            );
            broken.push(back_ref);
        }
    }
    Ok(broken)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;
    use crate::file_structure::FileStructure;

    fn setup() -> (TempDir, PathBuf, ReferenceManager) {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let fs = FileStructure::with_root(root.clone());
        let rm = ReferenceManager::new(fs);
        (dir, root, rm)
    }

    /// Creates a real content directory matching the package store layout:
    /// `{root}/packages/reg/sha256/{2hex}/{30hex}/content`.
    ///
    /// `n` selects a unique prefix; the suffix is fixed padding.
    fn make_content(root: &Path, n: u32) -> PathBuf {
        let p = root
            .join("packages")
            .join("reg")
            .join("sha256")
            .join(format!("{n:02x}"))
            .join("aabb1122ccdd3344eeff5566778899")
            .join("content");
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// Returns a full 64-hex sha256 digest whose 32-hex CAS prefix matches
    /// the layout produced by [`make_content`] for the same `n`.
    fn make_digest(n: u32) -> crate::oci::Digest {
        crate::oci::Digest::Sha256(format!(
            "{n:02x}aabb1122ccdd3344eeff5566778899000000000000000000000000000000000000"
        ))
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
            .join("symlinks")
            .join(ReferenceManager::name_for_path(forward))
    }

    // ── name_for_path ─────────────────────────────────────────────────────────

    #[test]
    fn name_for_path_is_deterministic_and_16_hex_chars() {
        let path = Path::new("/home/user/.ocx/symlinks/ocx.sh/cmake/candidates/3.28");
        let name = ReferenceManager::name_for_path(path);
        assert_eq!(name, ReferenceManager::name_for_path(path));
        assert_eq!(name.len(), 16);
        assert!(name.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn name_for_path_differs_for_different_paths() {
        assert_ne!(
            ReferenceManager::name_for_path(Path::new("/link/a")),
            ReferenceManager::name_for_path(Path::new("/link/b")),
        );
    }

    // ── link ──────────────────────────────────────────────────────────────────

    #[test]
    fn link_creates_forward_symlink_and_back_ref() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);
        let forward = fwd(&root, "link1");

        rm.link(&forward, &content).unwrap();

        assert_eq!(std::fs::read_link(&forward).unwrap(), content);
        let back_ref = back_ref_for(&content, &forward);
        assert!(crate::symlink::is_link(&back_ref));
        assert_eq!(std::fs::read_link(&back_ref).unwrap(), forward);
    }

    #[test]
    fn link_is_noop_when_already_pointing_to_same_content() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);
        let forward = fwd(&root, "link1");

        rm.link(&forward, &content).unwrap();
        let refs_dir = content.parent().unwrap().join("refs");
        let count_before = std::fs::read_dir(&refs_dir).unwrap().count();

        rm.link(&forward, &content).unwrap();
        assert_eq!(std::fs::read_dir(&refs_dir).unwrap().count(), count_before);
    }

    #[test]
    fn link_updates_forward_and_moves_back_ref_on_relink() {
        let (_dir, root, rm) = setup();
        let content_a = make_content(&root, 0xa);
        let content_b = make_content(&root, 0xb);
        let forward = fwd(&root, "link1");

        rm.link(&forward, &content_a).unwrap();
        rm.link(&forward, &content_b).unwrap();

        assert_eq!(std::fs::read_link(&forward).unwrap(), content_b);
        // New back-ref present.
        assert!(crate::symlink::is_link(&back_ref_for(&content_b, &forward)));
        // Old back-ref removed.
        assert!(!back_ref_for(&content_a, &forward).exists());
    }

    #[test]
    fn link_tolerates_missing_old_content_on_relink() {
        // Forward symlink already points to a GC'd (non-existent) content path.
        let (_dir, root, rm) = setup();
        let content_b = make_content(&root, 0xb);
        let forward = fwd(&root, "link1");

        let gone = root
            .join("objects")
            .join("reg")
            .join("repo")
            .join("gone")
            .join("content");
        crate::symlink::create(&gone, &forward).unwrap();

        rm.link(&forward, &content_b).unwrap();
        assert_eq!(std::fs::read_link(&forward).unwrap(), content_b);
    }

    #[test]
    fn link_replaces_stale_back_ref_at_target_location() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);
        let forward = fwd(&root, "link1");

        // Pre-plant a stale symlink at the exact back-ref path.
        let back_ref = back_ref_for(&content, &forward);
        std::fs::create_dir_all(back_ref.parent().unwrap()).unwrap();
        crate::symlink::create(root.join("nowhere"), &back_ref).unwrap();

        rm.link(&forward, &content).unwrap();

        // Stale target replaced with the correct forward path.
        assert_eq!(std::fs::read_link(&back_ref).unwrap(), forward);
    }

    // ── unlink ────────────────────────────────────────────────────────────────

    #[test]
    fn unlink_is_noop_when_path_is_not_a_symlink() {
        let (_dir, root, rm) = setup();
        rm.unlink(&root.join("nonexistent")).unwrap();
    }

    #[test]
    fn unlink_removes_forward_symlink_and_back_ref() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);
        let forward = fwd(&root, "link1");

        rm.link(&forward, &content).unwrap();
        let back_ref = back_ref_for(&content, &forward);
        assert!(crate::symlink::is_link(&back_ref));

        rm.unlink(&forward).unwrap();

        assert!(!crate::symlink::is_link(&forward));
        assert!(!back_ref.exists());
    }

    #[test]
    fn unlink_tolerates_dangling_forward_symlink() {
        // Forward points to a non-existent path — back_ref_path cannot canonicalize it.
        let (_dir, root, rm) = setup();
        let forward = fwd(&root, "link1");

        let gone = root
            .join("objects")
            .join("reg")
            .join("repo")
            .join("gone")
            .join("content");
        crate::symlink::create(&gone, &forward).unwrap();

        rm.unlink(&forward).unwrap();
        assert!(!crate::symlink::is_link(&forward));
    }

    // ── broken_refs ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn broken_refs_empty_when_objects_dir_absent() {
        let (_dir, _root, rm) = setup();
        assert_eq!(rm.broken_refs().await.unwrap(), Vec::<PathBuf>::new());
    }

    #[tokio::test]
    async fn broken_refs_empty_when_all_refs_valid() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);
        let forward = fwd(&root, "link1");
        rm.link(&forward, &content).unwrap();

        assert_eq!(rm.broken_refs().await.unwrap(), Vec::<PathBuf>::new());
    }

    #[tokio::test]
    async fn broken_refs_detects_missing_forward_symlink() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);

        // Back-ref points to a forward path that does not exist.
        let ghost_fwd = root.join("fwd").join("ghost");
        let refs_dir = content.parent().unwrap().join("refs").join("symlinks");
        std::fs::create_dir_all(&refs_dir).unwrap();
        let back_ref = refs_dir.join(ReferenceManager::name_for_path(&ghost_fwd));
        crate::symlink::create(&ghost_fwd, &back_ref).unwrap();

        assert_eq!(rm.broken_refs().await.unwrap(), vec![back_ref]);
    }

    #[tokio::test]
    async fn broken_refs_detects_forward_pointing_to_wrong_content() {
        let (_dir, root, rm) = setup();
        let content_a = make_content(&root, 0xa);
        let content_b = make_content(&root, 0xb);
        let forward = fwd(&root, "link1");

        // forward → content_a, but back-ref lives in content_b's refs/symlinks/.
        crate::symlink::create(&content_a, &forward).unwrap();
        let refs_dir = content_b.parent().unwrap().join("refs").join("symlinks");
        std::fs::create_dir_all(&refs_dir).unwrap();
        let back_ref = refs_dir.join(ReferenceManager::name_for_path(&forward));
        crate::symlink::create(&forward, &back_ref).unwrap();

        assert_eq!(rm.broken_refs().await.unwrap(), vec![back_ref]);
    }

    #[tokio::test]
    async fn broken_refs_does_not_recurse_into_content_dirs() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);

        // A "refs" dir inside the content dir must never be inspected.
        std::fs::create_dir_all(content.join("refs")).unwrap();

        assert_eq!(rm.broken_refs().await.unwrap(), Vec::<PathBuf>::new());
    }

    #[tokio::test]
    async fn broken_refs_skips_non_symlinks_in_refs_dir() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);
        let refs_dir = content.parent().unwrap().join("refs").join("symlinks");
        std::fs::create_dir_all(&refs_dir).unwrap();
        std::fs::write(refs_dir.join("not_a_symlink"), b"data").unwrap();

        assert_eq!(rm.broken_refs().await.unwrap(), Vec::<PathBuf>::new());
    }

    // ── link_blobs tests (plan_resolution_chain_refs.md tests 39-43) ──

    /// Helper: create a real blob data file at the CAS path for (registry, digest).
    fn make_blob(root: &Path, registry: &str, digest: &crate::oci::Digest) -> std::path::PathBuf {
        let store = crate::file_structure::BlobStore::new(root.join("blobs"));
        let data_path = store.data(registry, digest);
        std::fs::create_dir_all(data_path.parent().unwrap()).unwrap();
        std::fs::write(&data_path, b"manifest bytes").unwrap();
        data_path
    }

    /// Helper: build a `PinnedIdentifier` from a registry + digest pair using
    /// a synthetic repository (irrelevant for `link_blobs` semantics).
    fn make_pinned(registry: &str, digest: &crate::oci::Digest) -> crate::oci::PinnedIdentifier {
        let id = crate::oci::Identifier::new_registry("repo", registry).clone_with_digest(digest.clone());
        crate::oci::PinnedIdentifier::try_from(id).unwrap()
    }

    /// Test 39: link_blobs creates a symlink in refs/blobs/ for each
    /// chain entry. The symlink must point to blobs/{registry}/.../data.
    #[tokio::test]
    async fn link_blobs_creates_symlinks_for_all_chain_entries() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 1);

        let reg = "example.com";
        let d1 = make_digest(0x01);
        let d2 = make_digest(0x02);
        make_blob(&root, reg, &d1);
        make_blob(&root, reg, &d2);

        let chain = vec![make_pinned(reg, &d1), make_pinned(reg, &d2)];
        rm.link_blobs(&content, &chain).await.unwrap();

        // Both symlinks must exist in refs/blobs/.
        let refs_blobs = content.parent().unwrap().join("refs").join("blobs");
        assert!(refs_blobs.is_dir(), "refs/blobs/ directory must exist");
        let entries: Vec<_> = std::fs::read_dir(&refs_blobs).unwrap().collect();
        assert_eq!(
            entries.len(),
            2,
            "two chain entries must produce two refs/blobs/ symlinks"
        );
    }

    /// Test 40: link_blobs is idempotent — calling twice produces no
    /// duplicate entries and no errors.
    #[tokio::test]
    async fn link_blobs_idempotent_on_existing_correct_symlinks() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 2);

        let reg = "example.com";
        let d1 = make_digest(0x03);
        make_blob(&root, reg, &d1);

        let chain = vec![make_pinned(reg, &d1)];
        rm.link_blobs(&content, &chain).await.unwrap();
        rm.link_blobs(&content, &chain).await.unwrap(); // idempotent second call

        let refs_blobs = content.parent().unwrap().join("refs").join("blobs");
        let count = std::fs::read_dir(&refs_blobs).unwrap().count();
        assert_eq!(count, 1, "idempotent call must not create duplicate symlinks");
    }

    /// Test 41: link_blobs tolerates EEXIST when the target already
    /// matches — a racing peer creating the same symlink concurrently must not
    /// cause an error.
    #[tokio::test]
    async fn link_blobs_tolerates_eexist_when_target_matches() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 3);

        let reg = "example.com";
        let d1 = make_digest(0x04);
        let blob_path = make_blob(&root, reg, &d1);

        // Pre-create the symlink with the correct target (simulates a racing peer).
        let refs_blobs = content.parent().unwrap().join("refs").join("blobs");
        std::fs::create_dir_all(&refs_blobs).unwrap();
        let ref_name = crate::file_structure::cas_ref_name(&d1);
        let link_path = refs_blobs.join(&ref_name);
        crate::symlink::create(&blob_path, &link_path).unwrap();

        let chain = vec![make_pinned(reg, &d1)];
        // Must not error even though the symlink already exists with the correct target.
        rm.link_blobs(&content, &chain).await.unwrap();
    }

    /// Test 42: link_blobs updates a stale symlink target.
    /// (Stale targets are structurally impossible by construction — the ref
    /// name is derived from the digest — but the code must handle them.)
    #[tokio::test]
    async fn link_blobs_updates_stale_symlink_target() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 4);

        let reg = "example.com";
        let d1 = make_digest(0x05);
        let correct_blob = make_blob(&root, reg, &d1);

        // Pre-create the symlink pointing at an incorrect (stale) target.
        let refs_blobs = content.parent().unwrap().join("refs").join("blobs");
        std::fs::create_dir_all(&refs_blobs).unwrap();
        let ref_name = crate::file_structure::cas_ref_name(&d1);
        let link_path = refs_blobs.join(&ref_name);
        let stale_target = root.join("nowhere");
        crate::symlink::create(&stale_target, &link_path).unwrap();

        let chain = vec![make_pinned(reg, &d1)];
        rm.link_blobs(&content, &chain).await.unwrap();

        // After the call, the symlink must point at the correct blob.
        assert_eq!(
            std::fs::read_link(&link_path).unwrap(),
            correct_blob,
            "link_blobs must update a stale symlink target to the correct blob path"
        );
    }

    /// Test 43: link_blobs with a missing blob data file still creates the
    /// forward symlink (a dangling symlink). The eventual-consistency model
    /// (GC sweeps dangling refs) handles this — the producer no longer needs
    /// to pre-check for existence, which avoided a TOCTOU race against
    /// concurrent `ocx clean`.
    #[tokio::test]
    async fn link_blobs_missing_blob_file_creates_dangling_symlink() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 5);

        let reg = "example.com";
        let d1 = make_digest(0x06);
        // Do NOT create the blob data file — verify the symlink is still made.

        let chain = vec![make_pinned(reg, &d1)];
        rm.link_blobs(&content, &chain)
            .await
            .expect("link_blobs must succeed even when the blob is missing");

        let refs_blobs = content.parent().unwrap().join("refs").join("blobs");
        let ref_name = crate::file_structure::cas_ref_name(&d1);
        let link_path = refs_blobs.join(&ref_name);
        assert!(
            crate::symlink::is_link(&link_path),
            "dangling forward-ref symlink must be present"
        );
    }

    #[tokio::test]
    async fn broken_refs_skips_non_dir_entries_in_packages() {
        let (_dir, root, rm) = setup();
        let packages = root.join("packages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(packages.join("stray_file"), b"junk").unwrap();

        assert_eq!(rm.broken_refs().await.unwrap(), Vec::<PathBuf>::new());
    }

    // ── link_dependency / unlink_dependency ───────────────────────────────

    #[test]
    fn link_dependency_creates_forward_ref() {
        let (_dir, root, rm) = setup();
        let dep_content = make_content(&root, 0xde);
        let dep_digest = make_digest(0xde);
        let dependent_content = make_content(&root, 0xaa);

        rm.link_dependency(&dependent_content, &dep_content, &dep_digest)
            .unwrap();

        // No back-ref in dependency's refs/symlinks/
        let refs_dir = dep_content.parent().unwrap().join("refs").join("symlinks");
        assert!(
            !refs_dir.is_dir() || std::fs::read_dir(&refs_dir).unwrap().next().is_none(),
            "dependency's refs/symlinks/ should be empty (no back-refs)"
        );

        // Forward-ref: dependent's refs/deps/ → dependency's content, named
        // after the dependency digest so it's self-describing.
        let dep_path = dependent_content
            .parent()
            .unwrap()
            .join("refs")
            .join("deps")
            .join(crate::file_structure::cas_ref_name(&dep_digest));
        assert!(crate::symlink::is_link(&dep_path));
        assert_eq!(std::fs::read_link(&dep_path).unwrap(), dep_content);
    }

    #[test]
    fn link_dependency_is_idempotent() {
        let (_dir, root, rm) = setup();
        let dep_content = make_content(&root, 0xde);
        let dep_digest = make_digest(0xde);
        let dependent_content = make_content(&root, 0xaa);

        rm.link_dependency(&dependent_content, &dep_content, &dep_digest)
            .unwrap();
        rm.link_dependency(&dependent_content, &dep_content, &dep_digest)
            .unwrap();

        let deps_dir = dependent_content.parent().unwrap().join("refs").join("deps");
        assert_eq!(std::fs::read_dir(&deps_dir).unwrap().count(), 1);
    }

    #[test]
    fn unlink_dependency_removes_forward_ref() {
        let (_dir, root, rm) = setup();
        let dep_content = make_content(&root, 0xde);
        let dep_digest = make_digest(0xde);
        let dependent_content = make_content(&root, 0xaa);

        rm.link_dependency(&dependent_content, &dep_content, &dep_digest)
            .unwrap();
        rm.unlink_dependency(&dependent_content, &dep_digest).unwrap();

        // Forward-ref removed
        let deps_dir = dependent_content.parent().unwrap().join("refs").join("deps");
        assert!(std::fs::read_dir(&deps_dir).unwrap().next().is_none());
    }

    #[test]
    fn unlink_dependency_is_noop_when_no_ref() {
        let (_dir, root, rm) = setup();
        let _dep_content = make_content(&root, 0xde);
        let dep_digest = make_digest(0xde);
        let dependent_content = make_content(&root, 0xaa);

        // Should not error even though no ref was created.
        rm.unlink_dependency(&dependent_content, &dep_digest).unwrap();
    }

    // ── T5: empty-chain link_blobs is a no-op ────────────────────────────

    /// T5 (plan review): link_blobs with an empty chain slice returns Ok and
    /// does not create a refs/blobs/ directory. Cheap regression guard.
    #[tokio::test]
    async fn link_blobs_empty_chain_is_noop() {
        let (_dir, root, rm) = setup();
        let content = make_content(&root, 0x10);

        rm.link_blobs(&content, &[]).await.unwrap();

        let refs_blobs = content.parent().unwrap().join("refs").join("blobs");
        // The directory must either not exist or (if it was created) be empty.
        if refs_blobs.exists() {
            let count = std::fs::read_dir(&refs_blobs).unwrap().count();
            assert_eq!(count, 0, "refs/blobs/ must be empty after link_blobs with empty chain");
        }
        // No entry created — the call was a no-op.
    }

    // ── D1: refs/blobs/ symlinks are path-independent after temp→final rename

    /// D1 (plan review): blob forward-refs written against a temp content
    /// path continue to resolve correctly after the parent directory is
    /// atomically renamed to the final location.
    ///
    /// This validates the current implementation: symlinks in refs/blobs/ are
    /// absolute paths into the blob store (e.g.
    /// `{root}/blobs/reg/sha256/…/data`), which is independent of the
    /// package's own location. A rename of the package directory from a temp
    /// path to the final content path does not affect blob target resolution.
    #[tokio::test]
    async fn link_blobs_symlinks_are_path_independent_after_temp_to_final_rename() {
        let (_dir, root, rm) = setup();

        // Step 1: create a "temp" content directory (simulates TempStore layout).
        let temp_content = root
            .join("temp")
            .join("deadbeefdeadbeefdeadbeefdeadbeef")
            .join("content");
        std::fs::create_dir_all(&temp_content).unwrap();

        // Step 2: seed two real blob data files in the blob store.
        let reg = "example.com";
        let d1 = make_digest(0x11);
        let d2 = make_digest(0x12);
        make_blob(&root, reg, &d1);
        make_blob(&root, reg, &d2);

        // Step 3: call link_blobs against the temp content path.
        let chain = vec![make_pinned(reg, &d1), make_pinned(reg, &d2)];
        rm.link_blobs(&temp_content, &chain).await.unwrap();

        // Verify symlinks were created in temp content's refs/blobs/.
        let temp_refs = temp_content.parent().unwrap().join("refs").join("blobs");
        assert!(
            temp_refs.is_dir(),
            "refs/blobs/ must exist in temp dir after link_blobs"
        );
        assert_eq!(
            std::fs::read_dir(&temp_refs).unwrap().count(),
            2,
            "two chain entries must produce two symlinks in temp refs/blobs/"
        );

        // Step 4: rename temp dir (parent of content) to the final location.
        let final_parent = root
            .join("packages")
            .join("reg")
            .join("sha256")
            .join("11")
            .join("ffffffffffffffffffffffffffff11");
        std::fs::create_dir_all(final_parent.parent().unwrap()).unwrap();
        std::fs::rename(temp_content.parent().unwrap(), &final_parent).unwrap();
        let final_refs_blobs = final_parent.join("refs").join("blobs");

        // Step 5: verify all symlinks in the renamed location still resolve.
        for entry in std::fs::read_dir(&final_refs_blobs).unwrap() {
            let entry = entry.unwrap();
            let link_path = entry.path();
            assert!(
                crate::symlink::is_link(&link_path),
                "{} must be a symlink",
                link_path.display()
            );
            // Symlink target is absolute into blobs/ — it must exist regardless
            // of where the package directory moved.
            let target = std::fs::read_link(&link_path).unwrap();
            assert!(
                target.is_absolute(),
                "D1: blob forward-ref target must be absolute; got: {}",
                target.display()
            );
            assert!(
                target.exists(),
                "D1: blob forward-ref must resolve after temp→final rename; \
                 target {} not found (link at {})",
                target.display(),
                link_path.display()
            );
        }
    }
}
