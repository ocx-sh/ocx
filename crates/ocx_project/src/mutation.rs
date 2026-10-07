// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Two-file mutation transaction across `ocx.toml` and `ocx.lock`, committed
//! lock-first: manifest-first leaves an unlocked tool that stales every read
//! when resolution fails. Both publish by atomic rename, so no reader sees a
//! torn manifest and a SIGKILL keeps the previous document.
//!
//! ```ignore
//! let guard = load_project_for_mutate(&context).await?;
//! let staged = guard.stage(|cfg| { /* in-memory mutation */ Ok(()) })?;
//! let touched = [(group, name)];
//! let new_lock = resolve_lock_touched(staged.config(), guard.config(), guard.previous_lock().unwrap(), index, &touched, opts).await?;
//! guard.commit(staged, new_lock).await?;
//! ```

use std::path::{Path, PathBuf};

use ocx_util::fs::LockedFile;

use super::Error;
use super::config::ProjectConfig;
use super::error::{ProjectError, ProjectErrorKind};
use super::lock::ProjectLock;

/// The `ocx.toml` a guard was opened on: parsed, plus the verbatim text the
/// commit edits as a document so comments and order survive.
pub struct ManifestSnapshot {
    pub config: ProjectConfig,
    /// The exact text `config` was parsed from.
    pub text: String,
}

/// RAII handle to an in-flight project mutation: the mutation lock, the
/// manifest snapshot and the predecessor lock. Dropping it without commit
/// releases the lock and discards staged state.
#[non_exhaustive]
pub struct MutationGuard {
    /// Held until both files land; released on drop.
    mutate_lock: LockedFile,
    config_path: PathBuf,
    lock_path: PathBuf,
    /// `$OCX_HOME`, for the commit's GC-ledger registration.
    home: PathBuf,
    manifest: ManifestSnapshot,
    /// `None` when no `ocx.lock` exists yet.
    previous_lock: Option<ProjectLock>,
    /// The predecessor's raw bytes, so rollback restores it byte-for-byte
    /// rather than a re-serialization.
    previous_lock_bytes: Option<Vec<u8>>,
}

/// Candidate [`ProjectConfig`] from [`MutationGuard::stage`]. Lock-only
/// commits clear `manifest_changed` via [`StagedMutation::lock_only`], leaving
/// `ocx.toml` byte-identical.
#[non_exhaustive]
pub struct StagedMutation {
    candidate: ProjectConfig,
    manifest_changed: bool,
}

/// The files [`MutationGuard::commit`] wrote.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct MutationCommit {
    pub config_path: PathBuf,
    pub lock_path: PathBuf,
}

impl MutationGuard {
    /// The `ocx.toml` snapshot taken at acquisition, unchanged until commit.
    pub fn config(&self) -> &ProjectConfig {
        &self.manifest.config
    }

    /// The predecessor `ocx.lock`; `None` when none exists, so the caller runs
    /// a full [`crate::resolve_lock`] instead of [`crate::resolve_lock_touched`].
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

    /// `$OCX_HOME`, for the commit's GC-ledger registration.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Apply an in-memory mutation to a clone of the snapshot. The closure must
    /// not touch the filesystem; its `Err` leaves the guard valid.
    ///
    /// # Errors
    ///
    /// Whatever the closure returns.
    pub fn stage<F>(&self, mutate: F) -> Result<StagedMutation, Error>
    where
        F: FnOnce(&mut ProjectConfig) -> Result<(), Error>,
    {
        let mut candidate = self.manifest.config.clone();
        mutate(&mut candidate)?;
        Ok(StagedMutation {
            candidate,
            manifest_changed: true,
        })
    }

    /// Commit `staged` plus `new_lock`, lock-first: a failed lock write leaves
    /// `ocx.toml` untouched, a failed manifest write rolls the lock back.
    ///
    /// # Errors
    ///
    /// [`ProjectErrorKind::LockOutOfSync`] when `new_lock` does not match the
    /// candidate; [`ProjectLock::save`]'s error; or
    /// [`ProjectErrorKind::ManifestEditParse`] / [`ProjectErrorKind::ManifestEditDiverged`]
    /// after rollback. A rollback failure logs at ERROR and never masks it.
    pub async fn commit(self, staged: StagedMutation, new_lock: ProjectLock) -> Result<MutationCommit, Error> {
        // Refuse a lock whose hash differs from the candidate: the half-committed shape.
        let candidate_hash = staged.candidate.declaration_hash_cached();
        if new_lock.metadata.declaration_hash != candidate_hash {
            return Err(ProjectError::new(
                self.config_path.clone(),
                ProjectErrorKind::LockOutOfSync {
                    drift: Box::new(crate::lock::LockDrift::DeclarationHash {
                        previous_hash: new_lock.metadata.declaration_hash.clone(),
                        current_hash: candidate_hash.to_string(),
                    }),
                },
            )
            .into());
        }

        // Read once, so production makes no further env probes.
        let fault = read_fault_hook();

        maybe_inject_fault(fault.as_deref(), CommitStage::BeforeLockRename).await?;

        // Lock first: a failure here leaves `ocx.toml` untouched, so no rollback.
        new_lock
            .save(
                &self.lock_path,
                self.previous_lock.as_ref(),
                &self.home,
                &self.config_path,
            )
            .await?;

        // The new lock is on disk: every error below must roll it back, or the
        // lock advances past the manifest and the project wedges on a stale pair.
        let post_rename: Result<(), Error> = async {
            maybe_inject_fault(fault.as_deref(), CommitStage::AfterLockWrite).await?;

            // Pause point for tests that SIGKILL between lock rename and manifest publish.
            maybe_inject_fault(fault.as_deref(), CommitStage::PauseBeforeManifestWrite).await?;

            // Lock-only commits leave `ocx.toml` byte-identical.
            if staged.manifest_changed {
                let serialized =
                    super::document::render_preserving(&self.manifest.text, &staged.candidate, &self.config_path)?;
                super::mutate::publish_by_rename_async(&self.config_path, serialized.into_bytes()).await?;
            }
            Ok(())
        }
        .await;

        if let Err(primary) = post_rename {
            rollback_lock_after_failure(&self.lock_path, self.previous_lock_bytes.as_deref()).await;
            return Err(primary);
        }

        // Best-effort: a failure warns and never aborts the commit.
        super::registry::register_project_dir_best_effort(&self.config_path, &self.home).await;

        // Released only after both files have landed.
        drop(self.mutate_lock);
        Ok(MutationCommit {
            config_path: self.config_path,
            lock_path: self.lock_path,
        })
    }

