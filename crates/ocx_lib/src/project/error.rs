// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci::Identifier;
use crate::oci::identifier::error::IdentifierError;

/// Project-tier errors (parse, schema, canonicalization, lock I/O).
///
/// Outer error type returned from `project::*` APIs. Wraps a [`ProjectError`]
/// carrying contextual path, which wraps a [`ProjectErrorKind`] discriminant.
/// All project-tier failures flow through this single variant — path context
/// is attached by callers at I/O boundaries (see `ProjectError::new`).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A project-tier operation failed on a specific file.
    #[error("{0}")]
    Project(#[from] ProjectError),
}

/// Error context: which file the failure occurred on.
#[derive(Debug)]
pub struct ProjectError {
    pub path: PathBuf,
    pub kind: ProjectErrorKind,
}

impl ProjectError {
    /// Construct a new [`ProjectError`] attaching `path` context to `kind`.
    pub fn new(path: impl Into<PathBuf>, kind: ProjectErrorKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Path-less constructions (e.g., `from_toml_str` or in-memory
        // serialization in `to_toml_string`) carry an empty path. Emit
        // only the kind in that case so the chain doesn't lead with a
        // bare `: ` separator.
        if self.path.as_os_str().is_empty() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.path.display(), self.kind)
        }
    }
}

impl std::error::Error for ProjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.kind)
    }
}

