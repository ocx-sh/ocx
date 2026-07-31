// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared project-tier resolution prologue for `pull.rs` and `run.rs`.
//!
//! Consolidates the project-context loading logic currently inlined in
//! `pull.rs:58–135` (Phase 2–3: project resolution, lock load, staleness
//! gate) into a single reusable async helper.
//! Callers: `command/pull.rs` (Phase 4 extraction), `command/run.rs`.
//!
//! # Project-registry registration is lock-write-driven, not load-driven
//!
//! `load_project_with_lock` (and its mutating sibling
//! `load_project_for_mutate`) deliberately do NOT register the resolved
//! `ocx.lock` path in the per-user `ProjectRegistry`. Registration happens
//! exclusively at lock-write sites — `ProjectLock::save` (used by
//! `ocx lock` / `ocx update`) and `MutationGuard::commit` (used by
//! `ocx add` / `ocx remove`) — which already hold the project flock and
//! own the atomic-rename of `ocx.lock`.
//!
//! Rationale: the previous load-driven path did a stat + (when the lock
//! existed) flock + JSON read + tempfile + atomic-rename + parent fsync
//! on every `ocx run` / `ocx pull`. Direnv-style use re-runs `ocx run`
//! at every shell prompt, which made the registry write the dominant
//! cost of warm reads. Moving registration to the write side recovers
//! that overhead at the cost of one documented behaviour change: a
//! pure-`ocx pull` workflow (no preceding `ocx lock`) no longer auto-
//! registers the project on first pull. The first explicit
//! lock-mutating command is what installs the registry entry. See ADR
//! `adr_clean_project_backlinks.md` for the original
//! "register at every project-tier touch" intent that this perf fix
//! narrows.

use std::path::{Path, PathBuf};

use ocx_lib::package::metadata::env::entry::Entry;
use ocx_lib::project::{
    DEFAULT_GROUP, ManifestSnapshot, MutationGuard, Origin, ProjectConfig, ProjectLock, SelectedTool,
    acquire_project_lock_for_file, lock::lock_path_for,
};

/// Result of resolving the project tier: owned paths, parsed config, parsed lock.
///
/// All four fields are owned (`PathBuf`, parsed structs) so the caller can
/// drop the helper's borrow on `Context` immediately after this returns
/// and continue using `Context` freely.
pub struct ProjectContext {
    /// Absolute path to the `ocx.toml` file that was loaded.
    pub config_path: PathBuf,
    /// Absolute path to the sibling `ocx.lock` file that was loaded.
    pub lock_path: PathBuf,
    /// Parsed project configuration from `ocx.toml`.
    pub config: ProjectConfig,
    /// Parsed project lock from `ocx.lock`.
    pub lock: ProjectLock,
}

