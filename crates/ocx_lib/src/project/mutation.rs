// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Two-file mutation transaction across `ocx.toml` and `ocx.lock`.
//!
//! Project-tier mutators (`ocx add`, `ocx remove`, `ocx update`,
//! `ocx lock`, and the bootstrapping path of `ocx init`) need to land
//! coordinated changes to both `ocx.toml` (the human-edited
//! declaration) and `ocx.lock` (its resolved snapshot). A naive
//! sequence of writes — `ocx.toml` first, then resolve, then save the
//! lock — leaves `ocx.toml` ahead of the lock if resolution fails: the
//! manifest declares a tool that has no pinned digest, the staleness
//! gate (`ProjectContextError::StaleLock`) fires on every subsequent
//! read, and the user must hand-edit `ocx.toml` back to a consistent
//! state. This is the half-committed-mutation laundering bug surfaced
//! by Codex H2.
//!
//! [`MutationGuard`] inverts the ordering. The lock-first commit path
//! (`MutationGuard::commit`) writes `ocx.lock` first, then `ocx.toml`,
//! both atomically. If the manifest write fails after the lock write,
//! the staged lock is rolled back to the predecessor (or removed when
//! there was none). The reverse failure (lock write fails, manifest
//! untouched) leaves the existing on-disk state unchanged.
//!
//! The guard owns the exclusive advisory flock on `ocx.toml` itself,
//! acquired via [`crate::project::acquire_project_lock`]. The commit
//! body rewrites `ocx.toml` IN PLACE through the lock-owning handle
//! ([`crate::utility::fs::LockedFile::replace_bytes`]) — no tempfile,
//! no rename, no orphan inode. This keeps mutual exclusion on Windows
//! intact (`LockFileEx` is per-handle; a rename would strand the lock
//! fd on the orphan inode). See `project_lock.rs` module-doc and
//! `adr_file_lock_unification.md §Decision 3` for the full rationale.
//!
//! The guard is intentionally neither [`Clone`] nor [`Copy`]: a project
//! can have at most one in-flight mutation transaction at a time, and
//! the flock lifetime tracks the guard's. Drop semantics: dropping a
//! guard without [`MutationGuard::commit`] or
//! [`MutationGuard::rollback`] releases the flock and discards any
//! staged in-memory mutation.
//!
//! Typical use (from a CLI mutator):
//!
//! ```ignore
//! let guard = load_project_for_mutate(&context).await?;
//! let staged = guard.stage(|cfg| { /* in-memory mutation */ Ok(()) })?;
//! let touched = [(group, name)];
//! let new_lock = resolve_lock_touched(staged.config(), guard.config(), guard.previous_lock().unwrap(), index, &touched, opts).await?;
//! guard.commit(staged, new_lock).await?;
//! ```

use std::path::{Path, PathBuf};

use crate::log;
use crate::utility::fs::LockedFile;

use super::Error;
use super::config::ProjectConfig;
use super::error::{ProjectError, ProjectErrorKind};
use super::lock::ProjectLock;

