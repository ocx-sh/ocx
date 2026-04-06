// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Low-level hardlink primitives (create, update).
//!
//! Hardlinks share an inode between two paths — used for file-level dedup
//! when assembling a package's `content/` directory from one or more layers.
//! Immutable package/layer content makes hardlinks safe: there's no scenario
//! where mutating through one path would surprise a reader of another.
//!
//! Cross-volume hardlinks fail with `io::ErrorKind::CrossesDevices`. OCX
//! assumes that `blobs/`, `layers/`, `packages/`, and `temp/` all live on a
//! single filesystem — a constraint already imposed by the `temp → packages/`
//! atomic rename. Operators who split `$OCX_HOME` across volumes will see a
//! clear error at install time rather than a silent dedup regression.
//!
//! This module mirrors [`crate::symlink`] for consistency.

use crate::prelude::*;

/// Creates a new hardlink at `link` referencing the same inode as `source`.
///
/// Creates any missing parent directories. Fails if `link` already exists.
/// Fails with `io::ErrorKind::CrossesDevices` (or the platform equivalent)
/// if `source` and `link` are on different filesystems — callers must keep
/// `$OCX_HOME` on a single volume to guarantee success.
pub fn create(source: impl AsRef<std::path::Path>, link: impl AsRef<std::path::Path>) -> Result<()> {
    let source = source.as_ref();
    let link = link.as_ref();
    hard_link_or_err(source, link).map_err(|error| Error::InternalFile(link.to_path_buf(), error))?;
    Ok(())
}

/// Creates or replaces a hardlink at `link` referencing `source`.
///
/// If `link` already exists (file or link), it is removed first. Then the
/// hardlink is created. Fails on cross-volume for the same reason as
/// [`create`].
pub fn update(source: impl AsRef<std::path::Path>, link: impl AsRef<std::path::Path>) -> Result<()> {
    let source = source.as_ref();
    let link = link.as_ref();

    if link.exists() || crate::symlink::is_link(link) {
        std::fs::remove_file(link).map_err(|error| Error::InternalFile(link.to_path_buf(), error))?;
    }
    create(source, link)
}

// ── Platform-specific implementation ─────────────────────────────────────────

