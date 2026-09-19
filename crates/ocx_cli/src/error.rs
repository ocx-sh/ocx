// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The typed errors a command raises about its own input.
//!
//! These are CLI-input failures, not library failures: they are raised before
//! any work is attempted, by the process that owns the argument grammar. That
//! is why they live here rather than in `ocx_console`, which renders and knows
//! nothing about what a flag means.
//!
//! Their exit-code classification is nonetheless in the *library* pass of
//! [`crate::exit::classify_error`], not in its CLI-local first pass — see
//! `exit::cli_input`. The two passes are a precedence contract, and moving
//! these types across it would change the code of any chain that carries both
//! kinds.
//!
//! Currently exposes a single variant family — [`UsageError`] — which maps
//! to [`ExitCode::UsageError`](ocx_exit::ExitCode::UsageError) (`64`,
//! `EX_USAGE`). Use it whenever a CLI
//! command rejects its own input (bad flag value, mutually exclusive flags
//! we want to validate ourselves rather than rely on clap's exit code, path
//! containment violations, etc.).

/// Bad CLI invocation that our code (not clap) detects.
///
/// Carries a single sentence-case message intended to print directly to the
/// user as the outer context of the anyhow chain. Library-style lowercase
/// rules don't apply: `UsageError` is consumed only by the CLI binary and
/// its `Display` shows up at the terminal boundary alongside any inner
/// cause.
///
/// Use [`UsageError::with_source`] when the rejection originates from a
/// structured library error — this preserves the full `source()` chain so
/// diagnostics tools and the exit-code classifier can walk the inner cause.
/// (Identifier-parse errors and config-validation errors, for example, are
/// wrapped this way.)
///
/// Always classifies to [`ExitCode::UsageError`](ocx_exit::ExitCode::UsageError)
/// (`64`, mirrors `EX_USAGE`).
#[derive(Debug)]
pub struct UsageError {
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for UsageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|e| e as _)
    }
}

impl UsageError {
    /// Construct a usage error with the given message.
    ///
    /// Convention: name the offending flag or option (e.g. `"--platform"`,
    /// `"--self"`) inside the message so users can `grep` stderr for the
    /// failing option.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Construct a usage error that wraps an inner cause.
    ///
    /// The wrapped error is surfaced via [`std::error::Error::source`] so that
    /// chain-walking diagnostics and the exit-code classifier can inspect the
    /// underlying error. Use this form whenever the rejection originates from a
    /// structured library error rather than a pure formatting problem.
    pub fn with_source(message: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

/// Failure modes of metadata-path resolution for `ocx package push` and
/// `ocx package test`.
///
/// All variants classify to [`ExitCode::UsageError`](ocx_exit::ExitCode::UsageError)
/// (`64`): they signal
/// CLI-input problems the user must correct before any I/O can succeed.
#[derive(Debug)]
pub enum MetadataResolutionError {
    /// No explicit `--metadata` and no file layers to infer a sibling from.
    Required,
    /// File layers point at distinct candidate metadata paths; the caller
    /// must disambiguate via explicit `--metadata`.
    Ambiguous { candidates: Vec<std::path::PathBuf> },
    /// A file layer's path could not yield a metadata candidate (no parent,
    /// no file stem, etc.).
    InvalidLayerPath { layer: std::path::PathBuf, reason: String },
}

impl std::fmt::Display for MetadataResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Required => f.write_str("--metadata is required when no file layers are provided"),
            Self::Ambiguous { candidates } => {
                let list = candidates
                    .iter()
                    .map(|p| format!("'{}'", p.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(
                    f,
                    "file layers point at distinct metadata candidates ({list}); pass --metadata explicitly",
                )
            }
            Self::InvalidLayerPath { layer, reason } => {
                write!(
                    f,
                    "cannot infer metadata path from layer '{}': {reason}",
                    layer.display()
                )
            }
        }
    }
}

impl std::error::Error for MetadataResolutionError {}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn display_returns_message_verbatim() {
        let err = UsageError::new("--platform must be of the form os/arch, got 'rel'");
        assert_eq!(format!("{err}"), "--platform must be of the form os/arch, got 'rel'");
    }

    #[test]
    fn with_source_surfaces_inner_error_via_source_chain() {
        // Lock in: UsageError::with_source wraps an inner cause that is
        // reachable via std::error::Error::source() — chain walkers and
        // diagnostics see both the outer message and the inner error.
        #[derive(Debug)]
        struct Inner;
        impl std::fmt::Display for Inner {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "inner error detail")
            }
        }
        impl std::error::Error for Inner {}

        let err = UsageError::with_source("invalid package ref", Inner);
        // Display shows outer message only.
        assert_eq!(format!("{err}"), "invalid package ref");
        // source() returns Some and points to the inner error.
        let src = err.source().expect("source must be Some for with_source");
        assert_eq!(format!("{src}"), "inner error detail");
    }

    #[test]
    fn new_has_no_source() {
        // UsageError::new must have source() == None (message-only variant).
        let err = UsageError::new("plain usage error");
        assert!(err.source().is_none());
    }
}