/// RAII handle to an in-flight project-tier mutation transaction.
///
/// Holds the exclusive advisory flock on `ocx.toml` itself, an
/// immutable snapshot of the current [`ProjectConfig`] on disk, the optional
/// predecessor [`ProjectLock`], and the absolute paths of both files.
///
/// Constructed only via the CLI shim
/// `ocx_cli::app::project_context::load_project_for_mutate` (which
/// resolves the project-context precedence chain and acquires the
/// flock). Library consumers that need a guard for testing should
/// stage the prerequisite filesystem layout themselves and call
/// [`Self::from_parts`] (test-only constructor — see `#[cfg(test)]`
/// block below).
///
/// Not `Clone` / `Copy` by design: the flock has unique ownership and
/// a project supports at most one in-flight mutation transaction at a
/// time. Sharing a guard across tasks would race the staged-then-commit
/// invariant; pass references through the staging closure instead.
#[non_exhaustive]
pub struct MutationGuard {
    /// Exclusive advisory flock on `ocx.toml` itself. Released on drop.
    ///
    /// The lock target IS the data file. [`Self::commit`] rewrites `ocx.toml`
    /// in place via [`LockedFile::replace_bytes`] (truncate + write through the
    /// lock-owning handle). No tempfile, no rename — the inode stays stable so
    /// `LockFileEx` on Windows never strands on an orphan inode.
    flock: LockedFile,
    /// Absolute path to `ocx.toml`.
    config_path: PathBuf,
    /// Absolute path to the sibling `ocx.lock`.
    lock_path: PathBuf,
    /// `$OCX_HOME` — propagated so commit can register the lock with
    /// `ProjectRegistry` when the file is materialised for the first time.
    home: PathBuf,
    /// Snapshot of `ocx.toml` parsed at guard-acquisition time.
    config: ProjectConfig,
    /// Predecessor `ocx.lock`. `None` when the lock file does not exist
    /// yet (bootstrapping path: `ocx add` on a freshly initialised
    /// project, `ocx init` itself).
    previous_lock: Option<ProjectLock>,
    /// Raw on-disk bytes of the predecessor `ocx.lock`, captured verbatim at
    /// guard-acquisition time. `None` mirrors [`Self::previous_lock`] (no lock
    /// existed). Used by the rollback path to restore the predecessor
    /// byte-for-byte — including a V1 (`LegacyIndex`) lock, which the V2 writer
    /// refuses to serialize. Restoring the original bytes keeps a rolled-back
    /// mutation from converting (or panicking on) a committed V1 lock.
    previous_lock_bytes: Option<Vec<u8>>,
}

/// In-memory candidate [`ProjectConfig`] produced by
/// [`MutationGuard::stage`].
///
/// Owns the candidate config by value so the resolver layer can borrow
/// it for the entire `resolve_lock` / `resolve_lock_touched` await
/// without aliasing the guard. CLI mutators stage → resolve → commit
/// in three logically distinct steps; tying `StagedMutation`'s lifetime
/// to the guard would force the resolve call to outlive the guard
/// borrow, which the borrow checker rejects on the standard mutator
/// shape.
///
/// Library consumers do not construct `StagedMutation` directly —
/// only [`MutationGuard::stage`] returns it, and
/// [`MutationGuard::commit`] is the only consumer.
///
/// `manifest_changed` is `true` for the common mutator path (`add`,
/// `remove`) where the staging closure modifies the candidate config and
/// the resulting `ocx.toml` must be rewritten on commit. Lock-only
/// commits (`lock`, `update`) set `manifest_changed = false` via
/// [`StagedMutation::lock_only`] so [`MutationGuard::commit`] writes
/// only `ocx.lock` — leaving `ocx.toml` byte-identical even when the
/// staging closure was an identity operation.
#[non_exhaustive]
pub struct StagedMutation {
    /// Candidate `ProjectConfig` ready to be serialised to `ocx.toml`.
    candidate: ProjectConfig,
    /// Whether the candidate differs from the guard's snapshot in a way
    /// that warrants rewriting `ocx.toml`. See type-level docs.
    manifest_changed: bool,
}

/// Outcome record returned by [`MutationGuard::commit`].
///
/// Captures which files the commit actually rewrote so callers can
/// build deterministic post-commit reports (e.g. the `ocx add`
/// success report listing the resolved binding) without re-reading
/// the filesystem.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct MutationCommit {
    /// Absolute path of the `ocx.toml` that was rewritten.
    pub config_path: PathBuf,
    /// Absolute path of the `ocx.lock` that was rewritten (or
    /// freshly created when [`MutationGuard::previous_lock`] was
    /// `None`).
    pub lock_path: PathBuf,
}

impl MutationGuard {
    /// Read-only access to the snapshot of `ocx.toml` taken at
    /// guard-acquisition time.
    ///
    /// The snapshot is what the staging closure mutates a clone of —
    /// the guard's own copy is preserved verbatim until commit. CLI
    /// callers use this to reason about the pre-mutation state
    /// (e.g. detecting a no-op `ocx remove` before resolving anything).
    pub fn config(&self) -> &ProjectConfig {
        &self.config
    }

    /// Read-only access to the predecessor `ocx.lock`, if one existed
    /// on disk at guard-acquisition time.
    ///
    /// `None` indicates a bootstrapping case: the lock file has never
    /// been materialised. CLI callers (`ocx add`/`remove`) inspect this
    /// to choose between [`crate::project::resolve_lock_touched`]
    /// (predecessor present — carries untouched bindings forward,
    /// fail-closed on drift) and [`crate::project::resolve_lock`]
    /// (full bootstrap — nothing to preserve, never fails closed).
    pub fn previous_lock(&self) -> Option<&ProjectLock> {
        self.previous_lock.as_ref()
    }

