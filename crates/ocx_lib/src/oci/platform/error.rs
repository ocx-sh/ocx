// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::architecture::Architecture;
use super::operating_system::OperatingSystem;
use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

/// An error that occurred while parsing or validating an OCI platform.
#[derive(Debug, Clone, thiserror::Error)]
#[error("invalid platform '{input}': {kind}")]
#[non_exhaustive]
pub struct PlatformError {
    /// The raw input that failed to parse.
    pub input: String,
    /// The specific reason parsing failed.
    pub kind: PlatformErrorKind,
}

/// The specific reason a platform string failed to parse or validate.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformErrorKind {
    /// The platform string has an invalid format.
    #[error("expected format 'os/arch' or 'any'")]
    InvalidFormat,

    /// The OS component is not a recognized value.
    #[error("unsupported OS '{os}'. Possible values are: {}", OperatingSystem::VARIANTS.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "))]
    UnsupportedOs { os: String },

    /// The architecture component is not a recognized value.
    #[error("unsupported architecture '{arch}'. Possible values are: {}", Architecture::VARIANTS.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "))]
    UnsupportedArch { arch: String },

    /// The platform is syntactically valid but not supported by OCX.
    #[error("unsupported platform: {0}")]
    Unsupported(String),
}

impl ClassifyExitCode for PlatformError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::DataError)
    }
}
