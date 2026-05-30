// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Mutating writes to the project config file. Atomic full-file rewrite
//! under an exclusive advisory flock on the config file itself.
//!
//! Public mutation helpers ([`add_binding`], [`remove_binding`],
//! [`init_project`]) take the **resolved config file path** — typically
//! `<project_root>/ocx.toml` but may be `<project_root>/<custom>.toml`
//! when the caller passed `--project=<custom>.toml`. The flock target is
//! always the config file itself, never a hard-coded `ocx.toml`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::oci::Identifier;
use crate::project::Error;
use crate::project::error::{ProjectError, ProjectErrorKind};
use crate::project::project_lock::acquire_project_lock_for_file;

// ── TOML serialisation for ocx.toml ──────────────────────────────────────

/// Serialise `ProjectConfig` to a TOML string suitable for writing as
/// `ocx.toml`.
///
/// Uses `toml::to_string_pretty` so the output is human-readable.  Entries
/// inside each group are sorted by key for deterministic output.
fn config_to_toml_string(config: &crate::project::config::ProjectConfig) -> Result<String, Error> {
    toml::to_string_pretty(config)
        .map_err(|e| ProjectError::new(PathBuf::new(), ProjectErrorKind::TomlSerialize(e)).into())
}

/// Atomic write of `content` to `path` via a tempfile + rename in the same
/// directory — mirrors `ProjectLock::save`'s tempfile-and-rename pattern.
///
/// # Panics
///
/// Panics if `path` has no parent component. All callers pass an
/// already-resolved config-file path (typically `<project_root>/ocx.toml`,
/// or a custom `<project_root>/<custom>.toml` when `--project=<custom>.toml`
/// is in effect); both forms have a parent component.
fn atomic_write(path: &Path, content: &str) -> Result<(), Error> {
    // SAFETY: all callers resolve a config-file path with a parent component
    // (project root is always absolute when produced by the CLI shim, and
    // the in-tree tests construct their fixture under a tempdir).
    let parent = path
        .parent()
        .expect("config file path must have a parent (resolved by CLI shim or tempdir fixture)");
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
    }

    let mut tmp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| ProjectError::new(parent.to_path_buf(), ProjectErrorKind::Io(e)))?;
    tmp.write_all(content.as_bytes())
        .map_err(|e| ProjectError::new(tmp.path().to_path_buf(), ProjectErrorKind::Io(e)))?;
    tmp.as_file()
        .sync_data()
        .map_err(|e| ProjectError::new(tmp.path().to_path_buf(), ProjectErrorKind::Io(e)))?;
    tmp.persist(path)
        .map_err(|e| ProjectError::new(path.to_path_buf(), ProjectErrorKind::Io(e.error)))?;

    // Fsync the parent directory so the rename entry is durable.
    // parent is always non-empty (see SAFETY comment above).
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

// ── read-modify-write helpers ─────────────────────────────────────────────

/// Read and parse the project config file at `config_path`.
///
/// Enforces the same 64 KiB size cap used by
/// [`crate::project::config::ProjectConfig::from_path`] (via
/// `super::internal::FILE_SIZE_LIMIT_BYTES`). Oversized files are
/// rejected with [`ProjectErrorKind::FileTooLarge`] rather than read
/// into memory — defense-in-depth against pathological inputs during
/// the atomic read-modify-write cycle.
fn load_config(config_path: &Path) -> Result<crate::project::config::ProjectConfig, Error> {
    let limit = crate::project::internal::FILE_SIZE_LIMIT_BYTES;

    // Stat before reading — reject obviously oversized files without allocating.
    let meta = std::fs::metadata(config_path)
        .map_err(|e| ProjectError::new(config_path.to_path_buf(), ProjectErrorKind::Io(e)))?;
    if meta.len() > limit {
        return Err(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::FileTooLarge {
                size: meta.len(),
                limit,
            },
        )
        .into());
    }

    let text = std::fs::read_to_string(config_path)
        .map_err(|e| ProjectError::new(config_path.to_path_buf(), ProjectErrorKind::Io(e)))?;

    // Guard synthetic files (e.g. procfs) whose metadata reports 0 but whose
    // read may produce more bytes than the cap allows.
    if text.len() as u64 > limit {
        return Err(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::FileTooLarge {
                size: text.len() as u64,
                limit,
            },
        )
        .into());
    }

    crate::project::config::ProjectConfig::from_toml_str(&text)
}

