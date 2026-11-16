// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The project mutation lock, keyed by `ocx.toml` but never held on it: the
//! rename publish rotates its inode, so a lock on the file strands on the orphan.

use std::path::Path;

use ocx_util::fs::LockedFile;

use super::Error;
use super::error::{ProjectError, ProjectErrorKind};

const LOCK_SCOPE: &str = "project-mutate";

/// Wait before a contended acquire reports `Locked`: a forked child holds the
/// `flock` until it execs, so failing on first refusal turns that into `TempFail`.
const CONTENTION_BUDGET: std::time::Duration = std::time::Duration::from_millis(500);

/// [`acquire_project_lock_for_file`] for `<project_root>/ocx.toml`, with its errors.
pub async fn acquire_project_lock(project_root: &Path, locks_root: &Path) -> Result<LockedFile, Error> {
    acquire_project_lock_for_file(&project_root.join("ocx.toml"), locks_root).await
}

/// Acquire the exclusive mutation lock for `config_path` under `locks_root`,
/// creating the parent directory; `config_path` itself is never opened. A
/// symlink is keyed by the file it resolves to.
///
/// # Errors
///
/// - [`ProjectErrorKind::Locked`] — still held after [`CONTENTION_BUDGET`].
/// - [`ProjectErrorKind::Io`] — lock I/O failed, or `config_path` is a symlink that
///   does not resolve to an existing regular file.
pub async fn acquire_project_lock_for_file(config_path: &Path, locks_root: &Path) -> Result<LockedFile, Error> {
    let io = |path: &Path, error: std::io::Error| Error::from(ProjectError::new(path, ProjectErrorKind::Io(error)));

    // Keyed on the link's target, so every path to one file shares a lock.
    // Before any directory or lock file is created: a refused path leaves no trace.
    let keyed = {
        let path = config_path.to_path_buf();
        tokio::task::spawn_blocking(move || ocx_util::fs::resolve_user_file(&path))
            .await
            .map_err(|join| io(config_path, std::io::Error::other(join)))?
            .map_err(|error| io(config_path, error))?
    };

    let parent = keyed
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // `lock_scoped` keys off the directory's (device, inode), so it must exist.
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| io(parent, error))?;
    let file_name = keyed
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    ocx_util::fs::lock_scoped(locks_root, LOCK_SCOPE, parent, &file_name, CONTENTION_BUDGET)
        .await
        .map_err(|error| {
            if error.cause.kind() == std::io::ErrorKind::TimedOut {
                return Error::from(ProjectError::new(config_path, ProjectErrorKind::Locked));
            }
            io(&error.path, error.cause)
        })
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

    /// A symlink at `ocx.toml` is followed for the lock key: acquiring through
    /// the link and through its target contend for one lock.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_symlink_at_ocx_toml_shares_its_targets_lock() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let target = dir.path().join("real.toml");
        std::fs::write(&target, "[tools]\n").unwrap();
        let link = dir.path().join("ocx.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let _held = acquire_project_lock_for_file(&link, &locks_root)
            .await
            .expect("a symlinked ocx.toml must be lockable");
        let err = acquire_project_lock_for_file(&target, &locks_root)
            .await
            .expect_err("the target shares the link's lock");

        assert!(matches!(&err, Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::Locked)));
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "[tools]\n");
    }

    /// Two different links to one `ocx.toml` contend for the same lock.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn two_symlinks_to_one_ocx_toml_contend() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let target = dir.path().join("real.toml");
        std::fs::write(&target, "[tools]\n").unwrap();
        let first = dir.path().join("a");
        let second = dir.path().join("b");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::os::unix::fs::symlink(&target, first.join("ocx.toml")).unwrap();
        std::os::unix::fs::symlink(&target, second.join("ocx.toml")).unwrap();

        let _held = acquire_project_lock_for_file(&first.join("ocx.toml"), &locks_root)
            .await
            .expect("first acquire must succeed");
        let err = acquire_project_lock_for_file(&second.join("ocx.toml"), &locks_root)
            .await
            .expect_err("a second link to the same file must contend");

        assert!(matches!(&err, Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::Locked)));
    }

    /// A dangling link at `ocx.toml` fails the acquire as `Io` before any lock
    /// file is created, and the link is left alone.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dangling_symlink_at_ocx_toml_fails_before_any_lock_is_taken() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing.toml");
        let link = dir.path().join("ocx.toml");
        std::os::unix::fs::symlink(&missing, &link).unwrap();

        let err = acquire_project_lock_for_file(&link, &locks_root)
            .await
            .expect_err("a dangling ocx.toml link must be refused");

        assert!(matches!(&err, Error::Project(pe) if matches!(pe.kind, ProjectErrorKind::Io(_))));
        assert!(
            lock_files_under(&locks_root).is_empty(),
            "no lock file may be created for a refused path; found {:?}",
            lock_files_under(&locks_root)
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), missing);
        assert!(!missing.exists());
    }

    /// A regular `ocx.toml` keys its lock on its own directory and name, so
    /// holding it blocks a direct `lock_scoped` on that same key.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_regular_ocx_toml_keys_the_lock_on_its_own_directory_and_name() {
        let home = tempdir().unwrap();
        let locks_root = home.path().join("locks");
        let dir = tempdir().unwrap();
        let config_path = write_ocx_toml(dir.path());

        let _held = acquire_project_lock_for_file(&config_path, &locks_root)
            .await
            .expect("acquire must succeed");

        let err = ocx_util::fs::lock_scoped(
            &locks_root,
            LOCK_SCOPE,
            dir.path(),
            "ocx.toml",
            std::time::Duration::from_millis(50),
        )
        .await
        .expect_err("the key must be the directory plus the file name");
        assert_eq!(err.cause.kind(), std::io::ErrorKind::TimedOut);
    }
}
