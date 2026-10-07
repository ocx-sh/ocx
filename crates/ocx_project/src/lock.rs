// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx.lock` schema. Readers take no lock: writers publish both files by
//! atomic rename, so a concurrent read sees one whole document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use super::error::{ProjectError, ProjectErrorKind};
use super::lazy::LazyMode;
use ocx_oci::{Digest, PackageRef, PinnedPackageRef, Platform, Repository, Selection};

/// The `ocx.lock` beside `config_path`, whatever the config's name.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use ocx_project::lock::lock_path_for;
///
/// let lp = lock_path_for(Path::new("/project/ocx.toml"));
/// assert_eq!(lp, std::path::PathBuf::from("/project/ocx.lock"));
/// ```
pub fn lock_path_for(config_path: &Path) -> PathBuf {
    config_path.parent().unwrap_or_else(|| Path::new(".")).join("ocx.lock")
}

/// Lock format version. `V3` is the only one accepted; older locks are rejected
/// with [`ProjectErrorKind::UnsupportedLockVersion`], never bridged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum LockVersion {
    /// The only version this build reads or writes.
    V3 = 3,
}

// Hand-written: the derive cannot see through `serde_repr`.
impl schemars::JsonSchema for LockVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("LockVersion")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "description": "OCX lock format version (lock-format version 3). Written locks are always 3.",
            "enum": [3]
        })
    }
}

/// Header-only peek reading `lock_version` as a raw `u8`, so an unsupported
/// value is named in the error rather than `serde_repr`'s "unknown variant".
#[derive(Debug, Clone, Deserialize)]
struct LockVersionPeek {
    metadata: LockVersionPeekMetadata,
}

#[derive(Debug, Clone, Deserialize)]
struct LockVersionPeekMetadata {
    lock_version: u8,
}

const SUPPORTED_LOCK_VERSION: u8 = 3;

/// Lock metadata header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockMetadata {
    /// On-disk schema version. `V3` is the only version this build reads or
    /// writes.
    pub lock_version: LockVersion,
    /// Canonicalization contract version for `declaration_hash`. Currently
    /// always `1`.
    pub declaration_hash_version: u8,
    /// `sha256:<hex>` of the RFC 8785 JCS-canonicalized declaration.
    pub declaration_hash: String,
    /// Tooling version string that wrote the lock, e.g. `"ocx 0.3.0"`.
    pub generated_by: String,
    /// ISO-8601 UTC timestamp. Preserved verbatim when the resolved
    /// content (registry, repository, digest) of every tool is unchanged
    /// between two `ocx lock` runs; updated otherwise.
    pub generated_at: String,
}

/// Top-level `ocx.lock` document.
///
/// Tools are sorted by `(group, name)` at write time for byte-stable output.
/// Only `lock_version` `3` is accepted, every tool's `repository` must be bare
/// and every platform key canonical.
// The version check runs before the full parse, so a foreign version never surfaces as a shape error.
#[derive(Debug, Clone, PartialEq, Eq, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectLock {
    /// Lock metadata header (version, declaration hash, generator, etc.).
    pub metadata: LockMetadata,

    /// All locked tools in the project. Sorted by (group, name) at write
    /// time for byte-stable output. On disk this is the `[[tool]]` array.
    #[serde(default, rename = "tool")]
    pub tools: Vec<LockedTool>,
}

/// One locked tool entry, identified by the local binding `(group, name)`.
///
/// `name` is the key in the developer's `ocx.toml`; `group` is `"default"` for
/// the top-level `[tools]` table, else the `[group.<name>]` key. `repository`
/// is the bare registry/repo coordinate (no tag, no digest) shared by every
/// platform leaf. `platforms` records one leaf digest per shipped platform,
/// keyed by the canonical platform string; a platform the publisher does not
/// ship has no key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedTool {
    /// Local binding name (TOML key from `ocx.toml`, e.g. `cmake`).
    pub name: String,
    /// Owning group. `"default"` for entries from the top-level
    /// `[tools]` table; otherwise the named `[group.*]` key.
    pub group: String,
    /// Bare registry/repo coordinates shared by every platform leaf.
    #[schemars(with = "PackageRef")]
    pub repository: Repository,
    /// Available-only per-platform leaf digests, keyed by the canonical
    /// platform string (`os/arch[/variant][+feature,...]` or `any`).
    // `BTreeMap<String, _>`: byte-stable output without `Ord` on `Platform`.
    pub platforms: BTreeMap<String, Digest>,
}

impl LockedTool {
    /// The leaf digest this entry pins for `platform`, via
    /// [`crate::resolve::lookup_host_leaf`].
    ///
    /// # Errors
    ///
    /// [`ProjectErrorKind::NoHostLeaf`] when no leaf fits the host;
    /// [`ProjectErrorKind::AmbiguousHostLeaf`] when two tie. Kept distinct: the remedies differ.
    pub fn host_leaf(&self, platform: &Platform) -> Result<Digest, super::Error> {
        match super::resolve::lookup_host_leaf(&self.platforms, platform) {
            Selection::Found((leaf, _key)) => Ok(leaf.clone()),
            Selection::None => Err(ProjectError::new(
                PathBuf::new(),
                ProjectErrorKind::NoHostLeaf {
                    name: self.name.clone(),
                    platform: platform.to_string(),
                },
            )
            .into()),
            Selection::Ambiguous(candidates) => Err(ProjectError::new(
                PathBuf::new(),
                ProjectErrorKind::AmbiguousHostLeaf {
                    name: self.name.clone(),
                    platform: platform.to_string(),
                    candidates: candidates.into_iter().map(|(_, key)| key.to_string()).collect(),
                },
            )
            .into()),
        }
    }
}

/// On-disk mirror of [`ProjectLock`], parsed before the bare-repository check.
// `Repository`'s own `Deserialize` would surface a tagged value as `TomlParse`
// instead of the typed `LockRepositoryNotBare` (exit 78).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProjectLock {
    metadata: LockMetadata,
    #[serde(default, rename = "tool")]
    tools: Vec<RawLockedTool>,
}

/// On-disk mirror of [`LockedTool`], with `repository` still untyped.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLockedTool {
    name: String,
    group: String,
    repository: PackageRef,
    platforms: BTreeMap<String, Digest>,
}

impl RawProjectLock {
    /// # Errors
    ///
    /// [`ProjectErrorKind::LockRepositoryNotBare`] for a tagged or digested `repository`.
    fn into_lock(self, path: &Path) -> Result<ProjectLock, super::Error> {
        let tools = self
            .tools
            .into_iter()
            .map(|raw| {
                if raw.repository.tag().is_some() || raw.repository.digest().is_some() {
                    return Err(ProjectError::new(
                        path.to_path_buf(),
                        ProjectErrorKind::LockRepositoryNotBare {
                            value: raw.repository.to_string(),
                        },
                    )
                    .into());
                }
                Ok(LockedTool {
                    name: raw.name,
                    group: raw.group,
                    repository: Repository::from(&raw.repository),
                    platforms: raw.platforms,
                })
            })
            .collect::<Result<Vec<_>, super::Error>>()?;
        Ok(ProjectLock {
            metadata: self.metadata,
            tools,
        })
    }
}

/// A [`ProjectLock`] joined to the `ocx.toml` it was locked from, by
/// [`ProjectLock::bind`].
#[derive(Debug, Clone)]
pub enum Binding {
    /// Every lock entry paired with its declaration, in lock order.
    Current(Vec<BoundTool>),
    /// The lock no longer describes the declarations.
    Stale(Box<LockDrift>),
}

/// Why a [`ProjectLock`] no longer binds to its `ocx.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockDrift {
    /// `ocx.toml` changed since the lock was written.
    DeclarationHash {
        previous_hash: String,
        current_hash: String,
    },
    /// One entry's declaration is gone or names another repository, under an
    /// unchanged hash. `declared` is `None` when `ocx.toml` no longer declares it.
    Entry {
        group: String,
        name: String,
        locked: Repository,
        declared: Option<Repository>,
    },
}

impl std::fmt::Display for LockDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeclarationHash {
                previous_hash,
                current_hash,
            } => write!(f, "declaration_hash {current_hash} != locked {previous_hash}"),
            Self::Entry {
                group,
                name,
                locked,
                declared: Some(declared),
            } => write!(
                f,
                "binding '{name}' in group '{group}' is locked from {locked} but declared as {declared}"
            ),
            Self::Entry {
                group,
                name,
                locked,
                declared: None,
            } => write!(
                f,
                "binding '{name}' in group '{group}' is locked from {locked} but no longer declared"
            ),
        }
    }
}

/// One [`LockedTool`] paired with the identifier `ocx.toml` declares for its
/// `(group, name)`. Only [`ProjectLock::bind`] builds one, so a tag carried onto
/// a lock digest always comes from the declaration the lock was made from.
#[derive(Debug, Clone)]
pub struct BoundTool {
    locked: LockedTool,
    declared: PackageRef,
}

impl BoundTool {
    pub fn locked(&self) -> &LockedTool {
        &self.locked
    }

    /// The full declaration, a declared digest included.
    pub fn declared(&self) -> &PackageRef {
        &self.declared
    }

    /// The declaration's tag; `None` for a digest-only declaration.
    pub fn advisory_tag(&self) -> Option<&str> {
        self.declared.tag()
    }

    /// `registry/repository[:tag]@<leaf>` for the host leaf of `platform`; a
    /// declared digest yields to the lock's leaf.
    ///
    /// # Errors
    ///
    /// [`ProjectErrorKind::NoHostLeaf`] when no leaf fits the host;
    /// [`ProjectErrorKind::AmbiguousHostLeaf`] when two tie. Kept distinct: the remedies differ.
    pub fn host_leaf_identifier(&self, platform: &Platform) -> Result<PinnedPackageRef, super::Error> {
        tagged_host_leaf(&self.locked, &self.declared, platform)
    }
}

/// `declared` with its digest replaced by the lock's host leaf.
fn tagged_host_leaf(
    locked: &LockedTool,
    declared: &PackageRef,
    platform: &Platform,
) -> Result<PinnedPackageRef, super::Error> {
    // `declarations` admits only a declaration naming the lock's repository.
    Ok(PinnedPackageRef::pin(declared, locked.host_leaf(platform)?))
}

/// One entry per distinct content (registry, repository, digest), in input
/// order: a later tag on content already kept is dropped, so the first tag wins.
pub fn first_per_content<T>(entries: impl IntoIterator<Item = (T, PinnedPackageRef)>) -> Vec<(T, PinnedPackageRef)> {
    merge_per_content(entries, |_, _| {})
}

