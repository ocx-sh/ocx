// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::cli::{ClassifyExitCode, ExitCode};
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
    #[error("tool '{name}': value '{value}' is not a valid identifier: {source}")]
    ToolValueInvalid {
        name: String,
        value: String,
        #[source]
        source: IdentifierError,
    },
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
                ProjectErrorKind::Locked => ExitCode::TempFail,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
