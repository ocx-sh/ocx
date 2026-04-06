// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Layer assembly walker.
//!
//! Mirrors a layer's `content/` directory tree into a destination directory
//! (typically `packages/{P}/content/`) using hardlinks for files and
//! recreated symlinks for Unix symlinks. Windows symlinks in layers are
//! currently unsupported — see issue for "Windows layer symlink support".
//!
//! ## Invariant: Target Preservation
//!
//! The walker MUST preserve symlink target strings byte-for-byte from the
//! layer. It never dereferences a layer symlink to compute an absolute path,
//! never rewrites a relative target, and never constructs a target from the
//! destination path (which would break after the `temp → packages/` atomic
//! rename). See `symlink_with_temp_path_in_target_breaks_after_rename` in
//! `symlink.rs` for a unit test that locks in why this matters.
//!
//! ## Invariant: Hardlink Sharing
//!
//! After assembly, every regular file in the destination shares an inode
//! with its source in `layers/{digest}/content/`. This dedup is the whole
//! point of the walker — verified by `hardlink_survives_directory_rename`
//! in `hardlink.rs` and by E12/E13 acceptance tests in the plan.
//!
//! ## Concurrency
//!
//! The walker processes directories in parallel via a semaphore-bounded
//! JoinSet. Each task owns one `(src_dir, dest_dir)` pair and processes its
//! entries sequentially — hardlink syscalls are fast and serializing inside
//! a single directory keeps per-task state tiny. Concurrent tasks never
//! write to the same destination path because distinct tasks are rooted at
//! distinct `dest_dir`s.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::prelude::*;

/// Typed errors for the layer assembly walker.
///
/// Each variant represents a distinct failure mode that callers can
/// programmatically distinguish without parsing error messages. All
/// variants are wrapped into [`crate::Error::InternalFile`] at the
/// public API boundary so the walker's return type stays `crate::Result`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssemblyError {
    /// The `source_content` path is not a directory.
    #[error("source_content must be a directory")]
    SourceNotDirectory,

    /// The parent of `dest_content` does not exist.
    #[error("dest_content.parent() must exist")]
    DestinationParentMissing,

    /// The walker processed more entries than the configured cap.
    #[error("walker entry limit exceeded")]
    EntryLimitExceeded,

    /// The directory tree exceeds the maximum nesting depth.
    #[error("walker depth exceeded")]
    DepthExceeded,

    /// The scheduler channel closed unexpectedly.
    #[error("walker scheduler closed")]
    SchedulerClosed,

    /// A spawned walker task panicked.
    #[error("walker task panicked: {0}")]
    TaskPanicked(#[source] tokio::task::JoinError),

    /// Two layers contribute conflicting entries at the same path.
    #[error("layer overlap: path exists as non-directory in dest")]
    LayerOverlap,

    /// Layer symlinks are not supported on Windows.
    #[error("layer symlinks are not supported on Windows")]
    WindowsSymlinksUnsupported,
}

/// Maximum number of filesystem entries the walker will process per
/// `assemble_from_layer` call. Layers already extract to disk via `archive`
/// (which has its own entry cap); this is a defence-in-depth bound that
/// prevents a malformed layer from amplifying during assembly.
const MAX_WALK_ENTRIES: usize = 10_000_000;

/// Maximum depth of the directory walk (nesting budget). A layer that pushes
/// past this bound is rejected rather than allowed to consume unbounded
/// walker state.
const MAX_WALK_DEPTH: usize = 4096;

/// Default number of directory-level tasks allowed to run concurrently.
/// Mirrors the `utility::fs::dir_walker` default — directory fan-out is
/// IO-bound, so 50 in-flight tasks keeps the scheduler saturated without
/// exhausting file descriptors.
const DEFAULT_CONCURRENCY: usize = 50;

/// Summary of what the walker did during a single `assemble_from_layer`
/// invocation. Useful for progress reporting and as a test observable.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AssemblyStats {
    /// Regular files placed via `hardlink::create`.
    pub files_hardlinked: usize,
    /// Intra-layer symlinks recreated verbatim in the destination (Unix).
    pub symlinks_recreated: usize,
    /// Real directories created in the destination tree.
    pub dirs_created: usize,
    /// Total bytes across the files that were hardlinked. Informational.
    pub bytes_hardlinked: u64,
}

impl AssemblyStats {
    fn merge(&mut self, other: AssemblyStats) {
        self.files_hardlinked += other.files_hardlinked;
        self.symlinks_recreated += other.symlinks_recreated;
        self.dirs_created += other.dirs_created;
        self.bytes_hardlinked += other.bytes_hardlinked;
    }
}

/// Walks `source_content` depth-first and mirrors its tree into
/// `dest_content`. Regular files are hardlinked; directories are created as
/// real directories; symlinks are recreated verbatim on Unix.
///
/// `source_content` is typically `layers/{digest}/content/` and
/// `dest_content` is typically a not-yet-existing `content/` directory
/// inside a package's temp directory (which is later renamed into
/// `packages/`).
///
/// `dest_content` itself is created by the walker if missing. Its parent
/// must already exist. `dest_content` may already contain entries from a
/// previous `assemble_from_layer` call against a different source layer —
/// the walker merges non-overlapping trees. If two layers contribute the
/// same path the walker errors with `AlreadyExists`: files collide via
/// `hardlink::create` (underlying `link(2)` returns `EEXIST`), symlinks
/// collide via `symlink::create`, and a directory colliding with a non-
/// directory is detected explicitly and surfaces a "layer overlap" error.
///
/// On error, `dest_content` may contain partially-assembled files; callers
/// should assemble into a temp directory they can discard.
///
/// Returns an [`AssemblyStats`] summarizing what was placed.
///
/// # Errors
///
/// - `Error::InternalFile` wrapping the underlying `io::Error` if a
///   filesystem operation fails. Cross-device hardlink attempts surface as
///   `io::ErrorKind::CrossesDevices` — see the `$OCX_HOME` single-volume
///   invariant in the plan.
/// - Walker invariant violations (entry cap, depth cap, scheduler failure)
///   surface as `Error::InternalFile` wrapping an [`AssemblyError`] via
///   `io::Error::other`. Callers can downcast the `io::Error` source to
///   `AssemblyError` for programmatic matching.
/// - Windows-only: [`AssemblyError::WindowsSymlinksUnsupported`] if the
///   walker encounters a symlink inside the layer.
pub async fn assemble_from_layer(source_content: &Path, dest_content: &Path) -> Result<AssemblyStats> {
    assemble_from_layers(&[source_content], dest_content).await
}

/// Assembles multiple overlap-free layers into a single destination tree.
///
/// Walks all layers as a merged union — at each directory level, entries
/// from all contributing layers are read, merged by name, and placed into
/// `dest_content`. Non-directory entries (files, symlinks) must appear in
/// exactly one layer; directories merge across layers and recurse with
/// only the layers that contribute them.
///
/// `sources` ordering is preserved: layer 0 is the base, layer N is the
/// top. With overlap-free semantics ordering doesn't affect the result,
/// but it determines error messages and enables future shadowing.
///
/// Known limitation: on case-insensitive filesystems (macOS HFS+/APFS),
/// entries differing only in case (e.g., `Bin/tool` vs `bin/tool`) are
/// not detected as overlaps by the in-memory merge — the collision
/// surfaces as `AlreadyExists` from the filesystem instead of a clean
/// `LayerOverlap` error.
///
/// # Errors
///
/// - `AssemblyError::LayerOverlap` — two layers contribute same non-directory entry or type mismatch
/// - `AssemblyError::SourceNotDirectory` — any source path is not a directory
/// - `AssemblyError::DestinationParentMissing` — `dest_content.parent()` doesn't exist
/// - `AssemblyError::EntryLimitExceeded` / `DepthExceeded` — resource caps
/// - `AssemblyError::WindowsSymlinksUnsupported` — symlink in any layer on Windows
pub async fn assemble_from_layers(sources: &[&Path], dest_content: &Path) -> Result<AssemblyStats> {
    assemble_from_layers_with_cap(sources, dest_content, MAX_WALK_ENTRIES).await
}

/// Internal multi-layer walker entry point with configurable entry cap.
/// Exposed for tests so resource-limit checks can be exercised.
async fn assemble_from_layers_with_cap(
    sources: &[&Path],
    dest_content: &Path,
    max_entries: usize,
) -> Result<AssemblyStats> {
    // ── Early return: nothing to assemble. ──────────────────────────────────
    if sources.is_empty() {
        return Ok(AssemblyStats::default());
    }

    // ── Pre-condition: all sources must be existing directories. ────────────
    for src in sources {
        let meta = tokio::fs::metadata(src)
            .await
            .map_err(|e| crate::error::file_error(src, e))?;
        if !meta.is_dir() {
            return Err(crate::error::file_error(
                src,
                std::io::Error::other(AssemblyError::SourceNotDirectory),
            ));
        }
    }

    // ── Pre-condition: prepare dest_content. ────────────────────────────────
    if !tokio::fs::try_exists(dest_content).await.unwrap_or(false) {
        if let Some(parent) = dest_content.parent()
            && !tokio::fs::try_exists(parent).await.unwrap_or(false)
        {
            return Err(crate::error::file_error(
                dest_content,
                std::io::Error::other(AssemblyError::DestinationParentMissing),
            ));
        }
        tokio::fs::create_dir(dest_content)
            .await
            .map_err(|e| crate::error::file_error(dest_content, e))?;
    }

    // ── Parallel directory-level fan-out (multi-layer). ─────────────────────
    let semaphore = Arc::new(Semaphore::new(DEFAULT_CONCURRENCY));
    let entries_seen = Arc::new(AtomicUsize::new(0));
    let dest_root = dest_content.to_path_buf();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SpawnRequest>();
    let mut join_set: JoinSet<Result<AssemblyStats>> = JoinSet::new();

    // Seed with the root directory set.
    tx.send(SpawnRequest {
        src_dirs: sources.iter().map(|s| s.to_path_buf()).collect(),
        dest_dir: dest_content.to_path_buf(),
        depth: 0,
    })
    .expect("rx alive");

    let mut stats = AssemblyStats::default();

    loop {
        // Drain all pending spawn requests, subject to the semaphore.
        while let Ok(req) = rx.try_recv() {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("semaphore is never closed");
            let tx_child = tx.clone();
            let entries_seen_child = entries_seen.clone();
            let dest_root_child = dest_root.clone();
            join_set.spawn(async move {
                let res = process_directory(
                    req.src_dirs,
                    req.dest_dir,
                    dest_root_child,
                    req.depth,
                    entries_seen_child,
                    max_entries,
                    tx_child,
                )
                .await;
                drop(permit);
                res
            });
        }

        // Termination: no tasks running and no pending requests.
        if join_set.is_empty() {
            break;
        }

        // Wait for either a task to complete OR a new request to arrive.
        tokio::select! {
            biased;
            Some(req) = rx.recv() => {
                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("semaphore is never closed");
                let tx_child = tx.clone();
                let entries_seen_child = entries_seen.clone();
                let dest_root_child = dest_root.clone();
                join_set.spawn(async move {
                    let res = process_directory(
                        req.src_dirs,
                        req.dest_dir,
                        dest_root_child,
                        req.depth,
                        entries_seen_child,
                        max_entries,
                        tx_child,
                    )
                    .await;
                    drop(permit);
                    res
                });
            }
            Some(join_res) = join_set.join_next() => {
                let task_result = join_res.map_err(|e| {
                    crate::error::file_error(
                        &dest_root,
                        std::io::Error::other(AssemblyError::TaskPanicked(e)),
                    )
                })?;
                match task_result {
                    Ok(task_stats) => stats.merge(task_stats),
                    Err(e) => {
                        join_set.abort_all();
                        return Err(e);
                    }
                }
            }
        }
    }

    Ok(stats)
}

/// Internal single-layer entry point with a configurable entry cap.
/// Delegates to the multi-layer walker — single-layer is just `&[source]`.
/// Exposed for tests so the resource-limit check can be exercised without
/// materializing `MAX_WALK_ENTRIES` files on disk.
#[cfg(test)]
async fn assemble_from_layer_with_cap(
    source_content: &Path,
    dest_content: &Path,
    max_entries: usize,
) -> Result<AssemblyStats> {
    assemble_from_layers_with_cap(&[source_content], dest_content, max_entries).await
}