/// [`first_per_content`], except the kept entry is eager if any entry of its content is.
///
/// Order-independent on purpose: first-wins would let lock order (group names) decide
/// whether a tool another group wants eager gets pre-warmed at all.
pub fn eager_per_content(
    entries: impl IntoIterator<Item = (LazyMode, PinnedPackageRef)>,
) -> Vec<(LazyMode, PinnedPackageRef)> {
    merge_per_content(entries, |kept, mode| {
        if mode == LazyMode::Never {
            *kept = mode;
        }
    })
}

/// One entry per distinct content, in input order; `merge` folds each later item into the kept one.
fn merge_per_content<T>(
    entries: impl IntoIterator<Item = (T, PinnedPackageRef)>,
    mut merge: impl FnMut(&mut T, T),
) -> Vec<(T, PinnedPackageRef)> {
    let mut kept: Vec<(T, PinnedPackageRef)> = Vec::new();
    for (item, identifier) in entries {
        // ponytail: O(n²) over a handful of tools; a HashSet buys nothing at this scale.
        match kept.iter_mut().find(|(_, known)| known.eq_content(&identifier)) {
            Some((kept_item, _)) => merge(kept_item, item),
            None => kept.push((item, identifier)),
        }
    }
    kept
}

impl ProjectLock {
    /// Join every entry to its `ocx.toml` declaration by `(group, name)`. Pure;
    /// never errors — a lock that cannot be joined is [`Binding::Stale`].
    pub fn bind(&self, config: &crate::ProjectConfig) -> Binding {
        match self.declarations(config) {
            Ok(pairs) => Binding::Current(
                pairs
                    .into_iter()
                    .map(|(locked, declared)| BoundTool {
                        locked: locked.clone(),
                        declared,
                    })
                    .collect(),
            ),
            Err(drift) => Binding::Stale(drift),
        }
    }

    /// [`Self::bind`], refusing a stale lock.
    ///
    /// # Errors
    ///
    /// [`ProjectErrorKind::LockOutOfSync`] naming the drift.
    pub fn bind_current(&self, config: &crate::ProjectConfig) -> Result<Vec<BoundTool>, super::Error> {
        match self.bind(config) {
            Binding::Current(bound) => Ok(bound),
            Binding::Stale(drift) => {
                Err(ProjectError::new(PathBuf::new(), ProjectErrorKind::LockOutOfSync { drift }).into())
            }
        }
    }

    /// Whether this lock binds to `config`: [`Self::bind`] would answer
    /// [`Binding::Current`]. The one currency test; allocation-free, as it runs every prompt.
    #[must_use]
    pub fn is_current(&self, config: &crate::ProjectConfig) -> bool {
        self.metadata.declaration_hash == config.declaration_hash_cached()
            && self.tools.iter().all(|tool| declaration_of(config, tool).is_some())
    }

    /// Each entry, in lock order, with its declaration; the first drift otherwise.
    fn declarations(&self, config: &crate::ProjectConfig) -> Result<Vec<(&LockedTool, PackageRef)>, Box<LockDrift>> {
        let current_hash = config.declaration_hash_cached();
        if self.metadata.declaration_hash != current_hash {
            return Err(Box::new(LockDrift::DeclarationHash {
                previous_hash: self.metadata.declaration_hash.clone(),
                current_hash: current_hash.to_string(),
            }));
        }
        self.tools
            .iter()
            .map(|tool| match declaration_of(config, tool) {
                Some(declared) => Ok((tool, declared.clone())),
                None => Err(Box::new(LockDrift::Entry {
                    group: tool.group.clone(),
                    name: tool.name.clone(),
                    locked: tool.repository.clone(),
                    declared: super::resolve::declared_identifier(config, &tool.group, &tool.name)
                        .map(Repository::from),
                })),
            })
            .collect()
    }

    /// Every entry with its host identifier for `platform`, for callers that
    /// never refuse a stale lock: tagged from `config` while the lock binds to
    /// it, untagged otherwise, so a new tag never pairs with an old digest.
    /// `config` is `None` when the declarations could not be read.
    pub fn lenient_host_identifiers(
        &self,
        config: Option<&crate::ProjectConfig>,
        platform: &Platform,
    ) -> Vec<(&LockedTool, Result<PinnedPackageRef, super::Error>)> {
        match config.and_then(|config| self.declarations(config).ok()) {
            Some(pairs) => pairs
                .into_iter()
                .map(|(tool, declared)| (tool, tagged_host_leaf(tool, &declared, platform)))
                .collect(),
            None => self
                .tools
                .iter()
                .map(|tool| {
                    (
                        tool,
                        tool.host_leaf(platform).map(|leaf| tool.repository.pin_untagged(leaf)),
                    )
                })
                .collect(),
        }
    }
}

/// `tool`'s declaration, when it names the repository `tool` was locked from.
fn declaration_of<'c>(config: &'c crate::ProjectConfig, tool: &LockedTool) -> Option<&'c PackageRef> {
    super::resolve::declared_identifier(config, &tool.group, &tool.name).filter(|declared| {
        declared.registry() == tool.repository.registry() && declared.repository() == tool.repository.repository()
    })
}

/// Content equality ignoring `name`/`group`: `repository` and the full
/// `platforms` map.
pub fn locked_tool_content_equal(left: &LockedTool, right: &LockedTool) -> bool {
    left.repository == right.repository && left.platforms == right.platforms
}

/// Every key in `platforms` must be the canonical spelling of the [`Platform`]
/// it parses to; with an injective `Display` this also enforces uniqueness.
pub(super) fn validate_canonical_platform_keys(platforms: &BTreeMap<String, Digest>) -> Result<(), ProjectErrorKind> {
    for key in platforms.keys() {
        let parsed: Platform = key
            .parse()
            .map_err(|_| ProjectErrorKind::NoncanonicalPlatformKey { key: key.clone() })?;
        if &parsed.to_string() != key {
            return Err(ProjectErrorKind::NoncanonicalPlatformKey { key: key.clone() });
        }
    }
    Ok(())
}

impl ProjectLock {
    /// Parse a [`ProjectLock`] from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, super::Error> {
        Self::from_str_with_path(s, PathBuf::new())
    }

    /// Load from `path`; a missing file is an error (see [`Self::from_path`]).
    pub async fn load(path: &Path) -> Result<Self, super::Error> {
        use tokio::io::AsyncReadExt;
        let limit = super::internal::FILE_SIZE_LIMIT_BYTES;

        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        // `metadata.len()` fast-paths oversized files; the bounded `take` guards
        // procfs and pipes, whose metadata reports 0.
        let metadata = file
            .metadata()
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        if metadata.len() > limit {
            return Err(ProjectError::new(
                path.to_path_buf(),
                ProjectErrorKind::FileTooLarge {
                    size: metadata.len(),
                    limit,
                },
            )
            .into());
        }

        let mut content = String::new();
        let mut taken = file.take(limit + 1);
        taken
            .read_to_string(&mut content)
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        if content.len() as u64 > limit {
            return Err(ProjectError::new(
                path.to_path_buf(),
                ProjectErrorKind::FileTooLarge {
                    size: content.len() as u64,
                    limit,
                },
            )
            .into());
        }
        Self::from_str_with_path(&content, path.to_path_buf())
    }

    /// [`Self::load`], with `Ok(None)` for a missing file.
    pub async fn from_path(path: &Path) -> Result<Option<Self>, super::Error> {
        match Self::load(path).await {
            Ok(lock) => Ok(Some(lock)),
            Err(super::Error::Project(pe)) if matches!(&pe.kind, ProjectErrorKind::Io(io) if io.kind() == std::io::ErrorKind::NotFound) => {
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }

    /// Rejects a `lock_version` other than [`SUPPORTED_LOCK_VERSION`] before the
    /// full parse, naming the found version, then validates.
    fn from_str_with_path(s: &str, path: PathBuf) -> Result<Self, super::Error> {
        let peek: LockVersionPeek =
            toml::from_str(s).map_err(|e| ProjectError::new(path.clone(), ProjectErrorKind::TomlParse(e)))?;
        if peek.metadata.lock_version != SUPPORTED_LOCK_VERSION {
            return Err(ProjectError::new(
                path,
                ProjectErrorKind::UnsupportedLockVersion {
                    found: peek.metadata.lock_version,
                },
            )
            .into());
        }

        let raw: RawProjectLock =
            toml::from_str(s).map_err(|e| ProjectError::new(path.clone(), ProjectErrorKind::TomlParse(e)))?;
        let lock = raw.into_lock(&path)?;

        // `toml::from_str` already rejects duplicate platform keys and malformed digests.
        lock.validate(&path)?;

        Ok(lock)
    }

    /// V3 invariants beyond parsing: hash version and canonical platform keys
    /// (`repository` is bare by type). Runs on write too, or a hand-assembled
    /// lock is saved in a shape every later load rejects.
    fn validate(&self, path: &Path) -> Result<(), super::Error> {
        // A hash from another algorithm version would compare wrong, so refuse it.
        if self.metadata.declaration_hash_version != super::hash::DECLARATION_HASH_VERSION {
            return Err(ProjectError::new(
                path.to_path_buf(),
                ProjectErrorKind::UnsupportedDeclarationHashVersion {
                    version: self.metadata.declaration_hash_version,
                },
            )
            .into());
        }

        for tool in &self.tools {
            validate_canonical_platform_keys(&tool.platforms)
                .map_err(|kind| ProjectError::new(path.to_path_buf(), kind))?;
        }

        Ok(())
    }

    /// Serialize to TOML, deterministically: tools sorted by `(group, name)`.
    pub fn to_toml_string(&self) -> Result<String, super::Error> {
        self.validate(Path::new(""))?;

        let mut sorted_refs: Vec<&LockedTool> = self.tools.iter().collect();
        sorted_refs.sort_by(|a, b| (a.group.as_str(), a.name.as_str()).cmp(&(b.group.as_str(), b.name.as_str())));

        let view = SerializableView {
            metadata: &self.metadata,
            tools: &sorted_refs,
        };

        toml::to_string_pretty(&view)
            .map_err(|e| ProjectError::new(PathBuf::new(), ProjectErrorKind::TomlSerialize(e)).into())
    }

    /// Atomic save, then best-effort GC-ledger registration of `config_path`'s
    /// project. `generated_at` is kept from `previous` when no tool's content changed.
    pub async fn save(
        &self,
        path: &Path,
        previous: Option<&Self>,
        ocx_home: &Path,
        config_path: &Path,
    ) -> Result<(), super::Error> {
        let mut to_write = self.clone();
        if let Some(prev) = previous {
            if tools_content_equal(&to_write.tools, &prev.tools) {
                to_write.metadata.generated_at = prev.metadata.generated_at.clone();
            } else if to_write.metadata.generated_at <= prev.metadata.generated_at {
                // Changed within the same second: bump so diff tools see the change.
                to_write.metadata.generated_at = bump_timestamp_one_second(&prev.metadata.generated_at)
                    .unwrap_or_else(|| to_write.metadata.generated_at.clone());
            }
        }

        let serialized = to_write.to_toml_string()?;

        crate::mutate::publish_by_rename_async(path, serialized.into_bytes()).await?;

        super::registry::register_project_dir_best_effort(config_path, ocx_home).await;

        Ok(())
    }
}

/// Restore a captured `ocx.lock` byte-for-byte, crash-durably, for
/// [`MutationGuard`](super::mutation::MutationGuard) rollback.
///
/// # Errors
///
/// The atomic write's I/O error.
pub async fn restore_lock_bytes_verbatim(path: &Path, bytes: Vec<u8>) -> Result<(), super::Error> {
    crate::mutate::publish_by_rename_async(path, bytes).await
}

/// Borrowed, pre-sorted view for [`ProjectLock::to_toml_string`], avoiding a
/// whole-document clone.
#[derive(Serialize)]
struct SerializableView<'a> {
    metadata: &'a LockMetadata,
    #[serde(rename = "tool")]
    tools: &'a [&'a LockedTool],
}

