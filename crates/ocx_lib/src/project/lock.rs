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
use crate::oci::{Digest, Identifier, PinnedIdentifier};

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
/// Serialized as a bare integer via `serde_repr`. Unknown values fail
/// deserialization with a structured error (no silent fallback). An older
/// ocx meeting a `V2` lock sees an unknown discriminant and rejects it
/// cleanly (the loader surfaces "lock written by a newer ocx; upgrade
/// ocx").
///
/// `V2` is the only shape the writer emits; `V1` is read-only (legacy
/// locks migrate forward on the next write — there is no `V1` writer).
///
/// Intentionally NOT `#[non_exhaustive]` — this is an internal on-disk
/// discriminant, not a public library enum, and the project convention
/// omits `#[non_exhaustive]` on internal non-error enums so matches stay
/// total across the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum LockVersion {
    /// Version 1 of the `ocx.lock` on-disk format (read-only legacy shape:
    /// a single `pinned` image-index digest per tool).
    V1 = 1,
    /// Version 2 of the `ocx.lock` on-disk format (the only written shape:
    /// bare `repository` coordinates plus an available-only per-platform
    /// leaf-digest map).
    V2 = 2,
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
            "description": "OCX lock format version (lock-format version 2). Written locks are always 2.",
            "enum": [2]
        })
    }
}

/// Top-level `ocx.lock` document (in-memory model).
///
/// Serialization to the V2 on-disk shape is handled by
/// [`Self::to_toml_string`] via the borrowed-view projection; deserialization
/// is handled by the version-peek loader [`Self::from_str_with_path`]. The
/// JSON Schema is generated from the on-disk V2 projection [`ProjectLockV2`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectLock {
    /// Lock metadata header (version, declaration hash, generator, etc.).
    pub metadata: LockMetadata,

    /// All locked tools in the project. Sorted by (group, name) at write
    /// time for byte-stable output.
    pub tools: Vec<LockedTool>,
}

/// Owned V2 on-disk projection of [`ProjectLock`], used only for JSON Schema
/// generation (`ocx_schema`) and as the V2 deserialize target.
///
/// The runtime serializer projects [`ProjectLock`] through the borrowed
/// [`SerializableView`] rather than this owned type (to avoid cloning); this
/// type exists so `schemars::JsonSchema` and the V2 read path have a concrete
/// owned shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectLockV2 {
    /// Lock metadata header.
    pub metadata: LockMetadata,
    /// All locked tools in V2 on-disk shape.
    #[serde(default, rename = "tool")]
    pub tools: Vec<LockedToolV2>,
}

/// Owned V1 on-disk projection of [`ProjectLock`], used only as the V1 read
/// target (never serialized).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLockV1 {
    /// Lock metadata header.
    pub metadata: LockMetadata,
    /// All locked tools in V1 on-disk shape.
    #[serde(default, rename = "tool")]
    pub tools: Vec<LockedToolV1>,
}

/// Minimal header-only projection used to peek `metadata.lock_version`
/// before committing to a full V1 or V2 deserialize.
#[derive(Debug, Clone, Deserialize)]
struct LockVersionPeek {
    metadata: LockVersionPeekMetadata,
}

/// The single field the version peek needs from `[metadata]`.
#[derive(Debug, Clone, Deserialize)]
struct LockVersionPeekMetadata {
    lock_version: LockVersion,
}

/// Lock metadata header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockMetadata {
    /// On-disk schema version. Written locks are always [`LockVersion::V2`];
    /// [`LockVersion::V1`] is accepted on read (legacy locks).
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
    /// between two `ocx lock` runs; updated otherwise. The advisory tag
    /// inside [`PinnedIdentifier`] is ignored by the comparison via
    /// [`PinnedIdentifier::eq_content`].
    pub generated_at: String,
}

/// One locked tool entry — identified by the local binding `(group,
/// name)`.
///
/// This is the **in-memory** model. `name` is the TOML key used in the
/// developer's `ocx.toml` (the local binding); `group` is `"default"` for
/// entries from the top-level `[tools]` table or the named
/// `[group.<name>]` key. `resolution` carries either a legacy V1 index
/// pin or the V2 per-platform leaf map (see [`LockedResolution`]).
///
/// The on-disk shapes are [`LockedToolV1`] (read-only) and
/// [`LockedToolV2`] (the only written shape); the version-peek loader
/// normalizes both into this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedTool {
    /// Local binding name (TOML key from `ocx.toml`, e.g. `cmake`).
    pub name: String,
    /// Owning group. `"default"` for entries from the top-level
    /// `[tools]` table; otherwise the named `[group.*]` key.
    pub group: String,
    /// The resolved coordinates, normalized from whichever on-disk shape
    /// the loader read.
    pub resolution: LockedResolution,
}

