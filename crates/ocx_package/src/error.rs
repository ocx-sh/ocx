// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors specific to package metadata, versioning, and description operations.
#[derive(Debug, thiserror::Error)]
// `EnvVarInterpolation` holds `TemplateError` inline; error paths are cold.
#[allow(clippy::large_enum_variant)]
pub enum Error {
    /// A package version string could not be parsed.
    #[error("invalid package version: {0}")]
    VersionInvalid(String),

    /// Build metadata could not be attached to a parsed [`crate::version::Version`].
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

    /// An env var declares a modifier `type` this binary does not know.
    #[error("env var '{key}' declares unknown type '{type_name}'; upgrade ocx to use this package")]
    UnknownEnvModifier { key: String, type_name: String },

    /// A `list` env var omits `separator`, which the wire requires.
    #[error("env var '{key}' omits `separator`, which is required for list entries")]
    MissingListSeparator { key: String },

    /// An env var claims a key in the reserved `OCX_*` / `__OCX_*` namespace;
    /// raised at publish time only.
    #[error(
        "env var '{key}' is in the reserved OCX_* / __OCX_* namespace, which ocx keeps for its own \
         configuration; rename the variable"
    )]
    ReservedEnvKey { key: String },

    /// A `list` env var's separator fails
    /// [`separator_is_valid`](super::metadata::env::list::separator_is_valid).
    // `{:?}`, not `{}`: a raw newline in the separator would forge log lines (CWE-117).
    #[error(
        "env var '{key}' declares list separator {separator:?}; a separator must be non-empty and free of '=', newline and carriage return"
    )]
    InvalidListSeparator { key: String, separator: String },

    /// A `list` value starts or ends with its own separator.
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
    // `{:?}`, not `{}`: a raw newline in the namespace would forge log lines (CWE-117).
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

    // The variants below are unwrapped one by one in `ocx_package_manager`'s
    // hand-written `From`; a nesting `#[from]` there would lose their exit codes.
    #[error(transparent)]
    File(#[from] ocx_util::error::FileError),

    #[error(transparent)]
    OciClient(#[from] ocx_oci::client::error::ClientError),

    #[error(transparent)]
    Index(#[from] ocx_index::error::Error),

    #[error(transparent)]
    SerializationFailure(#[from] serde_json::Error),

    #[error(transparent)]
    Digest(#[from] ocx_oci::digest::error::DigestError),

    #[error(transparent)]
    Platform(#[from] ocx_oci::platform::error::PlatformError),

    #[error(transparent)]
    Archive(#[from] ocx_util::archive::Error),
}

/// Builds [`Error::File`] for `path` from the io failure that produced it.
pub fn file_error(path: impl AsRef<std::path::Path>, cause: std::io::Error) -> Error {
    Error::File(ocx_util::error::FileError::new(path, cause))
}

#[cfg(test)]
mod tests {

    // ── C-019: integrations error variants classify to exit 65 (DataError) ─
    //
    // `ClassifyExitCode` for these variants is already implemented (not a
    // stub) — these exercise real, already-correct wiring, not an
    // `unimplemented!()` stub body.
}
