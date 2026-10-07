// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Which config tier produced an error; picks the remediation hint (`--config` vs `--project`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// Config-tier source: `--config`, `OCX_CONFIG`, or a discovered
    /// tier-config path (system / user / `$OCX_HOME`).
    Config,
    /// Project-tier source: `--project` or `OCX_PROJECT`.
    Project,
}

impl ConfigSource {
    fn label(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Project => "project",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Config => "check --config or OCX_CONFIG",
            Self::Project => "check --project or OCX_PROJECT",
        }
    }
}

/// Errors that can occur during configuration parsing and validation.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "ConfigError")]
pub enum Error {
    /// A TOML configuration file could not be parsed.
    #[error("invalid TOML at {}", path.display())]
    #[exit(ConfigError, slug = "config_parse", summary = "A config file does not parse")]
    Parse {
        path: PathBuf,
        // Boxed: inline, `toml::de::Error` pushes this enum past clippy's `result_large_err` limit on Windows.
        #[source]
        source: Box<toml::de::Error>,
    },

    /// An explicit config file (`--config`/`OCX_CONFIG` or `--project`/`OCX_PROJECT`) does not exist.
    #[error("{} file not found: {} ({})", tier.label(), path.display(), tier.hint())]
    #[exit(
        NotFound,
        slug = "config_file_not_found",
        summary = "An explicitly named config or project file does not exist"
    )]
    FileNotFound { path: PathBuf, tier: ConfigSource },

    /// I/O failure while reading a config file.
    #[error("failed to read {} file {} ({})", tier.label(), path.display(), tier.hint())]
    #[exit(IoError, slug = "config_io", summary = "Reading a config file failed")]
    Io {
        path: PathBuf,
        tier: ConfigSource,
        #[source]
        source: std::io::Error,
    },

    /// The system config file exists but cannot be read (absence is a silent skip).
    ///
    /// Fatal, unlike the user tiers: skipping it would silently drop every `lock_as_system` policy.
    #[error("cannot read system config file {}; it carries operator policy and is never skipped", path.display())]
    #[exit(
        ConfigError,
        slug = "system_config_invalid",
        summary = "The system-wide config file is unusable"
    )]
    SystemConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A config file exceeds the maximum allowed size.
    #[error(
        "config file {} exceeds maximum allowed size ({size} bytes > {limit} bytes); OCX config files are typically under 1 KiB — did you point at the wrong file",
        path.display()
    )]
    #[exit(
        ConfigError,
        slug = "config_file_too_large",
        summary = "A config file exceeds the allowed size"
    )]
    FileTooLarge { path: PathBuf, size: u64, limit: u64 },

    /// A `toolchain_dir` value was refused.
    ///
    /// `cli::classify` downcasts this enum, so without this variant the refusal exits 1, not 78.
    // Boxed: inline, its two-path variants push this enum past clippy's `result_large_err` limit on Windows.
    #[error(transparent)]
    #[exit(delegate)]
    Toolchain(Box<crate::ToolchainRootError>),

    /// A single config file declares both `extra_ca_certs` and `extra_ca_certs_pem`.
    ///
    /// Checked per file at load time only: the XOR merge hides it afterwards, so no later check can.
    #[error(
        "{} declares both extra_ca_certs and extra_ca_certs_pem: keep one (extra_ca_certs names a local file; \
         extra_ca_certs_pem is the inline text `ocx config push` publishes)",
        path.display()
    )]
    #[exit(
        ConfigError,
        slug = "ambiguous_extra_ca_certs",
        summary = "Both extra_ca_certs and extra_ca_certs_pem are declared"
    )]
    AmbiguousExtraCaCerts { path: PathBuf },
}

/// [`Result`](std::result::Result) over this tier's own [`Error`].
pub(crate) type Result<T> = std::result::Result<T, Error>;

impl From<crate::ToolchainRootError> for Error {
    fn from(error: crate::ToolchainRootError) -> Self {
        Self::Toolchain(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_stays_small_enough_to_return_by_value() {
        // Clippy's `result_large_err` limit is 128 bytes and `PathBuf` is wider on Windows,
        // which only a Windows clippy run would catch; every wrapping tier inherits this size.
        let size = std::mem::size_of::<Error>();
        assert!(size <= 96, "ocx_config::error::Error is {size} bytes");
    }

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
            source: Box::new(toml_err),
        };
        let rendered = render_chain(&err as &(dyn std::error::Error + 'static));
        let count = rendered.matches(marker.as_str()).count();
        assert_eq!(
            count, 1,
            "TOML source should appear exactly once, got {count}: {rendered}"
        );
    }
}
