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
/// creating the parent directory; `config_path` itself is never opened.
///
/// # Errors
///
/// - [`ProjectErrorKind::Locked`] — still held after [`CONTENTION_BUDGET`].
/// - [`ProjectErrorKind::Io`] — lock I/O failed, or `config_path` is a symlink.
pub async fn acquire_project_lock_for_file(config_path: &Path, locks_root: &Path) -> Result<LockedFile, Error> {
    let io = |path: &Path, error: std::io::Error| Error::Project(ProjectError::new(path, ProjectErrorKind::Io(error)));

    let parent = config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // `lock_scoped` keys off the directory's (device, inode), so it must exist.
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
            if error.cause.kind() == std::io::ErrorKind::TimedOut {
                return Error::Project(ProjectError::new(config_path, ProjectErrorKind::Locked));
            }
            io(&error.path, error.cause)
        })?;

    // Under the lock, before the caller reads: an unrefused symlink redirects that read.
    refuse_symlink_at(config_path).await?;

    Ok(guard)
}

/// Refuse a symlink at `config_path`: it hands the mutator an attacker-chosen
/// document to edit and re-publish. A one-scheduling-gap window survives until
/// the caller's read; closing it needs `O_NOFOLLOW` on that read.
async fn refuse_symlink_at(config_path: &Path) -> Result<(), Error> {
    let refusal = |io_error| Error::Project(ProjectError::new(config_path, ProjectErrorKind::Io(io_error)));
    match tokio::fs::symlink_metadata(config_path).await {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(refusal(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ocx.toml path is a symlink",
        ))),
        Ok(_) => Ok(()),
        // NotFound is the create case: the writers publish the file.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
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