/// Failure modes surfaced by [`load_project_with_lock`].
///
/// Each variant maps to a concrete CLI exit code at the command boundary;
/// the helper itself does not `eprintln` (so callers retain control over the
/// exact message wording) and does not return `ExitCode` directly (so the
/// helper stays usable from non-CLI consumers).
///
/// Variant → exit code mapping:
/// - `NoProject`   → 64 (`UsageError`)
/// - `LockMissing` → 78 (`ConfigError`)
/// - `StaleLock`   → 65 (`DataError`)
/// - `Project`     → propagated via existing `ClassifyExitCode` for `ocx_lib::project::Error`
/// - `Config`      → propagated via existing `ClassifyExitCode` for `ocx_lib::config::error::Error`
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProjectContextError {
    /// No `ocx.toml` was found in `cwd` or any parent directory (nor via the
    /// `OCX_PROJECT` env override or `--project` flag).
    #[error("no ocx.toml found in {cwd} or any parent; run `ocx init` to create one")]
    NoProject { cwd: PathBuf },

    /// `ocx.toml` was found but the sibling `ocx.lock` is absent. The user
    /// must run `ocx lock` to create it before project-tier commands that
    /// require a lock can proceed.
    #[error("ocx.lock not found at {path}; run `ocx lock` to create it")]
    LockMissing { path: PathBuf },

    /// `ocx.lock` exists but its stored `declaration_hash` no longer matches
    /// the hash of the current `ocx.toml`. The lock is stale and must be
    /// regenerated with `ocx lock`.
    #[error("ocx.lock is stale (ocx.toml changed since last `ocx lock`); run `ocx lock`")]
    StaleLock { lock_path: PathBuf },

    /// A project-tier library error (parse failure, identifier error, etc.)
    /// propagated from `ocx_lib::project`. Display delegates to the inner
    /// error; `source()` returns the inner so `classify_error`'s chain walker
    /// reaches `ocx_lib::project::Error` and classifies via its
    /// `ClassifyExitCode` impl. (`#[error(transparent)]` would forward
    /// `source` past the inner, skipping classification.)
    #[error("{0}")]
    Project(#[from] ocx_lib::project::Error),

    /// A config-tier library error propagated from the config loader
    /// (e.g. `ProjectConfig::resolve` returning a `crate::config::error::Error`
    /// when an explicit `--project` path is absent or unreadable). Same
    /// `#[error("{0}")]` rationale as `Project`.
    #[error("{0}")]
    Config(#[from] ocx_lib::ConfigError),
}

impl ocx_lib::cli::ClassifyExitCode for ProjectContextError {
    fn classify(&self) -> Option<ocx_lib::cli::ExitCode> {
        use ocx_lib::cli::ExitCode;
        match self {
            // Misuse: the user pointed a project-tier command at a tree with
            // no `ocx.toml`.
            Self::NoProject { .. } => Some(ExitCode::UsageError),
            // Project exists but is not locked — a configuration gap.
            Self::LockMissing { .. } => Some(ExitCode::ConfigError),
            // Lock exists but disagrees with `ocx.toml` — stale on-disk data.
            Self::StaleLock { .. } => Some(ExitCode::DataError),
            // Defer to the wrapped library error's own classification via the
            // `source()` chain (both variants carry `#[source]` through
            // `#[from]`).
            Self::Project(_) | Self::Config(_) => None,
        }
    }
}

/// Load `ocx.toml`, its sibling `ocx.lock`, validate the staleness gate,
/// and register the lock in the per-user project registry.
///
/// Encapsulates the prologue currently inlined in `command/pull.rs` Phase 2–3:
///
/// 1. Resolve `ocx.toml` + sibling `ocx.lock` paths via the full precedence
///    chain (`--global`/`OCX_GLOBAL` selector ▸ `--project` ▸ `OCX_PROJECT`
///    ▸ CWD walk ▸ None).
/// 2. Load [`ProjectConfig`] from disk.
/// 3. Load [`ProjectLock`] from disk.
/// 4. Verify the lock's stored `declaration_hash` matches the current config
///    (`DataError` / exit 65 on mismatch).
///
/// Registration of the lock path in `ProjectRegistry` is deliberately
/// NOT performed here — it lives exclusively at lock-write sites (see the
/// module doc comment). Hot-path callers (`ocx run`, `ocx pull`) only pay
/// for the staleness gate, not the registry write.
///
/// # Errors
///
/// Returns `Err(ProjectContextError::NoProject)` when no `ocx.toml` is
/// reachable. Returns `Err(ProjectContextError::LockMissing)` when the lock
/// file does not exist. Returns `Err(ProjectContextError::StaleLock)` when
/// the lock's declaration hash does not match the current config. Returns
/// `Err(ProjectContextError::Project)` or `Err(ProjectContextError::Config)`
/// for lower-level parse or I/O errors.
/// Auto-create `$OCX_HOME/ocx.toml` when `context.global()` is true
/// (set by root `--global` / `OCX_GLOBAL`) and a mutator (e.g. `ocx --global add`)
/// runs against an absent global file (F7, adr_global_toolchain_tier.md §Decision 3).
///
/// Mirrors what project `add` would do on a fresh project, except project
/// `add` deliberately refuses to scaffold (exit 64) — the global tier is
/// the one place auto-init is sanctioned, because there is no
/// `ocx init`-equivalent for `$OCX_HOME` and the user explicitly opted
/// into the global file with `--global`. Reuses
/// [`ocx_lib::project::init_project`] rather than re-implementing the
/// scaffold (feedback_extend_dont_duplicate).
///
/// No-op when `context.global()` is false (a CWD-discovered project must
/// never be auto-scaffolded) or the global file already exists. Idempotent
/// under two distinct race shapes, both benign:
///
/// - **Sequential re-entry** (the caller probed before another mutator's
///   write landed, then `init_project` ran second): `init_project`'s own
///   `symlink_metadata` check sees the file and returns
///   `ProjectErrorKind::ConfigAlreadyExists`, which is swallowed — the file
///   the caller wanted now exists.
/// - **Genuinely concurrent**: both processes pass the `symlink_metadata`
///   check and both `rename(2)`-write the *identical* fixed empty scaffold.
///   Neither yields `ConfigAlreadyExists`; the double-write is an
///   idempotent overwrite with the same bytes. This is accepted because
///   real binding writes are flock-protected in `MutationGuard::commit` —
///   only the fixed empty scaffold is written here, never user data. Making
///   this init atomic is a deferred decision, not a correctness gap.
///
/// # Errors
///
/// Propagates `ProjectContextError::Project` for an I/O failure writing the
/// scaffold (other than the benign already-exists race).
pub async fn ensure_global_project_initialized(context: &crate::app::Context) -> Result<(), ProjectContextError> {
    use ocx_lib::project::error::ProjectErrorKind;

    if !context.global() {
        return Ok(());
    }

    let home = context.file_structure().root().to_path_buf();
    let config_path = home.join("ocx.toml");

    // `symlink_metadata` (via `init_project`) is the authoritative
    // existence check; this fast-path probe only avoids the spawn_blocking
    // hop on the common already-initialised case.
    if tokio::fs::symlink_metadata(&config_path).await.is_ok() {
        return Ok(());
    }

    let init_path = config_path.clone();
    let result = tokio::task::spawn_blocking(move || ocx_lib::project::init_project(&init_path))
        .await
        .map_err(|e| {
            ProjectContextError::Project(ocx_lib::project::Error::Project(
                ocx_lib::project::error::ProjectError::new(
                    config_path.clone(),
                    ProjectErrorKind::Io(std::io::Error::other(e)),
                ),
            ))
        })?;

    match result {
        Ok(_) => Ok(()),
        // Sequential re-entry: another global mutator's write landed
        // between our fast-path probe and `init_project`'s own
        // `symlink_metadata` check. The file the caller wanted now exists,
        // so swallow. (The genuinely-concurrent path never reaches here —
        // it double-writes the identical fixed scaffold; see fn doc.)
        Err(ocx_lib::project::Error::Project(pe))
            if matches!(pe.kind, ProjectErrorKind::ConfigAlreadyExists { .. }) =>
        {
            Ok(())
        }
        Err(e) => Err(ProjectContextError::Project(e)),
    }
}

/// Resolve the in-scope `ocx.toml` and its sibling `ocx.lock` path, and stop.
///
/// The path half of [`load_project_with_lock`], without the loads or the two
/// gates that helper enforces (`LockMissing` → 78, `StaleLock` → 65). `ocx
/// status` exists to *describe* both of those states, so it must not be
/// refused by them; it loads the two files itself and reports what it finds.
///
/// The lock path is returned whether or not the file exists — the caller
/// decides what absence means.
///
/// # Errors
///
/// [`ProjectContextError::NoProject`] when no `ocx.toml` is reachable through
/// the precedence chain, or [`ProjectContextError::Config`] for a resolution
/// I/O failure.
pub async fn resolve_project_paths(context: &crate::app::Context) -> Result<(PathBuf, PathBuf), ProjectContextError> {
    use ocx_lib::env;
    use ocx_lib::project::error::{ProjectError, ProjectErrorKind};

    let cwd = env::current_dir().map_err(|e| {
        ProjectContextError::Project(ocx_lib::project::Error::Project(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::Io(e),
        )))
    })?;
    let home = context.file_structure().root().to_path_buf();
    let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home), context.global()).await?;

    resolved.ok_or(ProjectContextError::NoProject { cwd })
}

