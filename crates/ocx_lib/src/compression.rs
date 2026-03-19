// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{ErrorExt, MEDIA_TYPE_TAR_GZ, MEDIA_TYPE_TAR_XZ, Result};

/// Enumeration of supported compression algorithms.
#[derive(Debug, Clone, Copy)]
pub enum CompressionAlgorithm {
    None,
    Lzma,
    Gzip,
}

impl CompressionAlgorithm {
    /// Infers the compression algorithm from the file extension of the given path.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let path = path.as_ref();
        match path.extension()?.to_str()? {
            "xz" => Some(CompressionAlgorithm::Lzma),
            "gz" | "tgz" => Some(CompressionAlgorithm::Gzip),
            _ => None,
        }
    }

    /// Infers the compression algorithm from the given media type.
    pub fn from_media_type(media_type: impl AsRef<str>) -> Option<Self> {
        match media_type.as_ref() {
            MEDIA_TYPE_TAR_GZ => Some(CompressionAlgorithm::Gzip),
            MEDIA_TYPE_TAR_XZ => Some(CompressionAlgorithm::Lzma),
            _ => None,
        }
    }
}

impl std::fmt::Display for CompressionAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionAlgorithm::None => write!(f, "none"),
            CompressionAlgorithm::Lzma => write!(f, "lzma"),
            CompressionAlgorithm::Gzip => write!(f, "gzip"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum CompressionLevel {
    Fast,
    Best,
    #[default]
    Default,
}

impl From<CompressionLevel> for lzma_rust2::XzOptions {
    fn from(val: CompressionLevel) -> Self {
        match val {
            CompressionLevel::Fast => lzma_rust2::XzOptions::with_preset(0),
            CompressionLevel::Best => lzma_rust2::XzOptions::with_preset(9),
            CompressionLevel::Default => lzma_rust2::XzOptions::with_preset(3),
        }
    }
}

impl From<CompressionLevel> for flate2::Compression {
    fn from(val: CompressionLevel) -> Self {
        match val {
            CompressionLevel::Fast => flate2::Compression::fast(),
            CompressionLevel::Best => flate2::Compression::best(),
            CompressionLevel::Default => flate2::Compression::default(),
        }
    }
}

/// Returns the default number of compression threads.
/// Uses all available CPU cores, capped at 16 to limit memory on high-core machines.
/// Falls back to 1 (single-threaded) if parallelism cannot be determined.
pub fn default_threads() -> u32 {
    std::thread::available_parallelism()
        .map(|n| (n.get() as u32).min(16))
        .unwrap_or(1)
}

/// Options for compression.
///
/// Thread semantics for LZMA (`threads` field):
/// - `0` (default) = auto-detect (all available cores, capped at 16)
/// - `1` = single-threaded
/// - `n` where n > 1 = use n threads via `XzWriterMt`
#[derive(Default)]
pub struct CompressionOptions {
    pub algorithm: Option<CompressionAlgorithm>,
    pub level: CompressionLevel,
    pub threads: u32,
}

impl CompressionOptions {
    pub fn new(algorithm: CompressionAlgorithm) -> Self {
        Self {
            algorithm: Some(algorithm),
            ..Default::default()
        }
    }

    pub fn from_level(level: CompressionLevel) -> Self {
        Self {
            level,
            ..Default::default()
        }
    }

    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let algorithm = CompressionAlgorithm::from_file(path).map_to_undefined_error()?;
        Ok(Self {
            algorithm: Some(algorithm),
            ..Default::default()
        })
    }

    pub fn with_algorithm(mut self, algorithm: CompressionAlgorithm) -> Self {
        self.algorithm = Some(algorithm);
        self
    }

    pub fn with_level(mut self, level: CompressionLevel) -> Self {
        self.level = level;
        self
    }

    pub fn with_threads(mut self, threads: u32) -> Self {
        self.threads = threads;
        self
    }

    /// Resolves the effective thread count.
    /// `0` → `default_threads()`, otherwise returns the value as-is.
    pub fn threads_or_default(&self) -> u32 {
        if self.threads == 0 {
            default_threads()
        } else {
            self.threads
        }
    }
}

