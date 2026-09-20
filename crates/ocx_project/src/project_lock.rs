// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Acquires the project mutation lock, keyed by `ocx.toml` but never held
//! *on* it.
//!
//! `ocx.toml` is published by atomic rename ([`super::mutate`]'s
//! `atomic_write`), so its inode rotates on every mutation and a lock taken on
//! the data file would strand on the inode the rename orphaned. Per the
//! arch-principles Locking Policy the mutex is therefore an
//! [`ocx_util::fs::lock_scoped`] entry under `$OCX_HOME/locks`, keyed by the
//! config file's parent directory identity and its file name — never a
//! `ocx.toml.lock` sidecar, which would be litter in a VCS-tracked project
//! root. `ocx_config::edit` is the same shape for `config.toml`.
//!
//! Readers (`ProjectLock::load`, `ProjectLock::from_path`,
//! `ProjectConfig::from_path`) never take a lock — concurrent reads are always
//! allowed, and rename-publish is what makes them safe: a reader holding an
//! open descriptor keeps reading the document it opened rather than having a
//! writer truncate it underneath.
//!
//! `init_project` does NOT call this function — it publishes a file that does
//! not exist yet, and refuses outright when one does.

use std::path::Path;

use ocx_util::fs::LockedFile;

use super::Error;
use super::error::{ProjectError, ProjectErrorKind};

/// The `scope` every project-mutation lock is taken under.
const LOCK_SCOPE: &str = "project-mutate";

/// How long a contended acquire keeps waiting before reporting
/// [`ProjectErrorKind::Locked`].
///
/// A contended `flock` does not always mean a live writer. `flock` is held by
/// the *open file description*, and `fork` duplicates every descriptor into
/// the child; `O_CLOEXEC` only drops it at `execve`. So any process that
/// spawns a subprocess while this lock is held keeps the lock alive in the
/// child until that child execs, even after the guard is dropped here. The
/// same holds for a concurrent writer that is milliseconds from releasing.
/// Reporting `Locked` on the first refusal turns both into a hard
/// `ExitCode::TempFail` for the user.
///
/// The budget is sized against the measured fork→exec window on this
/// codebase's own suite (32-way parallelism: p50 2.8 ms, max 27.9 ms), with
/// well over an order of magnitude of headroom. A writer that genuinely holds
/// the lock for longer still surfaces `Locked` — the budget smooths transient
/// contention, it does not wait out a real one.
const CONTENTION_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// Acquire the exclusive mutation lock for `<project_root>/ocx.toml`.
///
/// Convenience wrapper around [`acquire_project_lock_for_file`] for the
/// canonical `ocx.toml` case. Use [`acquire_project_lock_for_file`]
/// directly when the project config has a custom filename
/// (e.g. `--project=custom.toml`).
///
/// # Errors
///
/// See [`acquire_project_lock_for_file`].
pub async fn acquire_project_lock(project_root: &Path, locks_root: &Path) -> Result<LockedFile, Error> {
    acquire_project_lock_for_file(&project_root.join("ocx.toml"), locks_root).await
}

