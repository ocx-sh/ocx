// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the `ocx_store` error family.

use ocx_exit::ExitCode;

use ocx_store::file_structure::ToolchainPathError;
use ocx_store::file_structure::error::Error as FileStructureError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for FileStructureError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::MissingDigest(_) => ExitCode::DataError,
            Self::DigestMismatch { .. } => ExitCode::DataError,
            Self::MalformedRootDocument { .. } => ExitCode::DataError,
            Self::RepositoryEscapesIndexHome { .. } => ExitCode::DataError,
            Self::NonUtf8WireName { .. } => ExitCode::DataError,
        })
    }
}

impl ClassifyExitCode for ToolchainPathError {
    /// Exhaustive, not a blanket `Some`, so a new variant cannot ship unclassified.
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::Empty { .. }
            | Self::ControlCharacter { .. }
            | Self::Separator { .. }
            | Self::PathPrefix { .. }
            | Self::TrailingDotOrSpace { .. }
            | Self::Relative { .. } => Some(ExitCode::ConfigError),
        }
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, FileStructureError);
    downcast_arm!(cause, ToolchainPathError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_exit::ExitCode;
    use ocx_store::file_structure::ToolchainPathComponent;
    use std::collections::BTreeSet;

    // ── moved from ocx_lib::file_structure::toolchain_store with the impl ──

    // recovered from ocx_lib::file_structure::toolchain_store
    /// The variant's name.
    ///
    /// **Not the tripwire, and the distinction matters.** The compiler forces
    /// a new variant into this match and into
    /// [`ToolchainPathError::classify`] — and no further. Nothing forces it
    /// into `one_of_every_variant` or into the expected-name set below, so a
    /// variant added and listed only where the compiler demands leaves the
    /// count comparison green at 6 == 6 — the number of variants that survive
    /// C-073 — with the new variant never classified by any test.
    ///
    /// The real tripwire is `classify`'s wildcard-free match, which stops
    /// compiling until someone chooses the new variant's exit code. This
    /// module's sample list is the part a reviewer has to grow by hand.
    fn variant_name(error: &ToolchainPathError) -> &'static str {
        match error {
            ToolchainPathError::Empty { .. } => "Empty",
            ToolchainPathError::ControlCharacter { .. } => "ControlCharacter",
            ToolchainPathError::Separator { .. } => "Separator",
            ToolchainPathError::PathPrefix { .. } => "PathPrefix",
            ToolchainPathError::TrailingDotOrSpace { .. } => "TrailingDotOrSpace",
            ToolchainPathError::Relative { .. } => "Relative",
        }
    }

    // recovered from ocx_lib::file_structure::toolchain_store
    fn one_of_every_variant() -> Vec<ToolchainPathError> {
        vec![
            ToolchainPathError::Empty {
                component: ToolchainPathComponent::Group,
            },
            ToolchainPathError::ControlCharacter {
                component: ToolchainPathComponent::Group,
                value: "a\nb".to_string(),
            },
            ToolchainPathError::Separator {
                component: ToolchainPathComponent::Group,
                value: "a/b".to_string(),
            },
            ToolchainPathError::PathPrefix {
                component: ToolchainPathComponent::Group,
                value: "C:".to_string(),
            },
            ToolchainPathError::TrailingDotOrSpace {
                component: ToolchainPathComponent::Group,
                value: "bin.".to_string(),
            },
            ToolchainPathError::Relative {
                component: ToolchainPathComponent::Entry,
                value: "..".to_string(),
            },
        ]
    }

    // recovered from ocx_lib::file_structure::toolchain_store
    /// C-013/C-014's exit code, asserted for **every** variant rather than one
    /// sample: a group or tool name arriving from `ocx.toml`, `ocx.lock` or `-g`
    /// is bad configuration data, so every refusal here is exit 78.
    #[test]
    fn every_toolchain_path_error_variant_classifies_as_config_error() {
        let samples = one_of_every_variant();
        let covered: BTreeSet<&'static str> = samples.iter().map(variant_name).collect();
        let expected: BTreeSet<&'static str> = [
            "Empty",
            "ControlCharacter",
            "Separator",
            "PathPrefix",
            "TrailingDotOrSpace",
            "Relative",
        ]
        .into_iter()
        .collect();
        assert_eq!(
            covered, expected,
            "the sample set must name every variant, or this test covers less than it claims"
        );

        for error in &samples {
            assert_eq!(
                error.classify(),
                Some(ExitCode::ConfigError),
                "{} must classify as exit 78 (ConfigError)",
                variant_name(error)
            );
        }
    }
}