    /// Absolute path to `ocx.toml`.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Absolute path to the sibling `ocx.lock`.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// `$OCX_HOME` root directory (forwarded from the originating
    /// `Context`). Used by [`Self::commit`] to register the lock with
    /// the per-user `ProjectRegistry` so `ocx clean` does not GC
    /// packages pinned by this project.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Apply a pure-in-memory mutation to a clone of the guard's
    /// snapshot, returning a [`StagedMutation`] ready for commit.
    ///
    /// The closure receives `&mut ProjectConfig` and may freely add,
    /// remove, or rename bindings/groups. It MUST NOT touch the
    /// filesystem — the staging step is intentionally pure so callers
    /// can run an arbitrary number of resolve/probe operations against
    /// the candidate before deciding whether to commit.
    ///
    /// Returns the closure's `Err` verbatim when it fails; the guard
    /// remains valid for retry or rollback.
    ///
    /// # Errors
    ///
    /// Whatever [`Error`] the closure returns. The guard itself does
    /// not surface new errors at this stage.
    pub fn stage<F>(&self, mutate: F) -> Result<StagedMutation, Error>
    where
        F: FnOnce(&mut ProjectConfig) -> Result<(), Error>,
    {
        let mut candidate = self.config.clone();
        mutate(&mut candidate)?;
        Ok(StagedMutation {
            candidate,
            manifest_changed: true,
        })
    }

