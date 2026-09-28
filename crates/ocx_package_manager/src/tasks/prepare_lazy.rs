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
        };
        ocx_store::file_structure::write_digest_file(&staged_dir.digest_file(), &resolved.pinned.digest())
            .await
            .map_err(|error| PackageErrorKind::Internal(error.into()))?;
        link_closure_config_blobs(fs, &staged_dir, &nodes).await?;

        // `Refuse`: a deferred tool has no fallback for an unenumerable node.
        let names = exposed_names(&nodes, NotEnumerablePolicy::Refuse)?;
        // A claimed `ocx` is admitted; `activation.rs`'s `is_ocx_trampoline` closes the self-resolution hazard.
        let names: BTreeSet<BinaryName> = names.into_keys().collect();
        write_shim_launchers(&staged_dir.bin(), &resolved.pinned, &names, &fs.shim_bin).await?;

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

/// Writes one `ocx launcher shim` launcher per name into the staged tree's `bin_dir`.
///
/// Not `launcher::generate`: it takes `&Entrypoints`, and names like `c++` are not valid entry point names.
///
/// # Errors
///
/// Returns an error if creating `bin_dir` or writing any launcher fails.
#[cfg_attr(
    not(windows),
    expect(
        unused_variables,
        reason = "shim_bin feeds the Windows shim-slot producer only (C-026)"
    )
)]
async fn write_shim_launchers(
    bin_dir: &Path,
    package: &ocx_oci::PinnedPackageRef,
    names: &BTreeSet<BinaryName>,
    shim_bin: &ocx_store::file_structure::ShimBinStore,
) -> Result<(), PackageErrorKind> {
    // Even for no names: `bin/` is the completeness marker.
    tokio::fs::create_dir_all(bin_dir)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(bin_dir, e)))?;

    // One body for every name; `launcher::shim_body` is the sole producer of the wire token.
    let body = crate::launcher::shim_body(package).map_err(PackageErrorKind::Internal)?;

    for name in names {
        let path = bin_dir.join(name.as_str());
        tokio::fs::write(&path, body.as_bytes())
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&path, e)))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .await
                .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&path, e)))?;
        }
        #[cfg(windows)]
        write_windows_shim_slot(bin_dir, name, package, shim_bin).await?;
    }
    Ok(())
}