/// Acquire the exclusive mutation lock for the project config file at
/// `config_path`, under the machine-global `locks_root` (`$OCX_HOME/locks`).
///
/// The lock file is the [`ocx_util::fs::lock_scoped`] entry for
/// `(parent directory identity, "project-mutate", file name)`. `config_path`
/// itself is neither created nor opened: an absent config file is the
/// create case for the writers, which publish by rename.
///
/// The config file's parent directory is created when absent — `lock_scoped`
/// keys off that directory's filesystem identity, and `set_activate` writes
/// `$OCX_HOME/ocx.toml` on machines where `$OCX_HOME` has never existed.
///
/// A symlink at `config_path` is refused ([`refuse_symlink_at`]) before the
/// guard is handed back, so the read every caller performs next cannot be
/// redirected at an attacker-chosen file.
///
/// The returned guard holds the exclusive lock until it is dropped. All
/// blocking work runs on a `spawn_blocking` thread so the async runtime is not
/// stalled.
///
/// # Errors
///
/// - [`ProjectErrorKind::Locked`] — another writer still held the lock after
///   [`CONTENTION_BUDGET`] of waiting.
/// - [`ProjectErrorKind::Io`] — the lock file could not be created or locked,
///   the parent directory could not be created, or `config_path` is a symlink.
pub async fn acquire_project_lock_for_file(config_path: &Path, locks_root: &Path) -> Result<LockedFile, Error> {
    let io = |path: &Path, error: std::io::Error| Error::Project(ProjectError::new(path, ProjectErrorKind::Io(error)));

    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // `lock_scoped` keys off the guarded directory's (device, inode), so the
    // directory has to exist before it can be identified.
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| io(parent, error))?;
    let file_name = config_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    let guard = ocx_util::fs::lock_scoped(locks_root, LOCK_SCOPE, parent, &file_name, CONTENTION_BUDGET)
        .await
        .map_err(|error| {
            // A timeout is contention — the caller may retry. Anything else is
            // the lock file's own I/O failure.
            if error.cause.kind() == std::io::ErrorKind::TimedOut {
                return Error::Project(ProjectError::new(config_path, ProjectErrorKind::Locked));
            }
            io(&error.path, error.cause)
        })?;

    // Once, under the lock, before the caller reads: the writers replace
    // `config_path` by rename, so a planted symlink is overwritten rather than
    // written through (CWE-59), but an unrefused one would still redirect the
    // read that every caller performs next.
    refuse_symlink_at(config_path).await?;

    Ok(guard)
}