pub async fn load_project_with_lock(context: &crate::app::Context) -> Result<ProjectContext, ProjectContextError> {
    use ocx_lib::env;
    use ocx_lib::project::error::{ProjectError, ProjectErrorKind};

    // Resolve `ocx.toml` + sibling `ocx.lock` paths with the full precedence
    // chain: `--global`/`OCX_GLOBAL` selector ▸ `--project` ▸ `OCX_PROJECT`
    // ▸ CWD walk ▸ None.
    let cwd = env::current_dir().map_err(|e| {
        ProjectContextError::Project(ocx_lib::project::Error::Project(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::Io(e),
        )))
    })?;
    let home = context.file_structure().root().to_path_buf();
    let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home), context.global()).await?;

    let (config_path, lock_path) = match resolved {
        Some(pair) => pair,
        None => {
            return Err(ProjectContextError::NoProject { cwd });
        }
    };

    // Load the config so callers can validate `--group` names against real
    // group keys before touching the lock.
    let config = ProjectConfig::from_path(&config_path).await?;

    // Open without holding an advisory lock — read-only on ocx.lock;
    // only `ocx lock` and `ocx update` write it.
    let lock = match ProjectLock::from_path(&lock_path).await? {
        Some(l) => l,
        None => {
            return Err(ProjectContextError::LockMissing { path: lock_path });
        }
    };

    // Project-registry registration is intentionally NOT performed here —
    // it happens at lock-write sites (`ProjectLock::save` and
    // `MutationGuard::commit`). See the module doc comment for the full
    // rationale. The `_` discard below documents that the home path is
    // intentionally unused here (callers can still reach it via
    // `Context::file_structure().root()` if they need it).
    let _ = context.file_structure().root();

    // Staleness gate: the lock's stored declaration_hash must match
    // the current config. A mismatch means `ocx.toml` changed since
    // the lock was written → DataError (exit 65).
    //
    // Use the cached accessor so the JCS canonicalization + SHA-256 cost is
    // paid once per loaded `ProjectConfig`. Hot-path callers (`ocx run`,
    // `ocx pull`) hit this gate on every invocation and previously
    // recomputed the hash from scratch on each call.
    if lock.metadata.declaration_hash != config.declaration_hash_cached() {
        return Err(ProjectContextError::StaleLock { lock_path });
    }

    Ok(ProjectContext {
        config_path,
        lock_path,
        config,
        lock,
    })
}

