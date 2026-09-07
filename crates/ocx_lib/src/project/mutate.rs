// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Mutating writes to the project config file. Atomic full-file rewrite
//! under an exclusive advisory flock on the config file itself.
//!
//! Public mutation helpers ([`add_binding`], [`remove_binding`],
//! [`init_project`], [`set_activate`]) take the **resolved config file path** — typically
//! `<project_root>/ocx.toml` but may be `<project_root>/<custom>.toml`
//! when the caller passed `--project=<custom>.toml`. The flock target is
//! always the config file itself, never a hard-coded `ocx.toml`.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::oci::Identifier;
use crate::project::Error;
use crate::project::error::{ProjectError, ProjectErrorKind};
use crate::project::project_lock::acquire_project_lock_for_file;

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

/// Validate the `[tools]` key `ocx add` is about to write — the explicit name
/// from the `NAME=IDENTIFIER` form, or the key [`binding_key`] derived from
/// the identifier.
///
/// Both go through [`super::config::validate_toolchain_name`], the one grammar
/// the **reader** applies to every `[tools]` key, `[group.<g>].tools` key and
/// `[group.<g>]` name. The derived key is not the safer input of the two — it
/// comes from an identifier nobody spelled a name for, so an over-long or
/// off-charset repository basename reaches the file unexamined, and before
/// this guard existed nothing looked at it at all.
///
/// **What this buys, stated exactly.** It does not stop an unloadable file:
/// `super::document::render_preserving` re-parses the text it renders through
/// the same reader, so a refused name was already refused — late, with
/// [`ProjectErrorKind::ManifestEditDiverged`], which names neither the
/// offending key nor the rule it broke, and only after the lock was taken and
/// the lock file resolved. What this buys is the reader's own diagnosis, at
/// the first door, before any of that. The `my.tools` direction is the one
/// where behaviour genuinely changes: the old hand-rolled group charset
/// refused names the reader accepts.
///
/// `scope` matches the reader's own vocabulary so one string appears in the
/// diagnostic whichever door the name came through: `"tools"` for the default
/// group, `"group.<g>.tools"` for a named one.
///
/// # Errors
///
/// [`ProjectErrorKind::InvalidToolchainNameCharset`] — the reader's own
/// variant, so the message a user gets from `ocx add` is the message they
/// would get from the file.
fn validate_tool_key(key: &str, group: Option<&str>, path: &Path) -> Result<(), Error> {
    let scope = match group {
        None => "tools".to_owned(),
        Some(group_name) => format!("group.{group_name}.tools"),
    };
    super::config::validate_toolchain_name(&scope, key, path)
}

/// Validate a group name supplied via `--group`.
///
/// Two rules, in the reader's own order:
///
/// 1. The reserved selectors `default` and `all`, compared **ASCII
///    case-folded** — `[group.Default]` silently coexisting with the `default`
///    group is exactly the collision the reservation exists to stop, and the
///    reader folds case for the same comparison.
/// 2. Everything else is [`super::config::validate_toolchain_name`]: the same
///    charset and the same 64-byte cap the reader applies to a `[group.<g>]`
///    name.
///
/// Rule 2 replaces a hand-rolled `is_alphanumeric()` test that was wrong in
/// both directions: `is_alphanumeric` is **Unicode**, so it admitted `café`,
/// which the reader refuses; and it excluded `.`, so it refused `my.tools`,
/// which the reader accepts.
///
/// # The charset refusal is a **usage** fault here, not a config one (RUL-73)
///
/// Rule 2 applies the *reader's* charset to a value nobody put in a file, and
/// the reader's [`ProjectErrorKind::InvalidToolchainNameCharset`] classifies as
/// `ConfigError` (78) — the right answer for what an `ocx.toml` contains and
/// the wrong one for what a user typed. `ocx add --group '../../etc'` exited 64
/// through [`ProjectErrorKind::InvalidGroupName`] until C-014's shared
/// validator silently reclassified it, so the charset arm is re-attributed
/// back here, at the one door a command-line group name comes through.
/// `project/config.rs`'s parse-time validator is untouched and still answers 78
/// for the identical name read out of a file.
///
/// # Errors
///
/// [`ProjectErrorKind::InvalidGroupName`] for a reserved selector or a name
/// outside the toolchain charset — both usage faults in `--group`, exit 64.
/// The `_ => error` arm below is defensive rather than reachable: since C-073
/// deleted the `bin` reservation, the charset refusal is the only variant the
/// reader's validator still produces.
fn validate_group_name(name: &str, path: &Path) -> Result<(), Error> {
    if name.eq_ignore_ascii_case(super::internal::DEFAULT_GROUP)
        || name.eq_ignore_ascii_case(super::internal::ALL_GROUP)
    {
        return Err(invalid_group_name(name, path));
    }
    super::config::validate_toolchain_name("group", name, path).map_err(|error| match &error {
        // The reader's charset diagnosis is not carried through:
        // `InvalidGroupName` holds only a name. That is the trade RUL-73 makes
        // — a precise sentence at the wrong exit code is worse than a general
        // one at the right code, because only the code is machine-readable.
        Error::Project(project_error)
            if matches!(project_error.kind, ProjectErrorKind::InvalidToolchainNameCharset { .. }) =>
        {
            invalid_group_name(name, path)
        }
        _ => error,
    })
}

/// [`ProjectErrorKind::InvalidGroupName`] for `name`, attached to `path`.
fn invalid_group_name(name: &str, path: &Path) -> Error {
    Error::Project(ProjectError::new(
        path.to_path_buf(),
        ProjectErrorKind::InvalidGroupName { name: name.to_owned() },
    ))
}

