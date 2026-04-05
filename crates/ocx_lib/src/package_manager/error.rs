// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{file_structure, oci};

/// Task-level error for package manager operations.
///
/// Each variant corresponds to a specific command and contains one
/// [`PackageError`] per failed package, preserving the individual cause.
///
/// This type does **not** wrap [`crate::Error`] directly — library errors are
/// always attached to a specific package via [`PackageErrorKind::Internal`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A find operation failed for one or more packages.
    FindFailed(Vec<PackageError>),
    /// An install operation failed for one or more packages.
    InstallFailed(Vec<PackageError>),
    /// An uninstall operation failed for one or more packages.
    UninstallFailed(Vec<PackageError>),
    /// A deselect operation failed for one or more packages.
    DeselectFailed(Vec<PackageError>),
}

/// An error tied to a specific package.
#[derive(Debug, thiserror::Error)]
#[error("{identifier} — {kind}")]
#[non_exhaustive]
pub struct PackageError {
    pub identifier: oci::Identifier,
    pub kind: PackageErrorKind,
}

impl PackageError {
    pub fn new(identifier: oci::Identifier, kind: PackageErrorKind) -> Self {
        Self { identifier, kind }
    }
}

/// The cause of a single-package failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackageErrorKind {
    /// The package was not found in the index or object store.
    #[error("package not found")]
    NotFound,
    /// Multiple candidates matched the platform selection.
    #[error("ambiguous selection: {}", _0.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(", "))]
    SelectionAmbiguous(Vec<oci::Identifier>),
    /// A symlink-based path was requested but the identifier carries a digest.
    #[error("symlink resolution requires a tag, not a digest")]
    SymlinkRequiresTag,
    /// The requested install symlink does not exist.
    #[error("{}", match _0 {
        file_structure::SymlinkKind::Candidate => "no installed candidate",
        file_structure::SymlinkKind::Current => "no selected version",
    })]
    SymlinkNotFound(file_structure::SymlinkKind),
    /// A spawned task panicked unexpectedly.
    #[error("task panicked unexpectedly")]
    TaskPanicked,
    /// The identifier has no digest after resolution.
    #[error("identifier has no digest after resolution")]
    DigestMissing,
    /// An underlying internal error (I/O, OCI, network, etc.).
    #[error(transparent)]
    Internal(#[from] crate::Error),
}

impl From<crate::oci::client::error::ClientError> for PackageErrorKind {
    fn from(e: crate::oci::client::error::ClientError) -> Self {
        Self::Internal(e.into())
    }
}

// ---------------------------------------------------------------------------
// Display — manual impl because multi-line batch formatting is too complex
// for thiserror's `#[error(...)]` attribute.
// ---------------------------------------------------------------------------

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::FindFailed(errors) => write_batch(f, "find", errors),
            Error::InstallFailed(errors) => write_batch(f, "install", errors),
            Error::UninstallFailed(errors) => write_batch(f, "uninstall", errors),
            Error::DeselectFailed(errors) => write_batch(f, "deselect", errors),
        }
    }
}

fn write_batch(f: &mut std::fmt::Formatter<'_>, verb: &str, errors: &[PackageError]) -> std::fmt::Result {
    if errors.len() == 1 {
        write!(f, "Failed to {verb} package: {}", errors[0])
    } else {
        writeln!(f, "Failed to {verb} {} packages:", errors.len())?;
        for e in errors {
            writeln!(f, "  {e}")?;
        }
        Ok(())
    }
}

/// Errors from dependency resolution operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DependencyError {
    /// Two transitive dependencies resolve to different digests for the same repository.
    #[error("Conflicting digests for {repository}: {}", digests.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", "))]
    Conflict {
        repository: String,
        digests: Vec<oci::Digest>,
    },
    /// Dependency setup coordination failed (capacity, timeout, or abandoned leader).
    #[error("Dependency setup failed: {0}")]
    SetupFailed(#[from] crate::utility::singleflight::Error),
}
