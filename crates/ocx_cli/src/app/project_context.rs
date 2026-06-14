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
/// Toolchain-tier commands (`add`, `lock`, `upgrade`) declare bindings in
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
) -> anyhow::Result<()> {
    if !eager {
        return Ok(());
    }
    // Resolve each locked tool to its host-platform pull identifier. V2
    // ([`LockedResolution::PerPlatform`]): host→`"any"` leaf via
    // `repository.clone_with_digest(leaf)`. V1
    // ([`LockedResolution::LegacyIndex`]): the legacy pinned index id.
    let host = ocx_lib::oci::Platform::current().unwrap_or_else(ocx_lib::oci::Platform::any);
    let identifiers: Vec<ocx_lib::oci::Identifier> = lock
        .tools
        .iter()
        .map(|t| host_materialize_identifier(t, &host))
        .collect::<anyhow::Result<Vec<_>>>()?;
    context
        .manager()
        .pull_all(
            &identifiers,
            crate::conventions::platforms_or_default(&[]),
            context.concurrency(),
        )
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
/// `ocx upgrade`) are precisely the commands that *fix* a stale lock.
/// The guard surfaces the predecessor lock verbatim. `add`/`remove`
/// feed it to `resolve_lock_touched`, which carries untouched bindings
/// forward and fails closed (no silent fallback) on pre-mutation drift;
/// `ocx lock`/`ocx upgrade` re-resolve the whole file via `resolve_lock`.
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
        config,
        previous_lock,
        previous_lock_bytes,
    ))
}
