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

    /// A V2 lock `[[tool]]` `repository` carried a tag or digest. The V2
    /// shape stores bare registry/repo coordinates only — the per-platform
    /// pull id is reconstructed from `repository` + the per-platform leaf
    /// digest, so a tag or digest on `repository` is a malformed lock.
    #[error("lock repository '{value}' must be bare (no tag, no digest)")]
    LockRepositoryNotBare { value: String },

    /// Two advertised index children stringified to the same V2 platform
    /// map key while building the available-only `platforms` map. A
    /// defensive guard against a silent `BTreeMap` overwrite — should never
    /// fire under the lossless, injective [`crate::oci::Platform::lock_key`].
    #[error("duplicate platform key '{key}' in resolved leaf map")]
    DuplicatePlatformKey { key: String },

    /// A lock-mutating command (`ocx add`/`remove`) carried forward an
    /// untouched V1 entry whose legacy index digest is no longer retrievable,
    /// so it could not be transcribed to V2 exact-only. The command refuses to
    /// silently re-resolve the untouched tool's live tag (Codex R2); the user
    /// must run the whole-file bump verb `ocx upgrade` to re-resolve instead.
    ///
    /// The remedy is **tier-neutral** (spec §4.1): the error layer has no tier
    /// context, so a `ocx --global add/remove` user is told to add `--global`
    /// rather than being handed a project-only `ocx upgrade` that would target
    /// the wrong toolchain.
    #[error(
        "tool '{name}': locked entry can no longer be migrated exactly; run `ocx upgrade` to re-resolve (add `--global` for the global toolchain)"
    )]
    LockUpgradeRequired { name: String },

    /// A V2 lock entry ships no leaf for the host platform (no host key and
    /// no `"any"` fallback) at the locked version. Decided from local lock
    /// bytes, so it surfaces at lock-read **pre-network** rather than as a
    /// late `SelectResult::NotFound`. The publisher does not ship this
    /// platform at the locked version (or the lock predates it) — run
    /// `ocx upgrade` (the whole-file bump verb) to re-resolve if it has
    /// since been added.
    #[error(
        "no '{platform}' leaf for tool '{name}' at the locked version; run `ocx upgrade` to re-resolve if it has since been added"
    )]
    NoHostLeaf { name: String, platform: String },

    /// A reserved group keyword (`default`, `all`) was declared as a
    /// `[group.<name>]` table in `ocx.toml`.
    ///
    /// `name` is the reserved keyword that was found. `hint` is the
    /// actionable phrase shown to the user explaining what to do instead
    /// (e.g. `"put tools in the top-level [tools] table"` for `default`,
    /// `"rename this group; \`all\` is a reserved keyword that selects every declared group"`
    /// for `all`). Keeping `hint` as a `&'static str` avoids allocation and
    /// lets each reserved keyword carry its own tailored guidance.
    #[error("[group.{name}] is reserved; {hint}")]
    ReservedGroupName { name: String, hint: &'static str },

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

    /// Another writer holds the exclusive advisory flock on `ocx.toml`.
    ///
    /// Surfaced from [`crate::project::acquire_project_lock`] when
    /// `FileLock::try_exclusive` finds the lock already held. Distinct from
    /// [`Self::Io`] so callers can retry with backoff or surface a
    /// human-readable "another OCX process is writing" message.
    #[error("ocx.toml is locked by another process")]
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

    /// A `[package."<key>"]` table key is missing an explicit registry.
    ///
    /// Per-package settings (`no-patches`) key on a fully-qualified
    /// identifier (`registry/repo[:tag]`); a bare key like
    /// `[package."cmake"]` is rejected, mirroring the `[tools]` value rule.
    #[error("package key '{key}' is missing a registry; expected 'registry/repo[:tag]' (e.g. 'ocx.sh/cmake')")]
    PackageKeyMissingRegistry { key: String },

    /// A `[package."<key>"]` table key failed to parse as an
    /// [`crate::oci::Identifier`] for a reason other than missing registry
    /// (invalid characters, malformed digest, uppercase repo, etc.). Carries
    /// the underlying [`IdentifierError`] for diagnostic context.
    #[error("package key '{key}' is not a valid identifier")]
    PackageKeyInvalid {
        key: String,
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
    #[error("tool '{name}' defined in multiple selected groups: '{group_a}' and '{group_b}'")]
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

    /// A binding name passed to [`crate::project::resolve_lock_touched`]
    /// as a touched `(group, name)` pair is not declared in `ocx.toml`.
    /// Surfaced when a caller names a tool that does not exist.
    #[error("tool '{name}' not declared in ocx.toml")]
    ToolNotInConfig { name: String },

    /// The binding already exists in the target group in `ocx.toml`.
    /// Surfaced by `ocx add` when the user attempts to add a tool that is
    /// already declared in the same group. Callers should surface a hint
    /// to use `ocx remove` first or edit `ocx.toml` directly.
    ///
    /// `group` is `"default"` for the implicit top-level `[tools]` table,
    /// or the named group string for `[group.<name>]` tables.
    #[error("binding '{name}' already exists in group '{group}'")]
    BindingAlreadyExists { name: String, group: String },

    /// The binding to remove was found in multiple groups, making the
    /// target ambiguous. Surfaced by `ocx remove` when `--group` is not
    /// specified and the binding name appears in more than one group.
    /// Callers should re-invoke with `--group <name>` to disambiguate.
    #[error("binding '{name}' exists in multiple groups: {groups:?} — pass --group to disambiguate")]
    BindingAmbiguous { name: String, groups: Vec<String> },

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
    /// solely of alphanumeric characters, `-`, and `_`, must be non-empty,
    /// and must not be a reserved keyword (`default`, `all`).
    #[error(
        "invalid group name '{name}': must be non-empty, contain only alphanumeric characters, '-', or '_', and not be a reserved keyword (`default`, `all`)"
    )]
    InvalidGroupName { name: String },

    /// Pin-preserving carry-forward refused: the predecessor lock's
    /// [`crate::project::lock::LockMetadata::declaration_hash`] does not
    /// match the **pre-mutation** declaration hash of `ocx.toml`.
    ///
    /// Surfaced from [`crate::project::resolve_lock_touched`] (the
    /// `ocx add`/`remove` freshness gate) when the lock was not current
    /// with `ocx.toml` *before* this command's edit — typically the user
    /// hand-edited `ocx.toml` since the last lock, or two processes raced.
    /// The mutator refuses to carry untouched bindings forward against a
    /// stale lock; the user reconciles the whole file with `ocx lock`,
    /// then re-runs the add/remove.
    ///
    /// The remedy is **tier-neutral** (spec §4.1): the error layer has no tier
    /// context, so a `ocx --global add/remove` user is told to add `--global`
    /// rather than being handed a project-only `ocx lock` that would reconcile
    /// the wrong toolchain.
    ///
    /// Both hashes are surfaced in the message so operators can diff the
    /// lock-on-disk against the live config without re-running the
    /// hasher manually. `previous_hash` is the value carried by the
    /// supplied predecessor lock; `current_hash` is the hash of the
    /// pre-mutation `ocx.toml` snapshot.
    #[error(
        "lock is out of sync with ocx.toml (declaration_hash {current_hash} != locked {previous_hash}); run `ocx lock` to reconcile (add `--global` for the global toolchain)"
    )]
    StaleLockOnPartial {
        previous_hash: String,
        current_hash: String,
    },

    /// A no-resolve routing policy (`--offline` or `--frozen`) refused to
    /// resolve an unpinned (tag-only) reference while building the lock.
    ///
    /// Surfaced from the resolver ([`crate::project::resolve`]) when the
    /// index layer returns [`crate::oci::index::error::Error::PolicyResolutionBlocked`]:
    /// the tag was not in the local index and the active policy forbids
    /// walking the source chain to fetch + commit it. Terminal — the
    /// resolver does not retry. `policy` is the lowercase flag label
    /// (`"offline"` / `"frozen"`). Populate the local index (e.g.
    /// `ocx index update`) or loosen the flag.
    #[error(
        "{policy} mode refused to resolve unpinned reference '{identifier}'; run `ocx index update` or pin a digest"
    )]
    PolicyBlocked {
        identifier: Box<Identifier>,
        policy: &'static str,
    },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::Project(e) => match &e.kind {
                ProjectErrorKind::Io(_) => ExitCode::IoError,
                ProjectErrorKind::TomlParse(_)
                | ProjectErrorKind::TomlSerialize(_)
                | ProjectErrorKind::ReservedGroupName { .. }
                | ProjectErrorKind::UnsupportedDeclarationHashVersion { .. }
                | ProjectErrorKind::FileTooLarge { .. }
                | ProjectErrorKind::ToolValueMissingRegistry { .. }
                | ProjectErrorKind::ToolValueInvalid { .. }
                | ProjectErrorKind::PackageKeyMissingRegistry { .. }
                | ProjectErrorKind::PackageKeyInvalid { .. }
                | ProjectErrorKind::LockRepositoryNotBare { .. } => ExitCode::ConfigError,
                ProjectErrorKind::EmptyGroupFilter
                | ProjectErrorKind::UnknownGroup { .. }
                | ProjectErrorKind::DuplicateToolAcrossSelectedGroups { .. }
                | ProjectErrorKind::BindingAmbiguous { .. } => ExitCode::UsageError,
                ProjectErrorKind::Locked => ExitCode::TempFail,
                ProjectErrorKind::TagNotFound { .. } => ExitCode::NotFound,
                ProjectErrorKind::AuthFailure { .. } => ExitCode::AuthError,
                ProjectErrorKind::RegistryUnreachable { .. } | ProjectErrorKind::ResolveTimeout { .. } => {
                    ExitCode::Unavailable
                }
                ProjectErrorKind::LockMissing => ExitCode::ConfigError,
                // A carried-forward V1 entry cannot be migrated exactly —
                // the lock needs an explicit `ocx upgrade` re-resolve.
                ProjectErrorKind::LockUpgradeRequired { .. } => ExitCode::ConfigError,
                // The locked version ships no leaf for the host platform —
                // a pre-network config-state condition; the remedy is a
                // whole-file re-resolve (`ocx upgrade`).
                ProjectErrorKind::NoHostLeaf { .. } => ExitCode::ConfigError,
                ProjectErrorKind::ToolNotInConfig { .. } => ExitCode::NotFound,
                ProjectErrorKind::BindingAlreadyExists { .. } => ExitCode::UsageError,
                ProjectErrorKind::BindingNotFound { .. } => ExitCode::NotFound,
                ProjectErrorKind::ConfigAlreadyExists { .. } => ExitCode::UsageError,
                ProjectErrorKind::InvalidGroupName { .. } => ExitCode::UsageError,
                // Stale predecessor on partial-resolve: the caller's lock
                // snapshot is out of date with the live config. Same
                // classification as the read-side staleness gate
                // (`ProjectContextError::StaleLock` → DataError 65) so
                // wrappers and scripts get a single signal regardless of
                // which resolver path detected the mismatch.
                ProjectErrorKind::StaleLockOnPartial { .. } => ExitCode::DataError,
                // A dup-key collision is a structural integrity violation in
                // the resolved leaf map — classify as malformed data (65).
                ProjectErrorKind::DuplicatePlatformKey { .. } => ExitCode::DataError,
                // Offline / frozen refused an unpinned-tag resolve during lock
                // building — same category as the index-layer policy block.
                ProjectErrorKind::PolicyBlocked { .. } => ExitCode::PolicyBlocked,
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
        let err = ProjectError::new(
            PathBuf::from("/tmp/ocx.toml"),
            ProjectErrorKind::ReservedGroupName {
                name: "default".to_string(),
                hint: "put tools in the top-level [tools] table",
            },
        );
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
        let err = ProjectError::new(
            PathBuf::new(),
            ProjectErrorKind::ReservedGroupName {
                name: "default".to_string(),
                hint: "put tools in the top-level [tools] table",
            },
        );
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

    // ── Whole-file model: fail-closed remedy message contract (spec §4.1) ────

    /// `StaleLockOnPartial` (the `add`/`remove` drift gate, exit 65) must name
    /// the user remedy `ocx lock`, name no internal function, and stay
    /// tier-neutral (spec §4.1): because the error layer has no tier context, a
    /// `ocx --global add/remove` user must be steered to add `--global` rather
    /// than handed a bare project-only `ocx lock` that would target the wrong
    /// toolchain. The pre-mutation hash mismatch on a mutator directs the user
    /// to reconcile the whole file first.
    #[test]
    fn stale_lock_on_partial_names_ocx_lock_remedy() {
        let kind = ProjectErrorKind::StaleLockOnPartial {
            previous_hash: "sha256:aaa".to_string(),
            current_hash: "sha256:bbb".to_string(),
        };
        let rendered = kind.to_string();
        assert!(
            rendered.contains("ocx.toml"),
            "message must name the drifted file ocx.toml; got {rendered:?}"
        );
        assert!(
            rendered.contains("`ocx lock`"),
            "message must name the user remedy `ocx lock`; got {rendered:?}"
        );
        // Tier-neutral (spec §4.1): the remedy must point `--global` users at
        // their tier rather than hard-coding a project-only `ocx lock`.
        assert!(
            rendered.contains("--global"),
            "message must stay tier-neutral by naming `--global` for the global toolchain; got {rendered:?}"
        );
        assert!(
            !rendered.contains("resolve_lock"),
            "message must not name an internal function; got {rendered:?}"
        );
        assert!(
            !rendered.contains("partial-resolve"),
            "message must use the user-facing whole-file vocabulary, not 'partial-resolve'; got {rendered:?}"
        );
        // C-GOOD-ERR: lowercase first word, no trailing period.
        assert!(
            rendered.chars().next().is_some_and(|c| c.is_lowercase()),
            "message must start lowercase per C-GOOD-ERR; got {rendered:?}"
        );
        assert!(
            !rendered.trim_end().ends_with('.'),
            "message must not end with a period per C-GOOD-ERR; got {rendered:?}"
        );
        // Both hashes surfaced for operator diffing.
        assert!(
            rendered.contains("sha256:aaa") && rendered.contains("sha256:bbb"),
            "both the locked and current hashes must be surfaced; got {rendered:?}"
        );
    }

    /// `LockUpgradeRequired` (a carried/survivor V1 index gone, exit 78) must
    /// now name `ocx upgrade` — the whole-file bump verb that re-resolves —
    /// not the deleted `ocx lock --upgrade`. It must name no internal function
    /// and stay tier-neutral (spec §4.1): a `ocx --global add/remove` user must
    /// be steered to add `--global` rather than handed a bare project-only
    /// `ocx upgrade` that would re-resolve the wrong toolchain.
    #[test]
    fn lock_upgrade_required_names_ocx_upgrade_remedy() {
        let kind = ProjectErrorKind::LockUpgradeRequired {
            name: "cmake".to_string(),
        };
        let rendered = kind.to_string();
        assert!(
            rendered.contains("`ocx upgrade`"),
            "message must name the user remedy `ocx upgrade`; got {rendered:?}"
        );
        // Tier-neutral (spec §4.1): the remedy must point `--global` users at
        // their tier rather than hard-coding a project-only `ocx upgrade`.
        assert!(
            rendered.contains("--global"),
            "message must stay tier-neutral by naming `--global` for the global toolchain; got {rendered:?}"
        );
        // `--upgrade` (substring) is intentionally present inside `--global`-free
        // checks only via the bare verb; ensure the deleted `lock --upgrade`
        // flag form does not appear.
        assert!(
            !rendered.contains("lock --upgrade"),
            "message must not name the deleted `ocx lock --upgrade` verb; got {rendered:?}"
        );
        assert!(
            !rendered.contains("transcribe"),
            "message must not leak an internal function name; got {rendered:?}"
        );
        assert!(
            rendered.contains("cmake"),
            "message must name the affected tool; got {rendered:?}"
        );
        // C-GOOD-ERR: lowercase first word, no trailing period.
        assert!(
            rendered.chars().next().is_some_and(|c| c.is_lowercase()),
            "message must start lowercase per C-GOOD-ERR; got {rendered:?}"
        );
        assert!(
            !rendered.trim_end().ends_with('.'),
            "message must not end with a period per C-GOOD-ERR; got {rendered:?}"
        );
    }
}
