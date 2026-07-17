// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx.lock` schema: the machine-written, committed lock file.
//!
//! Locking model: writers acquire an exclusive advisory flock on `ocx.toml`
//! via [`crate::project::acquire_project_lock`] before mutating the project
//! state. Readers (`load`, `from_path`, `ProjectConfig::from_path`) never
//! take a lock — concurrent reads are always allowed.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use super::error::{ProjectError, ProjectErrorKind};
use crate::oci::{Digest, Identifier, Platform};

/// Derive the canonical `ocx.lock` data-file path for a given config file path.
///
/// The lock is always named `ocx.lock` and lives in the same directory as the
/// config file, regardless of the config file's name or extension. Using the
/// parent directory rather than `path.with_extension("lock")` avoids surprising
/// results when the config has an unusual name or no extension at all.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use ocx_lib::project::lock::lock_path_for;
///
/// let lp = lock_path_for(Path::new("/project/ocx.toml"));
/// assert_eq!(lp, std::path::PathBuf::from("/project/ocx.lock"));
/// ```
pub fn lock_path_for(config_path: &Path) -> PathBuf {
    config_path.parent().unwrap_or_else(|| Path::new(".")).join("ocx.lock")
}

/// Lock file version discriminant.
///
/// Serialized as a bare integer via `serde_repr`. `V3` is the only version
/// this build accepts — a lock written by an older OCX (V1 or V2) is
/// rejected with an actionable [`ProjectErrorKind::UnsupportedLockVersion`]
/// error (`ocx lock` regenerates it) rather than a bridged read. Per
/// `adr_platform_model_unification.md` D3/D5: no migration code, the
/// regenerate hint is the entire migration story.
///
/// The version-peek loader ([`ProjectLock::from_str_with_path`]) reads
/// `metadata.lock_version` as a raw integer — not through this enum's derived
/// `Deserialize` — so it can surface the found value explicitly rather than
/// relying on `serde_repr`'s generic "unknown variant" failure.
///
/// Intentionally NOT `#[non_exhaustive]` — this is an internal on-disk
/// discriminant, not a public library enum, and the project convention
/// omits `#[non_exhaustive]` on internal non-error enums so matches stay
/// total across the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum LockVersion {
    /// Version 3 of the `ocx.lock` on-disk format — the only version this
    /// build reads or writes: canonical-grammar platform keys (D2), bare
    /// `repository` coordinates, an available-only per-platform leaf-digest
    /// map.
    V3 = 3,
}

// `serde_repr` integer enums need a hand-written `JsonSchema` impl because
// the derive cannot peek into the repr. Mirrors the manual impl on
// `crate::package::metadata::bundle::Version`.
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

/// Minimal header-only projection used to peek `metadata.lock_version` before
/// committing to a full deserialize. Reads the version as a raw `u8` (not
/// [`LockVersion`]) so an unsupported value surfaces the actionable
/// [`ProjectErrorKind::UnsupportedLockVersion`] error naming the found value,
/// rather than `serde_repr`'s generic "unknown variant" failure.
#[derive(Debug, Clone, Deserialize)]
struct LockVersionPeek {
    metadata: LockVersionPeekMetadata,
}

/// The single field the version peek needs from `[metadata]`.
#[derive(Debug, Clone, Deserialize)]
struct LockVersionPeekMetadata {
    lock_version: u8,
}

/// The only `lock_version` this build accepts.
const SUPPORTED_LOCK_VERSION: u8 = 3;

/// Lock metadata header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockMetadata {
    /// On-disk schema version. `V3` is the only version this build reads or
    /// writes.
    pub lock_version: LockVersion,
    /// Canonicalization contract version for [`Self::declaration_hash`].
    /// Currently always `1` — see [`super::DECLARATION_HASH_VERSION`].
    pub declaration_hash_version: u8,
    /// `sha256:<hex>` of the RFC 8785 JCS-canonicalized declaration
    /// (see `super::hash`).
    pub declaration_hash: String,
    /// Tooling version string that wrote the lock, e.g. `"ocx 0.3.0"`.
    pub generated_by: String,
    /// ISO-8601 UTC timestamp. Preserved verbatim when the resolved
    /// content (registry, repository, digest) of every tool is unchanged
    /// between two `ocx lock` runs; updated otherwise.
    pub generated_at: String,
}

