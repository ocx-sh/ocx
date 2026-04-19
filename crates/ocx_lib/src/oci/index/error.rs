// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

/// Errors specific to OCI index operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A remote manifest was expected but not found during index update.
    #[error("remote manifest not found for '{0}' during index update")]
    RemoteManifestNotFound(String),

    /// The tag lock's embedded repository does not match the expected identifier.
    #[error("tag lock repository mismatch in '{}': expected {expected}, found {found}", path.display())]
    TagLockRepositoryMismatch {
        path: std::path::PathBuf,
        expected: crate::oci::Repository,
        found: crate::oci::Repository,
    },

    /// A chained-index source walk failed. Carries the original typed error
    /// inside an [`ArcError`] so it can be cloned for singleflight broadcast
    /// to waiters while preserving the full error chain. The leader and
    /// every waiter see the same underlying `crate::Error`.
    #[error("chained index source walk failed: {0}")]
    SourceWalkFailed(#[source] crate::error::ArcError),

    /// A singleflight coordination primitive failed (capacity exceeded,
    /// timeout, or abandoned leader) while walking the chain. Distinct
    /// from [`Self::SourceWalkFailed`], which reports a source-side failure.
    #[error("chained index singleflight failed")]
    SingleflightFailed(#[source] crate::utility::singleflight::Error),

    /// A nested image index was encountered while persisting a manifest
    /// chain. The OCI spec does not describe an image index nested inside
    /// another image index, so writing one would produce a corrupt cache
    /// entry; abort the persist instead.
    #[error("nested image index at {digest} is not a supported OCI shape")]
    NestedImageIndex { digest: crate::oci::Digest },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::RemoteManifestNotFound(_) => ExitCode::NotFound,
            Self::TagLockRepositoryMismatch { .. } | Self::NestedImageIndex { .. } => ExitCode::DataError,
            // Delegate to the full chain walker on the wrapped typed error,
            // not just a single-hop `classify()` on the inner `Error`. Mirrors
            // the `PackageErrorKind::Internal(inner)` pattern so nested causes
            // (e.g. a `ClientError::Authentication` inside a `crate::Error`)
            // are resolved via the generic `try_classify` ladder.
            Self::SourceWalkFailed(arc) => return Some(crate::cli::classify_error(arc.as_error())),
            Self::SingleflightFailed(_) => ExitCode::Failure,
        })
    }
}