/// In-memory resolution shape for a [`LockedTool`].
///
/// The loader reads both on-disk formats into this enum; read-path
/// consumers branch on it. The writer serializes **only** the
/// [`PerPlatform`](Self::PerPlatform) arm — a [`LegacyIndex`](Self::LegacyIndex)
/// reaching the writer is a bug (write commands transcribe V1 → V2 first).
///
/// Intentionally NOT `#[non_exhaustive]` — internal on-disk discriminant,
/// per the project convention for non-error enums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockedResolution {
    /// Read from a V1 lock (legacy on-disk `pinned`). Only the outer
    /// image-index digest is known → install/run/GC use the legacy
    /// index-digest + `Index::select` path. Never serialized: upgraded to
    /// [`PerPlatform`](Self::PerPlatform) (pin-preserving) before any write.
    LegacyIndex(PinnedIdentifier),
    /// V2 — the only written shape. Bare repository coordinates plus an
    /// available-only per-platform leaf-digest map.
    PerPlatform {
        /// Registry/repo coordinates shared by every platform leaf — a
        /// bare [`Identifier`] with no tag and no digest. The per-platform
        /// pull id is reconstructed as `repository.clone_with_digest(leaf)`.
        repository: Identifier,
        /// Per-platform leaf digests for shipped platforms only. Key = a
        /// lossless, injective `Platform` key string (see
        /// [`crate::oci::Platform::lock_key`]); a platform the publisher
        /// does not ship is encoded by absence of its key.
        platforms: BTreeMap<String, Digest>,
    },
}

/// V2 on-disk shape (read + write). The only format the writer emits.
///
/// `repository` is a bare [`Identifier`] (no tag, no digest); the outer
/// image-index digest is intentionally NOT stored. `platforms` records one
/// leaf digest per shipped platform, keyed by a lossless, injective
/// `Platform` string. Absence of a key means the publisher ships no such
/// leaf — there is no `Unavailable` marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedToolV2 {
    /// Local binding name (TOML key from `ocx.toml`, e.g. `cmake`).
    pub name: String,
    /// Owning group. `"default"` for entries from the top-level
    /// `[tools]` table; otherwise the named `[group.*]` key.
    pub group: String,
    /// Bare registry/repo coordinates shared by every platform leaf.
    pub repository: Identifier,
    /// Available-only per-platform leaf digests, keyed by a lossless,
    /// injective `Platform` key string. `BTreeMap` for byte-stable output
    /// without requiring `Ord` on `Platform`.
    pub platforms: BTreeMap<String, Digest>,
}

