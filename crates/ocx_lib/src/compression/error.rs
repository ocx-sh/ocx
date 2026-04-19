// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

/// Errors that can occur during compression or decompression operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Cannot determine the compression algorithm from the file extension.
    #[error("cannot determine compression algorithm for '{}'", .0.display())]
    UnknownFormat(PathBuf),

    /// Failed to open a file for reading/decompression.
    #[error("failed to open '{}' for decompression", .path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to create an output file for compression.
    #[error("failed to create compressed output '{}'", .path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A compression engine (XZ, GZ) failed to initialize.
    #[error("compression engine initialization failed")]
    EngineInit(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// An I/O error during compression/decompression.
    #[error("compression I/O error")]
    Io(#[source] std::io::Error),
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::UnknownFormat(_) => ExitCode::DataError,
            Self::Open { .. } | Self::Create { .. } | Self::Io(_) => ExitCode::IoError,
            Self::EngineInit(_) => ExitCode::Failure,
        })
    }
}
