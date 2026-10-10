// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// A refusal the CLI decides itself, before or around any library call; each carries its message.
///
/// Return this, not `eprintln!` + `Ok(ExitCode::…)`, so the message flows through [`super::finish`].
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum CliRefusal {
    /// clap rejected the command line; only the `--format json` document path builds this.
    #[error("{0}")]
    #[exit(
        UsageError,
        slug = "invalid_command_line",
        summary = "The command line names an unknown flag or nested subcommand, or a missing or malformed argument"
    )]
    InvalidCommandLine(String),

    /// Raw publisher-authored bytes bound for a terminal (CWE-150).
    #[error("{0}")]
    #[exit(
        UsageError,
        slug = "terminal_output_refused",
        summary = "Unsanitized document bytes were bound for a terminal; redirect to a file or a pipe"
    )]
    TerminalOutputRefused(String),

    /// `ocx shell allow` named the ocx home, which needs no consent.
    #[error("{0}")]
    #[exit(
        UsageError,
        slug = "ocx_home_needs_no_consent",
        summary = "The ocx home is always active and takes no consent stamp"
    )]
    OcxHomeNeedsNoConsent(String),

    /// An empty `--group` segment.
    #[error("{0}")]
    #[exit(
        UsageError,
        slug = "empty_group_filter",
        summary = "A group filter names an empty group"
    )]
    EmptyGroupFilter(String),

    /// A `--group` names an undeclared group.
    #[error("{0}")]
    #[exit(
        UsageError,
        slug = "unknown_group",
        summary = "A group filter names a group ocx.toml does not declare"
    )]
    UnknownGroup(String),

    /// A named binding is declared in no group the selection covers.
    #[error("{0}")]
    #[exit(
        UsageError,
        slug = "binding_not_selected",
        summary = "The named binding is declared in no selected group"
    )]
    BindingNotSelected(String),

    /// The predecessor `ocx.lock` a project verb rewrites is absent.
    #[error("{0}")]
    #[exit(ConfigError, slug = "lock_missing", summary = "ocx.lock does not exist")]
    LockMissing(String),

    /// `ocx direnv init` would overwrite an `.envrc` without `--force`.
    #[error("{0}")]
    #[exit(
        ConfigError,
        slug = "envrc_exists",
        summary = "An .envrc already exists and --force was not given"
    )]
    EnvrcExists(String),

    /// `ocx index regenerate` named a registry that is not a published index source.
    #[error("{0}")]
    #[exit(
        ConfigError,
        slug = "index_source_not_published",
        summary = "The registry is not a published index source, so it has no catalog to regenerate"
    )]
    IndexSourceNotPublished(String),

    /// A discovery verb ran under `--frozen`.
    #[error("{0}")]
    #[exit(
        PolicyBlocked,
        slug = "frozen_refused",
        summary = "The verb discovers new digests, which frozen mode forbids"
    )]
    FrozenRefused(String),

    /// `ocx package create` extracted an archive to nothing.
    #[error("{0}")]
    #[exit(
        DataError,
        slug = "archive_extracted_no_entries",
        summary = "The archive extracted no entries"
    )]
    ArchiveExtractedNoEntries(String),

    /// `ocx package receipt` found no build receipt beside the bundle.
    #[error("{0}")]
    #[exit(
        NotFound,
        slug = "build_receipt_not_found",
        summary = "No build receipt exists beside the bundle"
    )]
    BuildReceiptNotFound(String),

    /// A forge write resolved no API credential.
    #[error("{0}")]
    #[exit(
        AuthError,
        slug = "forge_credential_missing",
        summary = "A forge write found no API credential to authenticate with"
    )]
    ForgeCredentialMissing(String),
}
