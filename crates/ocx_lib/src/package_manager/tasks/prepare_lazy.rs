// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shim-directory generation — the producer half of lazy package loading
//! (plan contracts C-008 / C-009 / C-022,
//! [#302](https://github.com/ocx-sh/ocx/issues/302)).
//!
//! A *deferred* tool is one composed onto `PATH` without its content being
//! materialized. [`PackageManager::prepare_lazy`] is what puts it on disk: it
//! resolves the pinned digest, walks the metadata-only dependency closure,
//! derives the interface-surface name set, writes one generated launcher per
//! name, and publishes the whole tree into
//! [`ShimStore`](crate::file_structure::ShimStore) by a single atomic rename.
//!
//! Three properties this module exists to hold:
//!
//! - **It is a sibling task, never a cut in `pull.rs`** (ADR D-a). Nothing here
//!   touches `setup_owned_impl`, so a tool that later materializes is
//!   byte-identical to one that was never deferred.
//! - **Publication is all-or-nothing and lock-free** (C-022). The staged tree
//!   lands by `rename`; a losing racer discards its own copy and converges on
//!   the winner's, because two calls for the same *pinned* identifier see the
//!   same digest, the same closure and therefore byte-identical shim bodies.
//!   Correctness rests on content identity, not mutual exclusion.
//! - **`bin/` is the completeness marker** (C-022, 2026-08-10). Not `digest` —
//!   that file is written before the launchers are, so keying the race
//!   pre-check on it would report a half-built tree as complete the moment
//!   publishing stops being one atomic rename. The GC walker classifies on
//!   `bin/` too (`file_structure/shim_store.rs`), deliberately: producer and
//!   consumer key on one fact.
//!
//! Refusals live in [`PackageErrorKind`] and are shared with the consuming half
//! (`ocx launcher shim`, WP-7): a closure whose names are not enumerable. A
//! claimed name equal to ocx's own is **not** a refusal (plan contract C-024,
//! [`toolchain_names`](super::toolchain_names) D-4) — it renders like any
//! other name, and the self-resolution hazard that once justified refusing it
//! is closed on the trampoline's own re-entry path instead (WP-6).

use std::collections::BTreeSet;
use std::path::Path;

use crate::file_structure::{FileStructure, ShimDir};
use crate::oci;
use crate::package::metadata::BinaryName;
use crate::package_manager::error::PackageErrorKind;

use super::super::PackageManager;
use super::common::ClosureNode;
use super::lazy_advisory::{LazyAdvisory, classify_lazy_advisories};
use super::toolchain_names::{NotEnumerablePolicy, exposed_names};

/// Everything one [`PackageManager::prepare_lazy`] call produced (plan
/// contract C-008, F-5).
///
/// A named struct rather than a third tuple element: the closure is a
/// `Vec<ClosureNode>` and a positional triple at the call site reads as
/// nothing. It is handed back rather than discarded because the composer needs
/// exactly the closure this call already walked — otherwise `ocx env` over a
/// ten-tool lazy toolchain walks twenty closures per invocation, network-bound
/// on a cold store.
#[derive(Debug)]
pub struct PreparedLazy {
    /// The published shim directory. Its existence means completeness (C-020).
    pub shim: ShimDir,
    /// The metadata-only dependency closure the shim tree was generated from,
    /// deps before dependents, root last — the composer's carrier source for a
    /// deferred tool (C-020).
    pub closure: Vec<ClosureNode>,
    /// Advisories the deferred tool's declared metadata raised (C-015 (d)).
    pub advisories: Vec<LazyAdvisory>,
}