/// V1 on-disk shape (legacy locks; never written).
///
/// Retained only so the loader can read a committed V1 lock and normalize
/// it into [`LockedResolution::LegacyIndex`]. No code path serializes this
/// type — write commands transcribe forward to [`LockedToolV2`] first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LockedToolV1 {
    /// Local binding name (TOML key from `ocx.toml`, e.g. `cmake`).
    pub name: String,
    /// Owning group. `"default"` for entries from the top-level
    /// `[tools]` table; otherwise the named `[group.*]` key.
    pub group: String,
    /// Resolved registry/repo + content (image-index) digest.
    pub pinned: PinnedIdentifier,
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
    /// Peeks `metadata.lock_version` (lenient header parse) and deserializes
    /// the matching per-tool on-disk shape ([`ProjectLockV1`] or
    /// [`ProjectLockV2`]), normalizing both into the in-memory
    /// [`LockedResolution`] model. An unknown `lock_version` is rejected by
    /// `serde_repr` at the peek (clean "upgrade ocx").
    fn from_str_with_path(s: &str, path: PathBuf) -> Result<Self, super::Error> {
        // Peek the version first so a V1 lock no longer trips a raw
        // `TomlParse` against the V2 shape.
        let peek: LockVersionPeek =
            toml::from_str(s).map_err(|e| ProjectError::new(path.clone(), ProjectErrorKind::TomlParse(e)))?;

        let lock = match peek.metadata.lock_version {
            LockVersion::V1 => Self::from_v1_str(s, &path)?,
            LockVersion::V2 => Self::from_v2_str(s, &path)?,
        };

        // Canonicalization contract gate: a lock file whose hash was
        // produced by a newer algorithm version cannot be interpreted by
        // this build. Refuse rather than silently compare against a hash
        // our `declaration_hash` would compute differently.
        if lock.metadata.declaration_hash_version != super::hash::DECLARATION_HASH_VERSION {
            return Err(ProjectError::new(
                path,
                ProjectErrorKind::UnsupportedDeclarationHashVersion {
                    version: lock.metadata.declaration_hash_version,
                },
            )
            .into());
        }

        Ok(lock)
    }

    /// Deserialize the V1 on-disk shape and normalize into the in-memory
    /// model ([`LockedResolution::LegacyIndex`] per tool).
    fn from_v1_str(s: &str, path: &Path) -> Result<Self, super::Error> {
        let doc: ProjectLockV1 =
            toml::from_str(s).map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::TomlParse(e)))?;
        let tools = doc
            .tools
            .into_iter()
            .map(|t| LockedTool {
                name: t.name,
                group: t.group,
                resolution: LockedResolution::LegacyIndex(t.pinned),
            })
            .collect();
        Ok(Self {
            metadata: doc.metadata,
            tools,
        })
    }

    /// Deserialize the V2 on-disk shape and normalize into the in-memory
    /// model ([`LockedResolution::PerPlatform`] per tool). Enforces the
    /// bare-`repository` invariant and rejects malformed/duplicate platform
    /// keys (Codex C6).
    fn from_v2_str(s: &str, path: &Path) -> Result<Self, super::Error> {
        // `toml::from_str` rejects duplicate table keys (Codex C6 duplicate
        // platform entry) and a malformed leaf digest (Digest deserialize
        // fails) as parse-class errors before we ever see the document.
        let doc: ProjectLockV2 =
            toml::from_str(s).map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::TomlParse(e)))?;
        let mut tools = Vec::with_capacity(doc.tools.len());
        for t in doc.tools {
            // Bare-repository invariant: `repository` deserializes through
            // `Identifier::parse`, which accepts a tag or digest. A V2 lock
            // must carry only registry/repo coordinates.
            if t.repository.tag().is_some() || t.repository.digest().is_some() {
                return Err(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::LockRepositoryNotBare {
                        value: t.repository.to_string(),
                    },
                )
                .into());
            }
            tools.push(LockedTool {
                name: t.name,
                group: t.group,
                resolution: LockedResolution::PerPlatform {
                    repository: t.repository,
                    platforms: t.platforms,
                },
            });
        }
        Ok(Self {
            metadata: doc.metadata,
            tools,
        })
    }

    /// Serialize to TOML bytes. Deterministic by construction: the
    /// `tools` vector is sorted by `(group, name)` at save time and
    /// `toml::to_string_pretty` emits fields in struct-definition order.
    ///
    /// Advisory tags are stripped from `tools[*].pinned` for the
    /// canonical on-disk representation — `registry/repo@digest` is the
    /// only shape the writer emits. Callers may construct a
    /// [`ProjectLock`] whose pinned identifiers carry tags (e.g. for
    /// in-memory testing); those tags simply do not survive a round
    /// trip through this method.
    pub fn to_toml_string(&self) -> Result<String, super::Error> {
        // Sort by (group, name) for byte-stable output. Build a
        // reference-based view (`Vec<&LockedTool>`) to sort without cloning
        // the entire `ProjectLock` document; the serialization wrapper
        // (`SerializableView`) borrows the metadata and the sorted tool
        // slice and projects each tool's `PerPlatform` arm into a
        // `LockedToolV2` on the wire. A `LegacyIndex` reaching the writer is
        // a bug (write commands transcribe V1 → V2 first); the projection
        // hits `unreachable!()`.
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
    /// tool's pinned content (registry, repository, digest) is
    /// unchanged; updates it otherwise. `previous` is `None` on first
    /// write. The advisory tag inside [`PinnedIdentifier`] is ignored
    /// by the comparison via [`PinnedIdentifier::eq_content`] — a tag
    /// rewrite that still resolves to the same digest does not bust
    /// `generated_at`.
    ///
    /// The advisory tag is stripped from every locked tool's pinned
    /// identifier on the wire (see [`Self::to_toml_string`]) —
    /// `registry/repo@digest` is the canonical on-disk form.
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
/// the [`MutationGuard`](super::mutation::MutationGuard) rollback path.
///
/// When a partial mutation fails after the new lock has been renamed into
/// place, the predecessor must be restored verbatim — including a V1
/// (`LegacyIndex`) lock, which the V2 writer
/// ([`ProjectLock::to_toml_string`]) refuses to serialize (`unreachable!()`).
/// Writing the captured original bytes back sidesteps the V2 serializer
/// entirely, so a rolled-back mutation leaves a committed V1 lock exactly as
/// it was on disk (still V1).
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
/// async runtime. Shared by [`ProjectLock::save`] (serialized V2 bytes) and
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
/// Mirrors the V2 on-disk shape (`#[serde(rename = "tool")]` for the array
/// name) but borrows the metadata header and a pre-sorted slice of
/// `&LockedTool` references — this avoids cloning the entire `ProjectLock`
/// just to produce byte-stable output. The `tool` field projects each
/// reference through [`LockedToolV2View`], which emits the bare `repository`
/// plus the available-only `platforms` map. A [`LockedResolution::LegacyIndex`]
/// reaching the projection is a bug (`unreachable!()`).
#[derive(Serialize)]
struct SerializableView<'a> {
    metadata: &'a LockMetadata,
    /// Renamed to match the V2 on-disk `tool` array name. Wrapped via a
    /// custom `serialize_with` so we can project each in-memory `LockedTool`
    /// onto the V2 wire shape.
    #[serde(rename = "tool", serialize_with = "serialize_tool_views")]
    tools: &'a [&'a LockedTool],
}

/// Projection of a [`LockedTool`] for the V2 on-disk wire format: borrows the
/// `name`/`group` strings and the bare `repository`, and borrows the
/// available-only `platforms` leaf map.
#[derive(Serialize)]
struct LockedToolV2View<'a> {
    name: &'a str,
    group: &'a str,
    repository: &'a Identifier,
    platforms: &'a BTreeMap<String, Digest>,
}

