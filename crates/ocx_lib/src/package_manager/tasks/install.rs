// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    oci,
    package::install_info::InstallInfo,
    package_manager::{self, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

impl PackageManager {
    /// Downloads a package and creates install symlinks.
    ///
    /// Delegates to [`PackageManager::pull`] for the actual download and
    /// transitive dependency resolution (see that method for concurrency
    /// safety), then optionally creates:
    ///
    /// - A **candidate** symlink at `symlinks/{repo}/candidates/{tag}` when
    ///   `candidate` is `true` — pins this version as an installed candidate.
    /// - A **current** symlink at `symlinks/{repo}/current` when `select` is
    ///   `true` — makes this version the active selection.
    ///
    /// Both symlinks target the package root; consumers traverse into
    /// `<symlink>/content/`, `<symlink>/entrypoints/`, or `<symlink>/metadata.json`.
    /// Symlinks are managed via [`ReferenceManager::link`] which also creates
    /// back-references in the object store for GC tracking.
    pub async fn install(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
    ) -> Result<InstallInfo, PackageErrorKind> {
        let install_info = self.pull(package, platforms).await?;

        create_install_symlinks(self, package, &install_info, candidate, select).await?;

        Ok(install_info)
    }

    /// Installs multiple packages in parallel using a shared singleflight
    /// group for cross-package diamond dependency deduplication.
    ///
    /// Phase 1: [`pull_all`](PackageManager::pull_all) downloads all packages
    /// and their transitive deps with a shared singleflight group.
    /// Phase 2: Install symlinks are created sequentially (cheap I/O, no
    /// contention benefit from parallelism).
    pub async fn install_all(
        &self,
        packages: Vec<oci::Identifier>,
        platforms: Vec<oci::Platform>,
        candidate: bool,
        select: bool,
    ) -> Result<Vec<InstallInfo>, package_manager::error::Error> {
        // Phase 1: Pull all packages with shared singleflight group.
        let infos = self.pull_all(&packages, platforms).await?;

        // Phase 2: Create symlinks sequentially.
        if candidate || select {
            for (pkg, info) in packages.iter().zip(infos.iter()) {
                create_install_symlinks(self, pkg, info, candidate, select)
                    .await
                    .map_err(|kind| {
                        package_manager::error::Error::InstallFailed(vec![PackageError::new(pkg.clone(), kind)])
                    })?;
            }
        }

        Ok(infos)
    }
}

/// Creates candidate and/or current symlinks for a single package.
///
/// Delegates to [`super::common::wire_selection`] for the `current` symlink
/// update plus the per-registry entry-points index update. Collision
/// detection, lock acquisition, and rollback all live in the shared helper so
/// this path and the `command/select.rs` path stay byte-equivalent.
#[allow(clippy::result_large_err)]
async fn create_install_symlinks(
    mgr: &PackageManager,
    package: &oci::Identifier,
    info: &InstallInfo,
    candidate: bool,
    select: bool,
) -> Result<(), PackageErrorKind> {
    super::common::wire_selection(mgr.file_structure(), package, info, candidate, select).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::oci;
    use crate::package::metadata::entry_point::{EntryPoint, EntryPointName, EntryPoints};

    // ── helpers ─────────────────────────────────────────────────────────────

    fn make_ep(name: &str) -> EntryPoints {
        let entries = vec![EntryPoint {
            name: EntryPointName::try_from(name).unwrap(),
            target: format!("${{installPath}}/bin/{name}"),
        }];
        EntryPoints::new(entries).unwrap()
    }

    /// PackageDir::entrypoints() returns sibling of content/ — verify path shape.
    #[test]
    fn package_dir_entrypoints_path_is_sibling_of_content() {
        use crate::file_structure::PackageDir;
        let dir = std::path::PathBuf::from("/packages/sha256/ab/cdef");
        let pkg_dir = PackageDir { dir };
        assert_eq!(
            pkg_dir.entrypoints(),
            std::path::PathBuf::from("/packages/sha256/ab/cdef/entrypoints")
        );
        assert_eq!(
            pkg_dir.content().parent(),
            pkg_dir.entrypoints().parent(),
            "entrypoints must be sibling of content"
        );
    }

    /// Helper: drive `wire_selection` directly from a synthetic InstallInfo so
    /// we can exercise the new index-mediated collision detection without a
    /// full pull/install pipeline.
    fn make_install_info(
        registry: &str,
        repository: &str,
        digest_hex: &str,
        content_path: std::path::PathBuf,
        entry_points: EntryPoints,
    ) -> crate::package::install_info::InstallInfo {
        use crate::package::install_info::InstallInfo;
        use crate::package::metadata::Metadata;
        use crate::package::resolved_package::ResolvedPackage;

        let id = oci::Identifier::new_registry(repository, registry)
            .clone_with_digest(oci::Digest::Sha256(digest_hex.to_string()));
        let pinned = oci::PinnedIdentifier::try_from(id).unwrap();
        let json = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "entry_points": entry_points,
        })
        .to_string();
        let metadata: Metadata = serde_json::from_str(&json).unwrap();

        InstallInfo {
            identifier: pinned,
            metadata,
            resolved: ResolvedPackage::new(),
            content: content_path,
        }
    }

    /// Build a content directory under `{root}/packages/...`. The `current`
    /// and `candidates/{tag}` symlinks target the package root (parent of
    /// `content/`); `ReferenceManager::link` walks into `<pkg_root>/refs/symlinks/`
    /// for back-ref creation, which `link` itself creates if missing.
    fn make_pkg_content(root: &std::path::Path, prefix: u32) -> std::path::PathBuf {
        let pkg = root
            .join("packages")
            .join("reg")
            .join("sha256")
            .join(format!("{prefix:02x}"))
            .join("aabb1122ccdd3344eeff5566778899");
        let content = pkg.join("content");
        std::fs::create_dir_all(&content).unwrap();
        content
    }

    // ── plan_review_findings_pr64 §3.7–3.8: collision identifier + scan depth ──

    /// Plan §3.7 (R2.2): when a select-time collision is detected, the
    /// `EntryPointNameCollision::existing_package` field must carry a valid OCI
    /// identifier formed from `<registry>/<repository>` — not a filesystem path
    /// (the prior implementation stuffed `repo_dir.to_string_lossy()` into
    /// `repository`). The new index-mediated path stores `(registry, repository)`
    /// as structured fields so the recovered identifier is unambiguous.
    #[tokio::test]
    async fn collision_error_emits_oci_repo_not_filesystem_path() {
        use crate::file_structure::FileStructure;
        use crate::package_manager::error::PackageErrorKind;

        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let fs = FileStructure::with_root(root.clone());

        // Owner first: install --select on `ownerorg/cmake` writes the index
        // entry binding "cmake" to that repo.
        let owner_content = make_pkg_content(&root, 0xc1);
        let owner_info = make_install_info(
            "example.com",
            "ownerorg/cmake",
            &"a".repeat(64),
            owner_content,
            make_ep("cmake"),
        );
        let owner_id: oci::Identifier = owner_info.identifier.clone().into();
        super::super::common::wire_selection(&fs, &owner_id, &owner_info, false, true)
            .await
            .expect("owner wire_selection");

        // Newcomer in a different repo: must collide on "cmake".
        let new_content = make_pkg_content(&root, 0xc2);
        let new_info = make_install_info(
            "example.com",
            "neworg/cmake",
            &"b".repeat(64),
            new_content,
            make_ep("cmake"),
        );
        let new_id: oci::Identifier = new_info.identifier.clone().into();
        let result = super::super::common::wire_selection(&fs, &new_id, &new_info, false, true).await;

        let err = result.expect_err("collision must be detected");
        match err {
            PackageErrorKind::EntryPointNameCollision { name, existing_package } => {
                assert_eq!(name, "cmake");
                // Repo name must NOT be a filesystem path: no leading `/`,
                // no backslash separators, no embedded registry slug.
                assert!(
                    !existing_package.repository().starts_with('/'),
                    "repository must not be an absolute path: {}",
                    existing_package.repository(),
                );
                assert!(
                    !existing_package.repository().contains('\\'),
                    "repository must not contain Windows-style separators: {}",
                    existing_package.repository(),
                );
                assert!(
                    !existing_package.repository().contains("example.com"),
                    "repository must not embed the registry slug: {}",
                    existing_package.repository(),
                );
                assert_eq!(
                    existing_package.repository(),
                    "ownerorg/cmake",
                    "repository must equal the owning OCI repo name",
                );
                assert_eq!(
                    existing_package.registry(),
                    "example.com",
                    "registry must be carried verbatim",
                );
            }
            other => panic!("expected EntryPointNameCollision, got {other:?}"),
        }
    }

    /// Plan §3.8 (R2.3): collision detection must reach repos at any depth
    /// under the registry. A repo at `<host>/<org>/<sub>/<repo>` (3 segments
    /// deep under the registry slug) must surface a collision when its
    /// launcher name matches a newcomer in a different repo at any depth.
    /// The index-based path is depth-agnostic by construction: ownership is
    /// keyed by `(registry, repository)`, not by directory walk.
    #[tokio::test]
    async fn collision_check_finds_collision_at_depth_three() {
        use crate::file_structure::FileStructure;
        use crate::package_manager::error::PackageErrorKind;

        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let fs = FileStructure::with_root(root.clone());

        // Owner is nested three levels deep: org/sub/cmake.
        let owner_content = make_pkg_content(&root, 0xd1);
        let owner_info = make_install_info(
            "example.com",
            "org/sub/cmake",
            &"a".repeat(64),
            owner_content,
            make_ep("cmake"),
        );
        let owner_id: oci::Identifier = owner_info.identifier.clone().into();
        super::super::common::wire_selection(&fs, &owner_id, &owner_info, false, true)
            .await
            .expect("owner wire_selection at depth 3");

        // Newcomer at a different (and shorter) path collides on "cmake".
        let new_content = make_pkg_content(&root, 0xd2);
        let new_info = make_install_info(
            "example.com",
            "other/cmake",
            &"b".repeat(64),
            new_content,
            make_ep("cmake"),
        );
        let new_id: oci::Identifier = new_info.identifier.clone().into();
        let result = super::super::common::wire_selection(&fs, &new_id, &new_info, false, true).await;

        match result {
            Err(PackageErrorKind::EntryPointNameCollision { name, .. }) => {
                assert_eq!(name, "cmake");
            }
            other => panic!(
                "expected EntryPointNameCollision at depth 3, got {other:?} \
                 — index-mediated check must be depth-agnostic",
            ),
        }
    }

    /// Re-selecting the SAME package must not collide with itself — repeated
    /// `wire_selection` for the same `(registry, repository)` is idempotent.
    #[tokio::test]
    async fn wire_selection_idempotent_for_same_package() {
        use crate::file_structure::FileStructure;

        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let fs = FileStructure::with_root(root.clone());

        let content = make_pkg_content(&root, 0xe1);
        let info = make_install_info("example.com", "myorg/cmake", &"a".repeat(64), content, make_ep("cmake"));
        let id: oci::Identifier = info.identifier.clone().into();

        super::super::common::wire_selection(&fs, &id, &info, false, true)
            .await
            .expect("first select");
        super::super::common::wire_selection(&fs, &id, &info, false, true)
            .await
            .expect("re-select must not collide with self");
    }
}
