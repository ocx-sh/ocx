// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Deferred-tree generation, the producer half of lazy package loading.
//!
//! Never touches `pull.rs`'s `setup_owned_impl`, or a materialized tool stops being byte-identical to an eager one.

use std::collections::BTreeSet;
use std::path::Path;

use crate::error::PackageErrorKind;
use ocx_package::metadata::BinaryName;
use ocx_store::file_structure::{FileStructure, LockedScratch, ShimDir};
use ocx_util::fs::{Published, Retired, publish_dir, retire_dir};

use super::super::PackageManager;
use super::common::ClosureNode;
use super::lazy_advisory::{LazyAdvisory, classify_lazy_advisories};
use super::toolchain_names::{NotEnumerablePolicy, exposed_names};
use crate::launcher::{host_generation, names_complete};

/// Everything one [`PackageManager::prepare_lazy`] call produced.
#[derive(Debug)]
pub struct PreparedLazy {
    pub shim: ShimDir,
    /// The whole closure, sealed and private edges included, or the composer's conflict gate sees a truncated one.
    pub closure: Vec<ClosureNode>,
    pub advisories: Vec<LazyAdvisory>,
}

impl PackageManager {
    /// Publishes a deferred tool's launcher tree under `deferred/<G>/` without downloading content; idempotent under
    /// concurrency.
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::ShimNamesNotEnumerable`] — a closure node claims neither `binaries` nor entry points.
    /// - [`PackageErrorKind::ShimNameInvalid`] — an entry point name is not a valid [`BinaryName`] (e.g. `nul`).
    /// - [`PackageErrorKind::NotFound`] — unknown tag or digest, or closure metadata unavailable.
    /// - [`PackageErrorKind::Internal`] — offline policy block, a host without a launcher blob, staging or publication I/O failure.
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

        // `Refuse`: a deferred tool has no fallback for an unenumerable node.
        let names = exposed_names(&nodes, NotEnumerablePolicy::Refuse)?;
        // A claimed `ocx` is admitted; `activation.rs`'s `is_ocx_trampoline` closes the self-resolution hazard.
        let names: BTreeSet<BinaryName> = names.into_keys().collect();

        let generation = host_generation().ok_or_else(|| {
            PackageErrorKind::Internal(crate::error::file_error(
                fs.deferred_root(),
                std::io::Error::other("no launcher blob for this host"),
            ))
        })?;
        let destination = fs.deferred(generation).shim_dir(&resolved.pinned);
        ensure_tree(fs, &destination, &resolved.pinned, &nodes, &names).await?;

        Ok(PreparedLazy {
            shim: destination,
            closure: nodes,
            advisories: classify_lazy_advisories(&resolved.pinned, &metadata),
        })
    }
}

fn internal(path: &Path, error: std::io::Error) -> PackageErrorKind {
    PackageErrorKind::Internal(crate::error::file_error(path, error))
}

/// Leaves a complete tree at `destination`: a complete one is kept (probed here, or every warm `ocx env` stages and
/// discards a full tree), an incomplete one is retired and rebuilt.
async fn ensure_tree(
    fs: &FileStructure,
    destination: &ShimDir,
    package: &ocx_oci::PinnedPackageRef,
    nodes: &[ClosureNode],
    names: &BTreeSet<BinaryName>,
) -> Result<(), PackageErrorKind> {
    if tree_complete(destination, names).await {
        log::debug!("Reusing published deferred tree {}", destination.root().display());
        return Ok(());
    }
    // Retired, never edited: a published tree is only ever replaced whole.
    if tokio::fs::symlink_metadata(destination.root()).await.is_ok() {
        log::warn!(
            "Replacing the incomplete deferred tree '{}'",
            destination.root().display()
        );
        match retire_dir(destination.root()).await {
            Ok(Retired::Removed | Retired::LeftForLater) => {}
            Err(error) => return Err(internal(destination.root(), error)),
        }
    }
    let staged = stage_tree(fs, destination.marker, package, nodes, names).await?;
    publish_staged(&staged, destination, names).await
}

async fn tree_complete(tree: &ShimDir, names: &BTreeSet<BinaryName>) -> bool {
    names_complete(&tree.bin(), names.iter().map(BinaryName::as_str)).await
}

/// Builds the tree in a locked scratch directory that `ocx clean` skips until the returned guard drops: `digest`,
/// `refs/blobs/`, then the launchers last, so a refusal part-way never leaves a tree that reads complete.
async fn stage_tree(
    fs: &FileStructure,
    marker: &'static str,
    package: &ocx_oci::PinnedPackageRef,
    nodes: &[ClosureNode],
    names: &BTreeSet<BinaryName>,
) -> Result<LockedScratch, PackageErrorKind> {
    let scratch = fs
        .temp
        .scratch("deferred")
        .await
        .map_err(|error| internal(fs.temp.root(), std::io::Error::other(error)))?;
    let staged = ShimDir {
        dir: scratch.path().to_path_buf(),
        marker,
    };
    ocx_store::file_structure::write_digest_file(&staged.digest_file(), &package.digest())
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    link_closure_config_blobs(fs, &staged, nodes).await?;
    write_shim_launchers(&staged.bin(), package, names).await?;
    Ok(scratch)
}

/// The commit point: one rename of the staged tree onto `destination`. A racer that won must have published a
/// complete tree.
async fn publish_staged(
    staged: &LockedScratch,
    destination: &ShimDir,
    names: &BTreeSet<BinaryName>,
) -> Result<(), PackageErrorKind> {
    // Parents are not publications: only the tree itself is renamed in.
    if let Some(parent) = destination.root().parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| internal(parent, error))?;
    }
    let published = publish_dir(staged.path(), destination.root())
        .await
        .map_err(|error| internal(destination.root(), error))?;
    if published == Published::AlreadyThere && !tree_complete(destination, names).await {
        return Err(internal(
            destination.root(),
            std::io::Error::other("another process published an incomplete deferred tree"),
        ));
    }
    log::debug!(
        "Published deferred tree {} ({published:?})",
        destination.root().display()
    );
    Ok(())
}

/// Writes one launcher per name into the staged tree's launcher directory `bin_dir`, all recording the pinned `package`.
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
    let record = crate::launcher::deferred_record(package).map_err(PackageErrorKind::Internal)?;
    crate::launcher::validate_for_write(&record, cfg!(windows), bin_dir).map_err(PackageErrorKind::Internal)?;

    // Even for no names: `bin/` is the completeness marker.
    tokio::fs::create_dir_all(bin_dir)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(bin_dir, e)))?;

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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

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

    /// A deferred identifier is pinned by type (`PinnedPackageRef` renders `<registry>/<repo>@sha256:<hex>`), so the
    /// record `deferred_record` builds always satisfies the launcher's `deferred` rule: the validation in the writer
    /// is defence in depth, and this pins that the rule holds for a representative identifier.
    #[test]
    fn a_deferred_record_of_a_pinned_identifier_validates_for_both_targets() {
        let record = crate::launcher::deferred_record(&pinned("ns/cmake", "a")).unwrap();
        for windows in [false, true] {
            crate::launcher::validate_for_write(&record, windows, Path::new("/ctx")).expect("valid");
        }
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

    // ── all-or-nothing publication under `deferred/<G>/` ────────────────────────────

    fn generation() -> Option<ocx_store::launcher_generation::LauncherGeneration> {
        ocx_store::launcher_generation::LauncherGeneration::host()
    }

    fn names_of(names: &[&str]) -> BTreeSet<BinaryName> {
        names.iter().map(|n| BinaryName::try_from(*n).unwrap()).collect()
    }

    /// Removes what `ocx clean` removes from `temp/`: every entry `stale_entries` offers.
    fn sweep_temp(fs: &FileStructure) {
        for entry in fs.temp.stale_entries().expect("temp listing") {
            let dir = match entry {
                ocx_store::file_structure::StaleEntry::Locked(acquired) => acquired.dir.dir.clone(),
                ocx_store::file_structure::StaleEntry::Orphan(dir) => dir,
            };
            std::fs::remove_dir_all(dir).expect("sweep removes a stale entry");
        }
    }

    /// V-L26: a tree being staged survives the sweep `ocx clean` runs over `temp/`, and the publish then lands at
    /// `deferred/<G>/<reg>/<repo>/<alg>/<2>/<30>/` with `digest`, `refs/blobs/` and `l/`; nothing is written to `shims/`.
    #[tokio::test]
    async fn a_deferred_tree_being_staged_survives_the_temp_sweep_and_publishes_under_deferred() {
        let Some(generation) = generation() else { return };
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let package = pinned("ns/cmake", "a");
        let nodes = vec![node_with_config_digest(package.clone(), digest_from("1"), true)];
        let names = names_of(&["cmake"]);
        let destination = fs.deferred(&generation).shim_dir(&package);

        let staged = stage_tree(&fs, destination.marker, &package, &nodes, &names)
            .await
            .expect("staging");
        sweep_temp(&fs);
        assert!(
            staged.path().join("l").join("cmake.launch").is_file(),
            "ocx clean must not delete a tree that is being staged"
        );

        publish_staged(&staged, &destination, &names).await.expect("publish");

        let tree = fs
            .root()
            .join("deferred")
            .join(generation.as_str())
            .join("example.com")
            .join("ns")
            .join("cmake")
            .join("sha256")
            .join("aa")
            .join("a".repeat(30));
        assert_eq!(destination.root(), tree);
        assert!(tree.join("l").join("cmake.launch").is_file());
        assert!(tree.join("digest").is_file());
        assert!(tree.join("refs").join("blobs").is_dir());
        assert!(!fs.root().join("shims").exists(), "nothing is written under shims/");
    }

    /// A tree missing a record is retired and rebuilt; a complete one is returned as it is.
    #[tokio::test]
    async fn ensure_tree_rebuilds_an_incomplete_tree_and_keeps_a_complete_one() {
        let Some(generation) = generation() else { return };
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let package = pinned("ns/cmake", "a");
        let nodes = vec![node_with_config_digest(package.clone(), digest_from("1"), true)];
        let names = names_of(&["cmake"]);
        let destination = fs.deferred(&generation).shim_dir(&package);
        std::fs::create_dir_all(destination.bin()).unwrap();
        std::fs::write(destination.bin().join("stale"), b"x").unwrap();

        ensure_tree(&fs, &destination, &package, &nodes, &names)
            .await
            .expect("rebuild");
        assert!(destination.bin().join("cmake.launch").is_file());
        assert!(
            !destination.bin().join("stale").exists(),
            "the incomplete tree was replaced whole"
        );

        std::fs::write(destination.bin().join("keep"), b"x").unwrap();
        ensure_tree(&fs, &destination, &package, &nodes, &names)
            .await
            .expect("reuse");
        assert!(
            destination.bin().join("keep").is_file(),
            "a complete tree is not touched"
        );
    }

    /// A tree another process published incomplete is an error, never converged on.
    #[tokio::test]
    async fn publish_staged_refuses_an_incomplete_winner() {
        let Some(generation) = generation() else { return };
        let tempdir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(tempdir.path().to_path_buf());
        let package = pinned("ns/cmake", "a");
        let names = names_of(&["cmake"]);
        let destination = fs.deferred(&generation).shim_dir(&package);
        std::fs::create_dir_all(destination.bin()).unwrap();
        std::fs::write(destination.bin().join("other"), b"x").unwrap();
        let staged = stage_tree(&fs, destination.marker, &package, &[], &names)
            .await
            .unwrap();

        let error = publish_staged(&staged, &destination, &names)
            .await
            .expect_err("an incomplete winner is refused");

        assert!(matches!(error, PackageErrorKind::Internal(_)), "{error:?}");
        assert!(
            destination.bin().join("other").is_file(),
            "the destination is left alone"
        );
    }

    /// Golden: the exact bytes of a deferred record, and the launcher's own decoder reads them back as deferred.
    #[test]
    fn the_deferred_record_is_the_exact_compact_form_and_decodes_as_deferred() {
        let package = ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("ns/cmake", "example.com")
                .clone_with_digest(ocx_oci::Digest::Sha256("ab".repeat(32))),
        )
        .unwrap();
        let bytes = crate::launcher::deferred_record(&package).unwrap().encode();

        let expected = format!(
            "{{\"kind\":\"deferred\",\"schema_version\":1,\"identifier\":\"example.com/ns/cmake@sha256:{}\"}}\n",
            "ab".repeat(32)
        );
        assert_eq!(String::from_utf8(bytes.clone()).unwrap(), expected);
        let decoded = ocx_env_apply::launch::LaunchRecord::decode(&bytes, false).expect("the launcher accepts it");
        assert_eq!(decoded.kind(), ocx_env_apply::launch::LaunchKind::Deferred);
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
