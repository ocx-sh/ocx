// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Concurrency-safe garbage collection for the OCX object store.
//!
//! # Concurrency safety
//!
//! `ocx clean` is safe to run alongside concurrent `ocx install` and `ocx pull`
//! invocations.  Before scanning for unreachable objects the GC acquires an
//! **exclusive** advisory lock at `$OCX_STATE_DIR/gc.lock`; install and pull
//! take a **shared** lock on the same file.  Timeout behaviour:
//!
//! - Exclusive (clean): waits up to `OCX_GC_LOCK_TIMEOUT` seconds
//!   ([`DEFAULT_EXCLUSIVE_TIMEOUT`] = 120 s).  On timeout the process exits
//!   with [`crate::Error::GcLockTimeout`] which the CLI maps to exit code 75
//!   (`TempFail`) — the caller may retry.
//! - Shared (install/pull): waits up to 10 s ([`gc_lock::DEFAULT_SHARED_TIMEOUT`]).
//!   On timeout the caller proceeds **without** the lock (debug-logged only).
//!
//! The lock is per-instance — it lives in `$OCX_STATE_DIR` which is always
//! local to the current user/machine.  Cross-machine or cross-container safety
//! on shared content volumes is provided by content-addressing, the mtime grace
//! window, and (when `OCX_SHARED_STORE=true`) the shared-roots ledger.
//!
//! # mtime grace window
//!
//! Objects younger than [`DEFAULT_GRACE_SECONDS`] (600 s, controlled by
//! `OCX_GC_GRACE_SECONDS`) are retained even when they appear unreachable.
//! This closes the TOCTOU window between `ocx install` placing a package
//! directory and registering its install back-ref.  A future mtime (clock
//! skew) is treated as "retain" — the conservative direction.  Setting
//! `OCX_GC_GRACE_SECONDS=0` collects immediately.
//!
//! # Audit log
//!
//! Every deletion (real or `--dry-run`) is appended as a JSONL record to
//! `$OCX_STATE_DIR/gc-log.jsonl` via [`AuditLog`].  The log rotates when it
//! reaches `OCX_GC_LOG_MAX_BYTES` (default 10 MiB); only one previous
//! generation (`gc-log.jsonl.1`) is kept.  Set `OCX_GC_LOG=off` to disable.
//! Failures are best-effort (warn, never fatal).
//!
//! # Shared-roots ledger
//!
//! When `OCX_SHARED_STORE=true`, [`ProjectRootDigests`] is built from
//! `$OCX_PACKAGES_DIR/roots/<instance_id>/` — a per-instance directory of
//! JSON files, one per project.  The GC unions all peer directories before
//! determining what to collect so no object held by a peer container is
//! removed.  Fail-closed: an unreadable committed peer file retains all of
//! that peer's objects rather than risking a spurious deletion.
//!
//! # Network-filesystem posture
//!
//! Before touching any zone `ocx clean` calls
//! [`NetworkFsPosture::from_env`](crate::utility::fs::filesystem_kind::NetworkFsPosture::from_env)
//! (controlled by `OCX_NETWORK_FS`).  `warn` (default) logs and continues;
//! `refuse` returns [`crate::Error::NetworkFsRefused`] → exit 81
//! (`PolicyBlocked`); `allow` skips the detection check.

pub mod audit_log;
pub mod gc_lock;
mod project_roots;
mod reachability_graph;

#[allow(unused_imports)]
pub use audit_log::{AuditAction, AuditLog, AuditRecord, ObjectKind};
pub use gc_lock::{GcLock, lock_timeouts_from_env};
pub use project_roots::ProjectRootDigests;
#[allow(unused_imports)]
pub use reachability_graph::RefLiveness;

