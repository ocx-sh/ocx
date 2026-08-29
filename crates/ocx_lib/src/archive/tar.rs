// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::Result;

use super::backend::Backend;
use super::error::Error;

use crate::cli::progress::LOG_INTERVAL;

pub(super) struct TarBackend {
    inner: Arc<Mutex<tar::Builder<Box<dyn Write + Send>>>>,
}

impl TarBackend {
    pub fn new(writer: Box<dyn Write + Send>) -> Self {
        let mut builder = tar::Builder::new(writer);
        builder.follow_symlinks(false);
        // Deterministic headers: zero uid/gid/mtime/uname/gname. Without this, every
        // archive embeds the build user's uid and the current mtime, breaking byte-for-byte
        // reproducibility and producing files owned by a stale uid after extraction.
        builder.mode(tar::HeaderMode::Deterministic);
        Self {
            inner: Arc::new(Mutex::new(builder)),
        }
    }

    /// Locks the builder on a blocking thread, runs `f`, and releases the lock.
    async fn run_blocking<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut tar::Builder<Box<dyn Write + Send>>) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(Error::internal)?
    }
}

#[async_trait::async_trait]
impl Backend for TarBackend {
    async fn add_file(&mut self, archive_path: PathBuf, file: PathBuf) -> Result<()> {
        self.run_blocking(move |builder| {
            let mut f = std::fs::File::open(&file).map_err(|e| Error::Io { path: file, source: e })?;
            builder.append_file(&archive_path, &mut f).map_err(Error::Tar)?;
            Ok(())
        })
        .await
    }

    async fn add_dir(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()> {
        self.run_blocking(move |builder| Ok(builder.append_dir(&archive_path, &dir).map_err(Error::Tar)?))
            .await
    }

    async fn add_dir_all(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()> {
        self.run_blocking(move |builder| {
            let mut count = 0u64;
            add_dir_recursive(builder, &archive_path, &dir, &mut count)?;
            tracing::debug!("Bundled {count} entries total");
            Ok(())
        })
        .await
    }

    async fn finish(self: Box<Self>) -> Result<()> {
        let Ok(mutex) = Arc::try_unwrap(self.inner) else {
            panic!("backend has outstanding references");
        };
        let mut builder = mutex.into_inner().unwrap_or_else(|e| e.into_inner());
        tokio::task::spawn_blocking(move || {
            builder.finish().map_err(Error::Tar)?;
            builder.into_inner().map_err(Error::Tar)?.flush().map_err(Error::Tar)?;
            Ok(())
        })
        .await
        .map_err(Error::internal)?
    }
}

fn add_dir_recursive(
    builder: &mut tar::Builder<Box<dyn Write + Send>>,
    base_path: &Path,
    dir: &Path,
    count: &mut u64,
) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| Error::Io {
            path: dir.to_path_buf(),
            source: e,
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let archive_name = if base_path.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            base_path.join(&name)
        };

        builder
            .append_path_with_name(&path, &archive_name)
            .map_err(Error::Tar)?;

        let ft = entry.file_type().map_err(|e| Error::Io {
            path: path.clone(),
            source: e,
        })?;
        if ft.is_dir() {
            add_dir_recursive(builder, &archive_name, &path, count)?;
        }

        *count += 1;
        tracing::trace!("Adding {}", archive_name.display());
        if (*count).is_multiple_of(LOG_INTERVAL) {
            tracing::debug!("Bundled {} entries", *count);
        }
    }
    Ok(())
}

/// Extract a tar archive from `reader` to `output`, applying `strip_components`.
///
/// Returns an error on path-escape, I/O failure, or malformed archive.
pub(super) fn extract(reader: impl std::io::Read, output: &std::path::Path, strip_components: usize) -> Result<()> {
    extract_returning_reader(reader, output, strip_components).0
}

/// Like [`extract`] but returns the reader after extraction alongside the result.
///
/// Enables callers that wrapped the reader in a digest-accumulating or
/// progress-tracking adapter to recover their state after the tar extractor
/// has consumed the stream. The reader may be partially consumed on error.
pub(super) fn extract_returning_reader<R: std::io::Read>(
    reader: R,
    output: &std::path::Path,
    strip_components: usize,
) -> (Result<()>, R) {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);

    let result = extract_from_archive(&mut archive, output, strip_components);

    // Recover the reader from the archive regardless of whether extraction
    // succeeded or failed. This allows callers to finalize digest state.
    let reader = archive.into_inner();
    (result, reader)
}

