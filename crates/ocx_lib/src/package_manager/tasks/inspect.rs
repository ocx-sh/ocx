// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use tokio::task::JoinSet;

use crate::{
    file_structure, oci,
    oci::index::{IndexOperation, SelectResult},
    package::{
        metadata::{self, ValidMetadata, dependency::DependencyName, visibility::Visibility},
        resolved_package::ResolvedPackage,
    },
    package_manager::{self, error::PackageError, error::PackageErrorKind, tasks::resolve::ResolvedChain},
};

use super::super::PackageManager;

/// One child manifest of an image index, surfaced in default (no-`--resolve`)
/// mode so a caller can see which platforms a multi-platform tag offers
/// without committing to one.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The child manifest pinned by its own digest.
    pub identifier: oci::PinnedIdentifier,
    /// Declared platform, or [`oci::Platform::any`] when the entry omits one.
    pub platform: oci::Platform,
    /// The child descriptor's media type.
    pub media_type: String,
    /// The child descriptor's size in bytes.
    pub size: i64,
}

/// Read-only inspection output. The variant is chosen by what sits at the
/// requested reference and whether `--resolve` was given:
///
/// - [`Candidates`](InspectResult::Candidates) — default mode, the ref is an
///   image index: list the platform children, no metadata loaded.
/// - [`Manifest`](InspectResult::Manifest) — default mode, the ref is a single
///   image manifest (flat tag or `@digest`): metadata plus the manifest's
///   layer descriptors, no resolution chain.
/// - [`Resolved`](InspectResult::Resolved) — `--resolve`: platform-select
///   through the index, then metadata plus the full resolution chain.
///
/// No install or symlink side effects occur in any variant. Default mode may
/// still populate the local index / blob cache on a tag cache miss — a
/// `Resolve`-class read, not a write the caller asked for. "Read-only" here
/// means "no install, no symlink mutation", not "touches no local cache".
#[derive(Debug)]
pub enum InspectResult {
    Candidates {
        /// The image-index digest the candidates came from.
        pinned: oci::PinnedIdentifier,
        candidates: Vec<Candidate>,
    },
    Manifest {
        /// The manifest digest at the reference.
        pinned: oci::PinnedIdentifier,
        metadata: ValidMetadata,
        /// The manifest's layer descriptors (digest, media type, size).
        /// Already carried by the fetched manifest — surfaced so a default
        /// inspect shows the package's content without forcing `--resolve`.
        layers: Vec<oci::Descriptor>,
        /// The metadata-only dependency closure, present iff `--deps` was
        /// requested. See [`InspectClosure`] / `adr_inspect_metadata_closure.md` D3.
        closure: Option<InspectClosure>,
    },
    Resolved {
        /// The platform-selected pinned identifier.
        pinned: oci::PinnedIdentifier,
        metadata: ValidMetadata,
        /// Boxed — `ResolvedChain` is large relative to the other variants
        /// (`clippy::large_enum_variant`).
        chain: Box<ResolvedChain>,
        /// The metadata-only dependency closure, present iff `--deps` was
        /// requested. See [`InspectClosure`] / `adr_inspect_metadata_closure.md` D3.
        closure: Option<InspectClosure>,
    },
}

/// Mode switch for [`PackageManager::inspect`] / [`PackageManager::inspect_all`].
///
/// `deps` implies platform selection on an image-index root (the walk needs a
/// concrete root manifest to read declared deps from), so the pair is a mode,
/// not two independent booleans — see `adr_inspect_metadata_closure.md` D1/D3
/// (panel S1).
#[derive(Debug, Clone, Copy, Default)]
pub struct InspectOptions {
    /// Platform-select through the index and emit the OCI resolution chain
    /// alongside metadata and layers. See [`PackageManager::inspect`] doc.
    pub resolve: bool,
    /// Compute the metadata-only dependency closure and attach it to the
    /// report. See [`InspectClosure`].
    pub deps: bool,
}

/// A metadata-only dependency closure: the transitive set of packages reachable
/// from an inspected root, computed from config-blob metadata alone (no install).
///
/// See `adr_inspect_metadata_closure.md` D2/D3 for the wire shape and the
/// walker contract.
#[derive(Debug)]
pub struct InspectClosure {
    /// Flat, deduped node list in topological order (deps before dependents,
    /// root last). Diamonds appear once with the most-open merged visibility.
    pub nodes: Vec<ClosureNode>,
    /// Interface-surface binary claims: root unconditional, each dep iff its
    /// effective visibility `has_interface()`. Reuses the composer admission
    /// rule (`adr_declared_binaries_metadata.md` §4 Decision A).
    pub interface_binaries: Vec<(oci::PinnedIdentifier, metadata::BinaryName)>,
    /// Interface-surface entrypoint names, same admission rule.
    pub interface_entrypoints: Vec<(oci::PinnedIdentifier, metadata::EntrypointName)>,
    /// `false` iff at least one interface-admitted node has UNDECLARED binaries
    /// (`ClosureNode.binaries == None`). Entrypoints are always complete.
    pub interface_binaries_complete: bool,
    /// Install/compose-gate conditions detected over the interface projection:
    /// entrypoint-name collisions + same-repo-two-digests. Empty means the
    /// surface is realizable. Detection is pure post-processing over already-
    /// gathered metadata; reporting is non-fatal (view, not a gate — mirrors
    /// `composer::warn_repo_digest_conflicts`'s precedent).
    pub conflicts: ClosureConflicts,
}

/// Install/compose-gate conditions detected over the interface projection of
/// an [`InspectClosure`]. Both arrays are always present; empty means the
/// surface is realizable. See `adr_inspect_metadata_closure.md` D2 (Codex C2).
#[derive(Debug, Default)]
pub struct ClosureConflicts {
    pub entrypoints: Vec<EntrypointConflict>,
    pub repositories: Vec<RepositoryConflict>,
}

/// Two or more interface-admitted closure nodes declare the same entrypoint
/// name — install/compose (`composer::check_entrypoints`) would hard-reject
/// this closure.
#[derive(Debug)]
pub struct EntrypointConflict {
    pub name: metadata::EntrypointName,
    pub packages: Vec<oci::PinnedIdentifier>,
}

/// One repository resolved to two or more distinct digests on the interface
/// projection — install/compose (`composer::check_repo_digest_conflicts`)
/// would hard-reject this closure.
#[derive(Debug)]
pub struct RepositoryConflict {
    pub repository: oci::Repository,
    pub digests: Vec<oci::Digest>,
}

/// One node of an [`InspectClosure`].
#[derive(Debug)]
pub struct ClosureNode {
    /// Digest-addressed; advisory tag preserved for display.
    pub identifier: oci::PinnedIdentifier,
    /// Composed from the root via `Visibility::through_edge`/`merge`. `None`
    /// iff `is_root` — the composed-from-root axis is undefined for the root
    /// itself (the wire key is absent exactly when `root: true`).
    pub effective_visibility: Option<Visibility>,
    /// Tri-state, straight from the node's `Bundle.binaries`: key absent on
    /// the wire means undeclared; `Some(empty)` means the publisher asserts
    /// zero interface executables.
    pub binaries: Option<metadata::Binaries>,
    /// The node's declared entrypoint map keys.
    pub entrypoints: Vec<metadata::EntrypointName>,
    /// The node's own declared dependency edges (as authored).
    pub dependencies: Vec<ClosureEdge>,
    pub is_root: bool,
}

/// A declared dependency edge (as authored), carrying its declared visibility.
#[derive(Debug, Clone)]
pub struct ClosureEdge {
    pub identifier: oci::PinnedIdentifier,
    /// The DECLARED edge visibility (goal #2 — "dependencies state their
    /// linkage visibility"), as distinct from [`ClosureNode::effective_visibility`]
    /// (the composed-from-root visibility).
    pub visibility: Visibility,
    pub name: DependencyName,
}

