// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Low-level symlink primitives (create, update, remove).
//!
//! No bookkeeping: install symlinks go through `ReferenceManager`, which keeps `refs/` in sync for GC.

use std::path::Path;

use crate::error::FileError;

/// Validates that a symlink target resolves within `root`; an absolute `target` is always refused.
///
/// `link_path` is where the link is (or will be); `target` is its raw, typically relative, target.
pub fn validate_target(root: &Path, link_path: &Path, target: &Path) -> Result<(), crate::archive::Error> {
    let escape = || crate::archive::Error::SymlinkEscape {
        link: link_path.to_path_buf(),
        target: target.to_path_buf(),
    };

    if target.is_absolute() {
        return Err(escape());
    }

    // Physical, not lexical: after an earlier entry's `a -> .`, `e -> a/..` climbs above the root.
    let parent = link_path.parent().unwrap_or(root);
    let full = parent.join(target);
    match dunce::canonicalize(root) {
        Ok(canonical_root) => resolve_within_root(&canonical_root, &full)
            .map(|_| ())
            .ok_or_else(escape),
        // `root` is not on disk: nothing can redirect, so a lexical check suffices.
        Err(_) => {
            let rel = full.strip_prefix(root).map_err(|_| escape())?;
            crate::fs::path::join_under_root(root, rel).map_err(|_| escape())?;
            Ok(())
        }
    }
}

/// Physically resolves `path`, returning it only when it lands inside `canonical_root`.
///
/// Canonicalizes the longest existing prefix and folds the missing tail lexically, exact since a missing component is no symlink.
fn resolve_within_root(canonical_root: &Path, path: &Path) -> Option<std::path::PathBuf> {
    let components: Vec<_> = path.components().collect();
    for split in (0..=components.len()).rev() {
        let mut prefix = std::path::PathBuf::new();
        for component in &components[..split] {
            prefix.push(component.as_os_str());
        }
        if let Ok(canonical) = dunce::canonicalize(&prefix) {
            let mut candidate = canonical.strip_prefix(canonical_root).ok()?.to_path_buf();
            for component in &components[split..] {
                candidate.push(component.as_os_str());
            }
            return crate::fs::path::join_under_root(canonical_root, &candidate).ok();
        }
    }
    None
}

/// Whether `path` is a symlink or a Windows junction, which [`Path::is_symlink`] misses.
pub fn is_link(path: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        match path.symlink_metadata() {
            Ok(meta) => meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        path.is_symlink()
    }
}

/// Creates or updates a symlink at `link_path`; a no-op when it already points to `target_path`.
pub fn update(
    target_path: impl AsRef<std::path::Path>,
    link_path: impl AsRef<std::path::Path>,
) -> Result<(), FileError> {
    let link_path = link_path.as_ref();
    let target_path = target_path.as_ref();

    if link_path.exists() || is_link(link_path) {
        let link_resolved = std::fs::read_link(link_path).map_err(|error| FileError::new(link_path, error))?;
        if link_resolved == target_path {
            log::debug!(
                "Symlink at '{}' already points to '{}', skipping update.",
                link_path.display(),
                target_path.display()
            );
            return Ok(());
        }
        log::debug!(
            "Symlink at '{}' points to '{}', updating to point to '{}'.",
            link_path.display(),
            link_resolved.display(),
            target_path.display()
        );
        remove(link_path)?;
    }
    create(target_path, link_path)
}

/// Creates a symlink at `link_path` to directory `target`, creating parents; fails if `link_path` exists.
///
/// Directory targets only: Windows junctions support nothing else.
pub fn create(target: impl AsRef<std::path::Path>, link_path: impl AsRef<std::path::Path>) -> Result<(), FileError> {
    let target = target.as_ref();
    let link_path = link_path.as_ref();
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| FileError::new(parent, error))?;
    }
    create_link(target, link_path).map_err(|error| FileError::new(link_path, error))?;
    Ok(())
}

