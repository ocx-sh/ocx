// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors specific to package metadata, versioning, and description operations.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "PackageError")]
// `EnvVarInterpolation` holds `TemplateError` inline; error paths are cold.
#[allow(clippy::large_enum_variant)]
pub enum Error {
    /// A package version string could not be parsed.
    #[error("invalid package version: {0}")]
    #[exit(DataError, slug = "version_invalid", summary = "A package version does not parse")]
    VersionInvalid(String),

    /// Build metadata could not be attached to a parsed [`crate::version::Version`].
    #[error(transparent)]
    #[exit(
        DataError,
        slug = "invalid_build_metadata",
        summary = "A version's build metadata is invalid"
    )]
    BuildMeta(#[from] super::version::build_meta::BuildMetaError),

    /// A logo file has an unsupported image format.
    #[error("unsupported logo format: {0}")]
    #[exit(
        DataError,
        slug = "unsupported_logo_format",
        summary = "The logo file has an unsupported format"
    )]
    UnsupportedLogoFormat(String),

    /// A logo file's bytes are not the image format its extension claims.
    #[error("logo at {} is not a {expected} image", .path.display())]
    #[exit(
        DataError,
        slug = "invalid_logo_content",
        summary = "The logo file's content does not match its format"
    )]
    InvalidLogoContent { path: PathBuf, expected: &'static str },

    /// A required path does not exist.
    #[error("required path does not exist: {}", .0.display())]
    #[exit(NotFound, slug = "required_path_missing", summary = "A required path does not exist")]
    RequiredPathMissing(PathBuf),

    /// A push was invoked with an empty platform set.
    #[error("push requires at least one target platform")]
    #[exit(DataError, slug = "empty_push_set", summary = "A push names no target platform")]
    EmptyPushSet,

    /// Env var template interpolation failed.
    #[error("env var '{var_key}' {source}")]
    #[exit(delegate = source)]
    EnvVarInterpolation {
        var_key: String,
        #[source]
        source: super::metadata::template::TemplateError,
    },

    /// An env var declares a modifier `type` this binary does not know.
    #[error("env var '{key}' declares unknown type '{type_name}'; upgrade ocx to use this package")]
    #[exit(
        DataError,
        slug = "unknown_env_modifier",
        summary = "A package env entry declares a type this build does not know"
    )]
    UnknownEnvModifier { key: String, type_name: String },

    /// A `list` env var omits `separator`, which the wire requires.
    #[error("env var '{key}' omits `separator`, which is required for list entries")]
    #[exit(
        DataError,
        slug = "missing_list_separator",
        summary = "A list-valued package env entry omits its separator"
    )]
    MissingListSeparator { key: String },

    /// An env var claims a key in the reserved `OCX_*` / `__OCX_*` namespace;
    /// raised at publish time only.
    #[error(
        "env var '{key}' is in the reserved OCX_* / __OCX_* namespace, which ocx keeps for its own \
         configuration; rename the variable"
    )]
    #[exit(
        DataError,
        slug = "reserved_env_key",
        summary = "A package env entry sets a reserved OCX key"
    )]
    ReservedEnvKey { key: String },

    /// A `list` env var's separator fails
    /// [`separator_is_valid`](super::metadata::env::list::separator_is_valid).
    // `{:?}`, not `{}`: a raw newline in the separator would forge log lines (CWE-117).
    #[error(
        "env var '{key}' declares list separator {separator:?}; a separator must be non-empty and free of '=', newline and carriage return"
    )]
    #[exit(
        DataError,
        slug = "invalid_list_separator",
        summary = "A package env entry declares an unusable list separator"
    )]
    InvalidListSeparator { key: String, separator: String },

    /// A `list` value starts or ends with its own separator.
    #[error("env var '{key}' has a list value starting or ending with its separator {separator:?}: {value:?}")]
    #[exit(
        DataError,
        slug = "separator_edged_list_value",
        summary = "A package list value starts or ends with its separator"
    )]
    SeparatorEdgedListValue {
        key: String,
        separator: String,
        value: String,
    },

    /// Entrypoint baked-arg template interpolation failed at publish time.
    #[error("entrypoint '{entrypoint}' arg '{arg}' {source}")]
    #[exit(delegate = source)]
    EntrypointArgInterpolation {
        entrypoint: String,
        arg: String,
        #[source]
        source: super::metadata::template::TemplateError,
    },

    /// An integrations namespace key violates the key grammar.
    // `{:?}`, not `{}`: a raw newline in the namespace would forge log lines (CWE-117).
    #[error("integrations namespace {namespace:?} is invalid: {reason}")]
    #[exit(
        DataError,
        slug = "integration_namespace_invalid",
        summary = "An integrations namespace is invalid"
    )]
    IntegrationNamespaceInvalid { namespace: String, reason: &'static str },

    /// An integrations payload exceeds its per-namespace size cap.
    #[error("integrations payload for {namespace:?} is {size} bytes, over the {max}-byte limit")]
    #[exit(
        DataError,
        slug = "integration_too_large",
        summary = "An integrations payload exceeds its size limit"
    )]
    IntegrationTooLarge { namespace: String, size: usize, max: usize },

    /// The whole integrations map exceeds the per-package size cap.
    #[error("integrations total {size} bytes, over the {max}-byte per-package limit")]
    #[exit(
        DataError,
        slug = "integrations_too_large",
        summary = "A package's integrations exceed the per-package size limit"
    )]
    IntegrationsTooLarge { size: usize, max: usize },

    /// Integrations payload template interpolation failed.
    #[error("integrations namespace {namespace:?} {source}")]
    #[exit(delegate = source)]
    IntegrationInterpolation {
        namespace: String,
        #[source]
        source: super::metadata::template::TemplateError,
    },

    // The variants below are unwrapped one by one in `ocx_package_manager`'s
    // hand-written `From`; a nesting `#[from]` there would lose their exit codes.
    #[error(transparent)]
    #[exit(
        IoError,
        slug = "package_file_io",
        summary = "Reading or writing a package file failed"
    )]
    File(#[from] ocx_util::error::FileError),

    #[error(transparent)]
    #[exit(delegate)]
    OciClient(#[from] ocx_oci::client::error::ClientError),

    #[error(transparent)]
    #[exit(delegate)]
    Index(#[from] ocx_index::error::Error),

    #[error(transparent)]
    #[exit(
        DataError,
        slug = "package_serialization",
        summary = "A package document could not be serialized"
    )]
    SerializationFailure(#[from] serde_json::Error),

    #[error(transparent)]
    #[exit(delegate)]
    Digest(#[from] ocx_oci::digest::error::DigestError),

    #[error(transparent)]
    #[exit(delegate)]
    Platform(#[from] ocx_oci::platform::error::PlatformError),

    #[error(transparent)]
    #[exit(delegate)]
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
