// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors that can occur during compression or decompression operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Cannot determine the compression algorithm from the file extension.
    #[error("Cannot determine compression algorithm for '{}'", .0.display())]
    UnknownFormat(PathBuf),

    /// Failed to open a file for reading/decompression.
    #[error("Failed to open '{}' for decompression", .path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to create an output file for compression.
    #[error("Failed to create compressed output '{}'", .path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A compression engine (XZ, GZ) failed to initialize.
    #[error("Compression engine initialization failed")]
    EngineInit(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// An I/O error during compression/decompression.
    #[error("Compression I/O error")]
    Io(#[source] std::io::Error),
}
