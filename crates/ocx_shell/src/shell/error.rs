// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors specific to shell operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A shell is not supported for clap completions.
    #[error("Shell '{}' is not supported for clap completions", .0)]
    UnsupportedClapShell(super::Shell),
}