    /// Drop the guard without writing; releases the lock, both files untouched.
    pub fn rollback(self) {
        // `self`'s drop releases the lock.
    }
}

impl StagedMutation {
    /// The candidate config, for the resolver before [`MutationGuard::commit`].
    pub fn config(&self) -> &ProjectConfig {
        &self.candidate
    }

    /// Whether commit rewrites `ocx.toml`; `false` for lock-only commands.
    pub fn manifest_changed(&self) -> bool {
        self.manifest_changed
    }

    /// Mark lock-only: commit rewrites `ocx.lock` and leaves `ocx.toml` untouched.
    #[must_use]
    pub fn lock_only(mut self) -> Self {
        self.manifest_changed = false;
        self
    }
}

impl MutationGuard {
    /// Assemble a guard from validated parts; nothing is re-validated. Callers
    /// must hold [`crate::acquire_project_lock`]'s lock, or a second writer races
    /// the commit; `previous_lock_bytes` must be `Some` exactly when `previous_lock` is.
    pub fn from_parts(
        mutate_lock: LockedFile,
        config_path: PathBuf,
        lock_path: PathBuf,
        home: PathBuf,
        manifest: ManifestSnapshot,
        previous_lock: Option<ProjectLock>,
        previous_lock_bytes: Option<Vec<u8>>,
    ) -> Self {
        Self {
            mutate_lock,
            config_path,
            lock_path,
            home,
            manifest,
            previous_lock,
            previous_lock_bytes,
        }
    }
}

/// Fault-injection points, keyed by `__OCX_TESTING_FAULT` value.
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

/// `__OCX_TESTING_FAULT`, with an empty value treated as unset.
#[cfg(any(test, feature = "__testing"))]
fn read_fault_hook() -> Option<String> {
    ocx_env::__OCX_TESTING_FAULT
        .get_os()
        .map(|raw| raw.to_string_lossy().into_owned())
}

/// A release build has no fault seam: the variable is not even declared there.
#[cfg(not(any(test, feature = "__testing")))]
fn read_fault_hook() -> Option<String> {
    None
}

/// `__OCX_TESTING_FAULT_RELEASE_FILE`, the file whose appearance ends the pause stage.
#[cfg(any(test, feature = "__testing"))]
fn fault_release_file() -> Option<std::ffi::OsString> {
    ocx_env::__OCX_TESTING_FAULT_RELEASE_FILE.get_raw()
}

#[cfg(not(any(test, feature = "__testing")))]
fn fault_release_file() -> Option<std::ffi::OsString> {
    None
}

/// Inject `stage`'s fault if `fault` names it: an I/O error, or for the pause
/// stage a wait until `__OCX_TESTING_FAULT_RELEASE_FILE` exists.
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
                "__OCX_TESTING_FAULT={fault} (test-only hook)"
            ))),
        )
        .into()),
        CommitStage::PauseBeforeManifestWrite => {
            // Without a release path this waits forever; the test must arrange one.
            let release = fault_release_file();
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

/// Restore the predecessor `ocx.lock` verbatim, or delete a fresh one, after a
/// failed commit, or the staleness gate refuses every read. Rollback failures
/// log at ERROR and never replace the original error.
async fn rollback_lock_after_failure(lock_path: &Path, previous_lock_bytes: Option<&[u8]>) {
    match previous_lock_bytes {
        Some(bytes) => {
            if let Err(e) = super::lock::restore_lock_bytes_verbatim(lock_path, bytes.to_vec()).await {
                log::error!(
                    "MutationGuard rollback: failed to restore predecessor ocx.lock at '{}': {e:#}",
                    lock_path.display()
                );
            }
        }
        None => {
            // Created by this commit. NotFound is benign: the forward write failed first.
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
