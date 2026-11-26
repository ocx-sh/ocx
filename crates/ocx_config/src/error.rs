// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

/// Which config tier produced an error; picks the remediation hint (`--config` vs `--project`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    /// Config-tier source named explicitly: `--config` or `OCX_CONFIG`.
    Config,
    /// A config file ocx found itself (system / user / `$OCX_HOME`), not one the operator named.
    Discovered,
    /// Project-tier source: `--project` or `OCX_PROJECT`.
    Project,
}

impl ConfigSource {
    fn label(self) -> &'static str {
        match self {
            Self::Config | Self::Discovered => "config",
            Self::Project => "project",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Config => "check --config or OCX_CONFIG",
            Self::Discovered => "edit or remove this file",
            Self::Project => "check --project or OCX_PROJECT",
        }
    }
}

/// A TOML parse failure reduced to position and message, never the source line.
///
/// Stored instead of the raw `toml::de::Error` because that type's `Display` quotes the offending
/// line, and a config line can hold a credential (`headers`); the error chain is printed whole.
/// The quoted value serde puts in a type or variant error (`invalid type: string "…"`,
/// ``unknown variant `…` ``) is replaced by `…`.
#[derive(Debug)]
pub struct ParseFailure {
    position: Option<(usize, usize)>,
    message: String,
}

impl ParseFailure {
    /// `text` is the document `error` came from; without a span (a value-level failure) no position is kept.
    pub fn new(error: &toml::de::Error, text: &str) -> Self {
        Self {
            position: error.span().and_then(|span| position_of(text, span.start)),
            message: without_quoted_values(error.message()),
        }
    }

    /// [`Self::new`] for a `toml_edit` syntax error, whose `Display` also quotes the source line.
    /// Its message is built from the parser's expected tokens and holds no source value, so it is kept whole.
    pub fn from_toml_edit(error: &toml_edit::TomlError, text: &str) -> Self {
        Self {
            position: error.span().and_then(|span| position_of(text, span.start)),
            message: error.message().to_owned(),
        }
    }
}

/// The 1-based line and column of byte `offset` in `text`.
fn position_of(text: &str, offset: usize) -> Option<(usize, usize)> {
    let before = text.get(..offset)?;
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |tail| tail.chars().count()) + 1;
    Some((line, column))
}

