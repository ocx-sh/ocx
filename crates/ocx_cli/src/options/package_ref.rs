// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Reference to a package — the parsed shape of an `ocx exec` positional.
//!
//! Two reference flavours sit behind a single positional argument so callers
//! and generated launchers share one parser:
//!
//! - **OCI identifier** (`node:20`, `oci://node:20`) — resolved through the
//!   index, auto-installed when missing.
//! - **`file://<absolute-package-root>`** — already-installed package on
//!   disk; skips identifier resolution and the index entirely. Generated
//!   launchers bake this form so an `ocx select` to a different version
//!   continues to resolve through the per-repo `current` symlink.
//!
//! Mirrors [`ocx_lib::publisher::LayerRef`]: a closed enum with `FromStr` /
//! `Display`, a typed parse error, and a small validation surface that lives
//! alongside its sole consumer (the CLI) so other binaries (mirror, future
//! SDK consumers) parse the same syntax via this module.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use ocx_lib::cli::UsageError;
use ocx_lib::oci;

use super::Identifier;

/// Error produced when a string cannot be parsed as a [`PackageRef`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PackageRefParseError {
    /// `file://` URI carried no path body (`file://` on its own).
    #[error("file:// URI must include an absolute package-root path")]
    EmptyFileUri,
    /// `file://` URI carried a path body that is not absolute.
    #[error("file:// URI must be absolute, got '{0}' (use file:///abs/path)")]
    RelativeFileUri(String),
    /// `file://` URI carried percent-encoded bytes (`%XX`). The parser does not
    /// URL-decode; passing `%XX` would be opened as a literal-byte path and fail
    /// later when `metadata.json` cannot be resolved. Surface it here instead.
    #[error("file:// URI '{0}' contains percent-encoding ('%XX') which is not supported; pass a literal absolute path")]
    PercentEncodedFileUri(String),
    /// The body after stripping a leading `oci://` (or the bare input) did
    /// not parse as an OCI identifier.
    #[error("invalid OCI identifier '{raw}': {source}")]
    InvalidIdentifier {
        /// The body that was handed to the OCI identifier parser.
        raw: String,
        /// The structured parse error from [`oci::Identifier::from_str`].
        #[source]
        source: oci::IdentifierError,
    },
}

/// A package reference — either an OCI identifier or a file:// URI pointing
/// at an installed package root on disk.
///
/// Validation is split between parse time and resolution time:
/// - Parse time (this module): scheme detection, absolute-path check on
///   `file://`, OCI-identifier shape check on `oci://` / bare.
/// - Resolution time ([`validate_package_root`], with runtime context): for
///   `file://`, the target must canonicalize inside the OCX packages root and
///   contain `metadata.json`. The library deliberately keeps that check out
///   of parsing so it can run with the live filesystem.
#[derive(Debug, Clone)]
pub enum PackageRef {
    /// OCI reference. Stores the parsed identifier (raw + default-registry
    /// applied at resolution time via [`Identifier::with_domain`]) so the
    /// caller can apply *its own* default registry — `oci::Identifier::from_str`
    /// would inject the OCI default registry, shadowing per-CLI overrides like
    /// `OCX_DEFAULT_REGISTRY`.
    Oci(Identifier),
    /// Absolute filesystem path to an already-installed package root
    /// (the directory containing `metadata.json`, `content/`, `entrypoints/`).
    PackageRoot(PathBuf),
}

impl FromStr for PackageRef {
    type Err = PackageRefParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("file://") {
            if rest.is_empty() {
                return Err(PackageRefParseError::EmptyFileUri);
            }
            // Reject percent-encoding instead of silently treating `%20` as a
            // literal-byte filename. `PathBuf::from(rest)` would happily accept
            // the literal bytes; the failure would only surface much later when
            // `metadata.json` lookup misses, with a confusing "not a package
            // root" message. Emit a clear hint at the parsing boundary.
            if rest.contains('%') {
                return Err(PackageRefParseError::PercentEncodedFileUri(rest.to_string()));
            }
            let path = PathBuf::from(rest);
            if !path.is_absolute() {
                return Err(PackageRefParseError::RelativeFileUri(rest.to_string()));
            }
            return Ok(PackageRef::PackageRoot(path));
        }

        // Strip an optional `oci://` prefix, validate the body parses as an
        // OCI identifier (against the *built-in* default registry — the real
        // default is applied later by the consumer via `Identifier::with_domain`),
        // then store the validated body in our `Identifier` newtype so the
        // structured `IdentifierError` reaches the caller without being
        // re-wrapped by `ocx_lib::Error`.
        let body = s.strip_prefix("oci://").unwrap_or(s);
        oci::Identifier::from_str(body).map_err(|source| PackageRefParseError::InvalidIdentifier {
            raw: body.to_string(),
            source,
        })?;
        Ok(PackageRef::Oci(Identifier::from_validated_raw(body)))
    }
}

