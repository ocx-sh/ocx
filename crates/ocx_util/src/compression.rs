// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use error::Result;

/// Enumeration of supported compression algorithms.
#[derive(Debug, Clone, Copy)]
pub enum CompressionAlgorithm {
    None,
    Lzma,
    Gzip,
    Zstd,
    /// Decode only. `.tar.bz2` is an accepted *input* to `ocx package create
    /// --extract`; nothing in OCX writes bzip2, and no layer media type spells
    /// it, so [`write_file`] refuses this variant rather than producing a
    /// bundle that could never be published.
    Bzip2,
}

impl CompressionAlgorithm {
    /// Infers the compression algorithm from the file extension of the given path.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let path = path.as_ref();
        match path.extension()?.to_str()? {
            "xz" => Some(CompressionAlgorithm::Lzma),
            "gz" | "tgz" => Some(CompressionAlgorithm::Gzip),
            "zst" | "zstd" | "tzst" => Some(CompressionAlgorithm::Zstd),
            "bz2" | "tbz2" | "tbz" => Some(CompressionAlgorithm::Bzip2),
            _ => None,
        }
    }

    /// Identifies the compressor from a stream's leading bytes (its magic
    /// number); `head` needs at most [`MAGIC_LEN`] bytes, fewer only near EOF.
    ///
    /// Content, not name: an upstream asset can be compressed without saying
    /// so (`tool` holding a gzip stream) or say so without being an archive
    /// (`tool.gz` holding a bare executable). No ELF, Mach-O, PE or `#!` file
    /// starts with any of these, so a hit is unambiguous.
    pub(crate) fn from_magic(head: &[u8]) -> Option<Self> {
        match head {
            [0x1f, 0x8b, ..] => Some(CompressionAlgorithm::Gzip),
            [0xfd, b'7', b'z', b'X', b'Z', 0x00, ..] => Some(CompressionAlgorithm::Lzma),
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Some(CompressionAlgorithm::Zstd),
            // `BZh` alone is printable — a plain tar whose first entry is named
            // `BZh…` would match — so require the block size digit and the
            // first block's (or the empty stream's end-of-stream) magic too.
            [b'B', b'Z', b'h', b'1'..=b'9', 0x31, 0x41, 0x59, 0x26, 0x53, 0x59, ..]
            | [b'B', b'Z', b'h', b'1'..=b'9', 0x17, 0x72, 0x45, 0x38, 0x50, 0x90, ..] => {
                Some(CompressionAlgorithm::Bzip2)
            }
            _ => None,
        }
    }

    /// Identifies the compressor from the magic number `file` starts with,
    /// by content rather than by name; `None` when it carries none of them.
    pub async fn from_file_magic(file: impl AsRef<std::path::Path>) -> Result<Option<Self>> {
        use tokio::io::AsyncReadExt as _;

        let file = file.as_ref();
        let mut handle = tokio::fs::File::open(file).await.map_err(|e| error::Error::Open {
            path: file.to_path_buf(),
            source: e,
        })?;
        let mut head = [0u8; MAGIC_LEN];
        let mut filled = 0;
        while filled < head.len() {
            let read = handle.read(&mut head[filled..]).await.map_err(error::Error::Io)?;
            if read == 0 {
                break;
            }
            filled += read;
        }
        Ok(Self::from_magic(&head[..filled]))
    }
}

/// The longest magic number [`CompressionAlgorithm::from_magic`] matches (bzip2).
pub(crate) const MAGIC_LEN: usize = 10;