/// Derive the binding key (TOML map key) from an identifier.
///
/// Per the `ocx.toml` schema, the binding key is the repository basename —
/// the last `/`-separated segment of the repository path.  For example,
/// `ghcr.io/acme/cmake` → `"cmake"`, `ocx.sh/shellcheck` → `"shellcheck"`.
///
/// Promoted to `pub` so CLI commands (`add.rs`, `remove.rs`) in `ocx_cli`
/// can reuse the same derivation instead of duplicating it inline.
pub fn binding_key(identifier: &Identifier) -> String {
    identifier
        .repository()
        .rsplit('/')
        .next()
        .unwrap_or_else(|| identifier.repository())
        .to_owned()
}

/// Validate a group name supplied via `--group`.
///
/// Valid names: non-empty, consist solely of ASCII alphanumeric characters,
/// hyphens (`-`), and underscores (`_`). Rejects `/`, `\`, NUL bytes,
/// `..` sequences, and any other character not in that set.
///
/// Also rejects the reserved keywords `default` and `all` — both are
/// reserved group selectors that cannot be declared as literal `[group.*]`
/// tables or used as group names in mutating commands (`ocx add`,
/// `ocx remove`). The error variant is `InvalidGroupName` (same class as
/// other unusable group names; no new variant needed per plan §Reserved Name
/// Enforcement).
fn validate_group_name(name: &str) -> bool {
    if name == super::internal::DEFAULT_GROUP || name == super::internal::ALL_GROUP {
        return false;
    }
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

// ── public API ────────────────────────────────────────────────────────────

/// Apply an `add` binding mutation to a [`crate::project::config::ProjectConfig`]
/// in memory.
///
/// Pure: validates inputs, mutates `config` in place, returns the derived
/// binding key on success. Performs NO filesystem I/O — callers handle
/// flock acquisition + atomic write separately (see
/// [`crate::project::MutationGuard`] for the transactional commit path
/// or [`add_binding`] for the legacy disk-touching API).
///
/// `path` is used solely for error context (`ProjectError::new(path, ...)`)
/// when surfacing structured errors; it is NOT read from or written to.
///
/// # Errors
///
/// Same set as [`add_binding`] minus the I/O / parse variants
/// (`Io`, `FileTooLarge`, `TomlParse`, `TomlSerialize`, `Locked`):
///
/// - [`ProjectErrorKind::InvalidGroupName`] — `group` contains invalid characters.
/// - [`ProjectErrorKind::BindingAlreadyExists`] — the derived key already
///   exists in the target group.
pub fn add_binding_in_memory(
    config: &mut crate::project::config::ProjectConfig,
    path: &Path,
    identifier: &Identifier,
    group: Option<&str>,
) -> Result<String, Error> {
    if let Some(group_name) = group
        && !validate_group_name(group_name)
    {
        return Err(Error::Project(ProjectError::new(
            path.to_path_buf(),
            ProjectErrorKind::InvalidGroupName {
                name: group_name.to_owned(),
            },
        )));
    }

    let key = binding_key(identifier);

    // Duplicate check: scoped to the target group only.
    match group {
        None => {
            if config.tools.contains_key(&key) {
                return Err(Error::Project(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::BindingAlreadyExists {
                        name: key,
                        group: "default".to_owned(),
                    },
                )));
            }
            config.tools.insert(key.clone(), identifier.clone());
        }
        Some(group_name) => {
            if config.groups.get(group_name).is_some_and(|g| g.contains_key(&key)) {
                return Err(Error::Project(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::BindingAlreadyExists {
                        name: key,
                        group: group_name.to_owned(),
                    },
                )));
            }
            config
                .groups
                .entry(group_name.to_owned())
                .or_default()
                .insert(key.clone(), identifier.clone());
        }
    }

    // Cache coherence: the in-place mutation invalidates any previously cached
    // declaration hash on `config`. Mutators that call this helper must NOT
    // reuse a stale cached hash for downstream gates.
    config.invalidate_declaration_hash_cache();
    Ok(key)
}