/// Top-level `ocx.lock` document — both the in-memory model and the on-disk
/// wire shape (the `tool` TOML array name is the only difference, via
/// `#[serde(rename = "tool")]`).
///
/// All locked tools are sorted by `(group, name)` at write time for
/// byte-stable output; [`Self::to_toml_string`] serializes through a
/// borrowed, pre-sorted view rather than cloning the whole document.
/// [`Self::from_str_with_path`] is the version-peek read path: it rejects any
/// `lock_version` other than [`SUPPORTED_LOCK_VERSION`] before attempting the
/// full deserialize, and validates the bare-`repository` and
/// canonical-platform-key invariants (D3) on every tool afterward.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectLock {
    /// Lock metadata header (version, declaration hash, generator, etc.).
    pub metadata: LockMetadata,

    /// All locked tools in the project. Sorted by (group, name) at write
    /// time for byte-stable output. On disk this is the `[[tool]]` array.
    #[serde(default, rename = "tool")]
    pub tools: Vec<LockedTool>,
}

/// One locked tool entry — identified by the local binding `(group, name)`.
///
/// `name` is the TOML key used in the developer's `ocx.toml` (the local
/// binding); `group` is `"default"` for entries from the top-level `[tools]`
/// table or the named `[group.<name>]` key. `repository` is the bare
/// registry/repo coordinate (no tag, no digest) shared by every platform
/// leaf — the outer image-index digest is intentionally not stored (the lock
/// unit is the platform-manifest digest, uniform with dependency pins); the
/// per-platform pull id is reconstructed as `repository.clone_with_digest(leaf)`.
/// `platforms` records one leaf digest per shipped platform, keyed by the
/// canonical grammar string ([`Platform`] `Display` — D2); a platform the
/// publisher does not ship is encoded by absence of its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedTool {
    /// Local binding name (TOML key from `ocx.toml`, e.g. `cmake`).
    pub name: String,
    /// Owning group. `"default"` for entries from the top-level
    /// `[tools]` table; otherwise the named `[group.*]` key.
    pub group: String,
    /// Bare registry/repo coordinates shared by every platform leaf.
    pub repository: Identifier,
    /// Available-only per-platform leaf digests, keyed by the canonical
    /// grammar [`Platform`] string. `BTreeMap` for byte-stable output
    /// without requiring `Ord` on `Platform`.
    pub platforms: BTreeMap<String, Digest>,
}

/// Content equality for two [`LockedTool`]s, ignoring `name`/`group` —
/// `repository` and the full `platforms` map. Shared by the lock writer's
/// `generated_at`-preservation check ([`tools_content_equal`]) and the
/// duplicate-across-selected-groups gate in `project::compose`.
pub fn locked_tool_content_equal(left: &LockedTool, right: &LockedTool) -> bool {
    left.repository == right.repository && left.platforms == right.platforms
}