mod xz {
    /// Wraps [`lzma_rust2::XzWriter`] and calls [`lzma_rust2::XzWriter::finish`] on drop.
    ///
    /// `XzWriter` does not implement `Drop` itself, so when it is erased to
    /// `Box<dyn Write>` the XZ stream footer is never written unless `finish()` is
    /// called explicitly.  This wrapper restores that guarantee.
    pub struct WriterWrapper<W: std::io::Write>(pub Option<lzma_rust2::XzWriter<W>>);

    impl<W: std::io::Write> std::io::Write for WriterWrapper<W> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.as_mut().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.as_mut().unwrap().flush()
        }
    }

    impl<W: std::io::Write> Drop for WriterWrapper<W> {
        fn drop(&mut self) {
            if let Some(w) = self.0.take() {
                let _ = w.finish(); // best-effort; errors cannot be surfaced from Drop
            }
        }
    }

    /// Wraps [`lzma_rust2::XzWriterMt`] and calls `finish()` on drop.
    pub struct MtWriterWrapper<W: std::io::Write>(pub Option<lzma_rust2::XzWriterMt<W>>);

    impl<W: std::io::Write> std::io::Write for MtWriterWrapper<W> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.as_mut().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.as_mut().unwrap().flush()
        }
    }

    impl<W: std::io::Write> Drop for MtWriterWrapper<W> {
        fn drop(&mut self) {
            if let Some(w) = self.0.take() {
                let _ = w.finish(); // best-effort; errors cannot be surfaced from Drop
            }
        }
    }
}

/// Opens a writer for the given file and compression options.
/// If the algorithm is not specified, it will be inferred from the file extension of the output path.
/// The file will be created if it does not exist, and truncated if it does exist.
///
/// For LZMA, uses `threads_or_default()` to resolve the thread count. When > 1, uses `XzWriterMt`
/// for multi-threaded compression with a 4 MiB block size. Otherwise uses single-threaded compression.
pub async fn write_file(
    file: impl AsRef<std::path::Path>,
    options: &CompressionOptions,
) -> Result<Box<dyn std::io::Write + Send>> {
    let algorithm = match options.algorithm {
        Some(algorithm) => algorithm,
        None => CompressionAlgorithm::from_file(&file).map_to_undefined_error()?,
    };
    let level = options.level;
    let threads = options.threads_or_default();
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(file)
        .map_to_undefined_error()?;
    let writer: Box<dyn std::io::Write + Send> = match algorithm {
        CompressionAlgorithm::Lzma if threads > 1 => {
            let mut xz_options: lzma_rust2::XzOptions = level.into();
            // 4 MiB block size — matches pixz and xz --block-size defaults
            xz_options.set_block_size(Some(std::num::NonZeroU64::new(4 * 1024 * 1024).unwrap()));
            let writer = lzma_rust2::XzWriterMt::new(output, xz_options, threads).map_to_undefined_error()?;
            Box::new(xz::MtWriterWrapper(Some(writer)))
        }
        CompressionAlgorithm::Lzma => {
            let writer = lzma_rust2::XzWriter::new(output, level.into()).map_to_undefined_error()?;
            Box::new(xz::WriterWrapper(Some(writer)))
        }
        CompressionAlgorithm::Gzip => {
            let writer = flate2::write::GzEncoder::new(output, level.into());
            Box::new(writer)
        }
        CompressionAlgorithm::None => Box::new(output),
    };
    Ok(writer)
}

/// Opens a reader for the given file.
/// If the algorithm is not specified, it will be tried to infer it from the file extension.
pub async fn read_file(
    file: impl AsRef<std::path::Path>,
    algorithm: Option<CompressionAlgorithm>,
) -> Result<Box<dyn std::io::Read + Send>> {
    let algorithm = match algorithm {
        Some(algorithm) => algorithm,
        None => CompressionAlgorithm::from_file(&file).map_to_undefined_error()?,
    };
    match algorithm {
        CompressionAlgorithm::Lzma => {
            let file = std::fs::File::open(file).map_to_undefined_error()?;
            Ok(Box::new(lzma_rust2::XzReader::new(file, false)))
        }
        CompressionAlgorithm::Gzip => {
            let file = std::fs::File::open(file).map_to_undefined_error()?;
            Ok(Box::new(flate2::read::GzDecoder::new(file)))
        }
        CompressionAlgorithm::None => {
            let file = std::fs::File::open(file).map_to_undefined_error()?;
            Ok(Box::new(file))
        }
    }
}