/// Append a binding to the project config file at `config_path`. If
/// `group` is `None`, lands in the implicit default `[tools]` table;
/// otherwise lands under `[group.<group>]`.
///
/// Acquires an exclusive advisory flock on the config file itself for
/// the duration of the read-modify-write cycle.
///
/// # Errors
///
/// - [`ProjectErrorKind::InvalidGroupName`] — `group` contains characters
///   outside `[a-zA-Z0-9_-]`, is empty, or contains path traversal sequences.
/// - [`ProjectErrorKind::Io`] — the config file could not be read or written.
/// - [`ProjectErrorKind::FileTooLarge`] — the config file exceeds the 64 KiB cap.
/// - [`ProjectErrorKind::TomlParse`] / [`ProjectErrorKind::TomlSerialize`] —
///   the config could not be parsed from or serialised back to TOML.
/// - [`ProjectErrorKind::BindingAlreadyExists`] — the derived binding key
///   already exists in the target group. The same name may exist in other
///   groups without error.
/// - [`ProjectErrorKind::Locked`] — another process holds the exclusive flock
///   on the config file; the caller should retry with backoff.
pub async fn add_binding(config_path: &Path, identifier: &Identifier, group: Option<&str>) -> Result<(), Error> {
    // Validate the group name before acquiring the lock so invalid input is
    // rejected cheaply — no filesystem operations needed.
    if let Some(group_name) = group
        && !validate_group_name(group_name)
    {
        return Err(Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::InvalidGroupName {
                name: group_name.to_owned(),
            },
        )));
    }

    let mut guard = acquire_project_lock_for_file(config_path).await?;

    let mut config = load_config(config_path)?;

    // Compose: in-memory mutation + write-back through the lock-owning handle.
    add_binding_in_memory(&mut config, config_path, identifier, group)?;

    let serialized = config_to_toml_string(&config)?;
    // Rewrite ocx.toml IN PLACE through the lock-owning handle. A tempfile +
    // rename would rotate the file's inode and strand the lock fd on the
    // orphan, breaking mutual exclusion on Windows (where LockFileEx is
    // per-handle and rename-over-open-file is refused). See project_lock.rs
    // module docs ("WHY IN-PLACE").
    guard.replace_bytes(serialized.as_bytes()).await.map_err(|e| {
        Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Io(std::io::Error::other(e)),
        ))
    })?;

    Ok(())
}

/// Remove a binding from the project config file at `config_path`.
///
/// When `group` is `Some("default")` or `Some("<name>")` the removal is
/// scoped to that specific group. When `group` is `None`, all groups are
/// searched: 0 hits → `BindingNotFound`, 1 hit → removed, 2+ hits →
/// `BindingAmbiguous` (caller should re-invoke with `--group`).
///
/// Match key = the binding name derived from `identifier` (typically the
/// repo basename, same key used by the TOML `[tools]` table).
///
/// Acquires an exclusive advisory flock on the config file itself for
/// the duration of the read-modify-write cycle.
///
/// # Errors
///
/// - [`ProjectErrorKind::Io`] — the config file could not be read or written.
/// - [`ProjectErrorKind::FileTooLarge`] — the config file exceeds the 64 KiB cap.
/// - [`ProjectErrorKind::TomlParse`] / [`ProjectErrorKind::TomlSerialize`] —
///   the config could not be parsed from or serialised back to TOML.
/// - [`ProjectErrorKind::BindingNotFound`] — the derived binding key was not
///   found in the targeted group (or any group when `group` is `None`).
/// - [`ProjectErrorKind::BindingAmbiguous`] — `group` is `None` and the
///   binding name appears in more than one group; pass `--group` to
///   disambiguate.
/// - [`ProjectErrorKind::Locked`] — another process holds the exclusive flock
///   on the config file; the caller should retry with backoff.
pub async fn remove_binding(config_path: &Path, identifier: &Identifier, group: Option<&str>) -> Result<(), Error> {
    let mut guard = acquire_project_lock_for_file(config_path).await?;

    let mut config = load_config(config_path)?;

    remove_binding_in_memory(&mut config, config_path, identifier, group)?;

    let serialized = config_to_toml_string(&config)?;
    // Rewrite in place through the lock-owning handle (see add_binding).
    guard.replace_bytes(serialized.as_bytes()).await.map_err(|e| {
        Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Io(std::io::Error::other(e)),
        ))
    })?;
    Ok(())
}