/// Inner error discriminant for project-tier failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectErrorKind {
    /// Failed to read the project file or lock from disk.
    #[error("I/O error")]
    Io(#[source] std::io::Error),

    /// TOML parse failure.
    #[error("invalid TOML")]
    TomlParse(#[source] toml::de::Error),

    /// TOML serialization failure (lock writing path).
    #[error("TOML serialization error")]
    TomlSerialize(#[source] toml::ser::Error),

    /// A tool name appears in both `[tools]` and a `[group.*]` table.
    #[error("tool '{name}' declared in both [tools] and [group.{group}]")]
    DuplicateToolAcrossSections { name: String, group: String },

    /// `[group.default]` is declared — `default` is a reserved group name
    /// that maps to the top-level `[tools]` table.
    #[error("[group.default] is reserved; put tools in the top-level [tools] table")]
    ReservedGroupName,

    /// A `--group` CLI argument contained an empty segment (e.g.
    /// `-g ci,,lint`). The CLI layer pre-validates this before calling
    /// the library; the variant exists as defense-in-depth for
    /// non-CLI callers that bypass the pre-validation.
    #[error("empty group name in group filter")]
    EmptyGroupFilter,

    /// A `--group` CLI argument referenced a group not declared in
    /// `ocx.toml`. The CLI layer pre-validates this before calling the
    /// library; the variant exists for non-CLI callers.
    #[error("unknown group '{name}'; declare `[group.{name}]` in ocx.toml first")]
    UnknownGroup { name: String },

    /// Unknown `declaration_hash_version` — the canonicalization contract
    /// version stored alongside the hash is from a newer OCX release.
    /// Reading the lock is refused rather than silently comparing against
    /// a hash computed by a different algorithm.
    #[error("unsupported declaration_hash_version {version}; this build understands version 1")]
    UnsupportedDeclarationHashVersion { version: u8 },

    /// The file on disk exceeds the project-tier size cap (64 KiB).
    /// Mirrors the ambient config loader's [`crate::config::ConfigErrorKind::FileTooLarge`]
    /// guard — a sanity check against pathologically large inputs in CI.
    #[error("file too large: {size} bytes exceeds limit of {limit} bytes")]
    FileTooLarge { size: u64, limit: u64 },

    /// The sidecar advisory lock for `ocx.lock` is held by another
    /// writer. Surfaced from [`crate::project::ProjectLock::load_exclusive`]
    /// when the exclusive `try_lock` fails immediately. Distinct from
    /// [`Self::Io`] so callers can retry-with-backoff or surface a
    /// human-readable "another OCX process is writing the lock" message.
    #[error("ocx.lock sidecar is locked by another process")]
    Locked,

    /// A `[tools]` or `[group.*]` value is missing an explicit registry.
    ///
    /// Bare-tag values like `cmake = "3.28"` are rejected: the project-tier
    /// declaration requires fully-qualified identifiers
    /// (`registry/repo:tag`) so resolution is reproducible regardless of
    /// `OCX_DEFAULT_REGISTRY`.
    #[error(
        "tool '{name}': value '{value}' is missing a registry; expected 'registry/repo:tag' (e.g. 'ocx.sh/cmake:3.28')"
    )]
    ToolValueMissingRegistry { name: String, value: String },

    /// A `[tools]` or `[group.*]` value failed to parse as an [`crate::oci::Identifier`]
    /// for a reason other than missing registry (invalid characters,
    /// malformed digest, uppercase repo, etc.). Carries the underlying
    /// [`IdentifierError`] for diagnostic context.
    #[error("tool '{name}': value '{value}' is not a valid identifier")]
    ToolValueInvalid {
        name: String,
        value: String,
        #[source]
        source: IdentifierError,
    },

    /// Resolution failed because the identifier's tag does not exist on
    /// the registry (404 from the manifest endpoint, or `Ok(None)` from
    /// the index layer). Distinct from [`Self::RegistryUnreachable`] so
    /// callers can exit with `NotFound` (79) rather than `Unavailable`
    /// (69).
    ///
    /// The message names the attempted tag explicitly because project-tier
    /// entries without a tag default to `:latest` at parse time
    /// (`crate::project::ProjectConfig::from_toml_str`); surfacing the
    /// effective tag tells the user which value the resolver actually
    /// asked the registry for.
    ///
    /// The [`Identifier`] is boxed to keep `ProjectErrorKind` small —
    /// mirrors the [`crate::package_manager::error::OfflineManifestMissing`]
    /// precedent, avoiding a workspace-wide `clippy::result_large_err`
    /// suppression.
    #[error(
        "tag '{tag}' not found in '{registry}/{repository}'",
        tag = .identifier.tag_or_latest(),
        registry = .identifier.registry(),
        repository = .identifier.repository(),
    )]
    TagNotFound { identifier: Box<Identifier> },

    /// Resolution failed because the registry rejected the request for
    /// authentication reasons (401, 403, or an equivalent policy
    /// denial). Terminal — the resolver does not retry.
    #[error("authentication failed for '{identifier}'")]
    AuthFailure {
        identifier: Box<Identifier>,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Resolution failed because the registry was unreachable after
    /// exhausting the retry budget. Transient-looking `ClientError`
    /// variants (network, 5xx) are wrapped here.
    #[error("registry unreachable for '{identifier}'")]
    RegistryUnreachable {
        identifier: Box<Identifier>,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Resolution for a single tool exceeded the per-tool timeout. The
    /// underlying cause is absent (we only know the deadline fired).
    #[error("resolve timed out for '{identifier}'")]
    ResolveTimeout { identifier: Box<Identifier> },

    /// The same binding name appears in two selected groups with
    /// non-equivalent identifiers — the composer cannot decide which to
    /// use without an explicit override.
    #[error("tool '{name}' defined in multiple selected groups")]
    DuplicateToolAcrossSelectedGroups {
        name: String,
        group_a: String,
        group_b: String,
    },

    /// At least one `--group` was selected but `ocx.lock` is absent from
    /// disk. Group resolution requires a committed lock. Surfaced from
    /// [`crate::project::compose_tool_set`] as defense-in-depth when the
    /// CLI layer's pre-load check is bypassed by a non-CLI consumer.
    #[error("ocx.lock is missing; run `ocx lock`")]
    LockMissing,

    /// A binding name passed to [`crate::project::resolve_lock_partial`]
    /// is not declared in `ocx.toml`. Surfaced by `ocx update <name>`
    /// when the user names a tool that does not exist.
    #[error("tool '{name}' not declared in ocx.toml")]
    ToolNotInConfig { name: String },

    /// The binding already exists in `ocx.toml` (default group or a named
    /// group). Surfaced by `ocx add` when the user attempts to add a
    /// tool that is already declared in any group. Callers should surface
    /// a hint to use `ocx remove` first or edit `ocx.toml` directly.
    #[error("binding '{name}' already exists in ocx.toml")]
    BindingAlreadyExists { name: String },

    /// The binding to remove was not found in any group in `ocx.toml`.
    /// Surfaced by `ocx remove` when the user names a tool that does not
    /// exist in any group.
    #[error("binding '{name}' not found in ocx.toml")]
    BindingNotFound { name: String },

    /// `ocx init` was called in a directory that already contains an
    /// `ocx.toml`. The command is idempotent-failure: rather than
    /// silently overwriting a hand-edited file, it surfaces this error
    /// so the user can inspect the existing config first.
    #[error("ocx.toml already exists at '{path}'")]
    ConfigAlreadyExists { path: PathBuf },

    /// The `--group` name passed to `ocx add` contains characters that are
    /// invalid for a TOML table key in `ocx.toml`. Valid group names consist
    /// solely of alphanumeric characters, `-`, and `_`, and must be non-empty.
    /// Rejected characters include `/`, `\`, NUL bytes, and `..` sequences.
    #[error("invalid group name '{name}': must be non-empty and contain only alphanumeric characters, '-', or '_'")]
    InvalidGroupName { name: String },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Project(e) => match &e.kind {
                ProjectErrorKind::Io(_) => ExitCode::IoError,
                ProjectErrorKind::TomlParse(_)
                | ProjectErrorKind::TomlSerialize(_)
                | ProjectErrorKind::DuplicateToolAcrossSections { .. }
                | ProjectErrorKind::ReservedGroupName
                | ProjectErrorKind::UnsupportedDeclarationHashVersion { .. }
                | ProjectErrorKind::FileTooLarge { .. }
                | ProjectErrorKind::ToolValueMissingRegistry { .. }
                | ProjectErrorKind::ToolValueInvalid { .. } => ExitCode::ConfigError,
                ProjectErrorKind::EmptyGroupFilter
                | ProjectErrorKind::UnknownGroup { .. }
                | ProjectErrorKind::DuplicateToolAcrossSelectedGroups { .. } => ExitCode::UsageError,
                ProjectErrorKind::Locked => ExitCode::TempFail,
                ProjectErrorKind::TagNotFound { .. } => ExitCode::NotFound,
                ProjectErrorKind::AuthFailure { .. } => ExitCode::AuthError,
                ProjectErrorKind::RegistryUnreachable { .. } | ProjectErrorKind::ResolveTimeout { .. } => {
                    ExitCode::Unavailable
                }
                ProjectErrorKind::LockMissing => ExitCode::ConfigError,
                ProjectErrorKind::ToolNotInConfig { .. } => ExitCode::NotFound,
                ProjectErrorKind::BindingAlreadyExists { .. } => ExitCode::UsageError,
                ProjectErrorKind::BindingNotFound { .. } => ExitCode::NotFound,
                ProjectErrorKind::ConfigAlreadyExists { .. } => ExitCode::UsageError,
                ProjectErrorKind::InvalidGroupName { .. } => ExitCode::UsageError,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::identifier::error::{IdentifierError, IdentifierErrorKind};

    /// Block #2 regression: `ToolValueInvalid` must NOT embed `: {source}` in its
    /// `Display` string. The `#[source]` attribute already exposes the inner error
    /// via `std::error::Error::source()`; duplicating it in the format string causes
    /// the `IdentifierError` message to appear twice when callers walk the chain
    /// with `{err:#}`.
    #[test]
    fn tool_value_invalid_source_appears_exactly_once_in_chain() {
        use std::error::Error;

        let ident_err = IdentifierError::new("bad//value", IdentifierErrorKind::InvalidFormat);
        // Capture the IdentifierError display message for comparison.
        let ident_display = ident_err.to_string();

        let kind = ProjectErrorKind::ToolValueInvalid {
            name: "cmake".to_string(),
            value: "bad//value".to_string(),
            source: ident_err,
        };

        // The Display of the kind itself must NOT contain the IdentifierError text.
        // The source is exposed only via the Error::source() chain, not inline.
        let kind_display = kind.to_string();
        assert!(
            !kind_display.contains(&ident_display),
            "ToolValueInvalid Display must not embed source message; got: {kind_display:?}"
        );

        // Walk the source chain manually and collect every Display string.
        let outer = crate::project::Error::Project(ProjectError::new(std::path::PathBuf::from("/tmp/ocx.toml"), kind));
        let mut chain_msgs = Vec::new();
        chain_msgs.push(outer.to_string());
        let mut cause: Option<&dyn Error> = outer.source();
        while let Some(e) = cause {
            chain_msgs.push(e.to_string());
            cause = e.source();
        }

        // "invalid format" is the Display of IdentifierErrorKind::InvalidFormat.
        // It must appear exactly once in the chain — via source(), not duplicated in Display.
        let occurrences = chain_msgs.iter().filter(|msg| msg.contains("invalid format")).count();
        assert_eq!(
            occurrences, 1,
            "IdentifierError message must appear exactly once in the chain; chain={chain_msgs:?}"
        );
    }

    #[test]
    fn display_with_path_uses_path_prefix_separator() {
        let err = ProjectError::new(PathBuf::from("/tmp/ocx.toml"), ProjectErrorKind::ReservedGroupName);
        let rendered = err.to_string();
        assert!(
            rendered.starts_with("/tmp/ocx.toml: "),
            "expected path prefix; got {rendered:?}"
        );
    }

    #[test]
    fn display_without_path_omits_leading_separator() {
        // Path-less constructions (e.g. `from_toml_str` or `to_toml_string`)
        // pass `PathBuf::new()`. The Display impl must skip the prefix so
        // the chain doesn't render with a bare ": <kind>" head.
        let err = ProjectError::new(PathBuf::new(), ProjectErrorKind::ReservedGroupName);
        let rendered = err.to_string();
        assert!(
            !rendered.starts_with(':'),
            "path-less error must not start with a colon; got {rendered:?}"
        );
        assert!(
            !rendered.starts_with(' '),
            "path-less error must not start with whitespace; got {rendered:?}"
        );
    }
}
