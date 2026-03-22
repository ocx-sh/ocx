// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors that can occur when parsing or validating OCI platforms.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The platform string has an invalid format.
    #[error("Invalid platform: {0}")]
    Invalid(String),

    /// The OS component is not a recognized value.
    #[error("Invalid platform OS '{os}'. Possible values are: {valid}")]
    InvalidOs { os: String, valid: String },

    /// The architecture component is not a recognized value.
    #[error("Invalid platform architecture '{arch}'. Possible values are: {valid}")]
    InvalidArch { arch: String, valid: String },

    /// The platform is syntactically valid but not supported.
    #[error("Unsupported OCI platform: {0}")]
    Unsupported(String),
}