/// Returns `true` when the entry at `path` should be **retained** because its
/// directory mtime is within the grace window.
///
/// Grace period: `OCX_GC_GRACE_SECONDS` (default 600). If the mtime is in the
/// future (clock skew) or is zero the entry is also retained (conservative
/// clock-skew guard). A grace of zero disables the check (`always_collect`).
///
/// This predicate is I/O-free given a pre-fetched `mtime` (the caller reads
/// the metadata once and passes the `SystemTime`). This keeps the hot path
/// testable without touching the filesystem.
///
/// Used by [`GarbageCollector::delete_objects`] to skip freshly-assembled
/// objects that have not yet registered their install back-refs — the TOCTOU
/// window that could otherwise cause `clean` to collect a live object that
/// was just installed by a concurrent `ocx install`.
pub fn is_within_grace(mtime: std::time::SystemTime, grace_seconds: u64) -> bool {
    use std::time::SystemTime;

    // A grace of zero disables the window — every entry is immediately
    // collectible regardless of mtime.
    if grace_seconds == 0 {
        return false;
    }

    let now = SystemTime::now();
    match now.duration_since(mtime) {
        // mtime is in the past: retain iff the elapsed age is strictly less
        // than the grace window (an entry exactly at the boundary is collected).
        Ok(age) => age.as_secs() < grace_seconds,
        // `duration_since` errors when mtime is in the FUTURE (clock skew on a
        // shared volume). Retain conservatively — never collect an object whose
        // mtime we cannot trust.
        Err(_) => true,
    }
}

/// Default GC mtime grace window (10 minutes) — spares freshly-assembled
/// objects whose install back-refs are not yet registered.
pub const DEFAULT_GRACE_SECONDS: u64 = 600;

/// Reads `OCX_GC_GRACE_SECONDS` from the environment.
///
/// Returns the default ([`DEFAULT_GRACE_SECONDS`]) when the variable is absent,
/// empty, or unparseable. A value of `0` disables the grace window.
pub fn grace_seconds_from_env() -> u64 {
    crate::env::var(crate::env::keys::OCX_GC_GRACE_SECONDS)
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GRACE_SECONDS)
}

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{
    file_structure::{CasTier, FileStructure},
    log,
};

use reachability_graph::ReachabilityGraph;

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
    /// See [`adr_clean_project_backlinks.md`] for the multi-root design.
    pub async fn build(file_structure: &FileStructure, project_roots: &[ProjectRootDigests]) -> crate::Result<Self> {
        let graph = ReachabilityGraph::build(file_structure, project_roots).await?;
        Ok(Self { graph })
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
    /// Thin wrapper over [`Self::delete_objects_with_policy`] with grace disabled
    /// and audit logging off — preserves the original signature for callers that
    /// do not need the mtime-grace TOCTOU guard or the audit trail (e.g.
    /// `purge`).
    pub async fn delete_objects(&self, targets: &HashSet<PathBuf>, dry_run: bool) -> crate::Result<Vec<PathBuf>> {
        self.delete_objects_with_policy(targets, dry_run, 0, &AuditLog::disabled())
            .await
    }

    /// Deletes the given CAS entry directories, honouring the mtime grace window
    /// and recording each delete (or would-delete) in the audit log.
    ///
    /// For packages: unlinks dependency, layer, and blob forward-refs via
    /// [`ReferenceManager`], then removes the directory. For layers and blobs:
    /// removes the directory directly (they have no outgoing refs).
    ///
    /// **Grace window** (`grace_seconds`): an entry whose directory mtime is
    /// younger than the window is retained (skipped) even when unreachable —
    /// the primary TOCTOU defence against collecting an object a concurrent
    /// `ocx install` just assembled but not yet back-referenced. Future/zero
    /// mtime is retained (clock-skew guard); `grace_seconds == 0` disables the
    /// window. Dry-run honours grace identically.
    ///
    /// **Audit log**: every collected (or would-be-collected in dry-run) entry
    /// emits an [`AuditRecord`]. Logging is best-effort — a log-write failure is
    /// a WARN, never fatal.
    ///
    /// Handles `NotFound` errors from `remove_dir_all` gracefully — a
    /// concurrent deletion or external cleanup is not treated as failure.
    pub async fn delete_objects_with_policy(
        &self,
        targets: &HashSet<PathBuf>,
        dry_run: bool,
        grace_seconds: u64,
        audit: &AuditLog,
    ) -> crate::Result<Vec<PathBuf>> {
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

        let action = if dry_run {
            AuditAction::WouldDelete
        } else {
            AuditAction::Deleted
        };

        for target in sorted_targets {
            // mtime grace: spare entries younger than the grace window. Reading
            // the dir metadata once here keeps the predicate I/O-free.
            if grace_seconds > 0
                && let Some(mtime) = entry_mtime(target).await
                && is_within_grace(mtime, grace_seconds)
            {
                log::debug!(
                    "Retaining '{}' — within {grace_seconds}s grace window.",
                    target.display()
                );
                continue;
            }

            let object_kind = self.object_kind(target);
            let digest = read_entry_digest(target).await;

            if dry_run {
                log::info!("Would remove unreferenced entry: {}", target.display());
                audit
                    .record_delete(action, object_kind, target, digest.as_deref())
                    .await;
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
            audit
                .record_delete(action, object_kind, target, digest.as_deref())
                .await;
            removed.push(target.clone());
        }

        log::debug!(
            "{} {} entry/entries.",
            if dry_run { "Would remove" } else { "Removed" },
            removed.len(),
        );

        Ok(removed)
    }

    /// Maps a target path's CAS tier to the audit-log [`ObjectKind`].
    ///
    /// Entries not present in `all_entries` (should not happen for collection
    /// targets) default to [`ObjectKind::Package`].
    fn object_kind(&self, target: &std::path::Path) -> ObjectKind {
        match self.graph.all_entries.get(target) {
            Some(CasTier::Layer) => ObjectKind::Layer,
            Some(CasTier::Blob) => ObjectKind::Blob,
            _ => ObjectKind::Package,
        }
    }
}

/// Reads the directory mtime of `path`, swallowing I/O errors as `None`.
///
/// A `None` result means "could not determine age" — the caller proceeds to
/// collect (the grace guard only applies when an mtime is available and fresh).
async fn entry_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    tokio::fs::metadata(path).await.ok().and_then(|m| m.modified().ok())
}

