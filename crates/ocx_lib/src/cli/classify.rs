// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error → [`ExitCode`] classification shared by all OCX binaries.
//!
//! Classification is distributed across error types via the
//! [`ClassifyExitCode`] trait. Each error type owns the mapping from its own
//! variants to a process exit code, placing the knowledge next to the type
//! definition. This makes the decision easy to nest: a wrapper variant that
//! carries an inner classifiable error can recursively call `inner.classify()`
//! and either delegate or override based on its own context.
//!
//! The binary-facing entry point [`classify_error`] walks a
//! [`std::error::Error`] chain via `source()` and downcasts each cause to the
//! known `ocx_lib` error subtrees, returning the first [`ExitCode`] that
//! matches. Taking the std boundary type keeps `ocx_lib` free of an `anyhow`
//! dependency; binaries using anyhow call `classify_error(err.as_ref())` via
//! `anyhow::Error`'s `AsRef<dyn std::error::Error + 'static>` impl.
//!
//! When no subtree matches, the classifier falls through to
//! [`ExitCode::Failure`]. A locked-in fall-through test in this module
//! prevents silent drift.
//!
//! TODO(future): evaluate a proc-macro crate (e.g. a `derive(ClassifyExitCode)`
//! helper) to remove the manual `try_downcast!` entries in
//! [`try_classify`]. For now a declarative macro keeps the dispatch local and
//! dependency-free.

use crate::cli::ExitCode;

/// Classify an error into an [`ExitCode`].
///
/// Each library error type implements this trait, owning the mapping from its
/// own variants to a process exit code. The default impl returns `None` so
/// types that do not classify can opt out.
///
/// # Composition
///
/// A wrapper variant that holds an inner classifiable error can recursively
/// call `inner.classify()` and either return the inner code as-is (delegate)
/// or override it based on its own context. This keeps each impl self-
/// contained: an `OciClientError::Registry` impl can inspect its inner cause
/// and decide whether to surface it or translate it (e.g. timeout vs. auth).
pub trait ClassifyExitCode {
    /// Return an exit code for this error, or `None` to defer to the next
    /// link in the source chain.
    fn classify(&self) -> Option<ExitCode> {
        None
    }
}

/// Classify a [`std::error::Error`] chain into an [`ExitCode`].
///
/// Walks the error chain via [`std::error::Error::source`] and downcasts each
/// cause to each known classifiable type. The first cause with a non-`None`
/// [`ClassifyExitCode::classify`] result wins; otherwise the function falls
/// back to [`ExitCode::Failure`].
pub fn classify_error(err: &(dyn std::error::Error + 'static)) -> ExitCode {
    // `successors` walks `err → err.source() → …` without allocating,
    // giving us the same reach as `anyhow::Error::chain()` through the
    // std boundary type.
    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(code) = try_classify(cause) {
            return code;
        }
    }
    ExitCode::Failure
}

