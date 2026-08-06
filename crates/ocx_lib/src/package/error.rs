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

    /// Build metadata could not be attached to a parsed [`crate::package::version::Version`].
    #[error(transparent)]
    BuildMeta(#[from] super::version::build_meta::BuildMetaError),

    /// A logo file has an unsupported image format.
    #[error("unsupported logo format: {0}")]
    UnsupportedLogoFormat(String),

    /// A logo file's bytes are not the image format its extension claims.
    #[error("logo at {} is not a {expected} image", .path.display())]
    InvalidLogoContent { path: PathBuf, expected: &'static str },

    /// A required path does not exist.
    #[error("required path does not exist: {}", .0.display())]
    RequiredPathMissing(PathBuf),

    /// A push was invoked with an empty platform set.
    #[error("push requires at least one target platform")]
    EmptyPushSet,

    /// Env var template interpolation failed.
    #[error("env var '{var_key}' {source}")]
    EnvVarInterpolation {
        var_key: String,
        #[source]
        source: super::metadata::template::TemplateError,
    },

    /// An env var declares a modifier `type` this binary does not know — the
    /// package was published against a newer ocx.
    #[error("env var '{key}' declares unknown type '{type_name}'; upgrade ocx to use this package")]
    UnknownEnvModifier { key: String, type_name: String },

    /// A `list` env var omits `separator`. Required on the wire: no human is
    /// present when metadata is read, and the wrong separator fails silently
    /// downstream.
    #[error("env var '{key}' omits `separator`, which is required for list entries")]
    MissingListSeparator { key: String },

    /// A `list` env var's separator cannot be folded with — see
    /// [`separator_is_valid`](super::metadata::env::list::separator_is_valid).
    // `{:?}` on the separator: it is refused precisely for carrying something
    // unprintable (empty, `=`, a line break), and a raw newline here would
    // forge log lines (CWE-117) and hide the very byte being reported.
    #[error(
        "env var '{key}' declares list separator {separator:?}; a separator must be non-empty and free of '=', newline and carriage return"
    )]
    InvalidListSeparator { key: String, separator: String },

    /// A `list` value starts or ends with its own separator, which would make
    /// the append fold's flank match ambiguous. Checked as authored and again
    /// once templates have resolved.
    #[error("env var '{key}' has a list value starting or ending with its separator {separator:?}: {value:?}")]
    SeparatorEdgedListValue {
        key: String,
        separator: String,
        value: String,
    },

    /// Entrypoint baked-arg template interpolation failed at publish time.
    #[error("entrypoint '{entrypoint}' arg '{arg}' {source}")]
    EntrypointArgInterpolation {
        entrypoint: String,
        arg: String,
        #[source]
        source: super::metadata::template::TemplateError,
    },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::VersionInvalid(_)
            | Self::UnsupportedLogoFormat(_)
            | Self::InvalidLogoContent { .. }
            | Self::BuildMeta(_)
            | Self::EmptyPushSet
            | Self::UnknownEnvModifier { .. }
            | Self::MissingListSeparator { .. }
            | Self::InvalidListSeparator { .. }
            | Self::SeparatorEdgedListValue { .. } => Some(ExitCode::DataError),
            Self::RequiredPathMissing(_) => Some(ExitCode::NotFound),
            Self::EnvVarInterpolation { source, .. } => source.classify(),
            Self::EntrypointArgInterpolation { source, .. } => source.classify(),
        }
    }
}