    /// Atomically commit `staged` plus a freshly resolved
    /// [`ProjectLock`] to disk under the guard's flock.
    ///
    /// Ordering is **lock-first**: `ocx.lock` is rewritten via the
    /// existing tempfile-and-rename helper in
    /// [`crate::project::lock::ProjectLock::save`] (which respects
    /// the `previous` byte-for-byte preservation contract), then
    /// `ocx.toml` is rewritten IN PLACE through the lock-owning handle
    /// via [`LockedFile::replace_bytes`]. The `ocx.lock` write uses
    /// `tempfile + rename + parent fsync`; the `ocx.toml` rewrite is a
    /// truncate-and-write on the locked inode so mutual exclusion on
    /// Windows remains intact (no inode rotation under a held
    /// `LockFileEx`).
    ///
    /// Kill-9 trade-off: a SIGKILL between the `ocx.toml` `set_len(0)`
    /// and `sync_data` leaves the file truncated or partial.
    /// Recovery is manual.
    ///
    /// - Lock write fails → return error, `ocx.toml` untouched on disk.
    /// - Lock write succeeds, manifest write fails → roll the lock
    ///   back to the predecessor (re-`save` the original
    ///   [`Self::previous_lock`]), or remove it when there was none,
    ///   then return the manifest error.
    /// - Both succeed → register the lock in `ProjectRegistry`
    ///   (non-fatal) and return [`MutationCommit`].
    ///
    /// The flock held by the guard is released when this method
    /// returns (success or failure) — the guard is consumed.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O / serialisation error from whichever
    /// write fails. On rollback, only the *original* error is
    /// surfaced; rollback failures log at WARN and do not mask the
    /// primary failure. This matches the design principle that
    /// callers should always see the first thing that went wrong.
    pub async fn commit(mut self, staged: StagedMutation, new_lock: ProjectLock) -> Result<MutationCommit, Error> {
        // Defense-in-depth coherence gate: the lock the caller hands us
        // must claim the same `declaration_hash` as the candidate config
        // we are about to write. If the resolver path produced a lock
        // whose metadata hash doesn't match, refuse to commit — that's
        // exactly the laundering shape Codex H1 + the
        // `StaleLockOnPartial` gate exist to prevent.
        let candidate_hash = staged.candidate.declaration_hash_cached();
        if new_lock.metadata.declaration_hash != candidate_hash {
            return Err(ProjectError::new(
                self.config_path.clone(),
                ProjectErrorKind::StaleLockOnPartial {
                    previous_hash: new_lock.metadata.declaration_hash.clone(),
                    current_hash: candidate_hash.to_string(),
                },
            )
            .into());
        }

        // Read the fault-injection hook ONCE up front so production paths
        // make no further env probes during the commit body.
        let fault = read_fault_hook();

        // Stage 0: pre-write fault — abort before either rename.
        maybe_inject_fault(fault.as_deref(), CommitStage::BeforeLockRename).await?;

        // Stage 1: lock-first atomic rename. This call already uses
        // tempfile + rename + parent fsync inside `ProjectLock::save`.
        // Failure here leaves `ocx.toml` byte-identical on disk, so no
        // rollback is needed for this branch.
        new_lock
            .save(
                &self.lock_path,
                self.previous_lock.as_ref(),
                &self.home,
                &self.config_path,
            )
            .await?;

        // ── Post-lock-rename rollback boundary ──────────────────────────
        //
        // From here on, the new `ocx.lock` is committed to disk. Every
        // subsequent fallible step must restore the predecessor lock (or
        // delete the freshly-created lock when there was none) before
        // surfacing the original error. Otherwise the lock advances while
        // the manifest stays old → declaration-hash gate sees a stale
        // pair → project wedged. Codex Critical-1 finding.
        //
        // Closure aggregates the post-rename fallible work so a single
        // `match` arm runs the rollback path on any error.
        let post_rename: Result<(), Error> = async {
            // Stage 2: post-lock-write fault — return Err to exercise the
            // mid-failure path. The on-disk lock has been renamed; manifest
            // is untouched until the spawn_blocking write below.
            maybe_inject_fault(fault.as_deref(), CommitStage::AfterLockWrite).await?;

            // Stage 3: optional pause — block here until the release file
            // appears so an external test can SIGKILL the process between
            // the lock rename and the manifest rewrite.
            maybe_inject_fault(fault.as_deref(), CommitStage::PauseBeforeManifestWrite).await?;

            // Stage 4: rewrite ocx.toml IN PLACE through the lock-owning
            // handle. Only when the staging closure actually changed the
            // manifest — lock-only commits (`lock`, `update`) legitimately
            // want to leave ocx.toml byte-identical.
            if staged.manifest_changed {
                let serialized = config_to_toml_string(&staged.candidate)?;
                self.flock.replace_bytes(serialized.as_bytes()).await.map_err(|e| {
                    ProjectError::new(self.config_path.clone(), ProjectErrorKind::Io(std::io::Error::other(e)))
                })?;
            }
            Ok(())
        }
        .await;

        if let Err(primary) = post_rename {
            rollback_lock_after_failure(&self.lock_path, self.previous_lock_bytes.as_deref()).await;
            return Err(primary);
        }

        // Best-effort GC-ledger registration so `ocx clean` retains packages
        // pinned by this lock. The shared infallible helper owns the
        // canonicalize→parent→register derivation and the WARN-on-failure
        // silent-data-loss policy; a registration failure never aborts the
        // commit (next `ocx lock` re-registers).
        super::registry::register_project_dir_best_effort(&self.config_path, &self.home).await;

        Ok(MutationCommit {
            config_path: self.config_path,
            lock_path: self.lock_path,
        })
        // Guard's flock released here on drop.
    }

    /// Drop the guard without writing anything.
    ///
    /// Equivalent to `let _ = guard;` but documents intent at the
    /// call site: the caller decided not to commit (e.g. validation
    /// failed after staging, the user passed `--dry-run`, etc.).
    /// Releases the flock; the on-disk `ocx.toml` and `ocx.lock` are
    /// untouched.
    pub fn rollback(self) {
        // No-op: the `Drop` impl on `FileLock` releases the OS lock
        // when `self` goes out of scope at the end of this function.
        // The method exists to document intent at call sites.
    }
}

impl StagedMutation {
    /// Read-only access to the candidate config produced by the
    /// staging closure. CLI callers feed this into the resolver
    /// (`resolve_lock` / `resolve_lock_touched`) before
    /// [`MutationGuard::commit`].
    pub fn config(&self) -> &ProjectConfig {
        &self.candidate
    }

    /// Whether [`MutationGuard::commit`] should rewrite `ocx.toml` on
    /// commit. `true` for binding-mutating commands (`add`, `remove`);
    /// `false` for lock-only commands (`lock`, `update`) where the
    /// candidate config is byte-identical to the guard's snapshot and
    /// the on-disk manifest must NOT be re-serialised (which would
    /// churn timestamps / formatting on equivalent input).
    pub fn manifest_changed(&self) -> bool {
        self.manifest_changed
    }

