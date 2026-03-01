use std::path::PathBuf;

/// Errors that can occur during OCI client operations.
#[derive(Debug)]
pub enum ClientError {
    /// Authentication with the registry failed.
    Authentication(String),
    /// Manifest digest mismatch between expected and actual.
    DigestMismatch { expected: String, actual: String },
    /// Expected an image manifest but got an image index or unknown type.
    UnexpectedManifestType,
    /// Manifest structure is invalid (e.g. wrong layer count, missing fields).
    InvalidManifest(String),
    /// A registry operation failed.
    Registry(String),
    /// File I/O error with path context.
    Io(PathBuf, std::io::Error),
    /// JSON serialization or deserialization failed.
    Serialization(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Authentication(msg) => write!(f, "Registry authentication failed: {}", msg),
            ClientError::DigestMismatch { expected, actual } => {
                write!(f, "Manifest digest mismatch: expected '{}', got '{}'", expected, actual)
            }
            ClientError::UnexpectedManifestType => write!(f, "Expected an image manifest, got an image index"),
            ClientError::InvalidManifest(msg) => write!(f, "Invalid manifest: {}", msg),
            ClientError::Registry(msg) => write!(f, "Registry operation failed: {}", msg),
            ClientError::Io(path, err) => write!(f, "I/O error for '{}': {}", path.display(), err),
            ClientError::Serialization(msg) => write!(f, "Serialization error: {}", msg),
        }
    }
}

impl std::error::Error for ClientError {}