impl PackageManager {
    /// Inspects `package` without installing or creating symlinks.
    ///
    /// No install or symlink side effects occur. Default mode resolves the
    /// tag through the index with `IndexOperation::Resolve`, so a tag cache
    /// miss may populate the local index / blob cache as a side effect of
    /// the read — intended behavior, not a write the caller requested.
    ///
    /// `resolve == false` (default): the manifest at the reference is fetched
    /// **without** platform selection. An image index yields
    /// [`InspectResult::Candidates`] (the available platforms); a single image
    /// manifest yields [`InspectResult::Manifest`] (its declared metadata and
    /// layer descriptors). `-p/--platform` does not apply here.
    ///
    /// `resolve == true`: the identifier is resolved through the index with
    /// platform selection (honoring `platform`), returning
    /// [`InspectResult::Resolved`] with metadata and the resolution chain.
    ///
    /// `deps == true`: additionally computes the metadata-only dependency
    /// closure (see [`InspectClosure`]) and attaches it to the report. On an
    /// image-index root `deps` implies platform selection (honoring
    /// `platform`) even without `resolve`, because the walk needs a concrete
    /// root manifest to read declared deps from — see
    /// `adr_inspect_metadata_closure.md` D1/D3.
    ///
    /// Accepts a tag or an `@digest` identifier.
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::NotFound`] — tag/digest unknown.
    /// - [`PackageErrorKind::OfflineManifestMissing`] — known tag but the
    ///   manifest blob is absent from the local cache in offline mode.
    /// - [`PackageErrorKind::Internal`] — config blob missing offline,
    ///   wrong media type, or metadata validation failure.
    pub async fn inspect(
        &self,
        package: &oci::Identifier,
        platform: oci::Platform,
        options: InspectOptions,
    ) -> Result<InspectResult, PackageErrorKind> {
        if options.resolve {
            return resolve_with_closure(self, package, platform, options.deps).await;
        }

        // Default mode: fetch the manifest at the reference without platform
        // selection, then adapt the result to its OCI shape.
        let (top_pinned, manifest) = fetch_top_manifest(self, package).await?;
        match manifest {
            oci::Manifest::Image(img) => {
                let metadata = super::common::load_config_metadata(self.index(), &top_pinned, &img).await?;
                let closure = maybe_walk_closure(
                    self.file_structure(),
                    self.index(),
                    self.is_offline(),
                    options.deps,
                    &top_pinned,
                    &metadata,
                    &platform,
                )
                .await?;
                Ok(InspectResult::Manifest {
                    pinned: top_pinned,
                    metadata,
                    layers: img.layers,
                    closure,
                })
            }
            oci::Manifest::ImageIndex(index) => {
                if options.deps {
                    // `--deps` on an index root always platform-selects (ADR
                    // D1): the walk needs a concrete root manifest to read
                    // declared deps from. Re-resolving here is accepted
                    // redundancy — under `ChainMode::Default`/`Frozen` the
                    // top-manifest fetch above already warmed the local CAS,
                    // so this second round-trip is local-first (ADR D3
                    // implementation note); under `--remote` there is no
                    // local warm cache to hit and this is a genuine second
                    // network fetch.
                    return resolve_with_closure(self, package, platform, true).await;
                }

                let mut candidates = Vec::with_capacity(index.manifests.len());
                for entry in index.manifests {
                    // A child descriptor whose `digest` string does not parse
                    // is a corrupt image index, not a "missing digest" — carry
                    // the structured `DigestError` so the message names the
                    // bad value (still classifies to DataError/65).
                    let digest = oci::Digest::try_from(entry.digest.as_str())
                        .map_err(|e| PackageErrorKind::Internal(crate::Error::from(e)))?;
                    let identifier =
                        oci::PinnedIdentifier::try_from(top_pinned.as_identifier().clone_with_digest(digest))
                            .map_err(|_| PackageErrorKind::DigestMissing)?;
                    let platform = oci::Platform::try_from(entry.platform).map_err(PackageErrorKind::Internal)?;
                    candidates.push(Candidate {
                        identifier,
                        platform,
                        media_type: entry.media_type,
                        size: entry.size,
                    });
                }
                Ok(InspectResult::Candidates {
                    pinned: top_pinned,
                    candidates,
                })
            }
        }
    }

    /// Inspects multiple packages in parallel, preserving input order.
    ///
    /// Empty input short-circuits to `Ok(vec![])`; a single package takes the
    /// direct path; otherwise each package is inspected on its own task and the
    /// results are drained via
    /// [`drain_package_tasks`](super::common::drain_package_tasks), which
    /// returns successes in input order and batch errors sorted by input index
    /// (deterministic exit code). Mirrors [`find_all`](PackageManager::find_all).
    pub async fn inspect_all(
        &self,
        packages: Vec<oci::Identifier>,
        platform: oci::Platform,
        options: InspectOptions,
    ) -> Result<Vec<InspectResult>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let _spin = self.progress().spinner(format!("Inspecting '{}'", packages[0]));
            let result = self.inspect(&packages[0], platform, options).await.map_err(|kind| {
                package_manager::error::Error::InspectFailed(vec![PackageError::new(packages[0].clone(), kind)])
            })?;
            return Ok(vec![result]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platform = platform.clone();
            tasks.spawn(async move {
                let _spin = mgr.progress().spinner(format!("Inspecting '{package}'"));
                let result = mgr.inspect(&package, platform, options).await;
                (package, result)
            });
        }

        super::common::drain_package_tasks(&packages, tasks, package_manager::error::Error::InspectFailed).await
    }
}

/// Fetches the top-level manifest for `package` without platform selection.
///
/// Thin wrapper over [`super::common::resolve_top_manifest`] pinning the
/// default-inspect [`IndexOperation::Resolve`] routing (intended behavior —
/// inspect deliberately uses `Resolve`, not `Query`). The shared helper
/// mirrors the tag/digest top-id derivation and not-found discrimination of
/// [`PackageManager::resolve`] (tag truly unknown → [`PackageErrorKind::NotFound`];
/// known tag but blob missing offline → [`PackageErrorKind::OfflineManifestMissing`]),
/// but stops before platform selection so callers can inspect an image index
/// as-is.
async fn fetch_top_manifest(
    mgr: &PackageManager,
    package: &oci::Identifier,
) -> Result<(oci::PinnedIdentifier, oci::Manifest), PackageErrorKind> {
    super::common::resolve_top_manifest(mgr.index(), package, IndexOperation::Resolve).await
}

/// Platform-selects `package` through the index, then loads metadata and
/// (when `deps`) the dependency closure, assembling an
/// [`InspectResult::Resolved`]. Shared by `inspect`'s two resolve-through-the-
/// index call sites — `options.resolve` and `--deps` on an image-index root
/// (ADR D1) — which differ only in whether chain-blob staging is gated on
/// `deps` or unconditional: the `--resolve` site passes `options.deps`
/// through, the image-index-root site always passes `true` (it is only ever
/// reached when `options.deps` already holds).
async fn resolve_with_closure(
    mgr: &PackageManager,
    package: &oci::Identifier,
    platform: oci::Platform,
    deps: bool,
) -> Result<InspectResult, PackageErrorKind> {
    let resolved = mgr.resolve(package, platform.clone()).await?;
    // `--deps` warms the whole root chain (dispatch/index + platform
    // manifest + config) the same way it warms each dep node — see
    // `stage_leaf_manifest`'s doc. Plain `--resolve` without `--deps`
    // keeps the non-persisting default (main's design, not ours).
    if deps {
        super::common::stage_chain_blobs(mgr.file_structure(), mgr.index(), &resolved).await?;
    }
    let metadata = super::common::load_config_metadata(mgr.index(), &resolved.pinned, &resolved.final_manifest).await?;
    let closure = maybe_walk_closure(
        mgr.file_structure(),
        mgr.index(),
        mgr.is_offline(),
        deps,
        &resolved.pinned,
        &metadata,
        &platform,
    )
    .await?;
    Ok(InspectResult::Resolved {
        pinned: resolved.pinned.clone(),
        metadata,
        chain: Box::new(resolved),
        closure,
    })
}

// ── Metadata-only dependency closure walker (ADR D3) ────────────────────────
//
// Module-private free functions, not a new `pub` facade method — per the
// task-module architecture rule, only `inspect`/`inspect_all` are `pub`.
// Wired into `inspect()` via `maybe_walk_closure` when `InspectOptions::deps`
// is set.

/// Phase-1 gather concurrency bound — caps how many per-node fetches
/// [`gather_closure_nodes`]'s admission queue spawns at once over the closure
/// frontier (ADR D3 panel W5). Codex C2: this bounds SPAWNED tasks, not just
/// running fetch bodies — see `gather_closure_nodes`'s doc.
const CLOSURE_FETCH_CONCURRENCY: usize = 8;

/// One gathered closure node: its RESOLVED pinned identity (the
/// platform-selected child for an image-index-pinned dep, unchanged for a
/// flat dep — Codex C1, matches install-time resolution — `pull.rs`'s
/// `info.identifier()` is likewise the resolved identity, never the index),
/// validated metadata, and its own declared dependency edges.
/// [`gather_closure_nodes`]'s output element type.
type GatheredClosureNode = (oci::PinnedIdentifier, ValidMetadata, Vec<ClosureEdge>);

/// A [`GatheredClosureNode`] tagged with its spawn slot and the edge's
/// DECLARED identity (as authored — may differ from the gathered node's
/// resolved identity for an image-index-pinned dep), so completion order
/// (nondeterministic per `JoinSet::join_next`) can be re-sorted back into
/// deterministic spawn order (quality-rust.md JoinSet rule) and
/// [`gather_closure_nodes`]'s declared→resolved alias can be built.
type SlottedClosureNode = (
    usize,
    oci::PinnedIdentifier,
    oci::PinnedIdentifier,
    ValidMetadata,
    Vec<ClosureEdge>,
);

/// Resolves `deps`, computing the closure only when requested. Thin gate so
/// every `inspect()` call site shares one line instead of duplicating the
/// `if options.deps { Some(walk_closure(..).await?) } else { None }` branch.
async fn maybe_walk_closure(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    offline: bool,
    deps: bool,
    pinned: &oci::PinnedIdentifier,
    metadata: &ValidMetadata,
    platform: &oci::Platform,
) -> Result<Option<InspectClosure>, PackageErrorKind> {
    if !deps {
        return Ok(None);
    }
    // The root itself is a leaf platform manifest inspect already fetched
    // (`fetch_top_manifest`/`resolve`) but never staged (A3) — stage it too,
    // so a `--deps` walk warms the *whole* closure including its own root,
    // not just the deps `walk_closure` reaches (goals 4+5,
    // `adr_inspect_metadata_closure.md`).
    stage_leaf_manifest(fs, index, pinned).await?;
    Ok(Some(
        walk_closure(fs, index, offline, pinned, metadata, platform).await?,
    ))
}

/// Stages `pinned`'s raw manifest bytes into the machine-global blob store
/// when not already local — mirrors `stage_and_link_chain_blobs`'s
/// `ChainRole::Manifest` step (`adr_index_indirection.md` A3/B2: the local
/// index never caches a leaf, only dispatch objects; `$OCX_HOME/blobs` is the
/// sanctioned content-cache home). No ref-link: `inspect` has no installed
/// package directory to link into, so the staged blob is an unreferenced
/// cache entry — `ocx clean` may reclaim it, same as any other cache-warming
/// write. Shared by the root's own fetch ([`maybe_walk_closure`]) and each
/// dep's fetch ([`fetch_closure_node`]).
async fn stage_leaf_manifest(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    pinned: &oci::PinnedIdentifier,
) -> Result<(), PackageErrorKind> {
    if super::common::blob_needs_fetch(fs, pinned).await?
        && let Some((bytes, _, _)) = index
            .fetch_manifest_raw_bytes(pinned.as_identifier())
            .await
            .map_err(PackageErrorKind::Internal)?
    {
        // `fetch_manifest_raw_bytes` verifies the source's returned bytes are
        // self-consistent with the digest the source computed, never against
        // the digest actually requested (CWE-345) — re-verify against
        // `pinned`'s own digest before this write, mirroring
        // `stage_chain_blobs`'s identical check for the same seam.
        super::common::verify_requested_digest(pinned, &bytes)?;
        fs.blobs
            .write_blob(pinned.registry(), &pinned.digest(), &bytes)
            .await
            .map_err(PackageErrorKind::Internal)?;
    }
    Ok(())
}

/// Two-phase metadata-only closure walker: Phase 1 parallel metadata gather
/// (I/O-bound, via [`gather_closure_nodes`]), Phase 2 pure visibility fold
/// (via [`fold_effective_visibility`]), then the interface-surface aggregate
/// (`interface_binaries`/`interface_entrypoints`/`interface_binaries_complete`)
/// and [`ClosureConflicts`] detection over the resulting node set — the
/// latter two are pure post-processing on already-gathered metadata (zero
/// extra I/O), so they are folded into this orchestrator rather than named
/// as their own free functions.
///
/// Fail-closed: any single node error aborts the whole closure — a partial
/// closure must never render as a complete one.
///
/// # Errors
///
/// See `adr_inspect_metadata_closure.md` Error Taxonomy: dep manifest/config
/// absent under offline policy → `PackageErrorKind::Internal(crate::Error::OfflineMode)`;
/// dep genuinely absent with a source consulted → `PackageErrorKind::NotFound`;
/// malformed / wrong-media-type / over-cap config → the existing
/// `load_config_metadata` errors; dep image-index child with no platform
/// match → `PackageErrorKind::FeatureMismatch`.
async fn walk_closure(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    offline: bool,
    root_pinned: &oci::PinnedIdentifier,
    root_metadata: &ValidMetadata,
    platform: &oci::Platform,
) -> Result<InspectClosure, PackageErrorKind> {
    let frontier = closure_edges_from_metadata(root_metadata);
    let (gathered, resolved_identity) = gather_closure_nodes(fs, index, offline, frontier, platform).await?;
    let nodes = fold_effective_visibility(root_pinned, root_metadata, gathered, &resolved_identity);

    // Interface-surface aggregate: root unconditional, each dep iff its
    // effective visibility `has_interface()` (mirrors the composer's
    // admission rule, `adr_declared_binaries_metadata.md` §4 Decision A).
    let mut interface_binaries = Vec::new();
    let mut interface_entrypoints = Vec::new();
    let mut interface_binaries_complete = true;
    for node in nodes.iter().filter(|node| is_interface_admitted(node)) {
        match &node.binaries {
            Some(binaries) => {
                interface_binaries.extend(binaries.iter().map(|name| (node.identifier.clone(), name.clone())));
            }
            // Undeclared binaries on an admitted node: "couldn't determine"
            // must not silently read as "determined zero".
            None => interface_binaries_complete = false,
        }
        interface_entrypoints.extend(
            node.entrypoints
                .iter()
                .map(|name| (node.identifier.clone(), name.clone())),
        );
    }

    let conflicts = detect_closure_conflicts(&nodes);

    Ok(InspectClosure {
        nodes,
        interface_binaries,
        interface_entrypoints,
        interface_binaries_complete,
        conflicts,
    })
}

/// Whether `node` is admitted to the interface-surface aggregate: the root
/// unconditionally, a dep iff its composed-from-root visibility
/// `has_interface()`. Shared by the aggregate build in [`walk_closure`] and
/// conflict detection in [`detect_closure_conflicts`] — both scan the same
/// admitted set.
fn is_interface_admitted(node: &ClosureNode) -> bool {
    node.is_root || node.effective_visibility.is_some_and(Visibility::has_interface)
}

/// Codex C2: install/compose-gate conditions detected over the interface
/// projection — entrypoint-name collisions and same-repository-two-digests.
/// Pure post-processing on already-gathered metadata (zero extra I/O).
/// Mirrors `composer::check_entrypoints` / `collect_repo_digest_conflicts`'s
/// admission and exclusion rules, adapted to read from [`ClosureNode`]s
/// instead of installed [`crate::package::install_info::InstallInfo`].
fn detect_closure_conflicts(nodes: &[ClosureNode]) -> ClosureConflicts {
    let mut entrypoint_owners: BTreeMap<metadata::EntrypointName, Vec<oci::PinnedIdentifier>> = BTreeMap::new();
    let mut repository_digests: BTreeMap<oci::Repository, Vec<oci::Digest>> = BTreeMap::new();

    for node in nodes.iter().filter(|node| is_interface_admitted(node)) {
        for name in &node.entrypoints {
            entrypoint_owners
                .entry(name.clone())
                .or_default()
                .push(node.identifier.clone());
        }
        let digests = repository_digests
            .entry(oci::Repository::from(node.identifier.as_identifier()))
            .or_default();
        let digest = node.identifier.digest();
        // Dedup by digest: the same node reached via two paths, or two tags
        // resolving to the same digest, is not a conflict.
        if !digests.contains(&digest) {
            digests.push(digest);
        }
    }

    let entrypoints = entrypoint_owners
        .into_iter()
        .filter(|(_, owners)| owners.len() > 1)
        .map(|(name, packages)| EntrypointConflict { name, packages })
        .collect();
    let repositories = repository_digests
        .into_iter()
        .filter(|(_, digests)| digests.len() > 1)
        .map(|(repository, digests)| RepositoryConflict { repository, digests })
        .collect();

    ClosureConflicts {
        entrypoints,
        repositories,
    }
}

/// Phase 1 — parallel metadata gather. BFS the DAG from `frontier` (the
/// root's declared dependency edges, deduped by advisory-stripped
/// DECLARED-identity), fetching each *unique* edge's node concurrently
/// through a bounded ADMISSION QUEUE: discovered edges wait in `pending`
/// until fewer than [`CLOSURE_FETCH_CONCURRENCY`] fetches are spawned, so the
/// bound caps outstanding tasks, not merely running fetch bodies (Codex C2 —
/// the prior `Semaphore` only gated the fetch body, so a wide frontier still
/// spawned every edge's task immediately). Digest-addressed edges make
/// cycles impossible, so the BFS always terminates. Fail-closed: any node
/// fetch error aborts the whole gather.
///
/// Per-node fetch: `fetch_manifest(dep.identifier, IndexOperation::Resolve)`
/// then `load_config_metadata` for an image manifest, or platform-select the
/// child then `load_config_metadata` for a dep pinned to an image index (ADR
/// D3 "Per-node fetch").
///
/// Returns each gathered node's RESOLVED pinned identifier, its
/// [`ValidMetadata`], and its own declared dependency edges (feeding
/// [`fold_effective_visibility`]) — deduped by RESOLVED identity, since two
/// different declared edges (a direct edge and an image-index edge) can
/// resolve to the same digest (Codex C1). Also returns the declared→resolved
/// alias map [`fold_effective_visibility`] needs to translate a
/// [`ClosureEdge`] (always as-authored) to the node it actually reached.
///
/// Per-gather invariants shared by every spawned fetch — grouped so `spawn`
/// stays under the arg-count lint instead of taking each field separately.
struct GatherContext<'a> {
    fs: &'a file_structure::FileStructure,
    index: &'a oci::index::Index,
    offline: bool,
    platform: &'a oci::Platform,
}

async fn gather_closure_nodes(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    offline: bool,
    frontier: Vec<ClosureEdge>,
    platform: &oci::Platform,
) -> Result<
    (
        Vec<GatheredClosureNode>,
        HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
    ),
    PackageErrorKind,
> {
    let context = GatherContext {
        fs,
        index,
        offline,
        platform,
    };
    let mut visited: HashSet<oci::PinnedIdentifier> = HashSet::new();
    let mut tasks: JoinSet<Result<SlottedClosureNode, PackageErrorKind>> = JoinSet::new();
    let mut next_slot = 0usize;
    // Discovered edges not yet admitted into `tasks` — `admit` drains this
    // up to the concurrency bound; slot numbers are assigned at DISCOVERY
    // time (below), independent of admission order, so the final ordering
    // stays deterministic regardless of scheduling.
    let mut pending: VecDeque<(usize, ClosureEdge)> = VecDeque::new();

    // Spawns one fetch for `edge` into slot `slot`. A plain function (not a
    // closure) so it can be called from `admit` without fighting the borrow
    // checker over a captured `&mut JoinSet`.
    fn spawn(
        tasks: &mut JoinSet<Result<SlottedClosureNode, PackageErrorKind>>,
        context: &GatherContext<'_>,
        slot: usize,
        edge: ClosureEdge,
    ) {
        let fs = context.fs.clone();
        let index = context.index.clone();
        let offline = context.offline;
        let platform = context.platform.clone();
        tasks.spawn(async move {
            let declared = edge.identifier.clone();
            let (resolved_pinned, metadata, edges) =
                fetch_closure_node(&fs, &index, offline, &declared, &platform).await?;
            Ok((slot, declared, resolved_pinned, metadata, edges))
        });
    }

    // Admits queued edges into `tasks` up to the concurrency bound — the
    // admission bound itself (Codex C2), enforced by never having more than
    // `CLOSURE_FETCH_CONCURRENCY` tasks spawned at once, rather than a
    // `Semaphore` gating only the fetch body inside an already-spawned task.
    fn admit(
        tasks: &mut JoinSet<Result<SlottedClosureNode, PackageErrorKind>>,
        context: &GatherContext<'_>,
        pending: &mut VecDeque<(usize, ClosureEdge)>,
    ) {
        while tasks.len() < CLOSURE_FETCH_CONCURRENCY {
            let Some((slot, edge)) = pending.pop_front() else {
                break;
            };
            spawn(tasks, context, slot, edge);
        }
    }

    for edge in frontier {
        if visited.insert(edge.identifier.strip_advisory()) {
            let slot = next_slot;
            next_slot += 1;
            pending.push_back((slot, edge));
        }
    }
    admit(&mut tasks, &context, &mut pending);

    // Results indexed by spawn slot for deterministic ordering
    // (quality-rust.md JoinSet rule) — `join_next` completion order is
    // otherwise nondeterministic.
    let mut slots: Vec<Option<GatheredClosureNode>> = Vec::new();
    // Declared (as-authored) identity → RESOLVED identity, built as each
    // fetch completes. `fold_effective_visibility` needs this because a
    // `ClosureEdge` always names the DECLARED identity, which for an
    // image-index-pinned dep differs from the node it resolved to.
    let mut resolved_identity: HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier> = HashMap::new();
    // RESOLVED identities already gathered (Codex C1/C2 post-selection
    // dedup). The `visited` check above dedups by DECLARED edge identity,
    // which cannot see that two different declared edges (e.g. a direct edge
    // and an image-index edge selecting the same child) resolve to the same
    // digest — this second, post-fetch check catches that and drops the
    // duplicate fetch's node instead of inserting a second one (double
    // counting its claims / manufacturing a false repo conflict downstream).
    let mut resolved_seen: HashSet<oci::PinnedIdentifier> = HashSet::new();
    while let Some(joined) = tasks.join_next().await {
        // Fail-closed: the `?`s below return early on the first node error,
        // dropping `tasks` — `JoinSet::drop` aborts every task still
        // in-flight, so a partial closure is never observed by the caller.
        let (slot, declared, resolved_pinned, metadata, edges) =
            joined.map_err(|_| PackageErrorKind::TaskPanicked)??;
        resolved_identity.insert(declared.strip_advisory(), resolved_pinned.strip_advisory());

        for child_edge in &edges {
            if visited.insert(child_edge.identifier.strip_advisory()) {
                let slot = next_slot;
                next_slot += 1;
                pending.push_back((slot, child_edge.clone()));
            }
        }

        // Drop a duplicate resolution instead of inserting a second node;
        // `slots[slot]` stays `None` and is filtered out below. Still worth
        // discovering `edges` above regardless of the outcome here — content
        // addressing guarantees a duplicate's declared deps are byte-identical
        // to the first resolution's, so `visited` already no-ops any re-spawn.
        if resolved_seen.insert(resolved_pinned.strip_advisory()) {
            if slot >= slots.len() {
                slots.resize_with(slot + 1, || None);
            }
            slots[slot] = Some((resolved_pinned, metadata, edges));
        }

        admit(&mut tasks, &context, &mut pending);
    }

    Ok((slots.into_iter().flatten().collect(), resolved_identity))
}

/// Per-node fetch for the closure walker (ADR D3 "Per-node fetch"): fetches
/// `dep_pinned`'s manifest via the same `IndexOperation::Resolve` routing
/// `inspect`'s default mode uses (local-first, write-through on miss), loads
/// its OCX metadata, and returns the node's RESOLVED identity plus its
/// metadata and its own declared dependency edges.
///
/// A dep pinned to an image index (hand-authored — ordinary `ocx package
/// create` always pins a platform-manifest digest, `dependency_pinning.rs`)
/// platform-selects the child before loading its config; the returned
/// identity is the SELECTED CHILD's digest — `dep_pinned`'s own advisory tag
/// is preserved on it (`Index::fetch_candidates` derives the candidate via
/// `identifier.clone_with_digest`, so the tag survives selection unchanged)
/// — matching install-time resolution (`pull.rs`'s `info.identifier()` is
/// likewise the platform-selected identity, never the index digest; Codex
/// C1). The authored index reference stays visible, unchanged, on the
/// parent's [`ClosureEdge`] — only the node's own identity and the gather-time
/// dedup key move to the resolved child.
async fn fetch_closure_node(
    fs: &file_structure::FileStructure,
    index: &oci::index::Index,
    offline: bool,
    dep_pinned: &oci::PinnedIdentifier,
    platform: &oci::Platform,
) -> Result<(oci::PinnedIdentifier, ValidMetadata, Vec<ClosureEdge>), PackageErrorKind> {
    let dep_identifier = dep_pinned.as_identifier().clone();
    let manifest = match index
        .fetch_manifest(&dep_identifier, IndexOperation::Resolve)
        .await
        .map_err(PackageErrorKind::Internal)?
    {
        Some((_, manifest)) => manifest,
        None => return Err(closure_fetch_miss(offline)),
    };

    let (resolved_pinned, image) = match manifest {
        oci::Manifest::Image(img) => (dep_pinned.clone(), img),
        oci::Manifest::ImageIndex(_) => {
            let selected = match index
                .select(&dep_identifier, platform, IndexOperation::Resolve)
                .await
                .map_err(PackageErrorKind::Internal)?
            {
                SelectResult::Found(id) => id,
                SelectResult::Ambiguous(candidates) => return Err(PackageErrorKind::SelectionAmbiguous(candidates)),
                SelectResult::NotFound => return Err(PackageErrorKind::NotFound),
                SelectResult::FeatureMismatch {
                    host_features,
                    available,
                } => {
                    return Err(PackageErrorKind::FeatureMismatch {
                        host_features,
                        available,
                    });
                }
            };
            let child_pinned =
                oci::PinnedIdentifier::try_from(selected.clone()).map_err(|_| PackageErrorKind::DigestMissing)?;
            let image = match index
                .fetch_manifest(&selected, IndexOperation::Resolve)
                .await
                .map_err(PackageErrorKind::Internal)?
            {
                Some((_, oci::Manifest::Image(img))) => img,
                // A selected child that is itself an image index, or that
                // vanished between select and fetch, is not a valid OCI
                // dependency shape — mirrors the "absent child digest" row
                // of the ADR Error Taxonomy.
                Some((_, oci::Manifest::ImageIndex(_))) | None => return Err(closure_fetch_miss(offline)),
            };
            (child_pinned, image)
        }
    };

    stage_leaf_manifest(fs, index, &resolved_pinned).await?;

    let metadata = super::common::load_config_metadata(index, &resolved_pinned, &image).await?;
    let edges = closure_edges_from_metadata(&metadata);
    Ok((resolved_pinned, metadata, edges))
}

/// Resolves a closure-frontier manifest miss to the correct error, matching
/// the ADR D3 Error Taxonomy: a policy block under `--offline` (no source
/// was allowed to be consulted), or a genuine not-found when a source could
/// have been (or was) consulted.
fn closure_fetch_miss(offline: bool) -> PackageErrorKind {
    if offline {
        PackageErrorKind::Internal(crate::Error::OfflineMode)
    } else {
        PackageErrorKind::NotFound
    }
}

/// Builds a node's declared dependency edges (as authored) from its
/// validated metadata — the wire-shape source for [`ClosureEdge`], shared by
/// every gather call site and by the root's own `dependencies` field.
fn closure_edges_from_metadata(metadata: &ValidMetadata) -> Vec<ClosureEdge> {
    metadata
        .dependencies()
        .iter()
        .map(|dep| ClosureEdge {
            identifier: dep.identifier.clone(),
            visibility: dep.visibility,
            name: dep.name(),
        })
        .collect()
}

/// Phase 2 — pure visibility fold (no I/O). Computes each gathered node's
/// effective visibility as seen from the root by folding
/// `Visibility::through_edge` down every path from the root and
/// `Visibility::merge`-ing at diamonds — the identical algorithm
/// [`crate::package::resolved_package::ResolvedPackage::with_dependencies`]
/// applies to an installed transitive closure, sourced here from `gathered`
/// metadata instead of `resolve.json`.
///
/// Returns the flat, deduped node list in topological order (deps before
/// dependents, root last) — `root_pinned`'s own node carries
/// `effective_visibility: None` and `is_root: true` (the composed-from-root
/// axis is undefined for the root itself).
fn fold_effective_visibility(
    root_pinned: &oci::PinnedIdentifier,
    root_metadata: &ValidMetadata,
    gathered: Vec<GatheredClosureNode>,
    resolved_identity: &HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
) -> Vec<ClosureNode> {
    let by_identity: HashMap<oci::PinnedIdentifier, GatheredClosureNode> = gathered
        .into_iter()
        .map(|entry| (entry.0.strip_advisory(), entry))
        .collect();

    // Bottom-up (post-order) DFS: each node's own `ResolvedPackage` needs its
    // direct children's already-computed `ResolvedPackage`s, exactly as the
    // install pipeline computes `resolve.json` while recursively pulling
    // deps (`pull.rs`). `order` collects the post-order visitation sequence
    // — deps before dependents, by construction.
    let mut resolved: HashMap<oci::PinnedIdentifier, ResolvedPackage> = HashMap::new();
    let mut order: Vec<oci::PinnedIdentifier> = Vec::new();
    let root_edges = closure_edges_from_metadata(root_metadata);
    for edge in &root_edges {
        let resolved_key = resolved_edge_identity(resolved_identity, edge);
        visit_closure_node(
            &resolved_key,
            &by_identity,
            resolved_identity,
            &mut resolved,
            &mut order,
        );
    }

    // Root's own `ResolvedPackage`, built the same way, yields every
    // descendant's effective visibility as seen from the root — the
    // identical algorithm `ResolvedPackage::with_dependencies` applies at
    // install time, sourced here from gathered metadata instead of
    // `resolve.json`. Keyed by RESOLVED identity (Codex C1) — `with_dependencies`
    // dedups by its identifier argument, so a declared identity here would
    // fragment a single package into two entries whenever a dep is pinned to
    // an image index.
    let root_children: Vec<(oci::PinnedIdentifier, ResolvedPackage, Visibility)> = root_edges
        .iter()
        .map(|edge| {
            let resolved_key = resolved_edge_identity(resolved_identity, edge);
            let child_resolved = resolved.get(&resolved_key).cloned().unwrap_or_default();
            (resolved_key, child_resolved, edge.visibility)
        })
        .collect();
    let effective: HashMap<oci::PinnedIdentifier, Visibility> = ResolvedPackage::new()
        .with_dependencies(root_children)
        .dependencies
        .into_iter()
        .map(|dep| (dep.identifier.strip_advisory(), dep.visibility))
        .collect();

    let mut nodes: Vec<ClosureNode> = order
        .into_iter()
        .map(|key| {
            let (identifier, metadata, edges) = by_identity
                .get(&key)
                .expect("fold_effective_visibility only visits keys populated by gather_closure_nodes");
            let effective_visibility = Some(
                *effective
                    .get(&key)
                    .expect("every gathered node is reachable from root by construction"),
            );
            ClosureNode {
                identifier: identifier.clone(),
                effective_visibility,
                binaries: metadata.binaries().cloned(),
                entrypoints: metadata
                    .entrypoints()
                    .map(|entries| entries.names().cloned().collect())
                    .unwrap_or_default(),
                dependencies: edges.clone(),
                is_root: false,
            }
        })
        .collect();

    // Root last — the composed-from-root axis is undefined for the root
    // itself, so `effective_visibility` stays `None` (panel W3).
    nodes.push(ClosureNode {
        identifier: root_pinned.clone(),
        effective_visibility: None,
        binaries: root_metadata.binaries().cloned(),
        entrypoints: root_metadata
            .entrypoints()
            .map(|entries| entries.names().cloned().collect())
            .unwrap_or_default(),
        dependencies: root_edges,
        is_root: true,
    });

    nodes
}

/// Translates a declared [`ClosureEdge`]'s identity to the RESOLVED identity
/// [`gather_closure_nodes`] actually gathered a node under (Codex C1) — the
/// platform-selected child for an image-index-pinned dep, unchanged for a
/// flat dep. Every edge reachable from root has an alias entry by
/// construction (`gather_closure_nodes` populates one per fetch it completes,
/// and it completes a fetch for every edge the BFS discovers).
fn resolved_edge_identity(
    resolved_identity: &HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
    edge: &ClosureEdge,
) -> oci::PinnedIdentifier {
    resolved_identity
        .get(&edge.identifier.strip_advisory())
        .cloned()
        .expect("gather_closure_nodes populates the alias map for every edge reachable from root")
}

/// Post-order DFS over `key`'s declared edges, memoizing each visited node's
/// own [`ResolvedPackage`] (its transitive closure as seen from itself) into
/// `resolved` and recording deps-before-dependents visitation order in
/// `order`. A no-op once `key` is already memoized — every node is visited
/// at most once regardless of how many edges reach it (diamond dedup).
/// `key` and every memoization key here are RESOLVED identities (Codex C1) —
/// `resolved_identity` translates each child edge's declared identity before
/// recursing, so two different declared edges resolving to the same digest
/// (a direct edge and an image-index edge selecting it) memoize to the SAME
/// entry instead of computing the fold twice.
fn visit_closure_node(
    key: &oci::PinnedIdentifier,
    by_identity: &HashMap<oci::PinnedIdentifier, GatheredClosureNode>,
    resolved_identity: &HashMap<oci::PinnedIdentifier, oci::PinnedIdentifier>,
    resolved: &mut HashMap<oci::PinnedIdentifier, ResolvedPackage>,
    order: &mut Vec<oci::PinnedIdentifier>,
) {
    if resolved.contains_key(key) {
        return;
    }
    let (_, _, edges) = by_identity
        .get(key)
        .expect("gather_closure_nodes populates by_identity for every edge reachable from root");
    let children: Vec<(oci::PinnedIdentifier, ResolvedPackage, Visibility)> = edges
        .iter()
        .map(|edge| {
            let child_key = resolved_edge_identity(resolved_identity, edge);
            visit_closure_node(&child_key, by_identity, resolved_identity, resolved, order);
            let child_resolved = resolved.get(&child_key).cloned().unwrap_or_default();
            (child_key, child_resolved, edge.visibility)
        })
        .collect();
    resolved.insert(key.clone(), ResolvedPackage::new().with_dependencies(children));
    order.push(key.clone());
}

#[cfg(test)]
mod spec_tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::{
        MEDIA_TYPE_PACKAGE_METADATA_V1,
        file_structure::FileStructure,
        oci::index::{ChainMode, Index, IndexImpl, IndexOperation, LocalConfig, LocalIndex},
        oci::{self, Algorithm, Digest, Identifier},
        package::metadata::{ValidMetadata, visibility::Visibility},
        package_manager::{PackageManager, error::PackageErrorKind},
    };

