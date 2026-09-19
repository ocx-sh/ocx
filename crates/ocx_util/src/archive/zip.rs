// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use zip::write::SimpleFileOptions;

use crate::compression;

use super::backend::Backend;
use super::error::{Error, Result};

use super::LOG_INTERVAL;

/// Object-safe trait combining `Write`, `Seek`, and `Send` (required by `ZipWriter`).
pub(super) trait WriteSeek: Write + Seek + Send {}
impl<T: Write + Seek + Send> WriteSeek for T {}

pub(super) struct ZipBackend {
    inner: Arc<Mutex<zip::ZipWriter<Box<dyn WriteSeek>>>>,
    options: SimpleFileOptions,
    output_path: PathBuf,
}

impl ZipBackend {
    pub fn new(output: &Path, level: compression::CompressionLevel) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(output)
            .map_err(|e| Error::Io {
                path: output.to_path_buf(),
                source: e,
            })?;
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(match level {
                compression::CompressionLevel::Fast => 1,
                compression::CompressionLevel::Best => 9,
                compression::CompressionLevel::Default => 6,
            }));
        let writer = zip::ZipWriter::new(Box::new(file) as Box<dyn WriteSeek>);
        Ok(Self {
            inner: Arc::new(Mutex::new(writer)),
            options,
            output_path: output.to_path_buf(),
        })
    }

    /// Locks the writer on a blocking thread, runs `f`, and releases the lock.
    async fn run_blocking<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut zip::ZipWriter<Box<dyn WriteSeek>>) -> Result<R> + Send + 'static,
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
impl Backend for ZipBackend {
    async fn add_file(&mut self, archive_path: PathBuf, file: PathBuf) -> Result<()> {
        let options = self.options;
        self.run_blocking(move |writer| {
            let opts = file_options_with_permissions(options, &file);
            let mut source = std::fs::File::open(&file).map_err(|e| Error::Io { path: file, source: e })?;
            let name = path_to_zip_name(&archive_path);
            writer.start_file(name, opts).map_err(Error::Zip)?;
            std::io::copy(&mut source, writer).map_err(|e| Error::Io {
                path: archive_path,
                source: e,
            })?;
            Ok(())
        })
        .await
    }

    async fn add_dir(&mut self, archive_path: PathBuf, _dir: PathBuf) -> Result<()> {
        let options = self.options;
        self.run_blocking(move |writer| {
            let name = path_to_zip_name(&archive_path);
            if !name.is_empty() {
                let dir_name = if name.ends_with('/') { name } else { format!("{name}/") };
                writer.add_directory(dir_name, options).map_err(Error::Zip)?;
            }
            Ok(())
        })
        .await
    }

    async fn add_dir_all(&mut self, archive_path: PathBuf, dir: PathBuf) -> Result<()> {
        let options = self.options;
        self.run_blocking(move |writer| {
            let mut count = 0u64;
            add_dir_recursive(writer, options, &archive_path, &dir, &mut count)?;
            tracing::debug!("Bundled {count} entries total");
            Ok(())
        })
        .await
    }

    async fn finish(self: Box<Self>) -> Result<()> {
        let Ok(mutex) = Arc::try_unwrap(self.inner) else {
            panic!("backend has outstanding references");
        };
        let writer = mutex.into_inner().unwrap_or_else(|e| e.into_inner());
        let output_path = self.output_path;
        tokio::task::spawn_blocking(move || {
            let mut inner = writer.finish().map_err(Error::Zip)?;
            inner.flush().map_err(|e| Error::Io {
                path: output_path,
                source: e,
            })?;
            Ok(())
        })
        .await
        .map_err(Error::internal)?
    }
}

