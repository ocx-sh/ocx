// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot determine compression algorithm for '{}'", .0.display())]
    UnknownFormat(PathBuf),

    #[error("failed to open '{}' for decompression", .path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to create compressed output '{}'", .path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("compression engine initialization failed")]
    EngineInit(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("compression I/O error")]
    Io(#[source] std::io::Error),

    #[error("{0} archives can be extracted but not written")]
    DecodeOnly(super::CompressionAlgorithm),
}

pub type Result<T> = std::result::Result<T, Error>;
