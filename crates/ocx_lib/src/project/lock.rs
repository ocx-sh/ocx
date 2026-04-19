// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx.lock` schema: the machine-written, committed lock file.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use super::error::{ProjectError, ProjectErrorKind};
use crate::oci::PinnedIdentifier;

/// Lock file version discriminant.
///
/// Serialized as a bare integer via `serde_repr`. Unknown values fail
/// deserialization with a structured error (no silent fallback). When a
/// future on-disk format is introduced, add `V2 = 2` alongside `V1`;
/// existing v1 files continue to deserialize.
///
/// Intentionally NOT `#[non_exhaustive]` — this is an internal on-disk
/// discriminant, not a public library enum, and the project convention
/// omits `#[non_exhaustive]` on internal non-error enums so matches stay
/// total across the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum LockVersion {
    /// Version 1 of the `ocx.lock` on-disk format.
    V1 = 1,
}

/// Top-level `ocx.lock` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLock {
    /// Lock metadata header (version, declaration hash, generator, etc.).
    pub metadata: LockMetadata,

    /// All locked tools in the project. Sorted by (group, name) at write
    /// time for byte-stable output.
    #[serde(default, rename = "tool")]
    pub tools: Vec<LockedTool>,
}

/// Lock metadata header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockMetadata {
    /// On-disk schema version (currently always [`LockVersion::V1`]).
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
/// `name` is the TOML key used in the developer's `ocx.toml` (the local
/// binding); `group` is `"default"` for entries from the top-level
/// `[tools]` table or the named `[group.<name>]` key. `pinned` carries
/// the registry/repo + content digest the resolver locked to. The
/// advisory tag inside `pinned` is stripped by the writer (see
/// [`PinnedIdentifier::strip_advisory`]) so the on-disk form is the
/// canonical `registry/repo@digest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockedTool {
    /// Local binding name (TOML key from `ocx.toml`, e.g. `cmake`).
    pub name: String,
    /// Owning group. `"default"` for entries from the top-level
    /// `[tools]` table; otherwise the named `[group.*]` key.
    pub group: String,
    /// Resolved registry/repo + content digest. The advisory tag is
    /// stripped at write time — see [`Self`] for the on-disk shape.
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
        // Size-cap check before read: a pathologically large lock file is a
        // sanity-check failure (corruption, misconfiguration, or a merge
        // mistake), not a meaningful input to parse. Matches the ambient
        // config loader's 64 KiB cap.
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
        let size = metadata.len();
        if size > super::internal::FILE_SIZE_LIMIT_BYTES {
            return Err(ProjectError::new(
                path.to_path_buf(),
                ProjectErrorKind::FileTooLarge {
                    size,
                    limit: super::internal::FILE_SIZE_LIMIT_BYTES,
                },
            )
            .into());
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e)))?;
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

    fn from_str_with_path(s: &str, path: PathBuf) -> Result<Self, super::Error> {
        let lock: Self =
            toml::from_str(s).map_err(|e| ProjectError::new(path.clone(), ProjectErrorKind::TomlParse(e)))?;

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
        // Strip advisory tags + sort by (group, name) for byte-stable
        // output. Cloning here is cheap relative to the serialization
        // cost and keeps `&self` pure.
        //
        // Tag-stripping policy is project-lock-specific. The sibling
        // `resolve.json` writer (`package_manager::tasks::pull`) keeps
        // tags verbatim — it captures the install-time identifier as an
        // audit trail, not a canonical pin. See plan_project_toolchain.md
        // §7.4 before harmonising the two policies.
        let mut sorted = self.clone();
        for tool in &mut sorted.tools {
            tool.pinned = tool.pinned.strip_advisory();
        }
        sorted
            .tools
            .sort_by(|a, b| (a.group.as_str(), a.name.as_str()).cmp(&(b.group.as_str(), b.name.as_str())));

        toml::to_string_pretty(&sorted)
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
    pub async fn save(&self, path: &Path, previous: Option<&Self>) -> Result<(), super::Error> {
        let mut to_write = self.clone();
        if let Some(prev) = previous
            && tools_content_equal(&to_write.tools, &prev.tools)
        {
            to_write.metadata.generated_at = prev.metadata.generated_at.clone();
        }

        let serialized = to_write.to_toml_string()?;

        // Atomic write via tempfile + rename in the same directory. Done on
        // a blocking thread so the sync filesystem calls do not block the
        // async runtime.
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || -> Result<(), super::Error> {
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
            }

            // Snapshot the existing file's permissions (if any) so the
            // rename doesn't demote, say, 0o644 down to the tempfile's
            // default 0o600. On first-ever save this lookup fails with
            // NotFound, which we tolerate — tempfile's default stands.
            let prior_perms = std::fs::metadata(&path).ok().map(|m| m.permissions());

            let mut tmp = tempfile::NamedTempFile::new_in(parent)
                .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
            tmp.write_all(serialized.as_bytes())
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

            // fsync the containing directory so the rename is durable
            // across a crash. On Unix, opening a directory and calling
            // sync_all() commits the directory entry; on Windows opening
            // a directory as a File is not supported, so we skip.
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
        .expect("spawn_blocking panicked in ProjectLock::save")?;

        Ok(())
    }
}