/// Serialize the `&[&LockedTool]` slice as a TOML array of [`LockedToolV2View`]s.
///
/// Only the [`LockedResolution::PerPlatform`] arm is serializable; a
/// [`LockedResolution::LegacyIndex`] reaching the writer is a bug — write
/// commands transcribe V1 → V2 before any save.
fn serialize_tool_views<S>(tools: &&[&LockedTool], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;

    // Project each tool's `PerPlatform` arm onto a `LockedToolV2View`; a
    // `LegacyIndex` reaching the writer is a bug (write commands transcribe
    // V1 → V2 first).
    let mut seq = serializer.serialize_seq(Some(tools.len()))?;
    for t in *tools {
        let view = match &t.resolution {
            LockedResolution::PerPlatform { repository, platforms } => LockedToolV2View {
                name: &t.name,
                group: &t.group,
                repository,
                platforms,
            },
            LockedResolution::LegacyIndex(_) => {
                unreachable!("LegacyIndex reached the V2 writer; write commands transcribe V1 → V2 first")
            }
        };
        seq.serialize_element(&view)?;
    }
    seq.end()
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
/// equality.
///
/// For V2 [`LockedResolution::PerPlatform`] entries this compares `name`,
/// `group`, `repository`, and the full `platforms` map — `BTreeMap`
/// equality detects key add, key remove, and value change (the platform
/// drop/appear signal counts as content-changed and advances
/// `generated_at`). For legacy V1 [`LockedResolution::LegacyIndex`]
/// entries the comparison excludes the advisory tag via
/// [`PinnedIdentifier::eq_content`]. Callers may pass tools in any order;
/// the comparison sorts both sides by `(group, name)` before checking
/// pairwise equality so the preservation decision is order-independent.
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

    a_sorted
        .iter()
        .zip(b_sorted.iter())
        .all(|(left, right)| resolution_content_equal(left, right))
}

/// Compare two [`LockedTool`]s for content equality (name + group +
/// resolution). Delegates the resolution comparison to
/// [`resolutions_content_equal`].
fn resolution_content_equal(left: &LockedTool, right: &LockedTool) -> bool {
    left.name == right.name
        && left.group == right.group
        && resolutions_content_equal(&left.resolution, &right.resolution)
}

