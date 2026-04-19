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
//! Mirrors [`crate::publisher::LayerRef`]: a closed enum with `FromStr` /
//! `Display`, a typed parse error, and a small validation surface that lives
//! in the library so every binary (CLI, mirror, future SDK consumers) parses
//! the same syntax.

use std::path::PathBuf;
use std::str::FromStr;

use crate::oci;

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
/// - Resolution time (caller, with runtime context): for `file://`, the
///   target must canonicalize inside the OCX packages root and contain
///   `metadata.json`. The library deliberately keeps that check out of
///   parsing so it can run with the live filesystem.
#[derive(Debug, Clone)]
pub enum PackageRef {
    /// OCI reference. Stores the post-scheme-strip body verbatim so the
    /// caller can apply *its own* default registry at resolution time —
    /// `oci::Identifier::from_str` injects the OCI default registry, which
    /// would shadow per-CLI overrides like `OCX_DEFAULT_REGISTRY`.
    Oci(String),
    /// Absolute filesystem path to an already-installed package root
    /// (the directory containing `metadata.json`, `content/`, `entrypoints/`).
    PackageRoot(PathBuf),
}

impl PackageRef {
    /// Parses the OCI variant against the caller-supplied default registry,
    /// producing the resolved [`oci::Identifier`].
    ///
    /// Returns `None` when this ref is a [`PackageRef::PackageRoot`]; the
    /// caller is expected to pattern-match before calling.
    pub fn into_identifier(self, default_registry: &str) -> Result<Option<oci::Identifier>, oci::IdentifierError> {
        match self {
            Self::Oci(raw) => Ok(Some(oci::Identifier::parse_with_default_registry(
                &raw,
                default_registry,
            )?)),
            Self::PackageRoot(_) => Ok(None),
        }
    }
}

impl FromStr for PackageRef {
    type Err = PackageRefParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("file://") {
            if rest.is_empty() {
                return Err(PackageRefParseError::EmptyFileUri);
            }
            let path = PathBuf::from(rest);
            if !path.is_absolute() {
                return Err(PackageRefParseError::RelativeFileUri(rest.to_string()));
            }
            return Ok(PackageRef::PackageRoot(path));
        }

        // Strip an optional `oci://` prefix, validate the body parses as an
        // OCI identifier (against the *built-in* default registry — the real
        // default is applied later by the consumer), then store the body
        // unchanged so the consumer can re-parse with its own default.
        let body = s.strip_prefix("oci://").unwrap_or(s);
        oci::Identifier::from_str(body).map_err(|source| PackageRefParseError::InvalidIdentifier {
            raw: body.to_string(),
            source,
        })?;
        Ok(PackageRef::Oci(body.to_string()))
    }
}

impl std::fmt::Display for PackageRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageRef::Oci(raw) => write!(f, "{raw}"),
            PackageRef::PackageRoot(path) => write!(f, "file://{}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_identifier_is_oci() {
        let r: PackageRef = "node:20".parse().unwrap();
        match r {
            PackageRef::Oci(raw) => assert_eq!(raw, "node:20"),
            other => panic!("expected Oci, got {other:?}"),
        }
    }

    #[test]
    fn parse_oci_scheme_strips_prefix() {
        let r: PackageRef = "oci://node:20".parse().unwrap();
        match r {
            PackageRef::Oci(raw) => assert_eq!(raw, "node:20"),
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
    fn into_identifier_applies_default_registry() {
        let r: PackageRef = "node:20".parse().unwrap();
        let id = r.into_identifier("example.com").unwrap().expect("expected Some");
        assert_eq!(id.registry(), "example.com");
        assert_eq!(id.repository(), "node");
    }

    #[test]
    fn into_identifier_returns_none_for_package_root() {
        let r: PackageRef = "file:///abs/pkg".parse().unwrap();
        assert!(r.into_identifier("example.com").unwrap().is_none());
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
}