impl std::fmt::Display for PackageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageRef::Oci(id) => write!(f, "{id}"),
            PackageRef::PackageRoot(path) => write!(f, "file://{}", path.display()),
        }
    }
}

/// Validate a `file://` package root: must be an absolute path that
/// canonicalizes to a location *inside* the OCX packages CAS root and contains
/// a `metadata.json` file. Returns the canonical path on success.
///
/// Runtime containment check belonging alongside [`PackageRef`] (the parser):
/// `PackageRef::from_str` only enforces shape (absolute path, `file://` scheme);
/// this function enforces *containment* + *package-root signature* once the OCX
/// home is known. Lives here so any binary consuming `PackageRef::PackageRoot`
/// performs the same defense without recopying the logic into every CLI handler.
///
/// Both checks surface as [`UsageError`] so [`ocx_lib::cli::classify_error`]
/// maps the failure to [`ocx_lib::cli::ExitCode::UsageError`] (`64`) rather
/// than the default [`ocx_lib::cli::ExitCode::Failure`] (`1`).
pub async fn validate_package_root(dir: &Path, packages_root: &Path) -> Result<PathBuf, UsageError> {
    // Canonicalize both sides so symlinks and `..` components cannot smuggle
    // a path outside the packages root. `tokio::fs::canonicalize` keeps us in
    // the async runtime instead of blocking via `std::fs`.
    let canonical_dir = tokio::fs::canonicalize(dir)
        .await
        .map_err(|e| UsageError::new(format!("file:// path '{}' cannot be resolved: {e}", dir.display())))?;
    let canonical_root = tokio::fs::canonicalize(packages_root).await.map_err(|e| {
        UsageError::new(format!(
            "file:// validation failed: cannot resolve packages root ({}): {}",
            e,
            packages_root.display()
        ))
    })?;

    if !canonical_dir.starts_with(&canonical_root) {
        return Err(UsageError::new(format!(
            "file:// path must point inside {} (got {})",
            canonical_root.display(),
            canonical_dir.display()
        )));
    }

    // Quick existence check on metadata.json — the canonical signal that
    // `dir` is actually a package root and not, say, a registry slug dir.
    let metadata = canonical_dir.join("metadata.json");
    if !tokio::fs::try_exists(&metadata).await.unwrap_or(false) {
        return Err(UsageError::new(format!(
            "file:// path is not a package root (missing metadata.json): {}",
            canonical_dir.display()
        )));
    }

    Ok(canonical_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_identifier_is_oci() {
        let r: PackageRef = "node:20".parse().unwrap();
        match r {
            PackageRef::Oci(id) => assert_eq!(id.raw(), "node:20"),
            other => panic!("expected Oci, got {other:?}"),
        }
    }

    #[test]
    fn parse_oci_scheme_strips_prefix() {
        let r: PackageRef = "oci://node:20".parse().unwrap();
        match r {
            PackageRef::Oci(id) => assert_eq!(id.raw(), "node:20"),
            other => panic!("expected Oci, got {other:?}"),
        }
    }

    #[test]
    fn parse_file_scheme_yields_package_root() {
        let r: PackageRef = "file:///abs/pkg/root".parse().unwrap();
        match r {
            PackageRef::PackageRoot(p) => assert_eq!(p, PathBuf::from("/abs/pkg/root")),
            other => panic!("expected PackageRoot, got {other:?}"),
        }
    }

    #[test]
    fn parse_file_scheme_relative_rejected() {
        let err: PackageRefParseError = "file://relative/path".parse::<PackageRef>().unwrap_err();
        match err {
            PackageRefParseError::RelativeFileUri(body) => assert_eq!(body, "relative/path"),
            other => panic!("expected RelativeFileUri, got {other:?}"),
        }
    }

    #[test]
    fn parse_file_scheme_empty_rejected() {
        let err: PackageRefParseError = "file://".parse::<PackageRef>().unwrap_err();
        assert!(matches!(err, PackageRefParseError::EmptyFileUri));
    }

    #[test]
    fn parse_file_scheme_percent_encoded_rejected() {
        // `%20` would be a literal-byte filename if accepted; surface the issue
        // at parse time so the user sees a clean hint instead of a downstream
        // "metadata.json missing" error.
        let err: PackageRefParseError = "file:///abs/path%20with%20space".parse::<PackageRef>().unwrap_err();
        match err {
            PackageRefParseError::PercentEncodedFileUri(body) => {
                assert_eq!(body, "/abs/path%20with%20space");
                let msg = PackageRefParseError::PercentEncodedFileUri(body).to_string();
                assert!(
                    msg.contains("percent-encoding"),
                    "msg should mention percent-encoding: {msg}"
                );
            }
            other => panic!("expected PercentEncodedFileUri, got {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_identifier_surfaces_structured_error() {
        // Empty body fails OCI parsing; structural error must carry the raw
        // body so callers can render a clean message.
        let err: PackageRefParseError = "oci://".parse::<PackageRef>().unwrap_err();
        match err {
            PackageRefParseError::InvalidIdentifier { raw, source: _ } => assert_eq!(raw, ""),
            other => panic!("expected InvalidIdentifier, got {other:?}"),
        }
    }

    #[test]
    fn oci_identifier_applies_default_registry_on_resolution() {
        let r: PackageRef = "node:20".parse().unwrap();
        match r {
            PackageRef::Oci(id) => {
                let resolved = id.with_domain("example.com").unwrap();
                assert_eq!(resolved.registry(), "example.com");
                assert_eq!(resolved.repository(), "node");
            }
            other => panic!("expected Oci, got {other:?}"),
        }
    }

    #[test]
    fn display_round_trips_bare_identifier() {
        let r: PackageRef = "node:20".parse().unwrap();
        assert_eq!(r.to_string(), "node:20");
    }

    #[test]
    fn display_round_trips_oci_scheme_strips_prefix() {
        let r: PackageRef = "oci://node:20".parse().unwrap();
        assert_eq!(r.to_string(), "node:20");
    }

    #[test]
    fn display_round_trips_file_scheme() {
        let r: PackageRef = "file:///abs/pkg/root".parse().unwrap();
        assert_eq!(r.to_string(), "file:///abs/pkg/root");
    }

    // ── validate_package_root ─────────────────────────────────────────────

    #[tokio::test]
    async fn validate_rejects_outside_packages_root() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let outside = tempfile::tempdir().expect("outside");
        let err = validate_package_root(outside.path(), packages_root.path())
            .await
            .expect_err("outside packages root must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("file://"), "msg should name scheme: {msg}");
    }

    #[tokio::test]
    async fn validate_rejects_inside_root_without_metadata_json() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let inside = packages_root.path().join("registry/algo/aa/rest");
        tokio::fs::create_dir_all(&inside).await.expect("mkdir");
        let err = validate_package_root(&inside, packages_root.path())
            .await
            .expect_err("missing metadata.json must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("metadata.json"), "msg should name metadata.json: {msg}");
    }

    #[tokio::test]
    async fn validate_accepts_path_under_packages_root_with_metadata() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let pkg_root = packages_root.path().join("registry/algo/aa/rest");
        tokio::fs::create_dir_all(&pkg_root).await.expect("mkdir");
        tokio::fs::write(pkg_root.join("metadata.json"), b"{}")
            .await
            .expect("write metadata");
        let canonical = validate_package_root(&pkg_root, packages_root.path())
            .await
            .expect("inside path with metadata.json should be accepted");
        assert!(canonical.starts_with(packages_root.path().canonicalize().expect("canon root")));
    }

    #[tokio::test]
    async fn validate_rejects_nonexistent_path_with_usage_error() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let missing = packages_root.path().join("does-not-exist");
        let err = validate_package_root(&missing, packages_root.path())
            .await
            .expect_err("missing dir must be rejected");
        assert!(err.to_string().contains("file://"));
    }

    // ── W4: symlink-pointing-outside-root rejection (CWE-22) ─────────────
    //
    // The runtime containment check relies on `tokio::fs::canonicalize`
    // resolving symlinks. A symlink that lives INSIDE `packages_root` but
    // points to a target OUTSIDE must be rejected post-canonicalize. This is
    // the explicit attack the canonicalize defense was added for: a
    // publisher (or local mutation) cannot smuggle an out-of-root path by
    // hiding it behind an in-root symlink.
    //
    // Gated `#[cfg(unix)]` because Windows symlink semantics differ
    // (privilege requirement + junction handling) and the test fixture would
    // need a different setup there. Linux CI fully covers the canonicalize
    // contract on this code path.
    #[cfg(unix)]
    #[tokio::test]
    async fn validate_rejects_symlink_pointing_outside_packages_root() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let outside = tempfile::tempdir().expect("outside dir");

        // Create a real package-root-shaped directory OUTSIDE packages_root,
        // complete with the metadata.json signal so the symlink would
        // otherwise pass the existence check if it were a real dir.
        let outside_pkg = outside.path().join("evil_pkg");
        tokio::fs::create_dir_all(&outside_pkg).await.expect("mkdir outside");
        tokio::fs::write(outside_pkg.join("metadata.json"), b"{}")
            .await
            .expect("write outside metadata");

        // Place a symlink INSIDE packages_root that points to the outside dir.
        let escape_link = packages_root.path().join("escape_pkg");
        std::os::unix::fs::symlink(&outside_pkg, &escape_link).expect("create symlink");

        // Containment check must reject after canonicalize resolves the link.
        let err = validate_package_root(&escape_link, packages_root.path())
            .await
            .expect_err("symlink pointing outside packages_root must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("file://") && msg.contains("must point inside"),
            "containment-violation error must surface (file:// + 'must point inside'); got: {msg}"
        );
    }
}