impl std::fmt::Display for CompressionAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionAlgorithm::None => write!(f, "none"),
            CompressionAlgorithm::Lzma => write!(f, "lzma"),
            CompressionAlgorithm::Gzip => write!(f, "gzip"),
            CompressionAlgorithm::Zstd => write!(f, "zstd"),
            CompressionAlgorithm::Bzip2 => write!(f, "bzip2"),
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
/// Public so downstream thread-count defaults import this cap instead of copying it.
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

    pub(crate) fn with_algorithm(mut self, algorithm: CompressionAlgorithm) -> Self {
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
    pub(crate) fn threads_or_default(&self) -> u32 {
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
    pub(crate) struct WriterWrapper<W: std::io::Write>(pub Option<lzma_rust2::XzWriter<W>>);

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
    pub(crate) struct MtWriterWrapper<W: std::io::Write>(pub Option<lzma_rust2::XzWriterMt<W>>);

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
    // bzip2 is decode-only (see `read_file`). Refused here, *before* the output
    // file below is created and truncated, so a `--output pkg.tar.bz2` leaves
    // nothing behind. The match arm below repeats the refusal rather than
    // `unreachable!()`, so reordering this guard can never turn it into a panic.
    if matches!(algorithm, CompressionAlgorithm::Bzip2) {
        return Err(error::Error::DecodeOnly(algorithm));
    }
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
        CompressionAlgorithm::Bzip2 => return Err(error::Error::DecodeOnly(algorithm)),
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
            // Multi-stream: `xz` appends a stream per invocation (`xz -c a >> f`),
            // and `xz -d` decodes them all. Single-stream stopped after the
            // first and reported success on a truncated payload.
            Ok(Box::new(lzma_rust2::XzReader::new(buffered, true)))
        }
        CompressionAlgorithm::Gzip => {
            let handle = std::fs::File::open(file).map_err(|e| error::Error::Open {
                path: file.to_path_buf(),
                source: e,
            })?;
            let buffered = std::io::BufReader::with_capacity(READ_FILE_BUF_CAPACITY, handle);
            // `MultiGzDecoder`, for the reason the bzip2 arm below uses
            // `MultiBzDecoder`: `pigz --independent`, `bgzip` and `cat a.gz
            // b.gz` produce several members, `gzip -d` decodes every one, and
            // `GzDecoder` stopped after the first as if the file ended there.
            Ok(Box::new(flate2::read::MultiGzDecoder::new(buffered)))
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
        CompressionAlgorithm::Bzip2 => {
            let handle = std::fs::File::open(file).map_err(|e| error::Error::Open {
                path: file.to_path_buf(),
                source: e,
            })?;
            let buffered = std::io::BufReader::with_capacity(READ_FILE_BUF_CAPACITY, handle);
            // `MultiBzDecoder`, not `BzDecoder`: pbzip2 and lbzip2 write
            // concatenated bzip2 streams, and `BzDecoder` stops at the end of
            // the first one — which would silently truncate such a tarball to
            // its first block instead of failing. `bzip2 -d` and `tar -xj`
            // decode every stream, so this matches what the operator's own
            // tools do with the same file.
            //
            // The `bufread` decoder takes the `BufRead` above as-is; its
            // `read` sibling would wrap it in a second, 8 KiB `BufReader` —
            // the same double buffering the zstd arm avoids with
            // `Decoder::with_buffer`.
            Ok(Box::new(bzip2::bufread::MultiBzDecoder::new(buffered)))
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

    #[test]
    fn from_magic_identifies_each_compressor_and_nothing_else() {
        let cases: [(&[u8], Option<&str>); 12] = [
            (&[0x1f, 0x8b, 0x08, 0x00], Some("gzip")),
            (&[0xfd, b'7', b'z', b'X', b'Z', 0x00], Some("lzma")),
            (&[0x28, 0xb5, 0x2f, 0xfd, 0x00], Some("zstd")),
            (b"BZh91AY&SY", Some("bzip2")),
            (b"BZh9\x17\x72\x45\x38\x50\x90", Some("bzip2")), // empty stream
            (b"BZh-notes.txt\0\0", None),                     // a tar entry name
            (b"BZh9", None),                                  // truncated bzip2 magic
            (b"\x7fELF\x02\x01", None),                       // ELF
            (&[0xcf, 0xfa, 0xed, 0xfe], None),                // Mach-O
            (b"MZ\x90\x00", None),                            // PE
            (b"#!/bin/sh", None),
            (&[0xfd, b'7', b'z'], None), // truncated xz magic is not xz
        ];
        for (head, expected) in cases {
            let found = CompressionAlgorithm::from_magic(head).map(|algorithm| algorithm.to_string());
            assert_eq!(found.as_deref(), expected, "head {head:02x?}");
        }
    }

    /// Two independently compressed members concatenated — `cat a.gz b.gz`,
    /// `pigz --independent`, `xz -c b >> a.xz` — must decode to both payloads,
    /// as `gzip -d` / `xz -d` do. The single-member decoders stopped after the
    /// first and reported success on half the bytes.
    #[tokio::test]
    async fn read_file_decodes_every_concatenated_member() {
        for algorithm in [CompressionAlgorithm::Gzip, CompressionAlgorithm::Lzma] {
            let dir = tempfile::tempdir().unwrap();
            let mut concatenated = Vec::new();
            for (index, part) in [b"first member ".as_slice(), b"second member"].into_iter().enumerate() {
                let path = dir.path().join(format!("part{index}"));
                {
                    let mut writer = write_file(&path, &CompressionOptions::new(algorithm)).await.unwrap();
                    writer.write_all(part).unwrap();
                }
                concatenated.extend(std::fs::read(&path).unwrap());
            }
            let path = dir.path().join("joined");
            std::fs::write(&path, concatenated).unwrap();

            let mut decoded = Vec::new();
            read_file(&path, Some(algorithm))
                .await
                .unwrap()
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(decoded, b"first member second member", "{algorithm} stopped early");
        }
    }

    #[tokio::test]
    async fn from_file_magic_reads_the_leading_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let compressed = dir.path().join("no-extension");
        {
            let mut writer = write_file(&compressed, &CompressionOptions::new(CompressionAlgorithm::Zstd))
                .await
                .unwrap();
            writer.write_all(b"payload").unwrap();
        }
        let short = dir.path().join("short");
        std::fs::write(&short, [0x1f]).unwrap();

        assert!(matches!(
            CompressionAlgorithm::from_file_magic(&compressed).await.unwrap(),
            Some(CompressionAlgorithm::Zstd)
        ));
        assert!(CompressionAlgorithm::from_file_magic(&short).await.unwrap().is_none());
    }

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

    #[test]
    fn from_file_infers_bzip2() {
        for name in ["pkg.tar.bz2", "pkg.tbz2", "pkg.tbz"] {
            assert!(
                matches!(CompressionAlgorithm::from_file(name), Some(CompressionAlgorithm::Bzip2)),
                "{name} should infer bzip2"
            );
        }
    }

    /// `bzip2` must stay on its pure-Rust `libbz2-rs-sys` backend, and the
    /// lockfile is the only place that can be checked: the C backend
    /// (`bzip2-sys`) vendors libbz2 and *static*-links it through `cc`, so it
    /// emits no NEEDED entry and `ocx_cli/tests/linux_self_contained.rs` — which
    /// bans a dynamic `libbz2` — passes either way.
    ///
    /// The live footgun this guards: `zip`, already a dependency, has a feature
    /// named `bzip2-rs` that maps to `bzip2/bzip2-sys`, i.e. the C path under the
    /// name a reader would take for the Rust one. Enabling it unifies this crate's
    /// `bzip2` onto that backend silently. `deny.toml [bans]` cannot hold this
    /// line — nothing in the taskfiles runs `cargo deny check bans`.
    #[test]
    fn bzip2_stays_on_the_pure_rust_backend() {
        let lockfile = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
        let lock = std::fs::read_to_string(&lockfile).unwrap_or_else(|e| panic!("reading {}: {e}", lockfile.display()));

        // Positive control: without it, a moved or emptied lockfile would make
        // the refusal below pass for the wrong reason.
        assert!(
            lock.contains(r#"name = "libbz2-rs-sys""#),
            "{} does not lock libbz2-rs-sys — bzip2 is no longer on the pure-Rust backend",
            lockfile.display()
        );
        assert!(
            !lock.contains(r#"name = "bzip2-sys""#),
            "{} locks bzip2-sys: some feature (e.g. zip/bzip2-rs, which means bzip2/bzip2-sys) \
             pulled in the C libbz2 backend",
            lockfile.display()
        );
    }

    #[test]
    fn display_bzip2() {
        assert_eq!(CompressionAlgorithm::Bzip2.to_string(), "bzip2");
    }

    /// One bzip2 stream over `data`.
    ///
    /// The encoder is deliberately test-only: the linked implementation can
    /// compress, and `write_file` refuses to expose it (no layer media type
    /// spells bzip2), so this is how a decode test gets genuine bzip2 bytes
    /// without a committed binary fixture. Externally produced bytes are
    /// covered end-to-end by `test/tests/test_package_create_extract.py`,
    /// whose `.tar.bz2` rows come out of CPython's `tarfile`.
    fn bzip2_stream(data: &[u8]) -> Vec<u8> {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::best());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    /// `read_file` decodes a `.tar.bz2` with the algorithm inferred from the
    /// extension — the `from_file` arm and the decoder in one pass, which is
    /// exactly what `ocx package create --extract` drives.
    #[tokio::test]
    async fn read_file_decodes_bzip2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.tar.bz2");
        let data = payload(64 * 1024);
        std::fs::write(&path, bzip2_stream(&data)).unwrap();

        let mut reader = read_file(&path, None).await.unwrap();
        let mut decoded = Vec::new();
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, data);
    }

    /// pbzip2 and lbzip2 emit back-to-back bzip2 streams in one file, and
    /// `bzip2 -d`/`tar -xj` decode all of them. `BzDecoder` would stop after
    /// the first and report a clean EOF, silently handing a truncated tar to
    /// the extractor; `MultiBzDecoder` is what keeps that from happening.
    #[tokio::test]
    async fn read_file_decodes_concatenated_bzip2_streams() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.tar.bz2");
        let first = payload(4096);
        let second: Vec<u8> = payload(4096).iter().map(|b| b ^ 0xFF).collect();
        let mut file_bytes = bzip2_stream(&first);
        file_bytes.extend_from_slice(&bzip2_stream(&second));
        std::fs::write(&path, &file_bytes).unwrap();

        let mut reader = read_file(&path, Some(CompressionAlgorithm::Bzip2)).await.unwrap();
        let mut decoded = Vec::new();
        reader.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, [first, second].concat(), "both streams must be decoded");
    }

    /// The compress path refuses bzip2 with a typed error (exit 65) and, since
    /// the refusal precedes the open, leaves no truncated output file behind.
    #[tokio::test]
    async fn write_file_refuses_bzip2() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.tar.bz2");

        // `expect_err` is unavailable here: the Ok type is `Box<dyn Write + Send>`,
        // which is not `Debug`.
        let error = match write_file(&path, &CompressionOptions::new(CompressionAlgorithm::Bzip2)).await {
            Ok(_) => panic!("bzip2 is decode-only, but write_file handed back a writer"),
            Err(error) => error,
        };

        assert!(
            matches!(error, error::Error::DecodeOnly(CompressionAlgorithm::Bzip2)),
            "unexpected error: {error:?}"
        );
        assert_eq!(error.to_string(), "bzip2 archives can be extracted but not written");
        assert!(!path.exists(), "a refused compression must not create the output file");
    }
}
