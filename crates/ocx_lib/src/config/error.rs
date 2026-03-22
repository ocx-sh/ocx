// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors that can occur during configuration parsing and validation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An invalid boolean string was provided in configuration.
    #[error("Invalid boolean string '{value}', possible values are: {possible}")]
    InvalidBooleanString { value: String, possible: String },

    /// A TOML configuration file could not be parsed.
    #[error("Configuration parse error: {0}")]
    Parse(#[source] toml::de::Error),
}