// ── public API ────────────────────────────────────────────────────────────

/// Read and parse the project config file THROUGH the lock-owning handle.
///
/// Reading via a *second* handle (e.g. `std::fs::read_to_string`) while the
/// exclusive advisory lock is held works on POSIX (flock is advisory) but hits
/// `ERROR_LOCK_VIOLATION` on Windows, where `LockFileEx` blocks reads of the
/// locked range by other handles. Reading through `guard` (the lock owner)
/// avoids the second handle. Enforces the same 64 KiB size cap as the former
/// `load_config`.
///
/// Returns the verbatim text alongside the parsed config: the write-back path
/// edits that document rather than re-serializing the parsed form, so the
/// user's comments and declaration order survive the mutation.
async fn read_config_via_guard(
    guard: &mut crate::utility::fs::LockedFile,
    config_path: &Path,
) -> Result<(String, crate::project::config::ProjectConfig), Error> {
    let bytes = guard.read_bytes().await.map_err(|e| {
        Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Io(std::io::Error::other(e)),
        ))
    })?;
    let limit = crate::project::internal::FILE_SIZE_LIMIT_BYTES;
    if bytes.len() as u64 > limit {
        return Err(Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::FileTooLarge {
                size: bytes.len() as u64,
                limit,
            },
        )));
    }
    let text = String::from_utf8(bytes).map_err(|e| {
        Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        ))
    })?;
    let config = crate::project::config::ProjectConfig::from_toml_str(&text)?;
    Ok((text, config))
}

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
/// `name` is the explicit binding key from the `NAME=IDENTIFIER` form of
/// `ocx add`; when `None` the key is derived with [`binding_key`] (the
/// repository basename). An explicit name lets two packages that share a
/// basename — `gitlab/cli` and `github/cli` — coexist in one group.
///
/// # Errors
///
/// Same set as [`add_binding`] minus the I/O / parse variants
/// (`Io`, `FileTooLarge`, `TomlParse`, `TomlSerialize`, `Locked`):
///
/// - [`ProjectErrorKind::InvalidGroupName`] — `group` is a reserved selector
///   (`default` or `all`, ASCII-case-folded).
/// - [`ProjectErrorKind::InvalidToolchainNameCharset`] — `group`, or the key
///   this mutation would write, is not a name the reader accepts. The key is
///   checked whether it was typed or derived (R-W21).
/// - [`ProjectErrorKind::BindingAlreadyExists`] — the key already exists in
///   the target group.
pub fn add_binding_in_memory(
    config: &mut crate::project::config::ProjectConfig,
    path: &Path,
    identifier: &Identifier,
    name: Option<&str>,
    group: Option<&str>,
) -> Result<String, Error> {
    if let Some(group_name) = group {
        validate_group_name(group_name, path)?;
    }

    // One guard for both doors: the explicit `NAME=IDENTIFIER` key and the one
    // `binding_key` derives. Validating only the typed name is what let a
    // derived key carry a name the reader refuses all the way to the write and
    // fail there with a diagnosis naming neither the key nor the rule.
    let key = name.map_or_else(|| binding_key(identifier), str::to_owned);
    validate_tool_key(&key, group, path)?;

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
            if config
                .groups
                .get(group_name)
                .is_some_and(|g| g.tools.contains_key(&key))
            {
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
                .tools
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
/// otherwise lands under `[group.<group>]`. `name` is the explicit binding
/// key (`None` derives it from the identifier — see
/// [`add_binding_in_memory`]).
///
/// Acquires an exclusive advisory flock on the config file itself for
/// the duration of the read-modify-write cycle.
///
/// # Errors
///
/// - [`ProjectErrorKind::InvalidGroupName`] — `group` is a reserved selector
///   (`default` or `all`, ASCII-case-folded).
/// - [`ProjectErrorKind::InvalidToolchainNameCharset`] — `group`, or the key
///   this mutation would write, is not a name the reader accepts.
/// - [`ProjectErrorKind::Io`] — the config file could not be read or written.
/// - [`ProjectErrorKind::FileTooLarge`] — the config file exceeds the 64 KiB cap.
/// - [`ProjectErrorKind::TomlParse`] — the config could not be parsed from TOML.
/// - [`ProjectErrorKind::ManifestEditParse`] — the on-disk `ocx.toml` did not
///   parse as an editable document during the format-preserving write-back.
/// - [`ProjectErrorKind::ManifestEditDiverged`] — the format-preserving edit
///   produced a document that no longer describes the staged configuration;
///   the write is abandoned rather than falling back to a whole-file rewrite.
/// - [`ProjectErrorKind::BindingAlreadyExists`] — the binding key already
///   exists in the target group. The same name may exist in other groups
///   without error.
/// - [`ProjectErrorKind::Locked`] — another process holds the exclusive flock
///   on the config file; the caller should retry with backoff.
pub async fn add_binding(
    config_path: &Path,
    identifier: &Identifier,
    name: Option<&str>,
    group: Option<&str>,
) -> Result<(), Error> {
    // Validate the group name before acquiring the lock so invalid input is
    // rejected cheaply — no filesystem operations needed.
    if let Some(group_name) = group {
        validate_group_name(group_name, config_path)?;
    }

    let mut guard = acquire_project_lock_for_file(config_path).await?;

    let (original, mut config) = read_config_via_guard(&mut guard, config_path).await?;

    // Compose: in-memory mutation + write-back through the lock-owning handle.
    add_binding_in_memory(&mut config, config_path, identifier, name, group)?;

    let serialized = super::document::render_preserving(&original, &config, config_path)?;
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
/// `name` is the binding key verbatim — the TOML `[tools]` key. Callers that
/// hold an identifier derive it with [`binding_key`]; deriving it here would
/// make a binding added under an explicit `NAME=IDENTIFIER` alias
/// unremovable by its own name.
///
/// Acquires an exclusive advisory flock on the config file itself for
/// the duration of the read-modify-write cycle.
///
/// # Errors
///
/// - [`ProjectErrorKind::Io`] — the config file could not be read or written.
/// - [`ProjectErrorKind::FileTooLarge`] — the config file exceeds the 64 KiB cap.
/// - [`ProjectErrorKind::TomlParse`] — the config could not be parsed from TOML.
/// - [`ProjectErrorKind::ManifestEditParse`] — the on-disk `ocx.toml` did not
///   parse as an editable document during the format-preserving write-back.
/// - [`ProjectErrorKind::ManifestEditDiverged`] — the format-preserving edit
///   produced a document that no longer describes the staged configuration;
///   the write is abandoned rather than falling back to a whole-file rewrite.
/// - [`ProjectErrorKind::BindingNotFound`] — `name` was not found in the
///   targeted group (or any group when `group` is `None`).
/// - [`ProjectErrorKind::BindingAmbiguous`] — `group` is `None` and the
///   binding name appears in more than one group; pass `--group` to
///   disambiguate.
/// - [`ProjectErrorKind::Locked`] — another process holds the exclusive flock
///   on the config file; the caller should retry with backoff.
pub async fn remove_binding(config_path: &Path, name: &str, group: Option<&str>) -> Result<(), Error> {
    let mut guard = acquire_project_lock_for_file(config_path).await?;

    let (original, mut config) = read_config_via_guard(&mut guard, config_path).await?;

    remove_binding_in_memory(&mut config, config_path, name, group)?;

    let serialized = super::document::render_preserving(&original, &config, config_path)?;
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
/// `name` is the binding key verbatim (see [`remove_binding`]).
///
/// # Errors
///
/// - [`ProjectErrorKind::BindingNotFound`] — `name` was not found in the
///   targeted group (or any group when `group` is `None`).
/// - [`ProjectErrorKind::BindingAmbiguous`] — `group` is `None` and the
///   binding appears in more than one group.
pub fn remove_binding_in_memory(
    config: &mut crate::project::config::ProjectConfig,
    path: &Path,
    name: &str,
    group: Option<&str>,
) -> Result<(), Error> {
    let key = name.to_owned();

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
            let group = config.groups.get_mut(group_name).filter(|g| g.tools.contains_key(&key));
            if let Some(g) = group {
                g.tools.remove(&key);
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
                if config.groups[group_name].tools.contains_key(&key) {
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
                            .tools
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
/// default content is a schema directive and an empty `[tools]` table —
/// non-interactive, backend-first per
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

    // Minimal non-interactive content: schema-server hint + empty `[tools]`
    // table. The first-line comment is recognized by the YAML/TOML language
    // servers (`taplo`, `yaml-language-server`) and IDE plugins (VS Code, Zed)
    // as a pointer to the canonical JSON Schema at the published URL — gives
    // schema-aware autocompletion and validation out of the box.
    // Intentionally kept under 10 non-blank lines per research §6.4.
    //
    // No `registry` hint: `ocx.toml` has no such key (`RawProjectConfig` is
    // `deny_unknown_fields`), so the commented form used to be a line that
    // broke the file the moment anyone uncommented it. The default registry is
    // `[registry] default` in `config.toml` (`crate::config`).
    let content = "\
#:schema https://ocx.sh/schemas/project/v1.json
# OCX project toolchain — managed by `ocx add` / `ocx remove`

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

/// Persist the toolchain `activate` mode as the top-level `activate` key of
/// the config file at `config_path`.
///
/// The writer behind `ocx self setup --toolchain-activate`, whose target is
/// always the **global** `$OCX_HOME/ocx.toml`. Omitting the flag calls nothing,
/// so a run that does not ask for the key leaves the file byte-identical —
/// the same contract the `[shell]` toggles keep for `config.toml`.
///
/// **Creates the file when absent, carrying only this key.** A machine that
/// has never run `ocx --global add` has no `$OCX_HOME/ocx.toml`, and the
/// alternative — refusing, or writing [`init_project`]'s `[tools]` template —
/// would either make the flag conditional on unrelated state or invent
/// declarations the user did not make.
///
/// Every write goes through [`acquire_project_lock_for_file`] and the shared
/// format-preserving render, exactly as [`add_binding`] does. There is no
/// second `toml_edit` site: a comment, a key order or a spacing convention
/// that survives `ocx add` must survive this too, and two editors of one file
/// format is how that stops being true.
///
/// **The create-when-absent case needs no branch.** `LockedFile::try_exclusive`
/// creates the file it locks, so an absent `$OCX_HOME/ocx.toml` is read back as
/// empty text, and the render inserts one scalar into an empty document — no
/// `[tools]` table, no template, only the key. The same acquire also runs the
/// shipped symlink refusal, so a symlink planted at the path is refused rather
/// than followed.
///
/// **This does not stale `ocx.lock`.** [`super::declaration_hash`] covers
/// `tools` and `group.*.tools` only, so writing `activate` cannot force a
/// re-lock — and the cached hash on `config` stays valid, which is why this is
/// the one mutator here that does not invalidate it.
///
/// # Errors
///
/// The [`add_binding`] set minus the binding-specific variants:
/// [`ProjectErrorKind::Io`], [`ProjectErrorKind::FileTooLarge`],
/// [`ProjectErrorKind::TomlParse`], [`ProjectErrorKind::ManifestEditParse`],
/// [`ProjectErrorKind::ManifestEditDiverged`], [`ProjectErrorKind::Locked`].
pub async fn set_activate(config_path: &Path, mode: crate::activate::ActivateMode) -> Result<(), Error> {
    let mut guard = acquire_project_lock_for_file(config_path).await?;

    let (original, mut config) = read_config_via_guard(&mut guard, config_path).await?;
    if config.activate == Some(mode) {
        // Already what was asked for: leave the bytes alone rather than
        // re-render them, so a re-run cannot disturb decor or mtime.
        return Ok(());
    }
    config.activate = Some(mode);

    let serialized = super::document::render_preserving(&original, &config, config_path)?;
    // Rewrite in place through the lock-owning handle (see `add_binding`).
    guard.replace_bytes(serialized.as_bytes()).await.map_err(|e| {
        Error::Project(ProjectError::new(
            config_path.to_path_buf(),
            ProjectErrorKind::Io(std::io::Error::other(e)),
        ))
    })?;

    Ok(())
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

        add_binding(&toml(dir.path()), &id, None, None).await.unwrap();

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

        add_binding(&toml(dir.path()), &id, None, Some("ci")).await.unwrap();

        let cfg = reload_config(dir.path());
        assert!(
            cfg.groups
                .get("ci")
                .map(|g| g.tools.contains_key("cmake"))
                .unwrap_or(false),
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
        add_binding(&toml(dir.path()), &id, None, None).await.unwrap();
        let err = add_binding(&toml(dir.path()), &id, None, None)
            .await
            .expect_err("second add to default must fail");
        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::BindingAlreadyExists { group, .. } if group == "default")),
            "expected BindingAlreadyExists with group=default; got: {err}"
        );

        // Dup in [group.ci] → error only when re-added to [group.ci],
        // NOT when added to [group.staging].
        let id2 = test_id("example.com", "ninja", "1.11");
        add_binding(&toml(dir.path()), &id2, None, Some("ci")).await.unwrap();
        let err2 = add_binding(&toml(dir.path()), &id2, None, Some("ci"))
            .await
            .expect_err("second add to ci must fail");
        assert!(
            matches!(&err2, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::BindingAlreadyExists { group, .. } if group == "ci")),
            "expected BindingAlreadyExists with group=ci; got: {err2}"
        );
        // Same name in [group.staging] must succeed.
        add_binding(&toml(dir.path()), &id2, None, Some("staging"))
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

        add_binding(&toml(dir.path()), &id, None, None)
            .await
            .expect("add cmake to default must succeed");
        add_binding(&toml(dir.path()), &id2, None, Some("ci"))
            .await
            .expect("add cmake to [group.ci] must succeed");

        let cfg = reload_config(dir.path());
        assert!(cfg.tools.contains_key("cmake"), "cmake in [tools]");
        assert!(
            cfg.groups
                .get("ci")
                .map(|g| g.tools.contains_key("cmake"))
                .unwrap_or(false),
            "cmake in [group.ci]"
        );
    }

    // ── explicit binding name (NAME=IDENTIFIER) ──────────────────────────────

    /// An explicit name becomes the binding key verbatim, with the full
    /// identifier as the value.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_with_explicit_name_uses_that_key() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "gitlab/cli", "1.0");

        add_binding(&toml(dir.path()), &id, Some("glab"), None)
            .await
            .expect("explicit name must be accepted");

        let cfg = reload_config(dir.path());
        assert_eq!(
            cfg.tools.get("glab").map(ToString::to_string),
            Some(id.to_string()),
            "explicit name must key the full identifier; got: {:?}",
            cfg.tools
        );
        assert!(
            !cfg.tools.contains_key("cli"),
            "the basename key must NOT be written when a name is explicit"
        );
    }

    /// Issue #279: two packages sharing a repository basename coexist when
    /// the second is bound under an explicit name.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_explicit_name_resolves_basename_collision() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let gitlab = test_id("example.com", "gitlab/cli", "1.0");
        let github = test_id("example.com", "github/cli", "2.0");

        add_binding(&toml(dir.path()), &gitlab, None, None)
            .await
            .expect("first add derives the basename key");
        add_binding(&toml(dir.path()), &github, Some("cli2"), None)
            .await
            .expect("colliding basename must succeed under an explicit name");

        let cfg = reload_config(dir.path());
        assert!(cfg.tools.contains_key("cli"), "derived key must remain");
        assert!(cfg.tools.contains_key("cli2"), "explicit key must be present");
    }

    /// A duplicate explicit name in the same group is still rejected.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_errors_on_duplicate_explicit_name() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let first = test_id("example.com", "gitlab/cli", "1.0");
        let second = test_id("example.com", "github/cli", "2.0");

        add_binding(&toml(dir.path()), &first, Some("glab"), None)
            .await
            .unwrap();
        let err = add_binding(&toml(dir.path()), &second, Some("glab"), None)
            .await
            .expect_err("second add under the same explicit name must fail");

        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::BindingAlreadyExists { name, group } if name == "glab" && group == "default")),
            "expected BindingAlreadyExists for the explicit name; got: {err}"
        );
    }

    /// An explicit name the reader's charset refuses is rejected before
    /// anything is written — and with the reader's own error, so `ocx add`
    /// and a hand-written `ocx.toml` give one answer for one string (R-W21).
    ///
    /// The candidates cover both halves of the merged charset variant: three
    /// off-charset spellings, one leading-punctuation spelling the old
    /// `[A-Za-z0-9._-]` test admitted, and one over the 64-byte cap.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_rejects_invalid_explicit_name() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "gitlab/cli", "1.0");
        let too_long = "a".repeat(65);

        for candidate in [
            "gitlab/cli",
            "",
            "with space",
            "-leading",
            ".leading",
            "_leading",
            too_long.as_str(),
        ] {
            let err = add_binding(&toml(dir.path()), &id, Some(candidate), None)
                .await
                .expect_err("invalid explicit name must be rejected");
            assert!(
                matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::InvalidToolchainNameCharset { scope, name } if scope == "tools" && name == candidate)),
                "expected InvalidToolchainNameCharset {{ scope: \"tools\" }} for {candidate:?}; got: {err}"
            );
        }

        let cfg = reload_config(dir.path());
        assert!(cfg.tools.is_empty(), "a rejected name must leave [tools] untouched");
    }

    /// R-W21: the **derived** key is validated too. `ocx add` names no key at
    /// all, so validating only the typed name leaves the one input nobody
    /// typed unguarded.
    ///
    /// The probe was `acme/bin` until C-073, whose derived key was the one
    /// word the reader reserved. That reservation is gone, so the probe is now
    /// an over-long basename — still derived, still refused, and still by the
    /// reader's own validator rather than a second one here.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_rejects_a_derived_key_the_reader_refuses() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let over_long = "a".repeat(crate::package::metadata::slug::SLUG_MAX_LEN + 1);
        let id = test_id("example.com", &format!("acme/{over_long}"), "1.0");

        let err = add_binding(&toml(dir.path()), &id, None, None)
            .await
            .expect_err("a derived key past the length cap must be rejected");
        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::InvalidToolchainNameCharset { scope, name } if scope == "tools" && name == &over_long)),
            "expected InvalidToolchainNameCharset for the derived key; got: {err}"
        );

        let cfg = reload_config(dir.path());
        assert!(cfg.tools.is_empty(), "a rejected key must leave [tools] untouched");
    }

    /// C-073 / S-003 from the writer side: `bin` is an ordinary tool name and
    /// an ordinary group name, in every ASCII case — including the key
    /// `ocx add example.com/acme/bin` derives without anyone typing it.
    ///
    /// This is the user-visible half of the reservation's deletion: the same
    /// command that exited 78 now writes `[tools] bin = …`, and the tree
    /// renders it at `links/<group>/bin`.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_accepts_bin_as_a_tool_and_a_group_name_case_folded() {
        let id = test_id("example.com", "acme/tool", "1.0");

        for candidate in ["bin", "Bin", "BIN"] {
            let dir = tempdir().unwrap();
            write_minimal_toml(dir.path(), "[tools]\n");
            add_binding(&toml(dir.path()), &id, Some(candidate), None)
                .await
                .unwrap_or_else(|e| panic!("C-073 — tool {candidate:?} must be accepted; got {e}"));
            assert!(
                reload_config(dir.path()).tools.contains_key(candidate),
                "the accepted tool must actually be written to [tools]"
            );

            let dir = tempdir().unwrap();
            write_minimal_toml(dir.path(), "[tools]\n");
            add_binding(&toml(dir.path()), &id, None, Some(candidate))
                .await
                .unwrap_or_else(|e| panic!("C-073 — group {candidate:?} must be accepted; got {e}"));
            assert!(
                reload_config(dir.path()).groups.contains_key(candidate),
                "the accepted group must actually be written to [group.<g>]"
            );
        }

        // The derived key, which nobody types: `acme/bin` -> `bin`.
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let derived = test_id("example.com", "acme/bin", "1.0");
        add_binding(&toml(dir.path()), &derived, None, None)
            .await
            .expect("C-073 — a derived `bin` key must be accepted");
        assert!(
            reload_config(dir.path()).tools.contains_key("bin"),
            "S-003 — `ocx add example.com/acme/bin` writes `[tools] bin`"
        );
    }

    /// R-W21: the group charset was `char::is_alphanumeric`, which is
    /// **Unicode**, and excluded `.`. It was therefore wrong in both
    /// directions at once — admitting a name the reader refuses and refusing
    /// one the reader accepts. Both directions are asserted here, because
    /// fixing only the first would still leave `ocx add -g my.tools` unusable.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_group_charset_matches_the_reader_in_both_directions() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        let err = add_binding(&toml(dir.path()), &id, None, Some("café"))
            .await
            .expect_err("a non-ASCII group name must be rejected");
        // RUL-73 — refused by the reader's charset, reported as the *usage*
        // fault a `--group` value is (`InvalidGroupName`, exit 64). Which names
        // the charset admits is what R-W21 is about, and that is unchanged:
        // `café` is still refused and `my.tools` still accepted below.
        assert!(
            matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::InvalidGroupName { name } if name == "café")),
            "expected InvalidGroupName for a Unicode group name; got: {err}"
        );

        add_binding(&toml(dir.path()), &id, None, Some("my.tools"))
            .await
            .expect("a dotted group name is legal to the reader and must be legal to the writer");
        let cfg = reload_config(dir.path());
        assert!(
            cfg.groups
                .get("my.tools")
                .is_some_and(|g| g.tools.contains_key("cmake")),
            "the dotted group must round-trip through the reader"
        );
    }

    /// D-V17 from the writer side: the reserved-selector comparison folds
    /// ASCII case, so `[group.Default]` cannot be written to coexist with the
    /// `default` group it would collide with.
    #[tokio::test(flavor = "multi_thread")]
    async fn add_binding_rejects_a_reserved_selector_whatever_its_case() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");

        for candidate in ["Default", "DEFAULT", "All", "ALL"] {
            let err = add_binding(&toml(dir.path()), &id, None, Some(candidate))
                .await
                .expect_err("a case variant of a reserved selector must be rejected");
            assert!(
                matches!(&err, Error::Project(pe) if matches!(&pe.kind, ProjectErrorKind::InvalidGroupName { name } if name == candidate)),
                "expected InvalidGroupName for {candidate:?}; got: {err}"
            );
        }
    }

    // ── set_activate (C-042) ─────────────────────────────────────────────

    /// C-042: an absent `$OCX_HOME/ocx.toml` is created carrying **only** the
    /// key — no `[tools]` template, no invented declarations.
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_creates_the_file_with_only_that_key() {
        use crate::activate::ActivateMode;

        let dir = tempdir().unwrap();
        let target = toml(dir.path());
        assert!(!target.exists(), "the fixture starts with no ocx.toml");

        set_activate(&target, ActivateMode::Bin).await.expect("write succeeds");

        assert_eq!(fs::read_to_string(&target).unwrap(), "activate = \"bin\"\n");
        assert_eq!(reload_config(dir.path()).activate, Some(ActivateMode::Bin));
    }

    /// C-042: an existing file keeps everything the typed model does not own —
    /// the schema directive, the bindings, the comments — and gains one key.
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_leaves_the_rest_of_the_file_alone() {
        use crate::activate::ActivateMode;

        let dir = tempdir().unwrap();
        let original = "#:schema https://ocx.sh/schemas/project/v1.json\n\
                        # my toolchain\n\
                        \n\
                        [tools]\n\
                        cmake    =    \"example.com/cmake:3.28\"  # pinned\n";
        write_minimal_toml(dir.path(), original);

        set_activate(&toml(dir.path()), ActivateMode::None)
            .await
            .expect("write succeeds");

        let rendered = fs::read_to_string(toml(dir.path())).unwrap();
        assert!(rendered.contains("#:schema"), "{rendered}");
        assert!(rendered.contains("# my toolchain"), "{rendered}");
        assert!(
            rendered.contains("cmake    =    \"example.com/cmake:3.28\"  # pinned"),
            "the untouched binding keeps its spacing and comment: {rendered}"
        );
        assert_eq!(reload_config(dir.path()).activate, Some(ActivateMode::None));
    }

    /// Re-running `ocx self setup --toolchain-activate <same>` must leave the
    /// file **byte-identical**, not merely equivalent — the same contract the
    /// `[shell]` toggles keep for `config.toml`.
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_is_byte_identical_on_a_re_run() {
        use crate::activate::ActivateMode;

        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");

        set_activate(&toml(dir.path()), ActivateMode::Env).await.unwrap();
        let first = fs::read_to_string(toml(dir.path())).unwrap();
        set_activate(&toml(dir.path()), ActivateMode::Env).await.unwrap();
        assert_eq!(fs::read_to_string(toml(dir.path())).unwrap(), first);
    }

    /// Writing `activate` must not stale `ocx.lock`: the declaration hash
    /// covers `tools` and `group.*.tools` only, so this key cannot force a
    /// re-lock. Asserted rather than assumed, because the cheapest wrong
    /// implementation — folding the key into the hash — would make every
    /// `ocx self setup --toolchain-activate` demand an `ocx lock`.
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_does_not_stale_the_lock() {
        use crate::activate::ActivateMode;
        use crate::project::declaration_hash;

        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\ncmake = \"example.com/cmake:3.28\"\n");
        let before = declaration_hash(&reload_config(dir.path()));

        set_activate(&toml(dir.path()), ActivateMode::Bin).await.unwrap();

        assert_eq!(declaration_hash(&reload_config(dir.path())), before);
    }

    /// R-W21's whole point, as one property: **the writer accepts a name if
    /// and only if the reader does.** Each candidate is put through
    /// `add_binding` and, independently, through a hand-written `ocx.toml`
    /// carrying the same key — and the two verdicts must agree.
    ///
    /// **The direction this test uniquely guards is "the writer refuses a
    /// name the reader accepts"** — the half R-W21's `my.tools` case names,
    /// and the half no other test here covers as a property rather than as a
    /// listed example. Measured, not assumed: deleting the key guard leaves
    /// this test **green**, because `render_preserving` re-parses what it
    /// renders and refuses the write a second time. Two independent guards
    /// defend the other direction, so neither one alone reds it.
    ///
    /// That second guard is also why the shipped defect was not a corrupt
    /// file: pre-fix, `ocx add ghcr.io/acme/bin` failed late with
    /// `ManifestEditDiverged` — a diagnosis naming nothing the user could act
    /// on — rather than writing an unloadable `ocx.toml`. Validating up front
    /// is what turns that into a named refusal before the lock is taken.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_writer_accepts_exactly_the_names_the_reader_accepts() {
        let too_long = "a".repeat(65);
        let candidates = [
            "cmake",
            "python3.13",
            "MSBuild",
            "my-tool",
            "my.tool",
            "my_tool",
            "bin",
            "Bin",
            "-leading",
            ".leading",
            "_leading",
            "with space",
            "gitlab/cli",
            "café",
            "",
            too_long.as_str(),
        ];

        for candidate in candidates {
            let dir = tempdir().unwrap();
            write_minimal_toml(dir.path(), "[tools]\n");
            let id = test_id("example.com", "acme/tool", "1.0");

            let written = add_binding(&toml(dir.path()), &id, Some(candidate), None).await.is_ok();
            let read =
                ProjectConfig::from_toml_str(&format!("[tools]\n\"{candidate}\" = \"example.com/acme/tool:1.0\"\n"))
                    .is_ok();

            assert_eq!(
                written, read,
                "writer and reader disagree about {candidate:?}: writer accepted = {written}, reader accepted = {read}"
            );
        }
    }

    /// A binding added under an explicit name is removable by that name, and
    /// the sibling that owns the derived basename key survives.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_targets_the_explicit_name_not_the_basename() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let gitlab = test_id("example.com", "gitlab/cli", "1.0");
        let github = test_id("example.com", "github/cli", "2.0");

        add_binding(&toml(dir.path()), &gitlab, None, None).await.unwrap();
        add_binding(&toml(dir.path()), &github, Some("glab"), None)
            .await
            .unwrap();

        remove_binding(&toml(dir.path()), "glab", None)
            .await
            .expect("remove by explicit name must succeed");

        let cfg = reload_config(dir.path());
        assert!(!cfg.tools.contains_key("glab"), "the named binding must be gone");
        assert!(
            cfg.tools.contains_key("cli"),
            "the basename-keyed sibling must survive; got: {:?}",
            cfg.tools
        );
    }

    // ── remove_binding ───────────────────────────────────────────────────────

    /// Spec: mutate §5 bullet 4 — remove from default group (no --group).
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_drops_default_group_entry() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        add_binding(&toml(dir.path()), &id, None, None).await.unwrap();

        remove_binding(&toml(dir.path()), "cmake", None).await.unwrap();

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
        add_binding(&toml(dir.path()), &id, None, Some("ci")).await.unwrap();

        remove_binding(&toml(dir.path()), "cmake", None).await.unwrap();

        let cfg = reload_config(dir.path());
        let gone = cfg
            .groups
            .get("ci")
            .map(|g| !g.tools.contains_key("cmake"))
            .unwrap_or(true);
        assert!(gone, "cmake must be gone from [group.ci] after remove_binding");
    }

    /// Spec: mutate §5 bullet 6 — missing binding returns BindingNotFound.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_errors_when_missing() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");

        let err = remove_binding(&toml(dir.path()), "cmake", None)
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
        add_binding(&toml(dir.path()), &id, None, None).await.unwrap();
        add_binding(&toml(dir.path()), &id2, None, Some("ci")).await.unwrap();

        // Remove from ci only.
        remove_binding(&toml(dir.path()), "cmake", Some("ci"))
            .await
            .expect("targeted remove from ci must succeed");

        let cfg = reload_config(dir.path());
        assert!(cfg.tools.contains_key("cmake"), "cmake must remain in [tools]");
        let ci_gone = cfg
            .groups
            .get("ci")
            .map(|g| !g.tools.contains_key("cmake"))
            .unwrap_or(true);
        assert!(ci_gone, "cmake must be gone from [group.ci]");
    }

    /// Without --group, ambiguous binding (in multiple groups) returns BindingAmbiguous.
    #[tokio::test(flavor = "multi_thread")]
    async fn remove_binding_without_group_errors_when_ambiguous() {
        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let id = test_id("example.com", "cmake", "3.28");
        let id2 = test_id("example.com", "cmake", "3.29");

        add_binding(&toml(dir.path()), &id, None, None).await.unwrap();
        add_binding(&toml(dir.path()), &id2, None, Some("ci")).await.unwrap();

        let err = remove_binding(&toml(dir.path()), "cmake", None)
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
        add_binding(&toml(dir.path()), &id, None, Some("ci")).await.unwrap();

        remove_binding(&toml(dir.path()), "cmake", None)
            .await
            .expect("unique binding without --group must succeed");

        let cfg = reload_config(dir.path());
        let gone = cfg
            .groups
            .get("ci")
            .map(|g| !g.tools.contains_key("cmake"))
            .unwrap_or(true);
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
        let err = add_binding(&toml(dir.path()), &id, None, None)
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

        let err = add_binding(&toml(dir.path()), &id, None, Some("all"))
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

        let err = add_binding(&toml(dir.path()), &id, None, Some("default"))
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

        add_binding(&toml(dir.path()), &id, None, Some("ci"))
            .await
            .expect("add_binding with group 'ci' must succeed");

        let cfg = reload_config(dir.path());
        assert!(
            cfg.groups
                .get("ci")
                .map(|g| g.tools.contains_key("cmake"))
                .unwrap_or(false),
            "cmake must appear under [group.ci] after add with normal group name"
        );
    }

    /// R-W21 / E-R10 / D-V14: **no second name grammar lives in this module.**
    ///
    /// The finding is not "the writer refuses the wrong names" — it is that a
    /// second copy of the grammar existed at all, and that it had drifted from
    /// the reader's in both directions. A fix that leaves a hand-rolled charset
    /// loop beside the shared validator re-creates the drift the moment either
    /// side changes, so the deleted validators must stay deleted.
    ///
    /// `SLUG_MAX_LEN` is included because RUL-21 makes a hand-rolled length cap
    /// the other half of the same copy.
    ///
    /// Comment lines and the test module are cut first: the doc comments in
    /// this file quote `is_alphanumeric` when they explain what was removed,
    /// and this test's own needle list is a literal in the file it scans.
    #[test]
    fn no_second_name_grammar_lives_in_this_module() {
        let source = include_str!("mutate.rs");
        let code: String = source
            .split("#[cfg(test)]")
            .next()
            .unwrap_or(source)
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            code.contains("validate_toolchain_name"),
            "the writer must route through the reader's grammar, not re-implement it"
        );
        for forbidden in [
            "is_alphanumeric",
            "is_ascii_alphanumeric",
            "SLUG_MAX_LEN",
            "fn validate_binding_name",
        ] {
            assert!(
                !code.contains(forbidden),
                "{forbidden:?} is a second copy of the grammar `project::config` owns"
            );
        }
    }

    /// C-042 / E-C9: the write path is new, so the symlink refusal is asserted
    /// rather than assumed. `$OCX_HOME/ocx.toml` is a path an attacker who can
    /// write the store root could replace with a link to something else, and
    /// the acquire is what refuses to follow it.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_refuses_a_symlink_at_the_config_path() {
        use crate::activate::ActivateMode;

        let dir = tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere.toml");
        fs::write(&elsewhere, "[tools]\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, toml(dir.path())).unwrap();

        let err = set_activate(&toml(dir.path()), ActivateMode::Bin)
            .await
            .expect_err("a symlink at the config path must be refused, not followed");

        assert!(
            matches!(&err, Error::Project(pe) if !matches!(pe.kind, ProjectErrorKind::ManifestEditDiverged)),
            "expected a refusal from the lock acquire; got: {err}"
        );
        assert_eq!(
            fs::read_to_string(&elsewhere).unwrap(),
            "[tools]\n",
            "the link target must not be written through"
        );
    }

    /// C-042 / E-C10: another process holding the flock yields
    /// [`ProjectErrorKind::Locked`] after the contention budget — never a
    /// half-written file. The sibling of
    /// `add_binding_returns_locked_when_ocx_toml_already_locked`, asserted for
    /// the new writer because it is a second entry into the same lock.
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_returns_locked_when_ocx_toml_already_locked() {
        use crate::activate::ActivateMode;

        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "[tools]\n");
        let guard = LockedFile::try_exclusive(&toml(dir.path()))
            .await
            .unwrap()
            .expect("first exclusive lock on ocx.toml must succeed");

        let err = set_activate(&toml(dir.path()), ActivateMode::Bin)
            .await
            .expect_err("set_activate must fail with Locked while ocx.toml is exclusively held");

        assert!(
            matches!(&err, Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::Locked)),
            "expected ProjectErrorKind::Locked; got: {err}"
        );

        // Released before the file is read back. `flock` is advisory, but
        // Windows' `LockFileEx` is mandatory over the locked range and fails an
        // unrelated handle's read with `ERROR_LOCK_VIOLATION` — so reading
        // through the live guard measures the platform's lock semantics, not
        // whether the contended write left a byte.
        drop(guard);
        assert_eq!(
            fs::read_to_string(toml(dir.path())).unwrap(),
            "[tools]\n",
            "a contended write must leave the file byte-identical"
        );
    }

    /// C-042 / E-C12: a file whose last line carries no trailing newline must
    /// not have the `activate` key glued onto it.
    ///
    /// The table-less case is the one that reaches `document::take_header` —
    /// `toml_edit` files every comment of a table-less document under trailing
    /// trivia — and `$OCX_HOME/ocx.toml` is table-less exactly when a corporate
    /// rollout creates it for this key alone.
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_does_not_glue_the_key_onto_an_unterminated_last_line() {
        use crate::activate::ActivateMode;

        let dir = tempdir().unwrap();
        write_minimal_toml(dir.path(), "# how the toolchain reaches PATH");

        set_activate(&toml(dir.path()), ActivateMode::Env)
            .await
            .expect("write succeeds");

        let rendered = fs::read_to_string(toml(dir.path())).unwrap();
        assert!(
            rendered
                .lines()
                .any(|line| line.trim_start().starts_with("activate") && line.contains("\"env\"")),
            "the key must occupy its own line: {rendered:?}"
        );
        assert!(rendered.contains("# how the toolchain reaches PATH"), "{rendered:?}");
        assert_eq!(reload_config(dir.path()).activate, Some(ActivateMode::Env));
    }

    /// C-042 / E-C8: every legal value round-trips through the **parser**, not
    /// through a text grep — a grep for `activate = "bin"` passes for a file
    /// the reader would refuse.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_activate_mode_round_trips_through_the_reader() {
        use crate::activate::ActivateMode;

        for mode in [ActivateMode::Env, ActivateMode::Bin, ActivateMode::None] {
            let dir = tempdir().unwrap();
            write_minimal_toml(dir.path(), "[tools]\ncmake = \"example.com/cmake:3.28\"\n");

            set_activate(&toml(dir.path()), mode).await.expect("write succeeds");

            let cfg = reload_config(dir.path());
            assert_eq!(cfg.activate, Some(mode), "for {mode}");
            assert!(
                cfg.tools.contains_key("cmake"),
                "the binding must survive the activate write: {mode}"
            );
        }
    }

    /// C-042 / E-C4: an existing `activate` set to a **different** value is
    /// replaced in place and the surrounding bytes survive.
    #[tokio::test(flavor = "multi_thread")]
    async fn set_activate_replaces_a_different_value_in_place() {
        use crate::activate::ActivateMode;

        let dir = tempdir().unwrap();
        let original = "#:schema https://ocx.sh/schemas/project/v1.json\nactivate = \"env\"\n\n[tools]\ncmake = \"example.com/cmake:3.28\"\n";
        write_minimal_toml(dir.path(), original);

        set_activate(&toml(dir.path()), ActivateMode::None)
            .await
            .expect("write succeeds");

        let rendered = fs::read_to_string(toml(dir.path())).unwrap();
        assert_eq!(rendered, original.replace("activate = \"env\"", "activate = \"none\""));
    }
}
