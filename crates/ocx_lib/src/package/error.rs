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

    /// An env var claims a key in the `OCX_*` / `__OCX_*` namespace ocx reserves
    /// for its own configuration. Publish-time only — a package that already
    /// carries one keeps reading, and the resolver skips the key.
    #[error(
        "env var '{key}' is in the reserved OCX_* / __OCX_* namespace, which ocx keeps for its own \
         configuration; rename the variable"
    )]
    ReservedEnvKey { key: String },

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

    /// An integrations namespace key violates the key grammar.
    // `{:?}` on the namespace, for the same reason `InvalidListSeparator` uses
    // it: a key is refused precisely for carrying something unprintable, and a
    // raw newline would forge log lines (CWE-117) and hide the offending byte.
    #[error("integrations namespace {namespace:?} is invalid: {reason}")]
    IntegrationNamespaceInvalid { namespace: String, reason: &'static str },

    /// An integrations payload exceeds its per-namespace size cap.
    #[error("integrations payload for {namespace:?} is {size} bytes, over the {max}-byte limit")]
    IntegrationTooLarge { namespace: String, size: usize, max: usize },

    /// The whole integrations map exceeds the per-package size cap.
    #[error("integrations total {size} bytes, over the {max}-byte per-package limit")]
    IntegrationsTooLarge { size: usize, max: usize },

    /// Integrations payload template interpolation failed.
    #[error("integrations namespace {namespace:?} {source}")]
    IntegrationInterpolation {
        namespace: String,
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
            | Self::ReservedEnvKey { .. }
            | Self::InvalidListSeparator { .. }
            | Self::SeparatorEdgedListValue { .. }
            | Self::IntegrationNamespaceInvalid { .. }
            | Self::IntegrationTooLarge { .. }
            | Self::IntegrationsTooLarge { .. } => Some(ExitCode::DataError),
            Self::RequiredPathMissing(_) => Some(ExitCode::NotFound),
            Self::EnvVarInterpolation { source, .. } => source.classify(),
            Self::EntrypointArgInterpolation { source, .. } => source.classify(),
            Self::IntegrationInterpolation { source, .. } => source.classify(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::dependency::DependencyName;
    use crate::package::metadata::template::TemplateError;

    // ── C-019: integrations error variants classify to exit 65 (DataError) ─
    //
    // `ClassifyExitCode` for these variants is already implemented (not a
    // stub) — these exercise real, already-correct wiring, not an
    // `unimplemented!()` stub body.

    #[test]
    fn integration_namespace_invalid_classifies_as_data_error() {
        let err = Error::IntegrationNamespaceInvalid {
            namespace: String::new(),
            reason: "empty",
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn integration_too_large_classifies_as_data_error() {
        let err = Error::IntegrationTooLarge {
            namespace: "com.example".to_owned(),
            size: 9000,
            max: 8192,
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn integrations_too_large_classifies_as_data_error() {
        let err = Error::IntegrationsTooLarge {
            size: 40_000,
            max: 32_768,
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn integration_interpolation_unknown_dependency_ref_classifies_as_data_error() {
        // IntegrationInterpolation delegates classification to its source —
        // UnknownDependencyRef -> DataError (65), same as an env value with
        // the same token.
        let err = Error::IntegrationInterpolation {
            namespace: "com.example".to_owned(),
            source: TemplateError::UnknownDependencyRef {
                ref_name: DependencyName::try_from("ninja").unwrap(),
                declared: vec![],
            },
        };
        assert_eq!(err.classify(), Some(ExitCode::DataError));
    }

    #[test]
    fn integration_interpolation_dependency_not_installed_classifies_as_not_found() {
        // The one IntegrationInterpolation source that classifies away from
        // DataError — DependencyNotInstalled maps to NotFound (79), matching
        // an env value carrying the same token today.
        let hex = "a".repeat(64);
        let identifier: crate::oci::Identifier = format!("ocx.sh/ninja:1@sha256:{hex}").parse().unwrap();
        let pinned = crate::oci::PinnedIdentifier::try_from(identifier).unwrap();

        let err = Error::IntegrationInterpolation {
            namespace: "com.example".to_owned(),
            source: TemplateError::DependencyNotInstalled {
                ref_name: DependencyName::try_from("ninja").unwrap(),
                dep_identifier: Box::new(pinned),
            },
        };
        assert_eq!(err.classify(), Some(ExitCode::NotFound));
    }
}
