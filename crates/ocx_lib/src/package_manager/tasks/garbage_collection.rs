// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod project_roots;
mod reachability_graph;

pub use project_roots::ProjectRootDigests;

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{file_structure::FileStructure, log};

use reachability_graph::ReachabilityGraph;

use super::resolve::SitePatchRoots;

/// Garbage collector for the object store.
///
/// Built from the current filesystem state, provides both full GC
/// ([`unreachable_objects`]) and scoped purge ([`orphaned_by_seeds`]).
/// Query methods return sets without side effects; [`delete_objects`]
/// performs the actual filesystem mutations.
pub struct GarbageCollector {
    graph: ReachabilityGraph,
}

impl GarbageCollector {
    /// Builds a [`GarbageCollector`] from the current filesystem state.
    ///
    /// `project_roots` supplies additional GC roots derived from registered
    /// projects' `ocx.lock` files (Unit 6). Pass `&[]` to omit project-registry
    /// roots (used when `--force` is specified or when the registry is unavailable).
    ///
    /// `patch_roots` supplies additional GC roots derived from the site-patch
    /// tier (companion packages + descriptor blobs). Pass `&SitePatchRoots::default()`
    /// when patch roots are irrelevant (e.g. purge, uninstall). Patch roots are
    /// always included in `clean` even under `--force` so required companions
    /// survive GC (invariant C7).
    ///
    /// See [`adr_clean_project_backlinks.md`] for the multi-root design.
    pub async fn build(
        file_structure: &FileStructure,
        project_roots: &[ProjectRootDigests],
        patch_roots: &SitePatchRoots,
    ) -> crate::Result<Self> {
        let graph = ReachabilityGraph::build(file_structure, project_roots).await?;
        let mut collector = Self { graph };

        // Seed patch_roots as additional BFS roots alongside project-registry
        // roots so companion packages and descriptor blobs survive GC even
        // when they have no live install symlinks (invariant C7).
        //
        // Companion packages: seed the package-store directory for each pinned
        // companion identifier as a root.  The BFS then follows refs/layers/
        // and refs/blobs/ edges from those directories, retaining their layers
        // and blobs too.
        for companion_pinned in &patch_roots.companions {
            let raw_path = file_structure.packages.path(companion_pinned);
            // Canonicalize BEFORE the guard: `all_entries` is keyed by canonical
            // paths (`ReachabilityGraph::build` canonicalizes every entry), so a
            // raw-path `contains_key` probe misses whenever `$OCX_HOME` itself
            // sits behind a symlink (macOS `/tmp` -> `/private/tmp`, an NFS or
            // bind-mounted home) — which would drop a present companion's root
            // and over-collect it. Canonicalize, then guard + insert on the
            // canonical path. An absent companion fails to canonicalize, falls
            // back to the raw path, misses the guard, and is correctly skipped.
            let canonical_path = dunce::canonicalize(&raw_path).unwrap_or_else(|error| {
                log::debug!("cannot canonicalize companion path {}: {error}", raw_path.display());
                raw_path
            });
            if collector.graph.all_entries.contains_key(&canonical_path) {
                collector.graph.roots.insert(canonical_path);
            }
        }

        // Descriptor blobs: seed the blob-store directory for each descriptor
        // digest.  The manifest blob and its layer blobs are separate entries;
        // each must be seeded individually (the reachability graph has no
        // "descriptor blob → layer blob" edges — those would require parsing
        // every candidate blob on every GC, which is expensive).
        //
        // SitePatchRoots.descriptors carries (registry, digest) pairs so we
        // can call BlobStore::path(registry, digest) directly — no linear
        // shard-suffix scan needed, and multi-registry correctness is preserved.
        // A blob absent from disk is not in `all_entries` and needs no GC root.
        for (registry, descriptor_digest) in &patch_roots.descriptors {
            let raw_path = file_structure.blobs.path(registry, descriptor_digest);
            // Canonicalize before guard + insert, identical to the companion
            // loop above: `all_entries` keys are canonical, so a raw-path probe
            // would miss a present descriptor blob under a symlinked `$OCX_HOME`
            // and over-collect it.
            let canonical_path = dunce::canonicalize(&raw_path).unwrap_or_else(|error| {
                log::debug!(
                    "cannot canonicalize descriptor blob path {}: {error}",
                    raw_path.display()
                );
                raw_path
            });
            if collector.graph.all_entries.contains_key(&canonical_path) {
                collector.graph.roots.insert(canonical_path);
            }
        }

        Ok(collector)
    }