/// Refuse a symlink at `config_path` before the caller reads it.
///
/// `O_NOFOLLOW` discipline for the data file, applied by hand: the read is an
/// ordinary bounded read and the publish is a rename, so neither carries an
/// open flag that would express this. `ocx.toml` is the canonical project
/// declaration the invoking user owns; a symlink there is a misconfiguration
/// at best and, at worst, a redirect that hands the mutator an attacker-chosen
/// document to edit and re-publish.
///
/// Runs once, after the lock is held — the lock no longer polls `config_path`,
/// so there is no reopen loop for a racing symlink to be planted into. A
/// window survives between this check and the caller's read, one scheduling
/// gap wide, and closing it would need `O_NOFOLLOW` on that read itself.
///
/// # Errors
///
/// - [`ProjectErrorKind::Io`] — the path is a symlink, or its metadata could
///   not be read. `NotFound` is not an error: an absent config file is the
///   create case for `set_activate` and for the bootstrapping mutators.
async fn refuse_symlink_at(config_path: &Path) -> Result<(), Error> {
    let refusal = |io_error| Error::Project(ProjectError::new(config_path, ProjectErrorKind::Io(io_error)));
    match tokio::fs::symlink_metadata(config_path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(refusal(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ocx.toml path is a symlink",
        ))),
        Ok(_) => Ok(()),
        // NotFound is fine — the writers create the file by publishing it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        // Any other metadata error is an I/O failure on the config path.
        Err(error) => Err(refusal(error)),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use super::*;

    /// Helper: create a minimal ocx.toml at `dir/ocx.toml`.
    fn write_ocx_toml(dir: &Path) -> PathBuf {
        let path = dir.join("ocx.toml");
        std::fs::write(&path, "[tools]\n").expect("write ocx.toml");
        path
    }

    /// Every `.lock` file reachable under `dir`, recursively.
    fn lock_files_under(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(lock_files_under(&path));
            } else if path.extension().is_some_and(|extension| extension == "lock") {
                found.push(path);
            }
        }
        found
    }

    /// The lock never lands beside the data: no `.lock` file anywhere in the
    /// project directory, and `ocx.toml` itself byte-identical afterwards.
    ///
    /// Same contract as when the lock was taken in place on `ocx.toml` — a
    /// `ocx.toml.lock` in a VCS-tracked project root is litter either way.
    /// What changed is how it is satisfied: the lock file is a content-keyed
    /// entry under `$OCX_HOME/locks`, which this also asserts, because "no
    /// sidecar" is vacuously true of a lock that was never taken at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn acquire_project_lock_leaves_no_sidecar() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        let guard = acquire_project_lock_for_file(&config_path, &locks_root)
            .await
            .expect("first lock acquisition must succeed");

        assert!(
            lock_files_under(dir.path()).is_empty(),
            "the project mutation lock must not write a .lock file into the project directory; found {:?}",
            lock_files_under(dir.path())
        );
        assert!(
            guard.path().starts_with(&locks_root),
            "the lock file must live under the locks root; got {}",
            guard.path().display()
        );

        drop(guard);

        // ocx.toml content is unmodified by the lock acquisition.
        let toml_content = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(
            toml_content, "[tools]\n",
            "ocx.toml must be unmodified by lock acquisition"
        );
    }

    /// Two acquires for the same config file; the second reports
    /// [`ProjectErrorKind::Locked`] once the contention budget is spent.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_mutation_contention_blocks_second_writer() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        let guard = acquire_project_lock_for_file(&config_path, &locks_root)
            .await
            .expect("first exclusive lock must succeed");

        let err = acquire_project_lock_for_file(&config_path, &locks_root)
            .await
            .expect_err("second lock attempt must fail while first holds");

        assert!(
            matches!(&err, Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::Locked)),
            "expected ProjectErrorKind::Locked on contention; got: {err}"
        );

        drop(guard);

        // ocx.toml is untouched by the lock machinery.
        let toml_content = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(toml_content, "[tools]\n", "ocx.toml must be unmodified");
    }

    /// Two projects contend independently: a lock held on one project's
    /// `ocx.toml` must not refuse a mutation of another's.
    ///
    /// The key is the *guarded directory's* identity, so this is what proves
    /// the discriminator is wired at all — a lock keyed on the locks root
    /// alone would serialise every project on the machine and still pass every
    /// other test here.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_projects_do_not_contend_with_each_other() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let first = tempdir().unwrap();
        let second = tempdir().unwrap();

        let _held = acquire_project_lock_for_file(&write_ocx_toml(first.path()), &locks_root)
            .await
            .expect("the first project's lock must be acquirable");

        acquire_project_lock_for_file(&write_ocx_toml(second.path()), &locks_root)
            .await
            .expect("a second project's lock must not contend with the first's");
    }

    /// Regression: a lock released while the acquirer is waiting must be
    /// picked up, not reported as `Locked`.
    ///
    /// Transient holds are not hypothetical: `flock` lives on the open file
    /// description, `fork` duplicates it into the child, and `O_CLOEXEC` only
    /// drops it at `execve` — so a subprocess spawned anywhere in the process
    /// keeps this lock alive for the child's fork→exec window even after the
    /// guard here is dropped. A 60 ms hold stands in for that window; it
    /// exceeds the acquire's own 25 ms poll tick, so the acquire must
    /// genuinely wait rather than win on the first try.
    #[tokio::test(flavor = "multi_thread")]
    async fn acquire_waits_out_a_transient_holder() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        let guard = acquire_project_lock_for_file(&config_path, &locks_root)
            .await
            .expect("first lock acquisition must succeed");
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            drop(guard);
        });

        acquire_project_lock_for_file(&config_path, &locks_root)
            .await
            .expect("a lock released mid-wait must be acquired, not reported as Locked");
    }

    /// A symlink at `ocx.toml` is refused rather than followed.
    ///
    /// The publish is a rename, so a planted symlink is *replaced* rather than
    /// written through — the CWE-59 truncation this once guarded is closed by
    /// construction. What the refusal still buys is the read: every caller
    /// reads `ocx.toml` immediately after this acquire, and an unrefused
    /// symlink would hand the mutator an attacker-chosen document to edit and
    /// then publish back under the project's own name.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_symlink_at_ocx_toml_is_refused() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("ocx.toml");
        let victim_path = dir.path().join("victim.toml");
        std::fs::write(&victim_path, "[victim]\n").expect("write the symlink target");
        std::os::unix::fs::symlink(&victim_path, &config_path).expect("plant the symlink");

        let err = acquire_project_lock_for_file(&config_path, &locks_root)
            .await
            .expect_err("a symlink at ocx.toml must be refused, not followed");

        assert!(
            matches!(&err, Error::Project(project_error)
                if matches!(&project_error.kind, ProjectErrorKind::Io(io_error)
                    if io_error.to_string().contains("symlink"))),
            "expected the symlink refusal; got: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&victim_path).unwrap(),
            "[victim]\n",
            "the symlink target must be untouched"
        );
    }
}