/// Compare two [`LockedTool`] lists for "resolved content unchanged"
/// equality.
///
/// Excludes the advisory tag inside [`PinnedIdentifier`] — a tag rename
/// that still resolves to the same digest must NOT bump `generated_at`.
/// Callers may pass tools in any order; the comparison sorts both sides
/// by `(group, name)` before checking pairwise equality so the
/// preservation decision is order-independent.
fn tools_content_equal(a: &[LockedTool], b: &[LockedTool]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a_sorted: Vec<&LockedTool> = a.iter().collect();
    let mut b_sorted: Vec<&LockedTool> = b.iter().collect();
    a_sorted.sort_by(|x, y| (x.group.as_str(), x.name.as_str()).cmp(&(y.group.as_str(), y.name.as_str())));
    b_sorted.sort_by(|x, y| (x.group.as_str(), x.name.as_str()).cmp(&(y.group.as_str(), y.name.as_str())));
    a_sorted
        .iter()
        .zip(b_sorted.iter())
        .all(|(x, y)| x.name == y.name && x.group == y.group && x.pinned.eq_content(&y.pinned))
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

    fn locked_tool(name: &str, group: &str, pinned_id: PinnedIdentifier) -> LockedTool {
        LockedTool {
            name: name.to_string(),
            group: group.to_string(),
            pinned: pinned_id,
        }
    }

    fn sample_metadata() -> LockMetadata {
        LockMetadata {
            lock_version: LockVersion::V1,
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
        let lock = ProjectLock::from_toml_str(&full_lock_toml()).expect("full lock parses");
        assert_eq!(lock.tools.len(), 2);

        let cmake = lock.tools.iter().find(|t| t.name == "cmake").expect("cmake present");
        assert_eq!(cmake.group, "default");
        assert_eq!(cmake.pinned.repository(), "cmake");
        assert_eq!(cmake.pinned.registry(), "ocx.sh");

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
    fn load_rejects_future_lock_version() {
        // `lock_version` is a `serde_repr` enum; unknown discriminants
        // fail deserialization at the serde layer and surface as a
        // TomlParse-class error. There is no dedicated
        // `UnsupportedLockVersion` variant: `serde_repr` owns the
        // structural rejection and forwarding would be dead code.
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
        let err = ProjectLock::from_toml_str(&toml_str).expect_err("unknown lock_version must reject");
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
            tools: vec![locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, '1'))],
        };
        let first = lock.to_toml_string().expect("first serialization");
        let reparsed = ProjectLock::from_toml_str(&first).expect("first reparse");
        let second = reparsed.to_toml_string().expect("second serialization");
        assert_eq!(first, second, "second pass must be byte-identical");
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
                locked_tool("zlib", "default", pinned("ocx.sh", "zlib", None, '3')),
                locked_tool("cmake", "ci", pinned("ocx.sh", "cmake", None, '1')),
                locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, '2')),
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
            tools: vec![locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, 'a'))],
        };

        // `self.tools` is content-equal to `prev.tools` per
        // `PinnedIdentifier::eq_content`, but `self.metadata.generated_at`
        // differs — the save must preserve prev's timestamp.
        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2099-12-31T23:59:59Z".to_string(),
                ..sample_metadata()
            },
            tools: prev.tools.clone(),
        };

        next.save(&path, Some(&prev)).await.expect("save ok");
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
            tools: vec![locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, 'a'))],
        };

        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-06-01T12:00:00Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool(
                "cmake",
                "default",
                // Different digest byte ⇒ different content.
                pinned("ocx.sh", "cmake", None, 'b'),
            )],
        };

        next.save(&path, Some(&prev)).await.expect("save ok");
        let reloaded = ProjectLock::load(&path).await.expect("reload ok");
        assert_ne!(
            reloaded.metadata.generated_at, "2026-01-01T00:00:00Z",
            "generated_at must change when digests change"
        );
    }

    #[tokio::test]
    async fn generated_at_preserved_when_only_tag_changes() {
        // Plan §7.1 + §6: the advisory `tag` carried inside
        // [`PinnedIdentifier`] must NOT bust `generated_at`
        // preservation. Only the resolved content (registry,
        // repository, digest) — surfaced via
        // [`PinnedIdentifier::eq_content`] — contributes to the
        // "digests unchanged" signal.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ocx.lock");

        let prev = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2026-01-01T00:00:00Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool(
                "cmake",
                "default",
                pinned("ocx.sh", "cmake", Some("3.28"), 'a'),
            )],
        };

        // Same registry/repo/digest — only the advisory `tag` moved.
        // `generated_at` must be preserved.
        let next = ProjectLock {
            metadata: LockMetadata {
                generated_at: "2099-12-31T23:59:59Z".to_string(),
                ..sample_metadata()
            },
            tools: vec![locked_tool(
                "cmake",
                "default",
                pinned("ocx.sh", "cmake", Some("3.29"), 'a'),
            )],
        };

        next.save(&path, Some(&prev)).await.expect("save ok");
        let reloaded = ProjectLock::load(&path).await.expect("reload ok");
        assert_eq!(
            reloaded.metadata.generated_at, "2026-01-01T00:00:00Z",
            "generated_at must be preserved when only the advisory tag moved"
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

        let tool_a = locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, 'a'));
        let tool_b = locked_tool("ninja", "default", pinned("ocx.sh", "ninja", None, 'b'));

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

        next.save(&path, Some(&prev)).await.expect("save ok");
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
            tools: vec![locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, 'a'))],
        };
        seed.save(&path, None).await.expect("seed save ok");
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
            tools: vec![locked_tool("ninja", "default", pinned("ocx.sh", "ninja", None, 'b'))],
        };
        let err = clobber.save(&path, None).await.expect_err("save must fail");
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
            tools: vec![locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, 'a'))],
        };
        lock.save(&path, None).await.expect("save ok");

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
            tools: vec![locked_tool("cmake", "default", pinned("ocx.sh", "cmake", None, 'a'))],
        };
        seed.save(&path, None).await.expect("seed save ok");

        // User (or a prior tool) relaxes the mode to 0o644.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod 0o644");
        let before = std::fs::metadata(&path).expect("meta before").permissions().mode();
        assert_eq!(before & 0o777, 0o644, "precondition: file is 0o644");

        // Overwrite with a fresh save — mode must survive.
        let next = ProjectLock {
            metadata: sample_metadata(),
            tools: vec![locked_tool("ninja", "default", pinned("ocx.sh", "ninja", None, 'b'))],
        };
        next.save(&path, None).await.expect("save ok");

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

    #[tokio::test]
    async fn save_strips_advisory_tag_on_locked_tools() {
        // Plan §6 step 4: "Apply strip_advisory() on every pinned
        // identifier stored in the lock." A LockedTool constructed
        // from a PinnedIdentifier that carries an advisory tag should
        // be persisted as registry/repo@digest (no `:tag`) on the
        // wire. Round-trip via load → tools[0].pinned has no tag.
        let tmp = tempfile::tempdir().expect("tempdir");
        let lock_path = tmp.path().join("ocx.lock");

        let pinned_with_tag = pinned("ocx.sh", "cmake", Some("3.28"), 'a');
        // Sanity-check the fixture: the advisory tag is actually on the
        // pinned identifier we're about to persist.
        assert_eq!(pinned_with_tag.tag(), Some("3.28"));

        let lock = ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V1,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", sha256_of('0')),
                generated_by: "ocx test".to_string(),
                generated_at: "2026-04-20T00:00:00Z".to_string(),
            },
            tools: vec![LockedTool {
                name: "cmake".to_string(),
                group: "default".to_string(),
                pinned: pinned_with_tag,
            }],
        };

        lock.save(&lock_path, None).await.expect("save succeeds");

        let on_disk = tokio::fs::read_to_string(&lock_path).await.expect("read");
        assert!(
            !on_disk.contains(":3.28@"),
            "advisory tag must be stripped on save; got: {on_disk}"
        );

        let reloaded = ProjectLock::load(&lock_path).await.expect("reload succeeds");
        assert_eq!(reloaded.tools.len(), 1);
        let reloaded_pinned = &reloaded.tools[0].pinned;
        assert!(
            reloaded_pinned.tag().is_none(),
            "reloaded pinned must have no advisory tag; got: {reloaded_pinned:?}"
        );
        // Digest preserved.
        assert_eq!(
            reloaded_pinned.digest().to_string(),
            format!("sha256:{}", sha256_of('a')),
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
}
