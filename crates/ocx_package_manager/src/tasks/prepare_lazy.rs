// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shim-directory generation, the producer half of lazy package loading.
//!
//! Never touches `pull.rs`'s `setup_owned_impl`, or a materialized tool stops being byte-identical to an eager one.

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::PackageErrorKind;
use ocx_package::metadata::BinaryName;
use ocx_store::file_structure::{FileStructure, ShimDir};

use super::super::PackageManager;
use super::common::ClosureNode;
use super::lazy_advisory::{LazyAdvisory, classify_lazy_advisories};
use super::toolchain_names::{NotEnumerablePolicy, exposed_names};

/// Everything one [`PackageManager::prepare_lazy`] call produced.
#[derive(Debug)]
pub struct PreparedLazy {
    pub shim: ShimDir,
    /// The whole closure, sealed and private edges included, or the composer's conflict gate sees a truncated one.
    pub closure: Vec<ClosureNode>,
    pub advisories: Vec<LazyAdvisory>,
}

impl PackageManager {
    /// Publishes a deferred tool's shim directory without downloading content; idempotent under concurrency.
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::ShimNamesNotEnumerable`] — a closure node claims neither `binaries` nor entry points.
    /// - [`PackageErrorKind::ShimNameInvalid`] — an entry point name is not a valid [`BinaryName`] (e.g. `nul`).
    /// - [`PackageErrorKind::NotFound`] — unknown tag or digest, or closure metadata unavailable.
    /// - [`PackageErrorKind::Internal`] — offline policy block, staging or publication I/O failure.
    pub async fn prepare_lazy(
        &self,
        package: &ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
    ) -> Result<PreparedLazy, PackageErrorKind> {
        let (fs, index) = (self.file_structure(), self.index());
        let resolved = self.resolve(package, platform.clone()).await?;
        // Every blob `refs/blobs/` names must already be staged; the walk stages each dep, never the root.
        super::common::stage_chain_blobs(fs, index, &resolved).await?;
        super::common::stage_leaf_manifest(fs, index, &resolved.pinned).await?;

        let metadata = super::common::load_config_metadata(index, &resolved.pinned, &resolved.final_manifest).await?;
        let config_digest = super::common::config_blob_digest(&resolved.final_manifest)?;
        // The walk `inspect --deps` runs, never a second one, or the two can disagree on a closure.
        let nodes = super::common::walk_closure_nodes(
            fs,
            index,
            self.is_offline(),
            &resolved.pinned,
            &metadata,
            config_digest,
            &platform,
        )
        .await?;

        let destination = fs.shims.shim_dir(&resolved.pinned);

        // Probed here, or every warm `ocx env` stages and discards a full tree.
        if ocx_util::fs::path_exists_lossy(&destination.bin()).await {
            log::debug!("Reusing published shim dir {}", destination.root().display());
            return Ok(PreparedLazy {
                shim: destination,
                closure: nodes,
                advisories: classify_lazy_advisories(&resolved.pinned, &metadata),
            });
        }

        // `bin/` is the completeness marker, so it is written last or a refusal leaves a tree that reads complete.
        let staged = stage_shim_dir(fs).await?;
        let staged_dir = ShimDir {
            dir: staged.path().to_path_buf(),
            marker: "bin",
        };
        ocx_store::file_structure::write_digest_file(&staged_dir.digest_file(), &resolved.pinned.digest())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
        link_closure_config_blobs(fs, &staged_dir, &nodes).await?;

        // `Refuse`: a deferred tool has no fallback for an unenumerable node.
        let names = exposed_names(&nodes, NotEnumerablePolicy::Refuse)?;
        // A claimed `ocx` is admitted; `activation.rs`'s `is_ocx_trampoline` closes the self-resolution hazard.
        let names: BTreeSet<BinaryName> = names.into_keys().collect();
        write_shim_launchers(&staged_dir.bin(), &resolved.pinned, &names).await?;

        publish_shim_dir(&staged_dir, &destination).await?;

        Ok(PreparedLazy {
            shim: destination,
            closure: nodes,
            advisories: classify_lazy_advisories(&resolved.pinned, &metadata),
        })
    }
}

/// Creates a fresh, unique staging directory under `temp/`, discarded on drop.
///
/// Not `TempStore::path`'s identifier-keyed one: that is shared and locked, and publication here is lock-free.
///
/// # Errors
///
/// Returns an error if the temp root or the staging directory cannot be created.
async fn stage_shim_dir(file_structure: &FileStructure) -> Result<tempfile::TempDir, PackageErrorKind> {
    let root = file_structure.temp.root().to_path_buf();
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&root, e)))?;
    tokio::task::spawn_blocking({
        let root = root.clone();
        move || tempfile::TempDir::new_in(&root)
    })
    .await
    .map_err(|join_error| {
        PackageErrorKind::Internal(crate::error::file_error(&root, std::io::Error::other(join_error)))
    })?
    .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&root, e)))
}

/// Writes one launcher per name into the staged tree's `bin_dir`, all recording the pinned `package`.
///
/// Not `launcher::generate`: it takes `&Entrypoints`, and names like `c++` are not valid entry point names.
///
/// # Errors
///
/// Returns an error if creating `bin_dir` or writing any launcher fails.
async fn write_shim_launchers(
    bin_dir: &Path,
    package: &ocx_oci::PinnedPackageRef,
    names: &BTreeSet<BinaryName>,
) -> Result<(), PackageErrorKind> {
    // Even for no names: `bin/` is the completeness marker.
    tokio::fs::create_dir_all(bin_dir)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(bin_dir, e)))?;

    let record = crate::launcher::deferred_record(package).map_err(PackageErrorKind::Internal)?;
    let blob = crate::launcher::host_launcher_blob(bin_dir).map_err(PackageErrorKind::Internal)?;
    for name in names {
        crate::launcher::write_launcher(bin_dir, name.as_str(), blob, &record, cfg!(windows))
            .await
            .map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}

/// Links the closure's config blobs into the staged tree's `refs/blobs/`, their only GC root.
///
/// Not `ReferenceManager::link_blobs`: a shim tree is neither in `packages/` nor has a `content/`.
///
/// # Errors
///
/// Returns an error if creating `refs/blobs/` or writing any forward-ref fails.
async fn link_closure_config_blobs(
    file_structure: &FileStructure,
    staged: &ShimDir,
    nodes: &[ClosureNode],
) -> Result<(), PackageErrorKind> {
    let refs_blobs = staged.refs_blobs_dir();
    tokio::fs::create_dir_all(&refs_blobs)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&refs_blobs, e)))?;

    for node in nodes {
        let target = file_structure
            .blobs
            .data(node.identifier.registry(), &node.config_digest);
        let link = refs_blobs.join(ocx_store::file_structure::cas_ref_name(&node.config_digest));
        // `update`, not `create`: two nodes may share one config blob, or the second write fails `EEXIST`.
        ocx_util::fs::symlink::update(&target, &link).map_err(|error| PackageErrorKind::Internal(error.into()))?;
    }
    Ok(())
}

/// Publishes `staged` to `destination` by atomic rename, lock-free, converging when a concurrent call won.
///
/// # Errors
///
/// Returns an error if creating the parent fails, or the rename fails with the destination still absent.
async fn publish_shim_dir(staged: &ShimDir, destination: &ShimDir) -> Result<(), PackageErrorKind> {
    let marker = destination.bin();

    // Without this probe a lost race burns the Windows rename retry's whole backoff (same ERROR_ACCESS_DENIED).
    if ocx_util::fs::path_exists_lossy(&marker).await {
        discard_staged_tree(staged, "already published").await;
        return Ok(());
    }

    if let Some(parent) = destination.root().parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(parent, e)))?;
    }

    // Never `move_dir`: it `remove_dir_all`s the destination, deleting a live tree under a concurrent exec.
    match ocx_util::fs::rename_with_windows_retry(staged.root(), destination.root()).await {
        Ok(()) => {
            log::debug!("Published shim dir {}", destination.root().display());
            Ok(())
        }
        // The winner's tree is byte-identical (same digest, closure and bodies), so converge.
        Err(_) if ocx_util::fs::path_exists_lossy(&marker).await => {
            discard_staged_tree(staged, "lost the publish race").await;
            Ok(())
        }
        // Whatever blocked the rename is not a published tree, and not this call's to remove.
        Err(e) => Err(PackageErrorKind::Internal(crate::Error::InternalFile(
            staged.root().to_path_buf(),
            e,
        ))),
    }
}

/// Removes an already-published staged tree; a failure is only logged, or it fails a call that succeeded.
async fn discard_staged_tree(staged: &ShimDir, reason: &str) {
    log::debug!("Discarding staged shim tree ({reason}): {}", staged.root().display());
    if let Err(e) = tokio::fs::remove_dir_all(staged.root()).await {
        log::debug!("Could not remove staged shim tree {}: {e}", staged.root().display());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use super::*;
    use ocx_package::metadata::Binaries;

    /// An arbitrary valid SHA-256 hex, built from a one-byte seed so each
    /// fixture node can carry a digest distinguishable from its neighbours'.
    fn digest_from(seed: &str) -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(seed.repeat(32))
    }

    fn pinned(repository: &str, seed: &str) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry(repository, "example.com").clone_with_digest(digest_from(seed)),
        )
        .expect("digest-bearing identifier is pinned")
    }

    fn binaries(names: &[&str]) -> Binaries {
        let set: BTreeSet<BinaryName> = names
            .iter()
            .map(|n| BinaryName::try_from(*n).expect("fixture binary name is valid"))
            .collect();
        Binaries::try_from(set).expect("fixture binaries claim is valid")
    }

    /// Same as [`node`] but with the config-blob digest decoupled from the
    /// node's own identity digest, so a ref-link assertion cannot pass by
    /// accidentally addressing the manifest instead of the config blob.
    fn node_with_config_digest(
        identifier: ocx_oci::PinnedPackageRef,
        config_digest: ocx_oci::Digest,
        is_root: bool,
    ) -> ClosureNode {
        ClosureNode {
            identifier,
            config_digest,
            effective_visibility: None,
            binaries: Some(binaries(&["tool"])),
            entrypoints: Vec::new(),
            env: Vec::new(),
            integrations: Vec::new(),
            dependencies: Vec::new(),
            is_root,
        }
    }

    fn shim_dir_at(path: PathBuf) -> ShimDir {
        ShimDir {
            dir: path,
            marker: "bin",
        }
    }

    /// Stages a shim tree at `dir` whose `bin/` holds one file named `marker`,
    /// so a publish assertion can tell one tree from another.
    fn stage_tree(dir: &Path, marker: &str) -> ShimDir {
        let staged = shim_dir_at(dir.to_path_buf());
        std::fs::create_dir_all(staged.bin()).expect("stage bin/");
        std::fs::write(staged.bin().join(marker), b"launcher").expect("stage marker");
        staged
    }

    /// Every path under `root`, so a "nothing else was created" assertion can
    /// name what it found.
    fn walk_paths(root: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path.clone());
                }
                found.push(path);
            }
        }
        found
    }

    // ── the generated launchers ──────────────────────────────

    /// One launcher and one record per name, written into `bin/` — and never at
    /// the shim dir's root, where `digest` and `refs` live.
    #[tokio::test]
    async fn write_shim_launchers_writes_one_launcher_per_name_under_bin() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/cmake", "a");
        let set: BTreeSet<BinaryName> = ["cmake", "ctest"]
            .into_iter()
            .map(|n| BinaryName::try_from(n).unwrap())
            .collect();

        write_shim_launchers(&staged.bin(), &package, &set)
            .await
            .expect("launchers are written");

        let launcher = |name: &str| {
            if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            }
        };
        for name in ["cmake", "ctest"] {
            assert!(staged.bin().join(launcher(name)).is_file(), "bin/{name} launcher");
            assert!(
                staged.bin().join(format!("{name}.launch")).is_file(),
                "bin/{name}.launch record"
            );
            assert!(
                !staged.root().join(name).exists(),
                "no launcher may sit at the shim dir root beside 'digest' and 'refs' (C-003)"
            );
        }
    }

    /// The record names the pinned identifier it was handed, advisory tag included, in the compact deferred form.
    #[tokio::test]
    async fn write_shim_launchers_records_the_pinned_identifier_for_each_name() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/cmake", "a");
        let set: BTreeSet<BinaryName> = std::iter::once(BinaryName::try_from("cmake").unwrap()).collect();

        write_shim_launchers(&staged.bin(), &package, &set)
            .await
            .expect("launchers are written");

        let raw = std::fs::read(staged.bin().join("cmake.launch")).expect("record exists");
        let expected = format!(
            "{{\"kind\":\"deferred\",\"schema_version\":1,\"identifier\":{}}}\n",
            serde_json::to_string(&package.to_string()).unwrap()
        );
        assert_eq!(String::from_utf8(raw).unwrap(), expected);
    }

    /// A launcher that is not executable is not on `PATH` in any useful sense, and each is a byte copy of the host blob.
    #[cfg(unix)]
    #[tokio::test]
    async fn write_shim_launchers_writes_executable_copies_of_the_host_blob() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/cmake", "a");
        let set: BTreeSet<BinaryName> = std::iter::once(BinaryName::try_from("cmake").unwrap()).collect();

        write_shim_launchers(&staged.bin(), &package, &set)
            .await
            .expect("launchers are written");

        let path = staged.bin().join("cmake");
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert_eq!(meta.nlink(), 1);
        assert!(
            std::fs::read(&path).unwrap() == ocx_store::shim::host_blob().expect("host blob"),
            "the launcher is the embedded host blob"
        );
    }

    /// A claimed name ending `.launch` would put its launcher over another name's record: it gets no launcher.
    #[tokio::test]
    async fn write_shim_launchers_skips_a_name_ending_in_the_record_suffix() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/cmake", "a");
        let set: BTreeSet<BinaryName> = ["cmake", "foo.launch"]
            .into_iter()
            .map(|n| BinaryName::try_from(n).unwrap())
            .collect();

        write_shim_launchers(&staged.bin(), &package, &set)
            .await
            .expect("launchers are written");

        let mut listing: Vec<String> = std::fs::read_dir(staged.bin())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        listing.sort();
        let expected: &[&str] = if cfg!(windows) {
            &["cmake.exe", "cmake.launch"]
        } else {
            &["cmake", "cmake.launch"]
        };
        assert_eq!(listing, expected);
    }

    // ── the config-blob forward-refs ─────────────────

    /// The ref-linking clause, and the guard `ClosureNode::config_digest`
    /// has been missing since the walker gained the field: every node's config
    /// blob — the root's included — is linked into the staged tree's
    /// `refs/blobs/`, and each link resolves to *that digest's* blob data.
    /// A wrong or dropped root digest reds here.
    #[tokio::test]
    async fn link_closure_config_blobs_links_every_nodes_config_blob_including_the_roots() {
        let tempdir = tempfile::tempdir().unwrap();
        let file_structure = FileStructure::with_root(tempdir.path().to_path_buf());
        let staged = shim_dir_at(tempdir.path().join("staged"));

        let root_config = digest_from("1");
        let dep_config = digest_from("2");
        let nodes = vec![
            node_with_config_digest(pinned("ns/zlib", "b"), dep_config.clone(), false),
            node_with_config_digest(pinned("ns/cmake", "a"), root_config.clone(), true),
        ];

        link_closure_config_blobs(&file_structure, &staged, &nodes)
            .await
            .expect("config blobs are ref-linked");

        for (registry_repo, config) in [("ns/cmake", &root_config), ("ns/zlib", &dep_config)] {
            let link = staged
                .refs_blobs_dir()
                .join(ocx_store::file_structure::cas_ref_name(config));
            assert!(
                ocx_util::fs::symlink::is_link(&link),
                "{registry_repo}'s config blob {config} must be forward-referenced at {}",
                link.display()
            );
            assert_eq!(
                std::fs::read_link(&link).expect("forward-ref resolves"),
                file_structure.blobs.data("example.com", config),
                "the forward-ref must target the blob store entry for {config}"
            );
        }
    }

    // ── lock-free, all-or-nothing publication ────────────────────────

    /// Steps (2) and (3): create the destination's parent, then rename
    /// onto an absent destination. And the lock-free clause: nothing resembling
    /// a lock file is left behind — `publish_shim_dir` has no locks root to
    /// write one into, so any lock it took would be a sidecar.
    #[tokio::test]
    async fn publish_shim_dir_renames_a_staged_tree_onto_an_absent_destination() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = stage_tree(&tempdir.path().join("staged"), "cmake");
        let destination = shim_dir_at(tempdir.path().join("shims/example.com/ns/cmake/sha256/aa/bb"));

        publish_shim_dir(&staged, &destination)
            .await
            .expect("an absent destination is published to");

        assert!(
            destination.bin().join("cmake").is_file(),
            "the staged tree must land whole at the destination"
        );
        assert!(
            !staged.root().exists(),
            "the staged tree must no longer be at its temp path"
        );

        let litter: Vec<_> = walk_paths(tempdir.path())
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(".lock") || n == "locks")
            })
            .collect();
        assert!(litter.is_empty(), "publication takes no lock, found: {litter:?}");
    }

    /// Step (1): a destination whose completeness marker is already
    /// present means a concurrent call won. Converge — return `Ok`, discard the
    /// temp, and leave the winner's tree **byte-for-byte as it was**. The last
    /// assertion is the one that would have caught `move_dir`, which
    /// `remove_dir_all`s its destination.
    #[tokio::test]
    async fn publish_shim_dir_leaves_a_published_destination_untouched() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = stage_tree(&tempdir.path().join("staged"), "loser");
        let destination = stage_tree(&tempdir.path().join("published"), "winner");

        publish_shim_dir(&staged, &destination)
            .await
            .expect("a lost race converges rather than failing");

        assert!(
            destination.bin().join("winner").is_file(),
            "the winner's tree must survive intact"
        );
        assert!(
            !destination.bin().join("loser").exists(),
            "the loser's tree must not overwrite the winner's"
        );
        assert!(!staged.root().exists(), "the losing temp tree must be discarded");
    }

    /// Step (4), the absent half: the marker is still absent after a
    /// failed rename, so the error propagates — and the destination that
    /// blocked the rename is left alone. `move_dir` would instead
    /// `remove_dir_all` it and report success, deleting data this call never
    /// published.
    #[tokio::test]
    async fn publish_shim_dir_never_deletes_a_destination_it_could_not_rename_onto() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = stage_tree(&tempdir.path().join("staged"), "cmake");

        // A destination that exists and is non-empty but carries no `bin/`:
        // the pre-check reads "absent", the rename then fails on a non-empty
        // directory, and the re-check still reads "absent".
        let destination = shim_dir_at(tempdir.path().join("half-built"));
        std::fs::create_dir_all(destination.root()).unwrap();
        std::fs::write(destination.root().join("digest"), b"sha256:...").unwrap();

        let error = publish_shim_dir(&staged, &destination)
            .await
            .expect_err("a rename failure with the marker still absent propagates");

        assert!(
            matches!(error, PackageErrorKind::Internal(_)),
            "expected an I/O failure, got {error:?}"
        );
        assert!(
            destination.root().join("digest").is_file(),
            "a destination this call did not publish must not be removed"
        );
    }

    // ── advisories have a return channel ───────────────────────

    /// Advisories are **returned**, never only logged — otherwise
    /// `--format json` has nothing to serialize. The walked closure is added to
    /// the same channel, so the composer does not walk it a
    /// second time. The channel is the return type, so this is where it is
    /// pinned; dropping either field stops this compiling.
    #[test]
    fn prepare_lazy_returns_the_closure_and_advisories_alongside_the_shim_dir() {
        async fn signature_binding(
            manager: &PackageManager,
            package: &ocx_oci::PackageRef,
            platform: ocx_oci::Platform,
        ) -> (Vec<ClosureNode>, Vec<LazyAdvisory>) {
            let PreparedLazy {
                shim,
                closure,
                advisories,
            } = manager.prepare_lazy(package, platform).await.expect("prepare_lazy");
            let _: ShimDir = shim;
            (closure, advisories)
        }

        // Referenced, never run: the assertion is that the annotated
        // destructuring above type-checks against `prepare_lazy`'s return.
        let _ = signature_binding;
    }
}