fn add_dir_recursive(
    writer: &mut zip::ZipWriter<Box<dyn WriteSeek>>,
    options: SimpleFileOptions,
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
        let file_type = entry.file_type().map_err(|e| Error::Io {
            path: entry.path(),
            source: e,
        })?;
        let name = entry.file_name();
        let archive_path = if base_path.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            base_path.join(&name)
        };

        if file_type.is_symlink() {
            let entry_path = entry.path();
            let target = std::fs::read_link(&entry_path).map_err(|e| Error::Io {
                path: entry_path,
                source: e,
            })?;
            let link_name = path_to_zip_name(&archive_path);
            let target_name = target.to_string_lossy();
            writer
                .add_symlink(link_name, &*target_name, options)
                .map_err(Error::Zip)?;
        } else if file_type.is_dir() {
            let dir_name = path_to_zip_name(&archive_path);
            let dir_name = if dir_name.ends_with('/') {
                dir_name
            } else {
                format!("{dir_name}/")
            };
            writer.add_directory(dir_name, options).map_err(Error::Zip)?;
            add_dir_recursive(writer, options, &archive_path, &entry.path(), count)?;
        } else {
            let file_path = entry.path();
            let file_name = path_to_zip_name(&archive_path);
            let opts = file_options_with_permissions(options, &file_path);
            writer.start_file(file_name, opts).map_err(Error::Zip)?;
            let mut source = std::fs::File::open(&file_path).map_err(|e| Error::Io {
                path: file_path.clone(),
                source: e,
            })?;
            std::io::copy(&mut source, writer).map_err(|e| Error::Io {
                path: file_path,
                source: e,
            })?;
        }

        *count += 1;
        tracing::trace!("Adding {}", archive_path.display());
        if (*count).is_multiple_of(LOG_INTERVAL) {
            tracing::debug!("Bundled {} entries", *count);
        }
    }
    Ok(())
}

