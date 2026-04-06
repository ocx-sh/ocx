// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Low-level symlink primitives (create, update, remove).
//!
//! These functions operate on a single symlink without any bookkeeping.
//! For install symlinks (candidates and current under `symlinks/`), use
//! [`crate::reference_manager::ReferenceManager`] instead — it keeps the
//! `refs/` back-references in sync, which is required for garbage collection.
//!
//! On Windows, NTFS junction points are used as a transparent fallback when
//! the process lacks the `SeCreateSymbolicLinkPrivilege` required for native
//! symlinks.  Junctions behave identically to directory symlinks for the
//! purposes of this crate.

use std::path::Path;

use crate::{log, prelude::*, utility::fs::path::lexical_normalize};

/// Validates that a symlink target resolves within `root`.
///
/// `link_path` is the absolute path where the symlink is (or will be) located.
/// `target` is the raw symlink target (typically relative). Absolute targets
/// are rejected unconditionally.
///
/// Used by the archive extractor (as the primary input-trust boundary) and by
/// the layer assembly walker (as defence-in-depth against any path that
/// populates `layers/.../content/` without going through the archive layer).
pub fn validate_target(root: &Path, link_path: &Path, target: &Path) -> Result<()> {
    if target.is_absolute() {
        return Err(Error::Archive(crate::archive::Error::SymlinkEscape {
            link: link_path.to_path_buf(),
            target: target.to_path_buf(),
        }));
    }
    let parent = link_path.parent().unwrap_or(root);
    let resolved = lexical_normalize(&parent.join(target));
    let normalized_root = lexical_normalize(root);
    if !resolved.starts_with(&normalized_root) {
        return Err(Error::Archive(crate::archive::Error::SymlinkEscape {
            link: link_path.to_path_buf(),
            target: target.to_path_buf(),
        }));
    }
    Ok(())
}

/// Returns `true` if `path` is a symlink or (on Windows) a junction point.
///
/// On Unix this is equivalent to [`Path::is_symlink`].  On Windows it also
/// detects NTFS junction points (reparse points with tag `IO_REPARSE_TAG_MOUNT_POINT`),
/// which [`Path::is_symlink`] does not report.
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

/// Creates or updates a symlink at `link_path` pointing to `target_path`.
///
/// No-op if `link_path` already resolves to `target_path`.
/// Removes any existing symlink (including dangling ones) before creating anew.
///
/// For install symlinks, use [`crate::reference_manager::ReferenceManager::link`].
pub fn update(target_path: impl AsRef<std::path::Path>, link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let link_path = link_path.as_ref();
    let target_path = target_path.as_ref();

    if link_path.exists() || is_link(link_path) {
        let link_resolved =
            std::fs::read_link(link_path).map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
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

/// Creates a new symlink at `link_path` pointing to `target`.
///
/// Creates any missing parent directories. Fails if `link_path` already exists.
/// The target is expected to be a directory (or a not-yet-existing path that
/// will become a directory). On Windows, NTFS junction points are used which
/// only support directory targets.
///
/// For install symlinks, use [`crate::reference_manager::ReferenceManager::link`].
pub fn create(target: impl AsRef<std::path::Path>, link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let target = target.as_ref();
    let link_path = link_path.as_ref();
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::InternalFile(parent.to_path_buf(), error))?;
    }
    create_link(target, link_path).map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
    Ok(())
}

/// Removes the symlink at `link_path`.
///
/// No-op if `link_path` does not exist and is not a dangling symlink.
///
/// For install symlinks, use [`crate::reference_manager::ReferenceManager::unlink`].
pub fn remove(link_path: impl AsRef<std::path::Path>) -> Result<()> {
    let link_path = link_path.as_ref();
    if link_path.exists() || is_link(link_path) {
        remove_link(link_path).map_err(|error| Error::InternalFile(link_path.to_path_buf(), error))?;
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
    // Use NTFS junction points on Windows. They behave like directory
    // symlinks but do not require elevated privileges or Developer Mode.
    // Junction targets must be absolute paths.
    let abs_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()?.join(target)
    };
    junction::create(&abs_target, link_path)
}

#[cfg(windows)]
fn remove_link(link_path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

    // `Metadata::is_dir()` returns false for junction points because Rust
    // checks `!is_symlink() && is_directory()`. Check raw file attributes
    // instead to correctly identify junctions and directory symlinks.
    let meta = link_path.symlink_metadata()?;
    if meta.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0 {
        // Junctions are empty directory entries — the contents live in the
        // target. First strip the reparse data, then remove the empty dir.
        // Using `remove_dir` (not `_all`) ensures we can never accidentally
        // recurse into the target, regardless of future Rust behavior.
        junction::delete(link_path)?;
        std::fs::remove_dir(link_path)
    } else {
        std::fs::remove_file(link_path)
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
