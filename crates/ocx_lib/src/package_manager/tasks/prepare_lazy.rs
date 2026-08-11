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
//! (`ocx launcher shim`, WP-7): a closure whose names are not enumerable, and a
//! claimed name equal to ocx's own (C-009).

use std::collections::BTreeSet;
use std::path::Path;

use crate::file_structure::{FileStructure, ShimDir};
use crate::oci;
use crate::package::metadata::{self, BinaryName};
use crate::package_manager::composer;
use crate::package_manager::error::{PackageErrorKind, ShimClaim};

use super::super::PackageManager;
use super::common::ClosureNode;
use super::lazy_advisory::{LazyAdvisory, classify_lazy_advisories};

/// The surface a shim directory is generated from: a deferred tool is composed
/// onto a **consumer's** `PATH`, never into its own private view, so every
/// admission and crossing decision here reads the interface axis (C-008).
const INTERFACE_SURFACE: bool = false;

/// The one name a shim may never carry (C-009).
///
/// The literal, not `current_exe()`'s stem: the generated body resolves
/// `${OCX_BINARY_PIN:-ocx}`, whose *fallback* is this string on every build
/// however the running binary is named, so a shim called `ocx` ahead on `PATH`
/// re-invokes itself whenever the pin is unset.
const OCX_BINARY_NAME: &str = "ocx";

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
    ///   generate from (C-009).
    /// - [`PackageErrorKind::ShimNameShadowsOcx`] — a claimed name is ocx's own
    ///   binary name; a shim for it would re-invoke itself (C-009).
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

        // C-008 (A2): the name set is the *interface surface*, so the closure
        // is pre-filtered by the composer's own admission rule before
        // `interface_shim_names` — which stays a pure function over whatever it
        // is handed — and a sealed or private dependency's `binaries` claim
        // therefore gets no launcher, exactly as under eager composition.
        let admitted: Vec<&ClosureNode> = nodes
            .iter()
            .filter(|node| super::inspect::admitted_on_surface(node, INTERFACE_SURFACE))
            .collect();
        let names = interface_shim_names(admitted)?;
        write_shim_launchers(&staged_dir.bin(), &resolved.pinned, &names).await?;

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