impl PackageManager {
    /// Prepares the shim directory for a deferred tool, returning it, the
    /// closure it was generated from, and the advisories its declared metadata
    /// raised.
    ///
    /// Resolves `package` to a pinned digest (honouring the ambient index
    /// chain, so `--frozen` materializes by digest with no tag resolve and
    /// `--offline` refuses), walks the metadata-only dependency closure via
    /// [`common::walk_closure_nodes`](super::common::walk_closure_nodes) —
    /// the same walk `ocx package inspect --deps` uses, never a second one —
    /// computes the interface-surface name set, stages one generated launcher
    /// per name plus the closure's config-blob forward-refs, and publishes the
    /// tree into [`ShimStore::path`](crate::file_structure::ShimStore::path)
    /// (C-008).
    ///
    /// The returned [`PreparedLazy::advisories`] list is this site's half of
    /// C-015 (d):
    /// advisories are classified here, for a **deferred** tool only, and are
    /// returned rather than logged so the composing caller can serialize them
    /// under `--format json`. An eagerly-materialized tool never reaches this
    /// method, which is what makes the "deferred only" clause testable.
    ///
    /// Idempotent and safe under concurrent callers: an already-published tree
    /// is returned as-is and a losing racer converges on the winner's
    /// (C-022). The returned directory is **complete** — every consumer
    /// (composer, GC) may treat its existence as the only completeness probe
    /// it needs (C-020).
    ///
    /// No content is downloaded and no package directory is created; the tool
    /// materializes on the first invocation of one of the generated names.
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::ShimNamesNotEnumerable`] — a closure node claims
    ///   neither `binaries` nor entry points, so there is no name set to
    ///   generate from (C-022, [`NotEnumerablePolicy::Refuse`]).
    /// - [`PackageErrorKind::ShimNameInvalid`] — a declared entry point name is
    ///   a valid `EntrypointName` but not a valid [`BinaryName`] (every
    ///   Windows-reserved device name is one: `nul`, `con`, `com1`…), so no
    ///   launcher can be written for it. Refusing beats skipping: a quietly
    ///   incomplete shim set is the failure C-009 exists to prevent.
    /// - [`PackageErrorKind::NotFound`] — the tag or digest is unknown, or the
    ///   closure's metadata is not available locally and no source may be
    ///   consulted (the caller warns and omits the tool — S-009, WP-8).
    /// - [`PackageErrorKind::Internal`] — offline policy block, staging or
    ///   publication I/O failure.
    pub async fn prepare_lazy(
        &self,
        package: &oci::Identifier,
        platform: oci::Platform,
    ) -> Result<PreparedLazy, PackageErrorKind> {
        let (fs, index) = (self.file_structure(), self.index());
        let resolved = self.resolve(package, platform.clone()).await?;
        // The shim tree's `refs/blobs/` is the only thing that keeps the
        // closure's config blobs off GC's unreachable set (C-014) and the only
        // place a consumer can read a deferred tool's env carriers from
        // (C-020), so every blob those links name must be in the blob store
        // first. Same warm-the-whole-chain step `inspect --deps` runs, for the
        // same reason — the walk stages each dep, never its own root.
        super::common::stage_chain_blobs(fs, index, &resolved).await?;
        super::common::stage_leaf_manifest(fs, index, &resolved.pinned).await?;

        let metadata = super::common::load_config_metadata(index, &resolved.pinned, &resolved.final_manifest).await?;
        let config_digest = super::common::config_blob_digest(&resolved.final_manifest)?;
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

        // Already published — nothing below would change a byte of it, so the
        // whole stage-and-discard is skipped. `bin/` is the completeness marker
        // (C-022), the same fact `publish_shim_dir` step (1) converges on and
        // the GC walker classifies on; probing it *here* rather than there is
        // what keeps a warm `ocx env` from staging and `remove_dir_all`ing a
        // full tree per deferred tool on every direnv reload. The closure walk
        // above stays: the composer consumes it.
        if crate::utility::fs::path_exists_lossy(&destination.bin()).await {
            crate::log::debug!("Reusing published shim dir {}", destination.root().display());
            return Ok(PreparedLazy {
                shim: destination,
                closure: nodes,
                advisories: classify_lazy_advisories(&resolved.pinned, &metadata),
            });
        }

        // Staged, then published by one rename — the tree is whole before it is
        // named (C-022). Order inside the temp is load-bearing: `bin/` is the
        // completeness marker, so it is written last and a refusal below leaves
        // nothing that could read as complete.
        let staged = stage_shim_dir(fs).await?;
        let staged_dir = ShimDir {
            dir: staged.path().to_path_buf(),
        };
        crate::file_structure::write_digest_file(&staged_dir.digest_file(), &resolved.pinned.digest())
            .await
            .map_err(PackageErrorKind::Internal)?;
        link_closure_config_blobs(fs, &staged_dir, &nodes).await?;

        // C-021 / C-023: `exposed_names` does its own admission filtering (a
        // sealed or private dependency's `binaries` claim gets no launcher,
        // exactly as under eager composition), so `nodes` is handed over
        // whole. A deferred tool has no fallback if a node in its closure
        // turns out unenumerable — `Refuse` (C-022) — and only the keys go on;
        // ownership/shadow tracking is for `ocx inspect`, not this call site.
        let names = exposed_names(&nodes, NotEnumerablePolicy::Refuse)?;
        let names: BTreeSet<BinaryName> = names.into_keys().collect();
        write_shim_launchers(&staged_dir.bin(), &resolved.pinned, &names, &fs.shim_bin).await?;

        publish_shim_dir(&staged_dir, &destination).await?;

        // The whole closure, not the interface-admitted subset: the composer
        // synthesizes the deferred root's transitive closure from it and must
        // see the sealed and private edges too, or the version-conflict gate
        // and the surface algebra both answer against a truncated TC (F-12).
        Ok(PreparedLazy {
            shim: destination,
            closure: nodes,
            advisories: classify_lazy_advisories(&resolved.pinned, &metadata),
        })
    }
}

/// Creates an empty staging directory for one shim tree under `temp/`.
///
/// A fresh unique directory per call rather than
/// [`TempStore::path`](crate::file_structure::TempStore::path)'s
/// identifier-keyed one: that path is shared by every caller for the same
/// identifier and comes with a sibling lock file, and publication here is
/// deliberately lock-free (C-022). The [`tempfile::TempDir`] guard also
/// discards the tree on every error path below, so a refused package leaves no
/// half-built litter; after a successful publish its path is already gone and
/// the drop is a no-op.
///
/// # Errors
///
/// Returns an error if the temp root or the staging directory cannot be created.
async fn stage_shim_dir(file_structure: &FileStructure) -> Result<tempfile::TempDir, PackageErrorKind> {
    let root = file_structure.temp.root().to_path_buf();
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&root, e)))?;
    // `TempDir::new_in` is synchronous, so it runs on a blocking thread — the
    // same treatment `ShimBinStore::ensure` gives its `NamedTempFile` staging.
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

/// Writes one generated launcher per name into `bin_dir`, each dispatching to
/// `ocx launcher shim '<package>' -- "$(basename "$0")" "$@"` (C-008, C-010).
///
/// `bin_dir` is the staged tree's `bin/`, never the published one — the tree is
/// complete before it is named (C-022).
///
/// `launcher::generate` is deliberately not reused: its signature takes an
/// `&Entrypoints` and the union set here carries [`BinaryName`]s that are not
/// valid entry point names (`c++`, `python3.13`, `MSBuild`), which is the whole
/// point of the looser grammar (C-008 (b), ADR D8).
///
/// On Windows this also writes each name's `.exe`/`.shimref` pair (C-026): the
/// extensionless body above is a shell script, and a directory of those on a
/// Windows `PATH` is a directory of non-executables. See
/// [`write_windows_shim_slot`].
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
    package: &oci::PinnedIdentifier,
    names: &BTreeSet<BinaryName>,
    shim_bin: &crate::file_structure::ShimBinStore,
) -> Result<(), PackageErrorKind> {
    // Created even for an empty name set: `bin/` is the completeness marker,
    // and a package claiming zero executables still has a complete tree.
    tokio::fs::create_dir_all(bin_dir)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(bin_dir, e)))?;

    // One rendering for every name — the body carries no name, `$(basename
    // "$0")` does. Produced by `launcher::shim_body`, the sanctioned producer
    // of the `launcher shim` wire token (C-018); nothing here spells a body.
    let body = crate::package_manager::launcher::shim_body(package).map_err(PackageErrorKind::Internal)?;

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

/// Windows producer for a single name's shim slot (C-026): hardlinks
/// `<name>.exe` from `shim_bin` (the same committed blob
/// [`crate::package_manager::launcher::generate`] hardlinks for an installed
/// package's `entrypoints/`) and writes its `<name>.shimref` sidecar — one
/// line, the pinned identifier, newline-terminated (RUL-35) — so
/// `ocx launcher shim`'s Windows dispatch (`materialize_lazy`'s
/// `GENERATED_SIBLING_EXTENSIONS`) can find it. `.shimref`, never `.shim`:
/// `.shim` names an installed package's sidecar under `entrypoints/`, and a
/// shim tree's `bin/` is a different grammar (`materialize_lazy.rs`).
///
/// The `.exe` is linked before the `.shimref` is written, mirroring the
/// sibling `.shim` producer's write-ordering postcondition (ADR Contract 2):
/// the only recoverable partial state on a mid-write fault is
/// exe-present/sidecar-absent, never a `.shimref` whose `.exe` is missing.
///
/// `hardlink::create`, never `hardlink::update` (RUL-33): the tree is staged
/// fresh into a `TempDir` and published by one atomic rename (C-022), so a
/// slot can never land on an occupied path — an occupied slot is a bug in the
/// caller, and this surfaces it as `EEXIST` rather than converging on it.
///
/// Not exercised by this workspace's `cargo check` gate: `#[cfg(windows)]`
/// code compiles only when cross-compiling for a Windows target, which this
/// repository's toolchain cannot do without an MSVC host (tracked separately,
/// R-W9). The Implement stage's merge gate (RUL-36) type-checks this arm by
/// hand via a local flip, applied to **every** `cfg` in this file, not only
/// the `#[cfg(windows)]` attributes: `cfg(windows)` → `cfg(unix)` **and**
/// `not(windows)` → `not(unix)`. The second rewrite is load-bearing, not
/// cosmetic — `write_shim_launchers` carries a sibling
/// `#[cfg_attr(not(windows), expect(unused_variables, …))]` on its `shim_bin`
/// parameter, and `not(windows)` does not contain the substring `cfg(windows)`,
/// so a flip that only rewrites the latter leaves that attribute evaluating
/// true on Linux. Once the flip makes this arm's call site live, `shim_bin` is
/// used, the `expect`'s lint no longer fires, and an unfulfilled `#[expect]`
/// is itself a hard error under `-D warnings`
/// (`unfulfilled_lint_expectations`) — so the doc names both halves because
/// running only the first produces a clippy failure, not a clean type-check.
/// The flip proves the arm compiles under the same borrow/type rules the
/// Windows target would apply, and nothing more: `crate::hardlink::create` is
/// a plain `std::fs::hard_link` on the flipped host's real filesystem and
/// `crate::shim::SHIM_BYTES` is `&[]` off Windows, so the flip *does* write a
/// real (zero-byte-sourced) `.shimref` to that filesystem — it cannot observe
/// Windows-only failure modes (`ERROR_*` codes, NTFS/ReFS/FAT/network-share
/// path length limits, file locking). Actual Windows behaviour is unverified
/// until this arm runs on a real Windows host or CI runner.
///
/// # Errors
///
/// Returns an error if publishing the shared shim blob, hardlinking it, or
/// writing the `.shimref` sidecar fails — including an occupied `.exe` slot
/// (`EEXIST`) and a cross-device `shim_bin` store (`CrossesDevices`, no copy
/// fallback, matching [`crate::hardlink::create`]'s own contract).
#[cfg(windows)]
async fn write_windows_shim_slot(
    bin_dir: &Path,
    name: &BinaryName,
    package: &oci::PinnedIdentifier,
    shim_bin: &crate::file_structure::ShimBinStore,
) -> Result<(), PackageErrorKind> {
    let exe_path = bin_dir.join(format!("{}.exe", name.as_str()));
    let shimref_path = bin_dir.join(format!("{}.shimref", name.as_str()));

    let shim_bin_path = shim_bin.ensure().await.map_err(PackageErrorKind::Internal)?;
    crate::hardlink::create(&shim_bin_path, &exe_path).map_err(PackageErrorKind::Internal)?;

    // Exactly `<pinned identifier>\n` — the whole grammar RUL-35's golden
    // literal pins, and the exact shape `ocx_shim::core::parse_shimref_sidecar`
    // reads back (see `assert_shimref_grammar` and the byte-exact test below).
    let body = format!("{package}\n");
    tokio::fs::write(&shimref_path, body.as_bytes())
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(&shimref_path, e)))?;

