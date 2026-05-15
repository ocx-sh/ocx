// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error type for the project registry subsystem.
//!
//! Mirrors the structure of [`crate::project::error`]: outer [`Error`] enum
//! wrapping a context-bearing [`ProjectRegistryError`] struct, which wraps a
//! [`ProjectRegistryErrorKind`] discriminant. All registry failures flow through
//! this single chain so callers can match on kind without downcasting.
//!
//! See `adr_project_gc_symlink_ledger.md` for failure-mode rationale. The flat
//! symlink store has no JSON document, no schema version, and no advisory-lock
//! sentinel, so the only failure class is filesystem I/O (`Io` variant).

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
/// The `path` field carries the `projects/` store directory, a `projects/`
/// entry, or a staging temp link, depending on which operation failed.
#[derive(Debug)]
pub struct ProjectRegistryError {
    /// Path associated with the failure (the `projects/` store directory, an
    /// entry link, or a staging temp link — depending on which operation failed).
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
/// The flat symlink store (ADR `adr_project_gc_symlink_ledger.md`) has no JSON
/// document, no schema version, and no advisory-lock sentinel — there is
/// nothing to parse or to contend on. The only failure class is filesystem
/// I/O, so this enum carries a single `Io` variant. `#[non_exhaustive]` is
/// retained so a future variant is not a semver break.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectRegistryErrorKind {
    /// Filesystem I/O failure (store-directory creation, symlink create/
    /// rename, readdir, stat).
    #[error("I/O error: {0}")]
    Io(#[source] std::io::Error),
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Registry(e) => match &e.kind {
                ProjectRegistryErrorKind::Io(_) => ExitCode::IoError,
            },
        })
    }
}
