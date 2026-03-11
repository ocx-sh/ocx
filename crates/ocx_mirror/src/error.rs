// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

#[derive(Debug)]
pub enum MirrorError {
    /// Spec file has validation errors (YAML parse, schema, regex, etc.)
    SpecInvalid(Vec<String>),
    /// Spec file could not be read from disk.
    SpecNotFound(String),
    /// Runtime error during mirror execution (download, push, verify failures).
    ExecutionFailed(Vec<String>),
    /// Error fetching upstream version information from source (GitHub, URL index).
    SourceError(String),
}

impl MirrorError {
    pub fn exit_code(&self) -> ExitCode {
        match self {
            Self::SpecInvalid(_) | Self::SpecNotFound(_) => ExitCode::from(2),
            Self::ExecutionFailed(_) => ExitCode::from(3),
            Self::SourceError(_) => ExitCode::from(4),
        }
    }
}

impl std::fmt::Display for MirrorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpecInvalid(errors) => {
                writeln!(f, "Invalid mirror spec:")?;
                for error in errors {
                    writeln!(f, "  - {error}")?;
                }
                Ok(())
            }
            Self::SpecNotFound(path) => write!(f, "Mirror spec not found: {path}"),
            Self::ExecutionFailed(errors) => {
                writeln!(f, "Mirror execution failed:")?;
                for error in errors {
                    writeln!(f, "  - {error}")?;
                }
                Ok(())
            }
            Self::SourceError(msg) => write!(f, "Source error: {msg}"),
        }
    }
}

impl std::error::Error for MirrorError {}