/// Downcast `cause` to every known classifiable type and return the first
/// [`ExitCode`] produced by [`ClassifyExitCode::classify`].
///
/// Add a new `try_downcast!` entry here whenever a new top-level error type
/// gains a `ClassifyExitCode` impl. Each downcast is O(1) (`TypeId` check), so
/// the ladder is cheap even when it grows.
fn try_classify(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    use crate::archive::Error as ArchiveError;
    use crate::auth::error::AuthError;
    use crate::ci::error::Error as CiError;
    use crate::cli::error::UsageError;
    use crate::compression::error::Error as CompressionError;
    use crate::config::error::Error as ConfigError;
    use crate::file_structure::error::Error as FileStructureError;
    use crate::oci::client::error::ClientError;
    use crate::oci::digest::error::DigestError;
    use crate::oci::identifier::error::IdentifierError;
    use crate::oci::index::error::Error as OciIndexError;
    use crate::oci::pinned_identifier::PinnedIdentifierError;
    use crate::oci::platform::error::PlatformError;
    use crate::package::error::Error as PackageError;
    use crate::package_manager::error::{DependencyError, Error as PackageManagerError, PackageErrorKind};
    use crate::profile::ProfileError;

    macro_rules! try_downcast {
        ($ty:ty) => {
            if let Some(e) = cause.downcast_ref::<$ty>()
                && let Some(code) = e.classify()
            {
                return Some(code);
            }
        };
    }

    try_downcast!(UsageError);
    try_downcast!(crate::Error);
    try_downcast!(ConfigError);
    try_downcast!(ClientError);
    try_downcast!(OciIndexError);
    try_downcast!(DigestError);
    try_downcast!(IdentifierError);
    try_downcast!(PlatformError);
    try_downcast!(PinnedIdentifierError);
    try_downcast!(PackageManagerError);
    try_downcast!(PackageErrorKind);
    try_downcast!(DependencyError);
    try_downcast!(AuthError);
    try_downcast!(FileStructureError);
    try_downcast!(ArchiveError);
    try_downcast!(CompressionError);
    try_downcast!(CiError);
    try_downcast!(PackageError);
    try_downcast!(ProfileError);

    // `std::io::Error` is not OCX-owned, so we cannot impl `ClassifyExitCode`
    // for it (orphan rule). Only `PermissionDenied` maps to a specific code;
    // everything else falls through to the chain walker.
    if let Some(io) = cause.downcast_ref::<std::io::Error>()
        && io.kind() == std::io::ErrorKind::PermissionDenied
    {
        return Some(ExitCode::PermissionDenied);
    }

    None
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::error::Error as ConfigError;
    use crate::oci::client::error::ClientError;
    use crate::package_manager::error::{DependencyError, PackageError, PackageErrorKind};

    // Helper: wrap any error into a `Box<dyn std::error::Error + 'static>` and
    // call classify_error. The turbofish helps when the compiler can't infer
    // the concrete type.
    fn classify<E: std::error::Error + 'static>(err: E) -> ExitCode {
        classify_error(&err as &(dyn std::error::Error + 'static))
    }

    // ── config::Error variants ───────────────────────────────────────────────

    #[test]
    fn config_file_not_found_maps_to_not_found() {
        // Plan taxonomy: config::Error::FileNotFound → NotFound (79)
        let err = ConfigError::FileNotFound {
            path: PathBuf::from("/nonexistent.toml"),
        };
        assert_eq!(classify(err), ExitCode::NotFound);
    }

    #[test]
    fn config_file_too_large_maps_to_config_error() {
        // Plan taxonomy: config::Error::FileTooLarge → ConfigError (78)
        let err = ConfigError::FileTooLarge {
            path: PathBuf::from("/huge.toml"),
            size: 100_000,
            limit: 65_536,
        };
        assert_eq!(classify(err), ExitCode::ConfigError);
    }

    #[test]
    fn config_parse_error_maps_to_config_error() {
        // Plan taxonomy: config::Error::Parse → ConfigError (78)
        let toml_err = toml::from_str::<toml::Value>("invalid =[[[").unwrap_err();
        let err = ConfigError::Parse {
            path: PathBuf::from("/bad.toml"),
            source: toml_err,
        };
        assert_eq!(classify(err), ExitCode::ConfigError);
    }

    #[test]
    fn config_io_error_maps_to_io_error() {
        // Plan taxonomy: config::Error::Io → IoError (74)
        let err = ConfigError::Io {
            path: PathBuf::from("/config.toml"),
            source: std::io::Error::other("read failure"),
        };
        assert_eq!(classify(err), ExitCode::IoError);
    }

    // ── ocx_lib::Error variants ──────────────────────────────────────────────

    #[test]
    fn lib_offline_mode_maps_to_offline_blocked() {
        // Plan taxonomy: ocx_lib::Error::OfflineMode → OfflineBlocked (81)
        let err = crate::Error::OfflineMode;
        assert_eq!(classify(err), ExitCode::OfflineBlocked);
    }

    // ── std::io::Error with PermissionDenied kind ────────────────────────────

    #[test]
    fn io_permission_denied_maps_to_permission_denied() {
        // Plan taxonomy: std::io::ErrorKind::PermissionDenied → PermissionDenied (77)
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "EPERM");
        assert_eq!(classify(err), ExitCode::PermissionDenied);
    }

    // ── PackageManager three-layer chain ────────────────────────────────────

    #[test]
    fn lib_package_manager_find_failed_maps_to_not_found() {
        // Plan taxonomy: three-layer chain — first error wins.
        // crate::Error::PackageManager(Error::FindFailed([PackageError{NotFound}]))
        // → ExitCode::NotFound (79). Locks in "first error wins" behavior
        // across the three-layer chain.
        let identifier = crate::oci::Identifier::new_registry("pkg", "example.com");
        let inner = PackageError::new(identifier, PackageErrorKind::NotFound);
        let pm_err = crate::package_manager::error::Error::FindFailed(vec![inner]);
        let err = crate::Error::PackageManager(pm_err);
        assert_eq!(classify(err), ExitCode::NotFound);
    }

    // ── ClientError variants ─────────────────────────────────────────────────

    #[test]
    fn client_authentication_maps_to_auth_error() {
        // Plan taxonomy: ClientError::Authentication → AuthError (80)
        let err = ClientError::Authentication(Box::new(std::io::Error::other("bad creds")));
        assert_eq!(classify(err), ExitCode::AuthError);
    }

    #[test]
    fn client_manifest_not_found_maps_to_not_found() {
        // Plan taxonomy: ClientError::ManifestNotFound → NotFound (79)
        let err = ClientError::ManifestNotFound("x/y".into());
        assert_eq!(classify(err), ExitCode::NotFound);
    }

    #[test]
    fn client_invalid_manifest_maps_to_data_error() {
        // Plan taxonomy: ClientError::InvalidManifest → DataError (65)
        // (BlobNotFound omitted: requires full PinnedIdentifier construction)
        let err = ClientError::InvalidManifest("m".into());
        assert_eq!(classify(err), ExitCode::DataError);
    }

    #[test]
    fn client_registry_maps_to_unavailable() {
        // Plan taxonomy: ClientError::Registry → Unavailable (69)
        let err = ClientError::Registry(Box::new(std::io::Error::other("503")));
        assert_eq!(classify(err), ExitCode::Unavailable);
    }

    #[test]
    fn client_io_maps_to_io_error() {
        // Plan taxonomy: ClientError::Io → IoError (74)
        let err = ClientError::Io {
            path: PathBuf::from("/x"),
            source: std::io::Error::other("eio"),
        };
        assert_eq!(classify(err), ExitCode::IoError);
    }

    // ── DataError variants across subtypes ───────────────────────────────────

    #[test]
    fn digest_invalid_maps_to_data_error() {
        // Plan taxonomy: DigestError::Invalid → DataError (65)
        let err = crate::oci::digest::error::DigestError::Invalid("not-a-digest".into());
        assert_eq!(classify(err), ExitCode::DataError);
    }

    #[test]
    fn identifier_error_maps_to_data_error() {
        // Plan taxonomy: IdentifierError (any kind) → DataError (65)
        let err = crate::oci::identifier::error::IdentifierError::new(
            "bad-input",
            crate::oci::identifier::error::IdentifierErrorKind::InvalidFormat,
        );
        assert_eq!(classify(err), ExitCode::DataError);
    }

    #[test]
    fn platform_error_maps_to_data_error() {
        // Plan taxonomy: PlatformError → DataError (65)
        let err = crate::oci::platform::error::PlatformError {
            input: "bad/os/arch".into(),
            kind: crate::oci::platform::error::PlatformErrorKind::InvalidFormat,
        };
        assert_eq!(classify(err), ExitCode::DataError);
    }

    #[test]
    fn file_structure_missing_digest_maps_to_data_error() {
        // Plan taxonomy: file_structure::Error::MissingDigest → DataError (65)
        let err = crate::file_structure::error::Error::MissingDigest("some/pkg:1.0".into());
        assert_eq!(classify(err), ExitCode::DataError);
    }

    #[test]
    fn archive_unsupported_format_maps_to_data_error() {
        // Plan taxonomy: archive::Error::UnsupportedFormat → DataError (65)
        let err = crate::archive::Error::UnsupportedFormat(".rar".into());
        assert_eq!(classify(err), ExitCode::DataError);
    }

    // ── DependencyError variants ─────────────────────────────────────────────

    #[test]
    fn dependency_conflict_maps_to_data_error() {
        // Plan taxonomy: DependencyError::Conflict → DataError (65)
        let err = DependencyError::Conflict {
            repository: "pkg".into(),
            digests: vec![],
        };
        assert_eq!(classify(err), ExitCode::DataError);
    }

    // SetupFailed omitted: singleflight::Error has no public test constructor.

    // ── OciIndex chain-walk delegation ───────────────────────────────────────

    #[test]
    fn oci_index_source_walk_failed_delegates_to_inner_auth_error() {
        // Plan: `OciIndexError::SourceWalkFailed` returns `None` from its own
        // `classify()` so the chain walker continues via `source()`. The
        // `#[source]` attribute on the `ArcError` payload surfaces the wrapped
        // `crate::Error`, letting the generic `try_classify` ladder downcast
        // and resolve the inner cause — here a `ClientError::Authentication`
        // → `ExitCode::AuthError`. Before the fix, `SourceWalkFailed` called
        // `arc.as_error().classify()` directly, which resolves only one hop
        // and misses any deeper nesting.
        let client_err = ClientError::Authentication(Box::new(std::io::Error::other("bad creds")));
        let inner: crate::Error = client_err.into();
        let arc: crate::error::ArcError = inner.into();
        let err = crate::oci::index::error::Error::SourceWalkFailed(arc);
        assert_eq!(classify(err), ExitCode::AuthError);
    }

    // ── Fall-through lock-in ─────────────────────────────────────────────────

    #[test]
    fn unclassified_error_falls_through_to_failure() {
        // Plan: default fall-through — any error not matched by the dispatch ladder
        // must return ExitCode::Failure (1), not panic or return a wrong code.
        // Using a plain std::io::Error with Other kind (not PermissionDenied) as
        // a representative "unclassified" error.
        let err = std::io::Error::other("something unclassified");
        assert_eq!(classify(err), ExitCode::Failure);
    }
}
