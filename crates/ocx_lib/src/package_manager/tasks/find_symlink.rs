// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    file_structure::{self, PackageDir, SymlinkKind},
    log, oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
    utility,
};

use super::super::PackageManager;

impl PackageManager {
    /// Returns the content digest of the `current` installation for `identifier`.
    ///
    /// Resolves the `current` symlink for `identifier` to its package root and
    /// reads the `digest` file from that root. Returns `Ok(None)` when:
    ///
    /// - the `current` symlink is absent
    /// - the symlink is dangling (target does not exist)
    /// - the `digest` file is unreadable or malformed
    ///
    /// These are all treated as "not installed" without emitting any diagnostic
    /// (benign state per plan D6). Callers should treat `None` as "not current"
    /// and proceed to install. Because a not-installed machine is indistinguishable
    /// from a transient I/O surprise at this seam, even unexpected read failures
    /// resolve to `Ok(None)` rather than an error — not-installed semantics win.
    ///
    /// Used by the pinned bootstrap path (`ensure_self_installed`) to compare
    /// the installed digest against the one resolved from the pin, so that a
    /// re-run with the same pin on an already-current machine is a no-op.
    ///
    /// Reuses the same path-resolution machinery as
    /// [`find_symlink`](Self::find_symlink): the `current` symlink path from the
    /// [`SymlinkStore`](crate::file_structure::SymlinkStore), then
    /// [`PackageStore::digest_file_for_content`](crate::file_structure::PackageStore::digest_file_for_content)
    /// (which follows the symlink to the package root) plus
    /// [`read_digest_file`](crate::file_structure::read_digest_file) — the
    /// digest-reading half of `tasks/common.rs::identifier_for_symlink`.
    pub async fn installed_current_digest(&self, identifier: &oci::Identifier) -> crate::Result<Option<oci::Digest>> {
        let current = self.file_structure().symlinks.current(identifier);

        // Symlink absent or dangling → not installed. `path_exists_lossy`
        // follows the symlink, so a dangling `current` reads as `false`.
        if !utility::fs::path_exists_lossy(&current).await {
            return Ok(None);
        }

        // Resolve the `current` symlink to its package-root digest file. A
        // dangling symlink (canonicalize fails) or an unreadable/malformed
        // digest file is a benign not-installed state — return None, no warn.
        let objects = &self.file_structure().packages;
        let Ok(digest_path) = objects.digest_file_for_content(&current) else {
            return Ok(None);
        };
        match file_structure::read_digest_file(&digest_path).await {
            Ok(digest) => Ok(Some(digest)),
            Err(_) => Ok(None),
        }
    }

    /// Resolves a package's content path via an install symlink rather than the
    /// content-addressed object store.
    ///
    /// The `content` path in the returned [`InstallInfo`] is the *symlink path
    /// itself* rather than the resolved object-store path.  This is intentional:
    /// downstream consumers embed this path in their output so it must be stable
    /// across package updates.
    ///
    /// Unlike the resolver-backed tasks (`find`, `install`, etc.), this path does
    /// not walk the OCI resolution chain and therefore does not upsert entries
    /// into `refs/blobs/`. No resolution happens — the install symlink points
    /// directly at an already-installed package whose `refs/blobs/` was populated
    /// at install time.
    pub async fn find_symlink(
        &self,
        package: &oci::Identifier,
        kind: SymlinkKind,
    ) -> Result<InstallInfo, PackageErrorKind> {
        log::debug!("Finding {:?} symlink for '{}'.", kind, package);

        if package.digest().is_some() {
            return Err(PackageErrorKind::SymlinkRequiresTag);
        }

        if kind == SymlinkKind::Current
            && let Some(tag) = package.tag()
        {
            log::warn!("--current ignores the tag '{tag}' of '{package}'");
        }

        let symlink_path = self.file_structure().symlinks.symlink(package, kind);

        if !utility::fs::path_exists_lossy(&symlink_path).await {
            return Err(PackageErrorKind::SymlinkNotFound(kind));
        }

        let packages = &self.file_structure().packages;
        let (metadata, resolved) = super::common::load_object_data(packages, &symlink_path)
            .await
            .map_err(PackageErrorKind::Internal)?;
        let identifier = super::common::identifier_for_symlink(packages, &symlink_path, package, kind)
            .await
            .map_err(PackageErrorKind::Internal)?;

        log::debug!(
            "Resolved '{}' via {:?} symlink at '{}'",
            package,
            kind,
            symlink_path.display()
        );

        // Install symlinks target the package root (post-flatten layout). The
        // env-resolution layer derives `${installPath}` from
        // `info.dir().content()` so traversal stays stable through the symlink
        // while landing in the right subdir.
        let dir = PackageDir {
            dir: symlink_path.to_path_buf(),
        };

        Ok(InstallInfo::new(identifier, metadata, resolved, dir))
    }

