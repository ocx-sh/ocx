// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use super::super::PackageManager;
use super::clean::{CollectedRoots, collect_project_roots};
use super::garbage_collection::GarbageCollector;
use super::resolve::{PatchRootScope, SitePatchRoots};

/// Whether a [`PurgeUnrooted`] run could trust its root set; callers must report the
/// two apart, as `retained` means opposite things under each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootSet {
    /// Every root was readable; a retained seed is genuinely held.
    Determinate,
    /// A live project's lock was unreadable, so nothing was tested or removed.
    Indeterminate,
}

/// What a [`PackageManager::purge_unrooted`] run did.
pub struct PurgeUnrooted {
    /// Object directories deleted, in order, including layers and blobs the seeds orphaned.
    pub removed: Vec<PathBuf>,
    /// Seeds left on disk: each held by some root under [`RootSet::Determinate`], every seed
    /// under [`RootSet::Indeterminate`].
    pub retained: Vec<ocx_oci::PinnedPackageRef>,
    pub root_set: RootSet,
}

impl PackageManager {
    /// Purges one object and its orphaned transitive dependencies, returning the deleted
    /// directories (empty if the object is still reachable).
    pub async fn purge(&self, identifier: &ocx_oci::PinnedPackageRef) -> crate::Result<Vec<PathBuf>> {
        let obj_dir = self.file_structure().packages.path(identifier);
        let gc = GarbageCollector::build(self.file_structure(), &[], &SitePatchRoots::default()).await?;
        gc.purge(&[obj_dir]).await
    }

    /// Batch [`purge`](Self::purge) with one reachability-graph build.
    pub async fn purge_all(&self, identifiers: &[ocx_oci::PinnedPackageRef]) -> crate::Result<Vec<PathBuf>> {
        let obj_dirs: Vec<PathBuf> = identifiers
            .iter()
            .map(|id| self.file_structure().packages.path(id))
            .collect();
        let gc = GarbageCollector::build(self.file_structure(), &[], &SitePatchRoots::default()).await?;
        gc.purge(&obj_dirs).await
    }