/// `iso` plus one second in `%Y-%m-%dT%H:%M:%SZ`; `None` if unparseable, so
/// callers keep their own timestamp.
fn bump_timestamp_one_second(iso: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(iso).ok()?;
    let bumped = parsed.checked_add_signed(chrono::Duration::seconds(1))?;
    Some(
        bumped
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

/// Order-independent equality of two tool lists, `name`/`group` included; a
/// platform added or dropped counts as changed.
fn tools_content_equal(a: &[LockedTool], b: &[LockedTool]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let sort_key = |t: &&LockedTool| (t.group.clone(), t.name.clone());
    let mut a_sorted: Vec<&LockedTool> = a.iter().collect();
    let mut b_sorted: Vec<&LockedTool> = b.iter().collect();
    a_sorted.sort_by_key(sort_key);
    b_sorted.sort_by_key(sort_key);

    a_sorted.iter().zip(b_sorted.iter()).all(|(left, right)| {
        left.name == right.name && left.group == right.group && locked_tool_content_equal(left, right)
    })
}

/// Why the `ocx.lock` beside an `ocx.toml` cannot be used. Shared by the
/// command prologue and the per-prompt reconciler, so both print one sentence
/// and one exit code.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
pub enum LockCurrency {
    /// `ocx.toml` resolved but its sibling `ocx.lock` is absent.
    #[error("ocx.lock not found at {path}; run `ocx lock` to create it")]
    #[exit(ConfigError, slug = "lock_missing", summary = "ocx.lock does not exist")]
    Missing {
        /// The `ocx.lock` that was looked for.
        path: PathBuf,
    },

    /// `ocx.lock` exists but does not bind to `ocx.toml`: a moved
    /// `declaration_hash`, or an entry whose repository is not the declared one.
    #[error("ocx.lock is stale (it does not match ocx.toml); run `ocx lock`")]
    #[exit(DataError, slug = "lock_stale", summary = "ocx.lock does not match ocx.toml")]
    Stale {
        /// The stale lock.
        lock_path: PathBuf,
    },
}

#[cfg(test)]
mod tests {
    //! Contract-first tests for [`ProjectLock`] parsing, serialization, and
    //! atomic save semantics.
    //!
    //! Assertions prefer typed
    //! [`super::super::error::ProjectErrorKind`] matches over string matches.
    //! Determinism is a permanent contract.
    use super::*;
    use crate::error::ProjectErrorKind;
    use ocx_oci::Digest;

    /// Assert an [`Error`] carries a specific [`ProjectErrorKind`]
    /// pattern. Uses `let else` on the inner kind (not exhaustive
    /// `match`) because [`ProjectErrorKind`] is `#[non_exhaustive]`:
    /// an exhaustive match breaks the moment a new variant lands,
    /// producing a confusing "non-exhaustive patterns" error from the
    /// test macro rather than the actual test failure. The `let else`
    /// shape surfaces the real mismatch directly.
    ///
    /// The outer `Error::Project(pe) = err` destructure is irrefutable
    /// within this crate (`Error` currently has one variant) but the
    /// `#[non_exhaustive]` annotation means future additions won't
    /// silently break this macro — `let else` is forward-compatible,
    /// the warning suppressed below only fires while the enum is
    /// single-variant in-crate.
    #[allow(irrefutable_let_patterns)]
    macro_rules! assert_kind {
        ($err:expr, $pat:pat) => {{
            let err = $err;
            #[allow(irrefutable_let_patterns)]
            let crate::Error::Project(pe) = err else {
                panic!("expected Error::Project, got {err:?}");
            };
            let kind = &pe.kind;
            let $pat = kind else {
                panic!("unexpected error kind: {kind:?}");
            };
        }};
    }

    // --- Helpers ------------------------------------------------------------

    /// Construct a 64-hex sha256 digest filled with the given byte.
    fn sha256_of(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    /// Construct a 64-hex sha256 [`Digest`] filled with the given byte.
    fn digest_of(byte: char) -> Digest {
        Digest::Sha256(sha256_of(byte))
    }

    /// Construct the bare `registry/repo` [`Repository`] coordinate.
    fn bare_repo(registry: &str, repo: &str) -> Repository {
        Repository::new(registry, repo)
    }

    /// Build a [`LockedTool`] pinning `default/<name>` to one
    /// `linux/amd64` leaf digest.
    fn locked_tool(name: &str, group: &str, registry: &str, repo: &str, leaf_byte: char) -> LockedTool {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), digest_of(leaf_byte));
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            repository: bare_repo(registry, repo),
            platforms,
        }
    }

    /// Stage `<lock_dir>/ocx.toml` (so the registry's canonicalize step
    /// finds a real file) and return the path. Tests that exercise
    /// `ProjectLock::save` need to pass a `config_path` that exists, but
    /// they don't care whether registration succeeds — the save itself
    /// is the contract under test.
    fn stage_sibling_ocx_toml(lock_path: &std::path::Path) -> std::path::PathBuf {
        let parent = lock_path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let cfg = parent.join("ocx.toml");
        if !cfg.exists() {
            std::fs::write(&cfg, "[tools]\n").expect("seed ocx.toml for save() test");
        }
        cfg
    }

    fn sample_metadata() -> LockMetadata {
        LockMetadata {
            lock_version: LockVersion::V3,
            declaration_hash_version: 1,
            declaration_hash: format!("sha256:{}", sha256_of('d')),
            generated_by: "ocx 0.3.0".to_string(),
            generated_at: "2026-04-19T00:00:00Z".to_string(),
        }
    }

    // --- Fixtures -----------------------------------------------------------

    /// A V3 on-disk fixture: one tool, bare `repository`, two shipped
    /// platform leaves keyed by their canonical grammar key. `windows/amd64`
    /// is deliberately absent (publisher ships no such leaf).
    fn v3_lock_toml() -> String {
        format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"

[tool.platforms]
"linux/amd64" = "sha256:{amd64}"
"darwin/arm64" = "sha256:{arm64}"
"#,
            cafe = sha256_of('c'),
            amd64 = sha256_of('1'),
            arm64 = sha256_of('2'),
        )
    }

    // --- The staleness gate --------------------------------------------------

    /// The hash half of the one currency predicate, both answers on a config
    /// the test owns: a test that only saw one could not tell it from a constant.
    #[tokio::test]
    async fn f011_the_staleness_gate_answers_both_ways_on_one_config() {
        use crate::ProjectConfig;

        let temp = tempfile::tempdir().expect("tempdir");
        let config_path = temp.path().join("ocx.toml");
        std::fs::write(&config_path, "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n").expect("write ocx.toml");
        let config = ProjectConfig::from_path(&config_path).await.expect("parse ocx.toml");

        let mut lock = ProjectLock::from_toml_str(&v3_lock_toml()).expect("parse lock");

        lock.metadata.declaration_hash = config.declaration_hash_cached().to_owned();
        assert!(
            lock.is_current(&config),
            "a lock whose recorded hash is the config's own is current"
        );

        lock.metadata.declaration_hash = format!("sha256:{}", sha256_of('e'));
        assert!(
            !lock.is_current(&config),
            "a lock recording a different hash is stale: `ocx.toml` moved on without it"
        );
    }

    // --- Version rejection ---------------------------------------------------

    #[test]
    fn read_rejects_v1() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"
"#,
            abc = sha256_of('a'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("V1 lock must reject");
        assert_kind!(err, ProjectErrorKind::UnsupportedLockVersion { found: 1 });
    }

    #[test]
    fn read_rejects_v2() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 2
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"
"#,
            abc = sha256_of('a'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("V2 lock must reject");
        assert_kind!(err, ProjectErrorKind::UnsupportedLockVersion { found: 2 });
    }

    #[test]
    fn read_rejects_unknown_future_version() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 4
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 99.0.0"
generated_at = "2099-01-01T00:00:00Z"
"#,
            abc = sha256_of('a'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("unknown future version must reject");
        assert_kind!(err, ProjectErrorKind::UnsupportedLockVersion { found: 4 });
    }

    #[test]
    fn read_rejects_unsupported_version_with_regenerate_hint() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"
"#,
            abc = sha256_of('a'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("V1 lock must reject");
        assert!(
            err.to_string().contains("ocx lock"),
            "message must name the `ocx lock` regenerate remedy; got {err}"
        );
    }

    // --- Parsing --------------------------------------------------------------

    #[test]
    fn parse_empty_v3_lock_ok() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"