    /// Returns the attribution map: package-store path → `Vec<ocx.lock paths>`.
    ///
    /// Non-empty only when `project_roots` was non-empty at build time. Used by
    /// `PackageManager::clean` to populate `CleanedObject::held_by` in dry-run
    /// output.
    pub fn roots_attribution(&self) -> &std::collections::HashMap<PathBuf, Vec<PathBuf>> {
        &self.graph.roots_attribution
    }

    /// Returns all entries not reachable from any root.
    ///
    /// Blobs are first-class GC participants: any blob reachable from an
    /// installed package's `refs/blobs/` survives, and any orphan blob is
    /// collected. Follow-up #50 tracks policy-based retention for users who
    /// want stricter retention semantics in shared `$OCX_HOME` scenarios.
    pub fn unreachable_objects(&self) -> HashSet<PathBuf> {
        let reachable = self.graph.reachable();
        self.graph
            .all_entries
            .iter()
            .filter(|(path, _)| !reachable.contains(*path))
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Returns entries newly orphaned by removing the given seeds.
    ///
    /// Computes what becomes unreachable when the seeds are no longer roots:
    ///   reachable_with_seeds    = bfs(roots ∪ seeds)
    ///   reachable_without_seeds = bfs(roots - seeds)
    ///   orphaned = reachable_with_seeds - reachable_without_seeds
    ///
    /// Correct regardless of whether seeds are currently roots in the graph.
    pub fn orphaned_by_seeds(&self, seeds: &[PathBuf]) -> HashSet<PathBuf> {
        let seed_set: HashSet<PathBuf> = seeds.iter().cloned().collect();

        let reachable_with_seeds = self
            .graph
            .bfs(self.graph.roots.iter().cloned().chain(seeds.iter().cloned()));
        let reachable_without_seeds = self
            .graph
            .bfs(self.graph.roots.iter().filter(|r| !seed_set.contains(*r)).cloned());

        reachable_with_seeds
            .difference(&reachable_without_seeds)
            .filter(|p| self.graph.all_entries.contains_key(*p))
            .cloned()
            .collect()
    }

    /// Convenience: combines [`orphaned_by_seeds`] and [`delete_objects`].
    pub async fn purge(&self, obj_dirs: &[PathBuf]) -> crate::Result<Vec<PathBuf>> {
        let targets = self.orphaned_by_seeds(obj_dirs);
        self.delete_objects(&targets, false).await
    }

    /// Deletes the given CAS entry directories from disk.
    ///
    /// For packages: unlinks dependency, layer, and blob forward-refs via
    /// [`ReferenceManager`], then removes the directory. For layers and blobs:
    /// removes the directory directly (they have no outgoing refs).
    ///
    /// Handles `NotFound` errors from `remove_dir_all` gracefully — a
    /// concurrent deletion or external cleanup is not treated as failure.
    ///
    /// **Note:** No guard against concurrent installs. Do not run `clean`
    /// while other OCX operations are in progress.
    pub async fn delete_objects(&self, targets: &HashSet<PathBuf>, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        log::debug!(
            "{} {} entry/entries{}.",
            if dry_run { "Would remove" } else { "Removing" },
            targets.len(),
            if dry_run { " (dry run)" } else { "" },
        );

        let mut removed = Vec::new();
        let mut sorted_targets: Vec<&PathBuf> = targets.iter().collect();
        sorted_targets.sort();

        for target in sorted_targets {
            if dry_run {
                log::info!("Would remove unreferenced entry: {}", target.display());
                removed.push(target.clone());
                continue;
            }

            log::info!("Removing unreferenced entry: {}", target.display());

            match tokio::fs::remove_dir_all(target).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    log::debug!("Entry '{}' already removed (concurrent deletion).", target.display());
                }
                Err(e) => return Err(crate::Error::InternalFile(target.clone(), e)),
            }
            removed.push(target.clone());
        }

