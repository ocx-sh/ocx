// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::{cli::ClassifyExitCode, cli::ExitCode};

/// Errors specific to package metadata, versioning, and description operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
// `EnvVarInterpolation` holds `TemplateError` inline; the size asymmetry is acceptable
// because error paths are cold and boxing would complicate every construction site.
#[allow(clippy::large_enum_variant)]
pub enum Error {
    /// A package version string could not be parsed.
    #[error("invalid package version: {0}")]
    VersionInvalid(String),

    /// A logo file has an unsupported image format.
    #[error("unsupported logo format: {0}")]
    UnsupportedLogoFormat(String),

    /// A required path does not exist.
    #[error("required path does not exist: {}", .0.display())]
    RequiredPathMissing(PathBuf),

    /// Env var template interpolation failed.
    #[error("env var '{var_key}' {source}")]
    EnvVarInterpolation {
        var_key: String,
        #[source]
        source: super::metadata::template::TemplateError,
    },

    /// An entry point target template failed validation at publish time.
    #[error("entry point '{name}' target {source}")]
    EntryPointTargetInvalid {
        name: String,
        #[source]
        source: super::metadata::template::TemplateError,
    },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::VersionInvalid(_) | Self::UnsupportedLogoFormat(_) => Some(ExitCode::DataError),
            Self::RequiredPathMissing(_) => Some(ExitCode::NotFound),
            Self::EnvVarInterpolation { source, .. } => source.classify(),
            Self::EntryPointTargetInvalid { source, .. } => source.classify(),
        }
    }
}
