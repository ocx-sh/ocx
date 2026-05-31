// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;
use crate::package::metadata::entrypoint::EntrypointName;
use crate::{file_structure, oci};

/// Task-level error for package manager operations.
///
/// Each variant corresponds to a specific command and contains one
/// [`PackageError`] per failed package, preserving the individual cause.
///
/// This type does **not** wrap [`crate::Error`] directly — library errors are
/// always attached to a specific package via [`PackageErrorKind::Internal`].
///
/// # Exit code classification
///
/// Batch classification uses **first error wins**: when a batch variant
/// carries multiple [`PackageError`]s, the process exit code is derived from
/// the first element's [`PackageError::kind`]. This makes the exit code for
/// multi-package operations input-order-dependent — running
/// `ocx install a b c` where `a` fails with `NotFound` and `b` fails with
/// `SelectionAmbiguous` exits with `NotFound`'s code, regardless of how many
/// `SelectionAmbiguous` entries follow. This is the v1 contract; a future
/// priority function (e.g. "worst code wins") may upgrade the policy without
/// touching variant data.
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
    /// An inspect operation failed for one or more packages.
    #[error("{}", format_batch("inspect", _0))]
    InspectFailed(Vec<PackageError>),
    /// A select operation failed for one or more packages.
    #[error("{}", format_batch("select", _0))]
    SelectFailed(Vec<PackageError>),
    /// The self-update check operation failed.
    ///
    /// Distinct from [`InstallFailed`]: a check failure does not imply an
    /// install was attempted.  Carries the [`PackageError`] that caused the
    /// check to abort, boxed to break the recursive-type cycle
    /// (`crate::Error` → `package_manager::Error` → `PackageError` →
    /// `PackageErrorKind::Internal` → `crate::Error`).
    #[error("self-update check failed: {0}")]
    SelfCheckFailed(Box<PackageError>),
}

/// An error tied to a specific package.
///
/// `kind` deliberately omits `#[source]` — a deviation from the three-layer
/// error pattern in `quality-rust-errors.md`. Exit-code classification for
/// package errors does **not** walk the `source()` chain; it dispatches
/// directly through the [`ClassifyExitCode`] impls on both `PackageError`
/// and `PackageErrorKind` (see the bottom of this file and
/// `classify_error` in `crate::cli::classify`). Adding `#[source]` would
/// duplicate the kind into both the `Display` chain and the `source()`
/// chain without improving diagnosability.
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
    /// An entrypoint name collision was detected in the interface surface of the
    /// transitive closure. Raised at install time when N≥2 packages in the
    /// interface projection declare the same entrypoint `name`. Reports all
    /// owners so the user can deselect the right one. Supersedes the
    /// 2-owner `EntrypointNameCollision` variant (see `adr_two_env_composition.md`).
    #[error(
        "entrypoint name collision: '{name}' declared by {} packages: {}; deselect one before selecting another",
        owners.len(),
        owners.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    )]
    EntrypointCollision {
        name: EntrypointName,
        owners: Vec<oci::PinnedIdentifier>,
    },

    /// A required companion package install failed during patch discovery.
    ///
    /// The base install succeeded, but a companion marked `required = true`
    /// could not be fetched or installed. Fail-closed: the install as a whole
    /// is considered failed so the caller does not run with an incomplete
    /// environment overlay. Optional companions (required = false) are logged
    /// as warnings and do not produce this variant.
    #[error("required companion install failed for '{companion}': {source}")]
    RequiredCompanionFailed {
        /// Identifier of the companion package that failed to install.
        companion: crate::oci::Identifier,
        /// The underlying package error kind from the companion's install.
        #[source]
        source: Box<PackageErrorKind>,
    },

    /// Patch discovery failed due to a domain-level patch error (fetch, parse,
    /// persist, or structural validation of a `__ocx.patch` descriptor).
    ///
    /// Carries the full [`crate::patch::PatchError`] chain via `#[source]` so
    /// the error chain is preserved for exit-code classification and diagnostics.
    /// This replaces the former `Internal(io::Error::other(patch_error.to_string()))`
    /// workaround that erased the structured source chain.
    #[error("patch discovery error: {0}")]
    PatchDiscovery(#[source] crate::patch::PatchError),

    /// No index entry satisfies the host's detected `os.features` requirements.
    ///
    /// Raised by `Index::select` when the host declares a non-empty
    /// `os.features` set (e.g. `libc.glibc`) but every candidate platform in
    /// the index sharing the host's os+arch declares an `os_features` set that
    /// is not a subset of the host's features. This is a general
    /// `os.features` mismatch — libc is the first such feature, but the
    /// matcher is not libc-specific.
    ///
    /// The user can override by passing `--platform` with an explicit
    /// `os/arch[+feature...]` matching one of the available entries — the
    /// available platforms are rendered with their `+feature` suffixes so the
    /// value is copy-pasteable.
    ///
    /// Error string follows API Guidelines: lowercase, no period.
    #[error(
        "feature mismatch: host provides {}; available platforms: {}; pass --platform <os/arch[+features]> to override",
        host_features.join(", "),
        available.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ")
    )]
    FeatureMismatch {
        /// The `os.features` values the host reported (e.g. `["libc.glibc"]`).
        host_features: Vec<String>,
        /// The candidate [`oci::Platform`] values sharing the host os+arch, so
        /// the user can see which `--platform <os/arch[+features]>` value to
        /// pass.
        available: Vec<oci::Platform>,
    },

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
        format!("failed to {verb} package: {}", errors[0])
    } else {
        let mut s = format!("failed to {verb} {} packages:", errors.len());
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
    /// Two or more packages on the active surface resolve the same repository to
    /// different digests. A single environment cannot expose multiple versions
    /// of one package, so composition fails. The identifiers name the conflicting
    /// versions (tag and digest) so the user can tell which were involved.
    #[error("conflicting versions for {repository}: {}", identifiers.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "))]
    Conflict {
        repository: oci::Repository,
        identifiers: Vec<oci::PinnedIdentifier>,
    },
    /// Dependency setup coordination failed (capacity, timeout, or abandoned leader).
    #[error("dependency setup failed: {0}")]
    SetupFailed(#[from] crate::utility::singleflight::Error),
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        // Batch variants wrap `Vec<PackageError>` with no `#[source]`, so the
        // chain walker never reaches the inner `PackageErrorKind`. Classify
        // the first package error directly — preserves per-package semantics
        // for single-failure cases.
        match self {
            // Box<PackageError>: deref to access kind directly.
            Self::SelfCheckFailed(pe) => pe.kind.classify(),
            Self::FindFailed(es)
            | Self::InstallFailed(es)
            | Self::UninstallFailed(es)
            | Self::DeselectFailed(es)
            | Self::ResolveFailed(es)
            | Self::InspectFailed(es)
            | Self::SelectFailed(es) => es.first().and_then(|pe| pe.kind.classify()),
        }
    }
}

