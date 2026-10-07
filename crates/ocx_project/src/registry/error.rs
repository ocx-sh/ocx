// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error type for the project registry.

use std::path::PathBuf;

/// Error returned from [`super::ProjectRegistry`] methods.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "ProjectRegistryError")]
pub enum Error {
    /// A registry operation failed; see [`ProjectRegistryError`] for context.
    #[error("{0}")]
    #[exit(delegate = 0.kind)]
    Registry(#[from] ProjectRegistryError),
}

/// A registry failure and the path it occurred on.
#[derive(Debug)]
pub struct ProjectRegistryError {
    /// The `projects/` store directory, an entry link, or a staging temp link.
    pub path: PathBuf,
    pub kind: ProjectRegistryErrorKind,
}

impl ProjectRegistryError {
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

/// Registry failure kind; the symlink store's only failure class is I/O.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "ProjectRegistryError")]
pub enum ProjectRegistryErrorKind {
    /// Filesystem I/O failure.
    #[error("I/O error: {0}")]
    #[exit(
        IoError,
        slug = "project_registry_io",
        summary = "Reading or writing the project registry failed"
    )]
    Io(#[source] std::io::Error),
}