/// Apply a `remove` binding mutation to a [`crate::project::config::ProjectConfig`]
/// in memory.
///
/// Pure counterpart to [`remove_binding`] — mutates `config` in place,
/// performs NO filesystem I/O. `path` is used only for error context.
/// See [`add_binding_in_memory`] for the full library/CLI split rationale.
///
/// # Errors
///
/// - [`ProjectErrorKind::BindingNotFound`] — the derived key was not
///   found in the targeted group (or any group when `group` is `None`).
/// - [`ProjectErrorKind::BindingAmbiguous`] — `group` is `None` and the
///   binding appears in more than one group.
pub fn remove_binding_in_memory(
    config: &mut crate::project::config::ProjectConfig,
    path: &Path,
    identifier: &Identifier,
    group: Option<&str>,
) -> Result<(), Error> {
    let key = binding_key(identifier);

    match group {
        // ── explicit group: remove from that group only ───────────────────
        Some("default") => {
            if config.tools.remove(&key).is_none() {
                return Err(Error::Project(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::BindingNotFound { name: key },
                )));
            }
        }
        Some(group_name) => {
            let tools = config.groups.get_mut(group_name).filter(|g| g.contains_key(&key));
            if let Some(t) = tools {
                t.remove(&key);
            } else {
                return Err(Error::Project(ProjectError::new(
                    path.to_path_buf(),
                    ProjectErrorKind::BindingNotFound { name: key },
                )));
            }
        }
        // ── no group specified: search all ────────────────────────────────
        None => {
            let mut hits: Vec<String> = Vec::new();
            if config.tools.contains_key(&key) {
                hits.push("default".to_owned());
            }
            for group_name in config.groups.keys() {
                if config.groups[group_name].contains_key(&key) {
                    hits.push(group_name.clone());
                }
            }
            hits.sort();

            match hits.len() {
                0 => {
                    return Err(Error::Project(ProjectError::new(
                        path.to_path_buf(),
                        ProjectErrorKind::BindingNotFound { name: key },
                    )));
                }
                1 => {
                    let target = &hits[0];
                    if target == "default" {
                        config.tools.remove(&key);
                    } else {
                        config
                            .groups
                            .get_mut(target)
                            .expect("group must exist — just found it")
                            .remove(&key);
                    }
                }
                _ => {
                    return Err(Error::Project(ProjectError::new(
                        path.to_path_buf(),
                        ProjectErrorKind::BindingAmbiguous {
                            name: key,
                            groups: hits,
                        },
                    )));
                }
            }
        }
    }

    // Cache coherence: see `add_binding_in_memory` — any successful path
    // through this function mutated `config` and must drop the cached hash.
    config.invalidate_declaration_hash_cache();
    Ok(())
}