/// Internal extraction loop shared by `extract` and `extract_returning_reader`.
fn extract_from_archive<R: std::io::Read>(
    archive: &mut tar::Archive<R>,
    output: &std::path::Path,
    strip_components: usize,
) -> Result<()> {
    let mut count = 0u64;
    // Remember only the most recent parent passed to `create_dir_all`. Tar
    // archives list entries depth-first, so the same parent recurs across many
    // consecutive entries; without this guard every file re-issues a
    // `create_dir_all` (an N+1 syscall pattern). A single-slot guard collapses
    // that run of duplicates with O(1) memory — unlike a whole-archive
    // `HashSet`, whose size would be attacker-controlled by directory fan-out
    // (memory-amplification surface). `create_dir_all` is idempotent, so an
    // interleaved-parent layout merely re-issues a harmless syscall, never a
    // wrong result.
    let mut last_parent: Option<PathBuf> = None;
    for entry in archive.entries().map_err(Error::Tar)? {
        let mut entry = entry.map_err(Error::Tar)?;
        let path = entry.path().map_err(Error::Tar)?.to_path_buf();
        let stripped: std::path::PathBuf = path.iter().skip(strip_components).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        // Reject entries whose path escapes the output root.
        if stripped.is_absolute() || crate::utility::fs::path::escapes_root(&stripped) {
            return Err(Error::EntryEscape(path).into());
        }

        let output_path = output.join(&stripped);

        if let Some(parent) = output_path.parent()
            && last_parent.as_deref() != Some(parent)
        {
            std::fs::create_dir_all(parent).map_err(|e| Error::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
            last_parent = Some(parent.to_path_buf());
        }

        if entry.header().entry_type() == tar::EntryType::Symlink {
            if let Some(target) = entry.link_name().map_err(Error::Tar)? {
                crate::symlink::validate_target(output, &output_path, target.as_ref())?;
                crate::symlink::create(target.as_ref(), &output_path)?;
            }
        } else if entry.header().entry_type() == tar::EntryType::Link {
            let target = entry.link_name().map_err(Error::Tar)?.unwrap_or_default().into_owned();
            let source =
                resolve_hard_link_source(output, &target, strip_components).ok_or_else(|| Error::HardLinkEscape {
                    link: stripped.clone(),
                    target: target.clone(),
                })?;
            std::fs::hard_link(&source, &output_path).map_err(|e| Error::Io {
                path: output_path.clone(),
                source: e,
            })?;
        } else {
            // QW2 deferred: wrapping the regular-file write in a BufWriter is not
            // achievable cleanly with tar 0.4.46. `Entry::unpack`/`unpack_in`
            // always open their own unbuffered `File`, and the only public way to
            // apply the header's permission/ownership/mtime bits (set_perms_ownerships
            // is crate-private) and to honour sparse-file padding is to let `unpack`
            // own the write. A manual BufWriter copy would drop the executable bit
            // asserted by test_executable_bit_preserved_through_round_trip, so the
            // buffering quick win is left for an upstream tar API that exposes
            // "unpack into a provided writer".
            entry.unpack(&output_path).map_err(Error::Tar)?;
        }

        count += 1;
        tracing::trace!("Extracted {}", stripped.display());
        if count.is_multiple_of(LOG_INTERVAL) {
            tracing::debug!("Extracted {count} entries");
        }
    }
    tracing::debug!("Extracted {count} entries total");

    Ok(())
}

/// Resolves a hard-link entry's link name to an existing path inside `output`.
///
/// Returns `None` when the link name is absent, is emptied by `strip_components`,
/// or does not resolve to a file inside `output`.
///
/// `tar::Entry::unpack` cannot be trusted with hard links: it calls
/// `fields.unpack(None, dst)`, and with `target_base: None` the hard-link branch
/// hands the archive's raw link name to `fs::hard_link` verbatim (tar 0.4.46,
/// `src/entry.rs`). That is wrong in both directions — an absolute link name
/// pulls any host file the extracting user can read into the tree as an ordinary
/// regular file (invisible to every symlink guard, so it gets bundled and
/// published), and a relative one resolves against the process CWD instead of
/// `output`, which makes ordinary GNU-tar-deduplicated archives fail to extract.
fn resolve_hard_link_source(output: &Path, link_name: &Path, strip_components: usize) -> Option<PathBuf> {
    // The link name addresses an earlier entry by its path *within the archive*,
    // so it takes the same `strip_components` transform as the entry paths.
    let stripped: PathBuf = link_name.iter().skip(strip_components).collect();
    if stripped.as_os_str().is_empty() {
        return None;
    }
    let candidate = crate::utility::fs::path::join_under_root(output, &stripped).ok()?;
    // `join_under_root` is lexical, and the extraction root need not be empty:
    // an intermediate component that is itself a symlink can collapse a
    // declared in-root path onto a real out-of-root file. The source has to
    // exist already for `hard_link` to succeed, so resolve it for real and
    // re-check — the same containment argument tar's own `validate_inside_dst`
    // makes for `unpack_in`.
    let source = dunce::canonicalize(&candidate).ok()?;
    let root = dunce::canonicalize(output).ok()?;
    source.starts_with(&root).then_some(source)
}

#[cfg(test)]
mod tests {
    use crate::archive::Archive;

    /// Builds a tar containing `dir/original.txt` plus a hard-link entry
    /// `dir/alias.txt` whose link name is `link_target`, and extracts it.
    async fn extract_with_hard_link(link_target: &str) -> (tempfile::TempDir, crate::Result<()>) {
        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let body = b"original contents";
            let mut header = ::tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, "dir/original.txt", &body[..]).unwrap();

            let mut link = ::tar::Header::new_gnu();
            link.set_entry_type(::tar::EntryType::Link);
            link.set_size(0);
            link.set_mode(0o644);
            builder.append_link(&mut link, "dir/alias.txt", link_target).unwrap();
            builder.finish().unwrap();
        }
        let extract_dir = tempfile::tempdir().unwrap();
        let result = Archive::extract(&archive_path, extract_dir.path()).await;
        (extract_dir, result)
    }

    /// Regression for #275: a hard-link entry whose link name points outside the
    /// extraction root must fail the run. `tar::Entry::unpack` passes the raw link
    /// name to `fs::hard_link`, so an absolute name links a host file into the tree
    /// as an ordinary regular file — no symlink guard can see it afterwards, and it
    /// would be bundled and published under the attacker's chosen name.
    #[tokio::test]
    async fn hard_link_target_outside_the_root_is_rejected() {
        let secret_dir = tempfile::tempdir().unwrap();
        let secret = secret_dir.path().join("secret");
        std::fs::write(&secret, b"host secret").unwrap();

        for target in [secret.to_str().unwrap(), "../../etc/passwd"] {
            let (extract_dir, result) = extract_with_hard_link(target).await;
            let err = result.unwrap_err();
            assert!(
                matches!(err, crate::Error::Archive(crate::archive::Error::HardLinkEscape { .. })),
                "link name {target:?} was not rejected as an escape: {err}"
            );
            assert!(
                !extract_dir.path().join("dir/alias.txt").exists(),
                "link name {target:?} still produced an entry in the tree"
            );
        }
    }

    /// Regression for #275: an ordinary GNU-tar-deduplicated archive — two identical
    /// files stored once, the second as a hard link — must extract. Passing the raw
    /// link name to `fs::hard_link` resolved it against the process CWD instead of
    /// the extraction root, so these archives (Kibana's release tarballs among them)
    /// failed outright.
    #[tokio::test]
    async fn legitimate_in_tree_hard_link_extracts() {
        let (extract_dir, result) = extract_with_hard_link("dir/original.txt").await;
        result.unwrap();

        let alias = extract_dir.path().join("dir/alias.txt");
        assert_eq!(std::fs::read(&alias).unwrap(), b"original contents");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let original = extract_dir.path().join("dir/original.txt");
            assert_eq!(
                std::fs::metadata(&alias).unwrap().ino(),
                std::fs::metadata(&original).unwrap().ino(),
                "alias is a copy, not a hard link"
            );
        }
    }

    /// Regression: tar archives must not embed the build host's ownership or per-file
    /// mtimes. Without `HeaderMode::Deterministic` every entry carries the build user's
    /// uid/gid and the source file's mtime, breaking byte-reproducibility and producing
    /// files owned by a stale uid after extraction on a different machine. The tar crate
    /// uses a fixed non-zero constant for mtime to work around tools that mishandle a
    /// zero timestamp (see rust-lang/cargo#9512), so we assert mtime is uniform across
    /// entries — not derived from the source filesystem.
    #[tokio::test]
    async fn test_headers_have_zero_ownership_and_constant_mtime() {
        let src = tempfile::tempdir().unwrap();
        let nested = src.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(src.path().join("top.txt"), b"top").unwrap();
        std::fs::write(nested.join("inner.txt"), b"inner").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive.add_dir_all("", src.path()).await.unwrap();
        archive.finish().await.unwrap();

        let file = std::fs::File::open(&archive_path).unwrap();
        let mut tar = ::tar::Archive::new(file);
        let mut entry_count = 0;
        let mut first_mtime: Option<u64> = None;
        for entry in tar.entries().unwrap() {
            let entry = entry.unwrap();
            let header = entry.header();
            let path = entry.path().unwrap().to_path_buf();
            assert_eq!(header.uid().unwrap(), 0, "uid not zeroed on {path:?}");
            assert_eq!(header.gid().unwrap(), 0, "gid not zeroed on {path:?}");
            assert_eq!(
                header.username().unwrap().unwrap_or(""),
                "",
                "uname not cleared on {path:?}"
            );
            assert_eq!(
                header.groupname().unwrap().unwrap_or(""),
                "",
                "gname not cleared on {path:?}"
            );
            let mtime = header.mtime().unwrap();
            match first_mtime {
                None => first_mtime = Some(mtime),
                Some(expected) => assert_eq!(
                    mtime, expected,
                    "mtime varies across entries (source mtime leaked) on {path:?}"
                ),
            }
            entry_count += 1;
        }
        assert!(entry_count >= 2, "expected at least 2 entries, got {entry_count}");
    }

    /// Regression: `HeaderMode::Deterministic` normalizes mode bits but must still
    /// propagate the user-execute bit so distributed binaries remain runnable after
    /// extraction. Regular files land at 0o644, executables at 0o755.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_executable_bit_preserved_through_round_trip() {
        use std::os::unix::fs::PermissionsExt;

        let src = tempfile::tempdir().unwrap();
        let bin = src.path().join("tool");
        let data = src.path().join("data.txt");
        std::fs::write(&bin, b"#!/bin/sh\necho hi").unwrap();
        std::fs::write(&data, b"plain").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o644)).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive.add_dir_all("", src.path()).await.unwrap();
        archive.finish().await.unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path()).await.unwrap();

        let bin_mode = extract_dir.path().join("tool").metadata().unwrap().permissions().mode() & 0o777;
        let data_mode = extract_dir
            .path()
            .join("data.txt")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(bin_mode, 0o755, "executable bit lost through round-trip");
        assert_eq!(data_mode, 0o644, "regular file mode not normalized to 0o644");
    }

    /// Regression: identical source trees produce byte-identical tar archives across
    /// invocations. Confirms determinism end-to-end.
    #[tokio::test]
    async fn test_archive_bytes_are_reproducible() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a.txt"), b"alpha").unwrap();
        std::fs::write(src.path().join("b.txt"), b"beta").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let first = out_dir.path().join("first.tar");
        let second = out_dir.path().join("second.tar");

        for path in [&first, &second] {
            let mut archive = Archive::create(path).await.unwrap();
            archive.add_dir_all("", src.path()).await.unwrap();
            archive.finish().await.unwrap();
        }

        let bytes_first = std::fs::read(&first).unwrap();
        let bytes_second = std::fs::read(&second).unwrap();
        assert_eq!(
            bytes_first, bytes_second,
            "two runs over the same source tree produced different archive bytes"
        );
    }
}