/// Crash-safe symlink replace: stage a `.tmp-*` link beside `link_path`, then rename it over.
///
/// Readers see the old or new link, never none, or `ProjectRegistry` drops a live GC root (`adr_project_gc_symlink_ledger.md`).
pub fn replace_atomic(
    target: impl AsRef<std::path::Path>,
    link_path: impl AsRef<std::path::Path>,
) -> Result<(), FileError> {
    let target = target.as_ref();
    let link_path = link_path.as_ref();

    let parent = match link_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        // A bare name: the staging link must share its directory for an atomic rename.
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).map_err(|error| FileError::new(parent, error))?;

    let temp_path = parent.join(temp_link_name());

    // A crashed run may have left this name; a real failure surfaces from `create_link`.
    if is_link(&temp_path) || temp_path.exists() {
        let _ = remove_link(&temp_path);
    }

    create_link(target, &temp_path).map_err(|error| FileError::new(&temp_path, error))?;

    match rename_replace(&temp_path, link_path) {
        Ok(()) => Ok(()),
        Err(error) => {
            // The rename failed; the staging link would otherwise leak.
            let _ = remove_link(&temp_path);
            Err(FileError::new(link_path, error))
        }
    }
}

/// A process-and-call-unique staging name; `live_projects` skips `.tmp-*`, so it is never a ledger link.
fn temp_link_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(".tmp-{}-{seq}-{nanos}", std::process::id())
}

/// Renames `from` onto `to`, atomically replacing an existing `to`.
#[cfg(not(windows))]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

/// Livelock backstop for the Windows remove-then-rename window, long enough to outlast a Defender-held handle.
#[cfg(windows)]
const WINDOWS_RENAME_RACE_RETRIES: usize = 40;

/// `ERROR_ACCESS_DENIED` (5) or `ERROR_SHARING_VIOLATION` (32): a peer or Defender briefly holding a handle.
#[cfg(windows)]
fn is_transient_lock_error(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5) | Some(32))
}

/// Sleeps an exponential backoff capped at 128 ms, with ±25% jitter.
#[cfg(windows)]
fn rename_race_backoff(attempt: usize) {
    use std::time::Duration;
    let base_ms = 1u64 << (attempt.min(7) as u32); // 2,4,…,128 ms
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter_scale = 0.75 + (f64::from(nanos % 1024) / 1023.0) * 0.5;
    std::thread::sleep(Duration::from_secs_f64((base_ms as f64) * jitter_scale / 1000.0));
}

/// Retries `op` on the transient lock class; `op` must be one syscall that leaves no partial state on failure.
#[cfg(windows)]
fn with_transient_retry<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut last_error: Option<std::io::Error> = None;
    for attempt in 0..WINDOWS_RENAME_RACE_RETRIES {
        if attempt > 0 {
            rename_race_backoff(attempt);
        }
        match op() {
            Ok(value) => return Ok(value),
            Err(error) if is_transient_lock_error(&error) => last_error = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("junction op retries exhausted")))
}