/// Materialize all bindings from `lock` into the object store via
/// `PackageManager::pull_all`. Pure object-store warming: pulls blobs and
/// assembles package content, never touches the `symlinks/` namespace.
///
/// Toolchain-tier commands (`add`, `lock`, `update`) declare bindings in
/// `ocx.toml` + `ocx.lock`; resolution at use-time goes through the lock
/// (project tier) or `resolve_global_pinned_env` (global tier, ADR D5
/// amended 2026-05-19). Neither path consults candidate or `current`
/// symlinks, so creating them here would only produce a second, redundant
/// GC root and conflate the OCI-tier `ocx package install` abstraction
/// with the toolchain-tier mutator semantics. Users that want a stable
/// per-repo anchor invoke `ocx package install` / `ocx package select`
/// explicitly.
///
/// When `eager` is `false`, returns immediately without contacting the
/// manager. This is the no-op path used by `--no-pull` callers.
///
/// `platform` selects which platform leaf to materialize (already defaulted
/// to the host native platform by the caller via
/// [`crate::conventions::platform_or_default`] when `--platform` was
/// omitted). The V2 lock pins every shipped platform's leaf, so this only
/// chooses which locked leaf to fetch — the lock stays host-agnostic. A
/// platform the publisher does not ship surfaces `NoHostLeaf` (exit 78).
///
/// Failures here do NOT roll back the manifest/lock — the binding is
/// declaratively present even if the pull needs a retry. Matches the
/// established `add.rs` semantics.
///
/// `--offline` is honoured transitively: `pull_all` calls
/// `manager.require_client()` for every cache-miss layer, returning
/// `Error::OfflineMode` (→ exit code `PolicyBlocked`) before any
/// filesystem mutation.
///
/// # Errors
///
/// Propagates errors from `PackageManager::pull_all` when `eager` is
/// `true`.
pub async fn materialize_lock(
    context: &crate::app::Context,
    lock: &ocx_lib::project::ProjectLock,
    eager: bool,
    platform: ocx_lib::oci::Platform,
) -> anyhow::Result<()> {
    if !eager {
        return Ok(());
    }
    // Resolve each tool to its pull identifier: the requested platform's
    // leaf via `repository.clone_with_digest(leaf)` (host key → `Any`-offer
    // fallback); a genuinely unshipped platform surfaces `NoHostLeaf` (exit
    // 78).
    let mut identifiers: Vec<ocx_lib::oci::Identifier> = Vec::new();
    for tool in &lock.tools {
        let identifier = host_materialize_identifier(tool, &platform)?;
        // ponytail: O(n) dedup over tools — tiny (a handful). A HashSet buys
        // nothing at this scale.
        if !identifiers.contains(&identifier) {
            identifiers.push(identifier);
        }
    }
    context
        .manager()
        .pull_all(&identifiers, platform, context.concurrency())
        .await?;
    Ok(())
}