    /// Collects the given packages that nothing else holds, by `clean`'s reachability roots.
    ///
    /// An unreadable lock retains every seed, fail-closed like `clean`: over-retention
    /// recovers, a deleted lock-pinned package does not.
    pub async fn purge_unrooted(&self, identifiers: &[ocx_oci::PinnedPackageRef]) -> crate::Result<PurgeUnrooted> {
        let ocx_home = self.file_structure().root().to_path_buf();
        let project_roots = match collect_project_roots(&ocx_home, self.file_structure()).await? {
            CollectedRoots::Roots(roots) => roots,
            CollectedRoots::RetainAll => {
                return Ok(PurgeUnrooted {
                    removed: Vec::new(),
                    retained: identifiers.to_vec(),
                    root_set: RootSet::Indeterminate,
                });
            }
        };

        // `RecordedAndSnapshot`, as in `clean`, or a freeze's companion pins are purged once
        // `ocx patch sync` advances the record.
        let host_platform = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
        let patch_roots = self
            .resolve_site_patch_roots(&host_platform, PatchRootScope::RecordedAndSnapshot)
            .await?;
        let gc = GarbageCollector::build(self.file_structure(), &project_roots, &patch_roots).await?;
        let reachable = gc.reachable();

        let mut seeds: Vec<PathBuf> = Vec::new();
        let mut retained: Vec<ocx_oci::PinnedPackageRef> = Vec::new();
        for identifier in identifiers {
            let raw_path = self.file_structure().packages.path(identifier);
            // The graph is keyed by canonical paths; a raw path under a symlinked `$OCX_HOME` would
            // delete a reachable package.
            let canonical_path = dunce::canonicalize(&raw_path).unwrap_or_else(|error| {
                log::debug!("cannot canonicalize package path {}: {error}", raw_path.display());
                raw_path
            });
            if reachable.contains(&canonical_path) {
                retained.push(identifier.clone());
            } else {
                seeds.push(canonical_path);
            }
        }

        let removed = gc.purge(&seeds).await?;
        Ok(PurgeUnrooted {
            removed,
            retained,
            root_set: RootSet::Determinate,
        })
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_store::file_structure::FileStructure;
    use ocx_store::reference_manager::ReferenceManager;

    use super::*;

    /// The leaf digest [`LOCK_PINNING_CMAKE`] pins, and the digest every
    /// fixture package below is addressed by.
    const CMAKE_LEAF_HEX: &str = "aaaa0000000000000000000000000000000000000000000000000000000000bb";

    /// A second digest, for the package nothing pins.
    const SHFMT_LEAF_HEX: &str = "bbbb0000000000000000000000000000000000000000000000000000000000cc";

    const REGISTRY: &str = "localhost:5000";

    /// A minimal V3 lock naming `cmake` at [`CMAKE_LEAF_HEX`]. Written to
    /// `$OCX_HOME/ocx.lock`, which `collect_project_roots` adds as an implicit
    /// root without any project needing to be registered.
    const LOCK_PINNING_CMAKE: &str = r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:0000000000000000000000000000000000000000000000000000000000000000"
generated_by = "ocx test"
generated_at = "2026-01-01T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "localhost:5000/cmake"

[tool.platforms]
"linux/amd64" = "sha256:aaaa0000000000000000000000000000000000000000000000000000000000bb"
"#;

    /// An offline manager over `ocx_home` — `purge_unrooted` reads the store
    /// and the lock and never the network.
    fn make_manager(ocx_home: &Path) -> PackageManager {
        let file_structure = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(ocx_home.join("index")),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        PackageManager::new(file_structure, index, None, REGISTRY)
    }

    fn pinned_leaf(repository: &str, digest_hex: &str) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repository, REGISTRY)
                .clone_with_digest(ocx_oci::Digest::Sha256(digest_hex.to_string())),
        )
        .expect("a digest-addressed identifier is pinned by construction")
    }

    /// Materialize the package-store directory for `pinned`, with the
    /// `content/` child every real package carries, and return its canonical
    /// path (the key the reachability graph uses).
    async fn seed_package_dir(file_structure: &FileStructure, pinned: &ocx_oci::PinnedPackageRef) -> PathBuf {
        let package_dir = file_structure.packages.path(pinned);
        tokio::fs::create_dir_all(package_dir.join("content"))
            .await
            .expect("the fixture package directory must be creatable");
        dunce::canonicalize(&package_dir).unwrap_or(package_dir)
    }

    /// Write the install back-reference an `ocx package install` leaves behind,
    /// through the same [`ReferenceManager::link`] the install path uses, so
    /// the fixture is the shape production writes rather than a hand-rolled
    /// symlink pair.
    fn seed_candidate_symlink(file_structure: &FileStructure, repository: &str, tag: &str, package_dir: &Path) {
        let tagged = ocx_oci::PackageRef::new_registry(repository, REGISTRY).clone_with_tag(tag);
        let candidate = file_structure.symlinks.candidate(&tagged);
        std::fs::create_dir_all(
            candidate
                .parent()
                .expect("a candidate symlink path always has a parent"),
        )
        .expect("the candidate directory must be creatable");
        ReferenceManager::new(file_structure.clone())
            .link(&candidate, package_dir)
            .expect("linking the fixture candidate must succeed");
    }

    /// A package an install symlink holds is reported as retained and
    /// left on disk. This is the property that separates `--rm` from
    /// `purge_all`, which would have deleted it and left the symlink dangling.
    #[tokio::test(flavor = "multi_thread")]
    async fn purge_unrooted_retains_a_candidate_symlinked_seed() {
        let home = tempfile::tempdir().expect("tempdir");
        let manager = make_manager(home.path());
        let file_structure = manager.file_structure().clone();

        let pinned = pinned_leaf("shfmt", SHFMT_LEAF_HEX);
        let package_dir = seed_package_dir(&file_structure, &pinned).await;
        seed_candidate_symlink(&file_structure, "shfmt", "3.8", &package_dir);

        let purged = manager
            .purge_unrooted(std::slice::from_ref(&pinned))
            .await
            .expect("purge_unrooted must not error on a readable store");

        assert_eq!(
            purged.retained,
            vec![pinned],
            "an install-symlinked package is held and must be retained; removed: {:?}",
            purged.removed
        );
        assert_eq!(
            purged.root_set,
            RootSet::Determinate,
            "the root set was readable, so this retention is a real hold, not a declined guess"
        );
        assert!(
            purged.removed.is_empty(),
            "nothing may be removed: {:?}",
            purged.removed
        );
        assert!(
            package_dir.exists(),
            "the retained package directory must still be on disk"
        );
    }

    /// A package a lock pins is reported as retained and left on disk.
    /// The implicit `$OCX_HOME/ocx.lock` root is the global toolchain tier, and
    /// it reaches the graph only because the collector is built with
    /// `collect_project_roots` — which is what the mutation proof for this test
    /// removes.
    #[tokio::test(flavor = "multi_thread")]
    async fn purge_unrooted_retains_a_project_root_pinned_seed() {
        let home = tempfile::tempdir().expect("tempdir");
        tokio::fs::write(home.path().join("ocx.lock"), LOCK_PINNING_CMAKE)
            .await
            .expect("the fixture lock must be writable");
        let manager = make_manager(home.path());
        let file_structure = manager.file_structure().clone();

        let pinned = pinned_leaf("cmake", CMAKE_LEAF_HEX);
        let package_dir = seed_package_dir(&file_structure, &pinned).await;
        assert!(
            !file_structure.symlinks.root().exists(),
            "precondition: the lock pin is the ONLY thing holding this package"
        );

        let purged = manager
            .purge_unrooted(std::slice::from_ref(&pinned))
            .await
            .expect("purge_unrooted must not error on a readable store");

        assert_eq!(
            purged.retained,
            vec![pinned],
            "a lock-pinned package is held and must be retained; removed: {:?}",
            purged.removed
        );
        assert_eq!(
            purged.root_set,
            RootSet::Determinate,
            "the lock read fine: a retention reported as Indeterminate here would mean the \
             fixture took the fail-closed path instead of the one under test"
        );
        assert!(
            package_dir.exists(),
            "the retained package directory must still be on disk"
        );
    }

    /// The package `ocx package exec` pulled and nothing else holds is
    /// the one this deletes. Without this arm the feature does nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn purge_unrooted_deletes_an_unrooted_seed() {
        let home = tempfile::tempdir().expect("tempdir");
        let manager = make_manager(home.path());
        let file_structure = manager.file_structure().clone();

        let pinned = pinned_leaf("shfmt", SHFMT_LEAF_HEX);
        let package_dir = seed_package_dir(&file_structure, &pinned).await;

        let purged = manager
            .purge_unrooted(std::slice::from_ref(&pinned))
            .await
            .expect("purge_unrooted must not error on a readable store");

        assert!(
            purged.retained.is_empty(),
            "nothing holds this package, so nothing may be retained: {:?}",
            purged.retained
        );
        assert_eq!(
            purged.root_set,
            RootSet::Determinate,
            "a removal can only ever follow a determinate root set"
        );
        assert_eq!(
            purged.removed,
            vec![package_dir.clone()],
            "the unrooted package directory must be the one thing removed"
        );
        assert!(
            !package_dir.exists(),
            "the removed package directory must be gone from disk"
        );
    }

    /// An indeterminate root set retains every seed and removes nothing. The
    /// unreadable lock here is `$OCX_HOME/ocx.lock` as a *directory*: the
    /// implicit global root is loaded unconditionally, and reading a directory
    /// as a file is the non-`NotFound` I/O error that drives the fail-closed
    /// arm on every platform.
    #[tokio::test(flavor = "multi_thread")]
    async fn purge_unrooted_retains_everything_when_the_root_set_is_indeterminate() {
        let home = tempfile::tempdir().expect("tempdir");
        tokio::fs::create_dir_all(home.path().join("ocx.lock"))
            .await
            .expect("the unreadable-lock fixture must be creatable");
        let manager = make_manager(home.path());
        let file_structure = manager.file_structure().clone();

        let pinned = pinned_leaf("shfmt", SHFMT_LEAF_HEX);
        let package_dir = seed_package_dir(&file_structure, &pinned).await;

        assert!(
            matches!(
                collect_project_roots(home.path(), &file_structure).await,
                Ok(CollectedRoots::RetainAll)
            ),
            "precondition: this fixture must actually produce RetainAll, or the \
             assertions below pass for the ordinary unrooted reason instead"
        );

        let purged = manager
            .purge_unrooted(std::slice::from_ref(&pinned))
            .await
            .expect("an indeterminate root set is non-fatal");

        assert_eq!(
            purged.root_set,
            RootSet::Indeterminate,
            "the caller has to be able to tell this retention from a real hold: nothing holds \
             this package, and a warning that claims otherwise is false"
        );
        assert_eq!(
            purged.retained,
            vec![pinned],
            "RetainAll must retain every seed; removed: {:?}",
            purged.removed
        );
        assert!(
            purged.removed.is_empty(),
            "nothing may be removed: {:?}",
            purged.removed
        );
        assert!(package_dir.exists(), "the package directory must still be on disk");
    }
}
