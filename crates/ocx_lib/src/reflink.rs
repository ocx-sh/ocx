// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cross-filesystem file placement: reflink (CoW clone) with a full-copy
//! fallback. THE one place `reflink_copy` is called — mirror of `hardlink.rs`
//! for the cross-device case.
//!
//! Unlike [`crate::hardlink::create`], which shares an inode between source and
//! destination, `reflink::create` always produces an **independent inode**. The
//! destination is either a copy-on-write clone of the source (when the
//! underlying filesystem supports reflinking — e.g. btrfs, APFS, XFS with
//! `reflink=1`) or a full byte-for-byte copy otherwise. In both cases:
//!
//! - Executable bits and content are duplicated, not shared.
//! - Modifying the destination through one inode does not affect the source
//!   (unlike a hardlink).
//! - On macOS, a package assembled cross-device may need re-signing even if
//!   the signature bytes were copied, because the inode is independent — this
//!   is handled in P2.4.
//!
//! **When to use this module vs `hardlink::create`:**
//! - Same filesystem → use `crate::hardlink::create` (shared inode, zero-copy).
//! - Different filesystem → use `crate::reflink::create` (independent inode,
//!   CoW or copy). The assembly walker probes `same_filesystem` and dispatches
//!   through [`crate::utility::fs::assemble::AssemblyMode`].

use crate::prelude::*;

/// Places a file at `link` as an independent inode copy of `source`.
///
/// Uses `reflink_copy::reflink_or_copy`: on filesystems that support CoW
/// reflinking (btrfs, APFS, XFS with `reflink=1`) the kernel clones the
/// extents instantly. On all other filesystems a full byte-for-byte copy is
/// performed. Either way the result is an **independent inode** — modifying
/// `link` does not affect `source`.
///
/// Creates any missing parent directories of `link`. Fails if `link` already
/// exists.
///
/// # Errors
///
/// Returns `crate::Error::InternalFile` wrapping the underlying `io::Error`
/// if:
/// - A parent directory of `link` cannot be created.
/// - `source` does not exist or is not readable.
/// - `link` already exists (`io::ErrorKind::AlreadyExists`).
/// - Any other I/O failure during the copy or reflink syscall.
pub fn create(source: impl AsRef<std::path::Path>, link: impl AsRef<std::path::Path>) -> Result<()> {
    let source = source.as_ref();
    let link = link.as_ref();
    reflink_or_copy_or_err(source, link).map_err(|error| Error::InternalFile(link.to_path_buf(), error))?;
    Ok(())
}

