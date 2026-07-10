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
use crate::oci;
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
    ///
    // reserved: no current caller — the PowerShell probes in `profiles.rs`
    // degrade subprocess failure to `None` / `false` by contract (PowerShell
    // absence is non-fatal), so this variant is never constructed today. Kept
    // because plan contract 6 declares it with a `Subprocess → 69` classify
    // mapping for a future probe site that surfaces the failure as a typed error.
    #[error("profile subprocess failed")]
    Subprocess(#[source] std::io::Error),
    /// The VERSION argument could not be parsed as a valid version spec.
    ///
    /// Surfaced via the clap `value_parser` so clap renders it as a usage error
    /// (exit 64). `reason` describes which part of the syntax was invalid.
    #[error("invalid version spec {input:?}: {reason}")]
    InvalidVersionSpec {
        /// The raw input string that was rejected.
        input: String,
        /// Human-readable description of why the input was rejected.
        reason: String,
    },
    /// A `tag@digest` pin was specified but the tag resolved to a different
    /// digest than the one pinned (fail-closed immutability assertion, plan D9).
    ///
    /// The error message names both digests so the operator can diagnose
    /// whether the index is stale (see `hint`).
    #[error(
        "pin digest mismatch for {tag}: expected {expected} but registry resolved {resolved}{hint}",
        hint = if let Some(h) = hint { format!("; {h}") } else { String::new() }
    )]
    PinDigestMismatch {
        /// The tag component of the `tag@digest` spec.
        tag: String,
        /// The digest that was pinned in the VERSION argument.
        expected: oci::Digest,
        /// The digest the registry (or local index) resolved for the tag.
        resolved: oci::Digest,
        /// Optional hint shown when resolution was against the local index and
        /// the mismatch may be caused by a stale index
        /// (e.g. `"run \`ocx index update\` to refresh the local index"`).
        hint: Option<String>,
    },
    /// The `--managed-config` ref could not be re-parsed as a valid OCI
    /// identifier at write time (defensive re-validation, CWE-74 guard —
    /// the fence body is real TOML serialization, never `format!`
    /// interpolation of the raw ref, but the ref itself must still be a
    /// well-formed identifier before it is adopted at all).
    #[error("managed config source '{value}' is not a valid OCI identifier")]
    InvalidManagedConfigSource {
        /// The rejected `--managed-config` value.
        value: String,
        /// The underlying identifier parse failure.
        #[source]
        source: oci::identifier::error::IdentifierError,
    },
    /// The synchronous fetch+persist step during `--managed-config` adoption
    /// failed. Per ADR "Setup ordering", no fence is written on failure — the
    /// caller sees zero partial state.
    #[error("failed to sync the managed-config snapshot")]
    ManagedConfigUpdateFailed(#[from] crate::managed_config::ManagedConfigUpdateError),
    /// A system-locked managed tier refused an explicit override that would
    /// clear or redirect it (locks only tighten — exit 78). The CLI seam
    /// (`resolve_managed_config_arg`) rejects this before calling in, but
    /// [`crate::setup::apply_managed_config`] re-checks so the public library
    /// function cannot be bypassed by a direct caller.
    #[error(transparent)]
    ManagedConfigLocked(#[from] crate::config::managed::ManagedConfigError),
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Delegate to the inner package-manager error so the existing
            // ladder decides (offline → 81, registry → 69, …). Returning
            // `None` lets the chain walker reach the inner cause via `source()`.
            // `Bootstrap` wraps the inner package_manager error via `#[from]`
            // (see the variant above), so `source()` exposes it for the walk —
            // do not "fix" this by returning a specific code here.
            Error::Bootstrap(_) => None,
            Error::Io { .. } => Some(ExitCode::IoError),
            Error::Subprocess(_) => Some(ExitCode::Unavailable),
            // Parsed via clap value_parser → rendered as usage error (exit 64).
            Error::InvalidVersionSpec { .. } => Some(ExitCode::UsageError),
            // tag@digest mismatch is a found-but-inconsistent error (exit 65),
            // not a "not found" (79). Fail-closed per plan D9.
            Error::PinDigestMismatch { .. } => Some(ExitCode::DataError),
            Error::InvalidManagedConfigSource { .. } => Some(ExitCode::ConfigError),
            // Delegate to the inner error so the existing ManagedConfigUpdateError
            // ladder decides (Unavailable/AuthError/DataError); `#[from]` exposes
            // it via `source()` for the chain walker, mirroring `Error::Bootstrap`.
            Error::ManagedConfigUpdateFailed(_) => None,
            // A locked-tier override rejection is a configuration policy error.
            Error::ManagedConfigLocked(_) => Some(ExitCode::ConfigError),
        }
    }
}
