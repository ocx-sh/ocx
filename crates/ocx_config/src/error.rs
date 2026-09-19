// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Which config tier produced an error — decides the remediation hint shown
/// in [`Error::FileNotFound`] / [`Error::Io`] messages. Without this, a
/// missing `--project` file would misdirect the user to `--config`.
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
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A TOML configuration file could not be parsed.
    #[error("invalid TOML at {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    /// An explicit config file (`--config` / `OCX_CONFIG` for the
    /// config tier, `--project` / `OCX_PROJECT` for the project tier)
    /// was specified but does not exist. The `tier` field steers the
    /// remediation hint so users see the flag they actually typed.
    #[error("{} file not found: {} ({})", tier.label(), path.display(), tier.hint())]
    FileNotFound { path: PathBuf, tier: ConfigSource },

    /// I/O failure while reading a config file (permission denied,
    /// unreadable file, config path is a directory, etc.).
    #[error("failed to read {} file {} ({})", tier.label(), path.display(), tier.hint())]
    Io {
        path: PathBuf,
        tier: ConfigSource,
        #[source]
        source: std::io::Error,
    },

    /// The SYSTEM-scope config file exists but cannot be consulted.
    ///
    /// Fatal, unlike the user and `$OCX_HOME` tiers, where an unreadable
    /// candidate is skipped with a warning: this file carries operator policy —
    /// every `lock_as_system` section lives in it — so skipping it drops the
    /// policy along with the file, silently, on every invocation. An operator
    /// who symlinks it at a config-managed fleet file would otherwise take the
    /// whole fleet out of a locked `[records]` sink without noticing.
    ///
    /// Absence is not this error: no `/etc/ocx/config.toml` is the ordinary
    /// case and stays a silent skip.
    #[error("cannot read system config file {}; it carries operator policy and is never skipped", path.display())]
    SystemConfig {
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

    /// A `toolchain_dir` value was refused (C-017, C-018, C-019, R-W1, R-W2).
    ///
    /// Transparent, because [`ToolchainRootError`](crate::ToolchainRootError)
    /// already names the tier, the path and the failing property — there is
    /// nothing this layer can add that the inner message does not carry.
    ///
    /// **This variant is what makes the refusal observable as exit 78.**
    /// `cli::classify` downcasts this enum, so the inner `classify` is reached
    /// only through here; without this arm a refusal falls through to
    /// `ExitCode::Failure` (1) while every unit test of the inner type stays
    /// green. `?` applies exactly one `From`, so a function returning
    /// `crate::Result` writes `.map_err(Error::from)?` — the hop from
    /// `ToolchainRootError` to `crate::Error` is two conversions, not one.
    #[error(transparent)]
    Toolchain(#[from] crate::ToolchainRootError),

    /// A single config file declares both `extra_ca_certs` and
    /// `extra_ca_certs_pem` (S-005) — same-file ambiguity, distinct from the
    /// cross-tier XOR [`crate::Config::merge`] applies.
    ///
    /// The permanent load-side carrier for this refusal. The loader checks
    /// this at LOAD time, per file, before `Config::merge` folds the parsed
    /// tier into the accumulator; the XOR merge then guarantees the merged
    /// `Config` can never carry both fields at once, so there is no
    /// equivalent resolve-time check to make — unlike `[trust.sigstore]`'s
    /// sibling ambiguity, which is refused at resolve time on the merged
    /// config (`oci/verify/trust_resolve.rs:102-106`).
    #[error(
        "{} declares both extra_ca_certs and extra_ca_certs_pem: keep one (extra_ca_certs names a local file; \
         extra_ca_certs_pem is the inline text `ocx config push` publishes)",
        path.display()
    )]
    AmbiguousExtraCaCerts { path: PathBuf },
}

/// [`Result`](std::result::Result) over this tier's own [`Error`].
///
/// The loader used to return `crate::Result`, i.e. the crate-wide `Error` that
/// E1 deletes. Every failure it could actually raise was already a
/// [`Config`](crate::Error::Config) wrapping one of the variants above, so
/// narrowing the alias moves no exit code: `?` at a caller returning
/// `crate::Result` applies the same `#[from]` this type always went through.
/// `pub(crate)` for the same reason `config::tls` is: `config` is a private
/// module today, so a `pub` alias here is an `unreachable_pub` the lint ratchet
/// counts. The extraction turns `config` into a crate root, where this becomes
/// `pub`.
pub(crate) type Result<T> = std::result::Result<T, Error>;

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