/// Creates any missing parent dirs of `link`, then reflinks (CoW clone) or
/// copies `source` to `link`. Returns the raw `io::Error` on failure.
///
/// This is THE ONE place `reflink_copy::reflink_or_copy` is called.
///
/// `Ok(None)` from `reflink_or_copy` means a CoW reflink succeeded (e.g. btrfs
/// cross-subvolume or APFS). `Ok(Some(bytes))` means a full byte-for-byte copy
/// was performed (cross-device on ext4/tmpfs). Either way the result is an
/// independent inode with byte-identical content.
fn reflink_or_copy_or_err(source: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    reflink_copy::reflink_or_copy(source, link)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    // ── Test helper: second_device_root ─────────────────────────────────────
    //
    // Returns a fresh unique subdirectory under `/dev/shm` IFF:
    //   (a) `/dev/shm` exists, AND
    //   (b) it is on a different `st_dev` than `primary`.
    //
    // On this Linux dev box `/dev/shm` is tmpfs — a different device from the
    // tmpfs used by `tempfile::tempdir()` — so these tests FAIL with the
    // unimplemented stub (proving RED).  If no second device is available the
    // tests print a note and return early (not a test failure).
    #[cfg(unix)]
    fn second_device_root(primary: &Path) -> Option<std::path::PathBuf> {
        use std::os::unix::fs::MetadataExt;

        let shm = std::path::Path::new("/dev/shm");
        if !shm.exists() {
            return None;
        }
        let primary_dev = std::fs::metadata(primary).ok()?.dev();
        let shm_dev = std::fs::metadata(shm).ok()?.dev();
        if primary_dev == shm_dev {
            return None;
        }
        // Unique per-test subdirectory to avoid collisions between parallel runs.
        let subdir = shm.join(format!("ocx_reflink_test_{}_{}", std::process::id(), uuid_suffix()));
        std::fs::create_dir_all(&subdir).ok()?;
        Some(subdir)
    }

    /// Simple non-crypto unique suffix using the thread ID + time-based counter.
    fn uuid_suffix() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        format!("{:016x}", COUNTER.fetch_add(1, Ordering::Relaxed))
    }

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

    // ── R1: same-fs create produces independent inode ────────────────────────
    //
    // `reflink::create` must ALWAYS produce an independent inode — even on the
    // same filesystem.  This is what distinguishes it from `hardlink::create`.
    // A reflink/copy never shares an inode with the source.
    //
    // FAILS NOW: `create` is `unimplemented!()`.
    #[test]
    fn create_produces_independent_inode_same_fs() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            let (_dir, root) = setup();
            let source = make_file(&root, "source.bin", b"hello reflink");
            let link = root.join("link.bin");

            create(&source, &link).unwrap();

            assert!(link.exists(), "link path must exist after create()");
            assert_eq!(
                std::fs::read(&link).unwrap(),
                b"hello reflink",
                "link must contain the same bytes as the source"
            );

            let ino_source = std::fs::metadata(&source).unwrap().ino();
            let ino_link = std::fs::metadata(&link).unwrap().ino();
            assert_ne!(
                ino_source, ino_link,
                "reflink::create must produce an independent inode (not a hardlink)"
            );
        }
        #[cfg(not(unix))]
        {
            // On non-Unix we can only check content correctness.
            let (_dir, root) = setup();
            let source = make_file(&root, "source.bin", b"hello reflink");
            let link = root.join("link.bin");
            create(&source, &link).unwrap();
            assert_eq!(std::fs::read(&link).unwrap(), b"hello reflink");
        }
    }

    // ── R2: cross-device create via copy fallback ────────────────────────────
    //
    // When source and link are on different devices, `reflink::create` must
    // succeed by falling back to a full byte copy.  Uses `/dev/shm` as the
    // second device (self-skips when unavailable or on the same device).
    //
    // FAILS NOW: `create` is `unimplemented!()`.
    #[cfg(unix)]
    #[test]
    fn create_cross_device_falls_back_to_copy() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        let source = make_file(&root, "source.bin", b"cross device content");

        let Some(shm_dir) = second_device_root(&root) else {
            eprintln!("skip: no second device available (no /dev/shm or same device)");
            return;
        };
        let link = shm_dir.join("link.bin");

        // Verify the devices actually differ before calling create.
        let dev_source = std::fs::metadata(&root).unwrap().dev();
        let dev_shm = std::fs::metadata(&shm_dir).unwrap().dev();
        assert_ne!(
            dev_source, dev_shm,
            "precondition: source and link must be on different devices"
        );

        let result = create(&source, &link);
        let content = result.as_ref().ok().and_then(|()| std::fs::read(&link).ok());
        let _ = std::fs::remove_dir_all(&shm_dir); // cleanup

        result.expect("cross-device create must succeed via copy fallback");
        assert_eq!(
            content.expect("link must be readable after copy"),
            b"cross device content",
        );
    }

    // ── R3: exec bit preserved ───────────────────────────────────────────────
    //
    // When the source file has mode 0o755, the link must also have mode 0o755.
    // Both a CoW reflink and a full copy must preserve Unix permission bits.
    //
    // FAILS NOW: `create` is `unimplemented!()`.
    #[cfg(unix)]
    #[test]
    fn create_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, root) = setup();
        let source = make_file(&root, "tool", b"#!/bin/sh\necho hi\n");
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();

        let link = root.join("tool_copy");
        create(&source, &link).unwrap();

        let mode = std::fs::metadata(&link).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "reflink::create must preserve executable bits (0o755)");
    }

    // ── R4: creates missing parent directories ───────────────────────────────
    //
    // `hardlink::create` creates missing parent dirs (see `hard_link_or_err`).
    // `reflink::create` mirrors this contract: if the link path is nested under
    // directories that don't exist yet, they must be created automatically.
    //
    // FAILS NOW: `create` is `unimplemented!()`.
    #[test]
    fn create_creates_missing_parent_dirs() {
        let (_dir, root) = setup();
        let source = make_file(&root, "source.bin", b"content");
        // Link lives inside a directory chain that does not exist yet.
        let link = root.join("nested").join("sub").join("dir").join("file.bin");

        create(&source, &link).unwrap();

        assert!(link.exists(), "link must exist even when parents were absent");
        assert_eq!(
            std::fs::read(&link).unwrap(),
            b"content",
            "link must carry the source content"
        );
    }

    // ── R5: missing source errors ────────────────────────────────────────────
    //
    // When the source path does not exist, `create` must return `Err` rather
    // than panic or silently succeed.
    #[test]
    fn create_errors_when_source_missing() {
        let (_dir, root) = setup();
        let ghost = root.join("nonexistent_source");
        let link = root.join("link");
        let result = create(&ghost, &link);
        assert!(result.is_err(), "create() must return Err for a missing source");
    }

    // ── R6: link-already-exists errors ──────────────────────────────────────
    //
    // When the link path already exists, `create` must return `Err` rather
    // than silently overwriting the existing file.
    #[test]
    fn create_errors_when_link_already_exists() {
        let (_dir, root) = setup();
        let source = make_file(&root, "source", b"original");
        let link = make_file(&root, "link", b"already here");
        let result = create(&source, &link);
        assert!(result.is_err(), "create() must fail when link already exists");
    }
}