/// Resolve a locked tool to its host-platform pull [`ocx_lib::oci::Identifier`]
/// for materialization.
///
/// Delegates the V1/V2 host-leaf resolution to
/// [`ocx_lib::project::host_leaf_identifier`] — the single source of the
/// absent-host-leaf error ([`ProjectErrorKind::NoHostLeaf`], exit 78) — so the
/// condition classifies identically across `compose_tool_set`, `ocx pull`, and
/// this materialization path. The `ProjectError` is converted to
/// `anyhow::Error` so the chain still classifies at the `main.rs` boundary.
///
/// [`ProjectErrorKind::NoHostLeaf`]: ocx_lib::project::error::ProjectErrorKind::NoHostLeaf
fn host_materialize_identifier(
    tool: &ocx_lib::project::LockedTool,
    host: &ocx_lib::oci::Platform,
) -> anyhow::Result<ocx_lib::oci::Identifier> {
    ocx_lib::project::host_leaf_identifier(tool, host).map_err(anyhow::Error::from)
}

/// Reject empty comma segments in a repeatable `--group` value.
///
/// clap's `value_delimiter = ','` splits `-g ci,,lint` into
/// `["ci", "", "lint"]`; an empty element is a stray-comma typo → exit 64.
/// Runs before any config load (both tiers), so it is config-free. Shared by
/// `ocx pull` and `ocx env`.
pub(crate) fn ensure_group_segments_nonempty(groups: &[String]) -> anyhow::Result<()> {
    if groups.iter().any(String::is_empty) {
        return Err(
            ocx_lib::cli::UsageError::new("empty group segment in --group value; check for stray commas").into(),
        );
    }
    Ok(())
}

/// Validate requested `--group` names against the loaded config.
///
/// `default` and `all` are always valid reserved keywords (`all` is expanded
/// downstream); any other name absent from `[group.*]` → exit 64. Shared by
/// the project-tier paths of `ocx pull` and `ocx env`.
pub(crate) fn ensure_groups_known(groups: &[String], config: &ProjectConfig) -> anyhow::Result<()> {
    for raw in groups {
        if raw == ocx_lib::project::DEFAULT_GROUP || raw == ocx_lib::project::ALL_GROUP {
            continue;
        }
        if !config.groups.contains_key(raw) {
            return Err(ocx_lib::cli::UsageError::new(format!("unknown group '{raw}' in --group filter")).into());
        }
    }
    Ok(())
}

/// Narrow a [`select_tool_set`](ocx_lib::project::select_tool_set) result to the
/// explicitly-requested binding names.
///
/// Operates on resolution-free [`SelectedTool`]s, reading only `binding` and
/// `origin`, so host-leaf resolution happens after the narrowing and an
/// unrelated, unnamed sibling that ships no leaf for this host never aborts the
/// command. Run
/// [`check_duplicate_selection`](ocx_lib::project::check_duplicate_selection)
/// on the result before resolving.
///
/// Empty `names` returns the full set unchanged — every binding in scope
/// participates. Otherwise user-supplied name order wins: the output preserves
/// the order of `names`, not of `selected`. A name repeated on the command line
/// is silently deduplicated (naming one binding twice is not a usage error).
///
/// Shared by `ocx run` and `ocx inspect`, the two commands that accept a NAME
/// subset of the selected groups.
///
/// # Errors
///
/// Exit 64 ([`ocx_lib::cli::UsageError`]) when a requested name matches nothing
/// in the selected groups, or when it matches entries in two or more selected
/// groups that resolve it differently — narrow with `-g <group>`.
pub(crate) fn filter_by_names(selected: Vec<SelectedTool>, names: &[String]) -> anyhow::Result<Vec<SelectedTool>> {
    if names.is_empty() {
        return Ok(selected);
    }

    // Build a `binding -> Vec<index_into_selected>` lookup once, so each
    // user-supplied name costs one probe rather than a scan. Hits are stored as
    // indices so the user-order walk below can clone out of `selected` without
    // reborrowing it.
    let mut hits_by_binding: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::with_capacity(selected.len());
    for (position, tool) in selected.iter().enumerate() {
        hits_by_binding.entry(tool.binding.as_str()).or_default().push(position);
    }

    let mut out = Vec::with_capacity(names.len());
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for name in names {
        if !seen.insert(name.as_str()) {
            continue;
        }
        let hits = hits_by_binding.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        match hits {
            [] => {
                return Err(
                    ocx_lib::cli::UsageError::new(format!("binding '{name}' not found in selected groups")).into(),
                );
            }
            [single] => out.push(selected[*single].clone()),
            // Reachable when two selected groups resolve this binding
            // differently: selection keeps both entries so a NAME filter can
            // still narrow past a conflict it never named.
            [_, _, ..] => {
                let groups: Vec<String> = hits
                    .iter()
                    .filter_map(|&position| match &selected[position].origin {
                        Origin::Group(group) => Some(group.clone()),
                        Origin::Explicit => None,
                    })
                    .collect();
                let groups_str = groups.join(", ");
                return Err(ocx_lib::cli::UsageError::new(format!(
                    "binding '{name}' exists in multiple selected groups: [{groups_str}]; pass `-g <group>` to narrow scope"
                ))
                .into());
            }
        }
    }

    Ok(out)
}

