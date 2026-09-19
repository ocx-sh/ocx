// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the errors a command raises about its own
//! input ([`crate::error`]).
//!
//! These types are this crate's, not a library's, which makes this the one
//! rung of the ladder that does not stand for an `ocx_*` crate. It is a rung
//! all the same, deliberately: [`crate::exit::classify_error`] sweeps the
//! CLI-local types over the whole chain **before** the library types, and that
//! ordering is observable. Classifying `UsageError` in the first pass would
//! put it ahead of every library cause that precedes it in a chain carrying
//! both — a changed exit code with byte-identical stderr, which is the class
//! DEC-23 exists for. It stays where it has always been answered from.

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
