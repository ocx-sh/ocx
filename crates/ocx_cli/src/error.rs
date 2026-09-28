// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The typed errors a command raises about its own input, before any work.

/// Bad CLI invocation our code (not clap) detects;
/// always [`ExitCode::UsageError`](ocx_exit::ExitCode::UsageError) (`64`).
///
/// The message is sentence-case: only the CLI shows it, as the chain's outer context.
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
    /// Construct a usage error; name the offending flag in the message so stderr can be grepped for it.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Construct a usage error keeping `source` in the chain the exit-code classifier walks.
    pub fn with_source(message: impl Into<String>, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

/// Metadata-path resolution failures for `ocx package push` and `ocx package test`; all exit `64`.
#[derive(Debug)]
pub enum MetadataResolutionError {
    /// No explicit `--metadata` and no file layers to infer a sibling from.
    Required,
    /// File layers point at distinct candidate metadata paths.
    Ambiguous { candidates: Vec<std::path::PathBuf> },
    /// A file layer's path could not yield a metadata candidate.
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