        log::debug!(
            "{} {} entry/entries.",
            if dry_run { "Would remove" } else { "Removed" },
            removed.len(),
        );

        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::reachability_graph::tests::{graph, graph_with_tiers, set};
    use super::*;
    use crate::file_structure::CasTier;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("/objects/{name}"))
    }

    fn gc(roots: &[&str], edges: &[(&str, &[&str])], extra: &[&str]) -> GarbageCollector {
        GarbageCollector {
            graph: graph(roots, edges, extra),
        }
    }

    fn gc_with_tiers(
        roots: &[&str],
        edges: &[(&str, &[&str])],
        extra: &[&str],
        tiers: &[(&str, CasTier)],
    ) -> GarbageCollector {
        GarbageCollector {
            graph: graph_with_tiers(roots, edges, extra, tiers),
        }
    }

    // ── unreachable ─────────────────────────────────────────────────────

    #[test]
    fn unreachable_no_roots() {
        assert_eq!(gc(&[], &[("A", &["B"])], &[]).unreachable_objects(), set(&["A", "B"]));
    }

    #[test]
    fn unreachable_skips_reachable() {
        assert_eq!(gc(&["A"], &[("A", &["B"])], &["X"]).unreachable_objects(), set(&["X"]));
    }

    #[test]
    fn empty_graph_nothing_unreachable() {
        assert!(gc(&[], &[], &[]).unreachable_objects().is_empty());
    }

    // ── unreachable (cross-tier) ────────────────────────────────────────

    #[test]
    fn unreachable_layers_are_collected() {
        let collector = gc_with_tiers(
            &["A"],
            &[("A", &["L1"])],
            &["L2"],
            &[("L1", CasTier::Layer), ("L2", CasTier::Layer)],
        );
        // L1 is reachable via A; L2 is unreachable and should be collected.
        assert_eq!(collector.unreachable_objects(), set(&["L2"]));
    }

    /// Test 44 (plan_resolution_chain_refs.md §44): an unreachable blob
    /// (not reachable via any package's refs/blobs/) IS collected by clean.
    #[test]
    fn unreachable_blob_is_collected() {
        // An orphan blob with no root pointing to it must be collected.
        let collector = gc_with_tiers(&["A"], &[], &["orphan_blob"], &[("orphan_blob", CasTier::Blob)]);
        assert!(
            collector.unreachable_objects().contains(&path("orphan_blob")),
            "unreachable blob must be collected by ocx clean"
        );
    }

    /// Test 45 (plan_resolution_chain_refs.md §45): a blob that IS reachable
    /// via a root package's refs/blobs/ edge survives GC.
    ///
    /// This is the converse of test 44: the GC must distinguish reachable
    /// blobs (linked from a package) from unreachable orphans.
    #[test]
    fn reachable_blob_via_refs_blobs_survives_gc() {
        // A (root) → B1 (blob via refs/blobs/). B1 is reachable, must not be collected.
        let collector = gc_with_tiers(&["A"], &[("A", &["B1"])], &[], &[("B1", CasTier::Blob)]);
        assert!(
            !collector.unreachable_objects().contains(&path("B1")),
            "blob reachable via refs/blobs/ from a root package must NOT be collected"
        );
        assert!(
            collector.unreachable_objects().is_empty(),
            "no unreachable objects when root covers all"
        );
    }

    /// Regression: an installed package's metadata config blob, reachable
    /// via its `refs/blobs/` edge, must survive `unreachable_objects()`.
    ///
    /// Before the architectural fix, `ResolvedChain.chain` listed only
    /// manifest blobs (image-index + image-manifest); `link_blobs` therefore
    /// never created `refs/blobs/{config_digest}`, GC treated the config
    /// blob as orphan, and `ocx clean` deleted it while the package was
    /// installed — breaking `ocx --offline install` rehydration.
    #[test]
    fn reachable_config_blob_via_refs_blobs_survives_gc() {
        // pkg (root) → config_blob (blob via refs/blobs/).
        // Identical edge shape to test 45, but documents the config-blob
        // regression intent explicitly so future readers see the bug origin.
        let collector = gc_with_tiers(
            &["pkg"],
            &[("pkg", &["config_blob"])],
            &[],
            &[("config_blob", CasTier::Blob)],
        );
        assert!(
            !collector.unreachable_objects().contains(&path("config_blob")),
            "config blob reachable via refs/blobs/ from an installed package must NOT be collected"
        );
        assert!(
            collector.unreachable_objects().is_empty(),
            "no unreachable objects when the package roots cover the config blob"
        );
    }

    /// Test 46 (plan_resolution_chain_refs.md §46): purge cascades through
    /// all intermediate chain blobs — both the top-level index blob and the
    /// platform manifest blob are purged when their parent package is purged.
    ///
    /// This generalises the existing purge_cascades_through_blobs test to the
    /// full two-blob chain scenario (image index + platform manifest).
    #[test]
    fn purge_cascades_through_intermediate_chain_blobs() {
        // Package A → B1 (image index blob) → B2 (platform manifest blob).
        // Neither B1 nor B2 is a root; both are reachable only via A.
        // Purging A must cascade to both B1 and B2.
        let collector = gc_with_tiers(
            &[],
            &[("A", &["B1"]), ("B1", &["B2"])],
            &[],
            &[("B1", CasTier::Blob), ("B2", CasTier::Blob)],
        );
        let orphaned = collector.orphaned_by_seeds(&[path("A")]);
        assert!(orphaned.contains(&path("A")), "A must be orphaned");
        assert!(orphaned.contains(&path("B1")), "image index blob B1 must be cascaded");
        assert!(
            orphaned.contains(&path("B2")),
            "platform manifest blob B2 must be cascaded"
        );
    }

    #[test]
    fn referenced_blob_survives() {
        // A (root) → B1 (blob). B1 is reachable via BFS, not collected.
        let collector = gc_with_tiers(&["A"], &[("A", &["B1"])], &[], &[("B1", CasTier::Blob)]);
        assert!(collector.unreachable_objects().is_empty());
    }

    #[test]
    fn unreachable_mixed_tiers() {
        let collector = gc_with_tiers(
            &["A"],
            &[("A", &["D", "L1"])],
            &["pkg_orphan", "layer_orphan", "blob_orphan"],
            &[
                ("L1", CasTier::Layer),
                ("layer_orphan", CasTier::Layer),
                ("blob_orphan", CasTier::Blob),
            ],
        );
        // Post-#35: blobs are first-class GC participants, so unreachable
        // orphans across all three tiers are collected.
        assert_eq!(
            collector.unreachable_objects(),
            set(&["pkg_orphan", "layer_orphan", "blob_orphan"])
        );
    }

    // ── orphaned_by_seeds ───────────────────────────────────────────────

    #[test]
    fn purge_cascades_chain() {
        assert_eq!(
            gc(&[], &[("A", &["B"]), ("B", &["C"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B", "C"])
        );
    }

    #[test]
    fn purge_shared_dep_survives() {
        assert_eq!(
            gc(&["B"], &[("A", &["C"]), ("B", &["C"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A"])
        );
    }

    #[test]
    fn purge_shared_dep_both_removed() {
        assert_eq!(
            gc(&[], &[("A", &["C"]), ("B", &["C"])], &[]).orphaned_by_seeds(&[path("A"), path("B")]),
            set(&["A", "B", "C"])
        );
    }

    #[test]
    fn purge_unrelated_orphan_untouched() {
        assert_eq!(
            gc(&[], &[("A", &["B"])], &["X"]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B"])
        );
    }

    #[test]
    fn purge_diamond() {
        assert_eq!(
            gc(&[], &[("A", &["B", "C"]), ("B", &["D"]), ("C", &["D"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B", "C", "D"])
        );
    }

    #[test]
    fn purge_seed_protected_as_dep() {
        assert_eq!(
            gc(&["R"], &[("R", &["A"]), ("A", &["B"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&[])
        );
    }

    #[test]
    fn purge_cycle_terminates() {
        assert_eq!(
            gc(&[], &[("A", &["B"]), ("B", &["A"])], &[]).orphaned_by_seeds(&[path("A")]),
            set(&["A", "B"])
        );
    }

    #[test]
    fn empty_graph_empty_seeds() {
        assert!(gc(&[], &[], &[]).orphaned_by_seeds(&[path("X")]).is_empty());
    }

    // ── orphaned_by_seeds (cross-tier) ──────────────────────────────────

    #[test]
    fn purge_cascades_through_layers() {
        // A → L1 (layer); removing A orphans both A and L1.
        let collector = gc_with_tiers(&[], &[("A", &["L1"])], &[], &[("L1", CasTier::Layer)]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "L1"]));
    }

    #[test]
    fn purge_cascades_through_blobs() {
        // A → B1 (blob); removing A orphans both A and B1.
        let collector = gc_with_tiers(&[], &[("A", &["B1"])], &[], &[("B1", CasTier::Blob)]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "B1"]));
    }

    #[test]
    fn purge_seed_that_is_root() {
        // A is a root (has live install symlink) AND is a seed being purged.
        // The algorithm must still identify A and its deps as orphaned.
        let collector = gc(&["A"], &[("A", &["B"])], &[]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "B"]));
    }

    #[test]
    fn purge_seed_that_is_root_with_shared_dep() {
        // A and B are both roots, sharing dep C. Removing A should orphan A only.
        // C survives via B. Old algorithm: bfs({A,B}) - bfs({A,B}) = {} (wrong).
        let collector = gc(&["A", "B"], &[("A", &["C"]), ("B", &["C"])], &[]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A"]));
    }

    #[test]
    fn purge_seed_that_is_root_private_dep_orphaned_shared_dep_survives() {
        // A (root, seed) → C (private), A → D (shared with B).
        // Removing A: C orphaned, D survives via B.
        let collector = gc(&["A", "B"], &[("A", &["C", "D"]), ("B", &["D"])], &[]);
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A", "C"]));
    }

    #[test]
    fn purge_shared_layer_survives() {
        // A → L1, B → L1; B is a root. Removing A leaves L1 reachable via B.
        let collector = gc_with_tiers(
            &["B"],
            &[("A", &["L1"]), ("B", &["L1"])],
            &[],
            &[("L1", CasTier::Layer)],
        );
        assert_eq!(collector.orphaned_by_seeds(&[path("A")]), set(&["A"]));
    }

    // ── Phase 5A integration tests — patch_roots GC retention ─────────────────
    //
    // These tests call `GarbageCollector::build` with a real `FileStructure` on
    // disk to verify that `patch_roots` seeding keeps companion packages and
    // descriptor blobs alive (spec test 4) and that an empty `patch_roots` lets
    // the same objects be collected (spec test 5).
    //
    // Both tests FAIL against the Phase 5A stub (`let _ = patch_roots;` line in
    // `build`) because the companion package is orphaned even when `patch_roots`
    // is non-empty.  They will pass once `build` seeds `patch_roots` into the
    // reachability graph.

    /// Seed a companion package directory at the CAS path for `pinned`.
    ///
    /// Creates `packages/{registry_slug}/{cas_shard}/content/` so `list_all`
    /// discovers it during the GC build scan.
    fn seed_companion_pkg_dir(root: &std::path::Path, pinned: &crate::oci::PinnedIdentifier) -> std::path::PathBuf {
        let store = crate::file_structure::PackageStore::new(root.join("packages"));
        let pkg_path = store.path(pinned);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        // Minimal metadata.json so the walker classifies the dir as a valid package.
        let meta = serde_json::json!({ "type": "bundle", "version": 1 });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(&crate::package::resolved_package::ResolvedPackage::new()).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();
        pkg_path
    }

    /// Seed a descriptor blob at the CAS path for `(registry, digest)`.
    ///
    /// Creates `blobs/{registry_slug}/{cas_shard}/data` so `list_all` on the
    /// blob store discovers it.
    fn seed_blob_dir(root: &std::path::Path, registry: &str, digest: &crate::oci::Digest) -> std::path::PathBuf {
        let store = crate::file_structure::BlobStore::new(root.join("blobs"));
        let blob_path = store.path(registry, digest);
        std::fs::create_dir_all(&blob_path).unwrap();
        std::fs::write(blob_path.join("data"), b"fake-blob").unwrap();
        blob_path
    }

    /// Spec test 4 — patch_roots retains companion package + descriptor blob.
    ///
    /// Seeds a companion package dir and a descriptor blob dir on disk (no live
    /// install symlink, so neither is reachable from the normal GC graph).
    /// With `patch_roots` seeding both as additional roots, `unreachable_objects`
    /// must NOT include them.
    ///
    /// FAILS against the Phase 5A stub (companion is collected instead of retained).
    ///
    /// Traceability: Phase 5A spec test 4.
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_roots_retains_companion_pkg_and_descriptor_blob() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let registry = "patches.example.com";
        let companion_digest = crate::oci::Digest::Sha256("c".repeat(64));
        let companion_id =
            crate::oci::Identifier::new_registry("ca-bundle", registry).clone_with_digest(companion_digest.clone());
        let companion_pinned = crate::oci::PinnedIdentifier::try_from(companion_id).unwrap();

        let descriptor_digest = crate::oci::Digest::Sha256("d".repeat(64));

        // Seed companion package dir (no refs/symlinks/ → not a GC root by itself).
        let companion_pkg_path = seed_companion_pkg_dir(root, &companion_pinned);
        // Seed descriptor blob dir.
        let descriptor_blob_path = seed_blob_dir(root, registry, &descriptor_digest);

        let fs = crate::file_structure::FileStructure::with_root(root.to_path_buf());

        // Build patch_roots that seed the companion + descriptor blob as GC roots.
        let patch_roots = SitePatchRoots {
            companions: vec![companion_pinned.clone()],
            descriptors: vec![(registry.to_string(), descriptor_digest.clone())],
            // descriptor_pins is the freeze-source list (source key → manifest
            // digest); GC reachability uses `descriptors`, so leave it empty here.
            descriptor_pins: vec![],
        };

        let gc = GarbageCollector::build(&fs, &[], &patch_roots)
            .await
            .expect("GarbageCollector::build must succeed");

        let unreachable = gc.unreachable_objects();

        assert!(
            !unreachable.contains(&companion_pkg_path),
            "spec test 4: companion package dir must NOT be in unreachable_objects when patch_roots seeds it; \
             got unreachable: {unreachable:?}"
        );
        assert!(
            !unreachable.contains(&descriptor_blob_path),
            "spec test 4: descriptor blob dir must NOT be in unreachable_objects when patch_roots seeds it; \
             got unreachable: {unreachable:?}"
        );
    }

    /// Spec test 5 — empty patch_roots allows companion + descriptor to be collected.
    ///
    /// Same seed as spec test 4 but with `patch_roots = SitePatchRoots::default()`.
    /// The companion has no live install symlink and is not reachable from any
    /// project root, so it must appear in `unreachable_objects()`.
    ///
    /// This proves the derive-only invariant: patch roots are the ONLY thing keeping
    /// the companion alive; without them it is correctly collected.
    ///
    /// PASSES against the Phase 5A stub (companion is collected; no seeding needed to
    /// collect).  This test verifies the negative: no over-retain without patch_roots.
    ///
    /// Traceability: Phase 5A spec test 5.
    #[tokio::test(flavor = "multi_thread")]
    async fn empty_patch_roots_allows_companion_collection() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let registry = "patches.example.com";
        let companion_digest = crate::oci::Digest::Sha256("e".repeat(64));
        let companion_id =
            crate::oci::Identifier::new_registry("ca-bundle2", registry).clone_with_digest(companion_digest.clone());
        let companion_pinned = crate::oci::PinnedIdentifier::try_from(companion_id).unwrap();

        let descriptor_digest = crate::oci::Digest::Sha256("f".repeat(64));

        // Seed companion package dir and descriptor blob dir.
        let companion_pkg_path = seed_companion_pkg_dir(root, &companion_pinned);
        let descriptor_blob_path = seed_blob_dir(root, registry, &descriptor_digest);

        let fs = crate::file_structure::FileStructure::with_root(root.to_path_buf());

        // Empty patch_roots — nothing pins the companion or descriptor.
        let gc = GarbageCollector::build(&fs, &[], &SitePatchRoots::default())
            .await
            .expect("GarbageCollector::build must succeed");

        let unreachable = gc.unreachable_objects();

        assert!(
            unreachable.contains(&companion_pkg_path),
            "spec test 5: companion package dir must be in unreachable_objects when patch_roots is empty; \
             got unreachable: {unreachable:?}"
        );
        assert!(
            unreachable.contains(&descriptor_blob_path),
            "spec test 5: descriptor blob dir must be in unreachable_objects when patch_roots is empty; \
             got unreachable: {unreachable:?}"
        );
    }
}