    pub async fn find_symlink_all(
        &self,
        packages: Vec<oci::Identifier>,
        kind: SymlinkKind,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        let mut infos = Vec::with_capacity(packages.len());
        let mut errors: Vec<PackageError> = Vec::new();

        for package in &packages {
            let _spin = self.progress().spinner(format!("Resolving '{package}'"));
            match self.find_symlink(package, kind).await {
                Ok(info) => infos.push(info),
                Err(kind) => errors.push(PackageError::new(package.clone(), kind)),
            }
        }

        if !errors.is_empty() {
            return Err(package_manager::error::Error::FindFailed(errors));
        }

        Ok(infos)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::{
        file_structure::{FileStructure, IndexStore, write_digest_file},
        oci::{
            self,
            index::{ChainMode, Index, LocalConfig, LocalIndex},
        },
        package_manager::PackageManager,
    };

    /// Build a minimal offline [`PackageManager`] rooted at `ocx_home`.
    ///
    /// Mirrors `make_offline_manager` from `tasks/update_check.rs`.
    fn make_offline_manager(ocx_home: &Path) -> PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(ocx_home.join("index")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        PackageManager::new(fs, index, None, "ocx.sh")
    }

    // ── installed_current_digest: absent `current` symlink ───────────────────

    /// When no `current` symlink exists → `Ok(None)`.
    ///
    /// Benign not-installed state; no diagnostic emitted.
    #[tokio::test]
    async fn installed_current_digest_absent_symlink_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        // No symlink created; `current` path does not exist.
        let result = manager.installed_current_digest(&identifier).await;
        assert!(result.is_ok(), "absent current must not return an error");
        assert!(result.unwrap().is_none(), "absent current must return None");
    }

    // ── installed_current_digest: healthy package root + digest file ─────────

    /// When `current` → real package root with a valid `digest` file → `Ok(Some(digest))`.
    #[tokio::test]
    async fn installed_current_digest_healthy_install_returns_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        // Build a fake package root directory with a `digest` file.
        // `digest_file_for_content` calls `dunce::canonicalize` on the `current`
        // symlink target, then looks for `digest` in the resolved directory.
        let package_root = tmp.path().join("packages").join("fake_pkg");
        tokio::fs::create_dir_all(&package_root).await.unwrap();

        let expected_digest: oci::Digest =
            oci::Digest::try_from(format!("sha256:{}", "a".repeat(64)).as_str()).unwrap();
        let digest_file = package_root.join(crate::file_structure::DIGEST_FILENAME);
        write_digest_file(&digest_file, &expected_digest).await.unwrap();

        // Create the `current` symlink pointing at the package root.
        let current_path = manager.file_structure().symlinks.current(&identifier);
        tokio::fs::create_dir_all(current_path.parent().unwrap()).await.unwrap();
        crate::symlink::update(&package_root, &current_path).unwrap();

        let result = manager.installed_current_digest(&identifier).await;
        assert!(result.is_ok(), "healthy install must succeed");
        let digest = result.unwrap().expect("healthy install must return Some(digest)");
        assert_eq!(digest, expected_digest, "returned digest must match the digest file");
    }

    // ── installed_current_digest: dangling `current` symlink ─────────────────

    /// When `current` symlink points at a non-existent target → `Ok(None)`.
    ///
    /// `path_exists_lossy` follows the symlink; a dangling link reads as `false`,
    /// so the method short-circuits to `None` at the presence guard.
    #[tokio::test]
    async fn installed_current_digest_dangling_symlink_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        // Create a `current` symlink pointing at a directory that does not exist.
        let current_path = manager.file_structure().symlinks.current(&identifier);
        tokio::fs::create_dir_all(current_path.parent().unwrap()).await.unwrap();
        let nonexistent = tmp.path().join("packages").join("does_not_exist");
        crate::symlink::update(&nonexistent, &current_path).unwrap();

        let result = manager.installed_current_digest(&identifier).await;
        assert!(result.is_ok(), "dangling current must not return an error");
        assert!(result.unwrap().is_none(), "dangling current must return None");
    }

    // ── installed_current_digest: unreadable / garbage digest file ───────────

    /// When `current` → real directory but `digest` file contains garbage → `Ok(None)`.
    ///
    /// `read_digest_file` fails on malformed content; the `Err(_) => Ok(None)` arm fires.
    #[tokio::test]
    async fn installed_current_digest_malformed_digest_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = make_offline_manager(tmp.path());
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);

        let package_root = tmp.path().join("packages").join("malformed_pkg");
        tokio::fs::create_dir_all(&package_root).await.unwrap();
        let digest_file = package_root.join(crate::file_structure::DIGEST_FILENAME);
        tokio::fs::write(&digest_file, b"not-a-valid-digest!!").await.unwrap();

        let current_path = manager.file_structure().symlinks.current(&identifier);
        tokio::fs::create_dir_all(current_path.parent().unwrap()).await.unwrap();
        crate::symlink::update(&package_root, &current_path).unwrap();

        let result = manager.installed_current_digest(&identifier).await;
        assert!(result.is_ok(), "garbage digest file must not return an error");
        assert!(result.unwrap().is_none(), "garbage digest file must return None");
    }
}
