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
/// Always classifies to [`ExitCode::UsageError`] (`64`, mirrors `EX_USAGE`).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UsageError(String);

impl UsageError {
    /// Construct a usage error with the given message.
    ///
    /// Convention: name the offending flag or scheme (e.g. `"file://"`,
    /// `"--platform"`) inside the message so users can `grep` stderr for the
    /// failing option.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl ClassifyExitCode for UsageError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::UsageError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_to_usage_error() {
        let err = UsageError::new("file:// URI must be absolute");
        assert_eq!(err.classify(), Some(ExitCode::UsageError));
    }

    #[test]
    fn classify_through_chain_walker() {
        // Lock in: a UsageError surfaced through the std::error::Error chain
        // walker resolves to ExitCode::UsageError, not the default Failure.
        let err = UsageError::new("file:// path outside packages root");
        let exit = crate::cli::classify_error(&err as &(dyn std::error::Error + 'static));
        assert_eq!(exit, ExitCode::UsageError);
    }

    #[test]
    fn display_returns_message_verbatim() {
        let err = UsageError::new("file:// URI must be absolute, got 'rel'");
        assert_eq!(format!("{err}"), "file:// URI must be absolute, got 'rel'");
    }
}
