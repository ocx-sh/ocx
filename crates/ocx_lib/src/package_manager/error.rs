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
    #[error("{}", format_batch("find", _0))]
    FindFailed(Vec<PackageError>),
    /// An install operation failed for one or more packages.
    #[error("{}", format_batch("install", _0))]
    InstallFailed(Vec<PackageError>),
    /// An uninstall operation failed for one or more packages.
    #[error("{}", format_batch("uninstall", _0))]
    UninstallFailed(Vec<PackageError>),
    /// A deselect operation failed for one or more packages.
    #[error("{}", format_batch("deselect", _0))]
    DeselectFailed(Vec<PackageError>),
    /// A resolve operation failed for one or more packages.
    #[error("{}", format_batch("resolve", _0))]
    ResolveFailed(Vec<PackageError>),
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

/// Payload for [`PackageErrorKind::OfflineManifestMissing`]. Boxed in the
/// enum variant to keep `PackageErrorKind` small (avoids the
/// `clippy::result_large_err` lint).
#[derive(Debug)]
pub struct OfflineManifestMissing {
    pub identifier: oci::Identifier,
    pub digest: oci::Digest,
}

/// The cause of a single-package failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackageErrorKind {
    /// The package was not found in the index or object store.
    #[error("package not found")]
    NotFound,
    /// Offline mode: the tag pointer is cached locally but the manifest
    /// blob is missing from `blobs/`. The caller needs to re-run the
    /// command online to populate the blob cache.
    #[error(
        "manifest {} is not in the local cache; run `ocx install {}` online to populate it",
        _0.digest,
        _0.identifier
    )]
    OfflineManifestMissing(Box<OfflineManifestMissing>),
    /// A referenced blob (layer digest) was not present in the registry.
    ///
    /// The identifier is `registry/repository[:tag]@<blob-digest>` — see
    /// [`crate::oci::client::error::ClientError::BlobNotFound`] for the
    /// canonical construction contract.
    #[error("blob not found: {0}")]
    BlobNotFound(oci::PinnedIdentifier),
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
        match e {
            crate::oci::client::error::ClientError::BlobNotFound(pinned) => Self::BlobNotFound(pinned),
            other => Self::Internal(other.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Batch formatter — used by `#[error(...)]` attributes on `Error` variants.
// ---------------------------------------------------------------------------

fn format_batch(verb: &str, errors: &[PackageError]) -> String {
    use std::fmt::Write as _;
    if errors.len() == 1 {
        format!("Failed to {verb} package: {}", errors[0])
    } else {
        let mut s = format!("Failed to {verb} {} packages:", errors.len());
        for e in errors {
            let _ = write!(s, "\n  {e}");
        }
        s
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::client::error::ClientError;

    #[test]
    fn client_blob_not_found_routes_to_typed_kind() {
        let image: oci::native::Reference = "example.com/foo/bar:1.0".parse().unwrap();
        let blob_str = format!("sha256:{}", "a".repeat(64));
        let blob = oci::Digest::try_from(blob_str.as_str()).unwrap();
        let e = ClientError::blob_not_found(&image, &blob);
        let kind: PackageErrorKind = e.into();
        match kind {
            PackageErrorKind::BlobNotFound(pinned) => {
                assert_eq!(pinned.registry(), "example.com");
                assert_eq!(pinned.repository(), "foo/bar");
                assert_eq!(pinned.tag(), Some("1.0"));
                assert_eq!(pinned.digest().to_string(), blob_str);
            }
            other => panic!("expected BlobNotFound, got {other:?}"),
        }
    }

    #[test]
    fn other_client_errors_still_route_to_internal() {
        let e = ClientError::ManifestNotFound("example.com/pkg".to_string());
        let kind: PackageErrorKind = e.into();
        assert!(matches!(kind, PackageErrorKind::Internal(_)));
    }
}