"#,
            abc = sha256_of('a'),
        );
        let lock = ProjectLock::from_toml_str(&toml_str).expect("empty V3 lock parses");
        assert_eq!(lock.metadata.lock_version, LockVersion::V3);
        assert!(lock.tools.is_empty());
    }

    #[test]
    fn parse_full_v3_lock_ok() {
        let lock = ProjectLock::from_toml_str(&v3_lock_toml()).expect("V3 lock parses");
        assert_eq!(lock.tools.len(), 1);
        let cmake = &lock.tools[0];
        assert_eq!(cmake.name, "cmake");
        assert_eq!(cmake.group, "default");
        assert_eq!(cmake.repository.registry(), "ocx.sh");
        assert_eq!(cmake.repository.repository(), "cmake");
        assert_eq!(cmake.platforms.get("linux/amd64"), Some(&digest_of('1')));
        assert_eq!(cmake.platforms.get("darwin/arm64"), Some(&digest_of('2')));
        assert!(
            !cmake.platforms.contains_key("windows/amd64"),
            "an unshipped platform must be absent from the map"
        );
    }

    #[test]
    fn parse_unknown_top_level_field_rejects() {
        let abc = sha256_of('a');
        let toml_str = format!(
            r#"
unknown = "key"

[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"
"#
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("unknown top-level must reject");
        assert_kind!(err, ProjectErrorKind::TomlParse(_));
    }

    #[test]
    fn load_rejects_future_hash_version() {
        // The canonicalization contract version is the one version gate
        // the parser enforces itself (serde doesn't — it's a plain u8).
        // A lock file carrying `declaration_hash_version = 2` must be
        // rejected with a dedicated [`ProjectErrorKind::UnsupportedDeclarationHashVersion`]
        // rather than silently being compared against a hash this build
        // computes with version 1 semantics.
        let abc = sha256_of('a');
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 2
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.99.0"
generated_at = "2099-01-01T00:00:00Z"
"#
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("future hash version must reject");
        assert_kind!(err, ProjectErrorKind::UnsupportedDeclarationHashVersion { version: 2 });
    }

    #[test]
    fn load_rejects_empty_lock_file() {
        // Empty content must produce a parse-class error, not a
        // successful load of a default-initialized struct. `metadata`
        // is a required table — its absence trips `missing field
        // `metadata`` at the serde layer.
        let err = ProjectLock::from_toml_str("").expect_err("empty lock must reject");
        assert_kind!(err, ProjectErrorKind::TomlParse(_));
    }

    #[test]
    fn load_rejects_whitespace_only_lock_file() {
        // Whitespace-only content: same contract as empty. Spaces,
        // tabs, and newlines must not be treated as "default empty
        // lock".
        let err = ProjectLock::from_toml_str("   \n\t  \n").expect_err("whitespace must reject");
        assert_kind!(err, ProjectErrorKind::TomlParse(_));
    }

    // --- Canonical-key validation (D3) -----------------------------------

    /// An unsorted `os_features` list (`+b,a` instead of the canonical
    /// `+a,b`) is a noncanonical spelling of a valid `Platform` — rejected,
    /// never silently normalized.
    #[test]
    fn read_rejects_unsorted_feature_list_key() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"

[tool.platforms]
"linux/amd64+b,a" = "sha256:{leaf}"
"#,
            cafe = sha256_of('c'),
            leaf = sha256_of('1'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("unsorted feature-list key must reject");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::NoncanonicalPlatformKey { key } = &pe.kind else {
            panic!("expected NoncanonicalPlatformKey, got {:?}", pe.kind);
        };
        assert_eq!(key, "linux/amd64+b,a");
    }

    /// A wasm platform key is canonical and survives a load/write round
    /// trip — the lock accepts every pairing `SUPPORTED_PAIRS` names, not
    /// just the native ones.
    #[test]
    fn read_accepts_wasm_platform_key() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "wasm-tool"
group = "default"
repository = "ocx.sh/wasm-tool"

[tool.platforms]
"wasip1/wasm" = "sha256:{leaf}"
"#,
            cafe = sha256_of('c'),
            leaf = sha256_of('1'),
        );
        let lock = ProjectLock::from_toml_str(&toml_str).expect("wasip1/wasm is a canonical key");
        let tool = lock.tools.first().expect("one tool");
        assert_eq!(tool.platforms.get("wasip1/wasm"), Some(&digest_of('1')));
        let written = lock
            .to_toml_string()
            .expect("canonical key must survive the write gate");
        assert!(
            written.contains("\"wasip1/wasm\""),
            "wasm key must round-trip verbatim: {written}"
        );
    }

    /// A redundant duplicate feature (`+a,a` instead of the canonical `+a`)
    /// is likewise a noncanonical spelling — rejected.
    #[test]
    fn read_rejects_duplicate_feature_key() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"

[tool.platforms]
"linux/amd64+a,a" = "sha256:{leaf}"
"#,
            cafe = sha256_of('c'),
            leaf = sha256_of('1'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("duplicate-feature key must reject");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::NoncanonicalPlatformKey { key } = &pe.kind else {
            panic!("expected NoncanonicalPlatformKey, got {:?}", pe.kind);
        };
        assert_eq!(key, "linux/amd64+a,a");
    }

    /// A key that fails to parse as a `Platform` at all is also
    /// `NoncanonicalPlatformKey`, not a bare `TomlParse`.
    #[test]
    fn read_rejects_unparseable_platform_key() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"

[tool.platforms]
"not-a-platform" = "sha256:{leaf}"
"#,
            cafe = sha256_of('c'),
            leaf = sha256_of('1'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("unparseable key must reject");
        assert_kind!(err, ProjectErrorKind::NoncanonicalPlatformKey { .. });
    }

    /// R6: two distinct platform keys mapping to the SAME digest (the
    /// Rosetta 2 case — a publisher pushes the `darwin/amd64` binary under
    /// `darwin/arm64` too) is legitimate and must load + resolve normally.
    /// Only key-level canonical-platform uniqueness is enforced, never
    /// digest-value uniqueness.
    #[test]
    fn read_accepts_shared_digest_aliasing_across_distinct_platform_keys() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"

