// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_exit::{Pick, Row};
use ocx_oci::PackageRef;
use ocx_oci::client::error::ClientError;
use ocx_oci::package_ref::error::IdentifierError;

/// Project-tier errors; [`ProjectError`] attaches the file path at I/O boundaries.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "ProjectError")]
pub enum Error {
    /// A project-tier operation failed on a specific file.
    // Boxed: inline, `ProjectError` pushes this enum past clippy's `result_large_err` limit on Windows.
    #[error("{0}")]
    #[exit(delegate = 0.kind)]
    Project(#[source] Box<ProjectError>),
    /// An OCI client failure during lock resolution. Carried, not flattened:
    /// its transient/terminal split is what separates exit 69 from 74.
    #[error(transparent)]
    #[exit(delegate)]
    OciClient(#[from] ocx_oci::client::error::ClientError),
    /// An index-tier failure surfaced through lock resolution.
    // Not `#[from]`: the manual `From` below flattens two index variants a derive would wrap.
    #[error(transparent)]
    #[exit(delegate)]
    OciIndex(ocx_index::error::Error),
    /// A settings-tier failure surfaced while loading project state.
    #[error(transparent)]
    #[exit(delegate)]
    Config(#[from] ocx_config::error::Error),
    /// An I/O failure with its path, from a helper with no project context (exit 74).
    #[error("{0}: {1}")]
    #[exit(
        IoError,
        slug = "project_internal_file",
        summary = "Reading or writing an internal project file failed"
    )]
    InternalFile(PathBuf, #[source] std::io::Error),
    /// `ocx.toml` declared `[update]` (exit 78): a personal setting, and a repository must not
    /// switch on binary replacement for whoever clones it.
    #[error("{}: [update] belongs in config.toml, not in ocx.toml; update checks are a personal setting", .0.display())]
    #[exit(
        ConfigError,
        slug = "update_section_in_project",
        summary = "ocx.toml declares an [update] section, which only config.toml may hold"
    )]
    UpdateSectionInProject(PathBuf),
}

/// Flatten the index tier's `OciClient` and `File` onto this tier's variants.
/// A derived wrap hides the `ClientError` behind two transparent layers, so
/// `classify_client_error` misses it and a transient fault exits 69, not 75, unretried.
impl From<ocx_index::error::Error> for Error {
    fn from(error: ocx_index::error::Error) -> Self {
        use ocx_index::error::Error as IndexError;
        match error {
            IndexError::OciClient(error) => Self::OciClient(error),
            IndexError::File(error) => Self::InternalFile(error.path, error.cause),
            other => Self::OciIndex(other),
        }
    }
}

impl From<ProjectError> for Error {
    fn from(error: ProjectError) -> Self {
        Self::Project(Box::new(error))
    }
}

impl From<ocx_util::error::FileError> for Error {
    fn from(error: ocx_util::error::FileError) -> Self {
        Self::InternalFile(error.path, error.cause)
    }
}

/// Error context: which file the failure occurred on.
#[derive(Debug)]
pub struct ProjectError {
    pub path: PathBuf,
    pub kind: ProjectErrorKind,
}

impl ProjectError {
    pub fn new(path: impl Into<PathBuf>, kind: ProjectErrorKind) -> Self {
        Self {
            path: path.into(),
            kind,
        }
    }
}

