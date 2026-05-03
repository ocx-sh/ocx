// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error type for the project registry subsystem.
//!
//! Mirrors the structure of [`crate::project::error`]: outer [`Error`] enum
//! wrapping a context-bearing [`ProjectRegistryError`] struct, which wraps a
//! [`ProjectRegistryErrorKind`] discriminant. All registry failures flow through
//! this single chain so callers can match on kind without downcasting.
//!
//! See [`adr_clean_project_backlinks.md`] for failure-mode rationale.

use std::path::PathBuf;

use crate::cli::{ClassifyExitCode, ExitCode};

/// Top-level error type returned from [`super::ProjectRegistry`] methods.
///
/// Wraps a [`ProjectRegistryError`] that carries path context. Separate from
/// the project-tier [`crate::project::Error`] so callers can match registry
/// errors independently from lock/config errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A registry operation failed; see [`ProjectRegistryError`] for context.
    #[error("{0}")]
    Registry(#[from] ProjectRegistryError),
}

/// Context-bearing registry error: which path the failure occurred on.
///
/// The `path` field carries the registry file path (`projects.json`) or the
/// sentinel path (`.projects.lock`) depending on which operation failed.
#[derive(Debug)]
pub struct ProjectRegistryError {
    /// Path associated with the failure (registry file or sentinel).
    pub path: PathBuf,
    /// Discriminant identifying the failure category.
    pub kind: ProjectRegistryErrorKind,
}

impl ProjectRegistryError {
    /// Constructs a [`ProjectRegistryError`] attaching `path` context to `kind`.
    pub fn new(path: impl Into<PathBuf>, kind: ProjectRegistryErrorKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }
}

impl std::fmt::Display for ProjectRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.as_os_str().is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.path.display(), self.kind)
        }
    }
}

impl std::error::Error for ProjectRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

/// Inner error discriminant for registry failures.
///
/// Four variants cover all failure modes described in the ADR "Failure Modes"
/// table: I/O (filesystem), Corrupt (parse failure refusing blind overwrite),
/// UnknownVersion (unrecognised schema_version field), and Locked (advisory
/// lock contention past retry timeout).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectRegistryErrorKind {
    /// Filesystem I/O failure (directory creation, sentinel open, rename, read).
    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),

    /// Registry file exists but failed to parse as valid JSON.
    ///
    /// Per the ADR, a corrupt registry must surface as an error rather than
    /// being silently overwritten, to avoid masking data loss and to force
    /// the user to inspect and repair the file.
    #[error("registry file is corrupt")]
    Corrupt(#[source] serde_json::Error),

    /// Registry file has a `schema_version` value that this version of OCX
    /// does not recognise. Treated as a hard error to avoid silently
    /// overwriting data written by a newer version of OCX.
    #[error("unrecognised schema version {found}; expected {expected}")]
    UnknownVersion { found: u8, expected: u8 },

    /// Advisory lock on the sentinel file could not be acquired within the
    /// retry timeout. The registration window is lost; the next successful
    /// invocation re-registers.
    #[error("registry advisory lock is held by another process")]
    Locked,
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Registry(e) => match &e.kind {
                ProjectRegistryErrorKind::Io(_) => ExitCode::IoError,
                ProjectRegistryErrorKind::Corrupt(_) => ExitCode::ConfigError,
                ProjectRegistryErrorKind::UnknownVersion { .. } => ExitCode::ConfigError,
                ProjectRegistryErrorKind::Locked => ExitCode::TempFail,
            },
        })
    }
}
