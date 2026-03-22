// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A network operation was attempted while in offline mode.
    #[error("A network operation was attempted while in offline mode.")]
    OfflineMode,

    /// A file I/O error with path context.
    #[error("Internal file error for '{path}': {source}", path = .0.display(), source = .1)]
    InternalFile(std::path::PathBuf, #[source] std::io::Error),
    /// A path has an unexpected structure.
    #[error("Path '{}' has an unexpected structure", .0.display())]
    InternalPathInvalid(std::path::PathBuf),

    /// JSON serialization or deserialization failed.
    #[error("JSON serialization error: {0}")]
    SerializationFailure(#[from] serde_json::Error),

    /// An unsupported OCI media type was encountered.
    #[error("Unsupported media type '{media_type}'. Expected media types are: {supported}", media_type = .0, supported = .1.join(", "))]
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
    Package(#[from] crate::package::error::Error),
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
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::InternalFile(path.as_ref().to_path_buf(), error)
}
