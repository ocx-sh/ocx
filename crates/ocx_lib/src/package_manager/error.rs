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
#[derive(Debug)]
pub enum Error {
    /// A find operation failed for one or more packages.
    FindFailed(Vec<PackageError>),
    /// An install operation failed for one or more packages.
    InstallFailed(Vec<PackageError>),
    /// An uninstall operation failed for one or more packages.
    UninstallFailed(Vec<PackageError>),
    /// A select operation failed for one or more packages.
    SelectFailed(Vec<PackageError>),
    /// A deselect operation failed for one or more packages.
    DeselectFailed(Vec<PackageError>),
}

/// An error tied to a specific package.
#[derive(Debug)]
pub struct PackageError {
    pub identifier: oci::Identifier,
    pub kind: PackageErrorKind,
}

impl PackageError {
    pub fn new(identifier: oci::Identifier, kind: PackageErrorKind) -> Self {
        Self { identifier, kind }
    }
}

/// The cause of a single-package failure.
#[derive(Debug)]
pub enum PackageErrorKind {
    /// The package was not found in the index or object store.
    NotFound,
    /// Multiple candidates matched the platform selection.
    SelectionAmbiguous(Vec<oci::Identifier>),
    /// A symlink-based path was requested but the identifier carries a digest.
    SymlinkRequiresTag,
    /// The requested install symlink does not exist.
    SymlinkNotFound(file_structure::SymlinkKind),
    /// An underlying internal error (I/O, OCI, network, etc.).
    Internal(crate::Error),
}

// ---------------------------------------------------------------------------
// Display
// ---------------------------------------------------------------------------

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::FindFailed(errors) => write_batch(f, "find", errors),
            Error::InstallFailed(errors) => write_batch(f, "install", errors),
            Error::UninstallFailed(errors) => write_batch(f, "uninstall", errors),
            Error::SelectFailed(errors) => write_batch(f, "select", errors),
            Error::DeselectFailed(errors) => write_batch(f, "deselect", errors),
        }
    }
}

fn write_batch(f: &mut std::fmt::Formatter<'_>, verb: &str, errors: &[PackageError]) -> std::fmt::Result {
    if errors.len() == 1 {
        write!(f, "Failed to {verb} package: {}", errors[0])
    } else {
        writeln!(f, "Failed to {verb} {} packages:", errors.len())?;
        for e in errors {
            writeln!(f, "  {e}")?;
        }
        Ok(())
    }
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} — {}", self.identifier, self.kind)
    }
}

impl std::fmt::Display for PackageErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackageErrorKind::NotFound => write!(f, "package not found"),
            PackageErrorKind::SelectionAmbiguous(candidates) => write!(
                f,
                "ambiguous selection: {}",
                candidates
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            PackageErrorKind::SymlinkRequiresTag => {
                write!(f, "symlink resolution requires a tag, not a digest")
            }
            PackageErrorKind::SymlinkNotFound(kind) => match kind {
                file_structure::SymlinkKind::Candidate => {
                    write!(f, "no installed candidate")
                }
                file_structure::SymlinkKind::Current => {
                    write!(f, "no selected version")
                }
            },
            PackageErrorKind::Internal(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}
impl std::error::Error for PackageError {}
impl std::error::Error for PackageErrorKind {}
