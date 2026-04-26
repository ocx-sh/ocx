// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A network operation was attempted while in offline mode.
    #[error("network operation attempted in offline mode")]
    OfflineMode,

    /// A file I/O error with path context.
    #[error("internal file error for '{path}': {source}", path = .0.display(), source = .1)]
    InternalFile(std::path::PathBuf, #[source] std::io::Error),
    /// A path has an unexpected structure.
    #[error("path '{}' has an unexpected structure", .0.display())]
    InternalPathInvalid(std::path::PathBuf),

    /// JSON serialization or deserialization failed.
    #[error("JSON serialization error: {0}")]
    SerializationFailure(#[from] serde_json::Error),

    /// An unsupported OCI media type was encountered.
    #[error("unsupported media type '{media_type}', expected media types are: {supported}", media_type = .0, supported = .1.join(", "))]
    UnsupportedMediaType(String, &'static [&'static str]),

    /// An authentication operation failed.
    #[error(transparent)]
    Auth(#[from] crate::auth::error::AuthError),
    /// A platform parsing or validation error.
    #[error(transparent)]
    Platform(#[from] crate::oci::platform::error::PlatformError),
    /// A profile manifest operation failed.
    #[error(transparent)]
    Profile(#[from] crate::profile::ProfileError),
    /// A package manager operation failed.
    #[error(transparent)]
    PackageManager(#[from] crate::package_manager::error::Error),
    /// An OCI client operation failed.
    #[error(transparent)]
    OciClient(#[from] crate::oci::client::error::ClientError),
    /// An OCI identifier could not be parsed.
    #[error(transparent)]
    Identifier(#[from] crate::oci::identifier::error::IdentifierError),
    /// An archive operation failed.
    #[error(transparent)]
    Archive(#[from] crate::archive::Error),
    /// A compression or decompression operation failed.
    #[error(transparent)]
    Compression(#[from] crate::compression::error::Error),
    /// A CI export operation failed.
    #[error(transparent)]
    Ci(#[from] crate::ci::error::Error),
    /// A configuration error occurred.
    #[error(transparent)]
    Config(#[from] crate::config::error::Error),
    /// A package operation failed.
    #[error(transparent)]
    Package(Box<crate::package::error::Error>),
    /// A shell operation failed.
    #[error(transparent)]
    Shell(#[from] crate::shell::error::Error),
    /// An OCI index operation failed.
    #[error(transparent)]
    OciIndex(#[from] crate::oci::index::error::Error),
    /// A file structure operation failed.
    #[error(transparent)]
    FileStructure(#[from] crate::file_structure::error::Error),
    /// A digest string could not be parsed.
    #[error(transparent)]
    Digest(#[from] crate::oci::digest::error::DigestError),

    /// A dependency graph operation failed.
    #[error(transparent)]
    Dependency(#[from] crate::package_manager::DependencyError),
    /// A pinned identifier validation failed.
    #[error(transparent)]
    PinnedIdentifier(#[from] crate::oci::pinned_identifier::PinnedIdentifierError),

    /// An entrypoint's `target` template could not be resolved at install time.
    #[error("entrypoint '{name}' has invalid target: {source}")]
    EntrypointTargetInvalid {
        name: String,
        #[source]
        source: Box<crate::package::metadata::template::TemplateError>,
    },

    /// A string baked into an install-time launcher contains a character that
    /// cannot be safely embedded in either the Unix `.sh` or Windows `.cmd`
    /// template (single-quote, percent, double-quote, NUL, CR, LF).
    #[error("launcher-unsafe character {character:?} in {value:?}")]
    LauncherUnsafeCharacter { value: String, character: char },
}

impl From<crate::package::error::Error> for Error {
    fn from(e: crate::package::error::Error) -> Self {
        Error::Package(Box::new(e))
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::InternalFile(path.as_ref().to_path_buf(), error)
}

/// Clonable, source-preserving wrapper around [`Error`].
///
/// `crate::Error` is not `Clone` because several of its variants hold
/// `io::Error`, which is not `Clone`. That prevents it from flowing through
/// APIs that must broadcast a single failure to multiple consumers — most
/// notably [`crate::utility::singleflight`], which clones the leader's error
/// to every waiter.
///
/// `ArcError` wraps the typed error in an `Arc` so cloning is cheap and
/// preserves the full error chain (`source()` delegates to the inner
/// `Error`). Callers that need to broadcast a typed `Error` should accept
/// `ArcError` in the variant that carries the failure so downstream code
/// can still walk the chain and (where necessary) downcast to the original
/// variant.
#[derive(Debug, Clone)]
pub struct ArcError(std::sync::Arc<Error>);

impl ArcError {
    /// Returns a reference to the wrapped error.
    pub fn as_error(&self) -> &Error {
        &self.0
    }
}

impl From<Error> for ArcError {
    fn from(error: Error) -> Self {
        Self(std::sync::Arc::new(error))
    }
}

impl std::fmt::Display for ArcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for ArcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::OfflineMode => Some(ExitCode::OfflineBlocked),
            Self::InternalFile(_, _) => Some(ExitCode::IoError),
            Self::InternalPathInvalid(_) => Some(ExitCode::Failure),
            Self::SerializationFailure(_) | Self::UnsupportedMediaType(_, _) => Some(ExitCode::DataError),
            // Transparent wrappers delegate to the inner error's classification.
            Self::Auth(e) => e.classify(),
            Self::Platform(e) => e.classify(),
            Self::Profile(e) => e.classify(),
            Self::PackageManager(e) => e.classify(),
            Self::OciClient(e) => e.classify(),
            Self::Identifier(e) => e.classify(),
            Self::Archive(e) => e.classify(),
            Self::Compression(e) => e.classify(),
            Self::Ci(e) => e.classify(),
            Self::Config(e) => e.classify(),
            Self::Package(e) => e.as_ref().classify(),
            // Shell errors have no specific exit code yet; defer to chain walker.
            Self::Shell(_) => None,
            Self::OciIndex(e) => e.classify(),
            Self::FileStructure(e) => e.classify(),
            Self::Digest(e) => e.classify(),
            Self::Dependency(e) => e.classify(),
            Self::PinnedIdentifier(e) => e.classify(),
            Self::EntrypointTargetInvalid { source, .. } => source.classify(),
            Self::LauncherUnsafeCharacter { .. } => Some(ExitCode::DataError),
        }
    }
}