    /// Mark the staged mutation as lock-only: the commit will rewrite
    /// `ocx.lock` but leave `ocx.toml` untouched on disk. Intended for
    /// `ocx lock` / `ocx update` paths whose staging closure is the
    /// identity function but which still need transactional ordering
    /// across the lock-write step.
    #[must_use]
    pub fn lock_only(mut self) -> Self {
        self.manifest_changed = false;
        self
    }
}

// ── construction ─────────────────────────────────────────────────────────
//
// `MutationGuard` is constructed only by the CLI shim
// `ocx_cli::app::project_context::load_project_for_mutate`, which lives
// in `ocx_cli` (a different crate) and therefore must reach the
// constructor through a `pub` API. The associated function below is
// `pub` for that reason but documents the invariants the shim is
// expected to uphold: in particular, the flock must already have been
// acquired via `acquire_project_lock` (the only sanctioned ingress —
// constructing a `MutationGuard` without one would silently let a
// second writer race the commit).

impl MutationGuard {
    /// Assemble a [`MutationGuard`] from already-validated parts.
    ///
    /// Callers MUST have already acquired the exclusive advisory flock on
    /// `ocx.toml` via [`crate::project::acquire_project_lock`]
    /// and loaded both the current [`ProjectConfig`] and the optional
    /// predecessor [`ProjectLock`]. `previous_lock_bytes` carries the raw
    /// on-disk bytes of that predecessor (captured verbatim so the rollback
    /// path can restore a V1 lock byte-for-byte); it MUST be `Some` exactly
    /// when `previous_lock` is `Some`. The constructor does not re-validate
    /// these inputs — it merely packages them into a guard whose drop glue
    /// releases the flock.
    ///
    /// The only sanctioned external ingress is the CLI shim
    /// `ocx_cli::app::project_context::load_project_for_mutate`, which
    /// is the only call site that performs the full prologue
    /// (precedence chain, flock acquisition, config + lock load,
    /// staleness gate). Library consumers should reach the guard
    /// through that shim rather than calling `from_parts` directly.
    pub fn from_parts(
        flock: LockedFile,
        config_path: PathBuf,
        lock_path: PathBuf,
        home: PathBuf,
        config: ProjectConfig,
        previous_lock: Option<ProjectLock>,
        previous_lock_bytes: Option<Vec<u8>>,
    ) -> Self {
        Self {
            flock,
            config_path,
            lock_path,
            home,
            config,
            previous_lock,
            previous_lock_bytes,
        }
    }
}

// ── Fault injection (test-only hook) ────────────────────────────────────
//
// Production paths cost zero: `read_fault_hook` runs once at the entry of
// `commit` and reads `OCX_TEST_FAULT` via `var_os` (single allocation-free
// probe). When the env var is unset, every subsequent `maybe_inject_fault`
// call is a non-allocating Option::is_none short-circuit.

/// Stages at which a fault may be injected. Each variant maps to a
/// canonical `OCX_TEST_FAULT` value documented on the `MutationGuard`
/// commit doc-comment.
enum CommitStage {
    BeforeLockRename,
    AfterLockWrite,
    PauseBeforeManifestWrite,
}

impl CommitStage {
    fn matches(&self, fault: &str) -> bool {
        match self {
            Self::BeforeLockRename => fault == "before_lock_rename",
            Self::AfterLockWrite => fault == "after_lock_write",
            Self::PauseBeforeManifestWrite => fault == "pause_before_manifest_write",
        }
    }
}

/// Read the `OCX_TEST_FAULT` env var exactly once at the call site.
///
/// Returns `None` when unset (production path; subsequent fault checks
/// short-circuit). Returns `Some(value)` only for non-empty values; an
/// empty value is treated as unset.
fn read_fault_hook() -> Option<String> {
    let raw = std::env::var_os("OCX_TEST_FAULT")?;
    let s = raw.to_string_lossy().into_owned();
    if s.is_empty() { None } else { Some(s) }
}