/// Materialize the project's declared `[env]` and the selected groups'
/// `[group.<name>.env]` as resolved [`Entry`] values, in application order.
///
/// This is stages 4 and 5 of the composition order — appended to the entry
/// vector after the package-composed env and the patch overlay, so a constant
/// declared here replaces a package-declared one and a path entry lands ahead of
/// package paths. Stage 6 (`ocx run --env`) is the caller's to append after this.
///
/// `groups` is the already-expanded selection list (post `all` expansion, with
/// the empty case promoted to the default group), in `-g` order. A repeated name
/// keeps only its **last** occurrence so `-g ci -g docs -g ci` really does let
/// `ci` win — which is what "later group wins" means. [`DEFAULT_GROUP`] is
/// skipped: the default group's env is the top-level `[env]` already emitted as
/// stage 4, and there is no `[group.default]` table to read.
///
/// Relative `type = "path"` values resolve against the directory holding
/// `config_path`, never the current directory, so `ocx run` from a subdirectory
/// composes the same `PATH` as from the project root.
pub(crate) fn project_env_entries(config: &ProjectConfig, config_path: &Path, groups: &[String]) -> Vec<Entry> {
    // A resolved `ocx.toml` path always has a parent; `.` keeps a hand-built
    // relative path from panicking rather than inventing a fallback root.
    let project_root = config_path.parent().unwrap_or_else(|| Path::new("."));

    let mut entries = config.env.to_entries(project_root);

    for (index, name) in groups.iter().enumerate() {
        // Later-wins dedup: skip every occurrence but the last.
        if name == DEFAULT_GROUP || groups[index + 1..].contains(name) {
            continue;
        }
        let Some(group) = config.groups.get(name) else {
            continue;
        };
        entries.extend(group.env.to_entries(project_root));
    }

    entries
}

