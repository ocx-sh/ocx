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

use std::path::PathBuf;

use ocx_lib::project::{MutationGuard, ProjectConfig, ProjectLock, acquire_project_lock_for_file, lock::lock_path_for};

/// Result of resolving the project tier: owned paths, parsed config, parsed lock.
///
/// All four fields are owned (`PathBuf`, parsed structs) so the caller can
/// drop the helper's borrow on `Context` immediately after this returns
/// and continue using `Context` freely.
pub struct ProjectContext {
    /// Absolute path to the `ocx.toml` file that was loaded.
    // NOTE: not yet consumed by pull.rs / run.rs but exposed for future
    // callers (add, update, lock) when they migrate to this helper.
    #[allow(dead_code)]
    pub config_path: PathBuf,
    /// Absolute path to the sibling `ocx.lock` file that was loaded.
    // NOTE: not yet consumed by pull.rs / run.rs but exposed for future
    // callers (add, update, lock) when they migrate to this helper.
    #[allow(dead_code)]
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
/// Auto-create `$OCX_HOME/ocx.toml` when a `--global` mutator
/// (`ocx add --global`, the `ocx install --global` add-step) runs against
/// an absent global file (F7, adr_global_toolchain_tier.md §Decision 3).
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
/// The guard surfaces the predecessor lock verbatim and lets the
/// resolver layer decide whether to fall back from
/// `resolve_lock_partial` to `resolve_lock` based on the
/// `StaleLockOnPartial` signal.
///
/// Bootstrapping case: when `ocx.toml` exists but `ocx.lock` does
/// not, [`MutationGuard::previous_lock`] returns `None`. Callers
/// must use [`ocx_lib::project::resolve_lock`] (full resolve) in
/// that case rather than `resolve_lock_partial`.
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
    let flock = acquire_project_lock_for_file(&config_path).await?;

    // Load the current `ocx.toml` snapshot under the flock so any concurrent
    // reader-then-writer race resolves with us as the canonical writer.
    let config = ProjectConfig::from_path(&config_path).await?;

    // Optional predecessor lock — `None` is the bootstrap case.
    let previous_lock = ProjectLock::from_path(&lock_path).await?;

    Ok(MutationGuard::from_parts(
        flock,
        config_path,
        lock_path,
        home,
        config,
        previous_lock,
    ))
}