    Ok(())
}

/// Links the closure's config blobs into the staged tree's `refs/blobs/`
/// (C-008).
///
/// These forward-refs are what keeps the blobs off GC's unreachable set
/// (C-014) — and they are the only place a consumer can read a deferred tool's
/// env carriers from, since no package directory exists for it (C-020).
///
/// Each blob is addressed by pairing a node's own registry with its
/// [`ClosureNode::config_digest`] — the field the walker carries for exactly
/// this consumer, so no node is re-fetched to recover it.
///
/// `ReferenceManager::link_blobs` is not reusable here: it derives its target
/// directory through `PackageStore::refs_blobs_dir_for_content`, and a shim
/// tree is neither in `packages/` nor has a `content/`.
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
        let link = refs_blobs.join(crate::file_structure::cas_ref_name(&node.config_digest));
        // `update`, not `create`: two nodes may share one config blob, and the
        // ref name is derived from the digest alone, so the second write must
        // be a no-op rather than an `EEXIST`.
        crate::symlink::update(&target, &link).map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}

/// Publishes the fully staged tree at `staged` to `destination` by atomic
/// rename, converging rather than failing when a concurrent call won the race
/// (C-022).
///
/// The dance, which takes no lock: pre-check `destination.bin()` — present ⇒
/// discard the staged tree and return `Ok`; otherwise create the parent and
/// `rename_with_windows_retry` onto an absent destination; on rename failure
/// re-check `destination.bin()` — present ⇒ the race was lost and the winner's
/// tree is byte-identical, absent ⇒ propagate.
///
/// **`utility::fs::move_dir` is forbidden here.** It `remove_dir_all`s its
/// destination, so a loser would delete a live shim tree out from under a
/// concurrent exec, which then hits `ENOENT` on a `PATH` entry that existed a
/// moment earlier.
///
/// # Errors
///
/// Returns an error if creating the destination's parent fails, or if the
/// rename fails with the destination still absent.
async fn publish_shim_dir(staged: &ShimDir, destination: &ShimDir) -> Result<(), PackageErrorKind> {
    let marker = destination.bin();

    // Step (1). Also the fast path for a lost race on Windows, where a rename
    // onto an existing directory reports the same `ERROR_ACCESS_DENIED` the
    // transient retry targets — without this probe a loser would burn the
    // whole backoff schedule before the post-rename re-check catches it.
    if crate::utility::fs::path_exists_lossy(&marker).await {
        discard_staged_tree(staged, "already published").await;
        return Ok(());
    }

    // Step (2).
    if let Some(parent) = destination.root().parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::error::file_error(parent, e)))?;
    }

    // Step (3) — a bare rename onto an absent destination, never
    // `utility::fs::move_dir`, which `remove_dir_all`s its destination and
    // would delete a live shim tree out from under a concurrent exec.
    match crate::utility::fs::rename_with_windows_retry(staged.root(), destination.root()).await {
        Ok(()) => {
            crate::log::debug!("Published shim dir {}", destination.root().display());
            Ok(())
        }
        // Step (4), present half: the winner's tree is byte-identical to this
        // one — same pinned digest, same closure, same bodies — so converge.
        Err(_) if crate::utility::fs::path_exists_lossy(&marker).await => {
            discard_staged_tree(staged, "lost the publish race").await;
            Ok(())
        }
        // Step (4), absent half: whatever blocked the rename is not a
        // published shim tree, and it is not this call's to remove.
        Err(e) => Err(PackageErrorKind::Internal(crate::Error::InternalFile(
            staged.root().to_path_buf(),
            e,
        ))),
    }
}

