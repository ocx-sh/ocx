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

#[derive(Default)]
pub struct CompressionOptions {
    pub algorithm: Option<CompressionAlgorithm>,
    pub level: CompressionLevel,
}

impl CompressionOptions {
    pub fn new(algorithm: CompressionAlgorithm) -> Self {
        Self {
            algorithm: Some(algorithm),
            level: Default::default(),
        }
    }

    pub fn from_algorithm(algorithm: CompressionAlgorithm) -> Self {
        Self {
            algorithm: Some(algorithm),
            level: Default::default(),
        }
    }

    pub fn from_level(level: CompressionLevel) -> Self {
        Self { algorithm: None, level }
    }

    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let algorithm = CompressionAlgorithm::from_file(path).map_to_undefined_error()?;
        Ok(Self {
            algorithm: Some(algorithm),
            level: Default::default(),
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
}

/// Opens a writer for the given file and compression options.
/// If the algorithm is not specified, it will be tried to infer it from the file extension of the output path.
/// The file will be created if it does not exist, and truncated if it does exist.
pub async fn write_file(
    file: impl AsRef<std::path::Path>,
    algorithm: Option<CompressionAlgorithm>,
    level: Option<CompressionLevel>,
) -> Result<Box<dyn std::io::Write>> {
    let algorithm = match algorithm {
        Some(algorithm) => algorithm,
        None => CompressionAlgorithm::from_file(&file).map_to_undefined_error()?,
    };
    let level = level.unwrap_or_default();
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(file)
        .map_to_undefined_error()?;
    let writer: Box<dyn std::io::Write> = match algorithm {
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
) -> Result<Box<dyn std::io::Read>> {
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