/// Create a minimal project config file at `config_path`. Idempotent
/// failure: returns an error variant if a file already exists. The
/// default content is one registry declaration line and an empty
/// `[tools]` table — non-interactive, backend-first per
/// `.claude/artifacts/research_cli_package_manager_conventions.md`
/// section 6.
///
/// `config_path` must be the **full path** to the config file (typically
/// `<project_root>/ocx.toml` but may be a custom name when the caller
/// intends `--project=<custom>.toml` end-to-end). Returns the same path
/// back on success so callers can confirm what was written.
///
/// For the bare-directory bootstrap case (no custom config name), use
/// [`init_project_at_default`] which appends `ocx.toml` to the directory.
///
/// # Errors
///
/// - [`ProjectErrorKind::ConfigAlreadyExists`] — the config file already
///   exists at the target path (or a dangling symlink points to that path —
///   both cases are blocked to prevent inadvertent writes through a symlink).
/// - [`ProjectErrorKind::Io`] — the target path could not be written.
/// - [`ProjectErrorKind::TomlSerialize`] — internal serialisation error (should
///   not occur with the static template content).
pub fn init_project(config_path: &Path) -> Result<PathBuf, Error> {
    // SAFETY: `symlink_metadata` is used instead of `exists()` so that a
    // dangling symlink at the target path is treated as "already exists" and
    // blocked. `exists()` follows symlinks and returns `false` for a dangling
    // symlink, which would silently overwrite the symlink target — a TOCTOU
    // risk when a malicious symlink is placed at the config file path between
    // the check and the write. `symlink_metadata().is_ok()` returns `true` for
    // both valid files and dangling symlinks, preventing writes in both cases.
    if config_path.symlink_metadata().is_ok() {
        return Err(Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::ConfigAlreadyExists {
                path: config_path.to_path_buf(),
            },
        )));
    }

    // Minimal non-interactive content: schema-server hint + registry
    // comment + empty `[tools]` table. The first-line comment is
    // recognized by the YAML/TOML language servers (`taplo`, `yaml-
    // language-server`) and IDE plugins (VS Code, Zed) as a pointer to
    // the canonical JSON Schema at the published URL — gives schema-
    // aware autocompletion and validation out of the box.
    // Intentionally kept under 10 non-blank lines per research §6.4.
    let content = "\
#:schema https://ocx.sh/schemas/project/v1.json
# OCX project toolchain — managed by `ocx add` / `ocx remove`
# registry = \"ocx.sh\"

[tools]
";

    atomic_write(config_path, content)?;
    Ok(config_path.to_path_buf())
}