/// Resolved-content equality for two [`LockedResolution`]s, ignoring advisory
/// metadata.
///
/// For `PerPlatform`, the `repository` and the full `platforms` `BTreeMap` are
/// compared (`BTreeMap` equality detects key add, key remove, and value change
/// — the platform drop/appear signal counts as content-changed). For
/// `LegacyIndex`, the advisory tag is excluded via
/// [`PinnedIdentifier::eq_content`]. A `PerPlatform`/`LegacyIndex` shape
/// mismatch is never content-equal.
///
/// Single source of the resolution-equality predicate shared by the lock
/// writer's `generated_at`-preservation check ([`tools_content_equal`]) and
/// the `ocx upgrade --check` verify-only path.
pub fn resolutions_content_equal(left: &LockedResolution, right: &LockedResolution) -> bool {
    match (left, right) {
        (LockedResolution::LegacyIndex(a), LockedResolution::LegacyIndex(b)) => a.eq_content(b),
        (
            LockedResolution::PerPlatform {
                repository: ra,
                platforms: pa,
            },
            LockedResolution::PerPlatform {
                repository: rb,
                platforms: pb,
            },
        ) => ra == rb && pa == pb,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    //! Contract-first tests for [`ProjectLock`] parsing, serialization, and
    //! atomic save semantics.
    //!
    //! Assertions prefer typed
    //! [`super::super::error::ProjectErrorKind`] matches over string matches.
    //! Determinism + the strip-advisory write rule are permanent contracts.
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

    /// Build a [`PinnedIdentifier`] for `registry/repo:tag@sha256:<byte * 64>`.
    fn pinned(registry: &str, repo: &str, tag: Option<&str>, digest_byte: char) -> PinnedIdentifier {
        let mut id = Identifier::new_registry(repo, registry);
        if let Some(t) = tag {
            id = id.clone_with_tag(t);
        }
        let id = id.clone_with_digest(Digest::Sha256(sha256_of(digest_byte)));
        PinnedIdentifier::try_from(id).expect("identifier carries a digest")
    }

    /// Construct a 64-hex sha256 [`Digest`] filled with the given byte.
    fn digest_of(byte: char) -> Digest {
        Digest::Sha256(sha256_of(byte))
    }

    /// Construct a bare `registry/repo` [`Identifier`] (no tag, no digest) —
    /// the V2 `repository` coordinate shape.
    fn bare_repo(registry: &str, repo: &str) -> Identifier {
        Identifier::new_registry(repo, registry)
    }

    /// A V1-shaped [`LockedTool`] carrying a [`LockedResolution::LegacyIndex`].
    ///
    /// Used by legacy-read assertions and by `tools_content_equal` /
    /// `generated_at`-preservation tests that exercise the
    /// `PinnedIdentifier::eq_content` comparison path.
    fn locked_tool(name: &str, group: &str, pinned_id: PinnedIdentifier) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            resolution: LockedResolution::LegacyIndex(pinned_id),
        }
    }

    /// A V2-shaped [`LockedTool`] carrying a [`LockedResolution::PerPlatform`]
    /// with a single `"linux/amd64"` leaf keyed by its lossless platform key.
    ///
    /// The writer only serializes this arm; write/round-trip tests use it.
    fn locked_tool_v2(name: &str, group: &str, registry: &str, repo: &str, leaf_byte: char) -> LockedTool {
        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), digest_of(leaf_byte));
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            resolution: LockedResolution::PerPlatform {
                repository: bare_repo(registry, repo),
                platforms,
            },
        }
    }

    /// Read a [`LockedTool`]'s legacy index pin, panicking if it is V2.
    /// Keeps legacy-read assertions terse.
    fn legacy_pin(tool: &LockedTool) -> &PinnedIdentifier {
        match &tool.resolution {
            LockedResolution::LegacyIndex(pinned) => pinned,
            LockedResolution::PerPlatform { .. } => {
                panic!(
                    "expected LegacyIndex resolution, got PerPlatform for tool '{}'",
                    tool.name
                )
            }
        }
    }

    /// Read a [`LockedTool`]'s V2 per-platform map, panicking if it is V1.
    fn per_platform(tool: &LockedTool) -> (&Identifier, &BTreeMap<String, Digest>) {
        match &tool.resolution {
            LockedResolution::PerPlatform { repository, platforms } => (repository, platforms),
            LockedResolution::LegacyIndex(_) => {
                panic!(
                    "expected PerPlatform resolution, got LegacyIndex for tool '{}'",
                    tool.name
                )
            }
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

    /// V2 metadata header — the only shape the writer emits, so every
    /// serialize/save/round-trip test uses it.
    fn sample_metadata() -> LockMetadata {
        LockMetadata {
            lock_version: LockVersion::V2,
            declaration_hash_version: 1,
            declaration_hash: format!("sha256:{}", sha256_of('d')),
            generated_by: "ocx 0.3.0".to_string(),
            generated_at: "2026-04-19T00:00:00Z".to_string(),
        }
    }

    // --- Fixtures -----------------------------------------------------------

    fn minimal_lock_toml() -> String {
        format!(
            r#"
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"
"#,
            abc = sha256_of('a'),
        )
    }

    fn full_lock_toml() -> String {
        // Two tools across two groups, three different digests.
        format!(
            r#"
[metadata]
lock_version = 1
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
pinned = "ocx.sh/cmake@sha256:{aaa}"

[[tool]]
name = "shellcheck"
group = "ci"
pinned = "ocx.sh/shellcheck@sha256:{bbb}"
"#,
            cafe = sha256_of('c'),
            aaa = sha256_of('1'),
            bbb = sha256_of('2'),
        )
    }

    /// A V2 on-disk fixture: one tool, bare `repository`, two shipped
    /// platform leaves keyed by their lossless platform key. `windows/amd64`
    /// is deliberately absent (publisher ships no such leaf).
    fn v2_lock_toml() -> String {
        format!(
            r#"
[metadata]
lock_version = 2
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

    // --- Parsing ------------------------------------------------------------

    #[test]
    fn parse_minimal_lock_ok() {
        let lock = ProjectLock::from_toml_str(&minimal_lock_toml()).expect("minimal lock parses");
        assert_eq!(lock.metadata.lock_version, LockVersion::V1);
        assert_eq!(lock.metadata.declaration_hash_version, 1);
        assert!(lock.tools.is_empty());
    }

    #[test]
    fn parse_full_lock_ok() {
        // `full_lock_toml` is a V1 fixture (`lock_version = 1`, `pinned`
        // field), so every tool normalizes to `LockedResolution::LegacyIndex`.
        let lock = ProjectLock::from_toml_str(&full_lock_toml()).expect("full lock parses");
        assert_eq!(lock.tools.len(), 2);

        let cmake = lock.tools.iter().find(|t| t.name == "cmake").expect("cmake present");
        assert_eq!(cmake.group, "default");
        let cmake_pin = legacy_pin(cmake);
        assert_eq!(cmake_pin.repository(), "cmake");
        assert_eq!(cmake_pin.registry(), "ocx.sh");

        let sc = lock
            .tools
            .iter()
            .find(|t| t.name == "shellcheck")
            .expect("shellcheck present");
        assert_eq!(sc.group, "ci");
    }

    #[test]
    fn parse_unknown_top_level_field_rejects() {
        let abc = sha256_of('a');
        let toml_str = format!(
            r#"
unknown = "key"

[metadata]
lock_version = 1
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
    fn parse_empty_v2_lock_ok() {
        // `lock_version = 2` is now a known discriminant: a V2 lock carrying
        // only `[metadata]` (no `[[tool]]`) parses to an empty tool list.
        // (`lock_version = 3` is the unknown-discriminant case — covered by
        // `read_rejects_unknown_lock_version`.)
        let abc = sha256_of('a');
        let toml_str = format!(
            r#"
[metadata]
lock_version = 2
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"
"#
        );
        let lock = ProjectLock::from_toml_str(&toml_str).expect("empty V2 lock parses");
        assert_eq!(lock.metadata.lock_version, LockVersion::V2);
        assert!(lock.tools.is_empty());
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
lock_version = 1
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

    // --- Serialization + determinism ---------------------------------------

    #[test]
    fn roundtrip_deterministic() {
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", '1')],
        };
        let first = lock.to_toml_string().expect("first serialization");
        let reparsed = ProjectLock::from_toml_str(&first).expect("first reparse");
        let second = reparsed.to_toml_string().expect("second serialization");
        assert_eq!(first, second, "second pass must be byte-identical");
    }

    /// Idempotent re-lock must be byte-identical: serializing the same V2
    /// content twice (with the same `generated_at`) yields identical bytes.
    /// This is the cross-OS reproducibility + "no `generated_at` churn"
    /// contract (ADR Validation: "an unchanged re-lock is byte-identical").
    #[test]
    fn idempotent_relock_is_byte_identical() {
        let build = || ProjectLock {
            metadata: sample_metadata(),
            tools: vec![
                locked_tool_v2("cmake", "default", "ocx.sh", "cmake", '1'),
                locked_tool_v2("ninja", "default", "ocx.sh", "ninja", '2'),
            ],
        };
        let first = build().to_toml_string().expect("first serialization");
        let second = build().to_toml_string().expect("second serialization");
        assert_eq!(first, second, "two identical V2 locks must serialize byte-identically");
    }

    #[test]
    fn tools_written_sorted_by_name_then_group() {
        // Input order is intentionally unsorted. Output must be:
        // cmake/ci, cmake/default, zlib/default. The plan §1 sort key
        // is `(group, name)`; the test name predates the rename and is
        // preserved verbatim per the migration map.
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![
                locked_tool_v2("zlib", "default", "ocx.sh", "zlib", '3'),
                locked_tool_v2("cmake", "ci", "ocx.sh", "cmake", '1'),
                locked_tool_v2("cmake", "default", "ocx.sh", "cmake", '2'),
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
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
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
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
        };

        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-06-01T12:00:00Z".to_string(),
                ..sample_metadata()
            },
            // Different leaf digest byte ⇒ different content.
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'b')],
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
    async fn generated_at_preserved_when_only_legacy_tag_changes() {
        // The advisory `tag` carried inside a V1 [`PinnedIdentifier`]
        // ([`LockedResolution::LegacyIndex`]) must NOT bust `generated_at`
        // preservation: only the resolved content (registry, repository,
        // digest) — surfaced via [`PinnedIdentifier::eq_content`] —
        // contributes to the "content unchanged" signal. This exercises
        // `tools_content_equal` directly (no writer round-trip, since the
        // writer never serializes a LegacyIndex).
        let prev = vec![locked_tool(
            "cmake",
            "default",
            pinned("ocx.sh", "cmake", Some("3.28"), 'a'),
        )];
        // Same registry/repo/digest — only the advisory tag moved.
        let next = vec![locked_tool(
            "cmake",
            "default",
            pinned("ocx.sh", "cmake", Some("3.29"), 'a'),
        )];
        assert!(
            tools_content_equal(&prev, &next),
            "legacy entries differing only in advisory tag must compare content-equal"
        );
    }

    #[tokio::test]
    async fn generated_at_preserved_when_v2_platforms_unchanged() {
        // The V2 content-equality signal is `repository` + the full
        // `platforms` map. A re-lock that produces the identical map keeps
        // `generated_at` frozen (ADR Validation: unchanged re-lock is
        // byte-identical, no `generated_at` churn).
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let prev = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
        };
        // Rebuilt independently with the same repository + leaf digest.
        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2099-12-31T23:59:59Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
        };

        let cfg = stage_sibling_ocx_toml(&path);
        next.save(&path, Some(&prev), tmp.path(), &cfg).await.expect("save ok");
        let reloaded = ProjectLock::load(&path).await.expect("reload ok");
        assert_eq!(
            reloaded.metadata.generated_at, "2026-01-01T00:00:00Z",
            "generated_at must be preserved when the V2 platforms map is unchanged"
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

        let tool_a = locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a');
        let tool_b = locked_tool_v2("ninja", "default", "ocx.sh", "ninja", 'b');

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
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
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
            tools: vec![locked_tool_v2("ninja", "default", "ocx.sh", "ninja", 'b')],
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
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
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
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
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
            tools: vec![locked_tool_v2("ninja", "default", "ocx.sh", "ninja", 'b')],
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
             lock_version = 1\n\
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

    // ── V2 write shape ─────────────────────────────────────────────────────

    /// Structural guard (ADR Validation: "No V2 lock contains an index
    /// digest"). A V2 lock serializes the bare `repository` (no tag, no
    /// digest) plus the per-platform leaf map — the outer image-index digest
    /// is never written. We assert the on-disk form carries the repository
    /// coordinate, the per-platform key, and the leaf digest, and that the
    /// `[tool.platforms]` table shape (not a `pinned = "..."` line) is used.
    #[tokio::test]
    async fn save_v2_writes_repository_and_platforms_no_index_digest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_path = tmp.path().join("ocx.lock");

        let leaf = digest_of('a');
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
        };

        let cfg = stage_sibling_ocx_toml(&lock_path);
        lock.save(&lock_path, None, tmp.path(), &cfg)
            .await
            .expect("save succeeds");

        let on_disk = tokio::fs::read_to_string(&lock_path).await.expect("read");
        assert!(
            on_disk.contains("repository = \"ocx.sh/cmake\""),
            "V2 lock must record the bare repository coordinate; got:\n{on_disk}"
        );
        assert!(
            on_disk.contains("\"linux/amd64\""),
            "V2 lock must record the per-platform key; got:\n{on_disk}"
        );
        assert!(
            on_disk.contains(&leaf.to_string()),
            "V2 lock must record the leaf digest; got:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("pinned ="),
            "V2 lock must NOT carry a legacy `pinned` index-digest line; got:\n{on_disk}"
        );

        // Round-trips back into a PerPlatform entry with the same leaf.
        let reloaded = ProjectLock::load(&lock_path).await.expect("reload succeeds");
        assert_eq!(reloaded.tools.len(), 1);
        let (repo, platforms) = per_platform(&reloaded.tools[0]);
        assert_eq!(repo.registry(), "ocx.sh");
        assert_eq!(repo.repository(), "cmake");
        assert!(repo.tag().is_none(), "V2 repository must be bare (no tag)");
        assert!(repo.digest().is_none(), "V2 repository must be bare (no digest)");
        assert_eq!(platforms.get("linux/amd64"), Some(&leaf));
    }

    /// The writer serializes only the `PerPlatform` arm. Handing it a
    /// `LegacyIndex` (a V1 entry that reached the writer without being
    /// transcribed first) is a contract violation that must panic, not
    /// silently emit a malformed/legacy lock (ADR: writer only emits V2;
    /// write commands transcribe V1 → V2 first).
    #[test]
    #[should_panic(expected = "LegacyIndex")]
    fn serializer_panics_when_handed_legacy_index() {
        let lock = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, 'a'))],
        };
        // The `LegacyIndex` arm hits `unreachable!()` in the V2 projection.
        let _ = lock.to_toml_string();
    }

    /// Reading a V1 lock normalizes every tool to
    /// [`LockedResolution::LegacyIndex`] (ADR: read-both-V1/V2). The
    /// committed V1 lock keeps working on read paths without a forced
    /// upgrade.
    #[test]
    fn read_v1_normalizes_to_legacy_index() {
        let lock = ProjectLock::from_toml_str(&full_lock_toml()).expect("V1 lock parses");
        for tool in &lock.tools {
            assert!(
                matches!(tool.resolution, LockedResolution::LegacyIndex(_)),
                "V1 lock entry '{}' must normalize to LegacyIndex; got {:?}",
                tool.name,
                tool.resolution
            );
        }
    }

    /// Finding 3a — reading a V1 lock must NOT silently upgrade or mutate it.
    /// The in-memory `lock_version` stays [`LockVersion::V1`] (the upgrade
    /// happens only on the next *write*, not on read), and each entry's
    /// [`LockedResolution::LegacyIndex`] carries the exact on-disk `pinned`
    /// digest verbatim. A committed V1 lock keeps working on read paths with no
    /// forced upgrade and no read-path mutation (ADR: read-both-V1/V2).
    #[test]
    fn read_v1_preserves_version_and_pins_without_mutation() {
        let lock = ProjectLock::from_toml_str(&full_lock_toml()).expect("V1 lock parses");

        // The version metadata must remain V1 on read — no silent forward bump.
        assert_eq!(
            lock.metadata.lock_version,
            LockVersion::V1,
            "reading a V1 lock must keep lock_version V1 (upgrade happens on write, not read)"
        );

        // The carried pins must be the exact on-disk digests, unmutated.
        let cmake = lock
            .tools
            .iter()
            .find(|t| t.name == "cmake" && t.group == "default")
            .expect("cmake/default entry present");
        let LockedResolution::LegacyIndex(pinned) = &cmake.resolution else {
            panic!("V1 entry must normalize to LegacyIndex; got {:?}", cmake.resolution);
        };
        assert_eq!(
            pinned.digest(),
            Digest::Sha256(sha256_of('1')),
            "the V1 pinned digest must be read verbatim, not re-resolved or mutated"
        );
    }

    /// Reading a V2 lock normalizes every tool to
    /// [`LockedResolution::PerPlatform`] with the bare repository + leaf map
    /// (ADR: read-both-V1/V2; V2 reads the per-platform leaf directly).
    #[test]
    fn read_v2_normalizes_to_per_platform() {
        let lock = ProjectLock::from_toml_str(&v2_lock_toml()).expect("V2 lock parses");
        assert_eq!(lock.tools.len(), 1);
        let tool = &lock.tools[0];
        let (repo, platforms) = per_platform(tool);
        assert_eq!(repo.repository(), "cmake");
        assert!(
            repo.tag().is_none() && repo.digest().is_none(),
            "repository must be bare"
        );
        assert_eq!(platforms.get("linux/amd64"), Some(&digest_of('1')));
        assert_eq!(platforms.get("darwin/arm64"), Some(&digest_of('2')));
        // A platform the publisher does not ship is encoded by absence.
        assert!(
            !platforms.contains_key("windows/amd64"),
            "an unshipped platform must be absent from the map"
        );
    }

    /// `lock_version = 3` is an unknown discriminant. `serde_repr` rejects it
    /// structurally at the version peek, surfacing the clean "upgrade ocx"
    /// signal as a parse-class error (ADR: older ocx reading a V2 lock fails
    /// with a clean upgrade message; the same machinery rejects any future
    /// discriminant).
    #[test]
    fn read_rejects_unknown_lock_version() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 3