/// Removes a staged tree whose contents another call already published,
/// tolerating failure — a surviving temp is reclaimed by the next
/// `TempStore` sweep, and reporting it would fail a call that succeeded.
async fn discard_staged_tree(staged: &ShimDir, reason: &str) {
    crate::log::debug!("Discarding staged shim tree ({reason}): {}", staged.root().display());
    if let Err(e) = tokio::fs::remove_dir_all(staged.root()).await {
        crate::log::debug!("Could not remove staged shim tree {}: {e}", staged.root().display());
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::package::metadata::Binaries;

    /// An arbitrary valid SHA-256 hex, built from a one-byte seed so each
    /// fixture node can carry a digest distinguishable from its neighbours'.
    fn digest_from(seed: &str) -> oci::Digest {
        oci::Digest::Sha256(seed.repeat(32))
    }

    fn pinned(repository: &str, seed: &str) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(repository, "example.com").clone_with_digest(digest_from(seed)),
        )
        .expect("digest-bearing identifier is pinned")
    }

    /// The exact pinned identifier WP-6's `ocx_shim::core` test module names
    /// `PINNED_IDENTIFIER` — `ocx.sh/tool/cmake:3.28@sha256:0000…0001`. A
    /// dedicated fixture rather than a call through [`pinned`]/[`digest_from`]:
    /// those two build a `ns/<repo>@example.com` identifier with no tag and a
    /// seed-repeated digest, which cannot express this literal's registry,
    /// repository, tag, or trailing-`1` digest without widening a helper every
    /// other test in this module also uses.
    #[cfg(windows)]
    fn golden_pinned() -> oci::PinnedIdentifier {
        let digest = oci::Digest::Sha256(format!("{}1", "0".repeat(63)));
        oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry("tool/cmake", "ocx.sh")
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
        identifier: oci::PinnedIdentifier,
        config_digest: oci::Digest,
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

    /// A scratch [`crate::file_structure::ShimBinStore`] rooted under the
    /// test's own tempdir — never the real `$OCX_HOME`, matching
    /// `launcher::generate`'s own test fixture.
    fn shim_bin_store(tmp: &Path) -> crate::file_structure::ShimBinStore {
        crate::file_structure::ShimBinStore::new(tmp.join("shim_bin"))
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

    // ── C-008 / C-003: the generated launchers ──────────────────────────────

    /// C-008: one artifact per name, written into `bin/` — and C-003: never at
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

    /// C-010: the body dispatches through `ocx launcher shim '<pinned-id>'`.
    /// WP-7 owns the byte-exact template; what this pins is WP-6's half — that
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

    /// C-008: a shim artifact that is not executable is not on `PATH` in any
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

    // ── C-026: the Windows shim slot ────────────────────────────────────────
    //
    // EVERY test in this section is `#[cfg(windows)]`, because
    // `write_windows_shim_slot` is. **No gate in this repository compiles
    // them** (R-W9): `task rust:check:windows-cfg` is scoped to `ocx_shim`,
    // and `cargo check -p ocx_lib --target x86_64-pc-windows-msvc` dies in
    // `aws-lc-sys` on a non-MSVC host. They are written from C-026 and the
    // `.shimref` grammar `ocx_shim::core::parse_shimref_sidecar` already
    // enforces, and the Implement stage must type-check the arm by hand
    // before merging (see the Specify report's R-W9 procedure).

    /// The five read-side rules `ocx_shim::core::parse_one_line` applies to a
    /// `.shimref`, plus its pinned-identifier clause, asserted against the
    /// bytes this producer writes.
    ///
    /// Restated here rather than imported: `ocx_lib` cannot depend on
    /// `ocx_shim` (the shim is a standalone binary crate with no library
    /// surface `ocx_lib` may link), so producer and reader are bound by a
    /// paired golden, exactly as C-034 prescribes for the `launcher shim`
    /// wire token. See the report's E-36 gap.
    #[cfg(windows)]
    fn assert_shimref_grammar(raw: &[u8], expected: &oci::PinnedIdentifier) {
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

    /// C-026 (E-35, E-37): the slot is `<name>.exe` — a hardlink of the
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

    /// RUL-35 (C-034's paired-golden treatment, WP-5's half): the produced
    /// `.shimref` bytes against a literal, not merely against `expected`'s own
    /// `to_string()` (as `assert_shimref_grammar` does above) — a producer
    /// that quietly changed the wire shape while staying consistent with
    /// itself would still pass that check. The literal is [`golden_pinned`]'s
    /// own value, converged onto WP-6's `ocx_shim::core::tests::PINNED_IDENTIFIER`
    /// so the two halves of the paired golden assert the same bytes; WP-6
    /// restates it independently on the reader side
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

    /// C-026 (E-35, inode clause): `<name>.exe` is a **hardlink** of the
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

    /// C-026 (E-40): the extensionless body stays, on Windows too. It is not
    /// redundant — `materialize_lazy::is_generated_sibling` *requires* the
    /// extensionless file to be present before `.exe`/`.shimref` read as
    /// siblings, so dropping it would break the claim-set reader. Assert the
    /// whole trio, and (E-42) that an interior dot does not turn a claimed
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

    /// C-026 (E-41): a publisher may claim `mytool.exe` outright — `BinaryName`
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

    /// C-022 / C-026 (E-38): the slot is staged into a fresh `TempDir` and
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

    /// C-022 / C-026 (E-39): the shim blob cannot be published — here because
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

    // ── C-008 / C-014 / C-020: the config-blob forward-refs ─────────────────

    /// C-008 ref-linking clause, and the guard `ClosureNode::config_digest`
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
                .join(crate::file_structure::cas_ref_name(config));
            assert!(
                crate::symlink::is_link(&link),
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

    // ── C-022: lock-free, all-or-nothing publication ────────────────────────

    /// C-022 steps (2) and (3): create the destination's parent, then rename
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

    /// C-022 step (1): a destination whose completeness marker is already
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

    /// C-022 step (4), the absent half: the marker is still absent after a
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

    // ── C-008 (F-6): advisories have a return channel ───────────────────────

    /// C-008 (F-6): advisories are **returned**, never only logged — otherwise
    /// `--format json` (C-015) has nothing to serialize. C-008 (F-5) adds the
    /// walked closure to the same channel, so the composer does not walk it a
    /// second time. The channel is the return type, so this is where it is
    /// pinned; dropping either field stops this compiling.
    #[test]
    fn prepare_lazy_returns_the_closure_and_advisories_alongside_the_shim_dir() {
        async fn signature_binding(
            manager: &PackageManager,
            package: &oci::Identifier,
            platform: oci::Platform,
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