/// Mutation-side counterpart to [`load_project_with_lock`].
///
/// Acquires the project flock, loads the current [`ProjectConfig`]
/// snapshot and the optional predecessor [`ProjectLock`], and returns
/// a [`MutationGuard`] that callers use to stage in-memory mutations
/// and commit them atomically across `ocx.toml` + `ocx.lock`. The
/// guard's flock is held until commit / rollback / drop.
///
/// Unlike [`load_project_with_lock`], the staleness gate is NOT
/// enforced here: mutators (`ocx add`, `ocx remove`, `ocx lock`,
/// `ocx update`) are precisely the commands that *fix* a stale lock.
/// The guard surfaces the predecessor lock verbatim. `add`/`remove`
/// feed it to `resolve_lock_touched`, which carries untouched bindings
/// forward and fails closed (no silent fallback) on pre-mutation drift;
/// `ocx lock`/`ocx update` re-resolve the whole file via `resolve_lock`.
///
/// Bootstrapping case: when `ocx.toml` exists but `ocx.lock` does
/// not, [`MutationGuard::previous_lock`] returns `None`. Callers
/// must use [`ocx_lib::project::resolve_lock`] (full resolve) in
/// that case rather than `resolve_lock_touched`.
///
/// # Errors
///
/// Returns the same `ProjectContextError::NoProject` /
/// `ProjectContextError::Project` / `ProjectContextError::Config`
/// variants as [`load_project_with_lock`] when the project cannot be
/// resolved or its files cannot be loaded. Surfaces
/// `ProjectErrorKind::Locked` (wrapped in `ProjectContextError::Project`)
/// when another writer holds the flock.
pub async fn load_project_for_mutate(context: &crate::app::Context) -> Result<MutationGuard, ProjectContextError> {
    use ocx_lib::env;
    use ocx_lib::project::error::{ProjectError, ProjectErrorKind};

    // Resolve `ocx.toml` + sibling `ocx.lock` paths via the same precedence
    // chain consumed by `load_project_with_lock`: `--global`/`OCX_GLOBAL`
    // selector ▸ `--project` ▸ `OCX_PROJECT` ▸ CWD walk ▸ None.
    let cwd = env::current_dir().map_err(|e| {
        ProjectContextError::Project(ocx_lib::project::Error::Project(ProjectError::new(
            std::path::PathBuf::new(),
            ProjectErrorKind::Io(e),
        )))
    })?;
    let home = context.file_structure().root().to_path_buf();
    let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home), context.global()).await?;
    let (config_path, lock_path) = match resolved {
        Some(pair) => pair,
        None => return Err(ProjectContextError::NoProject { cwd }),
    };

    // Acquire the exclusive flock on the resolved config file BEFORE loading
    // the snapshot so a concurrent writer cannot race us between read and
    // commit. The flock target is the config file itself (typically
    // `ocx.toml`, but may be a custom name when `--project=<custom>.toml`
    // is in effect) — using the actual file path is what makes the flock
    // honour custom config names instead of silently locking a sibling
    // `ocx.toml`. The lock_path derivation in `MutationGuard` continues to
    // use the resolver's `lock_path_for` so the two stay consistent.
    debug_assert_eq!(
        lock_path,
        lock_path_for(&config_path),
        "lock_path must be derived from config_path"
    );
    let mut flock = acquire_project_lock_for_file(&config_path).await?;

    // Load the current `ocx.toml` snapshot THROUGH the lock-owning handle.
    // On Windows `LockFileEx` is per-handle and mandatory: opening a second
    // raw handle on the locked range (which is what `ProjectConfig::from_path`
    // does via `tokio::fs::File::open`) hits `ERROR_LOCK_VIOLATION (33)`. By
    // reading via `flock.read_bytes()` we route through the single
    // lock-owning fd, so the snapshot load is safe regardless of platform.
    let bytes = flock.read_bytes().await.map_err(|e| {
        ocx_lib::project::Error::Project(ocx_lib::project::error::ProjectError::new(
            config_path.clone(),
            ocx_lib::project::error::ProjectErrorKind::Io(std::io::Error::other(e)),
        ))
    })?;
    let config = ProjectConfig::from_toml_bytes_with_path(&bytes, config_path.clone())?;
    // Keep the verbatim text alongside the parsed form: the commit path edits
    // the document the user wrote rather than re-serializing the struct, so
    // comments and declaration order survive the mutation. Parsing above
    // already established the bytes are UTF-8.
    let text = String::from_utf8(bytes).map_err(|e| {
        ocx_lib::project::Error::Project(ocx_lib::project::error::ProjectError::new(
            config_path.clone(),
            ocx_lib::project::error::ProjectErrorKind::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        ))
    })?;
    let manifest = ManifestSnapshot { config, text };

    // Optional predecessor lock — `None` is the bootstrap case. Capture the
    // raw on-disk bytes verbatim alongside the parsed lock so the commit
    // rollback path can restore the predecessor byte-for-byte (a committed V1
    // lock must roll back as V1 — the V2 writer cannot serialize it).
    let previous_lock = ProjectLock::from_path(&lock_path).await?;
    let previous_lock_bytes = match &previous_lock {
        Some(_) => match tokio::fs::read(&lock_path).await {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(ocx_lib::project::Error::Project(ProjectError::new(
                    lock_path.clone(),
                    ProjectErrorKind::Io(e),
                ))
                .into());
            }
        },
        None => None,
    };

    Ok(MutationGuard::from_parts(
        flock,
        config_path,
        lock_path,
        home,
        manifest,
        previous_lock,
        previous_lock_bytes,
    ))
}