declaration_hash_version = 1
declaration_hash = "sha256:{abc}"
generated_by = "ocx 99.0.0"
generated_at = "2099-01-01T00:00:00Z"
"#,
            abc = sha256_of('a'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("unknown lock_version must reject");
        assert_kind!(err, ProjectErrorKind::TomlParse(_));
    }

    /// A V2 `repository` carrying a tag is non-bare and must be rejected at
    /// parse (Codex C6 bare-repository invariant) — never a panic.
    #[test]
    fn read_v2_rejects_non_bare_repository_with_tag() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 2
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake:3.28"

[tool.platforms]
"linux/amd64" = "sha256:{leaf}"
"#,
            cafe = sha256_of('c'),
            leaf = sha256_of('1'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("non-bare repository (tag) must reject");
        // Parse-class rejection; the exact kind may be TomlParse (deny_unknown
        // / invariant rejection) — assert it is an error, not a panic.
        let crate::project::Error::Project(_) = err;
    }

    /// A V2 `repository` carrying a digest is non-bare and must be rejected
    /// at parse (Codex C6) — never a panic.
    #[test]
    fn read_v2_rejects_non_bare_repository_with_digest() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 2
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake@sha256:{idx}"

[tool.platforms]
"linux/amd64" = "sha256:{leaf}"
"#,
            cafe = sha256_of('c'),
            idx = sha256_of('9'),
            leaf = sha256_of('1'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("non-bare repository (digest) must reject");
        let crate::project::Error::Project(_) = err;
    }

    /// A malformed platform leaf value (not a valid digest) must surface as a
    /// parse-class error, never a panic (Codex C6 parse invariants).
    #[test]
    fn read_v2_rejects_malformed_platform_digest() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 2
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"

