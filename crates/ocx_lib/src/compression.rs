// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use crate::{MEDIA_TYPE_TAR_GZ, MEDIA_TYPE_TAR_XZ, MEDIA_TYPE_TAR_ZSTD, Result};

/// Enumeration of supported compression algorithms.
#[derive(Debug, Clone, Copy)]
pub enum CompressionAlgorithm {
    None,
    Lzma,
    Gzip,
    Zstd,
}

impl CompressionAlgorithm {
    /// Infers the compression algorithm from the file extension of the given path.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let path = path.as_ref();
        match path.extension()?.to_str()? {
            "xz" => Some(CompressionAlgorithm::Lzma),
            "gz" | "tgz" => Some(CompressionAlgorithm::Gzip),
            "zst" | "zstd" | "tzst" => Some(CompressionAlgorithm::Zstd),
            _ => None,
        }
    }

    /// Infers the compression algorithm from the given media type.
    pub fn from_media_type(media_type: impl AsRef<str>) -> Option<Self> {
        match media_type.as_ref() {
            MEDIA_TYPE_TAR_GZ => Some(CompressionAlgorithm::Gzip),
            MEDIA_TYPE_TAR_XZ => Some(CompressionAlgorithm::Lzma),
            MEDIA_TYPE_TAR_ZSTD => Some(CompressionAlgorithm::Zstd),
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
            CompressionAlgorithm::Zstd => write!(f, "zstd"),
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

impl CompressionLevel {
    /// Maps to a zstd compression level. zstd accepts 1–22; `3` is zstd's own
    /// default. Mirrors the xz/gzip preset intent: `Fast` = low CPU, `Best` =
    /// max ratio (19 is the highest non-`--ultra` level), `Default` = library
    /// default.
    fn zstd_level(self) -> i32 {
        match self {
            CompressionLevel::Fast => 1,
            CompressionLevel::Best => 19,
            CompressionLevel::Default => 3,
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
        let path = path.as_ref();
        let algorithm =
            CompressionAlgorithm::from_file(path).ok_or_else(|| error::Error::UnknownFormat(path.to_path_buf()))?;
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
            self.0.as_mut().expect("writer used after drop").write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.as_mut().expect("writer used after drop").flush()
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
            self.0.as_mut().expect("writer used after drop").write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.as_mut().expect("writer used after drop").flush()
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
    let file = file.as_ref();
    let algorithm = match options.algorithm {
        Some(algorithm) => algorithm,
        None => CompressionAlgorithm::from_file(file).ok_or_else(|| error::Error::UnknownFormat(file.to_path_buf()))?,
    };
    let level = options.level;
    let threads = options.threads_or_default();
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(file)
        .map_err(|e| error::Error::Create {
            path: file.to_path_buf(),
            source: e,
        })?;
    let writer: Box<dyn std::io::Write + Send> = match algorithm {
        CompressionAlgorithm::Lzma if threads > 1 => {
            let mut xz_options: lzma_rust2::XzOptions = level.into();
            // 4 MiB block size — matches pixz and xz --block-size defaults
            xz_options.set_block_size(Some(
                std::num::NonZeroU64::new(4 * 1024 * 1024).expect("non-zero literal"),
            ));
            let writer = lzma_rust2::XzWriterMt::new(output, xz_options, threads)
                .map_err(|e| error::Error::EngineInit(Box::new(e)))?;
            Box::new(xz::MtWriterWrapper(Some(writer)))
        }
        CompressionAlgorithm::Lzma => {
            let writer =
                lzma_rust2::XzWriter::new(output, level.into()).map_err(|e| error::Error::EngineInit(Box::new(e)))?;
            Box::new(xz::WriterWrapper(Some(writer)))
        }
        CompressionAlgorithm::Gzip => {
            let writer = flate2::write::GzEncoder::new(output, level.into());
            Box::new(writer)
        }
        CompressionAlgorithm::Zstd => {
            let mut encoder = zstd::stream::write::Encoder::new(output, level.zstd_level())
                .map_err(|e| error::Error::EngineInit(Box::new(e)))?;
            // Threading parity with LZMA: spawn worker threads only when threads > 1.
            // A zstd worker count of 0 means single-threaded; `multithread` requires
            // the `zstdmt` crate feature to take effect. Must be set before the first
            // write, so it is configured here right after construction.
            if threads > 1 {
                encoder
                    .multithread(threads)
                    .map_err(|e| error::Error::EngineInit(Box::new(e)))?;
            }
            // `auto_finish` writes the zstd frame epilogue on drop, restoring the
            // same finish-on-drop guarantee the XZ `WriterWrapper` provides once the
            // writer is erased to `Box<dyn Write>`.
            Box::new(encoder.auto_finish())
        }
        CompressionAlgorithm::None => Box::new(output),
    };
    Ok(writer)
}

/// Buffered-read capacity used by [`read_file`] to coalesce small reads from
/// the decompressor into fewer filesystem syscalls.
///
/// 256 KiB matches the typical XZ block read-ahead size and keeps I/O
/// syscall count low without increasing working-set memory significantly.
const READ_FILE_BUF_CAPACITY: usize = 256 * 1024;

/// Opens a reader for the given file.
/// If the algorithm is not specified, it will be tried to infer it from the file extension.
///
/// The compressed-format paths (Lzma, Gzip) wrap the underlying file in a
/// [`std::io::BufReader`] with a 256 KiB buffer before handing it to the
/// decoder. This coalesces the many small reads that decompressors issue into
/// larger filesystem operations, reducing syscall count on large blobs.
pub async fn read_file(
    file: impl AsRef<std::path::Path>,
    algorithm: Option<CompressionAlgorithm>,
) -> Result<Box<dyn std::io::Read + Send>> {
    let file = file.as_ref();
    let algorithm = match algorithm {
        Some(algorithm) => algorithm,
        None => CompressionAlgorithm::from_file(file).ok_or_else(|| error::Error::UnknownFormat(file.to_path_buf()))?,
    };
    match algorithm {
        CompressionAlgorithm::Lzma => {
            let handle = std::fs::File::open(file).map_err(|e| error::Error::Open {
                path: file.to_path_buf(),
                source: e,
            })?;
            let buffered = std::io::BufReader::with_capacity(READ_FILE_BUF_CAPACITY, handle);
            Ok(Box::new(lzma_rust2::XzReader::new(buffered, false)))
        }
        CompressionAlgorithm::Gzip => {
            let handle = std::fs::File::open(file).map_err(|e| error::Error::Open {
                path: file.to_path_buf(),
                source: e,
            })?;
            let buffered = std::io::BufReader::with_capacity(READ_FILE_BUF_CAPACITY, handle);
            Ok(Box::new(flate2::read::GzDecoder::new(buffered)))
        }
        CompressionAlgorithm::Zstd => {
            let handle = std::fs::File::open(file).map_err(|e| error::Error::Open {
                path: file.to_path_buf(),
                source: e,
            })?;
            let buffered = std::io::BufReader::with_capacity(READ_FILE_BUF_CAPACITY, handle);
            // `with_buffer` consumes the existing `BufReader` instead of wrapping it
            // in a second one (which `Decoder::new` would do). The file is already
            // open, so a failure here is decoder-context allocation, not file I/O —
            // classified as `EngineInit` to match the zstd write path.
            let decoder = zstd::stream::read::Decoder::with_buffer(buffered)
                .map_err(|e| error::Error::EngineInit(Box::new(e)))?;
            Ok(Box::new(decoder))
        }
        CompressionAlgorithm::None => {
            let handle = std::fs::File::open(file).map_err(|e| error::Error::Open {
                path: file.to_path_buf(),
                source: e,
            })?;
            Ok(Box::new(handle))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;
    use crate::MEDIA_TYPE_TAR_ZSTD;

    #[test]
    fn from_file_infers_zstd() {
        for name in ["pkg.tar.zst", "pkg.tzst", "pkg.tar.zstd"] {
            assert!(
                matches!(CompressionAlgorithm::from_file(name), Some(CompressionAlgorithm::Zstd)),
                "{name} should infer zstd"
            );
        }
    }

    #[test]
    fn from_media_type_infers_zstd() {
        assert!(matches!(
            CompressionAlgorithm::from_media_type(MEDIA_TYPE_TAR_ZSTD),
            Some(CompressionAlgorithm::Zstd)
        ));
    }

    #[test]
    fn display_zstd() {
        assert_eq!(CompressionAlgorithm::Zstd.to_string(), "zstd");
    }

    /// Deterministic, modestly compressible payload of `len` bytes.
    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    async fn round_trip_zstd(threads: u32, len: usize) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.tar.zst");
        let data = payload(len);

        let options = CompressionOptions::new(CompressionAlgorithm::Zstd).with_threads(threads);
        {
            let mut writer = write_file(&path, &options).await.unwrap();
            writer.write_all(&data).unwrap();
            // Drop the writer to trigger `auto_finish`, writing the zstd epilogue.
            // Without it the stream is truncated and decode fails.
        }

        let mut reader = read_file(&path, Some(CompressionAlgorithm::Zstd)).await.unwrap();
        let mut decoded = Vec::new();
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, data, "round-trip mismatch (threads={threads}, len={len})");
    }

    /// Single-threaded zstd write -> read recovers the exact bytes, proving
    /// `auto_finish` writes the frame epilogue on drop.
    #[tokio::test]
    async fn round_trip_zstd_single_thread() {
        round_trip_zstd(1, 64 * 1024).await;
    }

    /// Multi-threaded zstd encoder (`Encoder::multithread`, `zstdmt` feature)
    /// produces a stream the single-threaded decoder reads back intact.
    #[tokio::test]
    async fn round_trip_zstd_multi_thread() {
        round_trip_zstd(4, 512 * 1024).await;
    }
}