/// Inject a fault if `fault` matches `stage`.
///
/// `BeforeLockRename` and `AfterLockWrite` return a synthetic I/O error
/// so the caller's `?` propagates as a `ProjectErrorKind::Io`.
/// `PauseBeforeManifestWrite` blocks until the file at
/// `OCX_TEST_FAULT_RELEASE_FILE` exists — used by SIGKILL tests that
/// need a deterministic pause window.
async fn maybe_inject_fault(fault: Option<&str>, stage: CommitStage) -> Result<(), Error> {
    let Some(fault) = fault else {
        return Ok(());
    };
    if !stage.matches(fault) {
        return Ok(());
    }

    match stage {
        CommitStage::BeforeLockRename | CommitStage::AfterLockWrite => Err(ProjectError::new(
            PathBuf::new(),
            ProjectErrorKind::Io(std::io::Error::other(format!(
                "OCX_TEST_FAULT={fault} (test-only hook)"
            ))),
        )
        .into()),
        CommitStage::PauseBeforeManifestWrite => {
            // Wait until the release-file appears; tests SIGKILL during
            // the wait. If the env var isn't set, sleep indefinitely so
            // the test must explicitly arrange a release path.
            let release = std::env::var_os("OCX_TEST_FAULT_RELEASE_FILE");
            loop {
                if let Some(ref path) = release
                    && tokio::fs::metadata(path).await.is_ok()
                {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
}

/// Restore the predecessor `ocx.lock` (or delete the freshly-renamed lock)
/// after a post-lock-rename failure inside [`MutationGuard::commit`].
///
/// Codex Critical-1 contract: when the lock has been renamed but a later
/// commit step fails, the on-disk lock must NOT outlive the failed
/// transaction. Otherwise the next reader sees the new lock paired with
/// the unchanged old `ocx.toml` and the staleness gate refuses every
/// subsequent read until the user hand-edits one of the two files.
///
/// Behaviour:
///
/// - `previous_lock_bytes = Some(bytes)` → write the captured predecessor
///   bytes back **verbatim** under the same atomic-rename + parent-fsync
///   primitive used by the forward path. Writing the raw bytes (rather than
///   re-serializing the parsed predecessor through `ProjectLock::save`) is
///   what lets a committed **V1** (`LegacyIndex`) lock roll back correctly: the
///   V2 writer would hit `unreachable!()` on a `LegacyIndex`, so a re-`save`
///   would panic instead of restoring. The captured bytes restore the
///   predecessor exactly as it was on disk (a V1 lock stays V1).
/// - `previous_lock_bytes = None`        → delete the just-created lock file.
///
/// Rollback failures log at ERROR but never replace the original error —
/// callers must always see the first thing that went wrong (the
/// `quality-rust-errors.md` "first cause" rule). The lock path may end up
/// in an inconsistent on-disk state if rollback itself fails; the WARN
/// log surfaces the divergence so an operator can recover by hand.
async fn rollback_lock_after_failure(lock_path: &Path, previous_lock_bytes: Option<&[u8]>) {
    match previous_lock_bytes {
        Some(bytes) => {
            // Restore the predecessor lock byte-for-byte. The bytes were
            // captured verbatim at guard-acquisition time, so the restored
            // file is identical to the pre-commit on-disk state — including
            // a V1 lock, which the V2 serializer cannot emit.
            if let Err(e) = super::lock::restore_lock_bytes_verbatim(lock_path, bytes.to_vec()).await {
                log::error!(
                    "MutationGuard rollback: failed to restore predecessor ocx.lock at '{}': {e:#}",
                    lock_path.display()
                );
            }
        }
        None => {
            // No predecessor — the lock was created by this commit. Delete
            // it so the on-disk state matches the pre-commit state. A
            // NotFound here is benign (e.g. the forward write itself
            // failed before rename); other errors surface at ERROR.
            match tokio::fs::remove_file(lock_path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    log::error!(
                        "MutationGuard rollback: failed to remove freshly-created ocx.lock at '{}': {e}",
                        lock_path.display()
                    );
                }
            }
        }
    }
}

/// Serialise a [`ProjectConfig`] to a TOML string. Mirrors
/// [`crate::project::mutate::config_to_toml_string`] privately —
/// duplicated here to avoid making the helper `pub(crate)` for one
/// caller. Single 4-line inline.
fn config_to_toml_string(config: &ProjectConfig) -> Result<String, Error> {
    toml::to_string_pretty(config)
        .map_err(|e| ProjectError::new(PathBuf::new(), ProjectErrorKind::TomlSerialize(e)).into())
}
