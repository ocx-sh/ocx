// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ExitCode;
use crate::cli::classify::ClassifyExitCode;

/// Errors that can occur during configuration parsing and validation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An invalid boolean string was provided in configuration.
    #[error("invalid boolean string '{value}', possible values are: {possible}")]
    InvalidBooleanString { value: String, possible: String },

    /// A TOML configuration file could not be parsed.
    #[error("invalid TOML at {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// An explicit config file (`--config` or `OCX_CONFIG_FILE`) was
    /// specified but does not exist.
    #[error("config file not found: {} (check --config or OCX_CONFIG_FILE)", path.display())]
    FileNotFound { path: PathBuf },

    /// I/O failure while reading a config file (permission denied,
    /// unreadable file, config path is a directory, etc.).
    #[error("failed to read config file {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A config file exceeds the maximum allowed size (safety cap; config
    /// files are expected to be well under 1 KiB).
    #[error(
        "config file {} exceeds maximum allowed size ({size} bytes > {limit} bytes); OCX config files are typically under 1 KiB — did you point at the wrong file?",
        path.display()
    )]
    FileTooLarge { path: PathBuf, size: u64, limit: u64 },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::FileNotFound { .. } => ExitCode::NotFound,
            Self::FileTooLarge { .. } | Self::Parse { .. } => ExitCode::ConfigError,
            Self::Io { .. } => ExitCode::IoError,
            Self::InvalidBooleanString { .. } => ExitCode::DataError,
        })
    }
}
