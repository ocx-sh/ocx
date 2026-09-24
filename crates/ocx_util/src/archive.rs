// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use log::*;

use crate::{compression, fs::path::validate_symlinks_in_dir};

mod backend;
mod error;
mod extract_options;
mod tar;
mod zip;

pub use error::{Error, Result};
pub use extract_options::ExtractOptions;

pub struct Archive {
    inner: Box<dyn backend::Backend>,
}

/// Returns `true` if the path has a `.zip` extension.
fn is_zip(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

/// Multiplier applied to a compressed archive's byte size to bound its
/// decompressed output (CWE-400). Matches the registry pull path's policy in
/// `oci::client`: 100x covers realistic compression ratios for tool binaries
/// with headroom.
const EXTRACTION_CAP_MULTIPLIER: u64 = 100;
/// Floor for the decompression cap, so a tiny compressed input still permits a
/// reasonable extraction. Matches the registry pull path's 256 MiB floor.
const EXTRACTION_CAP_MINIMUM: u64 = 256 << 20;

/// Number of entries between periodic debug log messages during archiving.
const LOG_INTERVAL: u64 = 100;

/// Decompressed-output ceiling for extracting a `compressed_size`-byte archive.
fn extraction_cap(compressed_size: u64) -> u64 {
    compressed_size
        .saturating_mul(EXTRACTION_CAP_MULTIPLIER)
        .max(EXTRACTION_CAP_MINIMUM)
}

/// Strips setuid/setgid/sticky (`0o7000`) and group/other write (`0o022`) from a
/// directory this extractor created. No-op off Unix, where there is no such mode.
#[cfg(unix)]
fn cap_directory_mode(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::symlink_metadata(path)?.permissions().mode();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & !(0o7000 | 0o022)))
}

/// Creates `dir` and every missing ancestor, then caps the mode of each
/// component strictly below `root` (see [`cap_directory_mode`]).
///
/// `create_dir_all` opens implicit parent directories with `0o777 & ~umask`, and
/// zip directory entries the same way — never chmodded afterwards. Under a
/// permissive umask (`000`/`002`, routine in containers and CI) an archive of
/// `a/b/file` would otherwise leave `a` and `b` group/other-writable, retaining
/// exactly the bits the entry-driven `0o022` cap promises to strip (Codex
/// Finding 4). `root` is the extraction root — a scratch/layer dir owned by the
/// caller — so it is left untouched; every component below it was created by this
/// extraction. The mode cap is idempotent, so re-capping a component an earlier
/// entry already created is harmless.
#[cfg_attr(not(unix), allow(unused_variables))] // the cap loop below is unix-only
fn create_dir_all_capped(root: &Path, dir: &Path) -> std::result::Result<(), Error> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Io {
        path: dir.to_path_buf(),
        source: e,
    })?;
    #[cfg(unix)]
    {
        for ancestor in dir.ancestors() {
            if ancestor == root || !ancestor.starts_with(root) {
                break;
            }
            cap_directory_mode(ancestor).map_err(|e| Error::Io {
                path: ancestor.to_path_buf(),
                source: e,
            })?;
        }
    }
    Ok(())
}

/// Sweeps every link under `root` once the entry loop has finished and refuses
/// the extraction if any resolves outside `root` *as the finished tree stands*.
///
/// The per-entry predicate judges each link against the disk as it is when the
/// entry arrives, so it cannot see a hop a LATER entry plants: `e1 -> a/..` with
/// `a` still absent folds to the root and is accepted, then `a -> .` lands and
/// `e1` physically resolves to `root/..`. Nothing is written through it — the
/// ancestor guard refuses that — but the tree now carries an escaping link, and
/// the layer routes have no re-pack to catch it.
///
/// This is a *physical* post-condition, not a second run of the entry-time
/// predicate: an existing link is judged by where it actually lands, so a
/// Windows junction — which the extractor necessarily creates with an absolute
/// substitute name — is judged by its resolution rather than refused for being
/// spelled absolutely. A link that resolves nowhere (dangling) can be walked by
/// nothing and is left alone.
///
/// A refused link is removed before the error is returned, and the sweep
/// repeats until the tree holds none: a per-entry refusal leaves a partial tree
/// the caller discards, but an escaping symlink is not a partial file — it is
/// the published artifact itself, and it must not survive in the scratch root
/// for anything that walks it later. The first refusal is the one reported.
pub(super) fn sweep_symlinks(canonical_root: &Path) -> std::result::Result<(), Error> {
    let mut first: Option<Error> = None;
    while let Some((link, target)) = first_escaping_link(canonical_root, canonical_root)? {
        // `symlink::remove` knows how to take a junction down; `remove_file`
        // does not.
        crate::fs::symlink::remove(&link).map_err(|e| Error::Io {
            path: link.clone(),
            source: std::io::Error::other(e),
        })?;
        first.get_or_insert(Error::SymlinkEscape { link, target });
    }
    first.map_or(Ok(()), Err)
}