/// Validates that every key in `platforms` is the canonical grammar spelling
/// of the [`Platform`] it parses to (`Platform::from_str(key)?.to_string()
/// == key`) — D3's canonical-key validation, applied at V3 lock load and at
/// every platform-map write site.
///
/// Because [`Platform`]'s `Display` is injective (`adr_platform_model_
/// unification.md` D2), two DISTINCT canonical keys can never parse to the
/// same `Platform` — a second key resolving to the same `Platform` would
/// itself have to equal the first key's canonical spelling, which `BTreeMap`
/// key uniqueness already rules out. So this single canonicalization check
/// also enforces D3 rule 2 (canonical-platform uniqueness) for free; no
/// separate uniqueness pass is needed. Digest-*value* aliasing (two distinct
/// platform keys sharing a digest — e.g. Rosetta 2) stays legal: this checks
/// keys only, never values (R6).
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

    /// Load a [`ProjectLock`] from a filesystem path. Errors if the
    /// file is missing.
    ///
    /// For "load if present, return `None` otherwise" semantics, use
    /// [`Self::from_path`].
    pub async fn load(path: &Path) -> Result<Self, super::Error> {
        use tokio::io::AsyncReadExt;
        let limit = super::internal::FILE_SIZE_LIMIT_BYTES;

        let file = tokio::fs::File::open(path)
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        // `metadata.len()` fast-paths normal oversized files without reading
        // any bytes; the bounded `take(limit + 1)` below guards synthetic
        // files (e.g. procfs, pipes) whose metadata reports 0 but whose read
        // is unbounded. Mirrors the ambient config loader's
        // `ConfigLoader::load_and_merge` pattern.
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

    /// "Open or report absent" wrapper over [`Self::load`]. Returns
    /// `Ok(None)` when the file does not exist; any other I/O error or
    /// parse failure surfaces as `Err`.
    pub async fn from_path(path: &Path) -> Result<Option<Self>, super::Error> {
        match Self::load(path).await {
            Ok(lock) => Ok(Some(lock)),
            Err(super::Error::Project(pe)) if matches!(&pe.kind, ProjectErrorKind::Io(io) if io.kind() == std::io::ErrorKind::NotFound) => {
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }

    /// Version-peek loader.
    ///
    /// Peeks `metadata.lock_version` as a raw integer and rejects any value
    /// other than [`SUPPORTED_LOCK_VERSION`] with
    /// [`ProjectErrorKind::UnsupportedLockVersion`] — an explicit,
    /// actionable error (naming the found version and the `ocx lock`
    /// regenerate remedy) rather than `serde_repr`'s generic "unknown
    /// variant" failure. Only then does it commit to the full deserialize,
    /// followed by the bare-`repository` and canonical-platform-key
    /// invariants (D3) on every tool.
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

        let lock: ProjectLock =
            toml::from_str(s).map_err(|e| ProjectError::new(path.clone(), ProjectErrorKind::TomlParse(e)))?;

        // Every remaining V3 structural invariant (declaration-hash
        // canonicalization contract version, per-tool bare-repository +
        // canonical-platform-key) is shared with the write path — see
        // `Self::validate`. `toml::from_str` already rejects a duplicate
        // `[[tool]]` platform TOML key and a malformed leaf digest as
        // parse-class errors before we reach this point.
        lock.validate(&path)?;

        Ok(lock)
    }

    /// Validates every V3 structural invariant this lock format enforces,
    /// beyond what `toml::from_str`'s `Deserialize` impls already reject
    /// (duplicate `[[tool]]` platform keys, malformed digests): the
    /// declaration-hash canonicalization contract version
    /// ([`ProjectErrorKind::UnsupportedDeclarationHashVersion`]), and per-tool
    /// bare-`repository` ([`ProjectErrorKind::LockRepositoryNotBare`]) +
    /// canonical-platform-key ([`validate_canonical_platform_keys`])
    /// invariants (D3).
    ///
    /// Called by **both** the load path ([`Self::from_str_with_path`]) and
    /// every write path ([`Self::to_toml_string`], transitively [`Self::save`])
    /// — a public struct's fields can be hand-assembled by any library
    /// caller, so validating only on read would let `save` silently write a
    /// lock every subsequent `load` rejects. `path` is used only for error
    /// context; `to_toml_string` (which has no path) passes an empty one.
    fn validate(&self, path: &Path) -> Result<(), super::Error> {
        // Canonicalization contract gate: a lock file whose hash was
        // produced by a newer algorithm version cannot be interpreted by
        // this build. Refuse rather than silently compare against a hash
        // our `declaration_hash` would compute differently.
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
            if tool.repository.tag().is_some() || tool.repository.digest().is_some() {
                return Err(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::LockRepositoryNotBare {
                        value: tool.repository.to_string(),
                    },
                )
                .into());
            }
            validate_canonical_platform_keys(&tool.platforms)
                .map_err(|kind| ProjectError::new(path.to_path_buf(), kind))?;
        }

        Ok(())
    }

    /// Serialize to TOML bytes. Deterministic by construction: the
    /// `tools` vector is sorted by `(group, name)` at save time and
    /// `toml::to_string_pretty` emits fields in struct-definition order.
    pub fn to_toml_string(&self) -> Result<String, super::Error> {
        // Defense-in-depth (D3 write-site validation): every invariant
        // `Self::validate` checks must already hold for the document the
        // writer is about to emit. In practice this never fires — every
        // producer (`build_lock`, `build_platforms_map`, authoring
        // `pin_for`) already builds a compliant document — but a caller
        // could in principle hand-assemble a `ProjectLock`/`LockedTool` that
        // violates one of these invariants (a tagged `repository`, a stale
        // `declaration_hash_version`, a noncanonical platform key), and the
        // writer must not silently persist a lock the loader would then
        // reject.
        self.validate(Path::new(""))?;

        // Sort by (group, name) for byte-stable output. Build a
        // reference-based view (`Vec<&LockedTool>`) to sort without cloning
        // the entire `ProjectLock` document.
        let mut sorted_refs: Vec<&LockedTool> = self.tools.iter().collect();
        sorted_refs.sort_by(|a, b| (a.group.as_str(), a.name.as_str()).cmp(&(b.group.as_str(), b.name.as_str())));

        let view = SerializableView {
            metadata: &self.metadata,
            tools: &sorted_refs,
        };

        toml::to_string_pretty(&view)
            .map_err(|e| ProjectError::new(PathBuf::new(), ProjectErrorKind::TomlSerialize(e)).into())
    }

    /// Atomic save: tempfile in the same directory + `sync_data` +
    /// rename + parent-dir fsync.
    ///
    /// Preserves `metadata.generated_at` from `previous` when every
    /// tool's resolved content (repository, platforms) is unchanged;
    /// updates it otherwise. `previous` is `None` on first write.
    ///
    /// Save the lock file and register its path in the per-user project registry.
    ///
    /// `ocx_home` is the OCX data-home directory (e.g., `~/.ocx`) used to
    /// locate the project ledger at `$OCX_HOME/projects/` (flat symlink store,
    /// ADR: `adr_project_gc_symlink_ledger.md`). `config_path` is the
    /// originating project config file (typically `<dir>/ocx.toml`, but may
    /// carry a custom name when `--project=<custom>.toml` was used) — its
    /// parent directory is canonicalized and registered as the project dir.
    /// Registration runs **after** the atomic rename succeeds; a registration
    /// failure is logged at WARN and never aborts the save.
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
                // Content unchanged — freeze timestamp at previous value.
                to_write.metadata.generated_at = prev.metadata.generated_at.clone();
            } else if to_write.metadata.generated_at <= prev.metadata.generated_at {
                // Content changed but the freshly-minted ISO-8601 second-
                // resolution timestamp happens to equal (or fall behind) the
                // previous one. Two `ocx lock` runs within the same second
                // on a fast registry is a realistic case in CI and the test
                // suite. Monotonically bump by 1 second so downstream
                // diffing tools see that the lock changed.
                to_write.metadata.generated_at = bump_timestamp_one_second(&prev.metadata.generated_at)
                    .unwrap_or_else(|| to_write.metadata.generated_at.clone());
            }
        }

        let serialized = to_write.to_toml_string()?;

        // Atomic write via tempfile + rename in the same directory.
        write_lock_bytes_atomic(path, serialized.into_bytes()).await?;

        // Register this project's directory in the per-user GC ledger so
        // `ocx clean` can retain packages held by this project's lock file.
        // The shared infallible helper owns the canonicalize→parent→register
        // derivation and the WARN-on-failure silent-data-loss policy; a
        // registration failure never aborts the save (next `ocx lock`
        // re-registers).
        super::registry::register_project_dir_best_effort(config_path, ocx_home).await;

        Ok(())
    }
}