impl ClassifyExitCode for PackageErrorKind {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::NotFound | Self::SymlinkNotFound(_) | Self::BlobNotFound(_) => ExitCode::NotFound,
            Self::OfflineManifestMissing(_) => ExitCode::PolicyBlocked,
            Self::SelectionAmbiguous(_)
            | Self::SymlinkRequiresTag
            | Self::DigestMissing
            | Self::EntrypointCollision { .. }
            | Self::FeatureMismatch { .. } => ExitCode::DataError,
            Self::TaskPanicked => ExitCode::Failure,
            // Required companion failure: delegate to the inner error's
            // classification so the exit code reflects the root cause (e.g.
            // NotFound if the companion is not published, Unavailable for
            // network errors, etc.). The `Box<PackageErrorKind>` source
            // implements `ClassifyExitCode` so we can recurse without walking
            // `std::error::Error::source()`.
            Self::RequiredCompanionFailed { source, .. } => return source.classify(),
            // Patch discovery errors delegate to the classify_error chain walker
            // so the inner PatchError's source chain (e.g. ClientError::Authentication
            // → AuthError, ClientError::Registry → Unavailable) is fully inspected.
            Self::PatchDiscovery(inner) => {
                return Some(crate::cli::classify_error(inner as &(dyn std::error::Error + 'static)));
            }
            // Internal wraps a full `crate::Error` — walk through classify_error
            // so the inner chain is inspected via the generic entry point.
            Self::Internal(inner) => return Some(crate::cli::classify_error(inner)),
        })
    }
}

impl ClassifyExitCode for DependencyError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::Conflict { .. } => Some(ExitCode::DataError),
            // Defer to the wrapped singleflight error's source chain so the
            // underlying classifiable variant (e.g. `EntrypointCollision`)
            // wins over the generic "setup failed" wrapper. The chain walker
            // re-enters `try_classify` on the next cause.
            Self::SetupFailed(_) => None,
        }
    }
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

    #[test]
    fn inspect_failed_classifies_from_first_error() {
        let errors = vec![
            PackageError::new(
                oci::Identifier::new_registry("a", "example.com"),
                PackageErrorKind::NotFound,
            ),
            PackageError::new(
                oci::Identifier::new_registry("b", "example.com"),
                PackageErrorKind::SymlinkRequiresTag,
            ),
        ];
        assert_eq!(Error::InspectFailed(errors).classify(), Some(ExitCode::NotFound));
    }

    #[test]
    fn select_failed_classifies_from_first_error() {
        let errors = vec![PackageError::new(
            oci::Identifier::new_registry("a", "example.com"),
            PackageErrorKind::SelectionAmbiguous(vec![]),
        )];
        assert_eq!(Error::SelectFailed(errors).classify(), Some(ExitCode::DataError));
    }
}
