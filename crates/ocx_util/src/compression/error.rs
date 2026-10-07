// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "CompressionError")]
pub enum Error {
    #[error("cannot determine compression algorithm for '{}'", .0.display())]
    #[exit(
        DataError,
        slug = "compression_unknown_format",
        summary = "The compression algorithm could not be determined"
    )]
    UnknownFormat(PathBuf),

    #[error("failed to open '{}' for decompression", .path.display())]
    #[exit(
        IoError,
        slug = "compression_open",
        summary = "Opening a file for decompression failed"
    )]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to create compressed output '{}'", .path.display())]
    #[exit(IoError, slug = "compression_create", summary = "Creating compressed output failed")]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("compression engine initialization failed")]
    #[exit(
        Failure,
        slug = "compression_engine_init",
        summary = "The compression engine failed to initialize"
    )]
    EngineInit(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("compression I/O error")]
    #[exit(
        IoError,
        slug = "compression_io",
        summary = "A compression stream failed to read or write"
    )]
    Io(#[source] std::io::Error),

    #[error("{0} archives can be extracted but not written")]
    #[exit(
        DataError,
        slug = "compression_decode_only",
        summary = "The format can be extracted but not written"
    )]
    DecodeOnly(super::CompressionAlgorithm),
}

pub type Result<T> = std::result::Result<T, Error>;
