// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Typed CLI errors shared across OCX binaries.
//!
//! These error types live in [`ocx_lib::cli`] (not `ocx_cli`) because the
//! [`classify_error`](super::classify_error) ladder lives in this crate and
//! must downcast every classifiable type. Putting the typed CLI error here
//! lets both `ocx` and `ocx-mirror` reuse it without duplicating the
//! classification entry.
//!
//! Currently exposes a single variant family — [`UsageError`] — which maps
//! to [`ExitCode::UsageError`] (`64`, `EX_USAGE`). Use it whenever a CLI
//! command rejects its own input (bad flag value, mutually exclusive flags
//! we want to validate ourselves rather than rely on clap's exit code, path
//! containment violations, etc.).

use crate::cli::{ClassifyExitCode, ExitCode};

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
/// Always classifies to [`ExitCode::UsageError`] (`64`, mirrors `EX_USAGE`).
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

impl ClassifyExitCode for UsageError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::UsageError)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn classifies_to_usage_error() {
        let err = UsageError::new("--platform must be of the form os/arch");
        assert_eq!(err.classify(), Some(ExitCode::UsageError));
    }

    #[test]
    fn classify_through_chain_walker() {
        // Lock in: a UsageError surfaced through the std::error::Error chain
        // walker resolves to ExitCode::UsageError, not the default Failure.
        let err = UsageError::new("--config path outside packages root");
        let exit = crate::cli::classify_error(&err as &(dyn std::error::Error + 'static));
        assert_eq!(exit, ExitCode::UsageError);
    }

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
        // Chain walks: the classify_error walker sees UsageError first.
        assert_eq!(
            crate::cli::classify_error(&err as &(dyn std::error::Error + 'static)),
            ExitCode::UsageError,
        );
    }

    #[test]
    fn new_has_no_source() {
        // UsageError::new must have source() == None (message-only variant).
        let err = UsageError::new("plain usage error");
        assert!(err.source().is_none());
    }
}