/// Hardlinks `<name>.exe` from `shim_bin` and writes its `<name>.shimref` sidecar (`adr_windows_exe_shim.md`).
///
/// Compiled only for Windows, so a host `cargo check` never type-checks it.
///
/// # Errors
///
/// Returns an error if publishing, hardlinking (incl. `EEXIST`, cross-device) or writing the sidecar fails.
#[cfg(windows)]
async fn write_windows_shim_slot(
    bin_dir: &Path,
    name: &BinaryName,
    package: &ocx_oci::PinnedPackageRef,
    shim_bin: &ocx_store::file_structure::ShimBinStore,
) -> Result<(), PackageErrorKind> {
    let exe_path = bin_dir.join(format!("{}.exe", name.as_str()));
    // `.shimref`, never `.shim`, which names an installed package's sidecar in another grammar.
    let shimref_path = bin_dir.join(format!("{}.shimref", name.as_str()));

    let shim_bin_path = shim_bin
        .ensure()
        .await
        .map_err(|error| PackageErrorKind::Internal(error.into()))?;
    // `create`, never `update`: an occupied slot in a fresh tree is a bug to surface, not converge on.
    ocx_store::hardlink::create(&shim_bin_path, &exe_path).map_err(|error| PackageErrorKind::Internal(error.into()))?;

    // Exactly `<pinned identifier>\n`, the shape `ocx_shim::core::parse_shimref_sidecar` reads back.
    let body = format!("{package}\n");
    tokio::fs::write(&shimref_path, body.as_bytes())
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&shimref_path, e)))?;

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

    /// The exact pinned identifier `ocx_shim::core`'s test module names
    /// `PINNED_IDENTIFIER` — `ocx.sh/tool/cmake:3.28@sha256:0000…0001`. A
    /// dedicated fixture rather than a call through [`pinned`]/[`digest_from`]:
    /// those two build a `ns/<repo>@example.com` identifier with no tag and a
    /// seed-repeated digest, which cannot express this literal's registry,
    /// repository, tag, or trailing-`1` digest without widening a helper every
    /// other test in this module also uses.
    #[cfg(windows)]
    fn golden_pinned() -> ocx_oci::PinnedPackageRef {
        let digest = ocx_oci::Digest::Sha256(format!("{}1", "0".repeat(63)));
        ocx_oci::PinnedPackageRef::try_from(
            ocx_oci::PackageRef::new_registry("tool/cmake", "ocx.sh")
                .clone_with_tag("3.28")
                .clone_with_digest(digest),
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
        ShimDir { dir: path }
    }

    /// A scratch [`ocx_store::file_structure::ShimBinStore`] rooted under the
    /// test's own tempdir — never the real `$OCX_HOME`, matching
    /// `launcher::generate`'s own test fixture.
    fn shim_bin_store(tmp: &Path) -> ocx_store::file_structure::ShimBinStore {
        ocx_store::file_structure::ShimBinStore::new(tmp.join("shim_bin"))
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

    /// One artifact per name, written into `bin/` — and never at
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
        let shim_bin = shim_bin_store(tempdir.path());

        write_shim_launchers(&staged.bin(), &package, &set, &shim_bin)
            .await
            .expect("launchers are written");

        for name in ["cmake", "ctest"] {
            assert!(
                staged.bin().join(name).is_file(),
                "bin/{name} must hold a generated launcher"
            );
            assert!(
                !staged.root().join(name).exists(),
                "no launcher may sit at the shim dir root beside 'digest' and 'refs' (C-003)"
            );
        }
    }

    /// The body dispatches through `ocx launcher shim '<pinned-id>'`.
    /// The byte-exact template is pinned elsewhere; what this pins is that
    /// the *pinned identifier it was handed* is the one baked in.
    #[cfg(unix)]
    #[tokio::test]
    async fn write_shim_launchers_bakes_the_pinned_identifier_into_each_body() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/cmake", "a");
        let set: BTreeSet<BinaryName> = std::iter::once(BinaryName::try_from("cmake").unwrap()).collect();
        let shim_bin = shim_bin_store(tempdir.path());

        write_shim_launchers(&staged.bin(), &package, &set, &shim_bin)
            .await
            .expect("launchers are written");

        let body = std::fs::read_to_string(staged.bin().join("cmake")).expect("launcher body is UTF-8");
        assert!(
            body.contains(&package.to_string()),
            "the launcher must name the package it triggers, got:\n{body}"
        );
        assert!(
            body.contains("launcher shim"),
            "the launcher must dispatch through the `launcher shim` verb (C-010), got:\n{body}"
        );
    }

    /// A shim artifact that is not executable is not on `PATH` in any
    /// useful sense. Same obligation the entry-point generator already carries
    /// (`launcher::generate` writes mode 0755).
    #[cfg(unix)]
    #[tokio::test]
    async fn write_shim_launchers_marks_each_launcher_executable() {
        use std::os::unix::fs::PermissionsExt;

        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/cmake", "a");
        let set: BTreeSet<BinaryName> = std::iter::once(BinaryName::try_from("cmake").unwrap()).collect();
        let shim_bin = shim_bin_store(tempdir.path());

        write_shim_launchers(&staged.bin(), &package, &set, &shim_bin)
            .await
            .expect("launchers are written");

        let mode = std::fs::metadata(staged.bin().join("cmake"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "a generated launcher must be executable");
    }

    // ── the Windows shim slot ────────────────────────────────────────
    //
    // EVERY test in this section is `#[cfg(windows)]`, because
    // `write_windows_shim_slot` is. **No gate in this repository compiles
    // them** (R-W9): `task rust:check:windows-cfg` is scoped to `ocx_shim`,
    // and `cargo check -p ocx_lib --target x86_64-pc-windows-msvc` dies in
    // `aws-lc-sys` on a non-MSVC host. They are written from the contract
    // above and the `.shimref` grammar `ocx_shim::core::parse_shimref_sidecar`
    // already enforces, and the Implement stage must type-check the arm by
    // hand before merging (see the Specify report's R-W9 procedure).

    /// The five read-side rules `ocx_shim::core::parse_one_line` applies to a
    /// `.shimref`, plus its pinned-identifier clause, asserted against the
    /// bytes this producer writes.
    ///
    /// Restated here rather than imported: `ocx_lib` cannot depend on
    /// `ocx_shim` (the shim is a standalone binary crate with no library
    /// surface `ocx_lib` may link), so producer and reader are bound by a
    /// paired golden for the `launcher shim`
    /// wire token, the same treatment applied elsewhere in this file.
    #[cfg(windows)]
    fn assert_shimref_grammar(raw: &[u8], expected: &ocx_oci::PinnedPackageRef) {
        assert!(raw.len() <= 32 * 1024, "a .shimref must fit the reader's 32 KiB cap");
        let (line, terminator) = raw.split_at(raw.len() - 1);
        assert_eq!(
            terminator, b"\n",
            "exactly one trailing newline, and it is the last byte"
        );
        assert!(!line.is_empty(), "non-empty after the terminator is stripped");
        assert!(
            !line.iter().any(|b| matches!(b, 0x00 | 0x0A | 0x0D)),
            "no NUL and no interior line terminator"
        );
        let line = std::str::from_utf8(line).expect("valid UTF-8");
        assert!(
            line.bytes().all(|b| (0x21..=0x7E).contains(&b)),
            "printable ASCII only — no space, no DEL, nothing non-ASCII: {line}"
        );
        assert!(!line.starts_with('-'), "no leading dash, or ocx reads it as a flag");
        let (_, digest) = line.rsplit_once('@').expect("digest-bearing");
        let (algorithm, hex) = digest.split_once(':').expect("<algorithm>:<hex>");
        assert!(
            !algorithm.is_empty() && algorithm.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
            "algorithm is [a-z0-9]+, got {algorithm}"
        );
        assert!(
            !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "hex is [0-9a-f]+, got {hex}"
        );
        assert_eq!(
            line,
            expected.to_string(),
            "the sidecar names the pinned identifier it was given"
        );
    }

    /// The slot is `<name>.exe` — a hardlink of the
    /// published shim blob — plus `<name>.shimref` holding one line, the
    /// pinned identifier. And **no `<name>.shim`**: `SIDECAR_PROBE_ORDER`
    /// probes `shim` before `shimref`, so one stray `.shim` in a shim tree's
    /// `bin/` silently switches dispatch to `launcher exec` against a package
    /// root that does not exist.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_windows_shim_slot_writes_an_exe_and_a_shimref_and_never_a_shim() {
        let tempdir = tempfile::tempdir().unwrap();
        let bin_dir = tempdir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let package = pinned("ns/cmake", "a");
        let shim_bin = shim_bin_store(tempdir.path());

        write_windows_shim_slot(&bin_dir, &BinaryName::try_from("cmake").unwrap(), &package, &shim_bin)
            .await
            .expect("the slot is written");

        assert!(bin_dir.join("cmake.exe").is_file(), "the slot needs its .exe");
        assert_shimref_grammar(
            &std::fs::read(bin_dir.join("cmake.shimref")).expect("the sidecar exists"),
            &package,
        );
        assert!(
            !bin_dir.join("cmake.shim").exists(),
            "a .shim here would divert dispatch to `launcher exec` (C-026)"
        );
    }

    /// The paired-golden treatment applied to the produced
    /// `.shimref` bytes against a literal, not merely against `expected`'s own
    /// `to_string()` (as `assert_shimref_grammar` does above) — a producer
    /// that quietly changed the wire shape while staying consistent with
    /// itself would still pass that check. The literal is [`golden_pinned`]'s
    /// own value, converged onto `ocx_shim::core::tests::PINNED_IDENTIFIER`
    /// so the two halves of the paired golden assert the same bytes; the
    /// reader side restates it independently
    /// (`ocx_shim::core::parse_shimref_sidecar`), byte-for-byte.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_windows_shim_slot_shimref_is_byte_exact_against_the_golden_literal() {
        let tempdir = tempfile::tempdir().unwrap();
        let bin_dir = tempdir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let package = golden_pinned();
        let shim_bin = shim_bin_store(tempdir.path());

        write_windows_shim_slot(&bin_dir, &BinaryName::try_from("cmake").unwrap(), &package, &shim_bin)
            .await
            .expect("the slot is written");

        let raw = std::fs::read(bin_dir.join("cmake.shimref")).expect("the sidecar exists");
        assert_eq!(
            raw, b"ocx.sh/tool/cmake:3.28@sha256:0000000000000000000000000000000000000000000000000000000000000001\n",
            "byte-exact golden (RUL-35) — WP-6 restates this literal on the reader side"
        );
    }

    /// `<name>.exe` is a **hardlink** of the
    /// store's published blob, not a copy — one inode per store, which is the
    /// property #301 exists for and what keeps an `ocx` upgrade or a re-sign
    /// reaching every generated `.exe`.
    ///
    /// Byte-equality alone cannot tell a hardlink from a copy, so the store's
    /// blob is mutated after the link and read back through the link. That is
    /// the discriminator; nothing weaker is one.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_windows_shim_slot_hardlinks_the_exe_rather_than_copying_it() {
        let tempdir = tempfile::tempdir().unwrap();
        let bin_dir = tempdir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let package = pinned("ns/cmake", "a");
        let shim_bin = shim_bin_store(tempdir.path());

        write_windows_shim_slot(&bin_dir, &BinaryName::try_from("cmake").unwrap(), &package, &shim_bin)
            .await
            .expect("the slot is written");

        let blob = shim_bin.ensure().await.expect("the blob is published");
        let linked = bin_dir.join("cmake.exe");
        assert_eq!(
            std::fs::read(&blob).unwrap(),
            std::fs::read(&linked).unwrap(),
            "the link starts byte-identical to the blob"
        );
        std::fs::write(&blob, b"mutated through the store").expect("the store blob is writable");
        assert_eq!(
            std::fs::read(&linked).unwrap(),
            b"mutated through the store",
            "a write through the store must be visible through the slot — a copy would not see it"
        );
    }

    /// The extensionless body stays, on Windows too. It is not
    /// redundant — `materialize_lazy::is_generated_sibling` *requires* the
    /// extensionless file to be present before `.exe`/`.shimref` read as
    /// siblings, so dropping it would break the claim-set reader. Assert the
    /// whole trio, and that an interior dot does not turn a claimed
    /// name into a sibling of something else.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_shim_launchers_writes_the_generated_sibling_trio_on_windows() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/cpython", "a");
        let set: BTreeSet<BinaryName> = ["cmake", "python3.13"]
            .into_iter()
            .map(|n| BinaryName::try_from(n).unwrap())
            .collect();
        let shim_bin = shim_bin_store(tempdir.path());

        write_shim_launchers(&staged.bin(), &package, &set, &shim_bin)
            .await
            .expect("launchers are written");

        for name in ["cmake", "python3.13"] {
            for path in [name.to_string(), format!("{name}.exe"), format!("{name}.shimref")] {
                assert!(
                    staged.bin().join(&path).is_file(),
                    "bin/{path} is part of the trio C-026 writes"
                );
            }
        }
    }

    /// A publisher may claim `mytool.exe` outright — `BinaryName`
    /// imposes no suffix rule and `materialize_lazy.rs` records the defect that
    /// assuming otherwise once caused. The slot is therefore `mytool.exe.exe`
    /// and `mytool.exe.shimref`, and the extensionless `mytool.exe` stays the
    /// claim itself.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_shim_launchers_pairs_a_claimed_name_that_already_ends_in_exe() {
        let tempdir = tempfile::tempdir().unwrap();
        let staged = shim_dir_at(tempdir.path().join("staged"));
        let package = pinned("ns/tool", "a");
        let set: BTreeSet<BinaryName> = std::iter::once(BinaryName::try_from("mytool.exe").unwrap()).collect();
        let shim_bin = shim_bin_store(tempdir.path());

        write_shim_launchers(&staged.bin(), &package, &set, &shim_bin)
            .await
            .expect("launchers are written");

        for path in ["mytool.exe", "mytool.exe.exe", "mytool.exe.shimref"] {
            assert!(
                staged.bin().join(path).is_file(),
                "bin/{path} must exist: the claimed name keeps its own suffix and still gets its siblings"
            );
        }
    }

    /// The slot is staged into a fresh `TempDir` and
    /// published by one rename, so it can **never** land on an occupied path.
    /// An occupied slot is therefore a bug, and the writer must surface it
    /// rather than paper over it — `hardlink::create` (`EEXIST`), no overwrite
    /// branch.
    ///
    /// If the Implement stage adds an overwrite branch anyway it must use
    /// `hardlink::update`, and this row flips with a recorded divergence. It
    /// is deliberately not written as "errs or is idempotent": a disjunction
    /// over outcomes is the cheapest form of unchecked green.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_windows_shim_slot_refuses_an_occupied_slot() {
        let tempdir = tempfile::tempdir().unwrap();
        let bin_dir = tempdir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join("cmake.exe"), b"someone else's file").unwrap();
        let package = pinned("ns/cmake", "a");
        let shim_bin = shim_bin_store(tempdir.path());

        let error = write_windows_shim_slot(&bin_dir, &BinaryName::try_from("cmake").unwrap(), &package, &shim_bin)
            .await
            .expect_err("an occupied slot is a bug in the caller, not a state to converge on");

        // Discriminates the actual hardlink `EEXIST`, not merely "some
        // Internal error" — a wildcard on the outer variant would also pass
        // for, say, a failed `ShimBinStore::ensure` or a permissions error,
        // neither of which is what this row exists to pin.
        match error {
            PackageErrorKind::Internal(crate::Error::InternalFile(path, io_error)) => {
                assert_eq!(
                    io_error.kind(),
                    std::io::ErrorKind::AlreadyExists,
                    "expected hardlink::create's EEXIST, got {io_error:?}"
                );
                assert_eq!(
                    path,
                    bin_dir.join("cmake.exe"),
                    "the error must name the occupied slot, not some other path"
                );
            }
            other => panic!("expected Internal(InternalFile(_, AlreadyExists)), got {other:?}"),
        }
    }

    /// The shim blob cannot be published — here because
    /// the store root is occupied by a file, so `ShimBinStore::ensure`'s
    /// `create_dir_all` fails. The refusal is `PackageErrorKind::Internal` and,
    /// crucially, **no sidecar is left behind**: `.shimref` without its `.exe`
    /// is the worse of the two partial states (ADR Contract 2's write-ordering
    /// postcondition), and the staged tree's `bin/` must never read as
    /// complete.
    #[cfg(windows)]
    #[tokio::test]
    async fn write_windows_shim_slot_fails_internally_when_the_blob_cannot_be_published() {
        let tempdir = tempfile::tempdir().unwrap();
        let bin_dir = tempdir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        // A regular file where the store's root directory must go.
        std::fs::write(tempdir.path().join("shim_bin"), b"not a directory").unwrap();
        let package = pinned("ns/cmake", "a");
        let shim_bin = shim_bin_store(tempdir.path());

        let error = write_windows_shim_slot(&bin_dir, &BinaryName::try_from("cmake").unwrap(), &package, &shim_bin)
            .await
            .expect_err("an unpublishable shim blob refuses the slot");

        assert!(
            matches!(error, PackageErrorKind::Internal(_)),
            "expected Internal, got {error:?}"
        );
        assert!(
            !bin_dir.join("cmake.shimref").exists(),
            "a .shimref without its .exe is the partial state the write order exists to exclude"
        );
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
