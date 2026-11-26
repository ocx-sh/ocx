// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publication primitives for directories built complete in a staging directory: one rename or one link switch,
//! never an edit of what is already published.

use std::path::Path;

/// What [`publish_dir`] found at the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    /// The staged directory is now the target.
    New,
    /// Another directory already holds the target; the staged one is untouched.
    AlreadyThere,
}

/// What [`retire_dir`] did with a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retired {
    /// The directory is gone.
    Removed,
    /// The directory was not removed and is whole; a later call may retire it.
    ///
    /// Also the answer for a path that exists but is not a real directory: it is never acted on.
    LeftForLater,
}

/// `rename(2)` of `staged` onto `target`.
///
/// Unix replaces an absent or empty target and maps `ENOTEMPTY` and `EEXIST` to [`Published::AlreadyThere`].
/// Windows publishes onto an absent target only; an existing one, empty or not, is `AlreadyThere`.
/// `staged` stays for the caller to discard.
///
/// # Errors
///
/// The rename fails for a reason other than an occupied target.
pub async fn publish_dir(_staged: &Path, _target: &Path) -> std::io::Result<Published> {
    unimplemented!("publish_dir")
}

/// Points `link` at `target`.
///
/// Unix stages a sibling link by exclusive create and renames it over `link`. Windows sets the junction's
/// mount-point data in place. An absent `link` is created.
///
/// # Errors
///
/// `link` exists and is neither a symlink nor a junction, or the switch fails.
pub fn retarget_link(_target: &Path, _link: &Path) -> std::io::Result<()> {
    unimplemented!("retarget_link")
}

/// Acts only on a real directory, never a symlink or reparse point: renames it to a sibling `.gc-<random>`,
/// then removes that. A refused rename leaves `dir` whole. Also sweeps sibling `.gc-*` and `.staging-*` debris.
///
/// # Errors
///
/// The directory cannot be inspected or removed for a reason other than being busy or non-empty.
pub async fn retire_dir(_dir: &Path) -> std::io::Result<Retired> {
    unimplemented!("retire_dir")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn staged_with_file(root: &Path) -> std::path::PathBuf {
        let staged = root.join("staged");
        std::fs::create_dir(&staged).unwrap();
        std::fs::write(staged.join("NAME"), b"x").unwrap();
        staged
    }

    #[tokio::test]
    async fn contract_publish_onto_absent_target_is_new() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = staged_with_file(tmp.path());
        let target = tmp.path().join("target");

        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::New);
        assert!(target.join("NAME").is_file());
        assert!(!staged.exists());
    }

    #[tokio::test]
    async fn contract_publish_onto_empty_dir_is_new() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = staged_with_file(tmp.path());
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();

        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::New);
        assert!(target.join("NAME").is_file());
    }

    #[tokio::test]
    async fn contract_publish_onto_non_empty_dir_is_already_there_and_keeps_staged() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = staged_with_file(tmp.path());
        let target = tmp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("theirs"), b"y").unwrap();

        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::AlreadyThere);
        assert!(staged.join("NAME").is_file(), "staged stays for the caller");
        assert!(target.join("theirs").is_file());
        assert!(!target.join("NAME").exists());
    }

    #[test]
    fn contract_retarget_creates_an_absent_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a");
        std::fs::create_dir(&dir).unwrap();
        let link = tmp.path().join("active");

        retarget_link(&dir, &link).unwrap();

        assert_eq!(std::fs::read_link(&link).unwrap(), dir);
    }

    #[test]
    fn contract_retarget_replaces_an_existing_link() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let link = tmp.path().join("active");
        retarget_link(&a, &link).unwrap();

        retarget_link(&b, &link).unwrap();

        assert_eq!(std::fs::read_link(&link).unwrap(), b);
        assert_eq!(
            std::fs::read_dir(tmp.path()).unwrap().count(),
            3,
            "a, b, active: no staged link left behind"
        );
    }

    #[test]
    fn contract_retarget_refuses_a_real_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a");
        let link = tmp.path().join("active");
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&link).unwrap();

        assert!(retarget_link(&target, &link).is_err());
        assert!(link.is_dir() && !link.is_symlink());
    }

    #[test]
    fn contract_retarget_refuses_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("a");
        let link = tmp.path().join("active");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(&link, b"x").unwrap();

        assert!(retarget_link(&target, &link).is_err());
        assert!(link.is_file());
    }

    #[tokio::test]
    async fn contract_retire_removes_a_real_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = staged_with_file(tmp.path());

        assert_eq!(retire_dir(&dir).await.unwrap(), Retired::Removed);
        assert!(!dir.exists());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0, "no .gc- debris");
    }

    #[tokio::test]
    async fn contract_retire_leaves_a_symlink_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let real = staged_with_file(tmp.path());
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert_eq!(retire_dir(&link).await.unwrap(), Retired::LeftForLater);
        assert!(link.is_symlink());
        assert!(real.join("NAME").is_file());
    }

    #[tokio::test]
    async fn contract_retire_sweeps_sibling_debris() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = staged_with_file(tmp.path());
        let gc = tmp.path().join(".gc-0123456789abcdef");
        let staging = tmp.path().join(".staging-0123456789abcdef");
        let bystander = tmp.path().join("keep");
        for debris in [&gc, &staging, &bystander] {
            std::fs::create_dir(debris).unwrap();
            std::fs::write(debris.join("f"), b"z").unwrap();
        }

        retire_dir(&dir).await.unwrap();

        assert!(!gc.exists(), ".gc-* swept");
        assert!(!staging.exists(), ".staging-* swept");
        assert!(bystander.join("f").is_file(), "other siblings untouched");
    }
}