impl std::fmt::Display for ProjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Path-less errors print only the kind, never a leading `: `.
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
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(family = "ProjectError")]
pub enum ProjectErrorKind {
    /// Failed to read the project file or lock from disk.
    #[error("I/O error")]
    #[exit(IoError, slug = "project_io", summary = "Reading or writing a project file failed")]
    Io(#[source] std::io::Error),

    /// TOML parse failure.
    #[error("invalid TOML")]
    #[exit(
        ConfigError,
        slug = "project_toml_invalid",
        summary = "ocx.toml or ocx.lock is not valid TOML of the expected shape"
    )]
    TomlParse(#[source] toml::de::Error),

    /// TOML serialization failure (lock writing path).
    #[error("TOML serialization error")]
    #[exit(
        ConfigError,
        slug = "project_toml_serialize",
        summary = "A project file could not be serialized"
    )]
    TomlSerialize(#[source] toml::ser::Error),

    /// `ocx.toml` did not parse as an editable document during a format-preserving mutation.
    #[error("ocx.toml is not an editable TOML document")]
    #[exit(
        ConfigError,
        slug = "project_manifest_not_editable",
        summary = "ocx.toml is not an editable TOML document"
    )]
    ManifestEditParse(#[source] toml_edit::TomlError),

    /// A format-preserving edit no longer describes the staged config; the write
    /// is abandoned, never replaced by a lossy whole-file rewrite.
    #[error("edited ocx.toml no longer matches the staged configuration")]
    #[exit(
        Failure,
        slug = "project_manifest_edit_diverged",
        summary = "The edited ocx.toml no longer matches the staged configuration"
    )]
    ManifestEditDiverged,

    /// A lock `[[tool]]` `repository` carried a tag or digest; it must be bare.
    #[error("lock repository '{value}' must be bare (no tag, no digest)")]
    #[exit(
        ConfigError,
        slug = "lock_repository_not_bare",
        summary = "ocx.lock records a repository carrying a tag or digest"
    )]
    LockRepositoryNotBare { value: String },

    /// Two index children stringified to one platform key; unreachable under the
    /// injective grammar, it guards a silent overwrite.
    #[error("duplicate platform key '{key}' in resolved leaf map")]
    #[exit(
        DataError,
        slug = "duplicate_platform_key",
        summary = "ocx.lock lists the same platform twice for one binding"
    )]
    DuplicatePlatformKey { key: String },

    /// A lock platform key is not the canonical spelling of the platform it parses
    /// to (e.g. `+b,a`); rejected, never normalized, so the bytes stay canonical.
    #[error("platform key '{key}' is not in canonical form; regenerate with `ocx lock`")]
    #[exit(
        DataError,
        slug = "noncanonical_platform_key",
        summary = "ocx.lock records a platform key not in canonical form"
    )]
    NoncanonicalPlatformKey { key: String },

    /// `metadata.lock_version` is not the version this build reads.
    #[error("unsupported ocx.lock version {found}; regenerate with `ocx lock`")]
    #[exit(
        ConfigError,
        slug = "unsupported_lock_version",
        summary = "ocx.lock has a version this build does not understand"
    )]
    UnsupportedLockVersion { found: u8 },

    /// A lock entry ships no leaf for the host (nor `"any"`) at the locked
    /// version; decided from lock bytes, before any network.
    #[error(
        "no '{platform}' leaf for binding '{name}' at the locked version; run `ocx update` to re-resolve if it has since been added"
    )]
    #[exit(
        ConfigError,
        slug = "no_host_leaf",
        summary = "ocx.lock pins no platform this host can run"
    )]
    NoHostLeaf { name: String, platform: String },

    /// Two or more lock leaves tie for the host (e.g. a dual-libc host); `ocx
    /// update` would tie again, so the remedy is `--platform`.
    #[error(
        "ambiguous '{platform}' leaf for binding '{name}': {} tied candidates ({}); pass --platform to disambiguate",
        candidates.len(),
        candidates.join(", ")
    )]
    #[exit(
        DataError,
        slug = "ambiguous_host_leaf",
        summary = "ocx.lock pins several platforms this host could run, none preferred"
    )]
    AmbiguousHostLeaf {
        name: String,
        platform: String,
        candidates: Vec<String>,
    },

    /// A reserved keyword (`default`, `all`) declared as `[group.<name>]`;
    /// `hint` says what to do instead.
    #[error("[group.{name}] is reserved; {hint}")]
    #[exit(
        ConfigError,
        slug = "reserved_group_name",
        summary = "ocx.toml declares a reserved group name"
    )]
    ReservedGroupName { name: String, hint: &'static str },

    /// A tools key or group name outside `^[A-Za-z0-9][A-Za-z0-9._-]*$` or over 64 bytes.
    /// A security control, not style: both become path components of the
    /// toolchain tree, which `ocx env` emits as environment values.
    #[error(
        "[{scope}] name '{name}' must match ^[A-Za-z0-9][A-Za-z0-9._-]*$ and be at most 64 bytes ({})",
        super::config::describe_toolchain_name_charset_violation(name)
    )]
    #[exit(
        ConfigError,
        slug = "invalid_toolchain_name_charset",
        summary = "A group or binding name carries a character toolchain paths refuse"
    )]
    InvalidToolchainNameCharset { scope: String, name: String },

    /// A `[group.<name>]` declares a binding directly instead of under
    /// `[group.<name>.tools]`; the message is the migration instruction.
    #[error(
        "group '{group}' declares binding '{binding}' directly; bindings belong under [group.{group}.tools] — [group.{group}] holds the `tools` and `env` sub-tables and the `lazy-mode` setting"
    )]
    #[exit(
        ConfigError,
        slug = "group_holds_direct_binding",
        summary = "A group table holds a binding outside its tools table"
    )]
    GroupHoldsDirectBinding { group: String, binding: String },

    /// A `[group.<name>]` key other than `tools`, `env` or `lazy-mode`; ignoring
    /// a typo would make a whole tool set vanish silently.
    #[error(
        "group '{group}' declares unknown key '{key}'; expected the `tools` or `env` sub-tables, or the `lazy-mode` setting"
    )]
    #[exit(
        ConfigError,
        slug = "unknown_group_section",
        summary = "A group table holds an unknown key"
    )]
    UnknownGroupSection { group: String, key: String },

    /// `ocx.toml` declared `[shell]`. Its own arm, not the typo detector: consent
    /// lives there, and a project file granting its own activation is self-consent.
    #[error(
        "[shell] belongs in a config.toml tier, not in ocx.toml; a project file cannot grant its own shell activation"
    )]
    #[exit(
        ConfigError,
        slug = "shell_section_in_project",
        summary = "ocx.toml declares a [shell] section, which only config.toml may hold"
    )]
    ShellSectionInProject,

    /// An `[env]` key in the reserved `OCX_*` / `__OCX_*` namespace: a checked-in
    /// file must not reconfigure how `ocx` resolves, and every child would inherit it.
    #[error("[{scope}] key '{key}' is reserved; OCX_* and __OCX_* keys cannot be set from ocx.toml")]
    #[exit(
        ConfigError,
        slug = "project_env_reserved_key",
        summary = "ocx.toml sets a reserved OCX environment key"
    )]
    EnvReservedKey { scope: String, key: String },

    /// An `[env]` key is not a POSIX environment-variable name.
    #[error("[{scope}] key '{key}' is not a valid environment variable name; expected [A-Za-z_][A-Za-z0-9_]*")]
    #[exit(
        ConfigError,
        slug = "project_env_invalid_key",
        summary = "ocx.toml sets an invalid environment variable name"
    )]
    EnvInvalidKey { scope: String, key: String },

    /// An `[env]` `type` outside the modifier vocabulary. Worded as `--env` is:
    /// a newer ocx's type is a version gap, not a typo.
    #[error(
        "[{scope}] key '{key}': unknown modifier type '{found}'; expected `path`, `constant` or `list` (a newer ocx may support it)"
    )]
    #[exit(
        ConfigError,
        slug = "project_env_unknown_modifier",
        summary = "An ocx.toml environment entry names an unknown modifier type"
    )]
    EnvUnknownModifier { scope: String, key: String, found: String },

    /// An `[env]` value is neither a string nor a `{ type, value }` table;
    /// `found` is its TOML type.
    #[error("[{scope}] key '{key}': expected a string or {{ type, value }} table, found {found}")]
    #[exit(
        ConfigError,
        slug = "project_env_invalid_value",
        summary = "An ocx.toml environment entry has a value of the wrong shape"
    )]
    EnvInvalidValue { scope: String, key: String, found: String },

    /// An `[env]` table field belonging to no modifier; ignoring a newer ocx's
    /// field (`required`) would silently change its semantics.
    #[error(
        "[{scope}] key '{key}': unknown field '{field}'; expected only `type`, `value`, and `separator` (list only)"
    )]
    #[exit(
        ConfigError,
        slug = "project_env_unknown_value_field",
        summary = "An ocx.toml environment entry table holds an unknown field"
    )]
    EnvUnknownValueField { scope: String, key: String, field: String },

    /// `separator` on a non-list type: a known field in the wrong company, so
    /// the remedy is a choice of two edits.
    #[error(
        "[{scope}] key '{key}': `separator` applies to `type = \"list\"` only, not `{kind}`; remove it or change the type"
    )]
    #[exit(
        ConfigError,
        slug = "project_env_separator_on_non_list",
        summary = "An ocx.toml environment entry sets a separator on a non-list value"
    )]
    EnvSeparatorOnNonList {
        scope: String,
        key: String,
        kind: ocx_package::metadata::env::modifier::ModifierKind,
    },

    /// An `[env]` list separator the fold cannot use.
    #[error(
        "[{scope}] key '{key}': separator {separator:?} must be non-empty and free of '=', newline and carriage return"
    )]
    #[exit(
        ConfigError,
        slug = "project_env_invalid_separator",
        summary = "An ocx.toml environment entry has an unusable list separator"
    )]
    EnvInvalidSeparator {
        scope: String,
        key: String,
        separator: String,
    },

    /// An `[env]` list value starts or ends with its separator, making the
    /// fold's flank match ambiguous.
    #[error("[{scope}] key '{key}': value {value:?} starts or ends with its separator {separator:?}")]
    #[exit(
        ConfigError,
        slug = "project_env_separator_edged_value",
        summary = "An ocx.toml list value starts or ends with its separator"
    )]
    EnvSeparatorEdgedValue {
        scope: String,
        key: String,
        separator: String,
        value: String,
    },

    /// An `[env]` path value embeds the path separator, naming several
    /// directories that split-based emitters never dedup or remove. Refused at
    /// parse: `ocx exec` and the shell emitters bypass the reconciler's drop.
    #[error(
        "[{scope}] key '{key}': path value {value:?} contains the platform path separator; declare one directory per entry"
    )]
    #[exit(
        DataError,
        slug = "project_env_path_separator_in_value",
        summary = "An ocx.toml path value contains the platform path separator"
    )]
    EnvPathSeparatorInValue { scope: String, key: String, value: String },

    /// An empty `--group` segment (`-g ci,,lint`).
    #[error("empty group name in group filter")]
    #[exit(
        UsageError,
        slug = "empty_group_filter",
        summary = "A group filter names an empty group"
    )]
    EmptyGroupFilter,

    /// A `--group` names an undeclared group.
    #[error("unknown group '{name}'; declare `[group.{name}]` in ocx.toml first")]
    #[exit(
        UsageError,
        slug = "unknown_group",
        summary = "A group filter names a group ocx.toml does not declare"
    )]
    UnknownGroup { name: String },

    /// A `declaration_hash_version` from a newer release, whose hash cannot be compared.
    #[error("unsupported declaration_hash_version {version}; this build understands version 1")]
    #[exit(
        ConfigError,
        slug = "unsupported_declaration_hash_version",
        summary = "ocx.lock records a declaration hash version this build does not understand"
    )]
    UnsupportedDeclarationHashVersion { version: u8 },

    /// The file exceeds the project-tier size cap.
    #[error("file too large: {size} bytes exceeds limit of {limit} bytes")]
    #[exit(
        ConfigError,
        slug = "project_file_too_large",
        summary = "A project file exceeds the allowed size"
    )]
    FileTooLarge { size: u64, limit: u64 },

    /// Another writer still holds the project mutation lock after the
    /// contention budget; distinct from `Io` so callers can retry.
    #[error("ocx.toml is locked by another process")]
    #[exit(
        TempFail,
        slug = "project_locked",
        summary = "Another process holds the lock on ocx.toml"
    )]
    Locked,

    /// A tools value lacks a registry; fully-qualified identifiers keep
    /// resolution independent of `OCX_DEFAULT_REGISTRY`.
    #[error(
        "binding '{name}': value '{value}' is missing a registry; expected 'registry/repo:tag' (e.g. 'ocx.sh/cmake:3.28')"
    )]
    #[exit(
        ConfigError,
        slug = "binding_value_missing_registry",
        summary = "A binding's identifier names no registry"
    )]
    ToolValueMissingRegistry { name: String, value: String },

    /// A tools value is not a valid identifier for another reason.
    #[error("binding '{name}': value '{value}' is not a valid identifier")]
    #[exit(
        ConfigError,
        slug = "binding_value_invalid",
        summary = "A binding's value is not a valid identifier"
    )]
    ToolValueInvalid {
        name: String,
        value: String,
        #[source]
        source: IdentifierError,
    },

    /// A `[package."<key>"]` key lacks a registry, as the `[tools]` rule requires.
    #[error("package key '{key}' is missing a registry; expected 'registry/repo[:tag]' (e.g. 'ocx.sh/cmake')")]
    #[exit(
        ConfigError,
        slug = "package_key_missing_registry",
        summary = "A package key names no registry"
    )]
    PackageKeyMissingRegistry { key: String },

    /// A `[package."<key>"]` key is not a valid identifier for another reason.
    #[error("package key '{key}' is not a valid identifier")]
    #[exit(
        ConfigError,
        slug = "package_key_invalid",
        summary = "A package key is not a valid identifier"
    )]
    PackageKeyInvalid {
        key: String,
        #[source]
        source: IdentifierError,
    },

    /// The tag does not exist on the registry; distinct from
    /// `RegistryUnreachable` so it exits 79, not 69. Named in the message
    /// because an untagged entry defaults to `:latest`.
    #[error(
        "tag '{tag}' not found in '{registry}/{repository}'",
        tag = .identifier.tag_or_latest(),
        registry = .identifier.registry(),
        repository = .identifier.repository(),
    )]
    #[exit(NotFound, slug = "tag_not_found", summary = "The registry has no such tag")]
    TagNotFound { identifier: Box<PackageRef> },

    /// The registry refused authentication (401, 403); not retried.
    #[error("authentication failed for '{identifier}'")]
    #[exit(
        AuthError,
        slug = "registry_auth_failed",
        summary = "The registry rejected the request's authentication"
    )]
    AuthFailure {
        identifier: Box<PackageRef>,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// The registry stayed unreachable through the retry budget.
    #[error("registry unreachable for '{identifier}'")]
    #[exit(with = registry_unreachable, rows((Unavailable, slug = "registry_unreachable", summary = "The registry could not be reached")))]
    RegistryUnreachable {
        identifier: Box<PackageRef>,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// One tool's resolution hit the per-tool timeout; exit 75, since the
    /// request went unanswered and a rerun can succeed.
    #[error("resolve timed out for '{identifier}'")]
    #[exit(TempFail, slug = "resolve_timeout", summary = "Resolving a binding timed out")]
    ResolveTimeout { identifier: Box<PackageRef> },

    /// One binding name in two selected groups with different identifiers.
    #[error("binding '{name}' defined in multiple selected groups: '{group_a}' and '{group_b}'")]
    #[exit(
        UsageError,
        slug = "duplicate_binding_across_groups",
        summary = "A binding is defined in more than one selected group"
    )]
    DuplicateToolAcrossSelectedGroups {
        name: String,
        group_a: String,
        group_b: String,
    },

    /// A `--group` was selected but `ocx.lock` is absent.
    #[error("ocx.lock is missing; run `ocx lock`")]
    #[exit(ConfigError, slug = "lock_missing", summary = "ocx.lock does not exist")]
    LockMissing,

    /// A touched pair passed to [`crate::resolve_lock_touched`] is not declared.
    #[error("binding '{name}' not declared in ocx.toml")]
    #[exit(
        NotFound,
        slug = "binding_not_declared",
        summary = "The named binding is not declared in ocx.toml"
    )]
    ToolNotInConfig { name: String },

    /// The key is taken in the target group by a different identifier; `ocx add`
    /// never silently repoints a declaration. The message names `ocx remove` +
    /// `ocx add` and `ocx update`, not the `NAME=` alias, which answers another question.
    #[error(
        "binding '{name}' in group '{group}' is already bound to '{existing}'; \
         run `ocx remove {name}` then add '{requested}', or `ocx update {name}` to move the pin"
    )]
    #[exit(
        UsageError,
        slug = "binding_already_exists",
        summary = "A binding with this name already exists"
    )]
    BindingAlreadyExists {
        group: String,
        name: String,
        existing: Box<PackageRef>,
        requested: Box<PackageRef>,
    },

    /// An explicit `NAME=` binding name is empty or not a valid key.
    #[error("invalid binding name '{name}': allowed characters are alphanumerics, '.', '_', and '-'")]
    #[exit(
        UsageError,
        slug = "invalid_binding_name",
        summary = "A binding name carries a character outside the allowed set"
    )]
    InvalidBindingName { name: String },

    /// `ocx remove` without `--group` found the binding in several groups.
    #[error("binding '{name}' exists in multiple groups: {groups:?} — pass --group to disambiguate")]
    #[exit(
        UsageError,
        slug = "binding_ambiguous",
        summary = "A binding exists in several groups and no group was named"
    )]
    BindingAmbiguous { name: String, groups: Vec<String> },

    /// `ocx remove` found the binding in no group.
    #[error("binding '{name}' not found in ocx.toml")]
    #[exit(
        NotFound,
        slug = "binding_not_found",
        summary = "The named binding does not exist in ocx.toml"
    )]
    BindingNotFound { name: String },

    /// `ocx init` found an existing `ocx.toml`, which it never overwrites.
    #[error("ocx.toml already exists at '{path}'")]
    #[exit(
        UsageError,
        slug = "project_already_exists",
        summary = "ocx.toml already exists at the target path"
    )]
    ConfigAlreadyExists { path: PathBuf },

    /// A `--group` name is a reserved selector or outside the toolchain charset.
    /// Argv, so a usage error (64); the same name in `ocx.toml` is
    /// [`Self::InvalidToolchainNameCharset`] (78).
    #[error(
        "invalid group name '{name}': must match ^[A-Za-z0-9][A-Za-z0-9._-]*$ and be at most 64 bytes, and must not be a reserved keyword (`default`, `all`)"
    )]
    #[exit(
        UsageError,
        slug = "invalid_group_name",
        summary = "A group name carries a character outside the allowed set"
    )]
    InvalidGroupName { name: String },

    /// The lock no longer describes `ocx.toml`: a moved hash, or one entry out
    /// of step under an unchanged hash. The remedy stays tier-neutral (`--global`
    /// for the global toolchain): this layer does not know the tier.
    #[error(
        "lock is out of sync with ocx.toml ({drift}); run `ocx lock` to reconcile (add `--global` for the global toolchain)"
    )]
    #[exit(DataError, slug = "lock_stale", summary = "ocx.lock does not match ocx.toml")]
    LockOutOfSync { drift: Box<crate::lock::LockDrift> },

    /// `--offline` or `--frozen` refused to resolve an unpinned tag missing from
    /// the local index; not retried. `policy` is `"offline"` or `"frozen"`.
    #[error("{}", .block.message(&.identifier.to_string(), .policy))]
    #[exit(
        PolicyBlocked,
        slug = "project_resolution_blocked",
        summary = "A local policy refused resolving a binding"
    )]
    PolicyBlocked {
        identifier: Box<PackageRef>,
        policy: &'static str,
        /// What the index could not look up.
        block: ocx_index::error::PolicyBlock,
    },
}