[tool.platforms]
"linux/amd64" = "not-a-digest"
"#,
            cafe = sha256_of('c'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("malformed platform digest must reject");
        let crate::project::Error::Project(_) = err;
    }

    /// A duplicate platform key in the `[tool.platforms]` table must surface
    /// as a parse-class error, never a silent last-write-wins or panic
    /// (Codex C6: reject duplicate platform entry).
    #[test]
    fn read_v2_rejects_duplicate_platform_key() {
        let toml_str = format!(
            r#"
[metadata]
lock_version = 2
declaration_hash_version = 1
declaration_hash = "sha256:{cafe}"
generated_by = "ocx 0.3.0"
generated_at = "2026-04-19T00:00:00Z"

[[tool]]
name = "cmake"
group = "default"
repository = "ocx.sh/cmake"

[tool.platforms]
"linux/amd64" = "sha256:{a}"
"linux/amd64" = "sha256:{b}"
"#,
            cafe = sha256_of('c'),
            a = sha256_of('1'),
            b = sha256_of('2'),
        );
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("duplicate platform key must reject");
        let crate::project::Error::Project(_) = err;
    }

    // ── tools_content_equal: drop / appear / value-change ──────────────────

    /// `tools_content_equal` over V2 entries must treat a key ADD (publisher
    /// ships a new platform) as content-changed (ADR: the drop/appear signal
    /// counts as content-changed and advances `generated_at`).
    #[test]
    fn tools_content_equal_detects_platform_key_add() {
        let prev = vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')];

        let mut platforms = BTreeMap::new();
        platforms.insert("linux/amd64".to_string(), digest_of('a'));
        platforms.insert("darwin/arm64".to_string(), digest_of('b'));
        let next = vec![LockedTool {
            name: "cmake".to_string(),
            group: "default".to_string(),
            resolution: LockedResolution::PerPlatform {
                repository: bare_repo("ocx.sh", "cmake"),
                platforms,
            },
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
            resolution: LockedResolution::PerPlatform {
                repository: bare_repo("ocx.sh", "cmake"),
                platforms,
            },
        }];
        // Only the amd64 leaf survives.
        let next = vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')];

        assert!(
            !tools_content_equal(&prev, &next),
            "removing a platform key must register as content-changed"
        );
    }

    /// `tools_content_equal` must treat a leaf VALUE change (same platform,
    /// new digest) as content-changed.
    #[test]
    fn tools_content_equal_detects_leaf_value_change() {
        let prev = vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')];
        let next = vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'b')];
        assert!(
            !tools_content_equal(&prev, &next),
            "a changed leaf digest must register as content-changed"
        );
    }

    /// Two identical V2 maps built in different key-insertion order compare
    /// content-equal — `BTreeMap` canonicalizes ordering, so an unchanged
    /// re-lock is detected regardless of how the resolver assembled the map.
    #[test]
    fn tools_content_equal_identical_v2_maps_any_order_are_equal() {
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
            resolution: LockedResolution::PerPlatform {
                repository: bare_repo("ocx.sh", "cmake"),
                platforms: a,
            },
        }];
        let rhs = vec![LockedTool {
            name: "cmake".to_string(),
            group: "default".to_string(),
            resolution: LockedResolution::PerPlatform {
                repository: bare_repo("ocx.sh", "cmake"),
                platforms: b,
            },
        }];
        assert!(
            tools_content_equal(&lhs, &rhs),
            "identical V2 maps must compare content-equal regardless of insertion order"
        );
    }

    #[test]
    fn tools_content_equal_is_order_independent() {
        // The function must compare two tool lists by *content* regardless of
        // input order — `save()` relies on this to preserve `generated_at`
        // even when prior and current locks differ only in iteration order.
        let a = pinned("ocx.sh", "cmake", None, '1');
        let b = pinned("ocx.sh", "ninja", None, '2');

        let lhs = vec![
            locked_tool("cmake", "default", a.clone()),
            locked_tool("ninja", "default", b.clone()),
        ];
        let rhs = vec![locked_tool("ninja", "default", b), locked_tool("cmake", "default", a)];

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
            tools: vec![locked_tool_v2("cmake", "default", "ocx.sh", "cmake", 'a')],
        };
        let cfg = stage_sibling_ocx_toml(&path);
        seed.save(&path, None, tmp.path(), &cfg).await.expect("seed save ok");

        // Force the file to 0o666 (world-writable).
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod 0o666");

        // Save again — the mode must be capped to 0o644.
        let next = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool_v2("ninja", "default", "ocx.sh", "ninja", 'b')],
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
