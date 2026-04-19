// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::ExitCode;

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
    /// Map a [`MirrorError`] variant to its [`ExitCode`].
    ///
    /// `ExecutionFailed` is intentionally fixed to `Failure (1)` because the
    /// current variant carries `Vec<String>` (stringified error messages),
    /// not a structured inner error to delegate to. Refactoring the variant
    /// to carry `anyhow::Error` is tracked as a follow-up so per-cause exit
    /// codes can be surfaced through the mirror pipeline.
    pub fn kind_exit_code(&self) -> ExitCode {
        match self {
            Self::SpecInvalid(_) => ExitCode::DataError,
            Self::SpecNotFound(_) => ExitCode::NotFound,
            Self::ExecutionFailed(_) => ExitCode::Failure,
            Self::SourceError(_) => ExitCode::Unavailable,
        }
    }
}

impl std::fmt::Display for MirrorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpecInvalid(errors) => {
                writeln!(f, "invalid mirror spec:")?;
                for error in errors {
                    writeln!(f, "  - {error}")?;
                }
                Ok(())
            }
            Self::SpecNotFound(path) => write!(f, "mirror spec not found: {path}"),
            Self::ExecutionFailed(errors) => {
                writeln!(f, "mirror execution failed:")?;
                for error in errors {
                    writeln!(f, "  - {error}")?;
                }
                Ok(())
            }
            Self::SourceError(msg) => write!(f, "source error: {msg}"),
        }
    }
}

impl std::error::Error for MirrorError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_invalid_maps_to_data_error() {
        // Plan taxonomy: SpecInvalid → DataError (65) — spec content is malformed input.
        let err = MirrorError::SpecInvalid(vec!["invalid field 'foo'".into()]);
        assert_eq!(err.kind_exit_code(), ExitCode::DataError);
    }

    #[test]
    fn spec_not_found_maps_to_not_found() {
        // Plan taxonomy: SpecNotFound → NotFound (79) — spec file absent from disk.
        let err = MirrorError::SpecNotFound("mirror-cmake.yml".into());
        assert_eq!(err.kind_exit_code(), ExitCode::NotFound);
    }

    #[test]
    fn execution_failed_maps_to_failure() {
        // Plan taxonomy: ExecutionFailed → Failure (1).
        // Divergence from per-cause classification: the variant carries Vec<String>
        // (stringified error messages), not a structured inner error to delegate to.
        // Refactoring the variant to carry structured errors is a follow-up.
        let err = MirrorError::ExecutionFailed(vec!["download failed for cmake 3.28".into()]);
        assert_eq!(err.kind_exit_code(), ExitCode::Failure);
    }

    #[test]
    fn source_error_maps_to_unavailable() {
        // Plan taxonomy: SourceError → Unavailable (69) — upstream source unreachable.
        let err = MirrorError::SourceError("GitHub API returned 503".into());
        assert_eq!(err.kind_exit_code(), ExitCode::Unavailable);
    }
}