/// Depth-first, returns the first link under `dir` that physically resolves
/// outside `canonical_root`, as `(link, target as written)`. Recurses into real
/// directories only — a link to a directory is judged, never entered.
fn first_escaping_link(
    canonical_root: &Path,
    dir: &Path,
) -> std::result::Result<Option<(std::path::PathBuf, std::path::PathBuf)>, Error> {
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |e: std::io::Error| Error::Io { path, source: e }
    };
    for entry in std::fs::read_dir(dir).map_err(io(dir))? {
        let entry = entry.map_err(io(dir))?;
        let path = entry.path();
        if crate::fs::symlink::is_link(&path) {
            if let Ok(resolved) = dunce::canonicalize(&path)
                && !resolved.starts_with(canonical_root)
            {
                let target = std::fs::read_link(&path).unwrap_or(resolved);
                return Ok(Some((path, target)));
            }
        } else if entry.file_type().map_err(io(&path))?.is_dir()
            && let Some(hit) = first_escaping_link(canonical_root, &path)?
        {
            return Ok(Some(hit));
        }
    }
    Ok(None)
}

impl Archive {
    /// Creates a new archive at the given path.
    /// Any existing file at the path will be overwritten.
    /// If the path has a known extension, the corresponding format and compression will be used.
    /// Otherwise, a plain tar archive will be created.
    /// If you want to enforce compression, use `create_with_compression` instead.
    pub async fn create(output: impl AsRef<Path>) -> Result<Self> {
        let output = output.as_ref();
        if is_zip(output) {
            return Ok(Self {
                inner: Box::new(zip::ZipBackend::new(output, compression::CompressionLevel::default())?),
            });
        }
        if let Some(algorithm) = compression::CompressionAlgorithm::from_file(output) {
            Self::create_with_compression(output, compression::CompressionOptions::new(algorithm)).await
        } else {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(output)
                .map_err(|e| error::Error::Io {
                    path: output.to_path_buf(),
                    source: e,
                })?;
            Ok(Self {
                inner: Box::new(tar::TarBackend::new(Box::new(file))),
            })
        }
    }

    /// Creates a new archive at the given path with the given compression options.
    /// For zip archives, the compression level from options is used; the algorithm field is ignored.
    /// For tar archives, the algorithm is inferred from the file extension if not specified.
    pub async fn create_with_compression(
        output: impl AsRef<Path>,
        options: compression::CompressionOptions,
    ) -> Result<Self> {
        let output = output.as_ref();
        if is_zip(output) {
            if let Some(algorithm) = options.algorithm {
                debug!("Compression algorithm '{algorithm}' is ignored for ZIP archives.");
            }
            return Ok(Self {
                inner: Box::new(zip::ZipBackend::new(output, options.level)?),
            });
        }

        let options = match options.algorithm {
            Some(_) => options,
            None => {
                let algorithm = compression::CompressionAlgorithm::from_file(output)
                    .ok_or_else(|| error::Error::UnsupportedFormat(output.display().to_string()))?;
                options.with_algorithm(algorithm)
            }
        };
        let writer = compression::write_file(output, &options).await?;
        Ok(Self {
            inner: Box::new(tar::TarBackend::new(writer)),
        })
    }

    /// Extracts the given archive to the given output path.
    /// If the archive has a known extension, the corresponding format and compression will be used.
    /// Otherwise, a plain tar archive will be assumed.
    pub async fn extract(archive: impl AsRef<Path>, output: impl AsRef<Path>) -> Result<()> {
        Self::extract_with_options(archive, output, None).await
    }

