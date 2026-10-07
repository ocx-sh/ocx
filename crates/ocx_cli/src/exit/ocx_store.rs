// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the `ocx_store` family. Its types declare their own codes with `#[derive(Classify)]`.

use ocx_store::file_structure::ToolchainPathError;
use ocx_store::file_structure::error::Error as FileStructureError;

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_exit::{ClassifyExitCode, ExitCode};
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

    /// Reds on: a store slug filed under another code than its variant exits with.
    #[test]
    fn store_details_are_registered_under_their_codes() {
        use crate::exit::tests::assert_detail;

        assert_detail(
            &FileStructureError::MissingDigest("ocx.sh/cmake:3".to_string()),
            "missing_digest",
        );
        let wire = FileStructureError::NonUtf8WireName { path: "index/o".into() };
        assert_detail(&wire, "non_utf8_wire_name");
        let expected = [
            "toolchain_name_empty",
            "toolchain_name_control_character",
            "toolchain_name_separator",
            "toolchain_name_path_prefix",
            "toolchain_name_trailing_dot_or_space",
            "toolchain_name_relative",
        ];
        let samples = one_of_every_variant();
        assert_eq!(samples.len(), expected.len(), "one slug per sampled variant");
        for (error, slug) in samples.iter().zip(expected) {
            assert_detail(error, slug);
        }
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