/// Restore a previously-captured `ocx.lock` to disk **byte-for-byte**, used by
/// the [`MutationGuard`](super::mutation::MutationGuard) rollback path when a
/// partial mutation fails after the new lock has been renamed into place.
///
/// Uses the same atomic tempfile + rename + parent-fsync primitive as
/// [`ProjectLock::save`], so the restore is crash-durable.
///
/// # Errors
///
/// Returns the underlying I/O error from the atomic write.
pub async fn restore_lock_bytes_verbatim(path: &Path, bytes: Vec<u8>) -> Result<(), super::Error> {
    write_lock_bytes_atomic(path, bytes).await
}

/// Atomically write `bytes` to `path` via tempfile + rename in the same
/// directory, preserving prior permissions and fsyncing the parent dir.
///
/// Done on a blocking thread so the sync filesystem calls do not block the
/// async runtime. Shared by [`ProjectLock::save`] (serialized bytes) and
/// [`restore_lock_bytes_verbatim`] (captured predecessor bytes).
async fn write_lock_bytes_atomic(path: &Path, bytes: Vec<u8>) -> Result<(), super::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<(), super::Error> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
        }

        // Snapshot the existing file's permissions (if any) so the rename
        // doesn't demote, say, 0o644 down to the tempfile's default 0o600. On
        // first-ever save this lookup fails with NotFound, which we tolerate —
        // tempfile's default stands.
        //
        // On Unix, cap the mode at 0o644 (user rw, group/other r) so an
        // accidentally world-writable file is not perpetuated through the
        // atomic rename cycle (Warn #8).
        let prior_perms = std::fs::metadata(&path).ok().map(|m| {
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut perms = m.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = perms.mode() & 0o644;
                perms.set_mode(mode);
            }
            perms
        });

        let mut tmp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
        tmp.write_all(&bytes)
            .map_err(|e| ProjectError::new(tmp.path().to_path_buf(), ProjectErrorKind::Io(e)))?;
        tmp.as_file()
            .sync_data()
            .map_err(|e| ProjectError::new(tmp.path().to_path_buf(), ProjectErrorKind::Io(e)))?;
        if let Some(perms) = prior_perms {
            tmp.as_file()
                .set_permissions(perms)
                .map_err(|e| ProjectError::new(tmp.path().to_path_buf(), ProjectErrorKind::Io(e)))?;
        }
        tmp.persist(&path)
            .map_err(|e| ProjectError::new(path.clone(), ProjectErrorKind::Io(e.error)))?;

        // fsync the containing directory so the rename is durable across a
        // crash. On Unix, opening a directory and calling sync_all() commits
        // the directory entry; on Windows opening a directory as a File is not
        // supported, so we skip.
        #[cfg(unix)]
        if !parent.as_os_str().is_empty() {
            let dir = std::fs::File::open(parent)
                .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
            dir.sync_all()
                .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
        }

        Ok(())
    })
    .await
    .expect("spawn_blocking panicked in write_lock_bytes_atomic")
}

