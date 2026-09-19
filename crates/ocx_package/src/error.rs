// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Errors specific to package metadata, versioning, and description operations.
#[derive(Debug, thiserror::Error)]
// `EnvVarInterpolation` holds `TemplateError` inline; the size asymmetry is acceptable
// because error paths are cold and boxing would complicate every construction site.
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

    // ── Stand-ins for the crate-wide error (E1, plan DEC-27) ──────────────
    //
    // The three below exist only because this tier may no longer name
    // `ocx_lib::Error` once it is `ocx_package`. Each is reconstructed
    // **exactly** at the boundary (`ocx_lib::error`'s hand-written `From`), so
    // the value the CLI classifies, and therefore the exit code and the
    // rendered message, is the one it was before the tier owned these sites
    // (DEC-23). They are deliberately not `#[from]`-derived into a single
    // nesting variant: a derived `From` would move all three under
    // `Error::Package` and change three exit codes with nothing to see in
    // review.
    /// An I/O failure on a path this tier touched — the metadata document, a
    /// bundle's temp file, a scanned content directory.
    #[error(transparent)]
    File(#[from] ocx_util::error::FileError),

    /// A registry read this tier performed on its own account: the cascade's
    /// child-manifest probes and the copy path's reads.
    #[error(transparent)]
    OciClient(#[from] ocx_oci::client::error::ClientError),

    /// A resolution-index failure reached while pinning a dependency.
    #[error(transparent)]
    Index(#[from] ocx_index::error::Error),

    /// A metadata document this tier serialized or parsed on its own account.
    #[error(transparent)]
    SerializationFailure(#[from] serde_json::Error),

    /// A digest this tier parsed or compared — the copy path's source reads.
    #[error(transparent)]
    Digest(#[from] ocx_oci::digest::error::DigestError),

    /// A platform string this tier parsed — the copy path's `--platform` set.
    #[error(transparent)]
    Platform(#[from] ocx_oci::platform::error::PlatformError),

    /// Writing or reading a bundle archive — the tar+gzip the bundler builds.
    #[error(transparent)]
    Archive(#[from] ocx_util::archive::Error),
}

/// Build this tier's [`Error::File`] for `path` from the io failure that
/// produced it — the tier-local twin of `ocx_lib::error::file_error`, with the
/// same signature, so the sites that called that one keep building the same
/// two-field value rather than merely the same shape.
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