    /// Extracts the given archive to the given output path with the given options.
    /// If the algorithm is not specified, it is inferred from the file extension,
    /// then from the file's magic number; neither matching means a plain tar.
    /// Zip archives are detected by extension; compression options are only used for tar archives.
    pub async fn extract_with_options(
        archive: impl AsRef<Path>,
        output: impl AsRef<Path>,
        options: Option<ExtractOptions>,
    ) -> Result<()> {
        let options = options.unwrap_or_default();
        let archive = archive.as_ref().to_path_buf();
        let output = output.as_ref().to_path_buf();

        // Decompression-bomb cap (CWE-400): a file-path extraction (`ocx package
        // create --extract`, or a locally materialized blob) carries no
        // registry-declared size, so derive the ceiling from the compressed
        // input. The registry pull path caps its decompressor directly
        // (`oci::client`); this is the equivalent guard for the file-path callers.
        let compressed_size = tokio::fs::metadata(&archive)
            .await
            .map(|metadata| metadata.len())
            .map_err(|e| error::Error::Io {
                path: archive.clone(),
                source: e,
            })?;
        let decompressed_cap = extraction_cap(compressed_size);

        if is_zip(&archive) {
            return tokio::task::spawn_blocking(move || {
                zip::extract(&archive, &output, options.strip_components, decompressed_cap)
            })
            .await
            .map_err(error::Error::internal)?;
        }

        // Extension first, then content: a compressed tarball whose name does
        // not say so used to reach the tar parser raw and fail on a garbage
        // checksum (ocx-sh/ocx#283) instead of being decoded.
        let algorithm = match options
            .algorithm
            .or_else(|| compression::CompressionAlgorithm::from_file(&archive))
        {
            Some(algorithm) => Some(algorithm),
            None => compression::CompressionAlgorithm::from_file_magic(&archive).await?,
        };

        let reader: Box<dyn std::io::Read + Send> = if let Some(algorithm) = algorithm {
            compression::read_file(&archive, Some(algorithm)).await?
        } else {
            Box::new(std::fs::File::open(&archive).map_err(|e| error::Error::Io {
                path: archive.clone(),
                source: e,
            })?)
        };

        tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            // take(cap + 1): a well-formed archive stops before the probe byte; a
            // stream that yields cap + 1 decompressed bytes drives `limit()` to 0,
            // which is the bomb signal — reported ahead of any truncation error
            // the capped read would otherwise surface (mirrors `oci::client`).
            let capped = reader.take(decompressed_cap.saturating_add(1));
            let (result, capped) = tar::extract_returning_reader(capped, &output, options.strip_components);
            if capped.limit() == 0 {
                return Err(error::Error::ExtractionCapExceeded { cap: decompressed_cap });
            }
            result
        })
        .await
        .map_err(error::Error::internal)?
    }

    pub async fn add_file(&mut self, archive_path: impl AsRef<Path>, file: impl AsRef<Path>) -> Result<()> {
        self.inner
            .add_file(archive_path.as_ref().to_path_buf(), file.as_ref().to_path_buf())
            .await
    }

    pub async fn add_dir(&mut self, archive_path: impl AsRef<Path>, dir: impl AsRef<Path>) -> Result<()> {
        self.inner
            .add_dir(archive_path.as_ref().to_path_buf(), dir.as_ref().to_path_buf())
            .await
    }

    pub async fn add_dir_all(&mut self, archive_path: impl AsRef<Path>, dir: impl AsRef<Path>) -> Result<()> {
        let dir = dir.as_ref();
        validate_symlinks_in_dir(dir, dir)?;
        self.inner
            .add_dir_all(archive_path.as_ref().to_path_buf(), dir.to_path_buf())
            .await
    }

    pub async fn finish(self) -> Result<()> {
        self.inner.finish().await
    }
}