/// The interface-surface name set a shim tree is generated from: every
/// `binaries` claim and every entry point name on the closure's interface
/// surface, deduplicated (C-008).
///
/// One set, not two lists: the generated body does not branch on name kind
/// (ADR D-b) and a single flat `bin/` holds the result, so a name claimed on
/// both axes yields exactly one launcher. PATH precedence is pre-applied in the
/// sense that it is settled elsewhere and needs no encoding here — the real
/// `entrypoints/` and `bin/` directories both outrank `shims/` at compose time
/// (C-012), and the trigger re-resolves the name on the materialized PATH
/// (C-011).
///
/// # Errors
///
/// - [`PackageErrorKind::ShimNamesNotEnumerable`] — a node declares neither
///   `binaries` nor entry points (C-009).
/// - [`PackageErrorKind::ShimNameShadowsOcx`] — a claimed name is the literal
///   `ocx`, which is what the generated body's `${OCX_BINARY_PIN:-ocx}`
///   fallback resolves to on every build, however this binary is named (C-009).
/// - [`PackageErrorKind::ShimNameInvalid`] — a declared entry point name does
///   not survive conversion to a [`BinaryName`] (see the method's `# Errors`).
fn interface_shim_names<'a>(
    nodes: impl IntoIterator<Item = &'a ClosureNode>,
) -> Result<BTreeSet<BinaryName>, PackageErrorKind> {
    let mut names = BTreeSet::new();
    for node in nodes {
        // `Some(empty)` is enumerable — the publisher asserted zero interface
        // executables — and yields an empty `bin/`, which is still a complete
        // tree: `PATH` is not all a shim dir provides, the ref-linked config
        // blobs carry the env (C-020). Only `None` *and* no entry points
        // leaves nothing to enumerate.
        if node.binaries.is_none() && node.entrypoints.is_empty() {
            return Err(PackageErrorKind::ShimNamesNotEnumerable {
                package: node.identifier.clone(),
            });
        }

        let mut claimed: Vec<BinaryName> = Vec::new();
        if composer::carrier_crosses(metadata::Binaries::IMPLICIT_VISIBILITY, node.is_root, INTERFACE_SURFACE)
            && let Some(claim) = &node.binaries
        {
            claimed.extend(claim.iter().cloned());
        }
        if composer::carrier_crosses(
            metadata::Entrypoints::IMPLICIT_VISIBILITY,
            node.is_root,
            INTERFACE_SURFACE,
        ) {
            for entrypoint in &node.entrypoints {
                // Not total: every Windows-reserved device name is a valid
                // slug and none is a valid `BinaryName`. Refusing beats
                // skipping — a quietly incomplete shim set is the failure
                // C-009 exists to prevent.
                claimed.push(BinaryName::try_from(entrypoint.as_str()).map_err(PackageErrorKind::ShimNameInvalid)?);
            }
        }

        for name in claimed {
            if name.as_str() == OCX_BINARY_NAME {
                return Err(PackageErrorKind::ShimNameShadowsOcx(Box::new(ShimClaim {
                    package: node.identifier.clone(),
                    name,
                })));
            }
            names.insert(name);
        }
    }
    Ok(names)
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
/// # Errors
///
/// Returns an error if creating `bin_dir` or writing any launcher fails.
async fn write_shim_launchers(
    bin_dir: &Path,
    package: &oci::PinnedIdentifier,
    names: &BTreeSet<BinaryName>,
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
    }
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
    use crate::package::metadata::{Binaries, EntrypointName};

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

    fn binaries(names: &[&str]) -> Binaries {
        let set: BTreeSet<BinaryName> = names
            .iter()
            .map(|n| BinaryName::try_from(*n).expect("fixture binary name is valid"))
            .collect();
        Binaries::try_from(set).expect("fixture binaries claim is valid")
    }

    fn entrypoints(names: &[&str]) -> Vec<EntrypointName> {
        names
            .iter()
            .map(|n| EntrypointName::try_from((*n).to_string()).expect("fixture entrypoint name is valid"))
            .collect()
    }

    /// A closure node carrying only what the interface-surface name set is
    /// derived from; every other field is the inert value for this axis.
    fn node(
        identifier: oci::PinnedIdentifier,
        claimed: Option<&[&str]>,
        entries: &[&str],
        is_root: bool,
    ) -> ClosureNode {
        ClosureNode {
            config_digest: identifier.digest(),
            identifier,
            effective_visibility: None,
            binaries: claimed.map(binaries),
            entrypoints: entrypoints(entries),
            env: Vec::new(),
            integrations: Vec::new(),
            dependencies: Vec::new(),
            is_root,
        }
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

    fn names(set: &BTreeSet<BinaryName>) -> Vec<&str> {
        set.iter().map(BinaryName::as_str).collect()
    }

    fn shim_dir_at(path: PathBuf) -> ShimDir {
        ShimDir { dir: path }
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

    // ── C-008: the interface-surface name set ───────────────────────────────

    /// C-008: the name set is `binaries ∪ entrypoints`.
    #[test]
    fn interface_shim_names_unions_binaries_and_entrypoint_names() {
        let nodes = vec![node(pinned("ns/cmake", "a"), Some(&["cmake"]), &["ctest"], true)];

        let set = interface_shim_names(&nodes).expect("an enumerable closure yields a name set");

        assert_eq!(names(&set), vec!["cmake", "ctest"]);
    }

    /// C-008: "a name claimed on both axes yields exactly one launcher" — the
    /// set is flat, so `bin/` never holds two entries for one name.
    #[test]
    fn interface_shim_names_yields_one_name_when_both_axes_claim_it() {
        let nodes = vec![node(pinned("ns/cmake", "a"), Some(&["cmake"]), &["cmake"], true)];

        let set = interface_shim_names(&nodes).expect("an enumerable closure yields a name set");

        assert_eq!(names(&set), vec!["cmake"], "a doubly-claimed name must appear once");
    }

    /// C-008: the set is the *closure's* interface surface, so a dependency's
    /// claims are in it too — not just the root's.
    #[test]
    fn interface_shim_names_unions_across_every_closure_node() {
        let nodes = vec![
            node(pinned("ns/zlib", "b"), Some(&["zlib-flate"]), &[], false),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let set = interface_shim_names(&nodes).expect("an enumerable closure yields a name set");

        assert_eq!(names(&set), vec!["cmake", "zlib-flate"]);
    }

    // ── C-009: the two refusals, plus the F-5 conversion refusal ────────────

    /// C-009: a node claiming neither `binaries` nor entry points makes the
    /// name set non-enumerable, and the error names *that node* — which may be
    /// a dependency, not the tool the user asked for.
    #[test]
    fn interface_shim_names_refuses_a_node_claiming_neither_binaries_nor_entrypoints() {
        let silent = pinned("ns/zlib", "b");
        let nodes = vec![
            node(silent.clone(), None, &[], false),
            node(pinned("ns/cmake", "a"), Some(&["cmake"]), &[], true),
        ];

        let error = interface_shim_names(&nodes).expect_err("a non-enumerable closure is refused");

        match error {
            PackageErrorKind::ShimNamesNotEnumerable { package } => {
                assert_eq!(package, silent, "the refusal must name the node that claims nothing");
            }
            other => panic!("expected ShimNamesNotEnumerable, got {other:?}"),
        }
    }

    /// C-009 / F-8: the refusal fires only when a node has **no** `binaries`
    /// **and** no entry points. A node declaring entry points and no `binaries`
    /// claim is perfectly enumerable — keying the refusal on
    /// `Surface::binaries_complete` would over-refuse it.
    #[test]
    fn interface_shim_names_admits_a_node_with_entrypoints_and_no_binaries_claim() {
        let nodes = vec![node(pinned("ns/cmake", "a"), None, &["cmake"], true)];

        let set = interface_shim_names(&nodes).expect("entry points alone make the set enumerable");

        assert_eq!(names(&set), vec!["cmake"]);
    }

    /// C-009: a claimed `ocx` would re-resolve to itself through the generated
    /// body's `${OCX_BINARY_PIN:-ocx}` fallback.
    #[test]
    fn interface_shim_names_refuses_the_literal_ocx_name() {
        let shadowing = pinned("ns/tool", "a");
        let nodes = vec![node(shadowing.clone(), Some(&["ocx"]), &[], true)];

        let error = interface_shim_names(&nodes).expect_err("a claimed 'ocx' is refused");

        match error {
            PackageErrorKind::ShimNameShadowsOcx(claim) => {
                assert_eq!(claim.package, shadowing);
                assert_eq!(claim.name.as_str(), "ocx");
            }
            other => panic!("expected ShimNameShadowsOcx, got {other:?}"),
        }
    }

    /// C-009 F-7: the predicate is the **literal** `ocx`, not
    /// `current_exe()`'s stem. This test binary is not named `ocx`, so an
    /// implementation that compared against the running binary's own stem
    /// would refuse this name — and would permit `ocx` on a renamed build,
    /// which is backwards.
    #[test]
    fn interface_shim_names_admits_a_name_equal_to_this_binarys_own_stem() {
        let current = std::env::current_exe().expect("the test binary has a path");
        let stem = current
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("the test binary's stem is UTF-8")
            .to_string();
        let stem = BinaryName::try_from(stem).expect("a cargo test binary's stem is a valid BinaryName");
        assert_ne!(stem.as_str(), "ocx", "this test binary must not itself be named 'ocx'");

        let nodes = vec![node(pinned("ns/tool", "a"), Some(&[stem.as_str()]), &[], true)];

        let set = interface_shim_names(&nodes).expect("only the literal 'ocx' shadows");

        assert_eq!(names(&set), vec![stem.as_str()]);
    }

    /// C-008 (F-5 decision): every Windows-reserved device name is a valid
    /// slug — hence a valid `EntrypointName` — and none is a valid
    /// `BinaryName`. Such a name **refuses the package**; skipping it would
    /// publish a quietly incomplete shim set, the failure C-009 exists to
    /// prevent.
    #[test]
    fn interface_shim_names_refuses_an_entrypoint_name_that_is_not_a_valid_binary_name() {
        // Guards the premise: `nul` really is a legal entry point name today.
        EntrypointName::try_from("nul".to_string()).expect("'nul' is a valid slug, hence a valid EntrypointName");
        assert!(BinaryName::try_from("nul").is_err(), "'nul' must not be a BinaryName");

        let nodes = vec![node(pinned("ns/tool", "a"), Some(&["tool"]), &["nul"], true)];

        let error = interface_shim_names(&nodes).expect_err("an unconvertible entry point name refuses the package");

        assert!(
            matches!(error, PackageErrorKind::ShimNameInvalid(_)),
            "expected ShimNameInvalid, got {error:?}"
        );
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

        write_shim_launchers(&staged.bin(), &package, &set)
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

        write_shim_launchers(&staged.bin(), &package, &set)
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

        write_shim_launchers(&staged.bin(), &package, &set)
            .await
            .expect("launchers are written");

        let mode = std::fs::metadata(staged.bin().join("cmake"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "a generated launcher must be executable");
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