/// Convenience wrapper for the bare-directory bootstrap case: creates
/// `<project_root>/ocx.toml` via [`init_project`].
///
/// Use this when the caller has a project directory and wants the
/// canonical `ocx.toml` filename. Use [`init_project`] directly when
/// the caller has a resolved config-file path that may be a custom name.
///
/// # Errors
///
/// Same as [`init_project`].
pub fn init_project_at_default(project_root: &Path) -> Result<PathBuf, Error> {
    init_project(&project_root.join("ocx.toml"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::oci::Identifier;
    use crate::project::{ProjectConfig, ProjectErrorKind};
    use crate::utility::fs::LockedFile;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn test_id(registry: &str, repo: &str, tag: &str) -> Identifier {
        Identifier::new_registry(repo, registry).clone_with_tag(tag)
    }

    fn write_minimal_toml(dir: &std::path::Path, body: &str) {
        fs::write(dir.join("ocx.toml"), body).unwrap();
    }

    fn reload_config(dir: &std::path::Path) -> ProjectConfig {
        let text = fs::read_to_string(dir.join("ocx.toml")).unwrap();
        ProjectConfig::from_toml_str(&text).expect("config must parse after mutation")
    }

    /// Resolve the canonical `<dir>/ocx.toml` path used by the post-Cluster B
    /// mutation API (which takes a config-file path, not a directory).
    fn toml(dir: &std::path::Path) -> std::path::PathBuf {
        dir.join("ocx.toml")
    }

    // ── add_binding ──────────────────────────────────────────────────────────

    /// Spec: mutate §5 bullet 1 — default group (group: None).
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_appends_to_default_group() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        add_binding(&toml(dir.path()), &id, None).await.unwrap();

        let cfg = reload_config(dir.path());
        assert!(
            cfg.tools.contains_key("cmake"),
            "cmake must appear in the default [tools] group"
        );
    }

    /// Spec: mutate §5 bullet 2 — named group.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_into_named_group() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        add_binding(&toml(dir.path()), &id, Some("ci")).await.unwrap();

        let cfg = reload_config(dir.path());
        assert!(
            cfg.groups.get("ci").map(|g| g.contains_key("cmake")).unwrap_or(false),
            "cmake must appear under [group.ci] after add_binding with group=ci"
        );
    }

    /// Spec: mutate §5 bullet 3 — duplicate binding in the same group returns BindingAlreadyExists.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_errors_on_same_group_duplicate() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        // Dup in default group → error when re-added to default.
        add_binding(&toml(dir.path()), &id, None).await.unwrap();
        let err = add_binding(&toml(dir.path()), &id, None)
            .await
            .expect_err("second add to default must fail");
        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::BindingAlreadyExists { group, .. } if group == "default")),
            "expected BindingAlreadyExists with group=default; got: {err}"
        );

        // Dup in [group.ci] → error only when re-added to [group.ci],
        // NOT when added to [group.staging].
        let id2 = test_id("example.com", "ninja", "1.11");
        add_binding(&toml(dir.path()), &id2, Some("ci")).await.unwrap();
        let err2 = add_binding(&toml(dir.path()), &id2, Some("ci"))
            .await
            .expect_err("second add to ci must fail");
        assert!(
            matches!(&err2, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::BindingAlreadyExists { group, .. } if group == "ci")),
            "expected BindingAlreadyExists with group=ci; got: {err2}"
        );
        // Same name in [group.staging] must succeed.
        add_binding(&toml(dir.path()), &id2, Some("staging"))
            .await
            .expect("same name in staging must succeed");
    }

    /// Allow same binding name in default group AND a named group.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_allows_same_name_default_and_named_group() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        let id2 = test_id("example.com", "cmake", "3.29");

        add_binding(&toml(dir.path()), &id, None)
            .await
            .expect("add cmake to default must succeed");
        add_binding(&toml(dir.path()), &id2, Some("ci"))
            .await
            .expect("add cmake to [group.ci] must succeed");

        let cfg = reload_config(dir.path());
        assert!(cfg.tools.contains_key("cmake"), "cmake in [tools]");
        assert!(
            cfg.groups.get("ci").map(|g| g.contains_key("cmake")).unwrap_or(false),
            "cmake in [group.ci]"
        );
    }

    // ── remove_binding ───────────────────────────────────────────────────────

    /// Spec: mutate §5 bullet 4 — remove from default group (no --group).
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_drops_default_group_entry() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        add_binding(&toml(dir.path()), &id, None).await.unwrap();

        remove_binding(&toml(dir.path()), &id, None).await.unwrap();

        let cfg = reload_config(dir.path());
        assert!(
            !cfg.tools.contains_key("cmake"),
            "cmake must be gone from [tools] after remove_binding"
        );
    }

    /// Spec: mutate §5 bullet 5 — remove from named group (no --group).
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_drops_named_group_entry() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        add_binding(&toml(dir.path()), &id, Some("ci")).await.unwrap();

        remove_binding(&toml(dir.path()), &id, None).await.unwrap();

        let cfg = reload_config(dir.path());
        let gone = cfg.groups.get("ci").map(|g| !g.contains_key("cmake")).unwrap_or(true);
        assert!(gone, "cmake must be gone from [group.ci] after remove_binding");
    }

    /// Spec: mutate §5 bullet 6 — missing binding returns BindingNotFound.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_errors_when_missing() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        let err = remove_binding(&toml(dir.path()), &id, None)
            .await
            .expect_err("remove_binding on absent entry must fail");

        let is_not_found = matches!(
            &err,
            Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::BindingNotFound { .. })
        );
        assert!(is_not_found, "expected BindingNotFound; got: {err}");
    }

    /// Explicit --group targets only that group; sibling groups untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_with_explicit_group_targets_that_group() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        let id2 = test_id("example.com", "cmake", "3.29");

        // Same binding name in default and [group.ci].
        add_binding(&toml(dir.path()), &id, None).await.unwrap();
        add_binding(&toml(dir.path()), &id2, Some("ci")).await.unwrap();

        // Remove from ci only.
        remove_binding(&toml(dir.path()), &id, Some("ci"))
            .await
            .expect("targeted remove from ci must succeed");

        let cfg = reload_config(dir.path());
        assert!(cfg.tools.contains_key("cmake"), "cmake must remain in [tools]");
        let ci_gone = cfg.groups.get("ci").map(|g| !g.contains_key("cmake")).unwrap_or(true);
        assert!(ci_gone, "cmake must be gone from [group.ci]");
    }

    /// Without --group, ambiguous binding (in multiple groups) returns BindingAmbiguous.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_without_group_errors_when_ambiguous() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        let id2 = test_id("example.com", "cmake", "3.29");

        add_binding(&toml(dir.path()), &id, None).await.unwrap();
        add_binding(&toml(dir.path()), &id2, Some("ci")).await.unwrap();

        let err = remove_binding(&toml(dir.path()), &id, None)
            .await
            .expect_err("remove_binding without --group on ambiguous entry must fail");
        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::BindingAmbiguous { name, groups } if name == "cmake" && groups.contains(&"default".to_owned()) && groups.contains(&"ci".to_owned()))),
            "expected BindingAmbiguous with groups=[ci, default]; got: {err}"
        );
    }

    /// Without --group, unique binding (only in one group) removes successfully.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_without_group_succeeds_when_unique() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        add_binding(&toml(dir.path()), &id, Some("ci")).await.unwrap();

        remove_binding(&toml(dir.path()), &id, None)
            .await
            .expect("unique binding without --group must succeed");

        let cfg = reload_config(dir.path());
        let gone = cfg.groups.get("ci").map(|g| !g.contains_key("cmake")).unwrap_or(true);
        assert!(gone, "cmake must be removed from [group.ci]");
    }

    // ── init_project ─────────────────────────────────────────────────────────

    /// Spec: mutate §5 bullet 7 — creates minimal ocx.toml with [tools].
    #[test]
    fn init_project_writes_minimal_toml() {
        let dir = tempdir().unwrap();
        let toml_path = toml(dir.path());

        let path = init_project(&toml_path).unwrap();

        assert_eq!(path, toml_path, "init_project must return the config-file path");
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("[tools]"), "ocx.toml must contain [tools] after init");
    }

    /// Cluster D.4 — `init_project` writes a `#:schema URL` directive on the
    /// first line so taplo / VS Code / Zed pick up the canonical project
    /// schema with no extra configuration. Anchored to the published URL so
    /// any drift between the schema URL emitted by `ocx_schema::schema_for`
    /// and the URL embedded in the init template surfaces here.
    #[test]
    fn init_project_emits_schema_directive_on_first_line() {
        let dir = tempdir().unwrap();
        let toml_path = toml(dir.path());

        init_project(&toml_path).unwrap();

        let content = fs::read_to_string(&toml_path).unwrap();
        let first_line = content.lines().next().expect("init must write at least one line");
        assert_eq!(
            first_line, "#:schema https://ocx.sh/schemas/project/v1.json",
            "first line must be the canonical taplo `#:schema` directive"
        );
    }

    /// Spec: mutate §5 bullet 8 — idempotent failure on existing file.
    #[test]
    fn init_project_errors_on_existing_file() {
        let dir = tempdir().unwrap();
        let toml_path = toml(dir.path());
        fs::write(&toml_path, "[tools]\n").unwrap();

        let err = init_project(&toml_path).expect_err("init_project must fail when ocx.toml already exists");

        let is_already_exists = matches!(
            &err,
            Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::ConfigAlreadyExists { .. })
        );
        assert!(is_already_exists, "expected ConfigAlreadyExists; got: {err}");
    }

    /// `init_project` honours a custom config-file name end-to-end: passing
    /// `<dir>/custom.toml` writes that file and does NOT create `ocx.toml`.
    /// Pins the Cluster B Codex H3 contract at the lib boundary.
    #[test]
    fn init_project_writes_custom_config_path() {
        let dir = tempdir().unwrap();
        let custom = dir.path().join("custom-name.toml");

        let path = init_project(&custom).unwrap();

        assert_eq!(path, custom, "init_project must return the supplied path verbatim");
        assert!(custom.is_file(), "custom config file must exist");
        assert!(
            !dir.path().join("ocx.toml").exists(),
            "init_project must NOT create a sibling ocx.toml when given a custom path"
        );
    }

    /// `init_project_at_default` is the bare-directory wrapper that
    /// preserves the previous "give me a directory, get ocx.toml" ergonomics
    /// for callers without a resolved file path.
    #[test]
    fn init_project_at_default_writes_ocx_toml_in_directory() {
        let dir = tempdir().unwrap();

        let path = init_project_at_default(dir.path()).unwrap();

        assert_eq!(path, dir.path().join("ocx.toml"));
        assert!(path.is_file(), "default init must create <dir>/ocx.toml");
    }

    // ── advisory lock integration ─────────────────────────────────────────────

    /// `add_binding` returns `Locked` when another task holds the exclusive
    /// lock on `ocx.toml` itself (in-place lock design — no sidecar).
    ///
    /// The lock target IS the data file: `MutationGuard::commit` rewrites
    /// `ocx.toml` through the lock-owning handle via
    /// `LockedFile::replace_bytes` (no rename, no orphan inode). A second
    /// `try_exclusive` on the same path returns `None` while the first
    /// handle's lock is held, surfacing as `ProjectErrorKind::Locked`.
    ///
    /// This works because `fs4` uses `flock(2)` on Unix (per-fd semantics), so
    /// a second open fd on the same path cannot acquire exclusive even within
    /// the same process.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_returns_locked_when_ocx_toml_already_locked() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");

        // Acquire an exclusive flock directly on `ocx.toml` from this task.
        let toml_path = dir.path().join("ocx.toml");
        let _guard = LockedFile::try_exclusive(&toml_path)
            .await
            .unwrap()
            .expect("first exclusive lock on ocx.toml must succeed");

        // add_binding internally calls acquire_project_lock_for_file, which
        // attempts LockedFile::try_exclusive on ocx.toml. Because the first
        // guard holds that lock, try_exclusive returns None → Locked.
        let id = test_id("example.com", "cmake", "3.28");
        let err = add_binding(&toml(dir.path()), &id, None)
            .await
            .expect_err("add_binding must fail with Locked while ocx.toml is exclusively held");

        assert!(
            matches!(&err, Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::Locked)),
            "expected ProjectErrorKind::Locked; got: {err}"
        );
    }

    // ── reserved group name rejection ────────────────────────────────────

    /// Plan §Phase 3.1: `add_binding_rejects_reserved_group_all`
    ///
    /// `add_binding(dir, id, Some("all"))` must return
    /// `Err(InvalidGroupName { name: "all" })`.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_rejects_reserved_group_all() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        let err = add_binding(&toml(dir.path()), &id, Some("all"))
            .await
            .expect_err("add_binding with group 'all' must fail");

        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::InvalidGroupName { name } if name == "all")),
            "expected InvalidGroupName {{ name: \"all\" }}; got: {err}"
        );
    }

    /// Plan §Phase 3.1: `add_binding_rejects_reserved_group_default`
    ///
    /// `add_binding(dir, id, Some("default"))` must return
    /// `Err(InvalidGroupName { name: "default" })`.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_rejects_reserved_group_default() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        let err = add_binding(&toml(dir.path()), &id, Some("default"))
            .await
            .expect_err("add_binding with group 'default' must fail");

        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::InvalidGroupName { name } if name == "default")),
            "expected InvalidGroupName {{ name: \"default\" }}; got: {err}"
        );
    }

    /// Plan §Phase 3.1: `add_binding_accepts_normal_group_name_unchanged`
    ///
    /// Regression: a normal group name like `"ci"` must still succeed.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_accepts_normal_group_name_unchanged() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        add_binding(&toml(dir.path()), &id, Some("ci"))
            .await
            .expect("add_binding with group 'ci' must succeed");

        let cfg = reload_config(dir.path());
        assert!(
            cfg.groups.get("ci").map(|g| g.contains_key("cmake")).unwrap_or(false),
            "cmake must appear under [group.ci] after add with normal group name"
        );
    }
}
