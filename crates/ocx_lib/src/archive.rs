// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Component, Path, PathBuf};

use crate::{Result, compression, log::*};

mod backend;
mod error;
mod extract_options;
mod tar;
mod zip;

pub use error::Error;
pub use extract_options::ExtractOptions;

pub struct Archive {
    inner: Box<dyn backend::Backend>,
}

/// Returns `true` if the path has a `.zip` extension.
fn is_zip(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
}

/// Lexically normalizes a path by resolving `.` and `..` components without filesystem access.
fn normalize_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                } else if !matches!(components.last(), Some(Component::RootDir | Component::Prefix(_))) {
                    components.push(component);
                }
            }
            Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Returns `true` if the normalized path contains any `..` components,
/// meaning it escapes its logical root.
fn escapes_root(path: &Path) -> bool {
    normalize_path(path)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

/// Validates that a symlink target resolves within `root`.
///
/// `link_path` is the absolute path where the symlink is (or will be) located.
/// `target` is the raw symlink target (typically relative).
fn validate_symlink_target(root: &Path, link_path: &Path, target: &Path) -> Result<()> {
    if target.is_absolute() {
        return Err(error::Error::SymlinkEscape {
            link: link_path.to_path_buf(),
            target: target.to_path_buf(),
        }
        .into());
    }
    let parent = link_path.parent().unwrap_or(root);
    let resolved = normalize_path(&parent.join(target));
    let normalized_root = normalize_path(root);
    if !resolved.starts_with(&normalized_root) {
        return Err(error::Error::SymlinkEscape {
            link: link_path.to_path_buf(),
            target: target.to_path_buf(),
        }
        .into());
    }
    Ok(())
}

/// Recursively validates that all symlinks under `dir` resolve within `root`.
fn validate_symlinks_in_dir(root: &Path, dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| error::Error::Io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| error::Error::Io(dir.to_path_buf(), e))?;
        let ft = entry.file_type().map_err(|e| error::Error::Io(entry.path(), e))?;
        if ft.is_symlink() {
            let target = std::fs::read_link(entry.path()).map_err(|e| error::Error::Io(entry.path(), e))?;
            validate_symlink_target(root, &entry.path(), &target)?;
        } else if ft.is_dir() {
            validate_symlinks_in_dir(root, &entry.path())?;
        }
    }
    Ok(())
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
                .map_err(|e| error::Error::Io(output.to_path_buf(), e))?;
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
                    .ok_or_else(|| crate::Error::UnsupportedArchive(output.display().to_string()))?;
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
    /// If the algorithm is not specified, it will be inferred from the file extension.
    /// Zip archives are detected by extension; compression options are only used for tar archives.
    pub async fn extract_with_options(
        archive: impl AsRef<Path>,
        output: impl AsRef<Path>,
        options: Option<ExtractOptions>,
    ) -> Result<()> {
        let options = options.unwrap_or_default();
        let archive = archive.as_ref().to_path_buf();
        let output = output.as_ref().to_path_buf();

        // Capture the current span so it propagates into spawn_blocking,
        // allowing `pb_inc(1)` inside extract to update the progress bar.
        let span = tracing::Span::current();

        if is_zip(&archive) {
            return tokio::task::spawn_blocking(move || {
                let _guard = span.entered();
                zip::extract(&archive, &output, options.strip_components)
            })
            .await
            .map_err(|e| error::Error::Internal(e.to_string()))?;
        }

        let algorithm = options
            .algorithm
            .or_else(|| compression::CompressionAlgorithm::from_file(&archive));

        let reader: Box<dyn std::io::Read + Send> = if let Some(algorithm) = algorithm {
            compression::read_file(&archive, Some(algorithm)).await?
        } else {
            Box::new(std::fs::File::open(&archive).map_err(|e| error::Error::Io(archive.clone(), e))?)
        };

        tokio::task::spawn_blocking(move || {
            let _guard = span.entered();
            tar::extract(reader, &output, options.strip_components)
        })
        .await
        .map_err(|e| error::Error::Internal(e.to_string()))?
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    #[tokio::test]
    async fn test_extraction_strip_components() {
        let archive_xz = test::data::archive_xz();
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

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize_path(Path::new("/a/../../b")), PathBuf::from("/b"));
        assert_eq!(normalize_path(Path::new("a/./b")), PathBuf::from("a/b"));
        assert_eq!(normalize_path(Path::new("a/../b")), PathBuf::from("b"));
        assert_eq!(normalize_path(Path::new("../../x")), PathBuf::from("../../x"));
    }

    #[test]
    fn test_validate_symlink_same_dir() {
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(validate_symlink_target(root, link, Path::new("sibling")).is_ok());
    }

    #[test]
    fn test_validate_symlink_parent_dir_within_root() {
        // link is in a subdirectory, target goes up one level but stays in root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/sub/link");
        assert!(validate_symlink_target(root, link, Path::new("../file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_into_sibling_dir() {
        // link in sub/link -> ../other/file (stays within root)
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/sub/link");
        assert!(validate_symlink_target(root, link, Path::new("../other/file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_deeply_nested_up_to_root_boundary() {
        // link at depth 3, goes up exactly 3 levels to root — still within root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/a/b/c/link");
        assert!(validate_symlink_target(root, link, Path::new("../../../file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_escapes_by_one_level() {
        // link at depth 1, target goes up 2 levels — escapes root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/sub/link");
        assert!(validate_symlink_target(root, link, Path::new("../../etc/passwd")).is_err());
    }

    #[test]
    fn test_validate_symlink_escapes_from_top_level() {
        // link at root level, target goes up — escapes
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(validate_symlink_target(root, link, Path::new("../outside")).is_err());
    }

    #[test]
    fn test_validate_symlink_absolute_target() {
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(validate_symlink_target(root, link, Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn test_validate_symlink_dot_components() {
        // ./sibling is equivalent to sibling — should be fine
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(validate_symlink_target(root, link, Path::new("./sibling")).is_ok());
    }

    #[test]
    fn test_validate_symlink_complex_path_within_root() {
        // sub/../other/./file normalizes to other/file — stays within root
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(validate_symlink_target(root, link, Path::new("sub/../other/./file")).is_ok());
    }

    #[test]
    fn test_validate_symlink_complex_path_escaping() {
        // sub/../../outside normalizes to ../outside — escapes
        let root = Path::new("/tmp/root");
        let link = Path::new("/tmp/root/link");
        assert!(validate_symlink_target(root, link, Path::new("sub/../../outside")).is_err());
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