/// Windows `rename_replace`: `rename` refuses an existing destination, so `to` is removed first.
///
/// That leaves a brief window with no entry at `to`; concurrent writers converge on an equivalent link.
#[cfg(windows)]
fn rename_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::io::ErrorKind;

    // No atomic junction replace exists: `MoveFileEx` refuses a directory destination.
    // `from` is unique per call, so a peer's remove never hits it.
    let mut last_error: Option<std::io::Error> = None;
    for attempt in 0..WINDOWS_RENAME_RACE_RETRIES {
        if attempt > 0 {
            rename_race_backoff(attempt);
        }

        // A peer already published our target: converge, since re-renaming widens the reader window.
        if is_link(to) {
            match (std::fs::read_link(to), std::fs::read_link(from)) {
                // An equivalent link is already published — drop our stage, done.
                (Ok(existing), Ok(staged)) if existing == staged => {
                    let _ = remove_link(from);
                    return Ok(());
                }
                // A different target is a genuine re-link: replace below.
                (Ok(_), Ok(_)) => {}
                // A read raced a peer: re-check, never remove, as concurrent `remove_link` teardown fails.
                _ => continue,
            }
        }

        // A removal error with `to` already gone means a peer removed it and we proceed; only a
        // still-present `to` counts as a failure.
        if (is_link(to) || to.exists())
            && let Err(error) = remove_link(to)
            && (is_link(to) || to.exists())
        {
            if is_transient_lock_error(&error) {
                last_error = Some(error);
                continue;
            }
            return Err(error);
        }

        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            // A peer republished `to` between remove and rename; the loop top converges or replaces.
            Err(_) if is_link(to) => continue,
            // Our stage is gone too: a peer consumed the window, nothing left to publish.
            Err(error) if error.kind() == ErrorKind::NotFound && !is_link(from) && !from.exists() => {
                return Ok(());
            }
            // A transient lock or a peer's interleaving: back off and retry.
            Err(error)
                if is_transient_lock_error(&error)
                    || matches!(error.kind(), ErrorKind::NotFound | ErrorKind::AlreadyExists) =>
            {
                last_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        }
    }

    // Exhausted: converge if a link now occupies the slot, else surface the last error.
    if is_link(to) {
        let _ = remove_link(from);
        return Ok(());
    }
    Err(last_error.unwrap_or_else(|| std::io::Error::other("rename retries exhausted")))
}

/// Removes the symlink at `link_path`; a no-op when nothing is there.
pub fn remove(link_path: impl AsRef<std::path::Path>) -> Result<(), FileError> {
    let link_path = link_path.as_ref();
    if link_path.exists() || is_link(link_path) {
        remove_link(link_path).map_err(|error| FileError::new(link_path, error))?;
    }
    Ok(())
}

// ── Platform-specific implementation ─────────────────────────────────────────

#[cfg(not(windows))]
fn create_link(target: &std::path::Path, link_path: &std::path::Path) -> std::io::Result<()> {
    symlink::symlink_auto(target, link_path)
}

#[cfg(not(windows))]
fn remove_link(link_path: &std::path::Path) -> std::io::Result<()> {
    symlink::remove_symlink_auto(link_path)
}

#[cfg(windows)]
fn create_link(target: &std::path::Path, link_path: &std::path::Path) -> std::io::Result<()> {
    // Junctions need no elevation but must have absolute targets.
    let abs_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        // Against the link's directory, as `validate_target` checks: CWD-based, `link -> .` escapes the extraction root.
        let joined = link_path.parent().map(|parent| parent.join(target));
        match joined {
            Some(joined) if joined.is_absolute() => joined,
            _ => std::env::current_dir()?.join(target),
        }
    };
    // One reparse-point write, so a failed attempt created nothing and is safe to retry.
    with_transient_retry(|| junction::create(&abs_target, link_path))
}

/// `ERROR_NOT_A_REPARSE_POINT`: `junction::delete` raced a peer that already stripped the reparse data.
#[cfg(windows)]
const ERROR_NOT_A_REPARSE_POINT: i32 = 4390;

