// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Controls whether progress indicators (spinners, bars) are displayed.
///
/// Uses per-stream TTY detection: progress is shown in interactive terminals
/// and suppressed when the stream is piped or redirected.
#[derive(Clone, Copy, Debug)]
pub struct ProgressMode {
    pub stderr: bool,
}

impl ProgressMode {
    /// Detect whether progress indicators should be shown based on TTY state.
    pub fn detect() -> Self {
        Self {
            stderr: console::Term::stderr().is_term(),
        }
    }
}
