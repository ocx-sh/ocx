// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether progress indicators (spinners, bars) are shown: only when stderr is a terminal.
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
