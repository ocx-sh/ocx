// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the errors a command raises about its own
//! input ([`crate::error`]).
//!
//! On the library ladder deliberately: moving them into [`crate::exit::classify_error`]'s CLI-local pass
//! would outrank earlier library causes, changing exit codes with byte-identical stderr.

use ocx_exit::ExitCode;

use crate::error::MetadataResolutionError;
use crate::error::UsageError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for UsageError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::UsageError)
    }
}

impl ClassifyExitCode for MetadataResolutionError {
    fn classify(&self) -> Option<ExitCode> {
        Some(ExitCode::UsageError)
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, UsageError);
    downcast_arm!(cause, MetadataResolutionError);
    None
}

#[cfg(test)]
mod tests {

    use super::*;

    // ── moved from crate::error with the impl ──

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
