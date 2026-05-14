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

/// Infallible variant of [`ClassifyExitCode`] for leaf "kind" enums.
///
/// "Kind" enums (e.g. `SignErrorKind`, `VerifyErrorKind`) are pure
/// discriminants — every variant has a well-defined exit code by construction.
/// Using a separate trait with a non-`Option` return value forces each impl to
/// be exhaustive: adding a new variant produces a match-exhaustiveness compile
/// error in the impl body, keeping the exit-code contract in lockstep with the
/// enum without a separate table that can silently drift.
///
/// Wrapping error types (e.g. `SignError { identifier, kind }`) still implement
/// [`ClassifyExitCode`] and delegate to `self.kind.exit_code()` wrapped in
/// `Some(_)`.
pub trait ClassifyErrorKind {
    /// Return the exit code this kind maps to.
    fn exit_code(&self) -> ExitCode;

    /// Stable snake_case discriminant for `envelope.error.detail`.
    ///
    /// Frozen contract C-S1-1 — values must NOT change between releases.
    /// Consumers pattern-match on this string to dispatch programmatically
    /// without parsing stderr. The snake_case parallel to `exit_code()`:
    /// coarse category goes on `exit_code`, fine-grained variant name goes here.
    ///
    /// Implementations must be exhaustive (no wildcard `_` arm) so that adding
    /// a new variant produces a compile error and forces an explicit mapping.
    fn kind_detail(&self) -> &'static str;
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
    use crate::cli::error::{MetadataResolutionError, UsageError};
    use crate::compression::error::Error as CompressionError;
    use crate::config::error::Error as ConfigError;
    use crate::file_structure::error::Error as FileStructureError;
    use crate::oci::client::error::ClientError;
    use crate::oci::digest::error::DigestError;
    use crate::oci::identifier::error::IdentifierError;
    use crate::oci::index::error::Error as OciIndexError;
    use crate::oci::pinned_identifier::PinnedIdentifierError;
    use crate::oci::platform::error::PlatformError;
    use crate::oci::sign::SignError;
    use crate::oci::verify::VerifyError;
    use crate::package::error::Error as PackageError;
    use crate::package_manager::error::{DependencyError, Error as PackageManagerError, PackageErrorKind};
    use crate::project::error::Error as ProjectError;
    use crate::utility::fs::{EmptyOrAbsentError, SameFilesystemError, SymlinkWalkError};

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
    try_downcast!(MetadataResolutionError);
    try_downcast!(SymlinkWalkError);
    try_downcast!(SameFilesystemError);
    try_downcast!(EmptyOrAbsentError);
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
    try_downcast!(ProjectError);
    try_downcast!(SignError);
    try_downcast!(VerifyError);

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
    use crate::config::error::{ConfigSource, Error as ConfigError};
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
            tier: ConfigSource::Config,
        };
        assert_eq!(classify(err), ExitCode::NotFound);
    }

    #[test]
    fn project_file_not_found_maps_to_not_found() {
        // Symmetry: project-tier `FileNotFound` also maps to NotFound (79).
        // Both `--config` and `--project` should exit 79 when the named file
        // is missing.
        let err = ConfigError::FileNotFound {
            path: PathBuf::from("/nonexistent.project.toml"),
            tier: ConfigSource::Project,
        };
        assert_eq!(classify(err), ExitCode::NotFound);
    }

    #[test]
    fn project_file_not_found_error_message_points_at_project_flag() {
        // Regression guard: a missing `--project` file must cite the project
        // flag/env var, not the config ones. Rendering the Display output
        // matters because this string is what users see on exit.
        let err = ConfigError::FileNotFound {
            path: PathBuf::from("/missing.toml"),
            tier: ConfigSource::Project,
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("--project") && rendered.contains("OCX_PROJECT"),
            "project-tier FileNotFound must cite --project flag/env, got: {rendered}"
        );
        assert!(
            !rendered.contains("--config") && !rendered.contains("OCX_CONFIG"),
            "project-tier FileNotFound must NOT misdirect to --config, got: {rendered}"
        );
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
            tier: ConfigSource::Config,
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

    /// `DependencyError::SetupFailed` itself returns `None` from `classify()` so the
    /// chain walker continues via `source()`. With `singleflight::Error::Failed` carrying
    /// `#[source]` and `SharedError::source()` exposing the wrapped error directly, the
    /// walker reaches the inner `PackageErrorKind::EntrypointCollision` and recovers
    /// `ExitCode::DataError`. Before the chain fix, the walker stopped at the wrapper
    /// and fell through to `ExitCode::Failure` — masking the typed discriminant.
    #[test]
    fn dependency_setup_failed_singleflight_collision_classifies_to_data_error() {
        use crate::oci;
        use crate::package::metadata::entrypoint::EntrypointName;
        use crate::utility::singleflight;

        let name = EntrypointName::try_from("cmake").unwrap();
        let hex = "a".repeat(64);
        let id_a: oci::Identifier = format!("ocx.sh/foo:1.0@sha256:{hex}").parse().unwrap();
        let id_b: oci::Identifier = format!("ocx.sh/bar:1.0@sha256:{hex}").parse().unwrap();
        let inner = PackageErrorKind::EntrypointCollision {
            name,
            owners: vec![
                oci::PinnedIdentifier::try_from(id_a).unwrap(),
                oci::PinnedIdentifier::try_from(id_b).unwrap(),
            ],
        };
        let shared = singleflight::SharedError::for_test(inner);
        let err = DependencyError::SetupFailed(singleflight::Error::Failed(shared));
        assert_eq!(classify(err), ExitCode::DataError);
    }

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

    // ── SignError (Slice 1 — referrers signing) ─────────────────────────────

    #[test]
    fn sign_error_oidc_token_rejected_maps_to_auth_error() {
        // Slice 1 C-S1-1: SignError delegates to SignErrorKind; OidcTokenRejected → 80
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::sign::SignError::new(id, crate::oci::sign::SignErrorKind::OidcTokenRejected);
        assert_eq!(classify(err), ExitCode::AuthError);
    }

    #[test]
    fn sign_error_rekor_unavailable_maps_to_rekor_unavailable() {
        // Slice 1: distinct exit code 82 so operators can distinguish Rekor
        // outage from registry outage.
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::sign::SignError::new(id, crate::oci::sign::SignErrorKind::RekorUnavailable);
        assert_eq!(classify(err), ExitCode::RekorUnavailable);
    }

    #[test]
    fn sign_error_referrers_unsupported_maps_to_referrers_unsupported() {
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::sign::SignError::new(id, crate::oci::sign::SignErrorKind::ReferrersUnsupported);
        assert_eq!(classify(err), ExitCode::ReferrersUnsupported);
    }

    #[test]
    fn sign_error_offline_sign_refused_maps_to_permission_denied() {
        // Slice 1 policy: `ocx package sign --offline` is rejected at the CLI.
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::sign::SignError::new(id, crate::oci::sign::SignErrorKind::OfflineSignRefused);
        assert_eq!(classify(err), ExitCode::PermissionDenied);
    }

    // ── VerifyError (Slice 1 — referrers verify) ────────────────────────────

    #[test]
    fn verify_error_no_signatures_found_maps_to_not_found() {
        // Slice 1 C-S1-2: "not signed" must exit 79 so scripts can distinguish
        // "no signature" from "bad signature" without stderr parsing.
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::verify::VerifyError::new(id, crate::oci::verify::VerifyErrorKind::NoSignaturesFound);
        assert_eq!(classify(err), ExitCode::NotFound);
    }

    #[test]
    fn verify_error_identity_mismatch_maps_to_permission_denied() {
        // Slice 1: "verified, but not by the signer you expected" = 77.
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::verify::VerifyError::new(id, crate::oci::verify::VerifyErrorKind::IdentityMismatch);
        assert_eq!(classify(err), ExitCode::PermissionDenied);
    }

    #[test]
    fn verify_error_issuer_mismatch_maps_to_permission_denied() {
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::verify::VerifyError::new(id, crate::oci::verify::VerifyErrorKind::IssuerMismatch);
        assert_eq!(classify(err), ExitCode::PermissionDenied);
    }

    #[test]
    fn verify_error_bundle_parse_failed_maps_to_data_error() {
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::verify::VerifyError::new(id, crate::oci::verify::VerifyErrorKind::BundleParseFailed);
        assert_eq!(classify(err), ExitCode::DataError);
    }

    #[test]
    fn verify_error_rekor_set_invalid_maps_to_data_error() {
        // RekorSetInvalid is a crypto / data integrity failure (tampered bundle),
        // not a service-unavailability signal. Exit 65 (DataError) so retry
        // handlers do not retry a tampered SET.
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::verify::VerifyError::new(id, crate::oci::verify::VerifyErrorKind::RekorSetInvalid);
        assert_eq!(classify(err), ExitCode::DataError);
    }

    #[test]
    fn verify_error_referrers_unsupported_maps_to_referrers_unsupported() {
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::verify::VerifyError::new(id, crate::oci::verify::VerifyErrorKind::ReferrersUnsupported);
        assert_eq!(classify(err), ExitCode::ReferrersUnsupported);
    }

    #[test]
    fn verify_error_trust_root_unavailable_maps_to_config_error() {
        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let err = crate::oci::verify::VerifyError::new(id, crate::oci::verify::VerifyErrorKind::TrustRootUnavailable);
        assert_eq!(classify(err), ExitCode::ConfigError);
    }

    // ── SignError: IdentityTokenFilePermissive walks the full chain ──────────

    #[test]
    fn sign_error_identity_token_file_permissive_maps_to_permission_denied() {
        // B-T1: `classify_error` must walk the full `source()` chain and return
        // `ExitCode::PermissionDenied` (77) when `SignErrorKind::IdentityTokenFilePermissive`
        // is buried one level deep under a context wrapper error.
        //
        // Motivation: `classify_error` uses `std::iter::successors(Some(err), |e| e.source())`
        // to walk the chain. This test proves the walker does NOT stop at the outer
        // wrapper (which has no `ClassifyExitCode` impl) and continues to the `SignError`
        // carried via `source()`.

        // A minimal wrapper that simulates an `anyhow::context()` layer: it has
        // a human-readable message and carries its cause via `source()`.
        #[derive(Debug)]
        struct ContextWrapper {
            msg: &'static str,
            source: crate::oci::sign::SignError,
        }

        impl std::fmt::Display for ContextWrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.msg)
            }
        }

        impl std::error::Error for ContextWrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.source)
            }
        }

        let id = crate::oci::Identifier::parse("registry.example/pkg:1.0").unwrap();
        let sign_err = crate::oci::sign::SignError::new(
            id,
            crate::oci::sign::SignErrorKind::IdentityTokenFilePermissive {
                path: std::path::PathBuf::from("/tmp/token"),
                mode: 0o644,
            },
        );
        // Wrap in a context layer — the outer error has no ClassifyExitCode impl,
        // so the classifier must descend via source() to find the SignError.
        let wrapped = ContextWrapper {
            msg: "reading identity token file for sign operation",
            source: sign_err,
        };

        assert_eq!(
            classify_error(&wrapped as &(dyn std::error::Error + 'static)),
            ExitCode::PermissionDenied,
        );
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