/// Creates any missing parent directories of `link` and then hardlinks
/// `source` to `link`. Returns the raw `io::Error` on failure.
///
/// The body is identical on Unix and Windows — `std::fs::hard_link` handles
/// both NTFS and POSIX filesystems. This is THE ONE place in the codebase
/// where direct `std::fs::hard_link` is allowed, because this IS the wrapper.
fn hard_link_or_err(source: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::hard_link(source, link)
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

    fn make_file(root: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        let p = root.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
        p
    }

    // ── create: happy path ───────────────────────────────────────────────────

    /// H1: create() on same filesystem — hardlink created, both paths accessible.
    #[test]
    fn create_links_file_same_directory() {
        let (_dir, root) = setup();
        let source = make_file(&root, "source.txt", b"hello hardlink");
        let link = root.join("link.txt");

        create(&source, &link).unwrap();

        assert!(link.exists(), "link path must exist after create()");
        assert_eq!(
            std::fs::read(&link).unwrap(),
            b"hello hardlink",
            "link must contain the same bytes as the source"
        );
    }

    // ── create: parent dir missing ───────────────────────────────────────────

    /// H2: create() when link's parent directory does not exist — parent is
    /// created recursively before the hardlink is made.
    #[test]
    fn create_creates_missing_parent_dirs() {
        let (_dir, root) = setup();
        let source = make_file(&root, "source.txt", b"content");
        // link lives inside a directory chain that does not exist yet
        let link = root.join("a").join("b").join("c").join("link.txt");

        create(&source, &link).unwrap();

        assert!(link.exists(), "link must exist even when parents were absent");
        assert_eq!(
            std::fs::read(&link).unwrap(),
            b"content",
            "link must carry the source content"
        );
    }

    // ── create: target exists ────────────────────────────────────────────────

    /// H3: create() when link path already exists — must return an error with
    /// an underlying io::ErrorKind::AlreadyExists cause.
    #[test]
    fn create_fails_if_link_exists() {
        let (_dir, root) = setup();
        let source = make_file(&root, "source.txt", b"original");
        // pre-create a file at the link path
        let link = make_file(&root, "link.txt", b"already here");

        let result = create(&source, &link);

        assert!(result.is_err(), "create() must fail when link already exists");
        // The error must wrap an io::ErrorKind::AlreadyExists
        match result.unwrap_err() {
            crate::Error::InternalFile(_, io_err) => {
                assert_eq!(
                    io_err.kind(),
                    std::io::ErrorKind::AlreadyExists,
                    "underlying io error must be AlreadyExists"
                );
            }
            other => panic!("expected Error::InternalFile, got: {other:?}"),
        }
    }

    // ── create: source missing ───────────────────────────────────────────────

    /// H4: `create()` with a source that does not exist must return an error
    /// wrapping the IO failure with path context (`Error::InternalFile`).
    #[test]
    fn create_returns_error_when_source_missing() {
        let (_dir, root) = setup();
        let ghost = root.join("nonexistent_source");
        let link = root.join("link");
        let result = create(&ghost, &link);
        assert!(result.is_err(), "expected error for missing source");
        if let Err(crate::Error::InternalFile(path, _)) = result {
            let valid = path == ghost || path == link;
            assert!(
                valid,
                "InternalFile path should reference source or link, got {}",
                path.display()
            );
        } else {
            panic!("expected Error::InternalFile");
        }
    }

    // ── create: cross-device error ───────────────────────────────────────────

    /// H5 (replaces create_or_copy): `create()` across filesystems must return
    /// a clear `CrossesDevices` error rather than silently degrading.
    ///
    /// Strategy: source lives under a tempdir in `/tmp` while the link
    /// destination lives in `/dev/shm` (a separate tmpfs). Device numbers are
    /// verified at runtime; the test self-skips if `/dev/shm` is absent or on
    /// the same device.
    #[cfg(target_os = "linux")]
    #[test]
    fn create_errors_on_cross_device() {
        use std::os::unix::fs::MetadataExt;

        let shm = std::path::Path::new("/dev/shm");
        if !shm.exists() {
            return; // /dev/shm not present — skip.
        }

        let shm_sub = shm.join(format!("ocx_hardlink_test_{}", std::process::id()));
        std::fs::create_dir_all(&shm_sub).unwrap();

        let (_dir, root) = setup();

        let dev_tmp = std::fs::metadata(&root).unwrap().dev();
        let dev_shm = std::fs::metadata(&shm_sub).unwrap().dev();

        if dev_tmp == dev_shm {
            let _ = std::fs::remove_dir_all(&shm_sub);
            return; // same device — can't exercise cross-device
        }

        let source = make_file(&root, "source", b"cross-device content");
        let link = shm_sub.join("link");

        let result = create(&source, &link);

        let _ = std::fs::remove_dir_all(&shm_sub);

        let err = result.expect_err("cross-device create must fail");
        match err {
            crate::Error::InternalFile(_, io_err) => {
                assert_eq!(
                    io_err.kind(),
                    std::io::ErrorKind::CrossesDevices,
                    "cross-device must surface CrossesDevices, got {:?}",
                    io_err.kind()
                );
            }
            other => panic!("expected Error::InternalFile, got: {other:?}"),
        }
    }

    // ── update ───────────────────────────────────────────────────────────────

    /// H6: update() when link already exists — old file is removed and new
    /// hardlink is placed pointing to the new source.
    #[test]
    fn update_replaces_existing_file() {
        let (_dir, root) = setup();
        let file_a = make_file(&root, "file_a.txt", b"version A");
        let file_b = make_file(&root, "file_b.txt", b"version B");
        let link = root.join("link.txt");

        // First create a hardlink to file_a
        create(&file_a, &link).unwrap();
        assert_eq!(std::fs::read(&link).unwrap(), b"version A");

        // Now update to point to file_b
        update(&file_b, &link).unwrap();

        assert!(link.exists(), "link must still exist after update()");
        assert_eq!(
            std::fs::read(&link).unwrap(),
            b"version B",
            "link must now contain file_b's content"
        );
    }

    /// update() when no link exists — should create the hardlink as if create()
    /// was called.
    #[test]
    fn update_creates_new_link_when_none_exists() {
        let (_dir, root) = setup();
        let source = make_file(&root, "source.txt", b"fresh content");
        let link = root.join("link.txt");

        // link does not exist yet — update() must handle this gracefully
        update(&source, &link).unwrap();

        assert!(link.exists(), "link must be created by update() when absent");
        assert_eq!(
            std::fs::read(&link).unwrap(),
            b"fresh content",
            "link must carry the source content"
        );
    }

    // ── inode sharing ────────────────────────────────────────────────────────

    /// H7: On Unix, a hardlink shares the same inode as the source file.
    /// Verified by comparing `metadata().ino()` for both paths.
    #[test]
    #[cfg(unix)]
    fn hardlink_shares_inode_on_unix() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        let source = make_file(&root, "source.txt", b"shared inode content");
        let link = root.join("link.txt");

        create(&source, &link).unwrap();

        let source_ino = std::fs::metadata(&source).unwrap().ino();
        let link_ino = std::fs::metadata(&link).unwrap().ino();

        assert_eq!(
            source_ino,
            link_ino,
            "source and hardlink must share the same inode (dev={} source_ino={} link_ino={})",
            std::fs::metadata(&source).unwrap().dev(),
            source_ino,
            link_ino
        );
    }

    // ── hardlinks survive rename (temp → packages invariant) ────────────────

    /// The walker assembles into a temp directory, then the pull pipeline
    /// atomically renames the temp dir to its final location in `packages/`.
    /// POSIX `rename(2)` is inode-preserving — the directory entry moves, the
    /// inode stays — so hardlinks created in the temp dir must remain hardlinks
    /// (with the same inode as the source in `layers/`) after the rename.
    /// This test locks that invariant in so a future regression is caught by
    /// a unit test rather than by an acceptance test.
    #[test]
    #[cfg(unix)]
    fn hardlink_survives_directory_rename() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, root) = setup();
        // Simulate a "layer" directory
        let layer_content = root.join("layer").join("content");
        std::fs::create_dir_all(&layer_content).unwrap();
        let source = layer_content.join("bin").join("tool");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"tool binary").unwrap();

        // Simulate a "temp package" directory
        let temp_pkg = root.join("temp").join("pkg");
        let temp_bin = temp_pkg.join("content").join("bin").join("tool");
        create(&source, &temp_bin).unwrap();
        let ino_before = std::fs::metadata(&temp_bin).unwrap().ino();
        assert_eq!(ino_before, std::fs::metadata(&source).unwrap().ino());

        // Atomically rename temp/pkg → final/pkg
        let final_pkg = root.join("final").join("pkg");
        std::fs::create_dir_all(final_pkg.parent().unwrap()).unwrap();
        std::fs::rename(&temp_pkg, &final_pkg).unwrap();

        let final_bin = final_pkg.join("content").join("bin").join("tool");
        let ino_after = std::fs::metadata(&final_bin).unwrap().ino();

        // Invariant: rename preserves the inode, so the hardlink to the layer
        // source is still intact after the move.
        assert_eq!(ino_before, ino_after, "rename must preserve inode");
        assert_eq!(
            ino_after,
            std::fs::metadata(&source).unwrap().ino(),
            "renamed file must still share its inode with the source in layers/"
        );
        assert_eq!(std::fs::read(&final_bin).unwrap(), b"tool binary");
    }

    // ── macOS-specific ───────────────────────────────────────────────────────

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;

        /// H8: macOS ad-hoc signing writes signature bytes into the file via the
        /// inode. A hardlink shares the inode, so signing either path signs both.
        ///
        /// We don't invoke the real `codesign` tool here — instead we verify the
        /// inode-equality property that the inheritance guarantee is founded on.
        /// If source and link share the same inode, any bytes written through
        /// one path (including a code signature) are immediately visible through
        /// the other.
        #[test]
        fn hardlink_inode_equal_on_macos() {
            use std::os::unix::fs::MetadataExt;

            let (_dir, root) = setup();
            let source = make_file(&root, "source", b"dummy binary content");
            let link = root.join("link");

            create(&source, &link).unwrap();

            let ino_source = std::fs::metadata(&source).unwrap().ino();
            let ino_link = std::fs::metadata(&link).unwrap().ino();
            assert_eq!(
                ino_source, ino_link,
                "macOS ad-hoc signing inherits through inode — this property requires shared \
                 inodes (source_ino={ino_source} link_ino={ino_link})"
            );
        }
    }

    // ── Windows-specific ─────────────────────────────────────────────────────

    #[cfg(windows)]
    mod windows {
        use super::*;

        /// W1: NTFS hardlink on the same volume succeeds without Developer Mode or
        /// elevated privileges. File contents are accessible through the link path.
        #[test]
        fn hardlink_on_same_ntfs_volume_succeeds() {
            let (_dir, root) = setup();
            let source = make_file(&root, "source", b"content");
            let link = root.join("link");

            create(&source, &link).unwrap();

            assert_eq!(
                std::fs::read(&link).unwrap(),
                b"content",
                "link path must contain the same bytes as the source"
            );
        }

        /// W3: On Windows, the inode equivalent is the (volume serial number,
        /// file index) pair returned by `MetadataExt::file_index()`.
        /// A hardlink on the same NTFS volume must share the file index with the
        /// source, proving they reference the same on-disk record.
        #[test]
        fn hardlink_preserves_file_index_on_ntfs() {
            use std::os::windows::fs::MetadataExt;

            let (_dir, root) = setup();
            let source = make_file(&root, "source", b"content");
            let link = root.join("link");

            create(&source, &link).unwrap();

            let m_source = std::fs::metadata(&source).unwrap();
            let m_link = std::fs::metadata(&link).unwrap();
            // Both paths live in the same tempdir so the volume serial number is
            // implicitly equal. Comparing file_index is sufficient to confirm
            // shared-inode semantics on NTFS.
            assert_eq!(
                m_source.file_index(),
                m_link.file_index(),
                "NTFS hardlink must share the FileId with its source"
            );
        }
    }
}