#[cfg(windows)]
fn remove_link(link_path: &std::path::Path) -> std::io::Result<()> {
    use std::io::ErrorKind;
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

    // A non-atomic two-step a peer may run concurrently, so every step tolerates it having completed.
    let meta = match link_path.symlink_metadata() {
        Ok(meta) => meta,
        // A peer already removed the entry — nothing left to do.
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    // Raw attributes: `Metadata::is_dir()` is false for junctions.
    if meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return match std::fs::remove_file(link_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
    }

    // Strip the reparse data, then `remove_dir` (never `_all`, which could recurse into the target).
    match junction::delete(link_path) {
        Ok(()) => {}
        // A peer stripped or removed it first: proceed.
        Err(error)
            if error.raw_os_error() == Some(ERROR_NOT_A_REPARSE_POINT) || error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    match std::fs::remove_dir(link_path) {
        Ok(()) => Ok(()),
        // A peer removed the directory first.
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        // A peer re-published a junction here: never delete a link we did not stage.
        Err(_) if is_link(link_path) => Ok(()),
        Err(error) => Err(error),
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

    fn make_dir(root: &Path, name: &str) -> std::path::PathBuf {
        let p = root.join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn link_path(root: &Path, name: &str) -> std::path::PathBuf {
        let p = root.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        p
    }

    // ── validate_target characterization (U15) — safety net for P2.7 ─────────
    //
    // These lock the CURRENT behaviour of `validate_target` before P2.7
    // refactors its internals onto `join_under_root` (Two Hats: structure
    // changes, behaviour must not). They are GREEN now and must stay GREEN after
    // the refactor. The comprehensive pre-existing suite in `archive.rs`
    // (test_validate_symlink_*) is the companion net and must remain unmodified.

    /// U15 (behavior-preserving · D8/F3): a parent-relative link that stays in
    /// root is accepted; an escaping `../../` target and an absolute target are
    /// both rejected as `SymlinkEscape`.
    #[test]
    fn validate_target_characterization() {
        let root = Path::new("/tmp/root");

        // Parent-relative but in-root: lib/link -> ../bin/tool.
        assert!(
            validate_target(root, Path::new("/tmp/root/lib/link"), Path::new("../bin/tool")).is_ok(),
            "a parent-relative target that stays within root must be accepted"
        );

        // Escaping traversal from a depth-1 link.
        let escaping = validate_target(root, Path::new("/tmp/root/sub/link"), Path::new("../../etc"));
        assert!(
            matches!(escaping, Err(crate::archive::Error::SymlinkEscape { .. })),
            "an escaping `../../` target must be rejected as SymlinkEscape, got {escaping:?}"
        );

        // Absolute target is rejected unconditionally.
        let absolute = validate_target(root, Path::new("/tmp/root/link"), Path::new("/etc/passwd"));
        assert!(
            matches!(absolute, Err(crate::archive::Error::SymlinkEscape { .. })),
            "an absolute target must be rejected as SymlinkEscape, got {absolute:?}"
        );
    }

    /// R2: the physical predicate closes the symlink-chain escape at the shared
    /// function itself, not only at the extraction call sites. An in-root hop
    /// `a -> .` collapses the *lexical* parent of `a/evil` onto the root; a
    /// component-counting predicate would grant `a/evil` a one-level `..` budget
    /// and accept `../out`, but the canonical parent resolves to the root
    /// (physical depth 0), so the escaping target is refused. Uses on-disk
    /// symlinks — the pre-fix lexical `strip_prefix` returned `Ok` here.
    #[cfg(unix)]
    #[test]
    fn validate_target_rejects_symlink_chain_collapse() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        // `a -> .` — an in-root hop that physically resolves back to the root.
        std::os::unix::fs::symlink(".", root.join("a")).unwrap();

        let escaping = validate_target(&root, &root.join("a/evil"), Path::new("../out"));
        assert!(
            matches!(escaping, Err(crate::archive::Error::SymlinkEscape { .. })),
            "the chain-collapsed escaping target must be refused, got {escaping:?}"
        );

        // Control: a genuine in-root parent-relative target through the same hop
        // is still accepted, so the predicate is not simply refusing everything.
        assert!(
            validate_target(&root, &root.join("a/evil"), Path::new("sibling")).is_ok(),
            "an in-root parent-relative target through the hop must still be accepted"
        );
    }

    /// R2 residual: the collapse must be caught even when the link's own leaf
    /// (and intermediate directories) do not exist on disk yet. `a -> .` is a
    /// real hop, `a/b/c` is absent. The physical parent of `a/b/c/evil` is
    /// `root/b/c` (depth 2, `a` folds away), so a 3-`..` target escapes — but a
    /// predicate that only canonicalizes the parent when it *fully* exists (the
    /// prior fix) falls back to a lexical count of `a/b/c` = 3 and accepts it.
    /// Resolving the longest existing prefix (`root/a` → root) and counting the
    /// absent `b/c` tail lexically gives the true depth and refuses it.
    #[cfg(unix)]
    #[test]
    fn validate_target_rejects_collapse_through_absent_tail() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        // `a -> .`; `b`/`c` are never created, so the parent `a/b/c` is absent.
        std::os::unix::fs::symlink(".", root.join("a")).unwrap();

        let escaping = validate_target(&root, &root.join("a/b/c/evil"), Path::new("../../../out"));
        assert!(
            matches!(escaping, Err(crate::archive::Error::SymlinkEscape { .. })),
            "a target escaping the true physical depth through an absent tail must be refused, got {escaping:?}"
        );

        // Control: two `..` from the physical depth-2 parent lands back in root.
        assert!(
            validate_target(&root, &root.join("a/b/c/evil"), Path::new("../../sibling")).is_ok(),
            "an in-root target within the true physical depth must still be accepted"
        );
    }

    // ── is_link ──────────────────────────────────────────────────────────────

    #[test]
    fn is_link_false_for_nonexistent() {
        let (_dir, root) = setup();
        assert!(!is_link(&root.join("nonexistent")));
    }

    #[test]
    fn is_link_false_for_regular_dir() {
        let (_dir, root) = setup();
        let d = make_dir(&root, "regular");
        assert!(!is_link(&d));
    }

    // ── create + is_link ─────────────────────────────────────────────────────

    #[test]
    fn create_to_existing_dir() {
        let (_dir, root) = setup();
        let target = make_dir(&root, "target");
        let link = link_path(&root, "link");

        create(&target, &link).unwrap();

        assert!(is_link(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn create_to_nonexistent_path() {
        let (_dir, root) = setup();
        let ghost = root.join("deep").join("nonexistent");
        let link = link_path(&root, "link");

        create(&ghost, &link).unwrap();

        assert!(is_link(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), ghost);
    }

    // ── remove ───────────────────────────────────────────────────────────────

    #[test]
    fn remove_existing_link() {
        let (_dir, root) = setup();
        let target = make_dir(&root, "target");
        let link = link_path(&root, "link");
        create(&target, &link).unwrap();

        remove(&link).unwrap();

        assert!(!is_link(&link));
        assert!(!link.exists());
    }

    #[test]
    fn remove_dangling_link() {
        let (_dir, root) = setup();
        let ghost = root.join("nonexistent");
        let link = link_path(&root, "link");
        create(&ghost, &link).unwrap();

        remove(&link).unwrap();

        assert!(!is_link(&link));
    }

    #[test]
    fn remove_noop_for_nonexistent() {
        let (_dir, root) = setup();
        remove(root.join("nope")).unwrap();
    }

    // ── update ───────────────────────────────────────────────────────────────

    #[test]
    fn update_creates_new_link() {
        let (_dir, root) = setup();
        let target = make_dir(&root, "target");
        let link = link_path(&root, "link");

        update(&target, &link).unwrap();

        assert!(is_link(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    #[test]
    fn update_replaces_link() {
        let (_dir, root) = setup();
        let a = make_dir(&root, "a");
        let b = make_dir(&root, "b");
        let link = link_path(&root, "link");

        create(&a, &link).unwrap();
        update(&b, &link).unwrap();

        assert_eq!(std::fs::read_link(&link).unwrap(), b);
    }

    #[test]
    fn update_noop_when_same_target() {
        let (_dir, root) = setup();
        let target = make_dir(&root, "target");
        let link = link_path(&root, "link");

        create(&target, &link).unwrap();
        update(&target, &link).unwrap();

        assert_eq!(std::fs::read_link(&link).unwrap(), target);
    }

    // ── replace_atomic ───────────────────────────────────────────────────────

    /// `replace_atomic` over an *existing* link (not just first create) must
    /// leave the final link pointing at the NEW target and must not leak any
    /// `.tmp-*` staging entry in the directory.
    #[test]
    fn replace_atomic_replaces_existing_link_no_temp_leak() {
        let (_dir, root) = setup();
        let store = make_dir(&root, "store");
        let old = make_dir(&root, "old");
        let new = make_dir(&root, "new");
        let link = store.join("entry");

        replace_atomic(&old, &link).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), old);

        // Replace over the existing link.
        replace_atomic(&new, &link).unwrap();
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            new,
            "final link must point at the new target after replace"
        );

        let leftovers: Vec<_> = std::fs::read_dir(&store)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "no .tmp-* staging entry may leak: {leftovers:?}");
    }

    /// `temp_link_name` produces a `.tmp-`-prefixed name, and two successive
    /// calls differ. Uniqueness is load-bearing: `live_projects`'s
    /// readdir-skip of `.tmp-*` relies on staging names never colliding with a
    /// real ledger entry, and concurrent stagers must not collide.
    #[test]
    fn temp_link_name_is_prefixed_and_unique() {
        let a = temp_link_name();
        let b = temp_link_name();
        assert!(a.starts_with(".tmp-"), "temp name must be .tmp-prefixed: {a}");
        assert!(b.starts_with(".tmp-"), "temp name must be .tmp-prefixed: {b}");
        assert_ne!(a, b, "two temp names must be unique (monotonic counter)");
    }

    /// The parentless/relative-path branch of `replace_atomic` must not panic:
    /// `link_path.parent()` is `Some("")` for a bare relative name, which the
    /// helper maps to `.` (the current dir) so the rename stays same-dir.
    #[test]
    fn replace_atomic_relative_bare_name_does_not_panic() {
        let (_dir, root) = setup();
        let target = make_dir(&root, "target");

        // Run inside the tempdir so the bare relative name resolves there and
        // we do not pollute the real CWD. Serialised: process-global CWD.
        let _guard = CwdGuard::enter(&root);
        let bare = Path::new("bare-entry");

        replace_atomic(&target, bare).expect("bare relative name must not panic");
        assert!(is_link(bare), "bare relative link must be created");
        assert_eq!(std::fs::read_link(bare).unwrap(), target);
    }

    /// RAII current-directory guard so the relative-path test does not leak a
    /// changed process CWD into other tests. `set_current_dir` is
    /// process-global; this test does not run in parallel with CWD-sensitive
    /// code under nextest's per-test isolation, and the guard restores on drop.
    struct CwdGuard {
        prev: std::path::PathBuf,
    }

    impl CwdGuard {
        fn enter(dir: &Path) -> Self {
            let prev = std::env::current_dir().expect("read cwd");
            std::env::set_current_dir(dir).expect("set cwd");
            Self { prev }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.prev);
        }
    }

    // ── chained links (back-ref style) ──────────────────────────────────────

    #[test]
    fn create_and_remove_chained_links() {
        let (_dir, root) = setup();
        let content = make_dir(&root, "objects/reg/repo/d1/content");
        let forward = link_path(&root, "fwd/link1");
        let refs_dir = make_dir(&root, "objects/reg/repo/d1/refs");
        let back_ref = refs_dir.join("somehash");

        // forward → content
        create(&content, &forward).unwrap();
        // back_ref → forward
        create(&forward, &back_ref).unwrap();

        assert!(is_link(&forward));
        assert!(is_link(&back_ref));
        assert_eq!(std::fs::read_link(&forward).unwrap(), content);
        assert_eq!(std::fs::read_link(&back_ref).unwrap(), forward);

        // Remove back-ref first, then forward
        remove(&back_ref).unwrap();
        assert!(!is_link(&back_ref));

        remove(&forward).unwrap();
        assert!(!is_link(&forward));
    }

    // ── symlinks survive directory rename (temp → packages invariant) ──────

    /// The walker creates symlinks from layers into the package's temp
    /// directory, then the pull pipeline atomically renames the temp dir to
    /// its final `packages/` location. POSIX `rename(2)` is inode-preserving
    /// and a symlink's target string lives in the inode — so a symlink
    /// created in temp must remain intact (same target string, still
    /// resolvable) after the rename. This is the symlink counterpart to
    /// `hardlink_survives_directory_rename` in `hardlink.rs`.
    ///
    /// The walker must preserve target strings verbatim, which means:
    ///   - A relative target inside a mirrored subtree continues to resolve
    ///     correctly after the containing directory moves.
    ///   - An absolute target pointing outside the moved tree is unaffected.
    #[test]
    #[cfg(unix)]
    fn symlink_survives_directory_rename() {
        let (_dir, root) = setup();

        // Simulate a "temp package" directory with two sibling files and
        // several symlinks that the walker would recreate from a layer.
        let temp_pkg = root.join("temp").join("pkg");
        let content = temp_pkg.join("content");
        let lib_dir = content.join("lib");
        std::fs::create_dir_all(&lib_dir).unwrap();

        // A real file we'll point relative symlinks at.
        let real_file = lib_dir.join("libfoo.so.1.2.3");
        std::fs::write(&real_file, b"shared library bytes").unwrap();

        // 1. Relative symlink, same directory:
        //    lib/libfoo.so.1 → libfoo.so.1.2.3
        let link_same_dir = lib_dir.join("libfoo.so.1");
        create(std::path::Path::new("libfoo.so.1.2.3"), &link_same_dir).unwrap();

        // 2. Relative symlink, cross-directory:
        //    content/tool → lib/libfoo.so.1.2.3
        let link_cross_dir = content.join("tool");
        create(std::path::Path::new("lib/libfoo.so.1.2.3"), &link_cross_dir).unwrap();

        // 3. Absolute symlink, pointing to an external location that will
        //    not be affected by the rename. We use a tempdir sibling so the
        //    assertion doesn't rely on any system file.
        let external = root.join("external_target");
        std::fs::write(&external, b"external bytes").unwrap();
        let link_absolute = content.join("external_link");
        create(&external, &link_absolute).unwrap();

        // Capture all target strings BEFORE the rename. These must be
        // byte-identical after the rename.
        let tgt_same_before = std::fs::read_link(&link_same_dir).unwrap();
        let tgt_cross_before = std::fs::read_link(&link_cross_dir).unwrap();
        let tgt_abs_before = std::fs::read_link(&link_absolute).unwrap();

        // Atomic rename — temp/pkg → final/pkg (same filesystem).
        let final_pkg = root.join("final").join("pkg");
        std::fs::create_dir_all(final_pkg.parent().unwrap()).unwrap();
        std::fs::rename(&temp_pkg, &final_pkg).unwrap();

        // Re-resolve the symlinks at their new paths.
        let final_content = final_pkg.join("content");
        let final_lib = final_content.join("lib");
        let final_link_same = final_lib.join("libfoo.so.1");
        let final_link_cross = final_content.join("tool");
        let final_link_abs = final_content.join("external_link");

        // Target strings are preserved verbatim (rename does not mutate the
        // symlink inode's data — this is THE fundamental guarantee).
        assert_eq!(
            std::fs::read_link(&final_link_same).unwrap(),
            tgt_same_before,
            "same-dir relative target must be preserved verbatim"
        );
        assert_eq!(
            std::fs::read_link(&final_link_cross).unwrap(),
            tgt_cross_before,
            "cross-dir relative target must be preserved verbatim"
        );
        assert_eq!(
            std::fs::read_link(&final_link_abs).unwrap(),
            tgt_abs_before,
            "absolute target must be preserved verbatim"
        );

        // Relative targets resolve correctly at the new location via the
        // mirrored directory structure.
        assert_eq!(
            std::fs::read(&final_link_same).unwrap(),
            b"shared library bytes",
            "same-dir relative symlink resolves to the moved sibling after rename"
        );
        assert_eq!(
            std::fs::read(&final_link_cross).unwrap(),
            b"shared library bytes",
            "cross-dir relative symlink resolves through the moved tree"
        );

        // Absolute symlink resolves to the unchanged external target.
        assert_eq!(
            std::fs::read(&final_link_abs).unwrap(),
            b"external bytes",
            "absolute symlink still resolves after rename"
        );
    }

    /// A relative symlink whose target string happens to contain the temp
    /// directory path (a publisher/walker bug) does NOT survive the rename.
    /// This test exists as documentation: it's the kind of target the walker
    /// must never construct. If this invariant ever regresses, this test
    /// will start failing the "broken after rename" assertion and the
    /// walker's target-preservation bug will be caught.
    #[test]
    #[cfg(unix)]
    fn symlink_with_temp_path_in_target_breaks_after_rename() {
        let (_dir, root) = setup();
        let temp_pkg = root.join("temp").join("pkg");
        let content = temp_pkg.join("content");
        std::fs::create_dir_all(&content).unwrap();

        let real = content.join("real");
        std::fs::write(&real, b"payload").unwrap();

        // Bug-shape symlink: absolute target pointing INSIDE the temp dir.
        // The walker must NEVER create one of these — it would only happen
        // if the walker called something like `temp_pkg.join("real")` to
        // compute the target instead of using the layer's `read_link` value.
        let bogus = content.join("bogus");
        create(&real, &bogus).unwrap();
        assert_eq!(std::fs::read(&bogus).unwrap(), b"payload");

        // Atomic rename.
        let final_pkg = root.join("final").join("pkg");
        std::fs::create_dir_all(final_pkg.parent().unwrap()).unwrap();
        std::fs::rename(&temp_pkg, &final_pkg).unwrap();

        // The link target still points at the OLD absolute path, which no
        // longer exists after the rename. Resolution fails.
        let final_bogus = final_pkg.join("content").join("bogus");
        let read_result = std::fs::read(&final_bogus);
        assert!(
            read_result.is_err(),
            "absolute-target-into-temp symlink must break after rename — \
             this failure mode is exactly what the walker must avoid"
        );
        // The target string itself still points at the original temp path.
        let preserved = std::fs::read_link(&final_bogus).unwrap();
        assert!(
            preserved.starts_with(&temp_pkg),
            "target is byte-preserved verbatim even when it's now a dangling absolute path"
        );
    }

    // ── Windows-specific junction behavior ──────────────────────────────────

    #[cfg(windows)]
    mod windows {
        use super::*;

        #[test]
        fn is_link_detects_junction() {
            let (_dir, root) = setup();
            let target = make_dir(&root, "target");
            let link = link_path(&root, "link");

            junction::create(&target, &link).unwrap();

            assert!(is_link(&link));
        }

        #[test]
        fn std_is_dir_is_false_for_junctions() {
            let (_dir, root) = setup();
            let target = make_dir(&root, "target");
            let link = link_path(&root, "link");

            junction::create(&target, &link).unwrap();

            // Rust's `is_dir()` returns false for junctions because it checks
            // `!is_symlink() && is_directory()`. This is why `remove_link`
            // checks raw file attributes instead.
            let meta = link.symlink_metadata().unwrap();
            assert!(!meta.is_dir());
        }

        #[test]
        fn junction_delete_then_remove_dir() {
            let (_dir, root) = setup();
            let target = make_dir(&root, "target");
            let link = link_path(&root, "link");

            junction::create(&target, &link).unwrap();
            junction::delete(&link).unwrap();
            std::fs::remove_dir(&link).unwrap();

            assert!(!link.exists());
            assert!(target.exists(), "target must not be deleted through junction");
        }
    }
}
