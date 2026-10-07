// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the errors a command raises about its own input ([`crate::error`]).
//!
//! Those types sit in the `families!` list on purpose: handling them in [`crate::exit::classify_error`]'s
//! CLI-local pass would outrank earlier library causes, changing exit codes with byte-identical stderr.

use ocx_exit::{ClassifyExitCode as _, ExitCode};

use crate::error::MetadataResolutionError;
use crate::error::RetiredEnvError;
use crate::error::UsageError;

#[cfg(test)]
mod tests {

    use super::*;

    // ── moved from crate::error with the impl ──

    /// Reds on: a CLI-input slug filed under another code than its error exits with.
    #[test]
    fn cli_input_details_are_registered_under_their_codes() {
        use crate::exit::tests::assert_detail;

        assert_detail(&UsageError::new("--platform must be of the form os/arch"), "usage");
        assert_detail(&MetadataResolutionError::Required, "metadata_required");
        let candidates = vec!["a/metadata.json".into(), "b/metadata.json".into()];
        assert_detail(&MetadataResolutionError::Ambiguous { candidates }, "metadata_ambiguous");
        let invalid = MetadataResolutionError::InvalidLayerPath {
            layer: "layer.tar.gz".into(),
            reason: "not a directory".to_string(),
        };
        assert_detail(&invalid, "metadata_invalid_layer_path");
        assert_detail(&RetiredEnvError { refused: Vec::new() }, "retired_env");
    }

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
        let exit = crate::exit::classify_library_error(&err as &(dyn std::error::Error + 'static));
        assert_eq!(exit, ExitCode::UsageError);
    }
}