/// Replaces every double-quoted span (Debug-escaped, as serde prints a string) and the name in an
/// "unknown variant" error with `…`, so a value read into the wrong type is never echoed.
fn without_quoted_values(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut chars = message.chars();
    let mut in_variant = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push_str("\"…\"");
                while let Some(next) = chars.next() {
                    match next {
                        '\\' => {
                            chars.next();
                        }
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '`' if message.starts_with("unknown variant") && !in_variant => {
                in_variant = true;
                out.push_str("`…`");
                for next in chars.by_ref() {
                    if next == '`' {
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

impl ParseFailure {
    /// A failure with no document position, from an error whose own `Display` would not be safe to print.
    pub fn from_message(message: impl Into<String>) -> Self {
        Self {
            position: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ParseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.position {
            Some((line, column)) => write!(f, "line {line}, column {column}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for ParseFailure {}

/// Errors that can occur during configuration parsing and validation.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "ConfigError")]
pub enum Error {
    /// A TOML configuration file could not be parsed.
    #[error("invalid TOML at {}", path.display())]
    #[exit(ConfigError, slug = "config_parse", summary = "A config file does not parse")]
    Parse {
        path: PathBuf,
        // Boxed to keep this enum under clippy's `result_large_err` limit on Windows.
        #[source]
        source: Box<ParseFailure>,
    },

    /// A path list (`OCX_CONFIG`, or the blob directories) names more entries than ocx will use.
    #[error("{list} lists {count} entries, more than the limit of {limit}; reduce it to at most {limit} entries")]
    #[exit(
        ConfigError,
        slug = "config_list_too_long",
        summary = "A path list holds more entries than the limit"
    )]
    ConfigListTooLong {
        list: &'static str,
        count: usize,
        limit: usize,
    },

    /// A path in the `OCX_CONFIG` list contains the list separator and cannot be carried through it.
    #[error(
        "config path contains the OCX_CONFIG list separator: {}; give --config a path without the separator, or move the file",
        path.display()
    )]
    #[exit(
        ConfigError,
        slug = "config_path_contains_separator",
        summary = "A config path contains the OCX_CONFIG list separator"
    )]
    ConfigPathContainsSeparator { path: PathBuf },

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

    /// A `[blobs]` path in the managed tier, or one that reached resolution unanchored, is relative.
    #[error("[blobs] path {} is relative; the managed tier accepts absolute paths only", path.display())]
    #[exit(
        ConfigError,
        slug = "blob_path_relative",
        summary = "A [blobs] path is relative where only an absolute one is accepted"
    )]
    BlobPathRelative { path: PathBuf },
    /// A config file still carries the retired `[trust.sigstore]` table.
    #[error(
        "{} declares `[trust.sigstore]`, which is no longer read; move its keys to `[sigstore.\"<fulcio host>\"]` \
         (the host of its fulcio_url, or \"fulcio.sigstore.dev\"), and add `sigstore = \"<fulcio host>\"` to each \
         keyless signer that trusted it",
        path.display()
    )]
    #[exit(
        ConfigError,
        slug = "trust_sigstore_retired",
        summary = "A config file declares the retired [trust.sigstore] table"
    )]
    TrustSigstoreRetired { path: PathBuf },

    /// A `[sigstore."<authority>"]` key, or a keyless signer's `sigstore` value, is not a valid authority.
    #[error("invalid Sigstore authority")]
    #[exit(
        ConfigError,
        slug = "sigstore_authority_invalid",
        summary = "A Sigstore authority is not a valid host and port"
    )]
    SigstoreAuthorityInvalid(#[source] ocx_trust::SigstoreAuthorityError),

    /// Two `[sigstore."<key>"]` keys of one file spell the same authority, so one table would silently replace the other.
    #[error("[sigstore.\"{first}\"] and [sigstore.\"{second}\"] name the same Sigstore authority: keep one table")]
    #[exit(
        ConfigError,
        slug = "sigstore_authority_invalid",
        summary = "A Sigstore authority is not a valid host and port"
    )]
    SigstoreAuthorityDuplicate { first: String, second: String },

    /// One `[sigstore."<authority>"]` table sets both `trusted_root` and `trusted_root_json`.
    #[error(
        "{} declares both trusted_root and trusted_root_json in [sigstore.\"{authority}\"]: keep one (trusted_root \
         names a local file; trusted_root_json is the inline document `ocx config push` publishes)",
        path.display()
    )]
    #[exit(
        ConfigError,
        slug = "ambiguous_trust_root",
        summary = "The payload declares both trusted_root and trusted_root_json"
    )]
    AmbiguousTrustRoot { path: PathBuf, authority: String },

    /// A `[sigstore."<authority>"]` table whose `fulcio_url` names a different authority than its key.
    #[error("[sigstore.\"{authority}\"] sets fulcio_url `{fulcio_url}`, whose authority differs from the table key")]
    #[exit(
        ConfigError,
        slug = "sigstore_table_key_mismatch",
        summary = "A Sigstore table's fulcio_url names another authority than its key"
    )]
    SigstoreTableKeyMismatch { authority: String, fulcio_url: String },

    /// Signing with a Rekor upload against a table that sets no `rekor_url`.
    #[error("[sigstore.\"{authority}\"] sets no rekor_url, so a signature cannot be uploaded to its transparency log")]
    #[exit(
        ConfigError,
        slug = "sigstore_rekor_url_missing",
        summary = "A Sigstore table has no rekor_url to upload a signature to"
    )]
    SigstoreRekorUrlMissing { authority: String },

    /// `index_signature = "enforce"` on a registry with no signer list.
    #[error(
        "[registries.\"{registry}\"] sets index_signature = \"enforce\" but names no signers; set index_publishers"
    )]
    #[exit(
        ConfigError,
        slug = "index_publishers_missing",
        summary = "Index signature enforcement is on with no publishers to check against"
    )]
    IndexPublishersMissing { registry: String },

    /// An `index_publishers` entry that cannot be what its author meant.
    #[error("[registries.\"{registry}\"] index_publishers is invalid: {reason}")]
    #[exit(
        ConfigError,
        slug = "index_publishers_invalid",
        summary = "An index_publishers entry is malformed"
    )]
    IndexPublishersInvalid { registry: String, reason: String },
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
        let marker = ParseFailure::new(&toml_err, "not valid [[").to_string();
        let err = Error::Parse {
            path: PathBuf::from("/bad.toml"),
            source: Box::new(ParseFailure::new(&toml_err, "not valid [[")),
        };
        let rendered = render_chain(&err as &(dyn std::error::Error + 'static));
        let count = rendered.matches(marker.as_str()).count();
        assert_eq!(
            count, 1,
            "TOML source should appear exactly once, got {count}: {rendered}"
        );
    }

    #[test]
    fn explicit_config_error_hints_at_the_config_flag() {
        let err = Error::FileNotFound {
            path: PathBuf::from("/given.toml"),
            tier: ConfigSource::Config,
        };
        assert!(err.to_string().contains("check --config or OCX_CONFIG"), "{err}");
    }

    #[test]
    fn discovered_config_error_never_hints_at_the_config_flag() {
        let err = Error::Io {
            path: PathBuf::from("/etc/ocx/config.toml"),
            tier: ConfigSource::Discovered,
            source: std::io::Error::other("denied"),
        };
        let text = err.to_string();
        assert!(text.contains("/etc/ocx/config.toml"), "{text}");
        assert!(text.contains("edit or remove this file"), "{text}");
        assert!(!text.contains("--config"), "{text}");
    }

    #[test]
    fn quoted_values_are_replaced_in_a_serde_message() {
        assert_eq!(
            without_quoted_values("invalid type: string \"Bearer \\\"x\\\" y\", expected a boolean"),
            "invalid type: string \"…\", expected a boolean"
        );
        assert_eq!(
            without_quoted_values("unknown variant `Bearer x`, expected `a` or `b`"),
            "unknown variant `…`, expected `a` or `b`"
        );
        assert_eq!(without_quoted_values("expected `=`"), "expected `=`");
    }

    /// toml_edit's expected-token hint may name `"`; it is kept, and the source line still is not.
    #[test]
    fn a_toml_edit_hint_naming_a_quote_survives_and_no_source_value_does() {
        let text = "key = \"ocxtest-tok-XXXX\nnext = 1\n";
        let error = text.parse::<toml_edit::DocumentMut>().unwrap_err();
        assert!(
            error.message().contains("`\"`"),
            "premise: hint names a quote: {}",
            error.message()
        );

        let rendered = ParseFailure::from_toml_edit(&error, text).to_string();

        assert!(rendered.contains(error.message()), "hint truncated: {rendered}");
        assert!(!rendered.contains("ocxtest-tok-XXXX"), "token leaked: {rendered}");
    }

    /// Whatever the syntax error, the canary on the broken line is in neither rendering and the line is.
    #[test]
    fn no_toml_edit_syntax_error_echoes_the_broken_line() {
        let documents = [
            ("duplicate key", "a = 1\na = \"ocxtest-tok-XXXX\"\n"),
            ("bad escape", "a = \"ocxtest-tok-XXXX \\q\"\n"),
            ("bad number", "a = 12ocxtest-tok-XXXX\n"),
            ("bad date", "a = 2024-13-45T00:00:00Z ocxtest-tok-XXXX\n"),
            ("unclosed table header", "ok = 1\n[ocxtest-tok-XXXX\n"),
        ];
        for (label, text) in documents {
            let error = text.parse::<toml_edit::DocumentMut>().unwrap_err();
            let failure = ParseFailure::from_toml_edit(&error, text);
            for rendered in [failure.to_string(), format!("{failure:?}")] {
                assert!(
                    !rendered.contains("ocxtest-tok-XXXX"),
                    "{label}: canary leaked: {rendered}"
                );
            }
            assert!(
                failure.to_string().contains("line "),
                "{label}: no line number: {failure}"
            );
        }
    }
}