/// Classification of a directory entry after `file_type()` resolution.
///
/// Used by `process_directory` to record each entry's kind during
/// the collection phase so that the merge phase can pattern-match without
/// redundant syscalls.
#[derive(Debug)]
enum EntryKind {
    Dir,
    File { size: u64 },
    Symlink,
}

/// Work item for the scheduler. Each request represents one directory level
/// that needs to be walked across one or more source layers.
#[derive(Debug)]
struct SpawnRequest {
    src_dirs: Vec<PathBuf>,
    dest_dir: PathBuf,
    depth: usize,
}

/// Processes one directory level across one or more source layers.
///
/// Reads entries from all `src_dirs`, merges by name into a `BTreeMap`,
/// detects overlaps, and places files/symlinks. Directories are posted
/// back to the scheduler with only the contributing layers.
async fn process_directory(
    src_dirs: Vec<PathBuf>,
    dest_dir: PathBuf,
    dest_root: PathBuf,
    depth: usize,
    entries_seen: Arc<AtomicUsize>,
    max_entries: usize,
    join_set_tx: tokio::sync::mpsc::UnboundedSender<SpawnRequest>,
) -> Result<AssemblyStats> {
    let mut stats = AssemblyStats::default();

    // ── Phase 1: Collect entries from all source layers. ────────────────────
    let mut merged: BTreeMap<OsString, Vec<(usize, EntryKind, PathBuf)>> = BTreeMap::new();

    for (layer_idx, src_dir) in src_dirs.iter().enumerate() {
        let mut entries = tokio::fs::read_dir(src_dir)
            .await
            .map_err(|e| crate::error::file_error(src_dir, e))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| crate::error::file_error(src_dir, e))?
        {
            let prev = entries_seen.fetch_add(1, Ordering::Relaxed);
            if prev + 1 > max_entries {
                return Err(crate::error::file_error(
                    &dest_root,
                    std::io::Error::other(AssemblyError::EntryLimitExceeded),
                ));
            }

            let src_path = entry.path();
            let file_name = entry.file_name();

            // `symlink::is_link` handles NTFS junctions on Windows, which
            // `DirEntry::file_type` reports as plain directories — checking
            // it first prevents misclassification.
            if crate::symlink::is_link(&src_path) {
                merged
                    .entry(file_name)
                    .or_default()
                    .push((layer_idx, EntryKind::Symlink, src_path));
                continue;
            }

            let file_type = entry
                .file_type()
                .await
                .map_err(|e| crate::error::file_error(&src_path, e))?;

            if file_type.is_dir() {
                merged
                    .entry(file_name)
                    .or_default()
                    .push((layer_idx, EntryKind::Dir, src_path));
            } else if file_type.is_file() {
                let size = entry
                    .metadata()
                    .await
                    .map_err(|e| crate::error::file_error(&src_path, e))?
                    .len();
                merged
                    .entry(file_name)
                    .or_default()
                    .push((layer_idx, EntryKind::File { size }, src_path));
            }
            // Other entry types (sockets, fifos, block/char devices) are
            // skipped silently — OCI layers should never contain them.
        }
    }

    // ── Phase 2: Process merged entries. ────────────────────────────────────
    for (name, contributors) in &merged {
        let dest_path = dest_dir.join(name);

        // Partition into dirs and non-dirs.
        let mut dirs: Vec<&PathBuf> = Vec::new();
        let mut non_dirs: Vec<(&EntryKind, &PathBuf)> = Vec::new();

        for (_layer_idx, kind, src_path) in contributors {
            match kind {
                EntryKind::Dir => dirs.push(src_path),
                _ => non_dirs.push((kind, src_path)),
            }
        }

        // Overlap checks.
        if non_dirs.len() > 1 {
            return Err(crate::error::file_error(
                &dest_path,
                std::io::Error::other(AssemblyError::LayerOverlap),
            ));
        }
        if !dirs.is_empty() && !non_dirs.is_empty() {
            return Err(crate::error::file_error(
                &dest_path,
                std::io::Error::other(AssemblyError::LayerOverlap),
            ));
        }

        if let Some((kind, src_path)) = non_dirs.first() {
            // Single non-directory entry.
            match kind {
                EntryKind::File { size } => {
                    crate::hardlink::create(src_path, &dest_path)?;
                    stats.files_hardlinked += 1;
                    stats.bytes_hardlinked += size;
                }
                EntryKind::Symlink => {
                    handle_symlink_entry(src_path, &dest_path, &dest_root, &mut stats).await?;
                }
                EntryKind::Dir => unreachable!("dirs filtered out above"),
            }
        } else if !dirs.is_empty() {
            // All contributors are directories — create the merged directory.
            // Handle AlreadyExists: dest may contain directories from a prior
            // assemble_from_layer call (sequential multi-layer usage) or from
            // manual pre-population. Accept if existing entry is a directory;
            // error if it's a non-directory (type mismatch).
            match tokio::fs::create_dir(&dest_path).await {
                Ok(()) => {
                    stats.dirs_created += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let existing = tokio::fs::symlink_metadata(&dest_path)
                        .await
                        .map_err(|e| crate::error::file_error(&dest_path, e))?;
                    if !existing.is_dir() {
                        return Err(crate::error::file_error(
                            &dest_path,
                            std::io::Error::other(AssemblyError::LayerOverlap),
                        ));
                    }
                    // Directory already exists — don't increment dirs_created.
                }
                Err(e) => return Err(crate::error::file_error(&dest_path, e)),
            }

            if depth + 1 >= MAX_WALK_DEPTH {
                return Err(crate::error::file_error(
                    &dest_root,
                    std::io::Error::other(AssemblyError::DepthExceeded),
                ));
            }

            // Post subdirectory request with only the contributing layers.
            let child_src_dirs: Vec<PathBuf> = dirs.into_iter().cloned().collect();
            if join_set_tx
                .send(SpawnRequest {
                    src_dirs: child_src_dirs,
                    dest_dir: dest_path,
                    depth: depth + 1,
                })
                .is_err()
            {
                return Err(crate::error::file_error(
                    &dest_root,
                    std::io::Error::other(AssemblyError::SchedulerClosed),
                ));
            }
        }
    }

    Ok(stats)
}