/// Reads the full digest string from the entry's sibling `digest` file.
///
/// Returns `None` when the file is absent or unreadable (e.g. a stale temp
/// entry) — the audit record then carries a `null` digest.
async fn read_entry_digest(entry_dir: &std::path::Path) -> Option<String> {
    let digest_file = entry_dir.join(crate::file_structure::DIGEST_FILENAME);
    tokio::fs::read_to_string(&digest_file)
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
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

    // ── is_within_grace ─────────────────────────────────────────────────────
    //
    // Requirement: system_design_shared_store.md §5 M4 item 3 —
    // "skip entry-dir mtime younger than OCX_GC_GRACE_SECONDS (default 600);
    //  future/zero mtime → retain; grace == 0 disables (collect immediately)."
    // Traced to: plan_shared_store P3.2s grace predicate tests.

    #[test]
    fn grace_younger_than_window_is_retained() {
        // An object whose mtime is well within the grace window must be retained.
        // Requirement: plan_shared_store P3.2s "younger-than-grace retained".
        let recent_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
        assert!(
            is_within_grace(recent_mtime, 600),
            "object mtime 100 s ago must be within a 600 s grace window (retain)"
        );
    }

    #[test]
    fn grace_older_than_window_is_collected() {
        // An object whose mtime is past the grace window must NOT be retained.
        // Requirement: plan_shared_store P3.2s "older collected".
        let old_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(700);
        assert!(
            !is_within_grace(old_mtime, 600),
            "object mtime 700 s ago must be outside a 600 s grace window (collect)"
        );
    }

    #[test]
    fn grace_future_mtime_is_retained() {
        // An mtime in the future (clock skew) must be retained conservatively.
        // Requirement: plan_shared_store P3.2s "future mtime retained (clock-skew guard)".
        let future_mtime = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert!(
            is_within_grace(future_mtime, 600),
            "future mtime must be retained (clock-skew guard)"
        );
    }

    #[test]
    fn grace_zero_disables_grace_period() {
        // grace_seconds == 0 means "collect immediately" — no grace window.
        // Requirement: plan_shared_store P3.2s "grace_seconds == 0 disables grace".
        let recent_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(1);
        assert!(
            !is_within_grace(recent_mtime, 0),
            "grace_seconds == 0 must disable grace: even a 1 s old object is collected"
        );
    }

    #[test]
    fn grace_exactly_at_boundary_is_collected() {
        // An object whose mtime is exactly at the grace boundary must be COLLECTED.
        // The predicate is `age.as_secs() < grace_seconds` — strictly less than.
        // An entry at exactly 600 s satisfies `age == grace_seconds`, which is NOT
        // `< grace_seconds`, so it is outside the window and must be collected.
        //
        // Requirement: plan_shared_store P3.2s "older collected" — strict < boundary.
        let boundary_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        assert!(
            !is_within_grace(boundary_mtime, 600),
            "object at exactly the grace boundary must be collected (strict < predicate)"
        );
    }
}