#[cfg(test)]
mod tests {
    use ocx_lib::oci::{Digest, Identifier, PinnedIdentifier};
    use ocx_lib::project::ToolSource;

    use super::*;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn pin(repository: &str, marker: char) -> PinnedIdentifier {
        let digest = Digest::Sha256(std::iter::repeat_n(marker, 64).collect());
        let identifier = Identifier::new_registry(repository, "ocx.sh").clone_with_digest(digest);
        PinnedIdentifier::try_from(identifier).expect("digest present")
    }

    fn tool(binding: &str, marker: char, group: &str) -> SelectedTool {
        SelectedTool {
            binding: binding.into(),
            origin: Origin::Group(group.into()),
            source: ToolSource::Explicit(pin(binding, marker).into()),
        }
    }

    // ── filter_by_names ──────────────────────────────────────────────────────

    /// An empty name list means "every binding in scope", not "nothing".
    #[test]
    fn filter_empty_names_returns_full_set() {
        let selected = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(selected.clone(), &[]).expect("empty names must succeed");
        assert_eq!(result.len(), selected.len());
        assert!(result.iter().any(|entry| entry.binding == "cmake"));
        assert!(result.iter().any(|entry| entry.binding == "ninja"));
    }

    /// A name with no match in the selected groups is a usage error.
    #[test]
    fn filter_unknown_name_errors() {
        let selected = vec![tool("cmake", 'a', "default")];
        let error = filter_by_names(selected, &["does-not-exist".into()]).expect_err("unknown name must fail");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("binding 'does-not-exist' not found in selected groups"),
            "unexpected message: {rendered}"
        );
    }

    /// Two selected groups resolving one binding differently is ambiguous only
    /// when the user actually names it — and the message lists both groups so
    /// the `-g` remedy is actionable.
    #[test]
    fn filter_ambiguous_name_errors_with_groups_listed() {
        let selected = vec![tool("tool", 'a', "ci"), tool("tool", 'b', "release")];
        let error = filter_by_names(selected, &["tool".into()]).expect_err("ambiguous name must fail");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("binding 'tool' exists in multiple selected groups"),
            "unexpected message: {rendered}"
        );
        assert!(rendered.contains("ci"), "groups must name 'ci': {rendered}");
        assert!(rendered.contains("release"), "groups must name 'release': {rendered}");
    }

    /// Happy path: exactly one matching entry survives.
    #[test]
    fn filter_unique_name_picks_single_entry() {
        let selected = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(selected, &["cmake".into()]).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].binding, "cmake");
    }

    /// User-supplied name order wins over selection order.
    #[test]
    fn filter_preserves_user_name_order_not_selection_order() {
        let selected = vec![
            tool("a", 'a', "default"),
            tool("b", 'b', "default"),
            tool("c", 'c', "default"),
        ];
        let names: Vec<String> = vec!["b".into(), "a".into()];
        let result = filter_by_names(selected, &names).expect("ok");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].binding, "b", "user-order: b must come first");
        assert_eq!(result[1].binding, "a", "user-order: a must come second");
    }

    /// Naming one binding twice is a typo, not a usage error.
    #[test]
    fn filter_duplicate_names_deduplicated_silently() {
        let selected = vec![tool("cmake", 'a', "default")];
        let names: Vec<String> = vec!["cmake".into(), "cmake".into()];
        let result = filter_by_names(selected, &names).expect("dedup must succeed");
        assert_eq!(result.len(), 1, "duplicate name must be silently deduped to one entry");
        assert_eq!(result[0].binding, "cmake");
    }

    /// The whole point of deferring the duplicate check: a conflict between two
    /// selected groups is dropped by the filter when the user named something
    /// else, so the surviving set validates clean.
    #[test]
    fn filter_drops_an_unnamed_conflict_so_the_check_passes() {
        let selected = vec![
            tool("shellcheck", 'a', "ci"),
            tool("shellcheck", 'b', "lint"),
            tool("cmake", 'c', "ci"),
        ];
        let result = filter_by_names(selected, &["cmake".into()]).expect("naming an unrelated binding must succeed");
        ocx_lib::project::check_duplicate_selection(&result).expect("the surviving set carries no conflict");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].binding, "cmake");
    }
}
