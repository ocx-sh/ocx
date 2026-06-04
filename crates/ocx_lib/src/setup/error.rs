// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error type for the `ocx self setup` subsystem.
//!
//! A dirty RC block refused without `--force` is **not** an error — it is a
//! non-error [`crate::setup::ProfileOutcome::SkippedDirty`] outcome, and the
//! CLI decides exit code 82 by inspecting the outcomes, not by matching an
//! error variant.

use std::path::PathBuf;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::package_manager;

/// Error raised while creating or refreshing shell integration.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Self-install bootstrap failed; the CAS could not be populated.
    #[error("bootstrap failed")]
    Bootstrap(#[from] package_manager::error::Error),
    /// A shim or profile file could not be read or written.
    #[error("I/O error for {path}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A profile-detection subprocess (PowerShell `$PROFILE` / exec-policy probe) failed.
    #[error("profile subprocess failed")]
    Subprocess(#[source] std::io::Error),
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Delegate to the inner package-manager error so the existing
            // ladder decides (offline → 81, registry → 69, …). Returning
            // `None` lets the chain walker reach the inner cause via `source()`.
            Error::Bootstrap(_) => None,
            Error::Io { .. } => Some(ExitCode::IoError),
            Error::Subprocess(_) => Some(ExitCode::Unavailable),
        }
    }
}
