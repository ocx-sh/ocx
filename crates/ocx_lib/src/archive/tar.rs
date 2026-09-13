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

/// Extract a tar archive from `reader` to `output`, applying `strip_components`,
/// and return the reader after extraction alongside the result.
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
    // The extraction root must exist before entries land and before the
    // canonical-root resolution below; a top-level file entry would otherwise
    // create it only lazily via `create_dir_all(parent)`.
    std::fs::create_dir_all(output).map_err(|e| Error::Io {
        path: output.to_path_buf(),
        source: e,
    })?;
    // Resolve the root once (following any symlink in the root's own ancestry)
    // so every per-entry containment check compares against a real path.
    let canonical_root = dunce::canonicalize(output).map_err(|e| Error::Io {
        path: output.to_path_buf(),
        source: e,
    })?;
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

        // Refuse GNU sparse entries (CWE-400). tar-rs materializes a sparse hole
        // with `seek`+`set_len`, consuming NO bytes from the tar stream, so the
        // decompressed-stream cap (`reader.take(cap + 1)`) never trips and a
        // single stored byte can claim an arbitrary apparent size on disk. OCX's
        // own bundler emits only regular entries (`append_file`/`append_dir_all`)
        // and OCI image layers do not use the GNU sparse extension, so refusing
        // outright closes the bypass without rejecting anything ocx produces or
        // pulls. (`is_gnu_sparse` is the `b'S'` typeflag — the only shape tar-rs
        // routes through its hole-materializing path.)
        if entry.header().entry_type().is_gnu_sparse() {
            return Err(Error::GnuSparseUnsupported(path).into());
        }

        let stripped: std::path::PathBuf = path.iter().skip(strip_components).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        // Join under the root, folding `.`/`..` lexically and rejecting an entry
        // that escapes it (absolute, Windows-prefixed, or `..`-escaping). This
        // keeps the raw `..` out of the write path — `output.join(&stripped)`
        // would have preserved it for `entry.unpack` to resolve physically.
        let out = crate::utility::fs::path::join_under_root(&canonical_root, &stripped)
            .map_err(|_| Error::EntryEscape(path.clone()))?;

        // A `.` / `a/..` entry normalizes to the root itself — the
        // self-referential `./` entry a `tar -C dir .` archive lists first. It
        // names no new content and its parent is the root's parent (legitimately
        // outside the root), so skip it rather than treating it as an escape.
        if out == canonical_root {
            continue;
        }

        // Physical containment BEFORE creating anything (CWE-22): refuse if any
        // existing ancestor of `out` below the root is a symlink. An earlier
        // entry can plant a symlink — in this entry's parent chain, or one its
        // own target reaches through — that redirects a later `create_dir_all` /
        // `unpack` outside the root; the mkdir must be refused *before* it
        // happens, not after (a check that ran post-`create_dir_all` has already
        // let the directory escape through the planted link). `Some(canonical_root)`
        // trusts the root's own ancestry (a symlinked `$OCX_HOME`) and scopes the
        // walk to the untrusted portion strictly below it. Every component created
        // below is then a fresh directory — which cannot be a symlink — so `out`
        // stays physically contained and is safe to act on by its own path.
        crate::utility::fs::refuse_if_symlink_in_path_sync(&out, Some(&canonical_root)).map_err(|e| match e {
            crate::utility::fs::SymlinkWalkError::Ancestor { .. } => Error::EntryEscape(path.clone()),
            // A component we could not even stat is an I/O fault (74), not a
            // containment verdict (65).
            crate::utility::fs::SymlinkWalkError::Io { path, source } => Error::Io { path, source },
        })?;

        if let Some(parent) = out.parent()
            && last_parent.as_deref() != Some(parent)
        {
            crate::archive::create_dir_all_capped(&canonical_root, parent)?;
            last_parent = Some(parent.to_path_buf());
        }

        let dst = out;

        if entry.header().entry_type() == tar::EntryType::Symlink {
            if let Some(target) = entry.link_name().map_err(Error::Tar)? {
                crate::symlink::validate_target(&canonical_root, &dst, target.as_ref())?;
                crate::symlink::create(target.as_ref(), &dst)?;
            }
        } else if entry.header().entry_type() == tar::EntryType::Link {
            let target = entry.link_name().map_err(Error::Tar)?.unwrap_or_default().into_owned();
            let source = resolve_hard_link_source(&canonical_root, &target, strip_components).ok_or_else(|| {
                Error::HardLinkEscape {
                    link: stripped.clone(),
                    target: target.clone(),
                }
            })?;
            std::fs::hard_link(&source, &dst).map_err(|e| Error::Io {
                path: dst.clone(),
                source: e,
            })?;
        } else {
            // Capture the archived mode before `unpack` so the cap can be applied
            // afterwards. `set_preserve_permissions(true)` makes `unpack` write
            // the raw 0o7777 mode, so setuid/setgid/sticky and group/other write
            // would otherwise survive into the extracted tree.
            #[cfg(unix)]
            let archived_mode = entry.header().mode().ok();
            // QW2 deferred: wrapping the regular-file write in a BufWriter is not
            // achievable cleanly with tar 0.4.46. `Entry::unpack`/`unpack_in`
            // always open their own unbuffered `File`, and the only public way to
            // apply the header's permission/ownership/mtime bits (set_perms_ownerships
            // is crate-private) and to honour sparse-file padding is to let `unpack`
            // own the write. A manual BufWriter copy would drop the executable bit
            // asserted by test_executable_bit_preserved_through_round_trip, so the
            // buffering quick win is left for an upstream tar API that exposes
            // "unpack into a provided writer".
            //
            // `unpack` writes to `dst` — the lexical join, whose every existing
            // ancestor the guard above proved is not a symlink — and never follows
            // a symlink at the final component: for a regular file it removes any
            // pre-existing file first, and refuses a directory. The mode cap below
            // is therefore safe against a planted final-component symlink.
            let unpacked = entry.unpack(&dst).map_err(Error::Tar)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Cap the archived mode at the single site that writes it: strip
                // setuid/setgid/sticky (0o7000) and group/other write (0o022).
                // Read and execute bits — the ones an archive legitimately carries
                // — pass through untouched, so the executable bit survives the
                // round trip. Applied to an entry that landed a regular file
                // (`Unpacked::File`) OR a directory, whose raw header mode `unpack`
                // wrote itself (`preserve_permissions`, no mask). Directory-ness is
                // read from the written `dst`, NOT the header typeflag: tar-rs's
                // old-BSD path unpacks a non-ustar header whose name ends in `/` as
                // a directory even under a regular typeflag, so a typeflag check
                // would miss it and leave `0o2777` intact. A metadata-only entry
                // (`pax_global_header`) creates nothing, so `dst` is absent and it
                // is correctly skipped — chmod on a missing path would fail the run.
                let landed_directory = std::fs::symlink_metadata(&dst).map(|m| m.is_dir()).unwrap_or(false);
                if let Some(mode) = archived_mode
                    && (matches!(unpacked, tar::Unpacked::File(_)) || landed_directory)
                {
                    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode & !(0o7000 | 0o022))).map_err(
                        |e| Error::Io {
                            path: dst.clone(),
                            source: e,
                        },
                    )?;
                }
            }
            #[cfg(not(unix))]
            let _ = unpacked;
        }

        count += 1;
        tracing::trace!("Extracted {}", stripped.display());
        if count.is_multiple_of(LOG_INTERVAL) {
            tracing::debug!("Extracted {count} entries");
        }
    }
    tracing::debug!("Extracted {count} entries total");

    // Every hop exists now; re-judge each link against the finished tree (see
    // `sweep_symlinks` for the reverse-order ladder this closes).
    crate::archive::sweep_symlinks(&canonical_root)?;

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

    /// Regression for B1: a symlink entry that lands in-root, followed by a file
    /// entry that traverses `..` *through* that symlink, must not write outside
    /// the extraction root. `output.join(raw)` preserved the `..` for the kernel
    /// to resolve physically through the symlink; joining the *normalized* path
    /// keeps the traversal in-root.
    #[tokio::test]
    async fn symlink_traversal_does_not_escape_root() {
        let scratch = tempfile::tempdir().unwrap();
        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);

            // Entry 1: a symlink five levels deep that lands exactly on the root.
            let mut link = ::tar::Header::new_gnu();
            link.set_entry_type(::tar::EntryType::Symlink);
            link.set_size(0);
            link.set_mode(0o777);
            builder
                .append_link(&mut link, "a/b/c/d/e/link", "../../../../..")
                .unwrap();

            // Entry 2: a file whose path traverses `..` through that symlink.
            // Physically that resolves to `output/..`; lexically it normalizes
            // back to `a/b/c/d/e/PWNED`, in-root. `append_data`/`set_path` refuse
            // a `..` path, so write the name field directly — a real hostile
            // archive carries the raw `..` that the extractor must handle.
            let body = b"pwned";
            let name = b"a/b/c/d/e/link/../PWNED";
            let mut header = ::tar::Header::new_gnu();
            header.set_entry_type(::tar::EntryType::Regular);
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            builder.append(&header, &body[..]).unwrap();
            builder.finish().unwrap();
        }

        let output = scratch.path().join("out");
        let result = Archive::extract(&archive_path, &output).await;

        // The escape target is `output/..` = `scratch`; it must never appear.
        assert!(
            !scratch.path().join("PWNED").exists(),
            "the file entry escaped the extraction root through the symlink"
        );
        assert!(
            result.is_ok(),
            "the neutralized entry should extract in-root: {result:?}"
        );
    }

    /// Regression for B1: the physical containment guard rejects a write whose
    /// parent resolves outside the root through a symlink already present in the
    /// output tree — the case lexical normalization cannot see.
    #[cfg(unix)]
    #[tokio::test]
    async fn write_through_pre_existing_symlink_is_rejected() {
        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let output = scratch.path().join("out");
        std::fs::create_dir_all(&output).unwrap();
        std::os::unix::fs::symlink(outside.path(), output.join("evil")).unwrap();

        let archive_path = scratch.path().join("pkg.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let body = b"pwned";
            let mut header = ::tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, "evil/PWNED", &body[..]).unwrap();
            builder.finish().unwrap();
        }

        let error = Archive::extract(&archive_path, &output).await.unwrap_err();
        assert!(
            matches!(error, crate::Error::Archive(crate::archive::Error::EntryEscape(_))),
            "a write resolving outside the root must be rejected: {error}"
        );
        assert!(
            !outside.path().join("PWNED").exists(),
            "the file was written through the symlink to outside the root"
        );
    }

    /// Regression for D8: `set_preserve_permissions(true)` applies the raw
    /// 0o7777 mode, so a `0o4755` (setuid) entry would extract setuid. The mask
    /// strips setuid/setgid/sticky, landing it at 0o755.
    #[cfg(unix)]
    #[tokio::test]
    async fn extract_masks_setuid_bit() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::tempdir().unwrap();
        let archive_path = scratch.path().join("pkg.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let body = b"#!/bin/sh\n";
            let mut header = ::tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o4755);
            header.set_cksum();
            builder.append_data(&mut header, "setuid-tool", &body[..]).unwrap();
            builder.finish().unwrap();
        }

        let output = scratch.path().join("out");
        Archive::extract(&archive_path, &output).await.unwrap();

        let mode = std::fs::metadata(output.join("setuid-tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o755, "setuid bit was not stripped from the extracted file");
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

    /// Block 2 exploit — the planted symlink is in the link's PARENT chain: entry
    /// 1 (`a` -> `.`) makes `a` an in-root symlink, then entry 2 (`a/evil` ->
    /// `../outside`) plants an escaping symlink whose parent traverses it. The
    /// extractor's pre-create ancestor-symlink refusal rejects entry 2 (its parent
    /// `a` is a planted symlink) before it can be created, and `validate_target`
    /// independently refuses the escaping target — so no escaping symlink is
    /// planted inside the root. (The target-in-the-component sibling of this
    /// attack is covered by `tar_target_symlink_traversal_cannot_mkdir_outside_root`
    /// and `tar_published_symlink_target_cannot_escape_root`.)
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_chain_cannot_plant_an_escaping_symlink() {
        let scratch = tempfile::tempdir().unwrap();
        // A file one level above the extraction root that the planted symlink
        // would point at; its presence lets `canonicalize` resolve the link.
        std::fs::write(scratch.path().join("outside"), b"original").unwrap();

        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);

            // Entry 1: `a` -> `.` — physically lands the "directory" back on root.
            let mut hop = ::tar::Header::new_gnu();
            hop.set_entry_type(::tar::EntryType::Symlink);
            hop.set_size(0);
            hop.set_mode(0o777);
            builder.append_link(&mut hop, "a", ".").unwrap();

            // Entry 2: `a/evil` -> `../outside`. Lexically its parent is `a`
            // (depth 1) so `../outside` normalizes to the in-root `outside`;
            // physically its parent is the root, so the link escapes upward.
            let mut evil = ::tar::Header::new_gnu();
            evil.set_entry_type(::tar::EntryType::Symlink);
            evil.set_size(0);
            evil.set_mode(0o777);
            builder.append_link(&mut evil, "a/evil", "../outside").unwrap();
            builder.finish().unwrap();
        }

        let output = scratch.path().join("out");
        let result = Archive::extract(&archive_path, &output).await;

        let canonical_root = dunce::canonicalize(&output).unwrap();
        let planted = output.join("evil");
        let escaped = std::fs::symlink_metadata(&planted)
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(false)
            && dunce::canonicalize(&planted)
                .map(|resolved| !resolved.starts_with(&canonical_root))
                .unwrap_or(false);
        assert!(!escaped, "an escaping symlink was planted inside the extraction root");
        assert!(
            result.is_err(),
            "the escaping symlink entry must be refused: {result:?}"
        );
    }

    /// Block 3 regression: a `tar -C dir .` archive begins with a `./` directory
    /// entry that normalizes to the root itself. It must be skipped, not treated
    /// as an escape — its parent is the root's parent, which is (correctly) not
    /// inside the root. Real-world release tarballs carry this layout.
    #[tokio::test]
    async fn dot_rooted_archive_extracts() {
        let scratch = tempfile::tempdir().unwrap();
        let archive_path = scratch.path().join("dot.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);

            // `./` — the self-referential root entry GNU tar writes first.
            let mut dir = ::tar::Header::new_gnu();
            dir.set_entry_type(::tar::EntryType::Directory);
            dir.set_size(0);
            dir.set_mode(0o755);
            dir.set_mtime(0);
            dir.as_gnu_mut().unwrap().name[..2].copy_from_slice(b"./");
            dir.set_cksum();
            builder.append(&dir, &b""[..]).unwrap();

            // `./file` — a normal file, dot-prefixed as GNU tar writes them.
            let body = b"content";
            let mut file_header = ::tar::Header::new_gnu();
            file_header.set_entry_type(::tar::EntryType::Regular);
            file_header.set_size(body.len() as u64);
            file_header.set_mode(0o644);
            file_header.set_mtime(0);
            file_header.as_gnu_mut().unwrap().name[..6].copy_from_slice(b"./file");
            file_header.set_cksum();
            builder.append(&file_header, &body[..]).unwrap();
            builder.finish().unwrap();
        }

        let output = scratch.path().join("out");
        Archive::extract(&archive_path, &output)
            .await
            .expect("a dot-rooted archive must extract");
        assert_eq!(
            std::fs::read(output.join("file")).unwrap(),
            b"content",
            "the dot-prefixed file entry must land under the root"
        );
    }

    /// Block 4 regression: a `pax_global_header` entry (git-archive writes one
    /// first) creates nothing on `unpack`. The mode cap must be gated on the
    /// entry having actually produced a file, or it chmods a path that does not
    /// exist and fails the whole extraction.
    #[tokio::test]
    async fn pax_global_header_does_not_break_extraction() {
        let scratch = tempfile::tempdir().unwrap();
        let archive_path = scratch.path().join("git.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);

            let pax = b"52 comment=0000000000000000000000000000000000000000\n";
            let mut global = ::tar::Header::new_gnu();
            global.set_entry_type(::tar::EntryType::XGlobalHeader);
            global.set_size(pax.len() as u64);
            global.set_mode(0o644);
            global.set_mtime(0);
            global.as_gnu_mut().unwrap().name[..17].copy_from_slice(b"pax_global_header");
            global.set_cksum();
            builder.append(&global, &pax[..]).unwrap();

            let body = b"real";
            let mut file_header = ::tar::Header::new_gnu();
            file_header.set_size(body.len() as u64);
            file_header.set_mode(0o644);
            file_header.set_cksum();
            builder.append_data(&mut file_header, "real.txt", &body[..]).unwrap();
            builder.finish().unwrap();
        }

        let output = scratch.path().join("out");
        Archive::extract(&archive_path, &output)
            .await
            .expect("a pax global header entry must not break extraction");
        assert_eq!(std::fs::read(output.join("real.txt")).unwrap(), b"real");
    }

    /// R1 regression: the D8 mode cap must cover directory entries too. `unpack`
    /// applies a directory's raw header mode itself (`preserve_permissions`, no
    /// mask), so a `0o2777` directory would extract setgid + world-writable on
    /// the install path (which writes trees verbatim, unlike the bundler's
    /// `HeaderMode::Deterministic`). Built with a raw `Builder` — not the ocx
    /// bundler — so the hostile mode reaches the extractor; asserted through the
    /// extract path, where the cap lives.
    #[cfg(unix)]
    #[tokio::test]
    async fn extract_masks_directory_mode() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::tempdir().unwrap();
        let archive_path = scratch.path().join("dir.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let mut header = ::tar::Header::new_gnu();
            header.set_entry_type(::tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o2777); // setgid + rwxrwxrwx
            header.set_mtime(0);
            header.set_path("wide-dir").unwrap();
            header.set_cksum();
            builder.append(&header, &b""[..]).unwrap();
            builder.finish().unwrap();
        }

        let output = scratch.path().join("out");
        Archive::extract(&archive_path, &output).await.unwrap();

        let mode = std::fs::metadata(output.join("wide-dir")).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o755, "directory mode not capped: setgid/world-write survived");
    }

    /// R1 residual: tar-rs unpacks a non-ustar header whose name ends in `/` as
    /// a directory even under a regular (`b'0'`) typeflag (its old-BSD path). The
    /// mode cap must therefore key on what landed on disk, not the header
    /// typeflag: a `wide/` entry at `0o2777` with a regular typeflag must still
    /// extract as a `0o755` directory. Asserted through the extract path, where
    /// the cap lives — the bundler's `HeaderMode::Deterministic` would mask the
    /// mode end-to-end and hide the bug.
    #[cfg(unix)]
    #[tokio::test]
    async fn extract_masks_old_bsd_trailing_slash_directory_mode() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::tempdir().unwrap();
        let archive_path = scratch.path().join("bsd.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let mut header = ::tar::Header::new_gnu();
            // Regular typeflag, but a trailing-slash name triggers tar-rs's
            // old-BSD directory heuristic on unpack. The name bytes are written
            // directly because `set_path` drops the trailing slash.
            header.set_entry_type(::tar::EntryType::Regular);
            header.set_size(0);
            header.set_mode(0o2777); // setgid + rwxrwxrwx
            header.set_mtime(0);
            header.as_gnu_mut().unwrap().name[..5].copy_from_slice(b"wide/");
            header.set_cksum();
            builder.append(&header, &b""[..]).unwrap();
            builder.finish().unwrap();
        }

        let output = scratch.path().join("out");
        Archive::extract(&archive_path, &output).await.unwrap();

        let extracted = output.join("wide");
        assert!(
            std::fs::symlink_metadata(&extracted).unwrap().is_dir(),
            "a trailing-slash regular-typeflag entry must land a directory"
        );
        let mode = std::fs::metadata(&extracted).unwrap().permissions().mode() & 0o7777;
        assert_eq!(
            mode, 0o755,
            "old-BSD directory mode not capped: setgid/world-write survived"
        );
    }

    /// Codex finding — the symlink is in the TARGET, not the link's parent. Entry
    /// 1 (`a` -> `.`) is a legitimate in-root symlink; entry 2 (`e1` -> `a/..`)
    /// has a target whose intermediate component `a` is that planted symlink, so
    /// the target folds to the root lexically while physically climbing one level
    /// UP. If `e1` were created, entry 3 (a regular file under `e1`) would
    /// `create_dir_all` OUTSIDE the root before any post-hoc check could fire.
    /// `validate_target` now resolves the target physically and refuses `e1`, so
    /// the chain never forms and nothing lands outside the root.
    #[cfg(unix)]
    #[tokio::test]
    async fn tar_target_symlink_traversal_cannot_mkdir_outside_root() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let mut a = ::tar::Header::new_gnu();
            a.set_entry_type(::tar::EntryType::Symlink);
            a.set_size(0);
            a.set_mode(0o777);
            builder.append_link(&mut a, "a", ".").unwrap();
            let mut e1 = ::tar::Header::new_gnu();
            e1.set_entry_type(::tar::EntryType::Symlink);
            e1.set_size(0);
            e1.set_mode(0o777);
            builder.append_link(&mut e1, "e1", "a/..").unwrap();
            let body = b"x";
            let mut f = ::tar::Header::new_gnu();
            f.set_size(body.len() as u64);
            f.set_mode(0o644);
            f.set_cksum();
            builder.append_data(&mut f, "e1/PWNED/x", &body[..]).unwrap();
            builder.finish().unwrap();
        }

        let result = Archive::extract(&archive_path, &root).await;
        assert!(
            result.is_err(),
            "the target-traversal chain must be refused: {result:?}"
        );
        assert!(
            !scratch.path().join("PWNED").exists(),
            "no directory may be created outside the extraction root"
        );
    }

    /// Codex finding — a published symlink whose target reaches through a planted
    /// in-root symlink must not resolve outside the root. `ROOTED` -> `a/../SECRET`
    /// folds to the in-root `SECRET` lexically, but `a` -> `.` makes it physically
    /// point at a scratch-level file. `validate_target` resolves the target
    /// physically and refuses it, so the symlink is never created — refused before
    /// anything lands, checked on the filesystem.
    #[cfg(unix)]
    #[tokio::test]
    async fn tar_published_symlink_target_cannot_escape_root() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        std::fs::write(scratch.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let mut a = ::tar::Header::new_gnu();
            a.set_entry_type(::tar::EntryType::Symlink);
            a.set_size(0);
            a.set_mode(0o777);
            builder.append_link(&mut a, "a", ".").unwrap();
            let mut rooted = ::tar::Header::new_gnu();
            rooted.set_entry_type(::tar::EntryType::Symlink);
            rooted.set_size(0);
            rooted.set_mode(0o777);
            builder.append_link(&mut rooted, "ROOTED", "a/../SECRET").unwrap();
            builder.finish().unwrap();
        }

        let result = Archive::extract(&archive_path, &root).await;
        assert!(
            result.is_err(),
            "the escaping symlink target must be refused: {result:?}"
        );
        assert!(
            std::fs::symlink_metadata(root.join("ROOTED")).is_err(),
            "the escaping symlink must not be created"
        );
        assert!(
            std::fs::read(root.join("ROOTED")).is_err(),
            "no out-of-root file may be readable through the extracted tree"
        );
    }

    /// L2 finding — the ladder in REVERSE entry order. `e1 -> a/..` arrives while
    /// `a` does not exist yet: the target's absent tail folds lexically to the root
    /// and the per-entry predicate accepts it. Then `a -> .` lands, and `e1` now
    /// physically resolves to `root/..`. Nothing is written through it (the
    /// ancestor guard holds), but the extracted tree carries an escaping link, and
    /// the layer routes have no re-pack to catch it. The post-loop sweep re-judges
    /// every link against the finished tree and refuses. Asserts the FILESYSTEM:
    /// a scratch-level file must not be readable through the extracted tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn tar_reverse_order_ladder_is_refused_by_the_post_loop_sweep() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        std::fs::write(scratch.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            // `e1` first: its hop `a` is absent, so the target folds in-root.
            let mut e1 = ::tar::Header::new_gnu();
            e1.set_entry_type(::tar::EntryType::Symlink);
            e1.set_size(0);
            e1.set_mode(0o777);
            builder.append_link(&mut e1, "e1", "a/..").unwrap();
            // `a` second: now `e1` -> `a/..` -> `./..` -> the scratch dir.
            let mut a = ::tar::Header::new_gnu();
            a.set_entry_type(::tar::EntryType::Symlink);
            a.set_size(0);
            a.set_mode(0o777);
            builder.append_link(&mut a, "a", ".").unwrap();
            builder.finish().unwrap();
        }

        let result = Archive::extract(&archive_path, &root).await;
        assert!(
            result.is_err(),
            "a link that escapes once a later hop lands must be refused: {result:?}"
        );
        assert!(
            std::fs::read(root.join("e1").join("SECRET")).is_err(),
            "no out-of-root file may be readable through the extracted tree"
        );
    }

    /// Codex cross-model recipe — three links, hop planted LAST: `e1 -> a/..`,
    /// `ROOTED -> e1/SECRET`, `a -> .`. Each is accepted on arrival (its hop is
    /// absent, the tail folds in-root); once `a` lands, `ROOTED` reads a
    /// scratch-level host file through the extracted tree. The post-loop sweep
    /// must refuse and leave neither link behind, in whatever order `read_dir`
    /// visits them — asserted on the filesystem: the host file is not readable
    /// through `ROOTED`, and `ROOTED` itself is gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn tar_three_link_ladder_with_the_hop_planted_last_is_refused() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        std::fs::write(scratch.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            for (name, target) in [("e1", "a/.."), ("ROOTED", "e1/SECRET"), ("a", ".")] {
                let mut header = ::tar::Header::new_gnu();
                header.set_entry_type(::tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                builder.append_link(&mut header, name, target).unwrap();
            }
            builder.finish().unwrap();
        }

        let result = Archive::extract(&archive_path, &root).await;
        assert!(result.is_err(), "the three-link ladder must be refused: {result:?}");
        assert!(
            std::fs::read(root.join("ROOTED")).is_err(),
            "no out-of-root file may be readable through the extracted tree"
        );
        // Which of the two links the sweep meets first is `read_dir` order:
        // meeting `ROOTED` first removes both; meeting `e1` first removes `e1`,
        // after which `ROOTED`'s target is absent and it is re-judged as an
        // in-root dangling link. Either way the property holds — nothing left
        // under the root resolves outside it — and that, not a particular
        // survivor set, is what the sweep promises.
        for name in ["e1", "ROOTED", "a"] {
            if let Ok(resolved) = std::fs::canonicalize(root.join(name)) {
                assert!(
                    resolved.starts_with(std::fs::canonicalize(&root).unwrap()),
                    "{name} still resolves outside the root: {}",
                    resolved.display()
                );
            }
        }
    }

    /// Windows: an in-root directory symlink extracts as a junction and the
    /// post-loop sweep keeps it. The junction's substitute name is necessarily
    /// absolute, so a sweep that re-ran the entry-time predicate (which refuses
    /// absolute targets) would refuse every junction it had just created — the
    /// regression the second cross-model gate caught. Runs on the Windows unit
    /// legs; an end-to-end `--extract` row cannot be green on Windows yet
    /// because the re-pack still judges a junction by its spelled target.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_in_root_directory_symlink_extracts_as_a_junction() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        let archive_path = scratch.path().join("linked.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let body = b"through the link";
            let mut f = ::tar::Header::new_gnu();
            f.set_size(body.len() as u64);
            f.set_mode(0o644);
            f.set_cksum();
            builder.append_data(&mut f, "d/f.txt", &body[..]).unwrap();
            let mut link = ::tar::Header::new_gnu();
            link.set_entry_type(::tar::EntryType::Symlink);
            link.set_size(0);
            link.set_mode(0o777);
            builder.append_link(&mut link, "link", "d").unwrap();
            builder.finish().unwrap();
        }

        Archive::extract(&archive_path, &root)
            .await
            .expect("an in-root directory link must extract");
        assert!(
            crate::symlink::is_link(&root.join("link")),
            "the link must land as a junction"
        );
        assert_eq!(
            dunce::canonicalize(root.join("link")).unwrap(),
            dunce::canonicalize(root.join("d")).unwrap(),
            "the junction must resolve to the in-root directory"
        );
        assert_eq!(
            std::fs::read(root.join("link").join("f.txt")).unwrap(),
            b"through the link"
        );
    }

    /// Windows: the reverse-order ladder must never leave a junction that
    /// resolves outside the root. Whether NTFS resolves a `..` inside a
    /// junction's substitute name is not something this host can observe, so
    /// the assertion is the invariant, not a survivor set: refused and removed,
    /// or unresolvable — never resolving outside.
    #[cfg(windows)]
    #[tokio::test]
    async fn windows_reverse_order_ladder_never_resolves_outside_the_root() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        std::fs::write(scratch.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            for (name, target) in [("e1", "a/.."), ("a", ".")] {
                let mut header = ::tar::Header::new_gnu();
                header.set_entry_type(::tar::EntryType::Symlink);
                header.set_size(0);
                header.set_mode(0o777);
                builder.append_link(&mut header, name, target).unwrap();
            }
            builder.finish().unwrap();
        }

        let _ = Archive::extract(&archive_path, &root).await;
        let canonical_root = dunce::canonicalize(&root).unwrap();
        for name in ["e1", "a"] {
            if let Ok(resolved) = dunce::canonicalize(root.join(name)) {
                assert!(
                    resolved.starts_with(&canonical_root),
                    "{name} resolves outside the root: {}",
                    resolved.display()
                );
            }
        }
        assert!(
            std::fs::read(root.join("e1").join("SECRET")).is_err(),
            "no host file may be readable through the tree"
        );
    }

    /// Belt (the "nothing is created before containment is established" rule): a
    /// regular file whose parent traverses a planted in-root symlink (`a` -> `.`)
    /// is refused BEFORE `create_dir_all`, so nothing is written through the
    /// symlink even though it stays lexically in-root. Exercises the extractor's
    /// pre-create ancestor-symlink refusal directly (no `validate_target` on the
    /// regular-file path).
    #[cfg(unix)]
    #[tokio::test]
    async fn tar_write_through_planted_symlink_is_refused_before_creating() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        let archive_path = scratch.path().join("evil.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let mut a = ::tar::Header::new_gnu();
            a.set_entry_type(::tar::EntryType::Symlink);
            a.set_size(0);
            a.set_mode(0o777);
            builder.append_link(&mut a, "a", ".").unwrap();
            let body = b"x";
            let mut f = ::tar::Header::new_gnu();
            f.set_size(body.len() as u64);
            f.set_mode(0o644);
            f.set_cksum();
            builder.append_data(&mut f, "a/through", &body[..]).unwrap();
            builder.finish().unwrap();
        }

        let result = Archive::extract(&archive_path, &root).await;
        assert!(
            result.is_err(),
            "a write through a planted symlink must be refused: {result:?}"
        );
        assert!(
            !root.join("through").exists(),
            "nothing may be written through the planted symlink"
        );
    }

    /// Codex second finding — a GNU sparse entry materializes its apparent size
    /// with `seek`+`set_len`, consuming no tar-stream bytes, so the stream cap
    /// never trips and one stored byte claims an arbitrary on-disk size. Refused
    /// outright (ocx never emits sparse). Builds a real sparse header: 1 stored
    /// data byte at a 4 GiB offset, real size 4 GiB.
    #[cfg(unix)]
    #[tokio::test]
    async fn gnu_sparse_entry_is_refused() {
        fn set_octal(field: &mut [u8; 12], value: u64) {
            let encoded = format!("{value:011o}\0");
            field.copy_from_slice(encoded.as_bytes());
        }

        let big: u64 = 1 << 32; // 4 GiB apparent size
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        let archive_path = scratch.path().join("sparse.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let mut header = ::tar::Header::new_gnu();
            header.set_entry_type(::tar::EntryType::GNUSparse);
            header.set_size(1); // one stored data byte (the rest is a hole)
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_path("sparsefile").unwrap();
            {
                let gnu = header.as_gnu_mut().unwrap();
                set_octal(&mut gnu.realsize, big);
                set_octal(&mut gnu.sparse[0].offset, big - 1);
                set_octal(&mut gnu.sparse[0].numbytes, 1);
            }
            header.set_cksum();
            builder.append(&header, &[0u8][..]).unwrap();
            builder.finish().unwrap();
        }

        let result = Archive::extract(&archive_path, &root).await;
        assert!(
            matches!(
                result,
                Err(crate::Error::Archive(crate::archive::Error::GnuSparseUnsupported(_)))
            ),
            "a GNU sparse entry must be refused: {result:?}"
        );
        assert!(
            std::fs::symlink_metadata(root.join("sparsefile")).is_err(),
            "no sparse file may be materialized"
        );
    }

    /// Codex Finding 4 — the mode cap must reach IMPLICITLY-created parent
    /// directories, not only entry-driven ones. `create_dir_all` opens them with
    /// `0o777 & ~umask`; under a permissive umask (000/002, routine in CI and
    /// containers) an archive of `pkg/sub/deep/file` would otherwise leave
    /// `pkg`, `pkg/sub` and `pkg/sub/deep` group/other-writable. Runs under
    /// umask 000 — the state a default (022) run cannot distinguish from the fix
    /// — and asserts each created directory has the `0o7000|0o022` bits stripped.
    #[cfg(unix)]
    #[tokio::test]
    async fn extract_masks_implicitly_created_parent_directories() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("out");
        let archive_path = scratch.path().join("nested.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            // Only a deep FILE entry — every parent directory is implicit, so it
            // is `create_dir_all`'d, never carried as its own capped entry.
            let body = b"x";
            let mut f = ::tar::Header::new_gnu();
            f.set_size(body.len() as u64);
            f.set_mode(0o644);
            f.set_cksum();
            builder.append_data(&mut f, "pkg/sub/deep/file", &body[..]).unwrap();
            builder.finish().unwrap();
        }

        // Extract under a fully permissive umask so an uncapped `create_dir_all`
        // yields 0o777. Capture the modes, restore the umask, THEN assert — so a
        // failing assertion never leaks the changed umask into a sibling test
        // (nextest runs each test in its own process; this keeps `cargo test`
        // safe too).
        let previous = unsafe { libc::umask(0) };
        let result = Archive::extract(&archive_path, &root).await;
        let modes: Vec<(std::path::PathBuf, u32)> = ["pkg", "pkg/sub", "pkg/sub/deep"]
            .iter()
            .map(|rel| {
                let dir = root.join(rel);
                let mode = std::fs::symlink_metadata(&dir).unwrap().permissions().mode() & 0o7777;
                (dir, mode)
            })
            .collect();
        unsafe {
            libc::umask(previous);
        }

        result.unwrap();
        for (dir, mode) in modes {
            assert_eq!(
                mode & (0o7000 | 0o022),
                0,
                "implicitly created directory {} kept forbidden bits: {mode:04o}",
                dir.display()
            );
        }
    }
}
