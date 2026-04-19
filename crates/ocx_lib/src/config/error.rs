// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

/// Errors that can occur during configuration parsing and validation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An invalid boolean string was provided in configuration.
    #[error("invalid boolean string '{value}', possible values are: {possible}")]
    InvalidBooleanString { value: String, possible: String },

    /// A TOML configuration file could not be parsed.
    #[error("invalid TOML at {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// An explicit config file (`--config` or `OCX_CONFIG_FILE`) was
    /// specified but does not exist.
    #[error("config file not found: {} (check --config or OCX_CONFIG_FILE)", path.display())]
    FileNotFound { path: PathBuf },

    /// I/O failure while reading a config file (permission denied,
    /// unreadable file, config path is a directory, etc.).
    #[error("failed to read config file {}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A config file exceeds the maximum allowed size (safety cap; config
    /// files are expected to be well under 1 KiB).
    #[error(
        "config file {} exceeds maximum allowed size ({size} bytes > {limit} bytes); OCX config files are typically under 1 KiB — did you point at the wrong file",
        path.display()
    )]
    FileTooLarge { path: PathBuf, size: u64, limit: u64 },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::FileNotFound { .. } => ExitCode::NotFound,
            Self::FileTooLarge { .. } | Self::Parse { .. } => ExitCode::ConfigError,
            Self::Io { .. } => ExitCode::IoError,
            Self::InvalidBooleanString { .. } => ExitCode::DataError,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render an error as its `Display` followed by each `source()` link,
    /// joined by `": "`. Mirrors `anyhow::Error`'s `{:#}` alternate format so
    /// we can verify chain-walk output without pulling anyhow into ocx_lib.
    fn render_chain(err: &(dyn std::error::Error + 'static)) -> String {
        let mut rendered = err.to_string();
        let mut cause = err.source();
        while let Some(next) = cause {
            rendered.push_str(": ");
            rendered.push_str(&next.to_string());
            cause = next.source();
        }
        rendered
    }

    #[test]
    fn parse_display_shows_source_only_once_in_chain() {
        // Regression test: before this fix, the `Parse` variant interpolated
        // `{source}` in its `#[error("...")]` string AND declared `#[source]`
        // on the same field. Any chain walker that appends `source()` to the
        // outer Display (e.g. `anyhow::Error`'s `{:#}`) would print the
        // underlying `toml::de::Error` twice.
        let toml_err = toml::from_str::<toml::Value>("not valid [[").unwrap_err();
        let marker = toml_err.to_string();
        let err = Error::Parse {
            path: PathBuf::from("/bad.toml"),
            source: toml_err,
        };
        let rendered = render_chain(&err as &(dyn std::error::Error + 'static));
        let count = rendered.matches(marker.as_str()).count();
        assert_eq!(
            count, 1,
            "TOML source should appear exactly once, got {count}: {rendered}"
        );
    }
}