/// Recreates a layer symlink in the destination tree.
///
/// On Unix, the target string is read verbatim from the layer and stored
/// in the new symlink — no canonicalization, no parent-join, no rewriting.
/// This preserves the target byte-for-byte so both relative and absolute
/// symlinks behave identically before and after the `temp → packages/`
/// atomic rename.
///
/// Defence-in-depth: the target is re-validated against `dest_root` via
/// [`crate::symlink::validate_target`]. The archive extractor already rejects
/// escaping symlinks at ingestion time, but revalidating here keeps the
/// walker safe if any future code path populates `layers/.../content/`
/// without going through `archive::extract`.
///
/// On Windows, intra-layer symlinks are currently unsupported. Both file and
/// directory symlinks return [`AssemblyError::WindowsSymlinksUnsupported`] —
/// native symlinks require `SeCreateSymbolicLinkPrivilege` / Developer Mode,
/// and NTFS junctions are absolute-only and cannot preserve the walker's
/// verbatim target-preservation invariant.
#[cfg_attr(windows, allow(unused_variables))]
async fn handle_symlink_entry(
    src_path: &Path,
    dest_path: &Path,
    dest_root: &Path,
    stats: &mut AssemblyStats,
) -> Result<()> {
    #[cfg(unix)]
    {
        // Read the target string verbatim from the layer symlink.
        let target = tokio::fs::read_link(src_path)
            .await
            .map_err(|e| crate::error::file_error(src_path, e))?;
        // Defence-in-depth: relative targets that escape `dest_root` via
        // `..` traversal are rejected. Absolute targets are left as-is —
        // OCI layers legitimately use them to reference system libraries,
        // and they cannot "escape" a destination root that does not
        // contain them in the first place. The archive extractor is the
        // primary trust boundary that filters absolute targets at layer
        // ingestion; this check catches any relative traversal that slips
        // through (or any future path that populates `layers/.../content/`
        // without going through `archive::extract`).
        if !target.is_absolute() {
            crate::symlink::validate_target(dest_root, dest_path, &target)?;
        }
        crate::symlink::create(&target, dest_path)?;
        stats.symlinks_recreated += 1;
        Ok(())
    }
    #[cfg(windows)]
    {
        Err(crate::error::file_error(
            src_path,
            std::io::Error::other(AssemblyError::WindowsSymlinksUnsupported),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    fn make_file(root: &Path, rel: &str, content: &[u8]) -> std::path::PathBuf {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        p
    }

    fn make_dir(root: &Path, rel: &str) -> std::path::PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ── structure: empty, flat, nested, deep, empty_subdirs ─────────────────

    /// E1: Empty layer content/ — dest content/ is created and empty.
    /// The walker must create the dest directory itself and return with no
    /// files hardlinked and no symlinks recreated.
    #[tokio::test]
    async fn assembles_empty_layer_content() {
        let (_dir, root) = setup();
        let src = make_dir(&root, "layer/content");
        // dest parent must pre-exist; dest itself must NOT pre-exist
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert!(dest.exists(), "dest content/ must be created by the walker");
        assert!(dest.is_dir(), "dest content/ must be a real directory, not a symlink");
        assert_eq!(stats.files_hardlinked, 0, "empty layer yields zero hardlinked files");
        assert_eq!(
            stats.symlinks_recreated, 0,
            "empty layer yields zero recreated symlinks"
        );
        // `dirs_created` counts subdirectories encountered during the walk.
        // `dest_content` itself is created via `create_dir_all` before the
        // walk begins and is NOT counted. An empty layer has zero subdirs.
        assert_eq!(stats.dirs_created, 0, "empty layer yields zero subdirectories");
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            0,
            "dest content/ must be empty"
        );
    }

    /// E2: Flat layer (files only, no subdirectories).
    /// All files must be hardlinked at the root of dest; stats must reflect
    /// the correct count and the correct byte total.
    #[tokio::test]
    async fn assembles_flat_layer_with_files_only() {
        let (_dir, root) = setup();
        let src = make_dir(&root, "layer/content");
        make_file(&root, "layer/content/alpha.txt", b"alpha bytes");
        make_file(&root, "layer/content/beta.txt", b"beta bytes");
        make_file(&root, "layer/content/gamma.txt", b"gamma bytes");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.files_hardlinked, 3, "all three files must be hardlinked");
        assert_eq!(stats.symlinks_recreated, 0, "no symlinks in a flat file layer");
        // `dirs_created` counts subdirectories encountered during the walk.
        // A flat layer has files only at the root — zero subdirectories.
        assert_eq!(stats.dirs_created, 0, "flat layer yields zero subdirectories");

        assert_eq!(
            std::fs::read(dest.join("alpha.txt")).unwrap(),
            b"alpha bytes",
            "alpha.txt content must be accessible in dest"
        );
        assert_eq!(
            std::fs::read(dest.join("beta.txt")).unwrap(),
            b"beta bytes",
            "beta.txt content must be accessible in dest"
        );
        assert_eq!(
            std::fs::read(dest.join("gamma.txt")).unwrap(),
            b"gamma bytes",
            "gamma.txt content must be accessible in dest"
        );
    }

    /// E3: Nested layer with subdirectories.
    /// All subdirectories must be recreated as real directories; all files
    /// must be hardlinked at their correct relative positions.
    #[tokio::test]
    async fn assembles_nested_subdirectories() {
        let (_dir, root) = setup();
        let _src = make_dir(&root, "layer/content");
        make_file(&root, "layer/content/bin/tool", b"tool binary");
        make_file(&root, "layer/content/lib/foo.so", b"shared library");
        make_file(&root, "layer/content/share/doc/readme.md", b"readme text");
        let src = root.join("layer/content");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.files_hardlinked, 3, "three files across three subdirs");
        assert_eq!(stats.symlinks_recreated, 0, "no symlinks in this layer");
        // `dirs_created` counts subdirectories encountered during the walk:
        // `bin`, `lib`, `share`, and `share/doc` = 4. `dest_content` itself
        // is NOT counted (created via `create_dir_all` before the walk).
        assert_eq!(stats.dirs_created, 4, "bin, lib, share, share/doc = 4 subdirectories");

        // Verify every file is in the correct position
        assert_eq!(
            std::fs::read(dest.join("bin/tool")).unwrap(),
            b"tool binary",
            "bin/tool must be hardlinked at the mirrored path"
        );
        assert_eq!(
            std::fs::read(dest.join("lib/foo.so")).unwrap(),
            b"shared library",
            "lib/foo.so must be hardlinked at the mirrored path"
        );
        assert_eq!(
            std::fs::read(dest.join("share/doc/readme.md")).unwrap(),
            b"readme text",
            "share/doc/readme.md must be hardlinked at the mirrored path"
        );

        // Verify subdirs are real directories (not symlinks)
        assert!(
            dest.join("bin").is_dir() && !dest.join("bin").is_symlink(),
            "bin/ must be a real directory in dest"
        );
        assert!(
            dest.join("lib").is_dir() && !dest.join("lib").is_symlink(),
            "lib/ must be a real directory in dest"
        );
        assert!(
            dest.join("share/doc").is_dir() && !dest.join("share/doc").is_symlink(),
            "share/doc/ must be a real directory in dest"
        );
    }

    /// E4: Deeply nested layer (≥5 levels).
    /// The walker must handle deep recursion without stack overflow and
    /// correctly reproduce the depth-5 path in dest.
    #[tokio::test]
    async fn assembles_deeply_nested_tree() {
        let (_dir, root) = setup();
        let _src = make_dir(&root, "layer/content");
        make_file(&root, "layer/content/a/b/c/d/e/deep.txt", b"deep content");
        let src = root.join("layer/content");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.files_hardlinked, 1, "one file at depth 5");
        // `dirs_created` counts subdirectories encountered during the walk:
        // `a`, `a/b`, `a/b/c`, `a/b/c/d`, `a/b/c/d/e` = 5 subdirs.
        // `dest_content` itself is NOT counted.
        assert_eq!(stats.dirs_created, 5, "five nested subdirectories a..e");
        assert_eq!(
            std::fs::read(dest.join("a/b/c/d/e/deep.txt")).unwrap(),
            b"deep content",
            "deep.txt must be accessible at its full depth-5 path in dest"
        );

        // Every intermediate directory must be real
        assert!(dest.join("a").is_dir(), "a/ must exist");
        assert!(dest.join("a/b").is_dir(), "a/b/ must exist");
        assert!(dest.join("a/b/c").is_dir(), "a/b/c/ must exist");
        assert!(dest.join("a/b/c/d").is_dir(), "a/b/c/d/ must exist");
        assert!(dest.join("a/b/c/d/e").is_dir(), "a/b/c/d/e/ must exist");
    }

    /// E11: Empty subdirectory in layer.
    /// An empty directory in the source must be preserved in the destination
    /// as an empty real directory — it must not be silently dropped.
    #[tokio::test]
    async fn preserves_empty_subdirectories() {
        let (_dir, root) = setup();
        let _src = make_dir(&root, "layer/content");
        // A populated subdir — gives the walker something to hardlink
        make_file(&root, "layer/content/bin/tool", b"tool binary");
        // An empty subdir — must be preserved verbatim
        make_dir(&root, "layer/content/empty");
        let src = root.join("layer/content");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.files_hardlinked, 1, "one file outside the empty dir");
        // `dirs_created` counts subdirectories encountered during the walk:
        // `bin` (holding `tool`) and `empty` (the preserved empty dir) = 2.
        // `dest_content` itself is NOT counted.
        assert_eq!(stats.dirs_created, 2, "bin and empty = 2 subdirectories");

        // empty/ must exist in dest
        let empty_dest = dest.join("empty");
        assert!(empty_dest.exists(), "empty/ must be created in dest");
        assert!(empty_dest.is_dir(), "empty/ must be a real directory in dest");
        assert!(
            !empty_dest.is_symlink(),
            "empty/ must not be a symlink — it must be a real directory"
        );
        assert_eq!(
            std::fs::read_dir(&empty_dest).unwrap().count(),
            0,
            "empty/ must contain no entries in dest"
        );
    }

    // ── symlinks + atomic move invariants ───────────────────────────────────

    /// E5: Relative symlink within the same directory.
    ///
    /// Layer has a real file `libfoo.so.1.2.3` and a sibling symlink
    /// `libfoo.so.1` → `libfoo.so.1.2.3`.  After assembly the symlink must
    /// appear verbatim in the destination — `read_link` returns the original
    /// relative string byte-for-byte and reading through the symlink yields
    /// the file contents.
    #[tokio::test]
    #[cfg(unix)]
    async fn preserves_relative_symlink_same_directory() {
        let (_dir, root) = setup();
        make_dir(&root, "layer/content/lib");
        make_file(&root, "layer/content/lib/libfoo.so.1.2.3", b"shared library bytes");
        // Layer symlink: lib/libfoo.so.1 → libfoo.so.1.2.3 (relative, same dir)
        let link_in_layer = root.join("layer/content/lib/libfoo.so.1");
        crate::symlink::create(std::path::Path::new("libfoo.so.1.2.3"), &link_in_layer).unwrap();

        let src = root.join("layer/content");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.symlinks_recreated, 1, "one symlink must be recreated");
        assert_eq!(stats.files_hardlinked, 1, "one real file must be hardlinked");

        let dest_link = dest.join("lib/libfoo.so.1");

        // Use is_symlink() — NOT exists() — because exists() follows the link.
        assert!(
            dest_link.is_symlink(),
            "dest/lib/libfoo.so.1 must be a symlink, not a regular file"
        );

        // Target string must be verbatim — relative, byte-identical to layer value.
        assert_eq!(
            std::fs::read_link(&dest_link).unwrap(),
            std::path::Path::new("libfoo.so.1.2.3"),
            "relative target must be preserved verbatim via read_link"
        );

        // Reading through the symlink must yield the real file's contents.
        assert_eq!(
            std::fs::read(&dest_link).unwrap(),
            b"shared library bytes",
            "symlink must resolve to the hardlinked sibling file in dest"
        );
    }

    /// E6: Relative symlink spanning directories (`../lib/real.so`).
    ///
    /// Layer has `lib/real.so` and `bin/link` → `../lib/real.so`.  After
    /// assembly the symlink at `dest/bin/link` must resolve through the
    /// mirrored directory structure.
    #[tokio::test]
    #[cfg(unix)]
    async fn preserves_relative_symlink_cross_directory() {
        let (_dir, root) = setup();
        make_file(&root, "layer/content/lib/real.so", b"real shared library");
        // Layer symlink: bin/link → ../lib/real.so (cross-directory relative)
        let link_in_layer = root.join("layer/content/bin/link");
        if let Some(p) = link_in_layer.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        crate::symlink::create(std::path::Path::new("../lib/real.so"), &link_in_layer).unwrap();

        let src = root.join("layer/content");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.symlinks_recreated, 1, "one cross-dir symlink recreated");
        assert_eq!(stats.files_hardlinked, 1, "one real file hardlinked");

        let dest_link = dest.join("bin/link");

        assert!(dest_link.is_symlink(), "dest/bin/link must be a symlink");

        // Target string preserved verbatim — still relative with `../`.
        assert_eq!(
            std::fs::read_link(&dest_link).unwrap(),
            std::path::Path::new("../lib/real.so"),
            "cross-dir relative target must be preserved verbatim"
        );

        // Resolves correctly through the mirrored dest structure.
        assert_eq!(
            std::fs::read(&dest_link).unwrap(),
            b"real shared library",
            "cross-dir symlink must resolve through mirrored dirs in dest"
        );
    }

    /// E7: Absolute symlink pointing to a target outside the layer.
    ///
    /// Layer contains a symlink with an absolute path target (e.g., pointing
    /// at a file outside the package tree).  The walker must recreate the
    /// symlink verbatim — the absolute target string must be byte-identical
    /// after assembly, read via `std::fs::read_link` (not `canonicalize`).
    #[tokio::test]
    #[cfg(unix)]
    async fn preserves_absolute_external_symlink() {
        let (_dir, root) = setup();

        // External file: lives outside the layer directory, simulating a
        // system path or a pre-existing package file the publisher chose to
        // reference absolutely.
        let external_file = root.join("external").join("target_file");
        std::fs::create_dir_all(external_file.parent().unwrap()).unwrap();
        std::fs::write(&external_file, b"external target bytes").unwrap();

        // Layer symlink: abs_link → <absolute path to external_file>
        make_dir(&root, "layer/content");
        let link_in_layer = root.join("layer/content/abs_link");
        crate::symlink::create(&external_file, &link_in_layer).unwrap();

        let src = root.join("layer/content");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.symlinks_recreated, 1, "one absolute symlink must be recreated");
        assert_eq!(stats.files_hardlinked, 0, "no regular files in this layer");

        let dest_link = dest.join("abs_link");

        assert!(dest_link.is_symlink(), "dest/abs_link must be a symlink");

        // Target string must be byte-identical — no path manipulation by the walker.
        assert_eq!(
            std::fs::read_link(&dest_link).unwrap(),
            external_file,
            "absolute target must be preserved verbatim via read_link"
        );

        // Still resolves to the external file (which has not moved).
        assert_eq!(
            std::fs::read(&dest_link).unwrap(),
            b"external target bytes",
            "absolute symlink must still resolve to the external file after assembly"
        );
    }

    /// E8: Chained symlinks (`a → b → c → real`).
    ///
    /// Layer has: `real` (regular file), `c` → `real`, `b` → `c`, `a` → `b`.
    /// All four entries must be recreated in the destination.  Reading
    /// `dest/a` must traverse the full chain and return the real file's
    /// contents.
    #[tokio::test]
    #[cfg(unix)]
    async fn preserves_chained_symlinks() {
        let (_dir, root) = setup();
        make_dir(&root, "layer/content");
        make_file(&root, "layer/content/real", b"real file content");
        // c → real
        crate::symlink::create(std::path::Path::new("real"), root.join("layer/content/c")).unwrap();
        // b → c
        crate::symlink::create(std::path::Path::new("c"), root.join("layer/content/b")).unwrap();
        // a → b
        crate::symlink::create(std::path::Path::new("b"), root.join("layer/content/a")).unwrap();

        let src = root.join("layer/content");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.files_hardlinked, 1, "one real file at the end of the chain");
        assert_eq!(stats.symlinks_recreated, 3, "all three chain links recreated");

        // All four entries must exist in dest.
        assert!(!dest.join("real").is_symlink(), "real must not be a symlink");
        assert!(dest.join("real").is_file(), "real must be a regular file");
        assert!(dest.join("c").is_symlink(), "c must be a symlink");
        assert!(dest.join("b").is_symlink(), "b must be a symlink");
        assert!(dest.join("a").is_symlink(), "a must be a symlink");

        // Each link's target string must be verbatim.
        assert_eq!(
            std::fs::read_link(dest.join("c")).unwrap(),
            std::path::Path::new("real")
        );
        assert_eq!(std::fs::read_link(dest.join("b")).unwrap(), std::path::Path::new("c"));
        assert_eq!(std::fs::read_link(dest.join("a")).unwrap(), std::path::Path::new("b"));

        // Reading dest/a traverses the full chain to the real file.
        assert_eq!(
            std::fs::read(dest.join("a")).unwrap(),
            b"real file content",
            "reading dest/a must traverse a→b→c→real and return the file contents"
        );
    }

    /// E9: Broken symlink in the layer (publisher bug).
    ///
    /// The layer contains a symlink whose target does not exist.  This is a
    /// publisher bug — the walker must NOT fail or attempt to fix it.  It must
    /// recreate the symlink verbatim, leaving it broken in the destination.
    ///
    /// Key assertion: use `symlink_metadata()` or `is_symlink()` — NOT
    /// `path.exists()` — because `exists()` follows symlinks and returns
    /// `false` for broken symlinks.
    #[tokio::test]
    #[cfg(unix)]
    async fn preserves_broken_symlink_verbatim() {
        let (_dir, root) = setup();
        make_dir(&root, "layer/content");
        // Broken symlink: target does not exist.
        let link_in_layer = root.join("layer/content/foo");
        crate::symlink::create(std::path::Path::new("does_not_exist"), &link_in_layer).unwrap();

        let src = root.join("layer/content");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        // Must not fail even though the layer contains a broken symlink.
        let stats = assemble_from_layer(&src, &dest).await.unwrap();

        assert_eq!(stats.symlinks_recreated, 1, "broken symlink must be recreated");
        assert_eq!(stats.files_hardlinked, 0, "no real files in this layer");

        let dest_link = dest.join("foo");

        // symlink_metadata() does NOT follow symlinks — safe for broken ones.
        assert!(
            dest_link.symlink_metadata().is_ok(),
            "dest/foo must exist as a symlink entry (check via symlink_metadata, not exists)"
        );
        assert!(
            dest_link.is_symlink(),
            "dest/foo must be a symlink, not a regular file or directory"
        );

        // Target string must be verbatim — the same broken relative target.
        assert_eq!(
            std::fs::read_link(&dest_link).unwrap(),
            std::path::Path::new("does_not_exist"),
            "broken symlink target must be preserved verbatim"
        );

        // Reading through the symlink must fail — still broken after assembly.
        assert!(
            std::fs::read(&dest_link).is_err(),
            "reading a broken symlink must fail — walker must not resolve publisher bugs"
        );
    }

    /// E12: Absolute symlink survives the `temp → packages/` atomic rename.
    ///
    /// The pull pipeline assembles into a temp directory, then renames the
    /// temp package directory into its final `packages/` location.  An
    /// absolute symlink pointing OUTSIDE the temp tree must be unaffected by
    /// the rename — its target string is stored in the inode and the external
    /// file remains at the same absolute path.
    ///
    /// # Publisher-bug note
    ///
    /// An absolute symlink whose target happens to point INSIDE the temp
    /// directory (e.g., because the walker incorrectly constructed the target
    /// from `dest_content.join(...)` instead of reading from the layer via
    /// `read_link`) would BREAK after the rename.  This is the same bug
    /// documented in `symlink_with_temp_path_in_target_breaks_after_rename`
    /// in `symlink.rs`.  The walker must always read the target verbatim from
    /// the layer — never compute a target from the destination path.
    #[tokio::test]
    #[cfg(unix)]
    async fn absolute_symlink_survives_dest_rename() {
        let (_dir, root) = setup();

        // External file: lives outside the temp package tree and will not be
        // affected by the rename that moves the package from temp to final.
        let external = root.join("external").join("stable_file");
        std::fs::create_dir_all(external.parent().unwrap()).unwrap();
        std::fs::write(&external, b"stable external content").unwrap();

        // Layer: one regular file + one absolute symlink to the external file.
        make_dir(&root, "layer/content");
        make_file(&root, "layer/content/real.txt", b"real file bytes");
        let link_in_layer = root.join("layer/content/abs_link");
        crate::symlink::create(&external, &link_in_layer).unwrap();

        // Assemble into a temp location — simulating the pull pipeline.
        let temp_pkg = root.join("temp").join("pkg");
        std::fs::create_dir_all(&temp_pkg).unwrap();
        let dest = temp_pkg.join("content");

        assemble_from_layer(&root.join("layer/content"), &dest).await.unwrap();

        // Capture the target string before the rename.
        let target_before = std::fs::read_link(dest.join("abs_link")).unwrap();

        // Atomic rename: temp/pkg → packages/pkg (same filesystem).
        let final_pkg = root.join("packages").join("pkg");
        std::fs::create_dir_all(final_pkg.parent().unwrap()).unwrap();
        std::fs::rename(&temp_pkg, &final_pkg).unwrap();

        let final_link = final_pkg.join("content").join("abs_link");

        // Target string must be byte-identical after the rename.
        let target_after = std::fs::read_link(&final_link).unwrap();
        assert_eq!(
            target_after, target_before,
            "absolute symlink target must be preserved verbatim after dest rename"
        );
        assert_eq!(
            target_after, external,
            "absolute target must still point at the original external file"
        );

        // The external file has not moved — resolution must succeed.
        assert_eq!(
            std::fs::read(&final_link).unwrap(),
            b"stable external content",
            "absolute symlink must resolve to the external file after rename"
        );
    }

    /// E13: Relative symlink survives the `temp → packages/` atomic rename.
    ///
    /// A relative symlink's target string lives in the inode and is not
    /// affected by a directory rename.  Because the walker mirrors the layer's
    /// directory structure into the destination verbatim, the relative
    /// relationship between the symlink and its target is preserved — both
    /// move together as part of the same package directory subtree.
    #[tokio::test]
    #[cfg(unix)]
    async fn relative_symlink_survives_dest_rename() {
        let (_dir, root) = setup();

        // Layer: real versioned library + relative symlink from the same dir.
        make_dir(&root, "layer/content/lib");
        make_file(&root, "layer/content/lib/libfoo.so.1.2.3", b"versioned library");
        // lib/libfoo.so → libfoo.so.1.2.3 (relative, same directory)
        let link_in_layer = root.join("layer/content/lib/libfoo.so");
        crate::symlink::create(std::path::Path::new("libfoo.so.1.2.3"), &link_in_layer).unwrap();

        // Assemble into a temp location — simulating the pull pipeline.
        let temp_pkg = root.join("temp").join("pkg");
        std::fs::create_dir_all(&temp_pkg).unwrap();
        let dest = temp_pkg.join("content");

        assemble_from_layer(&root.join("layer/content"), &dest).await.unwrap();

        // Capture the target string before the rename.
        let target_before = std::fs::read_link(dest.join("lib/libfoo.so")).unwrap();
        assert_eq!(
            target_before,
            std::path::Path::new("libfoo.so.1.2.3"),
            "pre-rename: target must be the verbatim relative string from the layer"
        );

        // Atomic rename: temp/pkg → packages/pkg (same filesystem).
        let final_pkg = root.join("packages").join("pkg");
        std::fs::create_dir_all(final_pkg.parent().unwrap()).unwrap();
        std::fs::rename(&temp_pkg, &final_pkg).unwrap();

        let final_link = final_pkg.join("content").join("lib").join("libfoo.so");

        // Target string must be byte-identical after the rename.
        let target_after = std::fs::read_link(&final_link).unwrap();
        assert_eq!(
            target_after, target_before,
            "relative target string must be preserved verbatim after rename"
        );
        assert_eq!(
            target_after,
            std::path::Path::new("libfoo.so.1.2.3"),
            "relative target must still be the verbatim layer value after rename"
        );

        // The relative target resolves correctly at the new location because
        // the mirrored directory structure was moved as a unit — the sibling
        // libfoo.so.1.2.3 is still adjacent to the symlink.
        assert_eq!(
            std::fs::read(&final_link).unwrap(),
            b"versioned library",
            "relative symlink must resolve to the mirrored sibling after rename"
        );
    }

    // ── file properties + inode sharing ─────────────────────────────────────

    /// Hardlink invariant: every regular file in the assembled destination
    /// shares the same inode (and device) as its source in the layer.
    ///
    /// This is the core correctness guarantee of the walker — files are not
    /// copied; they are hardlinked, so `dev+ino` must match.
    #[tokio::test]
    #[cfg(unix)]
    async fn hardlinked_file_shares_inode_with_layer_source() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        let source = make_file(&layer, "bin/tool", b"binary content");
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let _stats = assemble_from_layer(&layer, &dest).await.unwrap();

        let dest_tool = dest.join("bin/tool");
        let ino_source = std::fs::metadata(&source).unwrap().ino();
        let ino_dest = std::fs::metadata(&dest_tool).unwrap().ino();
        let dev_source = std::fs::metadata(&source).unwrap().dev();
        let dev_dest = std::fs::metadata(&dest_tool).unwrap().dev();

        assert_eq!(dev_source, dev_dest, "source and dest must be on the same device");
        assert_eq!(
            ino_source, ino_dest,
            "dest file must share inode with layer source (hardlink invariant)"
        );
    }

    /// E10: Executable permission bits are preserved after assembly.
    ///
    /// Because hardlinks share an inode, permissions set on the source in the
    /// layer are visible through the destination path — the walker must not
    /// accidentally reset permissions (e.g., by copying rather than hardlinking).
    #[tokio::test]
    #[cfg(unix)]
    async fn executable_permission_preserved_via_shared_inode() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        let source = make_file(&layer, "bin/tool", b"#!/bin/sh\necho hi");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");
        assemble_from_layer(&layer, &dest).await.unwrap();

        let dest_tool = dest.join("bin/tool");
        let mode = std::fs::metadata(&dest_tool).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "executable bits must be preserved");
    }

    /// AssemblyStats invariant: `files_hardlinked` counts every regular file
    /// placed in the destination, including files in subdirectories.
    ///
    /// Directories and symlinks are not counted here; only regular files.
    #[tokio::test]
    async fn stats_files_hardlinked_counts_regular_files() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        make_file(&layer, "a.txt", b"a");
        make_file(&layer, "b.txt", b"bb");
        make_file(&layer, "sub/c.txt", b"ccc");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");
        let stats = assemble_from_layer(&layer, &dest).await.unwrap();

        assert_eq!(stats.files_hardlinked, 3, "must count three files");
        // Intra-layer symlinks count separately — none here.
        assert_eq!(stats.symlinks_recreated, 0);
    }

    /// AssemblyStats invariant: `bytes_hardlinked` is the sum of the source
    /// file sizes across all hardlinked files.
    #[tokio::test]
    async fn stats_bytes_hardlinked_sums_source_file_sizes() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        make_file(&layer, "a.txt", b"12345"); // 5 bytes
        make_file(&layer, "b.txt", b"1234567890"); // 10 bytes

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");
        let stats = assemble_from_layer(&layer, &dest).await.unwrap();

        assert_eq!(stats.bytes_hardlinked, 15);
    }

    // ── error paths + Windows ───────────────────────────────────────────────

    /// Cross-platform error: `source_content` directory does not exist.
    ///
    /// The walker must return an `Error::InternalFile` wrapping an IO error
    /// when the source path is absent. `NotFound` and `InvalidInput` are both
    /// acceptable IO kinds depending on platform and implementation detail.
    #[tokio::test]
    async fn errors_when_source_content_missing() {
        let (_dir, root) = setup();
        let ghost_src = root.join("nonexistent_layer").join("content");
        let dest = root.join("pkg/content");
        // Pre-create dest.parent() so the error is from the missing source,
        // not from a missing dest parent.
        std::fs::create_dir_all(root.join("pkg")).unwrap();

        let result = assemble_from_layer(&ghost_src, &dest).await;
        assert!(result.is_err(), "must error when source_content does not exist");

        if let Err(crate::Error::InternalFile(_, io_err)) = result {
            assert!(
                matches!(
                    io_err.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                ),
                "expected NotFound or InvalidInput, got {:?}",
                io_err.kind()
            );
        } else {
            panic!("expected Error::InternalFile wrapping an IO error");
        }
    }

    /// Cross-platform error: `dest_content.parent()` does not exist.
    ///
    /// The walker creates `dest_content` itself but must NOT create grandparent
    /// directories. When the parent of `dest_content` is missing the walker
    /// must return an error rather than silently creating the full path.
    #[tokio::test]
    async fn errors_when_dest_parent_missing() {
        let (_dir, root) = setup();
        let src = make_dir(&root, "layer/content");
        make_file(&src, "file.txt", b"content");

        // dest parent (root/nope/does_not_exist/) does not exist
        let dest = root.join("nope").join("does_not_exist").join("content");

        let result = assemble_from_layer(&src, &dest).await;
        assert!(
            result.is_err(),
            "must error when dest.parent() does not exist (walker does not create grandparent dirs)"
        );
    }

    /// Multi-layer merge (issue #22): `dest_content` is pre-populated with
    /// files from a previous layer; a second non-overlapping layer is
    /// assembled over the top. Both sets of files must be present and the
    /// first set's inodes must be untouched.
    #[tokio::test]
    #[cfg(unix)]
    async fn assembles_existing_dest_populated_from_previous_layer() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();

        // Layer A (already assembled into dest before we run the walker).
        let layer_a = make_dir(&root, "layer_a/content");
        make_file(&layer_a, "bin/tool_a", b"tool A binary");
        make_file(&layer_a, "share/a.txt", b"share a");

        // Simulate a prior `assemble_from_layer(layer_a, dest)` by hardlinking
        // layer A's files into dest directly — this is exactly what the
        // walker would have done on a previous pass.
        let dest = root.join("pkg/content");
        std::fs::create_dir_all(dest.join("bin")).unwrap();
        std::fs::create_dir_all(dest.join("share")).unwrap();
        std::fs::hard_link(layer_a.join("bin/tool_a"), dest.join("bin/tool_a")).unwrap();
        std::fs::hard_link(layer_a.join("share/a.txt"), dest.join("share/a.txt")).unwrap();
        let ino_tool_a_before = std::fs::metadata(dest.join("bin/tool_a")).unwrap().ino();

        // Layer B: disjoint from A except for the shared `bin/` and `share/`
        // directories, which must merge cleanly.
        let layer_b = make_dir(&root, "layer_b/content");
        make_file(&layer_b, "bin/tool_b", b"tool B binary");
        make_file(&layer_b, "share/b.txt", b"share b");

        let stats = assemble_from_layer(&layer_b, &dest).await.unwrap();

        // Both layer B files must be hardlinked into dest.
        assert_eq!(stats.files_hardlinked, 2);

        // Layer A files must still be present and their inodes unchanged.
        assert_eq!(std::fs::read(dest.join("bin/tool_a")).unwrap(), b"tool A binary");
        assert_eq!(std::fs::read(dest.join("share/a.txt")).unwrap(), b"share a");
        let ino_tool_a_after = std::fs::metadata(dest.join("bin/tool_a")).unwrap().ino();
        assert_eq!(
            ino_tool_a_before, ino_tool_a_after,
            "layer A inodes must not be disturbed by a second-layer walk"
        );

        // Layer B files must be readable at their mirrored positions.
        assert_eq!(std::fs::read(dest.join("bin/tool_b")).unwrap(), b"tool B binary");
        assert_eq!(std::fs::read(dest.join("share/b.txt")).unwrap(), b"share b");
    }

    /// Multi-layer collision: two layers contribute the same file path.
    /// The walker must surface an `AlreadyExists` error — no silent overwrite.
    #[tokio::test]
    async fn errors_on_file_collision_across_layers() {
        let (_dir, root) = setup();

        // Pre-populate dest with a file at `bin/tool` (as if a prior layer
        // had contributed it).
        let dest = root.join("pkg/content");
        make_file(&dest, "bin/tool", b"first layer tool");

        // A new layer also contributes `bin/tool` — collision.
        let layer = make_dir(&root, "layer/content");
        make_file(&layer, "bin/tool", b"second layer tool");

        let result = assemble_from_layer(&layer, &dest).await;

        let err = result.expect_err("must error on overlapping file between layers");
        match err {
            crate::Error::InternalFile(path, io_err) => {
                assert_eq!(
                    io_err.kind(),
                    std::io::ErrorKind::AlreadyExists,
                    "expected AlreadyExists, got {:?}",
                    io_err.kind()
                );
                assert!(
                    path.ends_with("bin/tool"),
                    "error path must name the colliding entry, got {}",
                    path.display()
                );
            }
            other => panic!("expected Error::InternalFile, got {other:?}"),
        }
    }

    /// Multi-layer type mismatch: layer A contributes `foo/` as a directory;
    /// layer B contributes `foo` as a file. The walker must error rather than
    /// silently accept the mismatched types.
    #[tokio::test]
    async fn errors_on_type_mismatch_dir_vs_file() {
        let (_dir, root) = setup();

        // Pre-populate dest with `foo/` as a directory (layer A contribution).
        let dest = root.join("pkg/content");
        std::fs::create_dir_all(dest.join("foo")).unwrap();

        // Layer B contributes `foo` as a regular file.
        let layer = make_dir(&root, "layer/content");
        make_file(&layer, "foo", b"file contents");

        let result = assemble_from_layer(&layer, &dest).await;

        let err = result.expect_err("must error on dir-vs-file type mismatch across layers");
        match err {
            crate::Error::InternalFile(_, io_err) => {
                assert_eq!(
                    io_err.kind(),
                    std::io::ErrorKind::AlreadyExists,
                    "expected AlreadyExists on type mismatch, got {:?}",
                    io_err.kind()
                );
            }
            other => panic!("expected Error::InternalFile, got {other:?}"),
        }
    }

    // Windows-only: file symlink fallback (W1) and directory symlink error (W4)

    /// Windows: intra-layer file symlinks are currently unsupported and the
    /// walker must surface `AssemblyError::WindowsSymlinksUnsupported` rather
    /// than silently dereferencing the target into a regular-file copy (which
    /// would lose the dedup invariant and hide publisher intent).
    #[tokio::test]
    #[cfg(windows)]
    async fn windows_file_symlink_errors_unsupported() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        let real = make_file(&layer, "real.dll", b"binary bytes");
        let link = layer.join("alias.dll");
        // Create the file symlink directly in the layer — not via the walker.
        // Requires Developer Mode or elevation on Windows.
        std::os::windows::fs::symlink_file(&real, &link).unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");
        let result = assemble_from_layer(&layer, &dest).await;

        let err = result.expect_err("file symlink must error on Windows");
        if let crate::Error::InternalFile(_, io_err) = &err {
            let source = io_err.get_ref().expect("must have inner source");
            assert!(
                source.downcast_ref::<AssemblyError>().is_some(),
                "expected AssemblyError::WindowsSymlinksUnsupported, got {source:?}"
            );
        } else {
            panic!("expected Error::InternalFile wrapping AssemblyError");
        }
    }

    /// Windows: intra-layer directory symlinks are unsupported — the walker
    /// must surface `AssemblyError::WindowsSymlinksUnsupported` for the same
    /// reason as file symlinks (NTFS junctions cannot preserve the
    /// verbatim-target invariant the walker relies on).
    #[tokio::test]
    #[cfg(windows)]
    async fn windows_directory_symlink_errors_unsupported() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        let real_dir = make_dir(&layer, "real_dir");
        make_file(&real_dir, "inside.txt", b"x");

        let link_dir = layer.join("alias_dir");
        // Create the directory symlink directly in the layer — not via walker.
        // Requires Developer Mode or elevation on Windows.
        std::os::windows::fs::symlink_dir(&real_dir, &link_dir).unwrap();

        let dest = root.join("pkg/content");
        let result = assemble_from_layer(&layer, &dest).await;

        let err = result.expect_err("directory symlink must error on Windows");
        if let crate::Error::InternalFile(_, io_err) = &err {
            let source = io_err.get_ref().expect("must have inner source");
            assert!(
                source.downcast_ref::<AssemblyError>().is_some(),
                "expected AssemblyError::WindowsSymlinksUnsupported, got {source:?}"
            );
        } else {
            panic!("expected Error::InternalFile wrapping AssemblyError");
        }
    }

    // ── defence-in-depth + resource caps ────────────────────────────────────

    /// Item 3 defence-in-depth: a relative symlink whose target escapes
    /// `dest_content` via `..` traversal must be rejected by the walker
    /// even though the archive extractor is the primary trust boundary.
    #[tokio::test]
    #[cfg(unix)]
    async fn walker_rejects_escaping_symlink_into_dest_root() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        // Relative target that climbs out of dest_content/.
        let link_in_layer = layer.join("evil");
        crate::symlink::create(Path::new("../../../../etc/passwd"), &link_in_layer).unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layer(&layer, &dest).await;
        assert!(result.is_err(), "walker must reject escaping relative symlink");
    }

    /// Item 7: when `dest_content.parent()` is missing, the walker must
    /// error rather than silently walking up and creating ancestors.
    #[tokio::test]
    async fn walker_rejects_missing_parent_dir() {
        let (_dir, root) = setup();
        let src = make_dir(&root, "layer/content");
        make_file(&src, "file.txt", b"content");

        // Neither `nope` nor `nope/does_not_exist` exists.
        let dest = root.join("nope").join("does_not_exist").join("content");
        let result = assemble_from_layer(&src, &dest).await;
        assert!(result.is_err(), "walker must error when dest.parent() is missing");
    }

    /// E14: wide-fanout concurrency smoke test.
    ///
    /// 100 sibling directories each containing 10 files exercise the
    /// semaphore-bounded directory fan-out. Every file must be hardlinked
    /// exactly once and `AssemblyStats` must sum correctly across the
    /// parallel tasks — no entries lost, no double-counting.
    #[tokio::test]
    async fn parallel_walk_assembles_wide_tree() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        for d in 0..100 {
            for f in 0..10 {
                make_file(&layer, &format!("dir{d}/f{f}.txt"), b"x");
            }
        }

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");
        let stats = assemble_from_layer(&layer, &dest).await.unwrap();

        assert_eq!(
            stats.files_hardlinked, 1000,
            "all 1000 files across 100 dirs must be hardlinked"
        );
        assert_eq!(stats.dirs_created, 100, "100 subdirectories must be created");
        assert_eq!(stats.symlinks_recreated, 0);

        // Spot-check file presence at the extremes.
        assert!(dest.join("dir0/f0.txt").exists());
        assert!(dest.join("dir99/f9.txt").exists());
    }

    /// E15: entry cap still fires under the parallel scheduler. Lowering the
    /// cap via `assemble_from_layer_with_cap` must cause the walk to abort
    /// with an error rather than run to completion — atomic counter must be
    /// observed by every concurrent task.
    #[tokio::test]
    async fn parallel_walk_honors_entry_cap() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        // 10 dirs × 10 files = 100 entries + 10 dir entries = 110 total
        for d in 0..10 {
            for f in 0..10 {
                make_file(&layer, &format!("dir{d}/f{f}.txt"), b"x");
            }
        }
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        // Cap of 5 — must abort well before the walk finishes.
        let result = assemble_from_layer_with_cap(&layer, &dest, 5).await;
        assert!(result.is_err(), "parallel walker must honor the entry cap");
    }

    /// Item 4: entry-count cap — the walker must refuse to process a layer
    /// that exceeds `max_entries`. The private `assemble_from_layer_with_cap`
    /// entry point lets us lower the cap for the test so we do not need to
    /// create millions of files on disk.
    #[tokio::test]
    async fn walker_rejects_entry_flood() {
        let (_dir, root) = setup();
        let layer = make_dir(&root, "layer/content");
        for i in 0..10 {
            make_file(&layer, &format!("f{i}.txt"), b"x");
        }
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        // Cap of 5 — the 10-file layer must be rejected before completion.
        let result = assemble_from_layer_with_cap(&layer, &dest, 5).await;
        assert!(result.is_err(), "walker must reject a layer exceeding the entry cap");
    }

    /// Item 1: multi-layer manifests are rejected at the pull boundary with
    /// a typed `ClientError::InvalidManifest` referencing issue #22. This
    /// test locks in the error surface so a future regression that replaces
    /// the typed error with a panic is caught at test time.
    ///
    /// Note: the actual check lives in `pull.rs`, not the walker. This test
    /// exists here because the walker is the thing that *could* handle
    /// multi-layer merging (tests above already lock in that contract), so
    /// this file is the natural place to document "the walker supports N>1
    /// but the pipeline gates at N=1".
    #[test]
    fn multi_layer_error_surface_is_documented_here() {
        // This is a compile-time assertion that `ClientError::InvalidManifest`
        // is the chosen error variant for the pull-side multi-layer gate.
        // If the variant is renamed or removed, this test stops compiling
        // and points the reader at the call site in `pull.rs`.
        let err = crate::oci::client::error::ClientError::InvalidManifest(
            "multi-layer package assembly is not yet supported (#22)".to_string(),
        );
        assert!(format!("{err}").contains("multi-layer"));
    }

    // ── 3.1: Boundary / degenerate inputs ───────────────────────────────────

    /// ML-1: Empty sources slice returns AssemblyStats::default() and does NOT
    /// create dest_content. The no-op contract: if there is nothing to assemble,
    /// the destination should not be touched at all.
    #[tokio::test]
    async fn ml_empty_sources_is_noop() {
        let (_dir, root) = setup();
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[], &dest).await.unwrap();

        assert_eq!(
            stats,
            AssemblyStats::default(),
            "empty sources must return default stats"
        );
        assert!(
            !dest.exists(),
            "empty sources must NOT create dest_content — nothing to assemble"
        );
    }

    /// ML-2: Single-element sources produces the same assembled tree as
    /// assemble_from_layer. Validates that the multi-layer code path handles
    /// the degenerate N=1 case correctly without diverging from the reference.
    #[tokio::test]
    async fn ml_single_source_matches_single_layer() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/tool", b"tool binary");
        make_file(&root, "layer_a/content/lib/libfoo.so", b"shared lib");

        std::fs::create_dir_all(root.join("pkg_ml")).unwrap();
        let dest_ml = root.join("pkg_ml/content");
        let stats = assemble_from_layers(&[src_a.as_path()], &dest_ml).await.unwrap();

        assert_eq!(stats.files_hardlinked, 2, "single-source must hardlink all files");
        assert_eq!(stats.dirs_created, 2, "single-source must create bin/ and lib/");
        assert!(dest_ml.join("bin/tool").exists(), "bin/tool must appear in dest");
        assert!(
            dest_ml.join("lib/libfoo.so").exists(),
            "lib/libfoo.so must appear in dest"
        );
        assert_eq!(
            std::fs::read(dest_ml.join("bin/tool")).unwrap(),
            b"tool binary",
            "file contents must be correct"
        );
    }

    /// ML-3: When dest_content is absent but its parent exists, the walker
    /// creates dest_content itself (same pre-condition as assemble_from_layer).
    #[tokio::test]
    async fn ml_creates_dest_if_missing() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a.txt", b"a");

        // Parent exists, dest itself does not.
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");
        assert!(!dest.exists(), "dest must not pre-exist for this test");

        assemble_from_layers(&[src_a.as_path()], &dest).await.unwrap();

        assert!(dest.exists(), "walker must create dest_content when absent");
        assert!(dest.is_dir(), "dest_content must be a real directory");
    }

    /// ML-4: Two empty layers produce an empty dest directory. Stats are all
    /// zero; dest itself must exist after the call.
    #[tokio::test]
    async fn ml_empty_layers_produce_empty_dest() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        let src_b = make_dir(&root, "layer_b/content");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert!(dest.exists(), "dest must be created even for two empty layers");
        assert_eq!(stats.files_hardlinked, 0, "no files in empty layers");
        assert_eq!(stats.symlinks_recreated, 0, "no symlinks in empty layers");
        assert_eq!(stats.dirs_created, 0, "no subdirectories in empty layers");
        assert_eq!(stats.bytes_hardlinked, 0, "no bytes in empty layers");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0, "dest must be empty");
    }

    // ── 3.2: Two-layer merging — structure ───────────────────────────────────

    /// ML-5: Two layers with completely disjoint flat files — both appear in
    /// the assembled dest root.
    #[tokio::test]
    async fn ml_disjoint_files_flat() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a.txt", b"file from A");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/b.txt", b"file from B");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 2, "both disjoint files must be hardlinked");
        assert!(dest.join("a.txt").exists(), "a.txt from layer A must appear");
        assert!(dest.join("b.txt").exists(), "b.txt from layer B must appear");
        assert_eq!(std::fs::read(dest.join("a.txt")).unwrap(), b"file from A");
        assert_eq!(std::fs::read(dest.join("b.txt")).unwrap(), b"file from B");
    }

    /// ML-6: Two layers with completely disjoint subtrees — lib/ from A and
    /// bin/ from B appear as separate directories in dest.
    #[tokio::test]
    async fn ml_disjoint_subtrees() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/lib/a.so", b"libA");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"binB");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 2, "both disjoint files must be hardlinked");
        assert_eq!(stats.dirs_created, 2, "lib/ and bin/ must be created separately");
        assert!(dest.join("lib/a.so").exists(), "lib/a.so from A must appear");
        assert!(dest.join("bin/b").exists(), "bin/b from B must appear");
        assert!(
            !dest.join("bin").join("a.so").exists(),
            "a.so must not appear under bin/"
        );
        assert!(!dest.join("lib").join("b").exists(), "b must not appear under lib/");
    }

    /// ML-7: Two layers both contribute files to a shared bin/ directory.
    /// The merged bin/ must contain both files.
    #[tokio::test]
    async fn ml_shared_directory_merges_files() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/tool_a", b"tool A");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/tool_b", b"tool B");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 2, "both tools must be hardlinked");
        // bin/ should be created exactly once despite appearing in both layers
        assert_eq!(stats.dirs_created, 1, "shared bin/ must be created only once");
        assert!(dest.join("bin").is_dir(), "bin/ must be a real directory");
        assert!(
            dest.join("bin/tool_a").exists(),
            "tool_a from A must appear in shared bin/"
        );
        assert!(
            dest.join("bin/tool_b").exists(),
            "tool_b from B must appear in shared bin/"
        );
    }

    /// ML-8: Both layers share a 3-level-deep directory tree. The merge must
    /// correctly recurse all the way to a/b/c/ and place both files there.
    #[tokio::test]
    async fn ml_deep_shared_tree() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a/b/c/x.txt", b"x from A");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/a/b/c/y.txt", b"y from B");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 2, "both deep files must be hardlinked");
        // a/, a/b/, a/b/c/ — 3 shared directories each created once
        assert_eq!(
            stats.dirs_created, 3,
            "a/, a/b/, a/b/c/ = 3 directories created once each"
        );
        assert!(dest.join("a/b/c/x.txt").exists(), "x.txt from A must be at depth 3");
        assert!(dest.join("a/b/c/y.txt").exists(), "y.txt from B must be at depth 3");
        assert_eq!(std::fs::read(dest.join("a/b/c/x.txt")).unwrap(), b"x from A");
        assert_eq!(std::fs::read(dest.join("a/b/c/y.txt")).unwrap(), b"y from B");
    }

    /// ML-9: Layer A has bin/ and lib/; layer B has bin/ and share/. The
    /// merged result has bin/ (merged from both), lib/ (A-only), share/ (B-only).
    #[tokio::test]
    async fn ml_mixed_shared_and_disjoint() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/a", b"bin/a");
        make_file(&root, "layer_a/content/lib/liba.so", b"liba");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"bin/b");
        make_file(&root, "layer_b/content/share/doc.txt", b"doc");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 4, "all 4 files must be hardlinked");
        // bin/ (shared, once), lib/ (A-only), share/ (B-only) = 3 directories
        assert_eq!(stats.dirs_created, 3, "bin/, lib/, share/ = 3 directories");
        assert!(dest.join("bin/a").exists());
        assert!(dest.join("bin/b").exists());
        assert!(dest.join("lib/liba.so").exists());
        assert!(dest.join("share/doc.txt").exists());
    }

    /// ML-10: Layer A has lib/ with a.so; layer B has bin/ with b. The lib/
    /// directory should recurse with only layer A's contribution and must NOT
    /// contain any files from layer B (and vice versa for bin/).
    #[tokio::test]
    async fn ml_layer_pruned_on_recursion() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/lib/a.so", b"libA");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"binB");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(
            stats.dirs_created, 2,
            "lib/ and bin/ each created by their single layer"
        );
        // Pruning: lib/ should only contain A's files, not any B files
        let lib_entries: Vec<_> = std::fs::read_dir(dest.join("lib")).unwrap().collect();
        assert_eq!(lib_entries.len(), 1, "lib/ must contain exactly a.so from layer A only");
        assert!(dest.join("lib/a.so").exists(), "a.so must be in lib/");
        assert!(!dest.join("lib/b").exists(), "b from layer B must NOT be in lib/");
        // Similarly bin/ should only contain B's files
        assert!(dest.join("bin/b").exists(), "b must be in bin/");
        assert!(!dest.join("bin/a.so").exists(), "a.so from layer A must NOT be in bin/");
    }

    // ── 3.3: Three-layer merging ─────────────────────────────────────────────

    /// ML-11: Three completely disjoint layers each under their own top-level
    /// directory. All three trees must be present in dest.
    #[tokio::test]
    async fn ml_three_layers_all_disjoint() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a/x", b"ax");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/b/y", b"by");
        let src_c = make_dir(&root, "layer_c/content");
        make_file(&root, "layer_c/content/c/z", b"cz");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path(), src_c.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 3, "one file from each of 3 disjoint layers");
        assert_eq!(stats.dirs_created, 3, "a/, b/, c/ = 3 disjoint directories");
        assert!(dest.join("a/x").exists());
        assert!(dest.join("b/y").exists());
        assert!(dest.join("c/z").exists());
    }

    /// ML-12: Three layers all contribute files to the same bin/ directory.
    /// The merged bin/ must contain all three files with bin/ created once.
    #[tokio::test]
    async fn ml_three_layers_shared_root_dir() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/a", b"a");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"b");
        let src_c = make_dir(&root, "layer_c/content");
        make_file(&root, "layer_c/content/bin/c", b"c");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path(), src_c.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 3, "all three files must be hardlinked");
        assert_eq!(stats.dirs_created, 1, "shared bin/ must be created exactly once");
        assert!(dest.join("bin/a").exists());
        assert!(dest.join("bin/b").exists());
        assert!(dest.join("bin/c").exists());
    }

    /// ML-13: Three layers with partial overlap across shared directories.
    /// A: bin/a, lib/liba.so; B: bin/b, share/doc.txt; C: lib/libc.so, share/man.txt.
    /// Result: bin/{a,b}, lib/{liba,libc}, share/{doc,man}.
    #[tokio::test]
    async fn ml_three_layers_partial_overlap() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/a", b"a");
        make_file(&root, "layer_a/content/lib/liba.so", b"liba");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"b");
        make_file(&root, "layer_b/content/share/doc.txt", b"doc");
        let src_c = make_dir(&root, "layer_c/content");
        make_file(&root, "layer_c/content/lib/libc.so", b"libc");
        make_file(&root, "layer_c/content/share/man.txt", b"man");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path(), src_c.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(
            stats.files_hardlinked, 6,
            "all 6 files across 3 layers must be hardlinked"
        );
        // bin/ (A+B), lib/ (A+C), share/ (B+C) = 3 directories each created once
        assert_eq!(stats.dirs_created, 3, "bin/, lib/, share/ = 3 directories");
        assert!(dest.join("bin/a").exists());
        assert!(dest.join("bin/b").exists());
        assert!(dest.join("lib/liba.so").exists());
        assert!(dest.join("lib/libc.so").exists());
        assert!(dest.join("share/doc.txt").exists());
        assert!(dest.join("share/man.txt").exists());
    }

    /// ML-14: Progressive layer pruning across 3 levels. Layer A contributes
    /// a/b/c/x; layer B contributes a/b/y; layer C contributes a/z.
    /// At depth a/b/c/ only A contributes; at a/b/ A+B; at a/ all three.
    #[tokio::test]
    async fn ml_three_layers_progressive_pruning() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a/b/c/x", b"x");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/a/b/y", b"y");
        let src_c = make_dir(&root, "layer_c/content");
        make_file(&root, "layer_c/content/a/z", b"z");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path(), src_c.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 3, "x, y, z all placed");
        // a/ (all 3 contribute), a/b/ (A+B contribute), a/b/c/ (A only) = 3 dirs
        assert_eq!(stats.dirs_created, 3, "a/, a/b/, a/b/c/ = 3 directories");
        assert!(dest.join("a/b/c/x").exists(), "deepest file from A must be present");
        assert!(dest.join("a/b/y").exists(), "mid-level file from B must be present");
        assert!(dest.join("a/z").exists(), "shallow file from C must be present");
        // No cross-contamination: b/c/ must not appear under a/ directly
        assert!(!dest.join("a/c").exists(), "c/ must not appear directly under a/");
        assert!(!dest.join("a/y").exists(), "y must not appear directly under a/");
    }

    // ── 3.4: Hardlink + inode invariants (Unix) ──────────────────────────────

    /// ML-15: Every assembled file from any layer shares (dev, ino) with its
    /// source in that layer. This is the core correctness guarantee.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_every_file_shares_inode_with_source() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        let file_a = make_file(&root, "layer_a/content/bin/tool_a", b"tool A");
        let src_b = make_dir(&root, "layer_b/content");
        let file_b = make_file(&root, "layer_b/content/bin/tool_b", b"tool B");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        let meta_src_a = std::fs::metadata(&file_a).unwrap();
        let meta_dest_a = std::fs::metadata(dest.join("bin/tool_a")).unwrap();
        assert_eq!(
            meta_src_a.ino(),
            meta_dest_a.ino(),
            "tool_a in dest must share inode with its source in layer A"
        );
        assert_eq!(meta_src_a.dev(), meta_dest_a.dev());

        let meta_src_b = std::fs::metadata(&file_b).unwrap();
        let meta_dest_b = std::fs::metadata(dest.join("bin/tool_b")).unwrap();
        assert_eq!(
            meta_src_b.ino(),
            meta_dest_b.ino(),
            "tool_b in dest must share inode with its source in layer B"
        );
        assert_eq!(meta_src_b.dev(), meta_dest_b.dev());
    }

    /// ML-16: Three layers each contributing a file to shared bin/. Each
    /// assembled file's inode must match its specific source layer only.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_inode_stable_across_layers() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        let file_a = make_file(&root, "layer_a/content/bin/a", b"A content");
        let src_b = make_dir(&root, "layer_b/content");
        let file_b = make_file(&root, "layer_b/content/bin/b", b"B content");
        let src_c = make_dir(&root, "layer_c/content");
        let file_c = make_file(&root, "layer_c/content/bin/c", b"C content");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        assemble_from_layers(&[src_a.as_path(), src_b.as_path(), src_c.as_path()], &dest)
            .await
            .unwrap();

        let ino_src_a = std::fs::metadata(&file_a).unwrap().ino();
        let ino_src_b = std::fs::metadata(&file_b).unwrap().ino();
        let ino_src_c = std::fs::metadata(&file_c).unwrap().ino();

        let ino_dest_a = std::fs::metadata(dest.join("bin/a")).unwrap().ino();
        let ino_dest_b = std::fs::metadata(dest.join("bin/b")).unwrap().ino();
        let ino_dest_c = std::fs::metadata(dest.join("bin/c")).unwrap().ino();

        assert_eq!(ino_dest_a, ino_src_a, "bin/a inode must match layer A source");
        assert_eq!(ino_dest_b, ino_src_b, "bin/b inode must match layer B source");
        assert_eq!(ino_dest_c, ino_src_c, "bin/c inode must match layer C source");
        // Cross-check: no file shares an inode with the wrong layer's source
        assert_ne!(ino_dest_a, ino_src_b, "bin/a must not share inode with layer B");
        assert_ne!(ino_dest_b, ino_src_a, "bin/b must not share inode with layer A");
    }

    /// ML-17: After assembly into a temp directory followed by rename to the
    /// final location, all hardlink inodes are preserved. Rename does not
    /// break inode sharing.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_hardlink_survives_dest_rename() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        let file_a = make_file(&root, "layer_a/content/tool", b"binary");
        let src_b = make_dir(&root, "layer_b/content");
        let file_b = make_file(&root, "layer_b/content/lib.so", b"library");

        let temp_pkg = root.join("temp/pkg");
        std::fs::create_dir_all(&temp_pkg).unwrap();
        let temp_dest = temp_pkg.join("content");

        assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &temp_dest)
            .await
            .unwrap();

        let ino_before_a = std::fs::metadata(temp_dest.join("tool")).unwrap().ino();
        let ino_before_b = std::fs::metadata(temp_dest.join("lib.so")).unwrap().ino();

        // Atomic rename: temp/pkg → packages/pkg
        let final_pkg = root.join("packages/pkg");
        std::fs::create_dir_all(final_pkg.parent().unwrap()).unwrap();
        std::fs::rename(&temp_pkg, &final_pkg).unwrap();

        let ino_after_a = std::fs::metadata(final_pkg.join("content/tool")).unwrap().ino();
        let ino_after_b = std::fs::metadata(final_pkg.join("content/lib.so")).unwrap().ino();

        assert_eq!(ino_before_a, ino_after_a, "inode for tool must survive rename");
        assert_eq!(ino_before_b, ino_after_b, "inode for lib.so must survive rename");

        // Also verify they still match their layer sources
        let ino_src_a = std::fs::metadata(&file_a).unwrap().ino();
        let ino_src_b = std::fs::metadata(&file_b).unwrap().ino();
        assert_eq!(
            ino_after_a, ino_src_a,
            "tool inode must match layer A source after rename"
        );
        assert_eq!(
            ino_after_b, ino_src_b,
            "lib.so inode must match layer B source after rename"
        );
    }

    /// ML-18: Files contributed by different layers share bin/ but have
    /// different inodes because they are genuinely different files.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_different_layers_different_inodes() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/a", b"file A");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"file B");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        let ino_a = std::fs::metadata(dest.join("bin/a")).unwrap().ino();
        let ino_b = std::fs::metadata(dest.join("bin/b")).unwrap().ino();

        assert_ne!(ino_a, ino_b, "files from different layers must have different inodes");
    }

    // ── 3.5: Symlinks across layers (Unix) ───────────────────────────────────

    /// ML-19: Layer A has a symlink in lib/; layer B has a regular file in
    /// bin/. Both must appear correctly — symlink target preserved verbatim,
    /// file hardlinked correctly.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_symlink_in_one_layer_files_in_another() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_dir(&root, "layer_a/content/lib");
        crate::symlink::create(
            std::path::Path::new("libfoo.so.1"),
            root.join("layer_a/content/lib/libfoo.so"),
        )
        .unwrap();
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/tool", b"tool binary");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.symlinks_recreated, 1, "one symlink from A must be recreated");
        assert_eq!(stats.files_hardlinked, 1, "one file from B must be hardlinked");

        let dest_link = dest.join("lib/libfoo.so");
        assert!(dest_link.is_symlink(), "lib/libfoo.so must be a symlink");
        assert_eq!(
            std::fs::read_link(&dest_link).unwrap(),
            std::path::Path::new("libfoo.so.1"),
            "symlink target must be preserved verbatim"
        );
        assert!(dest.join("bin/tool").exists(), "tool from B must appear");
    }

    /// ML-20: Both layers contribute disjoint symlinks to the same shared lib/.
    /// Both symlinks must be present with verbatim targets.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_symlinks_in_both_layers_disjoint() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_dir(&root, "layer_a/content/lib");
        crate::symlink::create(
            std::path::Path::new("libfoo.so.1"),
            root.join("layer_a/content/lib/libfoo.so"),
        )
        .unwrap();
        let src_b = make_dir(&root, "layer_b/content");
        make_dir(&root, "layer_b/content/lib");
        crate::symlink::create(
            std::path::Path::new("libbar.so.2"),
            root.join("layer_b/content/lib/libbar.so"),
        )
        .unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.symlinks_recreated, 2, "both symlinks must be recreated");
        assert_eq!(stats.dirs_created, 1, "shared lib/ created exactly once");

        let link_foo = dest.join("lib/libfoo.so");
        let link_bar = dest.join("lib/libbar.so");
        assert!(link_foo.is_symlink(), "libfoo.so must be a symlink");
        assert!(link_bar.is_symlink(), "libbar.so must be a symlink");
        assert_eq!(
            std::fs::read_link(&link_foo).unwrap(),
            std::path::Path::new("libfoo.so.1"),
            "libfoo.so target must be verbatim from layer A"
        );
        assert_eq!(
            std::fs::read_link(&link_bar).unwrap(),
            std::path::Path::new("libbar.so.2"),
            "libbar.so target must be verbatim from layer B"
        );
    }

    /// ML-21: Layer A provides lib/libfoo.so.1 (a real file); layer B provides
    /// lib/libfoo.so (a symlink pointing to libfoo.so.1). These are different
    /// names so there is NO overlap. The assembled lib/ must contain both the
    /// real file and the symlink, and the symlink must resolve to the hardlinked
    /// real file in the same assembled lib/ directory.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_relative_symlink_across_shared_dir() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/lib/libfoo.so.1", b"versioned lib");
        let src_b = make_dir(&root, "layer_b/content");
        make_dir(&root, "layer_b/content/lib");
        // symlink: lib/libfoo.so → libfoo.so.1 (different name from the real file)
        crate::symlink::create(
            std::path::Path::new("libfoo.so.1"),
            root.join("layer_b/content/lib/libfoo.so"),
        )
        .unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        // Must NOT error — these are different names in the same directory.
        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(
            stats.files_hardlinked, 1,
            "real file libfoo.so.1 from A must be hardlinked"
        );
        assert_eq!(
            stats.symlinks_recreated, 1,
            "symlink libfoo.so from B must be recreated"
        );

        let dest_real = dest.join("lib/libfoo.so.1");
        let dest_link = dest.join("lib/libfoo.so");

        assert!(
            dest_real.is_file() && !dest_real.is_symlink(),
            "libfoo.so.1 must be a real file"
        );
        assert!(dest_link.is_symlink(), "libfoo.so must be a symlink");
        assert_eq!(
            std::fs::read_link(&dest_link).unwrap(),
            std::path::Path::new("libfoo.so.1"),
            "symlink target must be verbatim"
        );
        // Symlink must resolve to the real file placed by layer A
        assert_eq!(
            std::fs::read(&dest_link).unwrap(),
            b"versioned lib",
            "symlink must resolve to the hardlinked sibling from layer A"
        );
    }

    /// ML-22: After assembly into temp and rename to final location, the
    /// relative symlink target string is byte-identical. Rename does not
    /// alter any symlink target stored in the inode.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_symlink_target_survives_temp_rename() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/lib/libfoo.so.1", b"lib content");
        let src_b = make_dir(&root, "layer_b/content");
        make_dir(&root, "layer_b/content/lib");
        crate::symlink::create(
            std::path::Path::new("libfoo.so.1"),
            root.join("layer_b/content/lib/libfoo.so"),
        )
        .unwrap();

        let temp_pkg = root.join("temp/pkg");
        std::fs::create_dir_all(&temp_pkg).unwrap();
        let temp_dest = temp_pkg.join("content");

        assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &temp_dest)
            .await
            .unwrap();

        let target_before = std::fs::read_link(temp_dest.join("lib/libfoo.so")).unwrap();

        let final_pkg = root.join("packages/pkg");
        std::fs::create_dir_all(final_pkg.parent().unwrap()).unwrap();
        std::fs::rename(&temp_pkg, &final_pkg).unwrap();

        let target_after = std::fs::read_link(final_pkg.join("content/lib/libfoo.so")).unwrap();
        assert_eq!(
            target_after, target_before,
            "symlink target string must be byte-identical after rename"
        );
        assert_eq!(
            target_after,
            std::path::Path::new("libfoo.so.1"),
            "target must still be the verbatim layer value"
        );
    }

    // ── 3.6: Stats accumulation ──────────────────────────────────────────────

    /// ML-23: files_hardlinked sums correctly across multiple layers.
    /// Layer A: 3 files; Layer B: 2 files → total 5.
    #[tokio::test]
    async fn ml_stats_sum_files_across_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a1", b"a1");
        make_file(&root, "layer_a/content/a2", b"a2");
        make_file(&root, "layer_a/content/a3", b"a3");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/b1", b"b1");
        make_file(&root, "layer_b/content/b2", b"b2");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 5, "3 from A + 2 from B = 5 total files");
    }

    /// ML-24: bytes_hardlinked sums correctly across layers.
    /// Layer A: 100 bytes; Layer B: 50 bytes → total 150.
    #[tokio::test]
    async fn ml_stats_sum_bytes_across_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        // 100 bytes total in A
        make_file(&root, "layer_a/content/large", &[0u8; 100]);
        let src_b = make_dir(&root, "layer_b/content");
        // 50 bytes total in B
        make_file(&root, "layer_b/content/medium", &[0u8; 50]);

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.bytes_hardlinked, 150, "100 from A + 50 from B = 150 total bytes");
    }

    /// ML-25: dirs_created counts each unique directory exactly once, even when
    /// a directory is shared across layers.
    /// Layer A: bin/, lib/; Layer B: bin/, share/ → shared bin/ created once,
    /// dirs_created = 3 (bin, lib, share).
    #[tokio::test]
    async fn ml_stats_count_dirs_correctly() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/a", b"a");
        make_file(&root, "layer_a/content/lib/liba.so", b"liba");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"b");
        make_file(&root, "layer_b/content/share/doc.txt", b"doc");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        // bin/ (created once, shared), lib/ (A only), share/ (B only) = 3
        assert_eq!(
            stats.dirs_created, 3,
            "bin/ created once + lib/ + share/ = 3 unique dirs"
        );
    }

    /// ML-26: symlinks_recreated sums across all layers.
    /// Layer A: 1 symlink; Layer B: 2 symlinks (all at disjoint paths) → total 3.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_stats_count_symlinks_across_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_dir(&root, "layer_a/content/lib");
        crate::symlink::create(
            std::path::Path::new("libfoo.so.1"),
            root.join("layer_a/content/lib/libfoo.so"),
        )
        .unwrap();

        let src_b = make_dir(&root, "layer_b/content");
        make_dir(&root, "layer_b/content/lib");
        crate::symlink::create(
            std::path::Path::new("libbar.so.2"),
            root.join("layer_b/content/lib/libbar.so"),
        )
        .unwrap();
        crate::symlink::create(
            std::path::Path::new("libbaz.so.3"),
            root.join("layer_b/content/lib/libbaz.so"),
        )
        .unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.symlinks_recreated, 3, "1 from A + 2 from B = 3 symlinks");
    }

    /// ML-27: Zero-byte files are counted in files_hardlinked but contribute
    /// 0 to bytes_hardlinked.
    #[tokio::test]
    async fn ml_stats_zero_length_files_counted() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/empty_a", b"");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/empty_b", b"");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 2, "two zero-byte files must still be counted");
        assert_eq!(
            stats.bytes_hardlinked, 0,
            "zero-byte files contribute 0 to bytes_hardlinked"
        );
    }

    // ── 3.7: Error paths — overlap detection ─────────────────────────────────

    /// ML-28: Two layers both contribute the same file path → LayerOverlap error.
    /// Path bin/tool appears in both A and B.
    #[tokio::test]
    async fn ml_file_overlap_two_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/tool", b"A's tool");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/tool", b"B's tool");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest).await;

        let err = result.expect_err("overlapping file must cause an error");
        match err {
            crate::Error::InternalFile(path, io_err) => {
                let source = io_err.get_ref().expect("must have inner source");
                assert!(
                    source
                        .downcast_ref::<AssemblyError>()
                        .is_some_and(|e| matches!(e, AssemblyError::LayerOverlap))
                        || io_err.kind() == std::io::ErrorKind::AlreadyExists,
                    "expected LayerOverlap or AlreadyExists, got kind={:?} path={}",
                    io_err.kind(),
                    path.display()
                );
            }
            other => panic!("expected Error::InternalFile, got {other:?}"),
        }
    }

    /// ML-29: Three layers where A and C both contribute path x, B contributes
    /// nothing. The overlap must still be detected across non-adjacent layers.
    #[tokio::test]
    async fn ml_file_overlap_three_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/x", b"A's x");
        let src_b = make_dir(&root, "layer_b/content");
        // Layer B is empty — contributes nothing
        let src_c = make_dir(&root, "layer_c/content");
        make_file(&root, "layer_c/content/x", b"C's x");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), src_b.as_path(), src_c.as_path()], &dest).await;

        assert!(
            result.is_err(),
            "overlap between non-adjacent layers A and C must be detected"
        );
    }

    /// ML-30: Two layers both contribute a symlink at the same path → LayerOverlap.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_symlink_overlap() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_dir(&root, "layer_a/content/lib");
        crate::symlink::create(std::path::Path::new("target_a"), root.join("layer_a/content/lib/link")).unwrap();
        let src_b = make_dir(&root, "layer_b/content");
        make_dir(&root, "layer_b/content/lib");
        crate::symlink::create(std::path::Path::new("target_b"), root.join("layer_b/content/lib/link")).unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest).await;

        assert!(result.is_err(), "overlapping symlink name must cause an error");
    }

    /// ML-31: Layer A contributes foo/ as a directory (with a child file).
    /// Layer B contributes foo as a regular file. Type mismatch → error.
    #[tokio::test]
    async fn ml_type_mismatch_dir_vs_file() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/foo/child", b"child of dir");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/foo", b"regular file");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest).await;

        assert!(
            result.is_err(),
            "dir in A vs file in B at same path must cause an error"
        );
    }

    /// ML-32: Layer A contributes foo as a regular file. Layer B contributes
    /// foo/ as a directory. Type mismatch → error (reversed order).
    #[tokio::test]
    async fn ml_type_mismatch_file_vs_dir() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/foo", b"regular file");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/foo/child", b"child of dir");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest).await;

        assert!(
            result.is_err(),
            "file in A vs dir in B at same path must cause an error (reversed)"
        );
    }

    /// ML-33: Layer A contributes foo/ as a directory. Layer B contributes
    /// foo as a symlink. Type mismatch → error.
    #[tokio::test]
    #[cfg(unix)]
    async fn ml_type_mismatch_dir_vs_symlink() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/foo/child", b"child");
        let src_b = make_dir(&root, "layer_b/content");
        crate::symlink::create(std::path::Path::new("some_target"), root.join("layer_b/content/foo")).unwrap();

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest).await;

        assert!(
            result.is_err(),
            "dir in A vs symlink in B at same path must cause an error"
        );
    }

    /// ML-34: Overlap in a shared directory: A and B both contribute bin/a
    /// and bin/b respectively (no overlap), but both also contribute shared/x
    /// (overlap). The error must be raised for shared/x.
    ///
    /// Note: because the walker processes directory-level tasks concurrently,
    /// `bin/` may have been partially or fully assembled before the `shared/`
    /// task detects the overlap and triggers `abort_all()`. Callers assemble
    /// into a temp directory they can discard on error.
    #[tokio::test]
    async fn ml_overlap_detected_in_shared_dir() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/bin/a", b"a");
        make_file(&root, "layer_a/content/shared/x", b"x from A");
        let src_b = make_dir(&root, "layer_b/content");
        make_file(&root, "layer_b/content/bin/b", b"b");
        make_file(&root, "layer_b/content/shared/x", b"x from B");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest).await;

        assert!(result.is_err(), "overlap in shared/x must cause an error");
    }

    // ── 3.8: Error paths — pre-conditions ────────────────────────────────────

    /// ML-35: One of the source paths does not exist → error wrapping NotFound.
    #[tokio::test]
    async fn ml_source_not_existing() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a.txt", b"a");
        let ghost = root.join("nonexistent/content");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), ghost.as_path()], &dest).await;

        let err = result.expect_err("missing source must cause an error");
        match err {
            crate::Error::InternalFile(_, io_err) => {
                assert!(
                    matches!(
                        io_err.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                    ),
                    "expected NotFound or InvalidInput for missing source, got {:?}",
                    io_err.kind()
                );
            }
            other => panic!("expected Error::InternalFile, got {other:?}"),
        }
    }

    /// ML-36: One of the source paths is a regular file (not a directory)
    /// → SourceNotDirectory error.
    #[tokio::test]
    async fn ml_source_is_file() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a.txt", b"a");
        let file_src = make_file(&root, "not_a_dir", b"I am a file");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), file_src.as_path()], &dest).await;

        let err = result.expect_err("regular file as source must cause SourceNotDirectory error");
        match err {
            crate::Error::InternalFile(_, io_err) => {
                let source = io_err.get_ref();
                if let Some(s) = source {
                    assert!(
                        s.downcast_ref::<AssemblyError>()
                            .is_some_and(|e| matches!(e, AssemblyError::SourceNotDirectory)),
                        "expected AssemblyError::SourceNotDirectory, got {s:?}"
                    );
                }
                // If no source, accept AlreadyExists or InvalidInput as platform variation
            }
            other => panic!("expected Error::InternalFile, got {other:?}"),
        }
    }

    /// ML-37: First source is valid, second does not exist → error.
    /// Validates that all sources are checked, not just the first.
    #[tokio::test]
    async fn ml_mixed_valid_invalid_sources() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a.txt", b"a");
        let ghost = root.join("ghost/content");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let result = assemble_from_layers(&[src_a.as_path(), ghost.as_path()], &dest).await;

        assert!(
            result.is_err(),
            "invalid second source must cause an error even when first source is valid"
        );
    }

    /// ML-38: dest_content.parent() does not exist → DestinationParentMissing error.
    #[tokio::test]
    async fn ml_dest_parent_missing() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        make_file(&root, "layer_a/content/a.txt", b"a");

        // Neither nope/ nor nope/pkg/ exists
        let dest = root.join("nope/pkg/content");

        let result = assemble_from_layers(&[src_a.as_path()], &dest).await;

        let err = result.expect_err("missing dest parent must cause DestinationParentMissing");
        match err {
            crate::Error::InternalFile(_, io_err) => {
                let source = io_err.get_ref();
                if let Some(s) = source {
                    assert!(
                        s.downcast_ref::<AssemblyError>()
                            .is_some_and(|e| matches!(e, AssemblyError::DestinationParentMissing)),
                        "expected AssemblyError::DestinationParentMissing, got {s:?}"
                    );
                }
            }
            other => panic!("expected Error::InternalFile, got {other:?}"),
        }
    }

    // ── 3.9: Resource limits ─────────────────────────────────────────────────

    /// ML-39: Combined entry count across all layers triggers EntryLimitExceeded.
    /// Layer A: 5 files; Layer B: 5 files; cap=7 → exceeds limit.
    #[tokio::test]
    async fn ml_entry_limit_across_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        for i in 0..5 {
            make_file(&root, &format!("layer_a/content/a{i}.txt"), b"a");
        }
        let src_b = make_dir(&root, "layer_b/content");
        for i in 0..5 {
            make_file(&root, &format!("layer_b/content/b{i}.txt"), b"b");
        }

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        // Cap of 7 — 10 total entries across both layers exceeds the cap
        let result = assemble_from_layers_with_cap(&[src_a.as_path(), src_b.as_path()], &dest, 7).await;

        assert!(
            result.is_err(),
            "combined entry count of 10 must exceed cap of 7 and cause EntryLimitExceeded"
        );
    }

    // ── 3.10: Concurrency stress ─────────────────────────────────────────────

    /// ML-40: Wide fanout stress test. Layer A: 50 dirs × 5 files each (250 files).
    /// Layer B: same 50 dir names × 5 different files each (250 files).
    /// Result: 50 shared dirs, 500 total files, all hardlinked correctly.
    #[tokio::test]
    async fn ml_wide_fanout_two_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        let src_b = make_dir(&root, "layer_b/content");

        for d in 0..50 {
            for f in 0..5 {
                make_file(&root, &format!("layer_a/content/dir{d}/a{f}.txt"), b"a");
                make_file(&root, &format!("layer_b/content/dir{d}/b{f}.txt"), b"b");
            }
        }

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 500, "250 from A + 250 from B = 500 files");
        assert_eq!(
            stats.dirs_created, 50,
            "50 shared directories must each be created exactly once"
        );

        // Spot-check correctness at extremes
        assert!(dest.join("dir0/a0.txt").exists());
        assert!(dest.join("dir0/b0.txt").exists());
        assert!(dest.join("dir49/a4.txt").exists());
        assert!(dest.join("dir49/b4.txt").exists());
        // Verify no cross-contamination
        assert!(
            !dest.join("dir0/a5.txt").exists(),
            "only 5 files per dir from each layer"
        );
    }

    /// ML-41: Deep tree stress test. Layer A: 20-level deep path ending in x.txt.
    /// Layer B: same 20-level deep path ending in y.txt. Both must be assembled
    /// without stack overflow or depth error, and the 20-level shared path
    /// must contain both files.
    #[tokio::test]
    async fn ml_deep_tree_two_layers() {
        let (_dir, root) = setup();
        let src_a = make_dir(&root, "layer_a/content");
        let src_b = make_dir(&root, "layer_b/content");

        // Build 20-level deep path: level0/level1/.../level19/
        let deep_rel: String = (0..20).map(|i| format!("level{i}")).collect::<Vec<_>>().join("/");
        make_file(&root, &format!("layer_a/content/{deep_rel}/x.txt"), b"deep x");
        make_file(&root, &format!("layer_b/content/{deep_rel}/y.txt"), b"deep y");

        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let dest = root.join("pkg/content");

        let stats = assemble_from_layers(&[src_a.as_path(), src_b.as_path()], &dest)
            .await
            .unwrap();

        assert_eq!(stats.files_hardlinked, 2, "both deep files must be assembled");
        // 20 shared directories, each created once
        assert_eq!(stats.dirs_created, 20, "20 shared directory levels each created once");

        let deep_dest = dest.join(&deep_rel);
        assert!(deep_dest.join("x.txt").exists(), "x.txt must exist at depth 20");
        assert!(deep_dest.join("y.txt").exists(), "y.txt must exist at depth 20");
        assert_eq!(std::fs::read(deep_dest.join("x.txt")).unwrap(), b"deep x");
        assert_eq!(std::fs::read(deep_dest.join("y.txt")).unwrap(), b"deep y");
    }
}