/// A transient registry cause answers with its own code and slug; anything else is plain unreachable.
fn registry_unreachable(error: &ProjectErrorKind, [row]: [Row; 1]) -> Pick<'_> {
    if let ProjectErrorKind::RegistryUnreachable { source, .. } = error
        && let Some(client @ ClientError::RegistryTransient(_)) = source.downcast_ref::<ClientError>()
    {
        return Pick::delegate(client);
    }
    Pick::row(row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_oci::package_ref::error::{IdentifierError, IdentifierErrorKind};

    #[test]
    fn error_stays_small_enough_to_return_by_value() {
        // Clippy's `result_large_err` limit is 128 bytes and `PathBuf` is wider on Windows,
        // which only a Windows clippy run would catch; every wrapping tier inherits this size.
        // 104 is what the inline `OciIndex` variant needs; an inline `ProjectError` exceeds it.
        let size = std::mem::size_of::<Error>();
        assert!(size <= 104, "ocx_project::Error is {size} bytes");
    }

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
        let outer = crate::Error::from(ProjectError::new(std::path::PathBuf::from("/tmp/ocx.toml"), kind));
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

    /// `LockOutOfSync` (the `add`/`remove` drift gate, exit 65) must name
    /// the user remedy `ocx lock`, name no internal function, and stay
    /// tier-neutral (spec §4.1): because the error layer has no tier context, a
    /// `ocx --global add/remove` user must be steered to add `--global` rather
    /// than handed a bare project-only `ocx lock` that would target the wrong
    /// toolchain. The pre-mutation hash mismatch on a mutator directs the user
    /// to reconcile the whole file first.
    #[test]
    fn lock_out_of_sync_names_ocx_lock_remedy() {
        let kind = ProjectErrorKind::LockOutOfSync {
            drift: Box::new(crate::lock::LockDrift::DeclarationHash {
                previous_hash: "sha256:aaa".to_string(),
                current_hash: "sha256:bbb".to_string(),
            }),
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

    /// `UnsupportedLockVersion` must name the exact found version and the
    /// `ocx lock` regenerate remedy — the entire migration story for a
    /// rejected lock version (D3: no migration code).
    #[test]
    fn unsupported_lock_version_names_found_version_and_regenerate_remedy() {
        let kind = ProjectErrorKind::UnsupportedLockVersion { found: 2 };
        let rendered = kind.to_string();
        assert!(
            rendered.contains('2'),
            "message must name the found version; got {rendered:?}"
        );
        assert!(
            rendered.contains("`ocx lock`"),
            "message must name the user remedy `ocx lock`; got {rendered:?}"
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

    /// `NoncanonicalPlatformKey` must name the offending key and the
    /// `ocx lock` regenerate remedy.
    #[test]
    fn noncanonical_platform_key_names_key_and_regenerate_remedy() {
        let kind = ProjectErrorKind::NoncanonicalPlatformKey {
            key: "linux/amd64+b,a".to_string(),
        };
        let rendered = kind.to_string();
        assert!(
            rendered.contains("linux/amd64+b,a"),
            "message must name the offending key; got {rendered:?}"
        );
        assert!(
            rendered.contains("`ocx lock`"),
            "message must name the user remedy `ocx lock`; got {rendered:?}"
        );
    }

    /// A-10 names exit 65 for a path value that embeds the platform separator:
    /// the file's shape is legal and the value itself is the malformed datum,
    /// which is the class the wire form already refuses at 65. Pinned against
    /// the `ConfigError` (78) its `[env]` siblings carry, so a later "tidy-up"
    /// that folds it into that arm has to argue with a test.
    #[test]
    fn a_multi_directory_path_value_classifies_as_a_data_error() {
        let err = crate::Error::from(ProjectError::new(
            PathBuf::from("/tmp/ocx.toml"),
            ProjectErrorKind::EnvPathSeparatorInValue {
                scope: "env".to_string(),
                key: "PATH".to_string(),
                value: format!("a{}b", ocx_util::path::PATH_SEPARATOR),
            },
        ));
        let rendered = err.to_string();
        assert!(
            rendered.contains("[env]") && rendered.contains("PATH"),
            "the message must name the scope and key the author has to fix; got {rendered:?}"
        );
    }
}
