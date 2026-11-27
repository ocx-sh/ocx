// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The single-file form of an export: a scratch directory in the home's `temp/`, and the archive that packs
//! an export directory into one file or unpacks one back.

use std::path::Path;

use ocx_store::file_structure::{FileStructure, LockedScratch};
use ocx_util::archive::Archive;

use crate::error::{Error, Result, file_error};

/// Writes `export_dir` as one archive at `target`, replacing a file already there only once it is complete.
#[expect(
    clippy::disallowed_methods,
    reason = "publishes a freshly built archive artifact at the path the caller named"
)]
pub(super) async fn pack(export_dir: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    // The directory is created exclusively beside the target, so the build never touches a path this run
    // did not create; the target's own file name inside it keeps the extension that selects the format.
    let holder = {
        let beside = parent.clone();
        tokio::task::spawn_blocking(move || tempfile::Builder::new().prefix("._tmp_").tempdir_in(&beside))
            .await
            .map_err(|join| Error::InternalFile(parent.clone(), std::io::Error::other(join)))?
            .map_err(|error| file_error(&parent, error))?
    };
    let temporary = holder.path().join(target.file_name().unwrap_or_default());

    let mut archive = Archive::create(&temporary).await?;
    archive.add_dir_all("", export_dir).await?;
    archive.finish().await?;

    tokio::fs::rename(&temporary, target)
        .await
        .map_err(|error| file_error(&temporary, error))?;
    Ok(())
}

/// Extracts `archive` into a fresh scratch directory.
pub(super) async fn unpack(file_structure: &FileStructure, archive: &Path) -> Result<LockedScratch> {
    let staged = file_structure.temp.scratch("import").await?;
    Archive::extract(archive, staged.path()).await?;
    Ok(staged)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;
    use crate::test_support::export_fixture::Package;

    /// Every regular file under `root`, relative and sorted, with its bytes.
    fn tree(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    out.push((
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        std::fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    fn file_structure(home: &Path) -> FileStructure {
        FileStructure::with_root(home.to_path_buf())
    }

    /// Whether the plain tar at `archive` holds the export marker as an entry.
    fn holds_marker(archive: &Path) -> bool {
        tar::Archive::new(std::fs::File::open(archive).unwrap())
            .entries()
            .unwrap()
            .any(|entry| {
                entry
                    .unwrap()
                    .path()
                    .unwrap()
                    .to_string_lossy()
                    .trim_start_matches("./")
                    == "ocx-export.json"
            })
    }

    fn temporaries_in(dir: &Path) -> Vec<std::ffi::OsString> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("._tmp_"))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pack_then_unpack_restores_the_tree_for_every_archive_name() {
        for name in ["bundle.tar", "bundle.zip", "bundle.tar.gz", "bundle"] {
            let work = TempDir::new().unwrap();
            let source = work.path().join("export");
            Package::new().write_export_dir(&source, true).await;
            let archive = work.path().join(name);

            pack(&source, &archive).await.unwrap();
            let staged = unpack(&file_structure(&work.path().join("home")), &archive)
                .await
                .unwrap();

            assert!(archive.is_file(), "{name}");
            assert!(!tree(&source).is_empty(), "{name}");
            assert_eq!(tree(staged.path()), tree(&source), "{name}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pack_replaces_an_existing_file_and_leaves_no_temporary() {
        let work = TempDir::new().unwrap();
        let source = work.path().join("export");
        Package::layerless().write_export_dir(&source, false).await;
        let archive = work.path().join("bundle.tar");
        std::fs::write(&archive, b"stale").unwrap();

        pack(&source, &archive).await.unwrap();

        assert!(holds_marker(&archive));
        assert_eq!(temporaries_in(work.path()), Vec::<std::ffi::OsString>::new());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pack_leaves_a_directory_at_the_old_temporary_name_alone() {
        let work = TempDir::new().unwrap();
        let source = work.path().join("export");
        Package::layerless().write_export_dir(&source, false).await;
        let bystander = work.path().join("._tmp_bundle.tar");
        std::fs::create_dir(&bystander).unwrap();
        std::fs::write(bystander.join("keep.txt"), b"mine").unwrap();
        let archive = work.path().join("bundle.tar");

        pack(&source, &archive).await.unwrap();

        assert_eq!(std::fs::read(bystander.join("keep.txt")).unwrap(), b"mine");
        assert!(holds_marker(&archive));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_packs_onto_one_target_leave_one_readable_archive() {
        let work = TempDir::new().unwrap();
        let source = work.path().join("export");
        Package::new().write_export_dir(&source, true).await;
        let archive = work.path().join("bundle.tar");

        let runs: Vec<_> = (0..8)
            .map(|_| {
                let (source, archive) = (source.clone(), archive.clone());
                tokio::spawn(async move { pack(&source, &archive).await })
            })
            .collect();
        for run in runs {
            run.await.unwrap().unwrap();
        }

        assert!(holds_marker(&archive));
        let staged = unpack(&file_structure(&work.path().join("home")), &archive)
            .await
            .unwrap();
        assert_eq!(tree(staged.path()), tree(&source));
        assert_eq!(temporaries_in(work.path()), Vec::<std::ffi::OsString>::new());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_pack_leaves_the_existing_file_and_no_temporary() {
        let work = TempDir::new().unwrap();
        let archive = work.path().join("bundle.tar");
        std::fs::write(&archive, b"previous").unwrap();

        let result = pack(&work.path().join("missing-export"), &archive).await;

        assert!(result.is_err(), "a missing source cannot be packed");
        assert_eq!(std::fs::read(&archive).unwrap(), b"previous");
        assert_eq!(temporaries_in(work.path()), Vec::<std::ffi::OsString>::new());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpack_refuses_a_file_that_is_not_an_archive() {
        let work = TempDir::new().unwrap();
        let garbage = work.path().join("garbage.tar");
        std::fs::write(&garbage, b"this is not a tar archive at all, just text").unwrap();

        let result = unpack(&file_structure(&work.path().join("home")), &garbage).await;

        assert!(result.is_err(), "garbage bytes must not unpack");
    }
}