/// Extracts a tar archive from a sync reader to `output`, returning the reader
/// after extraction.
///
/// This is the streaming-pipeline entry point for `oci::client::Client::pull_layer`:
/// the tar extractor is driven from inside a `spawn_blocking` closure that holds the
/// `tokio_util::io::SyncIoBridge` over the async decompressor. The function applies
/// the same path-safety rules (escape check, strip_components) as
/// [`Archive::extract_with_options`].
///
/// Returning the reader allows the caller to recover state accumulated during the
/// read (e.g. a digest computed by a hashing wrapper). On error the reader may be
/// partially consumed.
///
/// # Why a separate function
///
/// [`Archive::extract_with_options`] accepts a file path and opens its own
/// `spawn_blocking` internally, which cannot be nested inside the caller's
/// `spawn_blocking`. This function accepts an already-open sync reader so the caller
/// manages the blocking boundary.
// `pub` rather than `pub(crate)`: its one consumer, `ocx_oci`'s layer-pull
// pipeline, is another crate now. The one visibility widening this extraction
// needs.
pub fn extract_tar_from_reader<R: std::io::Read>(reader: R, output: &Path, strip_components: usize) -> (Result<()>, R) {
    tar::extract_returning_reader(reader, output, strip_components)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_extraction_strip_components() {
        let archive_xz = ocx_test_support::data::archive_xz();
        println!("Archive path: {:?}", archive_xz);
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("output");
        Archive::extract_with_options(
            archive_xz,
            &output,
            Some(ExtractOptions {
                algorithm: None,
                strip_components: 2,
            }),
        )
        .await
        .expect("Failed to extract archive.");
        assert!(!output.join("level_0.txt").exists());
        assert!(!output.join("content_0.txt").exists());
        assert!(output.join("content_0_0.txt").exists());
    }

    /// A compressed tarball whose name carries no compression extension is
    /// decoded by its magic number instead of reaching the tar parser raw,
    /// which failed on a garbage checksum (ocx-sh/ocx#283).
    #[tokio::test]
    async fn extraction_detects_compression_by_magic_without_an_extension() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("file.txt"), b"content").unwrap();

        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("pkg.tar.gz");
        let mut archive = Archive::create(&named).await.unwrap();
        archive.add_file("file.txt", src.path().join("file.txt")).await.unwrap();
        archive.finish().await.unwrap();
        let unnamed = dir.path().join("pkg");
        std::fs::rename(&named, &unnamed).unwrap();

        let output = dir.path().join("output");
        Archive::extract(&unnamed, &output)
            .await
            .expect("a gzip tarball must extract whatever its name");
        assert_eq!(std::fs::read(output.join("file.txt")).unwrap(), b"content");
    }

    /// A plain tar whose first entry name starts with the printable bzip2
    /// prefix `BZh` must still extract as a plain tar.
    #[tokio::test]
    async fn a_plain_tar_starting_with_bzh_is_not_taken_for_bzip2() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("BZh-notes.txt"), b"notes").unwrap();

        let dir = tempfile::tempdir().unwrap();
        let named = dir.path().join("pkg.tar");
        let mut archive = Archive::create(&named).await.unwrap();
        archive
            .add_file("BZh-notes.txt", src.path().join("BZh-notes.txt"))
            .await
            .unwrap();
        archive.finish().await.unwrap();
        let unnamed = dir.path().join("pkg");
        std::fs::rename(&named, &unnamed).unwrap();

        let output = dir.path().join("output");
        Archive::extract(&unnamed, &output)
            .await
            .expect("a plain tar must extract");
        assert_eq!(std::fs::read(output.join("BZh-notes.txt")).unwrap(), b"notes");
    }

    /// strip_components works for zip archives.
    #[tokio::test]
    async fn test_extraction_strip_components_zip() {
        // Build a zip with nested structure: top/sub/file.txt
        let src = tempfile::tempdir().unwrap();
        let nested = src.path().join("top").join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("file.txt"), b"deep content").unwrap();
        std::fs::write(src.path().join("top").join("root.txt"), b"root content").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("nested.zip");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive.add_dir_all("top", src.path().join("top")).await.unwrap();
        archive.finish().await.unwrap();

        // strip 1 component: "top/" removed — zip detected by extension
        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract_with_options(
            &archive_path,
            extract_dir.path(),
            Some(ExtractOptions {
                algorithm: None,
                strip_components: 1,
            }),
        )
        .await
        .expect("extraction with strip_components failed");

        assert!(
            !extract_dir.path().join("top").exists(),
            "top-level dir should be stripped"
        );
        assert!(
            extract_dir.path().join("root.txt").exists(),
            "root.txt should be at top level"
        );
        assert!(
            extract_dir.path().join("sub/file.txt").exists(),
            "sub/file.txt should exist"
        );
        assert_eq!(
            std::fs::read(extract_dir.path().join("sub/file.txt")).unwrap(),
            b"deep content"
        );
    }

    /// Uncompressed tar round-trip (no compression extension).
    #[tokio::test]
    async fn test_round_trip_plain_tar() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("hello.txt"), b"world").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive
            .add_file("hello.txt", src.path().join("hello.txt"))
            .await
            .unwrap();
        archive.finish().await.unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("plain tar extraction failed");

        assert_eq!(std::fs::read(extract_dir.path().join("hello.txt")).unwrap(), b"world");
    }

    /// zstd tar round-trip through the `Archive` facade (`.tar.zst`).
    #[tokio::test]
    async fn test_round_trip_zstd() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("hello.txt"), b"world").unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar.zst");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive
            .add_file("hello.txt", src.path().join("hello.txt"))
            .await
            .unwrap();
        archive.finish().await.unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("zstd tar extraction failed");

        assert_eq!(std::fs::read(extract_dir.path().join("hello.txt")).unwrap(), b"world");
    }

    #[test]
    fn test_validate_symlink_same_dir() {
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("sibling")).is_ok());
    }

    #[test]
    fn test_validate_symlink_parent_dir_within_root() {
        // link is in a subdirectory, target goes up one level but stays in root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/sub/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("../file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_into_sibling_dir() {
        // link in sub/link -> ../other/file (stays within root)
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/sub/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("../other/file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_deeply_nested_up_to_root_boundary() {
        // link at depth 3, goes up exactly 3 levels to root — still within root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/a/b/c/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("../../../file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_escapes_by_one_level() {
        // link at depth 1, target goes up 2 levels — escapes root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/sub/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("../../etc/passwd")).is_err());
    }

    #[test]
    fn test_validate_symlink_escapes_from_top_level() {
        // link at root level, target goes up — escapes
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("../outside")).is_err());
    }

    #[test]
    fn test_validate_symlink_absolute_target() {
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn test_validate_symlink_dot_components() {
        // ./sibling is equivalent to sibling — should be fine
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("./sibling")).is_ok());
    }

    #[test]
    fn test_validate_symlink_complex_path_within_root() {
        // sub/../other/./file normalizes to other/file — stays within root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("sub/../other/./file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_complex_path_escaping() {
        // sub/../../outside normalizes to ../outside — escapes
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(crate::fs::symlink::validate_target(root, link, Path::new("sub/../../outside")).is_err());
    }

    /// Relative symlinks that stay within root survive a tar round-trip.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_relative_symlinks_within_root_tar() {
        use std::os::unix::fs::symlink;

        let src = tempfile::tempdir().unwrap();
        // src/
        //   lib/
        //     libfoo.so
        //   bin/
        //     tool -> ../lib/libfoo.so      (cross-directory, stays in root)
        //     alias -> tool                  (same-directory)
        let lib = src.path().join("lib");
        let bin = src.path().join("bin");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(lib.join("libfoo.so"), b"library content").unwrap();
        symlink("../lib/libfoo.so", bin.join("tool")).unwrap();
        symlink("tool", bin.join("alias")).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar.xz");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive
            .add_dir_all("", src.path())
            .await
            .expect("valid symlinks should be accepted");
        archive.finish().await.unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("extraction should succeed");

        // Verify symlinks are preserved as symlinks with correct targets.
        let extracted_tool = extract_dir.path().join("bin/tool");
        assert!(extracted_tool.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&extracted_tool).unwrap().to_str().unwrap(),
            "../lib/libfoo.so"
        );

        let extracted_alias = extract_dir.path().join("bin/alias");
        assert!(extracted_alias.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&extracted_alias).unwrap().to_str().unwrap(), "tool");

        // Verify the symlink chain actually resolves.
        assert_eq!(std::fs::read(&extracted_tool).unwrap(), b"library content");
        assert_eq!(std::fs::read(&extracted_alias).unwrap(), b"library content");
    }

    /// Relative symlinks that stay within root survive a zip round-trip.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_relative_symlinks_within_root_zip() {
        use std::os::unix::fs::symlink;

        let src = tempfile::tempdir().unwrap();
        let lib = src.path().join("lib");
        let bin = src.path().join("bin");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(lib.join("libfoo.so"), b"library content").unwrap();
        symlink("../lib/libfoo.so", bin.join("tool")).unwrap();
        symlink("tool", bin.join("alias")).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.zip");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        archive
            .add_dir_all("", src.path())
            .await
            .expect("valid symlinks should be accepted");
        archive.finish().await.unwrap();

        let extract_dir = tempfile::tempdir().unwrap();
        Archive::extract(&archive_path, extract_dir.path())
            .await
            .expect("extraction should succeed");

        let extracted_tool = extract_dir.path().join("bin/tool");
        assert!(extracted_tool.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_link(&extracted_tool).unwrap().to_str().unwrap(),
            "../lib/libfoo.so"
        );

        let extracted_alias = extract_dir.path().join("bin/alias");
        assert!(extracted_alias.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(&extracted_alias).unwrap().to_str().unwrap(), "tool");

        assert_eq!(std::fs::read(&extracted_tool).unwrap(), b"library content");
        assert_eq!(std::fs::read(&extracted_alias).unwrap(), b"library content");
    }

    /// Escaping symlinks are rejected during archive creation.
    #[cfg(unix)]
    #[tokio::test]
    async fn test_create_rejects_escaping_symlink() {
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("legit.txt"), b"ok").unwrap();
        std::os::unix::fs::symlink("../../etc/passwd", src.path().join("evil")).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("pkg.tar.xz");

        let mut archive = Archive::create(&archive_path).await.unwrap();
        let result = archive.add_dir_all("", src.path()).await;
        assert!(result.is_err(), "should reject escaping symlink during creation");
    }

    /// Escaping symlinks in tar archives are rejected during extraction.
    #[tokio::test]
    async fn test_extract_rejects_escaping_symlink_tar() {
        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("malicious.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            let mut header = ::tar::Header::new_gnu();
            header.set_entry_type(::tar::EntryType::Symlink);
            header.set_size(0);
            header.set_path("evil-link").unwrap();
            header.set_link_name("../../etc/passwd").unwrap();
            header.set_cksum();
            builder.append(&header, &b""[..]).unwrap();
            builder.finish().unwrap();
        }

        let extract_dir = tempfile::tempdir().unwrap();
        let result = Archive::extract(&archive_path, extract_dir.path()).await;
        assert!(result.is_err(), "should reject escaping symlink in tar");
        assert!(
            !extract_dir.path().join("evil-link").exists(),
            "escaping symlink should not be created"
        );
    }

    /// Escaping symlinks in zip archives are rejected during extraction.
    #[tokio::test]
    async fn test_extract_rejects_escaping_symlink_zip() {
        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("malicious.zip");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut writer = ::zip::ZipWriter::new(file);
            let options = ::zip::write::SimpleFileOptions::default();
            writer.add_symlink("evil-link", "../../etc/passwd", options).unwrap();
            writer.finish().unwrap();
        }

        let extract_dir = tempfile::tempdir().unwrap();
        let result = Archive::extract(&archive_path, extract_dir.path()).await;
        assert!(result.is_err(), "should reject escaping symlink in zip");
        assert!(
            !extract_dir.path().join("evil-link").exists(),
            "escaping symlink should not be created"
        );
    }

    /// Tar entries with path traversal in the entry name are rejected.
    #[tokio::test]
    async fn test_extract_rejects_path_traversal_tar() {
        let out_dir = tempfile::tempdir().unwrap();
        let archive_path = out_dir.path().join("traversal.tar");
        {
            let file = std::fs::File::create(&archive_path).unwrap();
            let mut builder = ::tar::Builder::new(file);
            // Write the path directly into the header bytes to bypass tar crate validation.
            let mut header = ::tar::Header::new_gnu();
            header.set_entry_type(::tar::EntryType::Regular);
            header.set_size(5);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.as_gnu_mut().unwrap().name[..14].copy_from_slice(b"../outside.txt");
            header.set_cksum();
            builder.append(&header, &b"hello"[..]).unwrap();
            builder.finish().unwrap();
        }

        let extract_dir = tempfile::tempdir().unwrap();
        let result = Archive::extract(&archive_path, extract_dir.path()).await;
        assert!(result.is_err(), "should reject entry with path traversal");
    }
}