    use super::super::common;
    use super::{InspectOptions, InspectResult};

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";
    // A fixed child digest used only as a *reference* inside an image index (the
    // child manifest is not read in default mode), and a fixed layer digest
    // referenced by a manifest descriptor — neither is a stored, verified object.
    const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const HEX_D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    const METADATA_JSON: &str = r#"{"type":"bundle","version":1,"env":[{"key":"PATH","type":"path","value":"${installPath}/bin","visibility":"public"}],"dependencies":[],"entrypoints":{}}"#;

    fn tagged_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }
    fn digest(hex: &str) -> Digest {
        Digest::Sha256(hex.to_string())
    }
    fn linux_amd64() -> oci::Platform {
        "linux/amd64".parse().unwrap()
    }

    /// Peak-concurrency probe for [`FakeManifestSource::fetch_manifest`]
    /// (Codex C2 regression): every clone shares the same counters via
    /// `Arc`, so it survives `box_clone` (one per spawned gather task —
    /// `spawn`'s `context.index.clone()`). `enter()` records one fetch
    /// entering flight, bumps the peak, and returns a guard that records the
    /// fetch leaving flight on drop.
    #[derive(Clone, Default)]
    struct ConcurrencyProbe {
        in_flight: Arc<std::sync::atomic::AtomicUsize>,
        peak: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl ConcurrencyProbe {
        fn peak(&self) -> usize {
            self.peak.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn enter(&self) -> ConcurrencyProbeGuard {
            let now = self.in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
            ConcurrencyProbeGuard { probe: self.clone() }
        }
    }

    struct ConcurrencyProbeGuard {
        probe: ConcurrencyProbe,
    }

    impl Drop for ConcurrencyProbeGuard {
        fn drop(&mut self) {
            self.probe.in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A minimal fake source serving a fixed set of `(tag-or-digest) -> bytes`
    /// manifest entries, keyed by tag for a tag-addressed lookup or by digest
    /// string for a digest-addressed one (a platform-selected child), plus a
    /// separate `digest -> bytes` map for opaque config blobs (`fetch_blob`,
    /// a distinct seam from manifest resolution). Used with
    /// `ChainMode::Default` so `PackageManager::inspect` can recover content
    /// through the same AbsentLeaf-recovery path a live registry would — a
    /// leaf platform manifest is never locally cached (A3), so an
    /// offline-only pre-seeded fixture cannot answer a lookup for one.
    #[derive(Clone, Default)]
    struct FakeManifestSource {
        entries: std::collections::HashMap<String, (Vec<u8>, Digest, oci::Manifest)>,
        blobs: std::collections::HashMap<String, Vec<u8>>,
        /// Set only by the C2 wide-frontier test — `None` elsewhere means
        /// zero behavior change (no counting, no artificial delay) for every
        /// other test using this fixture.
        probe: Option<ConcurrencyProbe>,
    }

    impl FakeManifestSource {
        fn with(mut self, key: &str, bytes: &[u8]) -> Self {
            let digest = Algorithm::Sha256.hash(bytes);
            let manifest = serde_json::from_slice(bytes).unwrap();
            self.entries.insert(key.to_string(), (bytes.to_vec(), digest, manifest));
            self
        }

        /// Attaches a peak-concurrency probe (Codex C2 regression only) —
        /// `fetch_manifest` briefly holds the fetch open so concurrent
        /// per-node fetches actually overlap and the peak is observable.
        fn with_concurrency_probe(mut self, probe: ConcurrencyProbe) -> Self {
            self.probe = Some(probe);
            self
        }

        /// Register an opaque blob (e.g. a package-metadata config blob,
        /// which does not parse as an OCI [`oci::Manifest`]) served by
        /// `fetch_blob`, keyed by its own digest string.
        fn with_blob(mut self, digest: &str, bytes: &[u8]) -> Self {
            self.blobs.insert(digest.to_string(), bytes.to_vec());
            self
        }

        /// Like [`Self::with`], but forces the entry's returned digest to
        /// `digest` instead of the real hash of `bytes`. `ChainedIndex`'s
        /// manifest-fetch path (`fetch_manifest`/`fetch_manifest_raw_bytes`)
        /// itself never verifies a source's claimed digest against the
        /// identifier actually requested — only `fetch_blob` (config blobs)
        /// digest-verifies at that layer. Task-layer callers that persist raw
        /// manifest bytes under a caller-chosen digest (`stage_leaf_manifest`,
        /// `stage_chain_blobs`'s `Index`/`Manifest` roles) now re-verify
        /// before writing (CWE-345 fix, `verify_requested_digest`) — so a
        /// `digest` that genuinely disagrees with `bytes`' real hash is
        /// rejected there rather than silently round-tripped. Used by the
        /// dedicated digest-mismatch regression test below; every other call
        /// site passes the real hash (functionally equivalent to
        /// [`Self::with`]) because its node must survive that re-verify.
        fn with_digest(mut self, key: &str, bytes: &[u8], digest: Digest) -> Self {
            let manifest = serde_json::from_slice(bytes).unwrap();
            self.entries.insert(key.to_string(), (bytes.to_vec(), digest, manifest));
            self
        }

        fn lookup(&self, identifier: &Identifier) -> Option<(Vec<u8>, Digest, oci::Manifest)> {
            let key = match identifier.digest() {
                Some(digest) => digest.to_string(),
                None => identifier.tag_or_latest().to_string(),
            };
            self.entries.get(&key).cloned()
        }
    }

    #[async_trait::async_trait]
    impl IndexImpl for FakeManifestSource {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            identifier: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(Digest, oci::Manifest)>> {
            Ok(self.lookup(identifier).map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<Digest>> {
            Ok(self.lookup(identifier).map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            // Config blobs are fetched by digest via `Index::fetch_blob`
            // (`load_config_metadata`), a separate seam from
            // `fetch_manifest_raw_bytes`.
            Ok(self.blobs.get(&blob_ref.digest().to_string()).cloned())
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &Identifier,
        ) -> crate::Result<Option<(Vec<u8>, Digest, oci::Manifest)>> {
            // The actual network-touching seam for a genuine (uncached) digest
            // lookup under `ChainMode::Default` — `ChainedIndex::fetch_manifest`
            // routes a fresh miss through `LocalIndex::persist_dispatch`, which
            // calls THIS method, not `fetch_manifest` above (which only answers
            // an already-local dispatch hit or a `--remote` query). The C2
            // wide-frontier probe hooks here so it observes real per-node fetch
            // concurrency.
            let _guard = self.probe.as_ref().map(ConcurrencyProbe::enter);
            if _guard.is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            Ok(self.lookup(identifier))
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Build a `PackageManager` chained to `source` under `ChainMode::Default`.
    fn make_manager(dir: &TempDir, source: FakeManifestSource) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: fs.index.clone(),
            }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// An offline `PackageManager` with no sources and an empty local index
    /// — for tests asserting a genuine local miss / policy block.
    fn make_offline_manager(dir: &TempDir) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: fs.index.clone(),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    fn image_manifest_json(config_digest: &Digest) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{layer}","size":4096}}]}}"#,
            size = METADATA_JSON.len(),
            layer = digest(HEX_D),
        )
    }

    /// Default mode against a flat image manifest returns metadata plus the
    /// manifest's layer descriptors — no `--resolve` needed. A leaf platform
    /// manifest is never locally cached (A3); the manager is chained to a
    /// live fake source under `ChainMode::Default` (default-inspect uses
    /// `IndexOperation::Resolve`, so the walk reaches it) that recovers it.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_flat_manifest_returns_metadata_and_layers() {
        let dir = TempDir::new().unwrap();
        let config_digest = Algorithm::Sha256.hash(METADATA_JSON.as_bytes());
        let manifest_json = image_manifest_json(&config_digest);
        let manifest_digest = Algorithm::Sha256.hash(manifest_json.as_bytes());
        let source = FakeManifestSource::default()
            .with(TAG, manifest_json.as_bytes())
            .with_blob(&config_digest.to_string(), METADATA_JSON.as_bytes());

        let mgr = make_manager(&dir, source);
        let options = InspectOptions {
            resolve: false,
            deps: false,
        };
        let result = mgr.inspect(&tagged_id(), linux_amd64(), options).await.unwrap();

        match result {
            InspectResult::Manifest {
                pinned,
                metadata,
                layers,
                closure,
            } => {
                assert_eq!(pinned.digest(), manifest_digest);
                assert_eq!(metadata.env().expect("env").into_iter().count(), 1);
                assert_eq!(layers.len(), 1, "manifest layer surfaced in default mode");
                assert_eq!(layers[0].digest, format!("sha256:{HEX_D}"));
                assert_eq!(layers[0].size, 4096);
                assert!(closure.is_none(), "no --deps requested, closure must be absent");
            }
            other => panic!("expected Manifest, got {other:?}"),
        }
    }

    /// Default mode against an image index returns the platform candidates,
    /// no metadata loaded. The top-level index is dispatch-shaped (locally
    /// cacheable, A3); the child is only a reference here (not read in
    /// default mode), so a fixed digest is fine and no fake source is needed.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_image_index_returns_candidates() {
        let dir = TempDir::new().unwrap();
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child}","size":7,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
            child = digest(HEX_B),
        );
        let index_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        let source = FakeManifestSource::default().with(TAG, index_json.as_bytes());

        let mgr = make_manager(&dir, source);
        let options = InspectOptions {
            resolve: false,
            deps: false,
        };
        let result = mgr.inspect(&tagged_id(), oci::Platform::any(), options).await.unwrap();

        match result {
            InspectResult::Candidates { pinned, candidates } => {
                assert_eq!(pinned.digest(), index_digest, "pinned = index digest");
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].identifier.digest(), digest(HEX_B));
                assert_eq!(candidates[0].platform, linux_amd64());
                assert_eq!(candidates[0].size, 7);
            }
            other => panic!("expected Candidates, got {other:?}"),
        }
    }

    /// `--resolve` against an image index platform-selects the child and
    /// returns metadata plus a 3-entry chain. The top-level index is
    /// dispatch-shaped (locally cacheable); the platform-selected child is a
    /// leaf, recovered via the fake source.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_resolve_image_index_returns_chain() {
        let dir = TempDir::new().unwrap();
        let config_digest = Algorithm::Sha256.hash(METADATA_JSON.as_bytes());
        let child_json = image_manifest_json(&config_digest);
        let child_digest = Algorithm::Sha256.hash(child_json.as_bytes());
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#
        );
        let source = FakeManifestSource::default()
            .with(TAG, index_json.as_bytes())
            .with(&child_digest.to_string(), child_json.as_bytes())
            .with_blob(&config_digest.to_string(), METADATA_JSON.as_bytes());

        let mgr = make_manager(&dir, source);
        let options = InspectOptions {
            resolve: true,
            deps: false,
        };
        let result = mgr.inspect(&tagged_id(), linux_amd64(), options).await.unwrap();

        match result {
            InspectResult::Resolved { pinned, chain, .. } => {
                assert_eq!(pinned.digest(), child_digest);
                assert_eq!(chain.chain.len(), 3, "index + child + config");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    /// Unknown (unpinned) tag under the offline manager is a **policy block**
    /// (#155): the local index has no pointer and offline forbids walking the
    /// source, so resolution refuses with `PolicyResolutionBlocked` (exit 81)
    /// rather than a not-found (79). This unifies offline with frozen — under
    /// either no-resolve policy the resolver was forbidden from checking, so
    /// "policy blocked" is the honest answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_unknown_tag_is_policy_blocked_offline() {
        let dir = TempDir::new().unwrap();
        let mgr = make_offline_manager(&dir);
        let err = mgr
            .inspect(
                &tagged_id(),
                oci::Platform::any(),
                InspectOptions {
                    resolve: false,
                    deps: false,
                },
            )
            .await
            .unwrap_err();
        match err {
            PackageErrorKind::Internal(crate::Error::OciIndex(
                crate::oci::index::error::Error::PolicyResolutionBlocked { policy, .. },
            )) => assert_eq!(
                policy, "offline",
                "offline manager must label the policy block as offline"
            ),
            other => panic!("unknown tag offline must be a policy block (exit 81), got {other:?}"),
        }
    }

    // ── F4 (Warn gap): exit-65 / malformed inputs ──
    //
    // Design record (`subsystem-cli-commands.md` "package inspect" gotcha):
    // "Exit codes via `classify_error`: NotFound→79, offline manifest/blob
    // miss→81, malformed metadata→65." The `Internal` / `DigestMissing`
    // kinds are what `classify_error` maps to `DataError` (65) at the CLI
    // boundary; these unit tests pin the kind the task layer must surface.

    /// Default mode against a flat image manifest whose config blob holds
    /// structurally-invalid metadata must surface `PackageErrorKind::Internal`
    /// (the metadata-validation-failure path documented for `inspect`).
    /// Mirrors the `common.rs` `load_object_data_rejects_invalid_metadata`
    /// pattern: an env entry references an undeclared dependency, so
    /// `ValidMetadata::try_from` rejects it at the ingress boundary.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_malformed_metadata_is_internal() {
        const BAD_METADATA_JSON: &str = r#"{"type":"bundle","version":1,"dependencies":[],"env":[{"key":"FOO","type":"constant","value":"${deps.missing.installPath}/x","visibility":"public"}],"entrypoints":{}}"#;

        let dir = TempDir::new().unwrap();
        let config_digest = Algorithm::Sha256.hash(BAD_METADATA_JSON.as_bytes());
        // The image manifest's config descriptor advertises the structurally
        // invalid metadata blob's length so the media-type/size gate passes
        // and validation is the failing step.
        let manifest_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
            size = BAD_METADATA_JSON.len(),
        );
        let source = FakeManifestSource::default()
            .with(TAG, manifest_json.as_bytes())
            .with_blob(&config_digest.to_string(), BAD_METADATA_JSON.as_bytes());

        let mgr = make_manager(&dir, source);
        let err = mgr
            .inspect(
                &tagged_id(),
                oci::Platform::any(),
                InspectOptions {
                    resolve: false,
                    deps: false,
                },
            )
            .await
            .unwrap_err();

        assert!(
            matches!(err, PackageErrorKind::Internal(_)),
            "malformed metadata must surface Internal (→ DataError/65), got {err:?}"
        );
    }

    /// Default mode against an image index whose child descriptor carries a
    /// structurally-invalid `digest` string must surface
    /// `PackageErrorKind::Internal` wrapping the structured `DigestError`
    /// (so the message names the bad value), not the misleading
    /// `DigestMissing` ("identifier has no digest after resolution"). The
    /// kind still classifies to `DataError`/65.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_bad_child_digest_is_internal_digest_error() {
        let dir = TempDir::new().unwrap();
        // Child descriptor `digest` is not a valid `algorithm:hex` string.
        let index_json = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"not-a-valid-digest","size":7,"platform":{"os":"linux","architecture":"amd64"}}]}"#;
        let source = FakeManifestSource::default().with(TAG, index_json.as_bytes());

        let mgr = make_manager(&dir, source);
        let err = mgr
            .inspect(
                &tagged_id(),
                oci::Platform::any(),
                InspectOptions {
                    resolve: false,
                    deps: false,
                },
            )
            .await
            .unwrap_err();

        assert!(
            matches!(err, PackageErrorKind::Internal(_)),
            "malformed child digest must surface Internal(DigestError), got {err:?}"
        );
        let chain = format!("{err:#}");
        assert!(
            chain.contains("not-a-valid-digest"),
            "error chain must name the bad digest value: {chain}"
        );
    }

    // ── Metadata-only dependency closure walker (ADR D2/D3, plan Test Strategy) ─
    //
    // Specification tests for `walk_closure` / `gather_closure_nodes` /
    // `fold_effective_visibility`, written against `adr_inspect_metadata_closure.md`
    // and `plan_inspect_binaries_closure.md` — NOT against the stub bodies.
    // Every call into the walker below is expected to panic with
    // `unimplemented!()` until the Implement phase fills the three free
    // functions; that panic is the gate this Specify phase must pass.

    /// Builds a 64-hex-char digest by cycling `seed`'s characters. `seed`
    /// must be valid lowercase hex — the value round-trips through
    /// `oci::Digest::try_from` inside the walker's own manifest/config
    /// parsing, so an invalid seed would fail construction for reasons
    /// unrelated to the behavior under test.
    fn cdigest(seed: &str) -> Digest {
        let hex: String = seed.chars().cycle().take(64).collect();
        Digest::Sha256(hex)
    }

    /// A digest-addressed `PinnedIdentifier` for `repo`, with no advisory tag.
    fn closure_pinned(repo: &str, d: &Digest) -> oci::PinnedIdentifier {
        let id = Identifier::new_registry(repo, REGISTRY).clone_with_digest(d.clone());
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    /// A digest-addressed `PinnedIdentifier` for `repo` that also carries an
    /// advisory tag — used by the diamond-repeat plain-render test to prove
    /// dedup is keyed by content digest, not by the tag-bearing identifier.
    fn closure_pinned_tagged(repo: &str, tag: &str, d: &Digest) -> oci::PinnedIdentifier {
        let id = Identifier::new_registry(repo, REGISTRY)
            .clone_with_tag(tag)
            .clone_with_digest(d.clone());
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    /// One `dependencies[]` entry as authored (ADR D2 wire shape): the
    /// dep's pinned identifier, its DECLARED edge visibility, and its
    /// interpolation name.
    fn closure_edge(identifier: &oci::PinnedIdentifier, visibility: &str, name: &str) -> serde_json::Value {
        serde_json::json!({ "identifier": identifier.to_string(), "visibility": visibility, "name": name })
    }

    /// A `Bundle`-shaped config JSON document. `binaries` mirrors the
    /// tri-state wire contract: `None` omits the key entirely (undeclared),
    /// `Some(value)` writes it verbatim (including an explicit empty array).
    fn closure_config(
        dependencies: serde_json::Value,
        binaries: Option<serde_json::Value>,
        entrypoints: serde_json::Value,
    ) -> String {
        let mut obj = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "dependencies": dependencies,
            "entrypoints": entrypoints,
        });
        if let Some(binaries) = binaries {
            obj.as_object_mut()
                .expect("closure_config always builds a JSON object")
                .insert("binaries".to_string(), binaries);
        }
        obj.to_string()
    }

    /// A flat image manifest referencing `config_digest`, sized to
    /// `config_json`'s real length (the honest case). No layers — the
    /// closure walker never reads them.
    fn closure_manifest_json(config_digest: &Digest, config_json: &str) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
            size = config_json.len(),
        )
    }

    /// Like [`closure_manifest_json`] but with an explicitly supplied
    /// (possibly dishonest) declared config size — used by the D5
    /// pre-fetch-rejection test.
    fn closure_manifest_json_with_declared_size(config_digest: &Digest, declared_size: usize) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{declared_size}}},"layers":[]}}"#,
        )
    }

    /// Registers one closure node's manifest + config blob into `source`,
    /// keyed by the manifest's own real content digest, and returns that
    /// digest so the caller can build the node's `PinnedIdentifier`
    /// (`closure_pinned`) for edge references and assertions.
    ///
    /// Both digests are the real hash of their bytes: `stage_leaf_manifest`
    /// now verifies a fetched dep leaf manifest's bytes hash to the digest it
    /// was requested under before staging it (CWE-345 regression test below),
    /// so an arbitrary forced `cdigest` identity — safe before that fix — is
    /// no longer usable here. The config digest was always real (`fetch_blob`
    /// digest-verifies independently).
    fn register_closure_node(source: FakeManifestSource, config_json: &str) -> (FakeManifestSource, Digest) {
        let config_digest = Algorithm::Sha256.hash(config_json.as_bytes());
        let manifest_json = closure_manifest_json(&config_digest, config_json);
        let manifest_digest = Algorithm::Sha256.hash(manifest_json.as_bytes());
        let source = source
            .with(&manifest_digest.to_string(), manifest_json.as_bytes())
            .with_blob(&config_digest.to_string(), config_json.as_bytes());
        (source, manifest_digest)
    }

    /// Registers the root's tag entry + manifest + config into `source`
    /// (callers register every dep first via [`register_closure_node`]),
    /// then loads it through the existing (non-stub) default-mode
    /// `inspect()` path — the same production route `walk_closure`'s caller
    /// uses to obtain `root_pinned`/`root_metadata` before handing them to
    /// the walker.
    async fn seed_and_load_root(
        dir: &TempDir,
        source: FakeManifestSource,
        root_digest: &Digest,
        root_config_json: &str,
    ) -> (PackageManager, oci::PinnedIdentifier, ValidMetadata) {
        let root_config_digest = Algorithm::Sha256.hash(root_config_json.as_bytes());
        let root_manifest_json = closure_manifest_json(&root_config_digest, root_config_json);
        let source = source
            .with_digest(TAG, root_manifest_json.as_bytes(), root_digest.clone())
            .with_blob(&root_config_digest.to_string(), root_config_json.as_bytes());

        let mgr = make_manager(dir, source);
        let result = mgr
            .inspect(
                &tagged_id(),
                oci::Platform::any(),
                InspectOptions {
                    resolve: false,
                    deps: false,
                },
            )
            .await
            .expect("root manifest+config must load cleanly");
        match result {
            InspectResult::Manifest { pinned, metadata, .. } => (mgr, pinned, metadata),
            other => panic!("expected Manifest, got {other:?}"),
        }
    }

    /// Minimal source `IndexImpl` serving exactly one dependency's manifest
    /// and config blob, keyed by digest. Used by the goals-4+5 warm/offline
    /// pin and the D5 size-cap tests, where the walker must reach an actual
    /// network source for one digest-addressed dependency (a purely local
    /// fixture can't distinguish "fetched" from "never touched").
    #[derive(Clone)]
    struct SingleDepSource {
        manifest_digest: Digest,
        manifest_json: String,
        config_digest: Digest,
        config_bytes: Vec<u8>,
        blob_calls: Arc<Mutex<usize>>,
    }

    impl SingleDepSource {
        fn new(manifest_digest: Digest, manifest_json: String, config_digest: Digest, config_bytes: Vec<u8>) -> Self {
            Self {
                manifest_digest,
                manifest_json,
                config_digest,
                config_bytes,
                blob_calls: Arc::new(Mutex::new(0)),
            }
        }

        fn blob_call_count(&self) -> usize {
            *self.blob_calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl IndexImpl for SingleDepSource {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }

        async fn fetch_manifest(
            &self,
            identifier: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(Digest, oci::Manifest)>> {
            if identifier.digest().as_ref() != Some(&self.manifest_digest) {
                return Ok(None);
            }
            let manifest: oci::Manifest =
                serde_json::from_str(&self.manifest_json).expect("fixture manifest JSON parses");
            Ok(Some((self.manifest_digest.clone(), manifest)))
        }

        async fn fetch_manifest_digest(
            &self,
            identifier: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<Digest>> {
            Ok(if identifier.digest().as_ref() == Some(&self.manifest_digest) {
                Some(self.manifest_digest.clone())
            } else {
                None
            })
        }

        async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            *self.blob_calls.lock().unwrap() += 1;
            if blob_ref.digest() == self.config_digest {
                Ok(Some(self.config_bytes.clone()))
            } else {
                Ok(None)
            }
        }

        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A bare `LocalIndex` over `dir`'s CAS — used when a test needs to
    /// build its own `Index::from_chained` (goals-4+5, D5 size cap) instead
    /// of going through `make_manager`.
    fn local_index_at(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(LocalConfig {
            index_store: crate::file_structure::IndexStore::new(dir.path().join("index")),
        })
    }

    // ── 3-node chain: topo order + through_edge composition ─────────────────

    /// Root -> cmake (edge sealed) -> zlib (edge public). Neither hop is a
    /// diamond, so this pins pure `through_edge` composition down a
    /// multi-hop chain: a sealed edge at the root blocks every node behind
    /// it, regardless of how open the deeper edges are — cmake and zlib
    /// both come out `sealed` from the root's perspective, and only the
    /// root's own claims survive into `interface_surface`.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_three_node_chain_topo_order_and_through_edge_composition() {
        let dir = TempDir::new().unwrap();

        let zlib_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["zlib-tool"])),
            serde_json::json!({"zlib-ep": {}}),
        );
        let (source, zlib_digest) = register_closure_node(FakeManifestSource::default(), &zlib_config);
        let zlib_id = closure_pinned("zlib", &zlib_digest);

        let cmake_config = closure_config(
            serde_json::json!([closure_edge(&zlib_id, "public", "zlib")]),
            Some(serde_json::json!(["cmake-tool"])),
            serde_json::json!({"cmake-ep": {}}),
        );
        let (source, cmake_digest) = register_closure_node(source, &cmake_config);
        let cmake_id = closure_pinned("cmake", &cmake_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&cmake_id, "sealed", "cmake")]),
            Some(serde_json::json!(["root-tool"])),
            serde_json::json!({"root-ep": {}}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert_eq!(closure.nodes.len(), 3, "root + cmake + zlib, deduped");

        let zlib_pos = closure
            .nodes
            .iter()
            .position(|n| n.identifier.digest() == zlib_digest)
            .expect("zlib node present");
        let cmake_pos = closure
            .nodes
            .iter()
            .position(|n| n.identifier.digest() == cmake_digest)
            .expect("cmake node present");
        let root_pos = closure
            .nodes
            .iter()
            .position(|n| n.identifier.digest() == cdigest("1"))
            .expect("root node present");
        assert!(
            zlib_pos < cmake_pos && cmake_pos < root_pos,
            "topological order must be deps before dependents, root last: zlib={zlib_pos} cmake={cmake_pos} root={root_pos}"
        );

        let root_node = &closure.nodes[root_pos];
        assert!(root_node.is_root, "root node must carry is_root=true");
        assert!(
            root_node.effective_visibility.is_none(),
            "the composed-from-root axis is undefined for the root itself"
        );
        assert_eq!(root_node.dependencies.len(), 1);
        assert_eq!(root_node.dependencies[0].identifier.digest(), cmake_digest);
        assert_eq!(root_node.dependencies[0].visibility, Visibility::SEALED);
        assert_eq!(root_node.dependencies[0].name.as_str(), "cmake");

        let cmake_node = &closure.nodes[cmake_pos];
        assert!(!cmake_node.is_root);
        assert_eq!(
            cmake_node.effective_visibility,
            Some(Visibility::SEALED),
            "cmake's effective visibility from root IS the sealed edge (direct dep)"
        );
        assert_eq!(cmake_node.dependencies.len(), 1);
        assert_eq!(cmake_node.dependencies[0].identifier.digest(), zlib_digest);
        assert_eq!(
            cmake_node.dependencies[0].visibility,
            Visibility::PUBLIC,
            "cmake's OWN declared edge to zlib is public, independent of how root sees cmake"
        );
        assert_eq!(cmake_node.dependencies[0].name.as_str(), "zlib");

        let zlib_node = &closure.nodes[zlib_pos];
        assert_eq!(
            zlib_node.effective_visibility,
            Some(Visibility::SEALED),
            "sealed.through_edge(public) = sealed: a sealed hop blocks everything behind it"
        );

        // interface_surface: only the root's own unconditional claims survive
        // — cmake/zlib are both sealed-effective, so neither is admitted.
        assert_eq!(closure.interface_binaries.len(), 1);
        assert_eq!(closure.interface_binaries[0].0, root_pinned);
        assert_eq!(closure.interface_binaries[0].1.as_str(), "root-tool");
        assert_eq!(closure.interface_entrypoints.len(), 1);
        assert_eq!(closure.interface_entrypoints[0].0, root_pinned);
        assert_eq!(closure.interface_entrypoints[0].1.as_str(), "root-ep");
        assert!(
            closure.interface_binaries_complete,
            "root declares binaries and is the only admitted node"
        );
    }

    // ── Diamond: merge at a shared node reached by two paths ────────────────

    /// Root -> A (public) -> C (sealed edge), root -> B (public) -> C
    /// (public edge). C is reached via two paths; the walker must dedup it
    /// to one node with the merged (most-open) visibility, and the
    /// interface_surface aggregate must admit its claim exactly once.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_diamond_dedups_to_one_node_with_merged_visibility() {
        let dir = TempDir::new().unwrap();

        let c_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["c-tool"])),
            serde_json::json!({}),
        );
        let (source, c_digest) = register_closure_node(FakeManifestSource::default(), &c_config);
        let c_id = closure_pinned("c", &c_digest);

        let a_config = closure_config(
            serde_json::json!([closure_edge(&c_id, "sealed", "c")]),
            None,
            serde_json::json!({}),
        );
        let (source, a_digest) = register_closure_node(source, &a_config);
        let a_id = closure_pinned("a", &a_digest);

        let b_config = closure_config(
            serde_json::json!([closure_edge(&c_id, "public", "c")]),
            None,
            serde_json::json!({}),
        );
        let (source, b_digest) = register_closure_node(source, &b_config);
        let b_id = closure_pinned("b", &b_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&a_id, "public", "a"), closure_edge(&b_id, "public", "b")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert_eq!(
            closure.nodes.len(),
            4,
            "root, a, b, c — c appears once despite two paths"
        );
        assert_eq!(
            closure
                .nodes
                .iter()
                .filter(|n| n.identifier.digest() == c_digest)
                .count(),
            1,
            "c must be deduped to a single node"
        );
        let c_node = closure
            .nodes
            .iter()
            .find(|n| n.identifier.digest() == c_digest)
            .unwrap();
        assert_eq!(
            c_node.effective_visibility,
            Some(Visibility::PUBLIC),
            "merge(sealed via A, public via B) = public — most-open wins"
        );

        assert_eq!(
            closure
                .interface_binaries
                .iter()
                .filter(|(id, name)| id.digest() == c_digest && name.as_str() == "c-tool")
                .count(),
            1,
            "c's binaries claim must be admitted exactly once, not once per path"
        );
    }

    // ── Fail-closed: an offline dep-blob miss aborts the whole closure ──────

    /// Root declares two deps: one whose manifest blob is genuinely absent
    /// (never written) and one that is fully resolvable locally. Under an
    /// offline manager, the walk must abort with the whole-closure error —
    /// never silently return a smaller closure containing only the
    /// resolvable sibling ("couldn't determine != determined zero").
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_offline_dep_blob_miss_fails_the_whole_closure() {
        let dir = TempDir::new().unwrap();

        let missing_id = closure_pinned("missingdep", &cdigest("3"));
        // Deliberately never registered: `register_closure_node` is not
        // called for this digest — the fake source has no entry for it, so
        // the manifest is genuinely absent (`fetch_manifest` returns `None`).

        let present_config = closure_config(serde_json::json!([]), None, serde_json::json!({}));
        let (source, present_digest) = register_closure_node(FakeManifestSource::default(), &present_config);
        let present_id = closure_pinned("presentdep", &present_digest);

        let root_config = closure_config(
            serde_json::json!([
                closure_edge(&missing_id, "public", "missingdep"),
                closure_edge(&present_id, "public", "presentdep"),
            ]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;
        // `mgr.is_offline()` reflects the absence of an OCI *client* (used
        // for install/pull blob downloads), independent of whether the
        // index has a chain source attached — the fixture manager never
        // carries a client (see `make_manager`), so this holds regardless.
        assert!(mgr.is_offline(), "fixture manager must be offline (no client)");

        let err = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect_err("stub gate: currently panics with unimplemented!, will assert this error once implemented");

        assert!(
            matches!(err, PackageErrorKind::Internal(crate::Error::OfflineMode)),
            "an offline dep-blob miss must surface Internal(OfflineMode) (exit 81), got {err:?}"
        );
    }

    // ── CWE-345: dep leaf manifest bytes verified before staging ────────────

    /// A source that serves a dep leaf manifest's bytes under a digest they
    /// do NOT actually hash to (a malicious or compromised registry) must be
    /// rejected by `stage_leaf_manifest`'s verify-before-write, and nothing
    /// may land in the blob store at the requested digest's path. Before the
    /// fix, `stage_leaf_manifest` discarded `fetch_manifest_raw_bytes`'s
    /// returned digest and wrote the served bytes straight under the
    /// requested digest unverified — exactly the shape
    /// `FakeManifestSource::with_digest` forces here.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_dep_leaf_manifest_digest_mismatch_is_rejected_and_nothing_is_written() {
        let dir = TempDir::new().unwrap();

        let dep_config = closure_config(serde_json::json!([]), None, serde_json::json!({}));
        let config_digest = Algorithm::Sha256.hash(dep_config.as_bytes());
        let dep_manifest_bytes = closure_manifest_json(&config_digest, &dep_config);
        // Deliberately NOT the real hash of `dep_manifest_bytes` — the
        // source's claim disagrees with the bytes it actually serves.
        // `cdigest` requires a valid-hex seed, hence "e" not "evil".
        let requested_digest = cdigest("e");
        let dep_id = closure_pinned("mismatcheddep", &requested_digest);

        let source = FakeManifestSource::default()
            .with_digest(
                &requested_digest.to_string(),
                dep_manifest_bytes.as_bytes(),
                requested_digest.clone(),
            )
            .with_blob(&config_digest.to_string(), dep_config.as_bytes());

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "mismatcheddep")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let err = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect_err("a dep leaf manifest whose bytes don't hash to the requested digest must be rejected");

        assert!(
            matches!(
                err,
                PackageErrorKind::Internal(crate::Error::FileStructure(
                    crate::file_structure::error::Error::DigestMismatch { .. }
                ))
            ),
            "must surface the digest-mismatch error (CWE-345), not silently accept the bytes: {err:?}"
        );
        assert!(
            !mgr.file_structure().blobs.data(REGISTRY, &requested_digest).exists(),
            "the mismatched bytes must never be written into the blob store at the requested digest's path"
        );
    }

    // ── Tri-state binaries aggregation ───────────────────────────────────────

    /// An interface-admitted dep with UNDECLARED `binaries` (no key at all)
    /// makes the aggregate `incomplete` — "couldn't determine" must not
    /// silently read as "determined zero". Root itself declares an explicit
    /// empty `binaries: []` so its own contribution can't be what flips the
    /// flag — isolates the assertion to the dep's tri-state.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_undeclared_binaries_on_admitted_dep_makes_aggregate_incomplete() {
        let dir = TempDir::new().unwrap();

        let dep_config = closure_config(serde_json::json!([]), None, serde_json::json!({}));
        let (source, dep_digest) = register_closure_node(FakeManifestSource::default(), &dep_config);
        let dep_id = closure_pinned("dep", &dep_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "dep")]),
            Some(serde_json::json!([])),
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        let dep_node = closure
            .nodes
            .iter()
            .find(|n| n.identifier.digest() == dep_digest)
            .unwrap();
        assert!(
            dep_node.binaries.is_none(),
            "undeclared binaries must round-trip as ClosureNode.binaries == None"
        );
        assert!(
            !closure.interface_binaries_complete,
            "an admitted node with undeclared binaries must flip the aggregate to incomplete"
        );
    }

    /// An interface-admitted dep with `binaries: []` (asserted zero) keeps
    /// the aggregate `complete` — an explicit empty claim is honest, not a
    /// gap. Same isolation as the undeclared-binaries sibling test.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_declared_empty_binaries_on_admitted_dep_keeps_aggregate_complete() {
        let dir = TempDir::new().unwrap();

        let dep_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!([])),
            serde_json::json!({}),
        );
        let (source, dep_digest) = register_closure_node(FakeManifestSource::default(), &dep_config);
        let dep_id = closure_pinned("dep", &dep_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "dep")]),
            Some(serde_json::json!([])),
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        let dep_node = closure
            .nodes
            .iter()
            .find(|n| n.identifier.digest() == dep_digest)
            .unwrap();
        assert!(
            dep_node.binaries.as_ref().is_some_and(|b| b.is_empty()),
            "an explicit empty array must round-trip as ClosureNode.binaries == Some(empty)"
        );
        assert!(
            closure.interface_binaries_complete,
            "an asserted-zero admitted node must keep the aggregate complete"
        );
    }

    // ── Sealed-only closure: aggregate equals the root's own claims ─────────

    /// Root's own deps are ALL sealed and declare no further nesting — the
    /// interface_surface aggregate must equal exactly the root's own
    /// binaries/entrypoints, excluding both sealed deps' claims entirely.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_sealed_only_closure_aggregate_equals_root_claims() {
        let dir = TempDir::new().unwrap();

        let dep1_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["dep1-tool"])),
            serde_json::json!({"dep1-ep": {}}),
        );
        let (source, dep1_digest) = register_closure_node(FakeManifestSource::default(), &dep1_config);
        let dep1_id = closure_pinned("dep1", &dep1_digest);

        let dep2_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["dep2-tool"])),
            serde_json::json!({"dep2-ep": {}}),
        );
        let (source, dep2_digest) = register_closure_node(source, &dep2_config);
        let dep2_id = closure_pinned("dep2", &dep2_digest);

        let root_config = closure_config(
            serde_json::json!([
                closure_edge(&dep1_id, "sealed", "dep1"),
                closure_edge(&dep2_id, "sealed", "dep2"),
            ]),
            Some(serde_json::json!(["root-tool"])),
            serde_json::json!({"root-ep": {}}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert_eq!(closure.interface_binaries.len(), 1);
        assert_eq!(closure.interface_binaries[0].0, root_pinned);
        assert_eq!(closure.interface_binaries[0].1.as_str(), "root-tool");
        assert_eq!(closure.interface_entrypoints.len(), 1);
        assert_eq!(closure.interface_entrypoints[0].0, root_pinned);
        assert_eq!(closure.interface_entrypoints[0].1.as_str(), "root-ep");
    }

    // ── Root node invariant: effective_visibility absent, is_root true ──────

    /// Dedicated pin for the root-node invariant (ADR panel W3): the axis is
    /// undefined for the root itself, so `effective_visibility` must be
    /// `None` exactly when `is_root` is `true` — never the sentinel `public`.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_root_node_has_no_effective_visibility_and_is_root_true() {
        let dir = TempDir::new().unwrap();

        let dep_config = closure_config(serde_json::json!([]), None, serde_json::json!({}));
        let (source, dep_digest) = register_closure_node(FakeManifestSource::default(), &dep_config);
        let dep_id = closure_pinned("dep", &dep_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "dep")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        let root_node = closure.nodes.iter().find(|n| n.is_root).expect("exactly one root node");
        assert!(root_node.effective_visibility.is_none());
        assert_eq!(root_node.identifier.digest(), cdigest("1"));

        let dep_node = closure.nodes.iter().find(|n| !n.is_root).expect("dep node present");
        assert_eq!(dep_node.effective_visibility, Some(Visibility::PUBLIC));
    }

    // ── Goals 4+5 pin: warm walk persists blobs, offline reuse succeeds ─────

    /// After a walk against a source-backed manager (content store attached),
    /// the dep's manifest AND config blobs must be present in local CAS —
    /// `fetch_closure_node` stages the resolved leaf's raw bytes itself
    /// (mirrors `stage_and_link_chain_blobs`'s `ChainRole::Manifest` step,
    /// `adr_index_indirection.md` A3/B2: the local index never caches a leaf,
    /// only `$OCX_HOME/blobs` does) — and a SUBSEQUENT walk against a purely
    /// `Offline` index over the same content store (zero sources) must
    /// succeed with the identical node set, zero network.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_warm_walk_persists_dep_blobs_then_offline_walk_succeeds_purely_local() {
        let dir = TempDir::new().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());

        let dep_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["dep-tool"])),
            serde_json::json!({}),
        );
        let dep_config_digest = Algorithm::Sha256.hash(dep_config.as_bytes());
        let dep_manifest = closure_manifest_json(&dep_config_digest, &dep_config);
        // The staged manifest is read back through the content store
        // (`recover_absent_leaf`'s `digest_matches`, A4) — unlike the
        // flat-leaf deps registered via `register_closure_node` (never
        // digest-verified, see that helper's doc), the identity here must be
        // the real hash of `dep_manifest`, not an arbitrary `cdigest` fixture.
        let dep_manifest_digest = Algorithm::Sha256.hash(dep_manifest.as_bytes());
        let dep_id = closure_pinned("goalsdep", &dep_manifest_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "goalsdep")]),
            None,
            serde_json::json!({}),
        );
        let (_mgr, root_pinned, root_metadata) =
            seed_and_load_root(&dir, FakeManifestSource::default(), &cdigest("1"), &root_config).await;

        let source = SingleDepSource::new(
            dep_manifest_digest.clone(),
            dep_manifest,
            dep_config_digest.clone(),
            dep_config.into_bytes(),
        );
        let source_index = Index::from_impl(source.clone());
        let warm_index = Index::from_chained_with_content_store(
            local_index_at(&dir),
            vec![source_index],
            ChainMode::Default,
            fs.blobs.clone(),
        );

        let closure_warm = super::walk_closure(
            &fs,
            &warm_index,
            false,
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert!(
            source.blob_call_count() > 0,
            "the warm walk must have consulted the source for the dep's config blob"
        );
        assert!(
            fs.blobs.data(REGISTRY, &dep_manifest_digest).exists(),
            "goal 4: the dep's manifest blob must be persisted into the local CAS after the warm walk"
        );
        assert!(
            fs.blobs.data(REGISTRY, &dep_config_digest).exists(),
            "goal 4: the dep's config blob must be persisted into the local CAS after the warm walk"
        );

        // Goal 5: a subsequent purely-local walk (zero sources) over the SAME
        // content store must succeed with the identical node set.
        let offline_index = Index::from_chained_with_content_store(
            local_index_at(&dir),
            Vec::new(),
            ChainMode::Offline,
            fs.blobs.clone(),
        );
        let closure_offline = super::walk_closure(
            &fs,
            &offline_index,
            true,
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("offline walk over a warmed cache must succeed purely locally");

        assert_eq!(
            closure_warm.nodes.len(),
            closure_offline.nodes.len(),
            "the offline-reuse walk must find the identical node set"
        );
    }

    // ── D5 size cap: both steps, exercised through the walker ───────────────

    /// An over-cap DECLARED config descriptor size must reject before any
    /// network blob fetch — the pre-fetch step of D5. The mock source's
    /// `fetch_blob` must observe zero calls.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_over_cap_declared_descriptor_size_rejects_with_zero_fetches() {
        let dir = TempDir::new().unwrap();

        // Config body is irrelevant — the pre-fetch rejection must fire
        // before fetch_blob is ever called — but its digest must still be
        // the real hash: `fetch_blob`'s source walk digest-verifies fetched
        // bytes, and this test wants D5 step 1's rejection, not an
        // incidental `DigestMismatch`. Likewise the dep's own identity must
        // be the real hash of `dep_manifest`: `stage_leaf_manifest` now
        // verifies a fetched leaf manifest's bytes against the digest it was
        // requested under (CWE-345) before D5's checks ever run.
        let config_body = b"{}".to_vec();
        let config_digest = Algorithm::Sha256.hash(&config_body);
        let dep_manifest =
            closure_manifest_json_with_declared_size(&config_digest, common::MAX_METADATA_BLOB_BYTES + 1);
        let dep_manifest_digest = Algorithm::Sha256.hash(dep_manifest.as_bytes());
        let dep_id = closure_pinned("capdep", &dep_manifest_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "capdep")]),
            None,
            serde_json::json!({}),
        );
        let (_mgr, root_pinned, root_metadata) =
            seed_and_load_root(&dir, FakeManifestSource::default(), &cdigest("1"), &root_config).await;

        let source = SingleDepSource::new(dep_manifest_digest, dep_manifest, config_digest, config_body);
        let source_index = Index::from_impl(source.clone());
        let warm_index = Index::from_chained(local_index_at(&dir), vec![source_index], ChainMode::Default);

        let err = super::walk_closure(
            _mgr.file_structure(),
            &warm_index,
            false,
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect_err("stub gate: currently panics with unimplemented!, will assert this error once D2/D5 land");

        assert!(
            matches!(err, PackageErrorKind::Internal(_)),
            "an over-cap declared descriptor must surface Internal (-> DataError/65), got {err:?}"
        );
        assert_eq!(
            source.blob_call_count(),
            0,
            "D5 step 1 (pre-fetch descriptor rejection) must never touch the network"
        );
    }

    /// An honest (small) declared descriptor size paired with an over-cap
    /// FETCHED body must reject post-fetch — the second D5 step, defending
    /// against a registry that lies small in the descriptor and ships big.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_honest_descriptor_over_cap_fetched_body_rejects_post_fetch() {
        let dir = TempDir::new().unwrap();

        // Declared size is honest and small; the actual served body lies —
        // but its digest must still be the real hash of the oversized body
        // (`fetch_blob` digest-verifies before D5 step 2 ever runs). The
        // dep's own identity must likewise be the real hash of
        // `dep_manifest`, or `stage_leaf_manifest`'s CWE-345 verify rejects
        // it before D5 step 2 (config fetch) is ever reached.
        let oversized_body = vec![b'a'; common::MAX_METADATA_BLOB_BYTES + 16];
        let config_digest = Algorithm::Sha256.hash(&oversized_body);
        let dep_manifest = closure_manifest_json_with_declared_size(&config_digest, 2);
        let dep_manifest_digest = Algorithm::Sha256.hash(dep_manifest.as_bytes());
        let dep_id = closure_pinned("capdep2", &dep_manifest_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "capdep2")]),
            None,
            serde_json::json!({}),
        );
        let (_mgr, root_pinned, root_metadata) =
            seed_and_load_root(&dir, FakeManifestSource::default(), &cdigest("1"), &root_config).await;

        let source = SingleDepSource::new(dep_manifest_digest, dep_manifest, config_digest, oversized_body);
        let source_index = Index::from_impl(source.clone());
        let warm_index = Index::from_chained(local_index_at(&dir), vec![source_index], ChainMode::Default);

        let err = super::walk_closure(
            _mgr.file_structure(),
            &warm_index,
            false,
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect_err("stub gate: currently panics with unimplemented!, will assert this error once D2/D5 land");

        assert!(
            matches!(err, PackageErrorKind::Internal(_)),
            "an over-cap fetched body must surface Internal (-> DataError/65), got {err:?}"
        );
        assert!(
            source.blob_call_count() > 0,
            "D5 step 2 fires only after the blob is actually fetched, unlike step 1"
        );
    }

    // ── Conflicts (Codex C2): entrypoint collision + repo-two-digest ────────

    /// Two interface-admitted leaf deps declare the SAME entrypoint name —
    /// `conflicts.entrypoints` must list the name with both owning
    /// identifiers. Inspect stays a view (`Ok`), not a gate.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_conflicts_entrypoint_collision_lists_both_owners() {
        let dir = TempDir::new().unwrap();

        let a_config = closure_config(serde_json::json!([]), None, serde_json::json!({"shared-ep": {}}));
        let (source, a_digest) = register_closure_node(FakeManifestSource::default(), &a_config);
        let a_id = closure_pinned("a", &a_digest);

        let b_config = closure_config(serde_json::json!([]), None, serde_json::json!({"shared-ep": {}}));
        let (source, b_digest) = register_closure_node(source, &b_config);
        let b_id = closure_pinned("b", &b_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&a_id, "public", "a"), closure_edge(&b_id, "public", "b")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert_eq!(closure.conflicts.entrypoints.len(), 1);
        let conflict = &closure.conflicts.entrypoints[0];
        assert_eq!(conflict.name.as_str(), "shared-ep");
        assert_eq!(conflict.packages.len(), 2);
        assert!(conflict.packages.contains(&a_id));
        assert!(conflict.packages.contains(&b_id));
    }

    /// One repository resolved to two distinct digests, BOTH reached via a
    /// public path (interface-admitted) — `conflicts.repositories` must
    /// carry a single entry listing both digests. Exit stays clean (`Ok`).
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_conflicts_repo_two_digest_interface_pair_reports_entry() {
        let dir = TempDir::new().unwrap();

        // `x` and `y` carry distinguishing marker binaries so their manifest
        // JSON — and therefore their real content digest — differ, even
        // though both are otherwise leaf, dep-free nodes; the test's premise
        // (one repository, two distinct digests) needs genuinely distinct
        // digests now that node identity is the real hash, not a forced
        // `cdigest` fixture.
        let (source, x_digest) = register_closure_node(
            FakeManifestSource::default(),
            &closure_config(
                serde_json::json!([]),
                Some(serde_json::json!(["x-marker"])),
                serde_json::json!({}),
            ),
        );
        let x_id = closure_pinned("shared-lib", &x_digest);
        let (source, a_digest) = register_closure_node(
            source,
            &closure_config(
                serde_json::json!([closure_edge(&x_id, "public", "shared-lib")]),
                None,
                serde_json::json!({}),
            ),
        );
        let a_id = closure_pinned("a", &a_digest);

        let (source, y_digest) = register_closure_node(
            source,
            &closure_config(
                serde_json::json!([]),
                Some(serde_json::json!(["y-marker"])),
                serde_json::json!({}),
            ),
        );
        let y_id = closure_pinned("shared-lib", &y_digest);
        let (source, b_digest) = register_closure_node(
            source,
            &closure_config(
                serde_json::json!([closure_edge(&y_id, "public", "shared-lib")]),
                None,
                serde_json::json!({}),
            ),
        );
        let b_id = closure_pinned("b", &b_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&a_id, "public", "a"), closure_edge(&b_id, "public", "b")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert_eq!(closure.conflicts.repositories.len(), 1);
        let conflict = &closure.conflicts.repositories[0];
        assert_eq!(conflict.repository, oci::Repository::from(x_id.as_identifier()));
        assert_eq!(conflict.digests.len(), 2);
        assert!(conflict.digests.contains(&x_digest));
        assert!(conflict.digests.contains(&y_digest));
    }

    /// Same repo-two-digest shape as the interface-pair test, but one of the
    /// two edges is SEALED — that digest never reaches the interface
    /// projection, so only one digest is active on the surface and NO
    /// conflict entry is reported. Exclusion parity with
    /// `collect_repo_digest_conflicts`.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_conflicts_repo_two_digest_sealed_edge_excluded() {
        let dir = TempDir::new().unwrap();

        // Distinguishing marker binaries, same reasoning as the sibling
        // interface-pair test above: `x`/`y` need genuinely distinct real
        // content digests, not a forced `cdigest` fixture.
        let (source, x_digest) = register_closure_node(
            FakeManifestSource::default(),
            &closure_config(
                serde_json::json!([]),
                Some(serde_json::json!(["x2-marker"])),
                serde_json::json!({}),
            ),
        );
        let x_id = closure_pinned("shared-lib2", &x_digest);
        let (source, a_digest) = register_closure_node(
            source,
            &closure_config(
                serde_json::json!([closure_edge(&x_id, "sealed", "shared-lib2")]),
                None,
                serde_json::json!({}),
            ),
        );
        let a_id = closure_pinned("a", &a_digest);

        let (source, y_digest) = register_closure_node(
            source,
            &closure_config(
                serde_json::json!([]),
                Some(serde_json::json!(["y2-marker"])),
                serde_json::json!({}),
            ),
        );
        let y_id = closure_pinned("shared-lib2", &y_digest);
        let (source, b_digest) = register_closure_node(
            source,
            &closure_config(
                serde_json::json!([closure_edge(&y_id, "public", "shared-lib2")]),
                None,
                serde_json::json!({}),
            ),
        );
        let b_id = closure_pinned("b", &b_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&a_id, "public", "a"), closure_edge(&b_id, "public", "b")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert!(
            closure
                .conflicts
                .repositories
                .iter()
                .all(|c| c.repository != oci::Repository::from(x_id.as_identifier())),
            "a repo pair with one sealed edge must not be reported — only one digest is on the interface surface"
        );
    }

    /// A clean closure (no collisions) must report both conflict arrays
    /// empty, always present on the wire (never omitted).
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_conflicts_clean_closure_both_arrays_empty() {
        let dir = TempDir::new().unwrap();

        let (source, dep_digest) = register_closure_node(
            FakeManifestSource::default(),
            &closure_config(serde_json::json!([]), None, serde_json::json!({})),
        );
        let dep_id = closure_pinned("dep", &dep_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&dep_id, "public", "dep")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert!(closure.conflicts.entrypoints.is_empty());
        assert!(closure.conflicts.repositories.is_empty());
    }

    // ── Invariant: digest-parse-before-path (panel sota-4) ──────────────────

    /// A dep pinned to an image index whose sole child descriptor carries a
    /// structurally-invalid `digest` string must error BEFORE any CAS path
    /// is constructed from that value. Black-box proof: the surfaced error
    /// is the structured `Internal(DigestError)` naming the bad literal
    /// (mirrors `inspect_default_bad_child_digest_is_internal_digest_error`
    /// for the root) — a path built from the unparsed string would instead
    /// surface as a generic not-found/read failure, not this specific
    /// digest-parse signature.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_dep_image_index_bad_child_digest_errors_before_path_construction() {
        let dir = TempDir::new().unwrap();

        // Child descriptor `digest` is not a valid `algorithm:hex` string —
        // mirrors the root-level fixture in
        // `inspect_default_bad_child_digest_is_internal_digest_error`. No
        // config is registered — the dep is an image index, and this test
        // errors before a config fetch is ever reached. Unlike a flat leaf
        // manifest, an image-index-shaped dep IS staged into the local
        // index's dispatch-object CAS (`persist_dispatch`/`stage_dispatch_bytes`,
        // A3), which digest-verifies the source's claimed digest against the
        // real hash of the bytes — so the identity digest here must be the
        // real hash, not an arbitrary `cdigest` fixture (unlike the flat-leaf
        // deps registered via `register_closure_node`).
        let index_json = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"not-a-valid-digest","size":7,"platform":{"os":"linux","architecture":"amd64"}}]}"#;
        let depindex_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        let depindex_id = closure_pinned("depindex", &depindex_digest);
        let source = FakeManifestSource::default().with_digest(
            &depindex_digest.to_string(),
            index_json.as_bytes(),
            depindex_digest,
        );

        let root_config = closure_config(
            serde_json::json!([closure_edge(&depindex_id, "public", "depindex")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let err = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &linux_amd64(),
        )
        .await
        .expect_err("stub gate: currently panics with unimplemented!, will assert this error once implemented");

        assert!(
            matches!(err, PackageErrorKind::Internal(_)),
            "malformed child digest must surface Internal(DigestError), got {err:?}"
        );
        let chain = format!("{err:#}");
        assert!(
            chain.contains("not-a-valid-digest"),
            "error chain must name the bad digest value: {chain}"
        );
    }

    // ── Digest-keyed dedup marker (used by the CLI plain-render diamond test) ─

    /// Sanity pin for `closure_pinned_tagged`: two identifiers with
    /// DIFFERENT advisory tags but the SAME digest must compare equal on
    /// content identity (`eq_content`) while differing on `Eq`/`Hash` — the
    /// exact property the CLI plain-render `(*)` dedup relies on being
    /// keyed by digest, not by the tag-bearing identifier.
    #[test]
    fn closure_pinned_tagged_shares_digest_across_distinct_advisory_tags() {
        let d = cdigest("c");
        let one = closure_pinned_tagged("c", "1.0", &d);
        let other = closure_pinned_tagged("c", "latest", &d);
        assert_ne!(one, other, "distinct advisory tags make distinct PinnedIdentifiers");
        assert!(one.eq_content(&other), "content identity must ignore the advisory tag");
        assert_eq!(one.digest(), other.digest());
    }

    // ── Image-index dep platform selection: FeatureMismatch / NotFound ──────

    /// A dep pinned to an image index whose sole child declares `linux/amd64`
    /// under a DIFFERENT `os.features` (libc) than the walk requests — same
    /// os+arch, incompatible feature set. `Index::select` must surface
    /// `SelectResult::FeatureMismatch`, and the walker must propagate
    /// `PackageErrorKind::FeatureMismatch` (DataError/65) — never the generic
    /// `NotFound` (79). Pins the deterministic-65 ruling for a platform gap.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_dep_image_index_incompatible_platform_is_feature_mismatch_not_not_found() {
        let dir = TempDir::new().unwrap();

        // The child is never fetched (selection fails first), so its digest
        // only needs to be well-formed, not registered.
        let child_digest = digest(HEX_B);
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":7,"platform":{{"os":"linux","architecture":"amd64","os.features":["libc.musl"]}}}}]}}"#,
        );
        // Image-index-shaped deps ARE staged into the local index's
        // dispatch-object CAS (A3), which digest-verifies — the identity
        // digest must be the real hash of `index_json`, mirroring
        // `walk_closure_dep_image_index_bad_child_digest_errors_before_path_construction`.
        let depindex_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        let depindex_id = closure_pinned("depindex-fm", &depindex_digest);
        let source = FakeManifestSource::default().with_digest(
            &depindex_digest.to_string(),
            index_json.as_bytes(),
            depindex_digest.clone(),
        );

        let root_config = closure_config(
            serde_json::json!([closure_edge(&depindex_id, "public", "depindex-fm")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let glibc_platform: oci::Platform = "linux/amd64+libc.glibc".parse().expect("valid platform grammar");

        let err = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &glibc_platform,
        )
        .await
        .expect_err("an incompatible-platform image-index dep must surface an error");

        assert!(
            matches!(err, PackageErrorKind::FeatureMismatch { .. }),
            "same os+arch but incompatible os.features must surface FeatureMismatch (DataError/65), \
             not NotFound (79), got {err:?}"
        );
    }

    /// A dep pinned to an image index whose sole child platform-selects
    /// cleanly (matches the walk platform exactly), but the SELECTED CHILD's
    /// manifest bytes were never registered in the source — distinct failure
    /// mode from the incompatible-platform case above: selection succeeds,
    /// the subsequent leaf fetch is what misses.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_dep_image_index_selected_child_manifest_absent_is_not_found() {
        let dir = TempDir::new().unwrap();

        let child_digest = digest(HEX_B);
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":7,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
        );
        let depindex_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        let depindex_id = closure_pinned("depindex-nf", &depindex_digest);
        let source = FakeManifestSource::default().with_digest(
            &depindex_digest.to_string(),
            index_json.as_bytes(),
            depindex_digest.clone(),
        );
        // Deliberately no entry registered for `child_digest` — the selected
        // child's manifest is genuinely absent from the source.

        let root_config = closure_config(
            serde_json::json!([closure_edge(&depindex_id, "public", "depindex-nf")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let err = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            // Explicit `false`, NOT `mgr.is_offline()` (always `true` for a
            // `make_manager`-built manager — see the offline-81 sibling
            // test's doc). This pins the "a source WAS consulted" branch of
            // `closure_fetch_miss`.
            false,
            &root_pinned,
            &root_metadata,
            &linux_amd64(),
        )
        .await
        .expect_err("a selected child with no registered manifest must surface an error");

        assert!(
            matches!(err, PackageErrorKind::NotFound),
            "a platform-selected child whose manifest is absent from the source must surface \
             NotFound (exit 79), got {err:?}"
        );
    }

    /// Happy path: a dep pinned to an image index with exactly one
    /// COMPATIBLE child walks successfully, and the resulting `ClosureNode`'s
    /// identity AND binaries/entrypoints reflect the SELECTED CHILD (Codex
    /// C1 — matches install-time resolution), per `fetch_closure_node`'s
    /// documented contract.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_dep_image_index_compatible_child_reflects_selected_child_metadata() {
        let dir = TempDir::new().unwrap();

        // The child's identity must be the real hash of its manifest bytes:
        // `stage_leaf_manifest` now verifies a fetched dep leaf manifest
        // against the digest it was requested under (CWE-345), so it can no
        // longer be an arbitrary fixture digest independent of content.
        let child_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["child-tool"])),
            serde_json::json!({"child-ep": {}}),
        );
        let (source, child_digest) = register_closure_node(FakeManifestSource::default(), &child_config);

        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":7,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
        );
        let depindex_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        let depindex_id = closure_pinned("depindex-happy", &depindex_digest);
        let source = source.with_digest(
            &depindex_digest.to_string(),
            index_json.as_bytes(),
            depindex_digest.clone(),
        );

        let root_config = closure_config(
            serde_json::json!([closure_edge(&depindex_id, "public", "depindex-happy")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &linux_amd64(),
        )
        .await
        .expect("a compatible image-index dep must walk successfully");

        assert_eq!(closure.nodes.len(), 2, "root + the image-index dep node");
        let dep_node = closure.nodes.iter().find(|n| !n.is_root).expect("dep node present");
        assert_eq!(
            dep_node.identifier.digest(),
            child_digest,
            "the closure node's identity is the SELECTED CHILD's digest (Codex C1 — matches \
             install-time resolution, `pull.rs`'s `info.identifier()`), not the image index's \
             own digest"
        );
        assert_eq!(
            dep_node
                .binaries
                .as_ref()
                .expect("child config declares binaries")
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>(),
            vec!["child-tool"],
            "the dep node's binaries must reflect the SELECTED CHILD's config metadata"
        );
        assert_eq!(
            dep_node.entrypoints.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
            vec!["child-ep"],
            "the dep node's entrypoints must reflect the SELECTED CHILD's config metadata"
        );
    }

    // ── Codex C1: same-content diamond must collapse to one node ────────────

    /// A diamond where root reaches the SAME underlying package via TWO
    /// different declared paths — once through an image index (root's own
    /// edge), once as a flat direct pin one hop deeper via a "wrapper" dep.
    /// (A single node's own `dependencies[]` cannot declare the same
    /// `(registry, repository)` twice — `Dependencies::new`'s
    /// `DuplicateIdentifier` check — so a same-repo diamond can only be
    /// authored across two different nodes, never from one parent's own two
    /// edges; an image index and its platform children always share one OCI
    /// repository, so the wrapper's direct pin and the index's selected
    /// child are the same `(registry, repository)`.) The closure must
    /// collapse both paths to a SINGLE node — claims counted once, no false
    /// same-repo-two-digests conflict — while both of root's own edges
    /// remain listed as authored.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_diamond_via_image_index_and_direct_pin_collapses_to_one_node() {
        let dir = TempDir::new().unwrap();

        let child_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["child-tool"])),
            serde_json::json!({"child-ep": {}}),
        );
        let (source, child_digest) = register_closure_node(FakeManifestSource::default(), &child_config);
        let direct_child_id = closure_pinned("child", &child_digest);

        let wrapper_config = closure_config(
            serde_json::json!([closure_edge(&direct_child_id, "public", "direct")]),
            None,
            serde_json::json!({}),
        );
        let (source, wrapper_digest) = register_closure_node(source, &wrapper_config);
        let wrapper_id = closure_pinned("wrapper", &wrapper_digest);

        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":7,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
        );
        let depindex_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        // Same repo as `direct_child_id` — an image index and its platform
        // children always share one OCI repository; only the digest differs.
        let depindex_id = closure_pinned("child", &depindex_digest);
        let source = source.with_digest(
            &depindex_digest.to_string(),
            index_json.as_bytes(),
            depindex_digest.clone(),
        );

        let root_config = closure_config(
            serde_json::json!([
                closure_edge(&depindex_id, "public", "viaindex"),
                closure_edge(&wrapper_id, "public", "wrapper"),
            ]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &linux_amd64(),
        )
        .await
        .expect("a same-digest diamond reached via an index and a direct pin must still walk successfully");

        assert_eq!(
            closure.nodes.len(),
            3,
            "root + wrapper + ONE collapsed child node — the index path and the direct \
             path resolve to the same digest and must not double-count (Codex C1)"
        );

        let child_nodes: Vec<_> = closure
            .nodes
            .iter()
            .filter(|n| n.identifier.digest() == child_digest)
            .collect();
        assert_eq!(child_nodes.len(), 1, "exactly one closure node for the collapsed child");

        let child_binary_claims = closure
            .interface_binaries
            .iter()
            .filter(|(id, _)| id.digest() == child_digest)
            .count();
        assert_eq!(
            child_binary_claims, 1,
            "the collapsed node's binary claim must be counted exactly once, not once per path"
        );

        assert!(
            closure
                .conflicts
                .repositories
                .iter()
                .all(|c| c.repository != oci::Repository::from(direct_child_id.as_identifier())),
            "the index path and the direct path resolving to the SAME digest under the SAME \
             repository must not be reported as a repo-digest conflict"
        );

        let root_node = closure.nodes.iter().find(|n| n.is_root).expect("root node present");
        assert_eq!(
            root_node.dependencies.len(),
            2,
            "both of root's own edges remain, as authored"
        );
        assert!(
            root_node
                .dependencies
                .iter()
                .any(|e| e.identifier.digest() == depindex_digest),
            "the index edge stays on the root, authored as the index's own digest"
        );
        assert!(
            root_node
                .dependencies
                .iter()
                .any(|e| e.identifier.digest() == wrapper_digest),
            "the wrapper edge stays on the root"
        );
    }

    // ── Codex C2: bounded task admission over a wide frontier ───────────────

    /// `gather_closure_nodes`'s admission loop must bound the number of
    /// per-node fetches SPAWNED at once, not merely the number allowed to run
    /// their fetch body — the previous implementation spawned a `tokio` task
    /// for every discovered edge immediately and relied on an in-task
    /// `Semaphore` to gate only the fetch body. A root with a wide (64-dep)
    /// frontier must still walk successfully, and peak observed concurrency
    /// must never exceed `CLOSURE_FETCH_CONCURRENCY`.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_wide_frontier_never_exceeds_fetch_concurrency_bound() {
        let dir = TempDir::new().unwrap();
        let probe = ConcurrencyProbe::default();

        let mut source = FakeManifestSource::default().with_concurrency_probe(probe.clone());
        let mut dep_edges = Vec::with_capacity(64);
        for index in 0..64 {
            let config = closure_config(serde_json::json!([]), None, serde_json::json!({}));
            let (next_source, dep_digest) = register_closure_node(source, &config);
            source = next_source;
            let dep_id = closure_pinned(&format!("dep{index}"), &dep_digest);
            dep_edges.push(closure_edge(&dep_id, "public", &format!("dep{index}")));
        }

        let root_config = closure_config(serde_json::Value::Array(dep_edges), None, serde_json::json!({}));
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("a wide dependency frontier must still walk successfully");

        assert_eq!(closure.nodes.len(), 65, "root + 64 unique deps");
        assert!(
            probe.peak() <= super::CLOSURE_FETCH_CONCURRENCY,
            "peak concurrent in-flight fetches ({}) must never exceed the admission bound ({})",
            probe.peak(),
            super::CLOSURE_FETCH_CONCURRENCY,
        );
    }

    // ── D6 zero-writes pin: plain inspect never touches the content store ───

    /// `inspect` with `deps: false` must perform zero new writes to the
    /// content blob store, for BOTH `resolve: false` (default mode) and
    /// `resolve: true` — the discriminating case, since only the `--resolve`
    /// branch has a `deps` gate around `stage_chain_blobs` to regress
    /// (`adr_inspect_metadata_closure.md` D6: "a plain inspect without
    /// `--deps` performs zero new writes").
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_plain_without_deps_performs_zero_new_blob_writes() {
        let dir = TempDir::new().unwrap();
        let config_digest = Algorithm::Sha256.hash(METADATA_JSON.as_bytes());
        let manifest_json = image_manifest_json(&config_digest);
        let source = FakeManifestSource::default()
            .with(TAG, manifest_json.as_bytes())
            .with_blob(&config_digest.to_string(), METADATA_JSON.as_bytes());

        let mgr = make_manager(&dir, source);
        let blobs = &mgr.file_structure().blobs;
        assert_eq!(
            blobs.list_all().await.unwrap().len(),
            0,
            "blob store must be empty before any inspect call"
        );

        mgr.inspect(
            &tagged_id(),
            linux_amd64(),
            InspectOptions {
                resolve: false,
                deps: false,
            },
        )
        .await
        .expect("default inspect must succeed");
        assert_eq!(
            blobs.list_all().await.unwrap().len(),
            0,
            "D6: a plain inspect (resolve=false, deps=false) must perform zero new blob writes"
        );

        mgr.inspect(
            &tagged_id(),
            linux_amd64(),
            InspectOptions {
                resolve: true,
                deps: false,
            },
        )
        .await
        .expect("--resolve without --deps must succeed");
        assert_eq!(
            blobs.list_all().await.unwrap().len(),
            0,
            "D6: --resolve without --deps must ALSO perform zero new blob writes — this is the \
             discriminating case pinning the `deps` gate around `stage_chain_blobs`"
        );
    }

    // ── Diamond dedup with distinct advisory tags on the shared digest ──────

    /// Variant of `walk_closure_diamond_dedups_to_one_node_with_merged_visibility`
    /// where the two edges reaching the shared node carry DIFFERENT advisory
    /// tags on the SAME digest (`closure_pinned_tagged`) — the closure must
    /// still dedup to exactly one node, proving dedup is keyed by
    /// digest identity (`strip_advisory`), not by the tag-bearing identifier.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_diamond_with_distinct_advisory_tags_still_dedups_to_one_node() {
        let dir = TempDir::new().unwrap();

        let c_config = closure_config(
            serde_json::json!([]),
            Some(serde_json::json!(["c-tool"])),
            serde_json::json!({}),
        );
        let (source, c_digest) = register_closure_node(FakeManifestSource::default(), &c_config);
        let c_tag1 = closure_pinned_tagged("c", "1.0", &c_digest);
        let c_tag2 = closure_pinned_tagged("c", "latest", &c_digest);

        let a_config = closure_config(
            serde_json::json!([closure_edge(&c_tag1, "sealed", "c")]),
            None,
            serde_json::json!({}),
        );
        let (source, a_digest) = register_closure_node(source, &a_config);
        let a_id = closure_pinned("a", &a_digest);

        let b_config = closure_config(
            serde_json::json!([closure_edge(&c_tag2, "public", "c")]),
            None,
            serde_json::json!({}),
        );
        let (source, b_digest) = register_closure_node(source, &b_config);
        let b_id = closure_pinned("b", &b_digest);

        let root_config = closure_config(
            serde_json::json!([closure_edge(&a_id, "public", "a"), closure_edge(&b_id, "public", "b")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) = seed_and_load_root(&dir, source, &cdigest("1"), &root_config).await;

        let closure = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            mgr.is_offline(),
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect("stub gate: fails with unimplemented! until the Implement phase");

        assert_eq!(
            closure.nodes.len(),
            4,
            "root, a, b, c — c appears once despite two paths with DISTINCT advisory tags on the same digest"
        );
        assert_eq!(
            closure
                .nodes
                .iter()
                .filter(|n| n.identifier.digest() == c_digest)
                .count(),
            1,
            "c must dedup to a single node even when the two edges carry distinct advisory tags"
        );
        assert_eq!(
            closure
                .interface_binaries
                .iter()
                .filter(|(id, name)| id.digest() == c_digest && name.as_str() == "c-tool")
                .count(),
            1,
            "c's binaries claim must be admitted exactly once, not once per distinctly-tagged edge"
        );
    }

    // ── Companion to the offline-81 test: same shape, source consulted online ─

    /// A dep genuinely absent from the source, walked with `offline: false`
    /// (a source WAS consulted, unlike `walk_closure_offline_dep_blob_miss_fails_the_whole_closure`)
    /// must surface `PackageErrorKind::NotFound` (exit 79), not the offline
    /// policy-block mapping.
    #[tokio::test(flavor = "multi_thread")]
    async fn walk_closure_online_dep_genuinely_absent_is_not_found() {
        let dir = TempDir::new().unwrap();

        let missing_id = closure_pinned("missingdep-online", &cdigest("3"));
        // Deliberately never registered via `register_closure_node` — the
        // fake source has no entry for this digest.

        let root_config = closure_config(
            serde_json::json!([closure_edge(&missing_id, "public", "missingdep-online")]),
            None,
            serde_json::json!({}),
        );
        let (mgr, root_pinned, root_metadata) =
            seed_and_load_root(&dir, FakeManifestSource::default(), &cdigest("1"), &root_config).await;

        let err = super::walk_closure(
            mgr.file_structure(),
            mgr.index(),
            // Explicit `false` — `mgr.is_offline()` is always `true` for a
            // `make_manager`-built manager (no client), so this test must
            // NOT reuse it, unlike the offline-81 sibling test which
            // deliberately does.
            false,
            &root_pinned,
            &root_metadata,
            &oci::Platform::any(),
        )
        .await
        .expect_err("a genuinely absent dep under offline=false must surface an error");

        assert!(
            matches!(err, PackageErrorKind::NotFound),
            "an online dep genuinely absent (source consulted, offline=false) must surface \
             NotFound (exit 79), got {err:?}"
        );
    }
}
