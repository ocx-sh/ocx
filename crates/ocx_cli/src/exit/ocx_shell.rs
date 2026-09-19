// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the CI-detection error family — the `ocx_shell` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_shell::ci::error::Error as CiError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for CiError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::MissingEnv(_) => ExitCode::ConfigError,
            Self::File { .. } | Self::Write(_) => ExitCode::IoError,
        })
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, CiError);
    None
}