[tool.platforms]
"darwin/amd64" = "sha256:{shared}"
"darwin/arm64" = "sha256:{shared}"
"#,
            cafe = sha256_of('c'),
            shared = sha256_of('1'),
        );
        let lock = ProjectLock::from_toml_str(&toml_str).expect("shared-digest aliasing must load normally");
        let platforms = &lock.tools[0].platforms;
        assert_eq!(platforms.get("darwin/amd64"), Some(&digest_of('1')));
        assert_eq!(platforms.get("darwin/arm64"), Some(&digest_of('1')));
    }

    // --- Serialization + determinism ---------------------------------------

    #[test]
    fn roundtrip_deterministic() {
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", '1')],
        };
        let first = lock.to_toml_string().expect("first serialization");
        let reparsed = ProjectLock::from_toml_str(&first).expect("first reparse");
        let second = reparsed.to_toml_string().expect("second serialization");
        assert_eq!(first, second, "second pass must be byte-identical");
    }

    /// Idempotent re-lock must be byte-identical: serializing the same
    /// content twice (with the same `generated_at`) yields identical bytes.
    /// This is the cross-OS reproducibility + "no `generated_at` churn"
    /// contract (ADR Validation: "an unchanged re-lock is byte-identical").
    #[test]
    fn idempotent_relock_is_byte_identical() {
        let build = || ProjectLock {
            metadata: sample_metadata(),
            tools: vec![
                locked_tool("cmake", "default", "ocx.sh", "cmake", '1'),
                locked_tool("ninja", "default", "ocx.sh", "ninja", '2'),
            ],
        };
        let first = build().to_toml_string().expect("first serialization");
        let second = build().to_toml_string().expect("second serialization");
        assert_eq!(first, second, "two identical locks must serialize byte-identically");
    }

    // --- Canonical-key validation (D3), write site ---------------------

    /// F5: `to_toml_string` runs the same canonicalization check as load —
    /// an unsorted `os_features` list built directly in memory (bypassing
    /// any parser) must still be rejected at write time.
    #[test]
    fn write_rejects_unsorted_feature_list_key() {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64+b,a".to_string(), digest_of('1'));
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![LockedTool {
                name: "cmake".to_string(),
                group: "default".to_string(),
                repository: bare_repo("ocx.sh", "cmake"),
                platforms,
            }],
        };
        let err = lock
            .to_toml_string()
            .expect_err("unsorted feature-list key must reject at write");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::NoncanonicalPlatformKey { key } = &pe.kind else {
            panic!("expected NoncanonicalPlatformKey, got {:?}", pe.kind);
        };
        assert_eq!(key, "linux/amd64+b,a");
    }

    /// F5: a redundant duplicate feature (`+a,a`) is likewise noncanonical
    /// and must be rejected at write time, not just on load.
    #[test]
    fn write_rejects_duplicate_feature_key() {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64+a,a".to_string(), digest_of('1'));
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![LockedTool {
                name: "cmake".to_string(),
                group: "default".to_string(),
                repository: bare_repo("ocx.sh", "cmake"),
                platforms,
            }],
        };
        let err = lock
            .to_toml_string()
            .expect_err("duplicate-feature key must reject at write");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::NoncanonicalPlatformKey { key } = &pe.kind else {
            panic!("expected NoncanonicalPlatformKey, got {:?}", pe.kind);
        };
        assert_eq!(key, "linux/amd64+a,a");
    }

    // --- Structural invariants shared by load and write (T2 terra-gate fix) --
    //
    // `Self::validate` runs identically on the load path
    // (`from_str_with_path`) and every write path (`to_toml_string`, hence
    // `save`). Before this fix, only the platform-key invariant above ran on
    // both sides — a hand-assembled `LockedTool` with a tagged `repository`
    // or a `LockMetadata` with a stale `declaration_hash_version` would
    // serialize successfully via `to_toml_string`/`save`, then be rejected
    // by every later `load` of the very file `save` just wrote.

    /// (a) Round-trip property: several structurally distinct, VALID locks
    /// all serialize successfully and reload without error. Proven by
    /// re-serializing the reload and comparing bytes (extends
    /// `roundtrip_deterministic` to more shapes: empty tool list, multiple
    /// tools across groups, and R6 shared-digest aliasing across two
    /// platform keys).
    #[test]
    fn every_valid_lock_that_serializes_also_loads() {
        let shared_digest_platforms = {
            let mut platforms = BTreeMap::new();
            platforms.insert("darwin/amd64".to_string(), digest_of('4'));
            platforms.insert("darwin/arm64".to_string(), digest_of('4'));
            platforms
        };
        let cases: Vec<ProjectLock> = vec![
            ProjectLock {
                metadata: sample_metadata(),
                tools: vec![],
            },
            ProjectLock {
                metadata: sample_metadata(),
                tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", '1')],
            },
            ProjectLock {
                metadata: sample_metadata(),
                tools: vec![
                    locked_tool("cmake", "default", "ocx.sh", "cmake", '1'),
                    locked_tool("cmake", "ci", "ocx.sh", "cmake", '2'),
                    locked_tool("ninja", "default", "ocx.sh", "ninja", '3'),
                ],
            },
            ProjectLock {
                metadata: sample_metadata(),
                tools: vec![LockedTool {
                    name: "rosetta".to_string(),
                    group: "default".to_string(),
                    repository: bare_repo("ocx.sh", "rosetta"),
                    platforms: shared_digest_platforms,
                }],
            },
        ];

        for (index, lock) in cases.into_iter().enumerate() {
            let serialized = lock
                .to_toml_string()
                .unwrap_or_else(|e| panic!("case {index}: a valid lock must serialize, got {e}"));
            let reparsed = ProjectLock::from_toml_str(&serialized)
                .unwrap_or_else(|e| panic!("case {index}: a lock that serialized must also load, got {e}"));
            let reserialized = reparsed
                .to_toml_string()
                .unwrap_or_else(|e| panic!("case {index}: a reparsed lock must re-serialize, got {e}"));
            assert_eq!(serialized, reserialized, "case {index}: round-trip must be byte-stable");
        }
    }

    /// (c) `LockMetadata.declaration_hash_version` is a plain `u8` — nothing
    /// at the type level stops a caller from setting it to a value other
    /// than `super::hash::DECLARATION_HASH_VERSION`. Must reject at write,
    /// mirroring `load_rejects_future_hash_version`.
    #[test]
    fn write_rejects_declaration_hash_version_mismatch() {
        let mut metadata = sample_metadata();
        metadata.declaration_hash_version = 2;
        let lock = ProjectLock {
            metadata,
            tools: vec![],
        };
        let err = lock
            .to_toml_string()
            .expect_err("a mismatched declaration_hash_version must reject at write, not just at load");
        let crate::Error::Project(pe) = err else {
            panic!("expected a project-tier error, got {err:?}");
        };
        let ProjectErrorKind::UnsupportedDeclarationHashVersion { version } = &pe.kind else {
            panic!("expected UnsupportedDeclarationHashVersion, got {:?}", pe.kind);
        };
        assert_eq!(*version, 2);
    }

    #[test]
    fn tools_written_sorted_by_name_then_group() {
        // Input order is intentionally unsorted. Output must be:
        // cmake/ci, cmake/default, zlib/default.
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![
                locked_tool("zlib", "default", "ocx.sh", "zlib", '3'),
                locked_tool("cmake", "ci", "ocx.sh", "cmake", '1'),
                locked_tool("cmake", "default", "ocx.sh", "cmake", '2'),
            ],
        };
        let out = lock.to_toml_string().expect("serialization");

        // `[group.ci]` sorts before `[group.default]`, which sorts
        // before `zlib/default`. We locate by `name = "..."` plus the
        // immediately-following `group = "..."` to disambiguate the two
        // cmake entries.
        let cmake_ci = out
            .find("name = \"cmake\"\ngroup = \"ci\"")
            .unwrap_or_else(|| panic!("cmake/ci appears in output:\n{out}"));
        let cmake_default = out
            .find("name = \"cmake\"\ngroup = \"default\"")
            .unwrap_or_else(|| panic!("cmake/default appears in output:\n{out}"));
        let zlib_default = out
            .find("name = \"zlib\"")
            .unwrap_or_else(|| panic!("zlib appears in output:\n{out}"));
        assert!(
            cmake_ci < cmake_default,
            "cmake/ci must come before cmake/default; \
             cmake_ci={cmake_ci}, cmake_default={cmake_default}, out=\n{out}"
        );
        assert!(
            cmake_default < zlib_default,
            "cmake/default must come before zlib/default; \
             cmake_default={cmake_default}, zlib_default={zlib_default}, out=\n{out}"
        );
    }

    // --- save() atomic + generated_at preservation -------------------------

    #[tokio::test]
    async fn save_preserves_generated_at_when_digests_unchanged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let prev = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };

        // `self.tools` is content-equal to `prev.tools` (same repository +
        // platforms map), but `self.metadata.generated_at` differs — the save
        // must preserve prev's timestamp.
        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2099-12-31T23:59:59Z".to_string(),
                ..sample_metadata()
            },
            tools: prev.tools.clone(),
        };

        let cfg = stage_sibling_ocx_toml(&path);
        next.save(&path, Some(&prev), tmp.path(), &cfg).await.expect("save ok");
        let reloaded = ProjectLock::load(&path).await.expect("reload ok");
        assert_eq!(
            reloaded.metadata.generated_at, "2026-01-01T00:00:00Z",
            "generated_at must be preserved when tools are unchanged"
        );
    }

    #[tokio::test]
    async fn save_updates_generated_at_when_digest_changes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let prev = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };

        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-06-01T12:00:00Z".to_string(),
                ..sample_metadata()
            },
            // Different leaf digest byte ⇒ different content.
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'b')],
        };

        let cfg = stage_sibling_ocx_toml(&path);
        next.save(&path, Some(&prev), tmp.path(), &cfg).await.expect("save ok");
        let reloaded = ProjectLock::load(&path).await.expect("reload ok");
        assert_ne!(
            reloaded.metadata.generated_at, "2026-01-01T00:00:00Z",
            "generated_at must change when digests change"
        );
    }

    #[tokio::test]
    async fn generated_at_preserved_when_platforms_unchanged() {
        // A re-lock that produces the identical map keeps `generated_at`
        // frozen (ADR Validation: unchanged re-lock is byte-identical, no
        // `generated_at` churn).
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let prev = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };
        // Rebuilt independently with the same repository + leaf digest.
        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2099-12-31T23:59:59Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };

        let cfg = stage_sibling_ocx_toml(&path);
        next.save(&path, Some(&prev), tmp.path(), &cfg).await.expect("save ok");
        let reloaded = ProjectLock::load(&path).await.expect("reload ok");
        assert_eq!(
            reloaded.metadata.generated_at, "2026-01-01T00:00:00Z",
            "generated_at must be preserved when the platforms map is unchanged"
        );
    }

    #[tokio::test]
    async fn generated_at_preserved_when_tool_order_differs() {
        // Regression: the digests-equal comparator sorts both sides by
        // (group, name) before comparing so a caller who rebuilds the
        // tools vec in a different order (e.g. via parallel resolution
        // that races) doesn't spuriously churn `generated_at`.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let tool_a = locked_tool("cmake", "default", "ocx.sh", "cmake", 'a');
        let tool_b = locked_tool("ninja", "default", "ocx.sh", "ninja", 'b');

        let prev = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![tool_a.clone(), tool_b.clone()],
        };
        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2099-12-31T23:59:59Z".to_string(),
                ..sample_metadata()
            },
            // Reversed order, same content.
            tools: vec![tool_b, tool_a],
        };

        let cfg = stage_sibling_ocx_toml(&path);
        next.save(&path, Some(&prev), tmp.path(), &cfg).await.expect("save ok");
        let reloaded = ProjectLock::load(&path).await.expect("reload ok");
        assert_eq!(
            reloaded.metadata.generated_at, "2026-01-01T00:00:00Z",
            "generated_at must be preserved regardless of input tool order"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_preserves_original_on_write_failure() {
        // Atomicity contract: when the tempfile creation or persist
        // step fails, the original lock file at the target path must
        // survive unchanged. We reproduce the failure by making the
        // parent directory non-writable (0o555) — `NamedTempFile::new_in`
        // returns an I/O error, and the pre-existing file remains
        // readable.
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;

        /// RAII guard that restores directory permissions on drop so a
        /// test failure doesn't leave an unreadable temp dir behind and
        /// break tempdir cleanup.
        struct RestorePerms {
            dir: PathBuf,
            original: std::fs::Permissions,
        }
        impl Drop for RestorePerms {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.dir, self.original.clone());
            }
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_dir = tmp.path().join("lockdir");
        std::fs::create_dir(&lock_dir).expect("mkdir lockdir");
        let path = lock_dir.join("ocx.lock");

        // Seed the target with known content via a first save.
        let seed = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                declaration_hash: format!("sha256:{}", sha256_of('1')),
                ..sample_metadata()
            },
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };
        let cfg = stage_sibling_ocx_toml(&path);
        seed.save(&path, None, tmp.path(), &cfg).await.expect("seed save ok");
        let original_bytes = tokio::fs::read(&path).await.expect("read original");

        // Make the parent directory read-only so tempfile creation fails.
        let original_perms = std::fs::metadata(&lock_dir).expect("meta").permissions();
        let _guard = RestorePerms {
            dir: lock_dir.clone(),
            original: original_perms.clone(),
        };
        std::fs::set_permissions(&lock_dir, std::fs::Permissions::from_mode(0o555)).expect("chmod 0o555");

        // Attempt to save a different lock. Expected: I/O-class error,
        // original content preserved.
        let clobber = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2099-12-31T23:59:59Z".to_string(),
                declaration_hash: format!("sha256:{}", sha256_of('2')),
                ..sample_metadata()
            },
            tools: vec![locked_tool("ninja", "default", "ocx.sh", "ninja", 'b')],
        };
        let cfg_clobber = stage_sibling_ocx_toml(&path);
        let err = clobber
            .save(&path, None, tmp.path(), &cfg_clobber)
            .await
            .expect_err("save must fail");
        assert_kind!(err, ProjectErrorKind::Io(_));

        // Original file still exists with unchanged bytes.
        let after_bytes = tokio::fs::read(&path).await.expect("read after");
        assert_eq!(
            original_bytes, after_bytes,
            "original lock file must be preserved when save fails"
        );
    }

    #[tokio::test]
    async fn save_writes_atomic() {
        // Best-effort: after a successful save, no .tmp* sibling should
        // linger next to the target. The tempfile is expected to be renamed
        // into place by the atomic pattern, not left behind.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };
        let cfg = stage_sibling_ocx_toml(&path);
        lock.save(&path, None, tmp.path(), &cfg).await.expect("save ok");

        let entries: Vec<_> = std::fs::read_dir(tmp.path())
            .expect("readdir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        let strays: Vec<_> = entries
            .iter()
            .filter(|n| n != &"ocx.lock" && (n.contains(".tmp") || n.starts_with(".tmp")))
            .collect();
        assert!(
            strays.is_empty(),
            "no .tmp* siblings should remain after atomic save; got: {strays:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn save_preserves_existing_file_permissions() {
        // Permission preservation contract: if a previous save left the
        // file at 0o644 (or any mode the user chose), a subsequent save
        // must not silently demote it to the tempfile's 0o600 default.
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let seed = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };
        let cfg = stage_sibling_ocx_toml(&path);
        seed.save(&path, None, tmp.path(), &cfg).await.expect("seed save ok");

        // User (or a prior tool) relaxes the mode to 0o644.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod 0o644");
        let before = std::fs::metadata(&path).expect("meta before").permissions().mode();
        assert_eq!(before & 0o777, 0o644, "precondition: file is 0o644");

        // Overwrite with a fresh save — mode must survive.
        let next = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("ninja", "default", "ocx.sh", "ninja", 'b')],
        };
        let cfg2 = stage_sibling_ocx_toml(&path);
        next.save(&path, None, tmp.path(), &cfg2).await.expect("save ok");

        let after = std::fs::metadata(&path).expect("meta after").permissions().mode();
        assert_eq!(
            after & 0o777,
            0o644,
            "permissions 0o644 must survive atomic rename; got 0o{:o}",
            after & 0o777
        );
    }

    #[tokio::test]
    async fn load_rejects_oversized_file() {
        // Size-cap contract: lock files larger than 64 KiB are a sanity
        // failure, surfaced as a structured `FileTooLarge` error rather
        // than proceeding into a pathological TOML parse.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        // Build a 100 KiB TOML comment (valid TOML, trivially large).
        let abc = sha256_of('a');
        let padding: String = "# padding comment line to exceed the size cap\n".repeat(2200);
        let oversized = format!(
            "{padding}\n[metadata]\n\
             lock_version = 3\n\
             declaration_hash_version = 1\n\
             declaration_hash = \"sha256:{abc}\"\n\
             generated_by = \"ocx 0.3.0\"\n\
             generated_at = \"2026-04-19T00:00:00Z\"\n"
        );
        assert!(
            oversized.len() > 64 * 1024,
            "fixture must exceed 64 KiB cap, got {}",
            oversized.len()
        );
        tokio::fs::write(&path, &oversized).await.expect("write oversized");

        let err = ProjectLock::load(&path).await.expect_err("oversized lock must reject");
        assert_kind!(err, ProjectErrorKind::FileTooLarge { .. });
    }

    // ── On-disk shape ──────────────────────────────────────────────────────

    /// Structural guard (ADR Validation: "No lock contains an index
    /// digest"). A lock serializes the bare `repository` (no tag, no
    /// digest) plus the per-platform leaf map — the outer image-index digest
    /// is never written. We assert the on-disk form carries the repository
    /// coordinate, the per-platform key, and the leaf digest, and that the
    /// `[tool.platforms]` table shape (not a `pinned = "..."` line) is used.
    #[tokio::test]
    async fn save_writes_repository_and_platforms_no_index_digest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_path = tmp.path().join("ocx.lock");

        let leaf = digest_of('a');
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };

        let cfg = stage_sibling_ocx_toml(&lock_path);
        lock.save(&lock_path, None, tmp.path(), &cfg)
            .await
            .expect("save succeeds");

        let on_disk = tokio::fs::read_to_string(&lock_path).await.expect("read");
        assert!(
            on_disk.contains("repository = \"ocx.sh/cmake\""),
            "lock must record the bare repository coordinate; got:\n{on_disk}"
        );
        assert!(
            on_disk.contains("\"linux/amd64\""),
            "lock must record the per-platform key; got:\n{on_disk}"
        );
        assert!(
            on_disk.contains(&leaf.to_string()),
            "lock must record the leaf digest; got:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("pinned ="),
            "lock must NOT carry a legacy `pinned` index-digest line; got:\n{on_disk}"
        );

        // Round-trips back into an identical entry with the same leaf.
        let reloaded = ProjectLock::load(&lock_path).await.expect("reload succeeds");
        assert_eq!(reloaded.tools.len(), 1);
        let tool = &reloaded.tools[0];
        assert_eq!(tool.repository.registry(), "ocx.sh");
        assert_eq!(tool.repository.repository(), "cmake");
        assert_eq!(tool.platforms.get("linux/amd64"), Some(&leaf));
    }

    // ── tools_content_equal: drop / appear / value-change ──────────────────

    /// `tools_content_equal` must treat a key ADD (publisher ships a new
    /// platform) as content-changed (ADR: the drop/appear signal counts as
    /// content-changed and advances `generated_at`).
    #[test]
    fn tools_content_equal_detects_platform_key_add() {
        let prev = vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')];

        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), digest_of('a'));
        platforms.insert("darwin/arm64".to_string(), digest_of('b'));
        let next = vec![LockedTool {
            name: "cmake".to_string(),
            group: "default".to_string(),
            repository: bare_repo("ocx.sh", "cmake"),
            platforms,
        }];

        assert!(
            !tools_content_equal(&prev, &next),
            "adding a platform key must register as content-changed"
        );
    }

    /// `tools_content_equal` must treat a key REMOVE (publisher drops a
    /// platform) as content-changed.
    #[test]
    fn tools_content_equal_detects_platform_key_remove() {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), digest_of('a'));
        platforms.insert("darwin/arm64".to_string(), digest_of('b'));
        let prev = vec![LockedTool {
            name: "cmake".to_string(),
            group: "default".to_string(),
            repository: bare_repo("ocx.sh", "cmake"),
            platforms,
        }];
        // Only the amd64 leaf survives.
        let next = vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')];

        assert!(
            !tools_content_equal(&prev, &next),
            "removing a platform key must register as content-changed"
        );
    }

    /// `tools_content_equal` must treat a leaf VALUE change (same platform,
    /// new digest) as content-changed.
    #[test]
    fn tools_content_equal_detects_leaf_value_change() {
        let prev = vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')];
        let next = vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'b')];
        assert!(
            !tools_content_equal(&prev, &next),
            "a changed leaf digest must register as content-changed"
        );
    }

    /// Two identical platform maps built in different key-insertion order
    /// compare content-equal — `BTreeMap` canonicalizes ordering, so an
    /// unchanged re-lock is detected regardless of how the resolver
    /// assembled the map.
    #[test]
    fn tools_content_equal_identical_maps_any_order_are_equal() {
        let mut a = BTreeMap::new();
        a.insert("linux/amd64".to_string(), digest_of('1'));
        a.insert("darwin/arm64".to_string(), digest_of('2'));
        let mut b = BTreeMap::new();
        // Reverse insertion order — BTreeMap canonicalizes.
        b.insert("darwin/arm64".to_string(), digest_of('2'));
        b.insert("linux/amd64".to_string(), digest_of('1'));

        let lhs = vec![LockedTool {
            name: "cmake".to_string(),
            group: "default".to_string(),
            repository: bare_repo("ocx.sh", "cmake"),
            platforms: a,
        }];
        let rhs = vec![LockedTool {
            name: "cmake".to_string(),
            group: "default".to_string(),
            repository: bare_repo("ocx.sh", "cmake"),
            platforms: b,
        }];
        assert!(
            tools_content_equal(&lhs, &rhs),
            "identical maps must compare content-equal regardless of insertion order"
        );
    }

    #[test]
    fn tools_content_equal_is_order_independent() {
        // The function must compare two tool lists by *content* regardless of
        // input order — `save()` relies on this to preserve `generated_at`
        // even when prior and current locks differ only in iteration order.
        let a = locked_tool("cmake", "default", "ocx.sh", "cmake", '1');
        let b = locked_tool("ninja", "default", "ocx.sh", "ninja", '2');

        let lhs = vec![a.clone(), b.clone()];
        let rhs = vec![b, a];

        assert!(tools_content_equal(&lhs, &rhs));
        assert!(tools_content_equal(&rhs, &lhs));
    }

    // ── acquire_project_lock integration ─────────────────────────────────────

    /// `acquire_project_lock` must reject a second concurrent acquire while the
    /// first guard is alive, then succeed once the first guard is dropped.
    /// Proves the scoped mutation lock serialises concurrent writers.
    #[tokio::test]
    async fn acquire_project_lock_blocks_second_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The manifest the lock is keyed by; the lock file itself lives
        // under the locks root, never beside it.
        std::fs::write(dir.path().join("ocx.toml"), "[tools]\n").expect("write ocx.toml");

        let first_guard = crate::acquire_project_lock(dir.path(), &dir.path().join(".ocx-home").join("locks"))
            .await
            .expect("first acquire must succeed");

        let err = crate::acquire_project_lock(dir.path(), &dir.path().join(".ocx-home").join("locks"))
            .await
            .expect_err("second acquire must fail while first guard is held");
        assert_kind!(err, ProjectErrorKind::Locked);

        drop(first_guard);

        crate::acquire_project_lock(dir.path(), &dir.path().join(".ocx-home").join("locks"))
            .await
            .expect("acquire after release must succeed");
    }

    /// A symlink at `ocx.toml` must cause `acquire_project_lock` to return
    /// `ProjectErrorKind::Io`, not `Locked`, and must NOT follow the symlink
    /// to its target.
    ///
    /// The publish is a rename, so a planted symlink is replaced rather than
    /// written through — but every mutator *reads* the config immediately
    /// after this acquire, and an unrefused symlink would hand it an
    /// attacker-chosen document to edit and publish back.
    #[tokio::test]
    async fn acquire_project_lock_rejects_symlink_at_ocx_toml() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink as make_symlink;
        #[cfg(not(unix))]
        fn make_symlink(target: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
            std::os::windows::fs::symlink_file(target, link)
        }

        let dir = tempfile::tempdir().expect("tempdir");

        let sensitive_file = dir.path().join("sensitive_file");
        let link_path = dir.path().join("ocx.toml");
        // Plant a symlink at ocx.toml pointing to a non-existent sensitive target.
        if make_symlink(&sensitive_file, &link_path).is_err() {
            // Symlink creation can fail on Windows without elevated privileges or
            // developer mode. Skip rather than fail on those configurations.
            return;
        }

        let err = crate::acquire_project_lock(dir.path(), &dir.path().join(".ocx-home").join("locks"))
            .await
            .expect_err("acquire_project_lock must fail when ocx.toml is a symlink");

        // The symlink_metadata pre-check surfaces InvalidInput → Io, not Locked.
        assert_kind!(err, ProjectErrorKind::Io(_));

        // The symlink target must NOT have been created or modified.
        assert!(
            !sensitive_file.exists(),
            "symlink target must not be touched; found: {sensitive_file:?}"
        );
    }

    /// While a writer holds the project mutation lock,
    /// `ProjectLock::from_path` (the unlocked read path) must complete promptly.
    ///
    /// Readers open `ocx.lock` directly without any lock — only writers
    /// coordinate, through the scoped mutation lock under `$OCX_HOME/locks`.
    /// Readers always observe a complete file via the atomic-rename
    /// guarantee, so they never need to wait.
    ///
    /// A regression where the reader blocks would exceed the 500 ms timeout.
    #[tokio::test]
    async fn acquire_project_lock_does_not_block_concurrent_reader() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ocx.toml"), "[tools]\n").expect("write ocx.toml");
        let lock_path = dir.path().join("ocx.lock");

        // Acquire the exclusive writer lock for this project.
        let _writer_guard = crate::acquire_project_lock(dir.path(), &dir.path().join(".ocx-home").join("locks"))
            .await
            .expect("first exclusive acquire must succeed");

        // ProjectLock::from_path opens ocx.lock directly — no lock acquired.
        // It must not wait on the writer's mutation lock.
        let reader_result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            ProjectLock::from_path(&lock_path),
        )
        .await
        .expect("reader must not block: timeout exceeded while the writer holds the mutation lock");

        // On a fresh dir the lock file does not exist — None is expected.
        match reader_result {
            Ok(None) => {}    // expected: lock file absent
            Ok(Some(_)) => {} // also acceptable: lock file existed from another test
            Err(e) => panic!("reader must not error while the writer holds the mutation lock; got: {e}"),
        }
    }

    // ── Warn #13 regression — lock_path_for derives ocx.lock correctly ────

    /// `lock_path_for` must always produce `<dir>/ocx.lock` regardless of the
    /// config file's name or extension — including unusual names that have no
    /// extension or a multi-segment extension.
    #[test]
    fn lock_path_for_always_produces_ocx_lock_in_config_dir() {
        // Standard case: ocx.toml → same dir, named ocx.lock.
        assert_eq!(
            lock_path_for(std::path::Path::new("/tmp/some-dir/ocx.toml")),
            std::path::PathBuf::from("/tmp/some-dir/ocx.lock"),
            "standard ocx.toml case"
        );

        // Non-standard name: custom config file name.
        assert_eq!(
            lock_path_for(std::path::Path::new("/tmp/some-dir/my-custom-name.toml")),
            std::path::PathBuf::from("/tmp/some-dir/ocx.lock"),
            "custom config name must still produce ocx.lock in the same dir"
        );

        // No extension: `with_extension("lock")` would produce `Manifest.lock`,
        // but `lock_path_for` always produces `ocx.lock`.
        assert_eq!(
            lock_path_for(std::path::Path::new("/tmp/some-dir/Manifest")),
            std::path::PathBuf::from("/tmp/some-dir/ocx.lock"),
            "extension-free config name"
        );

        // Hidden file: `.hidden` in the same directory.
        assert_eq!(
            lock_path_for(std::path::Path::new("/tmp/some-dir/.hidden")),
            std::path::PathBuf::from("/tmp/some-dir/ocx.lock"),
            "hidden config file"
        );
    }

    // ── the lock is published under the manifest's mode policy ────────────

    /// A save carries the mode it finds **whole** — group-writable and
    /// world-writable modes included.
    ///
    /// This inverts the former Warn #8 contract, which masked the carried mode
    /// with `0o644`. The cap was applied by `ocx.lock`'s own writer and by
    /// nothing else: the `ocx.toml` published beside it, in the same directory,
    /// by the same `MutationGuard` commit, was never capped — so the cap could
    /// not close anything an attacker able to rewrite one file would not get
    /// from the other, and it fired only on the saves that happened to occur.
    /// What it did reliably do is strip the group-write bit in an
    /// `umask 002` shared-group checkout, which is the bit that lets the next
    /// developer run `ocx lock` at all.
    ///
    /// `0o664` is the discriminating case: it is the mode the old mask would
    /// silently demote, and reinstating `& 0o644` in
    /// [`crate::mutate::publish_by_rename`] turns both arms red.
    #[cfg(unix)]
    #[tokio::test]
    async fn save_carries_the_mode_it_finds_including_group_and_world_writable() {
        use std::os::unix::fs::PermissionsExt;

        for mode in [0o664u32, 0o666u32] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let path = tmp.path().join("ocx.lock");

            // Seed with initial save.
            let seed = ProjectLock {
                metadata: sample_metadata(),
                tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
            };
            let cfg = stage_sibling_ocx_toml(&path);
            seed.save(&path, None, tmp.path(), &cfg).await.expect("seed save ok");

            // `set_permissions` is not umask-clipped, so the fixture wears
            // exactly `mode` — and neither is the `fchmod` the publish uses.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod the fixture");

            let next = ProjectLock {
                metadata: sample_metadata(),
                tools: vec![locked_tool("ninja", "default", "ocx.sh", "ninja", 'b')],
            };
            let cfg2 = stage_sibling_ocx_toml(&path);
            next.save(&path, None, tmp.path(), &cfg2).await.expect("save ok");

            let after = std::fs::metadata(&path).expect("meta after").permissions().mode();
            assert_eq!(
                after & 0o7777,
                mode,
                "a 0o{mode:o} ocx.lock must still be 0o{mode:o} after a save; got 0o{:o}",
                after & 0o7777
            );
        }
    }

    // ── Warn #14 regression — contention surfaces as Locked, real errors as Io ──

    /// Verify the error-kind discriminator: `Ok(None)` (contended) → `Locked`,
    /// `Err(e)` (real I/O) → `Io`. The contention path is already exercised by
    /// `acquire_project_lock_blocks_second_writer`; this test checks both branches
    /// in isolation.
    #[test]
    fn io_error_kind_discrimination_locked_vs_io() {
        let lock_path = std::path::PathBuf::from("/tmp/test.lock");

        // `Ok(None)` = contended → Locked; `Err(e)` = real I/O → Io.
        let locked_err = super::super::Error::from(ProjectError::new(lock_path.clone(), ProjectErrorKind::Locked));
        let io_err = super::super::Error::from(ProjectError::new(
            lock_path.clone(),
            ProjectErrorKind::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        ));

        // Contention (Ok(None)) → Locked
        assert_kind!(locked_err, ProjectErrorKind::Locked);

        // Real I/O error (Err) → Io
        assert_kind!(io_err, ProjectErrorKind::Io(_));
    }

    // --- The typed repository keeps the lock bytes -------------------------

    /// Regression pin: real `ocx lock` output (a frozen copy of this
    /// repository's own `ocx.lock`) round-trips byte-identical through the typed
    /// `Repository` field. Frozen so a toolchain bump never re-keys this test.
    #[test]
    fn c001_the_repository_s_own_lock_round_trips_byte_identical() {
        let original = include_str!("testdata/repo.ocx.lock");
        let lock = ProjectLock::from_toml_str(original).expect("the repository's own lock loads");
        assert!(!lock.tools.is_empty(), "the fixture must exercise real tool entries");
        assert_eq!(lock.to_toml_string().expect("serialize"), original);
    }

    /// A hand-edited tagged or digested `repository` is the typed
    /// `LockRepositoryNotBare` (exit 78) at load, never a serde `TomlParse`.
    #[test]
    fn c001_a_tagged_lock_repository_loads_as_lock_repository_not_bare() {
        for repository in [
            "ocx.sh/cmake:3.28".to_string(),
            format!("ocx.sh/cmake@sha256:{}", sha256_of('9')),
        ] {
            let toml_str = v3_lock_toml().replace(
                r#"repository = "ocx.sh/cmake""#,
                &format!(r#"repository = "{repository}""#),
            );
            assert!(toml_str.contains(&repository), "the fixture edit must land");
            let err = ProjectLock::from_toml_str(&toml_str).expect_err("a non-bare repository must reject");
            assert_kind!(err, ProjectErrorKind::LockRepositoryNotBare { .. });
        }
    }

    // --- Binding the lock to its declarations -------------------------------

    fn host() -> Platform {
        "linux/amd64".parse().expect("valid platform")
    }

    /// A config and a lock whose recorded hash is that config's own, so the
    /// hash half of the staleness test answers "current".
    fn locked_from(config_toml: &str, tools: Vec<LockedTool>) -> (crate::ProjectConfig, ProjectLock) {
        let config = crate::ProjectConfig::from_toml_str(config_toml).expect("parse ocx.toml");
        let mut metadata = sample_metadata();
        metadata.declaration_hash = config.declaration_hash_cached().to_owned();
        (config, ProjectLock { metadata, tools })
    }

    fn bound(lock: &ProjectLock, config: &crate::ProjectConfig) -> Vec<BoundTool> {
        assert!(lock.is_current(config), "a lock locked from this config is current");
        match lock.bind(config) {
            Binding::Current(bound) => bound,
            Binding::Stale(drift) => panic!("a lock locked from this config must bind as Current: {drift}"),
        }
    }

    /// `shellcheck` in two groups under two tags, and a lock deliberately not
    /// in `(group, name)` order: a join by name alone or a re-sorted result
    /// pairs a tag with the wrong entry.
    const TWO_GROUPS_TOML: &str = r#"
[tools]
shellcheck = "ocx.sh/shellcheck:0.10"
cmake = "ocx.sh/cmake:3.28"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.11"
"#;

    fn two_groups_lock_tools() -> Vec<LockedTool> {
        vec![
            locked_tool("shellcheck", "ci", "ocx.sh", "shellcheck", '2'),
            locked_tool("cmake", "default", "ocx.sh", "cmake", '1'),
            locked_tool("shellcheck", "default", "ocx.sh", "shellcheck", '3'),
        ]
    }

    /// The tag `TWO_GROUPS_TOML` declares for `(group, name)`.
    fn declared_tag(group: &str, name: &str) -> &'static str {
        match (group, name) {
            ("default", "shellcheck") => "0.10",
            ("ci", "shellcheck") => "0.11",
            ("default", "cmake") => "3.28",
            other => panic!("no declaration for {other:?}"),
        }
    }

    /// One `BoundTool` per lock entry, in lock order, each joined to the
    /// declaration of its own `(group, name)`.
    #[test]
    fn c003_bind_pairs_each_entry_with_its_own_group_declaration_in_lock_order() {
        let (config, lock) = locked_from(TWO_GROUPS_TOML, two_groups_lock_tools());
        let bound = bound(&lock, &config);

        assert_eq!(bound.len(), lock.tools.len(), "exactly one BoundTool per lock entry");
        for (bound, entry) in bound.iter().zip(&lock.tools) {
            assert_eq!(bound.locked(), entry, "BoundTools follow lock order");
            let tag = declared_tag(&entry.group, &entry.name);
            assert_eq!(bound.advisory_tag(), Some(tag), "{}/{}", entry.group, entry.name);
            assert_eq!(
                bound.declared().to_string(),
                format!("ocx.sh/{}:{tag}", entry.name),
                "declared() is the full declaration"
            );
        }
    }

    /// `ocx.toml` moved on without a relock.
    #[test]
    fn c003_bind_is_stale_when_the_declaration_hash_moved() {
        let (config, mut lock) = locked_from(TWO_GROUPS_TOML, two_groups_lock_tools());
        lock.metadata.declaration_hash = format!("sha256:{}", sha256_of('e'));
        assert!(!lock.is_current(&config), "a moved hash is not current");
        let Binding::Stale(drift) = lock.bind(&config) else {
            panic!("a moved hash must bind as Stale");
        };
        assert!(matches!(*drift, LockDrift::DeclarationHash { .. }), "{drift}");
    }

    /// An entry whose `(group, name)` the config does not declare, even
    /// with a fresh hash. The same name declared in another group does not count.
    #[test]
    fn c003_bind_is_stale_when_a_lock_entry_has_no_declaration() {
        let (config, lock) = locked_from(
            "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n\n[group.ci.tools]\nninja = \"ocx.sh/ninja:1.12\"\n",
            vec![
                locked_tool("cmake", "default", "ocx.sh", "cmake", '1'),
                locked_tool("ninja", "default", "ocx.sh", "ninja", '2'),
            ],
        );
        assert!(!lock.is_current(&config), "an undeclared entry is not current");
        let Binding::Stale(drift) = lock.bind(&config) else {
            panic!("an undeclared entry must bind as Stale");
        };
        assert_eq!(
            *drift,
            LockDrift::Entry {
                group: "default".into(),
                name: "ninja".into(),
                locked: Repository::new("ocx.sh", "ninja"),
                declared: None,
            }
        );
    }

    /// A per-entry desync the hash cannot see: the declaration names a
    /// different repository (or registry) than the entry was locked from.
    #[test]
    fn c003_bind_is_stale_when_the_declared_repository_is_not_the_locked_one() {
        for (registry, repo) in [("ocx.sh", "kitware/cmake"), ("ghcr.io", "cmake")] {
            let (config, lock) = locked_from(
                "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n",
                vec![locked_tool("cmake", "default", registry, repo, '1')],
            );
            assert!(
                !lock.is_current(&config),
                "{registry}/{repo} under a matching hash is not current"
            );
            let Binding::Stale(drift) = lock.bind(&config) else {
                panic!("{registry}/{repo} locked against a declared ocx.sh/cmake must bind as Stale");
            };
            assert_eq!(
                *drift,
                LockDrift::Entry {
                    group: "default".into(),
                    name: "cmake".into(),
                    locked: Repository::new(registry, repo),
                    declared: Some(Repository::new("ocx.sh", "cmake")),
                }
            );
        }
    }

    /// `registry/repo:tag@<host leaf>`.
    #[test]
    fn c004_host_leaf_identifier_carries_the_declared_tag_onto_the_lock_leaf() {
        let (config, lock) = locked_from(
            "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n",
            vec![locked_tool("cmake", "default", "ocx.sh", "cmake", '1')],
        );
        let identifier = bound(&lock, &config)[0]
            .host_leaf_identifier(&host())
            .expect("host leaf");
        assert_eq!(
            identifier.to_string(),
            format!("ocx.sh/cmake:3.28@sha256:{}", sha256_of('1'))
        );
    }

    /// A digest-only declaration has no tag to carry; the lock leaf wins
    /// over the declared digest.
    #[test]
    fn c004_a_digest_only_declaration_binds_untagged_and_yields_to_the_lock_leaf() {
        let declared_digest = sha256_of('f');
        let (config, lock) = locked_from(
            &format!("[tools]\ncmake = \"ocx.sh/cmake@sha256:{declared_digest}\"\n"),
            vec![locked_tool("cmake", "default", "ocx.sh", "cmake", '1')],
        );
        let bound = bound(&lock, &config);
        assert_eq!(bound[0].advisory_tag(), None);
        assert_eq!(
            bound[0].declared().to_string(),
            format!("ocx.sh/cmake@sha256:{declared_digest}"),
            "declared() keeps the declared digest"
        );
        let identifier = bound[0].host_leaf_identifier(&host()).expect("host leaf");
        assert_eq!(
            identifier.to_string(),
            format!("ocx.sh/cmake@sha256:{}", sha256_of('1'))
        );
    }

    /// A declaration pinning both a tag and a digest keeps the tag and
    /// takes the lock's platform leaf, never the declared (index) digest.
    #[test]
    fn c004_a_declared_digest_is_ignored_in_favour_of_the_lock_leaf() {
        let (config, lock) = locked_from(
            &format!("[tools]\ncmake = \"ocx.sh/cmake:3.28@sha256:{}\"\n", sha256_of('f')),
            vec![locked_tool("cmake", "default", "ocx.sh", "cmake", '1')],
        );
        let identifier = bound(&lock, &config)[0]
            .host_leaf_identifier(&host())
            .expect("host leaf");
        assert_eq!(
            identifier.to_string(),
            format!("ocx.sh/cmake:3.28@sha256:{}", sha256_of('1'))
        );
    }

    /// No leaf for the host stays `NoHostLeaf`.
    #[test]
    fn c004_host_leaf_identifier_keeps_no_host_leaf() {
        let (config, lock) = locked_from(
            "[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n",
            vec![locked_tool("cmake", "default", "ocx.sh", "cmake", '1')],
        );
        let windows: Platform = "windows/amd64".parse().expect("valid platform");
        let err = bound(&lock, &config)[0]
            .host_leaf_identifier(&windows)
            .expect_err("no windows leaf");
        assert_kind!(err, ProjectErrorKind::NoHostLeaf { .. });
    }

    /// Two equally good leaves stay `AmbiguousHostLeaf`, a remedy
    /// distinct from `NoHostLeaf`.
    #[test]
    fn c004_host_leaf_identifier_keeps_ambiguous_host_leaf() {
        let mut tool = locked_tool("cmake", "default", "ocx.sh", "cmake", '1');
        tool.platforms.clear();
        tool.platforms
            .insert("linux/amd64+libc.glibc".to_string(), digest_of('4'));
        tool.platforms
            .insert("linux/amd64+libc.musl".to_string(), digest_of('5'));
        let (config, lock) = locked_from("[tools]\ncmake = \"ocx.sh/cmake:3.28\"\n", vec![tool]);
        let dual_libc: Platform = "linux/amd64+libc.glibc,libc.musl".parse().expect("valid platform");
        let err = bound(&lock, &config)[0]
            .host_leaf_identifier(&dual_libc)
            .expect_err("two leaves tie");
        assert_kind!(err, ProjectErrorKind::AmbiguousHostLeaf { .. });
    }

    // --- Lenient fallback -----------------------------------------------------

    fn assert_untagged_in_lock_order(
        lock: &ProjectLock,
        pairs: &[(&LockedTool, Result<PinnedPackageRef, crate::Error>)],
    ) {
        assert_eq!(pairs.len(), lock.tools.len(), "one identifier per lock entry");
        for ((tool, identifier), entry) in pairs.iter().zip(&lock.tools) {
            assert_eq!(*tool, entry, "pairs follow lock order");
            let identifier = identifier.as_ref().expect("host leaf");
            assert_eq!(
                identifier.tag(),
                None,
                "{}/{}: no tag without a current binding",
                entry.group,
                entry.name
            );
            assert_eq!(
                identifier.to_string(),
                format!("{}@{}", entry.repository, entry.platforms["linux/amd64"])
            );
        }
    }

    /// Regression pin: no readable declarations → untagged, as today.
    #[test]
    fn c004_lenient_host_identifiers_are_untagged_without_a_config() {
        let (_config, lock) = locked_from(TWO_GROUPS_TOML, two_groups_lock_tools());
        assert_untagged_in_lock_order(&lock, &lock.lenient_host_identifiers(None, &host()));
    }

    /// A stale lock never pairs a (possibly new) declared tag with its old digest.
    #[test]
    fn c004_lenient_host_identifiers_are_untagged_for_a_stale_lock() {
        let (config, mut lock) = locked_from(TWO_GROUPS_TOML, two_groups_lock_tools());
        lock.metadata.declaration_hash = format!("sha256:{}", sha256_of('e'));
        assert_untagged_in_lock_order(&lock, &lock.lenient_host_identifiers(Some(&config), &host()));
    }

    /// A current lock carries each entry's own declared tag, paired with
    /// that entry — a misaligned zip would hand `ci/shellcheck` another tag.
    #[test]
    fn c004_lenient_host_identifiers_carry_each_entry_s_tag_for_a_current_lock() {
        let (config, lock) = locked_from(TWO_GROUPS_TOML, two_groups_lock_tools());
        let pairs = lock.lenient_host_identifiers(Some(&config), &host());
        assert_eq!(pairs.len(), lock.tools.len(), "one identifier per lock entry");
        for ((tool, identifier), entry) in pairs.iter().zip(&lock.tools) {
            assert_eq!(*tool, entry, "pairs follow lock order");
            let identifier = identifier.as_ref().expect("host leaf");
            assert_eq!(
                identifier.to_string(),
                format!(
                    "{}:{}@{}",
                    entry.repository,
                    declared_tag(&entry.group, &entry.name),
                    entry.platforms["linux/amd64"]
                ),
            );
        }
    }

    /// One content declared under two tags is one entry. `ocx.lock` orders
    /// entries by `(group, name)`, so `ci/jdk` precedes `default/java` and its
    /// tag wins.
    #[test]
    fn one_content_under_two_tags_keeps_the_first_entry_in_lock_order() {
        let (config, lock) = locked_from(
            "[tools]\njava = \"ocx.sh/java:21\"\n\n[group.ci.tools]\njdk = \"ocx.sh/java:21.0\"\n",
            vec![
                locked_tool("java", "default", "ocx.sh", "java", 'a'),
                locked_tool("jdk", "ci", "ocx.sh", "java", 'a'),
            ],
        );
        let lock = ProjectLock::from_toml_str(&lock.to_toml_string().expect("serialize")).expect("parse");
        let entries = bound(&lock, &config)
            .into_iter()
            .map(|tool| {
                let identifier = tool.host_leaf_identifier(&host()).expect("host leaf");
                (tool.locked().name.clone(), identifier)
            })
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 2, "both bindings bind");

        let kept = first_per_content(entries);

        assert_eq!(kept.len(), 1, "one content, one entry: {kept:?}");
        assert_eq!(kept[0].0, "jdk");
        assert_eq!(
            kept[0].1.to_string(),
            format!("ocx.sh/java:21.0@sha256:{}", sha256_of('a'))
        );
    }

    /// One content bound under two groups is eager when either group wants it eager,
    /// whichever comes first; the first binding's identifier (and tag) is the one kept.
    #[test]
    fn eager_per_content_is_eager_when_any_binding_is_eager() {
        use super::LazyMode::{Always, Never};
        let pin = |reference: &str| {
            PinnedPackageRef::try_from(PackageRef::parse(reference).expect("parse")).expect("digest present")
        };
        let java_21 = pin(&format!("ocx.sh/java:21@sha256:{}", "a".repeat(64)));
        let java_lts = pin(&format!("ocx.sh/java:lts@sha256:{}", "a".repeat(64)));
        let cmake = pin(&format!("ocx.sh/cmake:3.28@sha256:{}", "b".repeat(64)));

        for (first, second) in [(Always, Never), (Never, Always)] {
            let merged = eager_per_content([
                (first, java_21.clone()),
                (Always, cmake.clone()),
                (second, java_lts.clone()),
            ]);
            assert_eq!(
                merged,
                vec![(Never, java_21.clone()), (Always, cmake.clone())],
                "order {first:?}, {second:?}"
            );
        }
    }
}