/// Converts a `Path` to a forward-slash ZIP entry name.
fn path_to_zip_name(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Returns `options` with Unix permissions copied from `file`, if available.
#[cfg(unix)]
fn file_options_with_permissions(options: SimpleFileOptions, file: &Path) -> SimpleFileOptions {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = file.metadata() {
        options.unix_permissions(metadata.permissions().mode())
    } else {
        options
    }
}

#[cfg(not(unix))]
fn file_options_with_permissions(options: SimpleFileOptions, _file: &Path) -> SimpleFileOptions {
    options
}

/// What one zip entry costs against the decompressed-byte cap before its body
/// is read — the size of a tar header, so the two backends share a ceiling.
const ENTRY_FLOOR_BYTES: u64 = 512;

pub(super) fn extract(archive: &Path, output: &Path, strip_components: usize, decompressed_cap: u64) -> Result<()> {
    use std::io::Read as _;

    let file = std::fs::File::open(archive).map_err(|e| Error::Io {
        path: archive.to_path_buf(),
        source: e,
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(Error::Zip)?;

    // The extraction root must exist before the canonical-root resolution and
    // before any entry lands.
    std::fs::create_dir_all(output).map_err(|e| Error::Io {
        path: output.to_path_buf(),
        source: e,
    })?;
    let canonical_root = dunce::canonicalize(output).map_err(|e| Error::Io {
        path: output.to_path_buf(),
        source: e,
    })?;

    let mut count = 0u64;
    // Running total of decompressed bytes written, capped against
    // `decompressed_cap` (CWE-400) — the zip analog of the tar path's
    // `Read::take` bomb guard.
    let mut total_written: u64 = 0;
    for i in 0..zip.len() {
        // Every entry is charged a floor against the byte budget before it is
        // read: a zero-length entry writes no bytes, but it still costs an
        // inode, and a zip full of them exhausts the extraction filesystem well
        // inside the byte cap. The tar path is bounded by construction — its
        // cap is applied to the decompressed *stream*, where every entry
        // carries a 512-byte header — so charging the same 512 here gives both
        // backends one ceiling and one refusal, with no second constant to
        // drift.
        total_written = total_written.saturating_add(ENTRY_FLOOR_BYTES);
        if total_written > decompressed_cap {
            return Err(Error::ExtractionCapExceeded { cap: decompressed_cap });
        }
        let mut entry = zip.by_index(i).map_err(Error::Zip)?;
        // D7: an entry whose name is absolute or escapes the root has no
        // enclosed name. Refuse the archive rather than skipping the entry, so a
        // traversal attempt fails loudly exactly as the tar path does.
        let Some(enclosed_name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            return Err(Error::EntryEscape(PathBuf::from(entry.name())));
        };

        let stripped: PathBuf = enclosed_name.iter().skip(strip_components).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        let out = crate::fs::path::join_under_root(&canonical_root, &stripped)
            .map_err(|_| Error::EntryEscape(enclosed_name.clone()))?;

        // A `.` / `a/..` entry normalizing to the root itself names no new
        // content; skip it rather than treating it as an escape (its parent is
        // the root's parent, legitimately outside the root).
        if out == canonical_root {
            continue;
        }

        // Physical containment BEFORE creating anything (CWE-22): refuse if any
        // existing ancestor of `out` below the root is a symlink an earlier entry
        // planted — in this entry's parent chain, or one a symlink target reaches
        // through — so `create_dir_all` / the file write cannot escape through it.
        // The mkdir must be refused before it happens, not after (a check that ran
        // post-`create_dir_all` has already let the directory escape). Every
        // component created below is then a fresh directory, which cannot be a
        // symlink, so `out` stays physically contained and is safe to act on by
        // its own path. `Some(canonical_root)` trusts the root's own ancestry and
        // scopes the walk to the untrusted portion strictly below it.
        crate::fs::refuse_if_symlink_in_path_sync(&out, Some(&canonical_root)).map_err(|e| match e {
            crate::fs::SymlinkWalkError::Ancestor { .. } => Error::EntryEscape(enclosed_name.clone()),
            // A component we could not even stat is an I/O fault (74), not a
            // containment verdict (65).
            crate::fs::SymlinkWalkError::Io { path, source } => Error::Io { path, source },
        })?;
        if let Some(parent) = out.parent() {
            crate::archive::create_dir_all_capped(&canonical_root, parent)?;
        }
        let dst = out;

        if entry.is_dir() {
            crate::archive::create_dir_all_capped(&canonical_root, &dst)?;
        } else if entry.is_symlink() {
            // Read the symlink body bounded by the remaining budget plus one
            // probe byte, so a hostile entry declaring a huge body cannot be
            // slurped whole into memory before the cap check fires (CWE-400).
            let remaining = decompressed_cap.saturating_sub(total_written);
            let mut target = String::new();
            let read = entry
                .by_ref()
                .take(remaining.saturating_add(1))
                .read_to_string(&mut target)
                .map_err(|e| Error::Io {
                    path: dst.clone(),
                    source: e,
                })?;
            total_written = total_written.saturating_add(read as u64);
            if total_written > decompressed_cap {
                return Err(Error::ExtractionCapExceeded { cap: decompressed_cap });
            }
            crate::fs::symlink::validate_target(&canonical_root, &dst, Path::new(&target))?;
            crate::fs::symlink::create(Path::new(&target), &dst)?;
        } else {
            // Create the file with `create_new` so a symlink already at `dst`
            // (planted by an earlier entry) is never followed; on EEXIST remove
            // the occupant — the symlink itself, or a stale regular file — and
            // create afresh, so the write always lands as a real regular file
            // under the ancestor-guarded `dst` rather than through a link out of
            // the root.
            let mut outfile = match std::fs::OpenOptions::new().write(true).create_new(true).open(&dst) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::fs::remove_file(&dst).map_err(|e| Error::Io {
                        path: dst.clone(),
                        source: e,
                    })?;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&dst)
                        .map_err(|e| Error::Io {
                            path: dst.clone(),
                            source: e,
                        })?
                }
                Err(error) => {
                    return Err(Error::Io {
                        path: dst.clone(),
                        source: error,
                    });
                }
            };
            // Bound the per-entry copy at the remaining budget plus one probe
            // byte, so a bomb cannot write unbounded bytes to disk before the
            // check fires.
            let remaining = decompressed_cap.saturating_sub(total_written);
            let mut limited = entry.by_ref().take(remaining.saturating_add(1));
            let written = std::io::copy(&mut limited, &mut outfile).map_err(|e| Error::Io {
                path: dst.clone(),
                source: e,
            })?;
            total_written = total_written.saturating_add(written);
            if total_written > decompressed_cap {
                return Err(Error::ExtractionCapExceeded { cap: decompressed_cap });
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    // A zip stores the archiving host's whole unix mode, so an
                    // entry recorded as 0o4777 would land setuid + world-writable
                    // and be published that way. Setuid/setgid/sticky (0o7000)
                    // and group/other write (0o022) are masked off here, at the
                    // single site that applies the archived mode; read and
                    // execute bits — the ones an archive legitimately carries —
                    // pass through untouched.
                    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode & !(0o7000 | 0o022))).map_err(
                        |e| Error::Io {
                            path: dst.clone(),
                            source: e,
                        },
                    )?;
                }
            }
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

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A cap large enough that no mode/containment test trips it.
    const TEST_CAP: u64 = 1 << 30;

    /// Writes a zip holding one `body`-sized file per `(name, unix mode)` pair.
    fn zip_with_modes(dir: &Path, entries: &[(&str, u32)]) -> PathBuf {
        let path = dir.join("modes.zip");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, mode) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default().unix_permissions(*mode))
                .unwrap();
            writer.write_all(b"body").unwrap();
        }
        writer.finish().unwrap();
        path
    }

    fn extracted_mode(root: &Path, name: &str) -> u32 {
        std::fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o777
    }

    /// The full permission word including setuid/setgid/sticky (0o7000), which
    /// [`extracted_mode`]'s `& 0o777` mask would hide — the bits the D8 mask must
    /// strip.
    fn extracted_full_mode(root: &Path, name: &str) -> u32 {
        std::fs::metadata(root.join(name)).unwrap().permissions().mode() & 0o7777
    }

    /// Writes a one-entry zip whose central-directory external attributes carry
    /// the full unix mode `full_mode` (including setuid/setgid/sticky).
    ///
    /// The crate's `unix_permissions` masks those bits off at write time
    /// (`& 0o777`), so a real setuid entry — the kind Info-ZIP produces, and
    /// which `unix_mode()` surfaces via `external_attributes >> 16` — can only be
    /// forged by patching the external-attributes field directly. Without this,
    /// a test of the D8 mask is vacuous: it can never observe a set high bit.
    fn zip_with_external_mode(dir: &Path, name: &str, full_mode: u32) -> PathBuf {
        let path = dir.join(format!("{name}.zip"));
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file(name, SimpleFileOptions::default().unix_permissions(full_mode & 0o777))
                .unwrap();
            writer.write_all(b"body").unwrap();
            writer.finish().unwrap();
        }
        // Patch the single central-directory header's external attributes
        // (offset 38, little-endian u32): full unix mode in the high word, with
        // the S_IFREG type bit a Unix zip carries beside it.
        let mut bytes = std::fs::read(&path).unwrap();
        const CDFH_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
        let cdfh = bytes
            .windows(4)
            .position(|window| window == CDFH_SIGNATURE)
            .expect("central-directory header not found");
        let attrs = ((0o100000 | full_mode) << 16).to_le_bytes();
        bytes[cdfh + 38..cdfh + 42].copy_from_slice(&attrs);
        std::fs::write(&path, &bytes).unwrap();
        path
    }

    /// A zip records the archiving host's whole mode, so an entry written as
    /// 0o777 must not extract world-writable.
    #[test]
    fn extract_masks_group_and_other_write_bits() {
        let dir = tempfile::tempdir().unwrap();
        let archive = zip_with_modes(dir.path(), &[("wide.sh", 0o777), ("sticky.txt", 0o666)]);
        let output = dir.path().join("out");

        extract(&archive, &output, 0, TEST_CAP).unwrap();

        assert_eq!(extracted_mode(&output, "wide.sh"), 0o755);
        assert_eq!(extracted_mode(&output, "sticky.txt"), 0o644);
    }

    /// D8: setuid/setgid/sticky bits (0o7000) are stripped. A `0o4755` entry —
    /// setuid + rwxr-xr-x — must land 0o755, or a published package could carry
    /// a setuid binary the publisher never intended.
    #[test]
    fn extract_masks_setuid_setgid_and_sticky_bits() {
        // One entry per mode: each zip's external attributes are patched to carry
        // the full mode, since the crate writer cannot. Full mode (0o7777) is
        // read back — `& 0o777` would strip the very bits under test.
        for full in [0o4755u32, 0o2755, 0o1777] {
            let dir = tempfile::tempdir().unwrap();
            let archive = zip_with_external_mode(dir.path(), "tool", full);
            let output = dir.path().join("out");

            extract(&archive, &output, 0, TEST_CAP).unwrap();

            assert_eq!(
                extracted_full_mode(&output, "tool"),
                0o755,
                "setuid/setgid/sticky not stripped from {full:o}"
            );
        }
    }

    /// The mask touches only setuid/setgid/sticky and group/other write: the
    /// read and execute bits an archive legitimately carries survive the round
    /// trip.
    #[test]
    fn extract_preserves_modes_without_group_or_other_write() {
        let dir = tempfile::tempdir().unwrap();
        let archive = zip_with_modes(
            dir.path(),
            &[("tool", 0o755), ("data.txt", 0o644), ("private.key", 0o600)],
        );
        let output = dir.path().join("out");

        extract(&archive, &output, 0, TEST_CAP).unwrap();

        assert_eq!(extracted_mode(&output, "tool"), 0o755);
        assert_eq!(extracted_mode(&output, "data.txt"), 0o644);
        assert_eq!(extracted_mode(&output, "private.key"), 0o600);
    }

    /// Writes a zip with a single raw entry name (bypassing `enclosed_name`
    /// normalization on the write side), so a traversal name reaches the reader.
    fn zip_with_raw_name(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join("evil.zip");
        let file = std::fs::File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file(name, SimpleFileOptions::default().unix_permissions(0o644))
            .unwrap();
        writer.write_all(b"pwned").unwrap();
        writer.finish().unwrap();
        path
    }

    /// D7: a `../evil` entry escapes the root, so extraction refuses the archive
    /// with a typed escape error rather than silently skipping the entry (the
    /// prior `continue`).
    #[test]
    fn extract_rejects_a_traversal_entry() {
        let dir = tempfile::tempdir().unwrap();
        let archive = zip_with_raw_name(dir.path(), "../evil");
        let output = dir.path().join("out");

        let error = extract(&archive, &output, 0, TEST_CAP).unwrap_err();

        assert!(
            matches!(error, Error::EntryEscape(_)),
            "a traversal entry must be rejected as an escape, got: {error}"
        );
        assert!(
            !dir.path().join("evil").exists(),
            "the traversal entry escaped the extraction root"
        );
    }

    /// Suggest: `--strip-components` silently drops an entry whose whole path
    /// the strip consumes, keeping the deeper ones. This pins that observed
    /// contract — a shallow entry vanishes, a deep one is re-rooted.
    #[test]
    fn strip_drops_fully_consumed_entries_and_keeps_deeper_ones() {
        let dir = tempfile::tempdir().unwrap();
        let archive = zip_with_modes(dir.path(), &[("top.txt", 0o644), ("nested/deep/file.txt", 0o644)]);
        let output = dir.path().join("out");

        extract(&archive, &output, 1, TEST_CAP).unwrap();

        assert!(!output.join("top.txt").exists(), "the depth-1 entry should be dropped");
        assert!(
            output.join("deep/file.txt").exists(),
            "the depth-3 entry should be re-rooted to deep/file.txt"
        );
    }

    /// Codex (deferred, now owned): a zip of zero-length entries writes no bytes
    /// and so never trips a cap on bytes written — but every entry costs an
    /// inode. Each entry is charged the tar-header floor against the same
    /// budget, so the count is bounded by the cap exactly as it is for tar.
    /// The control: the same archive under a budget that covers the floor for
    /// every entry extracts, so the charge is a bound and not a ban.
    #[test]
    fn a_zip_of_empty_entries_is_bounded_by_the_byte_cap() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("empties.zip");
        let entries = 64u64;
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            for i in 0..entries {
                writer
                    .start_file(format!("e{i}"), SimpleFileOptions::default().unix_permissions(0o644))
                    .unwrap();
            }
            writer.finish().unwrap();
        }

        // Zero bytes will ever be written, so a cap on bytes alone passes this.
        let too_small = ENTRY_FLOOR_BYTES * (entries - 1);
        let error = extract(&archive, &dir.path().join("refused"), 0, too_small).unwrap_err();
        assert!(
            matches!(
                error,
                Error::ExtractionCapExceeded { cap } if cap == too_small
            ),
            "empty entries beyond the budget must be refused, got: {error}"
        );

        let enough = ENTRY_FLOOR_BYTES * entries;
        let accepted = dir.path().join("accepted");
        extract(&archive, &accepted, 0, enough).unwrap();
        assert_eq!(std::fs::read_dir(&accepted).unwrap().count() as u64, entries);
    }

    /// W20: a zip whose decompressed bytes exceed the cap is refused
    /// (decompression-bomb guard) rather than written to disk unbounded.
    #[test]
    fn extract_rejects_output_exceeding_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("big.zip")).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("big.txt", SimpleFileOptions::default().unix_permissions(0o644))
            .unwrap();
        writer.write_all(&vec![b'a'; 4096]).unwrap();
        writer.finish().unwrap();
        let archive = dir.path().join("big.zip");
        let output = dir.path().join("out");

        let error = extract(&archive, &output, 0, 16).unwrap_err();

        assert!(
            matches!(error, Error::ExtractionCapExceeded { cap: 16 }),
            "an over-cap extraction must be refused, got: {error}"
        );
    }

    /// Block 1 exploit — the planted symlink is in the link's PARENT chain: entry
    /// 1 (`a` -> `.`) makes `a` an in-root symlink; entry 2 (`a/evil` ->
    /// `../outside.txt`) plants a symlink whose parent traverses it. The extractor
    /// now refuses both entry 2 and entry 3 (the regular `evil`) at the pre-create
    /// ancestor-symlink check — each has the planted symlink on its path — before
    /// anything is written, and `validate_target` independently refuses entry 2's
    /// escaping target. The `create_new` + unlink-on-EEXIST write remains as a
    /// second line so a final-component symlink is never followed. (The
    /// target-in-the-component sibling is covered by `zip_target_symlink_*`.)
    #[test]
    fn symlink_chain_cannot_overwrite_a_file_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, b"original").unwrap();

        let archive = dir.path().join("evil.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let link_opts = SimpleFileOptions::default().unix_permissions(0o777);
            // `a` -> `.` collapses the parent onto the root; `a/evil` then plants
            // a symlink physically at `<root>/evil` pointing at `../outside.txt`.
            writer.add_symlink("a", ".", link_opts).unwrap();
            writer.add_symlink("a/evil", "../outside.txt", link_opts).unwrap();
            // A distinct name (no duplicate-filename refusal) that resolves to
            // the same `<root>/evil` planted symlink; `File::create` would follow
            // it out of the root.
            writer
                .start_file("evil", SimpleFileOptions::default().unix_permissions(0o644))
                .unwrap();
            writer.write_all(b"PWNED").unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let result = extract(&archive, &output, 0, TEST_CAP);

        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"original",
            "a file outside the extraction root was overwritten"
        );
        assert!(
            result.is_err(),
            "the escaping symlink entry must be refused: {result:?}"
        );
    }

    /// Warn 6: a directory entry whose parent is a planted escaping symlink must
    /// not create a directory outside the root. Closed by the extractor's
    /// pre-create ancestor-symlink refusal: entry 2 (`a/evil`) has the planted
    /// symlink `a` on its path and entry 3 (`a/evil/pwned`) traverses both, so
    /// each is refused before any `create_dir_all` runs — the mkdir never happens
    /// through the planted link, rather than being detected after the fact.
    #[test]
    fn directory_entry_cannot_escape_through_a_planted_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let captured = dir.path().join("captured");
        std::fs::create_dir_all(&captured).unwrap();

        let archive = dir.path().join("evil.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let link_opts = SimpleFileOptions::default().unix_permissions(0o777);
            writer.add_symlink("a", ".", link_opts).unwrap();
            writer.add_symlink("a/evil", "../captured", link_opts).unwrap();
            writer
                .add_directory("a/evil/pwned", SimpleFileOptions::default().unix_permissions(0o755))
                .unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let result = extract(&archive, &output, 0, TEST_CAP);

        assert!(
            !captured.join("pwned").exists(),
            "a directory was created outside the extraction root"
        );
        assert!(
            result.is_err(),
            "the escaping symlink entry must be refused: {result:?}"
        );
    }

    /// Warn 7: a symlink entry declaring a body larger than the remaining cap is
    /// refused as a decompression-bomb, and the body is read bounded rather than
    /// slurped whole into memory before the check. The refusal outcome is pinned
    /// here; the bound is the resource-hardening the fix adds.
    #[test]
    fn a_symlink_body_over_the_cap_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("biglink.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .add_symlink(
                    "biglink",
                    "a".repeat(4096),
                    SimpleFileOptions::default().unix_permissions(0o777),
                )
                .unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let error = extract(&archive, &output, 0, 16).unwrap_err();

        assert!(
            matches!(error, Error::ExtractionCapExceeded { cap: 16 }),
            "an over-cap symlink body must be refused, got: {error}"
        );
    }

    /// Codex finding — the symlink is in the TARGET, not the link's parent. Entry
    /// 1 (`a` -> `.`) is an in-root symlink; entry 2 (`e1` -> `a/..`) has a target
    /// whose component `a` is that planted symlink, folding to the root lexically
    /// while physically climbing one level UP. A regular file under `e1` would
    /// then `create_dir_all` outside the root. `validate_target` resolves the
    /// target physically and refuses `e1`, so nothing lands outside the root.
    #[cfg(unix)]
    #[test]
    fn zip_target_symlink_traversal_cannot_mkdir_outside_root() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("evil.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let link_opts = SimpleFileOptions::default().unix_permissions(0o777);
            writer.add_symlink("a", ".", link_opts).unwrap();
            writer.add_symlink("e1", "a/..", link_opts).unwrap();
            writer
                .start_file("e1/PWNED/x", SimpleFileOptions::default().unix_permissions(0o644))
                .unwrap();
            writer.write_all(b"x").unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let result = extract(&archive, &output, 0, TEST_CAP);
        assert!(
            result.is_err(),
            "the target-traversal chain must be refused: {result:?}"
        );
        assert!(
            !dir.path().join("PWNED").exists(),
            "no directory may be created outside the extraction root"
        );
    }

    /// Codex finding — a published symlink whose target reaches through a planted
    /// in-root symlink must not resolve outside the root. `ROOTED` -> `a/../SECRET`
    /// folds to the in-root `SECRET` lexically but `a` -> `.` makes it physically
    /// point at a scratch-level file. `validate_target` refuses it, so the symlink
    /// is never created — checked on the filesystem, not just the exit code.
    #[cfg(unix)]
    #[test]
    fn zip_published_symlink_target_cannot_escape_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive = dir.path().join("evil.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let link_opts = SimpleFileOptions::default().unix_permissions(0o777);
            writer.add_symlink("a", ".", link_opts).unwrap();
            writer.add_symlink("ROOTED", "a/../SECRET", link_opts).unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let result = extract(&archive, &output, 0, TEST_CAP);
        assert!(
            result.is_err(),
            "the escaping symlink target must be refused: {result:?}"
        );
        assert!(
            std::fs::symlink_metadata(output.join("ROOTED")).is_err(),
            "the escaping symlink must not be created"
        );
        assert!(
            std::fs::read(output.join("ROOTED")).is_err(),
            "no out-of-root file may be readable through the extracted tree"
        );
    }

    /// L2 finding — the ladder in REVERSE entry order (see the tar sibling).
    /// `e1 -> a/..` is accepted while `a` is absent; `a -> .` then makes `e1`
    /// resolve to the scratch dir. The post-loop sweep refuses the finished tree.
    #[cfg(unix)]
    #[test]
    fn zip_reverse_order_ladder_is_refused_by_the_post_loop_sweep() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive = dir.path().join("evil.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let link_opts = SimpleFileOptions::default().unix_permissions(0o777);
            writer.add_symlink("e1", "a/..", link_opts).unwrap();
            writer.add_symlink("a", ".", link_opts).unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let result = extract(&archive, &output, 0, TEST_CAP);
        assert!(
            result.is_err(),
            "a link that escapes once a later hop lands must be refused: {result:?}"
        );
        assert!(
            std::fs::read(output.join("e1").join("SECRET")).is_err(),
            "no out-of-root file may be readable through the extracted tree"
        );
    }

    /// Codex cross-model recipe (zip) — three links, hop planted LAST; see the
    /// tar sibling. The sweep must refuse and leave no escaping link behind.
    #[cfg(unix)]
    #[test]
    fn zip_three_link_ladder_with_the_hop_planted_last_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive = dir.path().join("evil.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let link_opts = SimpleFileOptions::default().unix_permissions(0o777);
            writer.add_symlink("e1", "a/..", link_opts).unwrap();
            writer.add_symlink("ROOTED", "e1/SECRET", link_opts).unwrap();
            writer.add_symlink("a", ".", link_opts).unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let result = extract(&archive, &output, 0, TEST_CAP);
        assert!(result.is_err(), "the three-link ladder must be refused: {result:?}");
        assert!(
            std::fs::read(output.join("ROOTED")).is_err(),
            "no out-of-root file may be readable through the extracted tree"
        );
        // Which of the two links the sweep meets first is `read_dir` order:
        // meeting `ROOTED` first removes both; meeting `e1` first removes `e1`,
        // after which `ROOTED`'s target is absent and it is re-judged as an
        // in-root dangling link. Either way the property holds — nothing left
        // under the root resolves outside it — and that, not a particular
        // survivor set, is what the sweep promises.
        for name in ["e1", "ROOTED", "a"] {
            if let Ok(resolved) = std::fs::canonicalize(output.join(name)) {
                assert!(
                    resolved.starts_with(std::fs::canonicalize(&output).unwrap()),
                    "{name} still resolves outside the root: {}",
                    resolved.display()
                );
            }
        }
    }

    /// Windows (zip): an in-root directory symlink extracts as a junction and
    /// survives the sweep — see the tar sibling for why this is the case the
    /// second cross-model gate caught.
    #[cfg(windows)]
    #[test]
    fn windows_in_root_directory_symlink_extracts_as_a_junction() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("linked.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            writer
                .start_file("d/f.txt", SimpleFileOptions::default().unix_permissions(0o644))
                .unwrap();
            writer.write_all(b"through the link").unwrap();
            writer
                .add_symlink("link", "d", SimpleFileOptions::default().unix_permissions(0o777))
                .unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        extract(&archive, &output, 0, TEST_CAP).expect("an in-root directory link must extract");
        assert!(
            crate::fs::symlink::is_link(&output.join("link")),
            "the link must land as a junction"
        );
        assert_eq!(
            dunce::canonicalize(output.join("link")).unwrap(),
            dunce::canonicalize(output.join("d")).unwrap(),
            "the junction must resolve to the in-root directory"
        );
        assert_eq!(
            std::fs::read(output.join("link").join("f.txt")).unwrap(),
            b"through the link"
        );
    }

    /// Windows (zip): the reverse-order ladder never leaves a junction that
    /// resolves outside the root — the invariant, not a survivor set.
    #[cfg(windows)]
    #[test]
    fn windows_reverse_order_ladder_never_resolves_outside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("SECRET"), b"TOPSECRET").unwrap();
        let archive = dir.path().join("evil.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let link_opts = SimpleFileOptions::default().unix_permissions(0o777);
            writer.add_symlink("e1", "a/..", link_opts).unwrap();
            writer.add_symlink("a", ".", link_opts).unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let _ = extract(&archive, &output, 0, TEST_CAP);
        let canonical_root = dunce::canonicalize(&output).unwrap();
        for name in ["e1", "a"] {
            if let Ok(resolved) = dunce::canonicalize(output.join(name)) {
                assert!(
                    resolved.starts_with(&canonical_root),
                    "{name} resolves outside the root: {}",
                    resolved.display()
                );
            }
        }
        assert!(
            std::fs::read(output.join("e1").join("SECRET")).is_err(),
            "no host file may be readable through the tree"
        );
    }

    /// Codex Finding 4 (zip) — both the implicit parents `create_dir_all` opens
    /// AND explicit directory entries (which zip also creates via
    /// `create_dir_all`, never chmodded) must be capped. Runs under umask 000 —
    /// the state a default (022) run cannot distinguish from the fix — and
    /// asserts every created directory has `0o7000|0o022` stripped.
    #[cfg(unix)]
    #[test]
    fn extract_masks_created_directories() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("nested.zip");
        {
            let file = std::fs::File::create(&archive).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            // Deep file → implicit parents `pkg`, `pkg/sub`, `pkg/sub/deep`.
            writer
                .start_file(
                    "pkg/sub/deep/file",
                    SimpleFileOptions::default().unix_permissions(0o644),
                )
                .unwrap();
            writer.write_all(b"x").unwrap();
            // An explicit directory entry declaring a permissive mode.
            writer
                .add_directory("pkg/explicit", SimpleFileOptions::default().unix_permissions(0o777))
                .unwrap();
            writer.finish().unwrap();
        }
        let output = dir.path().join("out");

        let previous = unsafe { libc::umask(0) };
        let result = extract(&archive, &output, 0, TEST_CAP);
        let modes: Vec<(std::path::PathBuf, u32)> = ["pkg", "pkg/sub", "pkg/sub/deep", "pkg/explicit"]
            .iter()
            .map(|rel| {
                let d = output.join(rel);
                let mode = std::fs::symlink_metadata(&d).unwrap().permissions().mode() & 0o7777;
                (d, mode)
            })
            .collect();
        unsafe {
            libc::umask(previous);
        }

        result.unwrap();
        for (d, mode) in modes {
            assert_eq!(
                mode & (0o7000 | 0o022),
                0,
                "created directory {} kept forbidden bits: {mode:04o}",
                d.display()
            );
        }
    }
}