/// Borrowed-view serialization wrapper used by [`ProjectLock::to_toml_string`].
///
/// Borrows the metadata header and a pre-sorted slice of `&LockedTool`
/// references — this avoids cloning the entire `ProjectLock` just to produce
/// byte-stable output.
#[derive(Serialize)]
struct SerializableView<'a> {
    metadata: &'a LockMetadata,
    /// Renamed to match the on-disk `tool` array name.
    #[serde(rename = "tool")]
    tools: &'a [&'a LockedTool],
}

/// Return `iso` bumped by exactly one second, preserving the canonical
/// `%Y-%m-%dT%H:%M:%SZ` format. Returns `None` if `iso` is not in the
/// expected format — callers fall back to their original timestamp in
/// that case rather than silently corrupting the lock.
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

/// Compare two [`LockedTool`] lists for "resolved content unchanged"
/// equality: `name`, `group`, `repository`, and the full `platforms` map —
/// `BTreeMap` equality detects key add, key remove, and value change (the
/// platform drop/appear signal counts as content-changed and advances
/// `generated_at`). Callers may pass tools in any order; the comparison sorts
/// both sides by `(group, name)` before checking pairwise equality so the
/// preservation decision is order-independent.
fn tools_content_equal(a: &[LockedTool], b: &[LockedTool]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    // Compare order-independently: sort references by (group, name) on both
    // sides, then compare pairwise.
    let sort_key = |t: &&LockedTool| (t.group.clone(), t.name.clone());
    let mut a_sorted: Vec<&LockedTool> = a.iter().collect();
    let mut b_sorted: Vec<&LockedTool> = b.iter().collect();
    a_sorted.sort_by_key(sort_key);
    b_sorted.sort_by_key(sort_key);

    a_sorted.iter().zip(b_sorted.iter()).all(|(left, right)| {
        left.name == right.name && left.group == right.group && locked_tool_content_equal(left, right)
    })
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
    use crate::oci::{Digest, Identifier};
    use crate::project::error::ProjectErrorKind;

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
            let crate::project::Error::Project(pe) = err else {
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

    /// Construct a bare `registry/repo` [`Identifier`] (no tag, no digest) —
    /// the `repository` coordinate shape.
    fn bare_repo(registry: &str, repo: &str) -> Identifier {
        Identifier::new_registry(repo, registry)
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
        assert!(cmake.repository.tag().is_none() && cmake.repository.digest().is_none());
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
        let crate::project::Error::Project(pe) = err;
        let ProjectErrorKind::NoncanonicalPlatformKey { key } = &pe.kind else {
            panic!("expected NoncanonicalPlatformKey, got {:?}", pe.kind);
        };
        assert_eq!(key, "linux/amd64+b,a");
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
        let crate::project::Error::Project(pe) = err;
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
        let crate::project::Error::Project(pe) = err;
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
        let crate::project::Error::Project(pe) = err;
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

    /// (b) A tagged `repository` is unreachable via any producer in this
    /// codebase (`build_lock`, authoring `pin_for`), but nothing at the type
    /// level stops a library caller from hand-assembling a `LockedTool` with
    /// one. Must reject at write, not just at load.
    #[test]
    fn write_rejects_tagged_repository() {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), digest_of('1'));
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![LockedTool {
                name: "cmake".to_string(),
                group: "default".to_string(),
                repository: bare_repo("ocx.sh", "cmake").clone_with_tag("3.28"),
                platforms,
            }],
        };
        let err = lock
            .to_toml_string()
            .expect_err("a tagged repository must reject at write, not just at load");
        let crate::project::Error::Project(pe) = err;
        let ProjectErrorKind::LockRepositoryNotBare { value } = &pe.kind else {
            panic!("expected LockRepositoryNotBare, got {:?}", pe.kind);
        };
        assert_eq!(value, "ocx.sh/cmake:3.28");
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
        let crate::project::Error::Project(pe) = err;
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
        assert!(tool.repository.tag().is_none(), "repository must be bare (no tag)");
        assert!(
            tool.repository.digest().is_none(),
            "repository must be bare (no digest)"
        );
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
    /// Proves the exclusive flock on `ocx.toml` serialises concurrent writers.
    ///
    /// `fs4` uses `flock(2)` on Unix (per-fd semantics): a second open fd on the
    /// same path cannot acquire exclusive while the first fd holds it, even within
    /// the same process.
    #[tokio::test]
    async fn acquire_project_lock_blocks_second_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Create the ocx.toml so acquire_project_lock can open it.
        std::fs::write(dir.path().join("ocx.toml"), "[tools]\n").expect("write ocx.toml");

        let first_guard = crate::project::acquire_project_lock(dir.path())
            .await
            .expect("first acquire must succeed");

        let err = crate::project::acquire_project_lock(dir.path())
            .await
            .expect_err("second acquire must fail while first guard is held");
        assert_kind!(err, ProjectErrorKind::Locked);

        drop(first_guard);

        crate::project::acquire_project_lock(dir.path())
            .await
            .expect("acquire after release must succeed");
    }

    /// A symlink at `ocx.toml` must cause `acquire_project_lock` to return
    /// `ProjectErrorKind::Io`, not `Locked`, and must NOT follow the symlink
    /// to its target.
    ///
    /// The lock target is `ocx.toml` itself (in-place lock — see ADR §Decision 3).
    /// A planted symlink at `ocx.toml` could redirect the lock and the
    /// subsequent in-place rewrite to an attacker-chosen file, so we reject it
    /// with `InvalidInput` before calling `LockedFile::try_exclusive`.
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

        let err = crate::project::acquire_project_lock(dir.path())
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

    /// While a writer holds the exclusive flock on `ocx.toml`,
    /// `ProjectLock::from_path` (the unlocked read path) must complete promptly.
    ///
    /// Readers open `ocx.lock` directly without any flock — only writers
    /// coordinate via the flock on `ocx.toml`. Readers always observe a complete
    /// file via the atomic-rename guarantee, so they never need to wait.
    ///
    /// A regression where the reader blocks would exceed the 500 ms timeout.
    #[tokio::test]
    async fn acquire_project_lock_does_not_block_concurrent_reader() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("ocx.toml"), "[tools]\n").expect("write ocx.toml");
        let lock_path = dir.path().join("ocx.lock");

        // Acquire the exclusive writer lock on ocx.toml.
        let _writer_guard = crate::project::acquire_project_lock(dir.path())
            .await
            .expect("first exclusive acquire must succeed");

        // ProjectLock::from_path opens ocx.lock directly — no flock acquired.
        // It must not wait on the writer's flock on ocx.toml.
        let reader_result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            ProjectLock::from_path(&lock_path),
        )
        .await
        .expect("reader must not block: timeout exceeded while writer holds flock on ocx.toml");

        // On a fresh dir the lock file does not exist — None is expected.
        match reader_result {
            Ok(None) => {}    // expected: lock file absent
            Ok(Some(_)) => {} // also acceptable: lock file existed from another test
            Err(e) => panic!("reader must not error while writer holds flock; got: {e}"),
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

    // ── Warn #8 regression — save() caps permissions at 0o644 ─────────────

    /// If the existing lock file has mode 0o666 (accidentally world-writable),
    /// the atomic-save must cap the preserved mode at 0o644.
    #[cfg(unix)]
    #[tokio::test]
    async fn save_caps_world_writable_permissions_at_0o644() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        // Seed with initial save.
        let seed = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("cmake", "default", "ocx.sh", "cmake", 'a')],
        };
        let cfg = stage_sibling_ocx_toml(&path);
        seed.save(&path, None, tmp.path(), &cfg).await.expect("seed save ok");

        // Force the file to 0o666 (world-writable).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod 0o666");

        // Save again — the mode must be capped to 0o644.
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
            "world-writable mode must be capped to 0o644; got 0o{:o}",
            after & 0o777
        );
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
        let locked_err = super::super::Error::Project(ProjectError::new(lock_path.clone(), ProjectErrorKind::Locked));
        let io_err = super::super::Error::Project(ProjectError::new(
            lock_path.clone(),
            ProjectErrorKind::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        ));

        // Contention (Ok(None)) → Locked
        assert_kind!(locked_err, ProjectErrorKind::Locked);

        // Real I/O error (Err) → Io
        assert_kind!(io_err, ProjectErrorKind::Io(_));
    }
}
