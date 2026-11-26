// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publication primitives for directories built complete in a staging directory: one rename or one link switch,
//! never an edit of what is already published.

use std::path::{Path, PathBuf};

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
pub async fn publish_dir(staged: &Path, target: &Path) -> std::io::Result<Published> {
    #[cfg(unix)]
    {
        match super::rename_with_windows_retry(staged, target).await {
            Ok(()) => Ok(Published::New),
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTEMPTY | libc::EEXIST)) => {
                Ok(Published::AlreadyThere)
            }
            Err(error) => Err(error),
        }
    }
    #[cfg(windows)]
    {
        // ERROR_FILE_EXISTS / ERROR_ALREADY_EXISTS: without MOVEFILE_REPLACE_EXISTING an occupied target is refused.
        const OCCUPIED: [i32; 2] = [80, 183];
        let (staged, target) = (staged.to_path_buf(), target.to_path_buf());
        let result = super::retry_windows_transient(|| {
            let (staged, target) = (staged.clone(), target.clone());
            async move {
                tokio::task::spawn_blocking(move || move_without_replace(&staged, &target))
                    .await
                    .map_err(std::io::Error::other)?
            }
        })
        .await;
        match result {
            Ok(()) => Ok(Published::New),
            Err(error) if error.raw_os_error().is_some_and(|code| OCCUPIED.contains(&code)) => {
                Ok(Published::AlreadyThere)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(windows)]
fn move_without_replace(staged: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (source, destination) = (wide(staged), wide(target));
    // SAFETY: both buffers are NUL-terminated and outlive the call; flags 0 never replaces an existing target.
    let moved = unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 0) };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
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
/// The caller holds the lock that excludes stagers in `dir`'s parent: a `.staging-*` sibling of a live stager is
/// swept as debris, and a symlink debris entry is removed as a link, never followed. An absent `dir` is `Removed`.
///
/// # Errors
///
/// The directory cannot be inspected or removed for a reason other than being busy or non-empty.
pub async fn retire_dir(dir: &Path) -> std::io::Result<Retired> {
    let outcome = match tokio::fs::symlink_metadata(dir).await {
        // Already absent: the name is gone, which is what the caller asked for.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Retired::Removed,
        Err(error) => return Err(error),
        Ok(metadata) if !is_real_directory(&metadata) => Retired::LeftForLater,
        Ok(_) => retire_real_directory(dir).await?,
    };
    if let Some(parent) = dir.parent() {
        sweep_debris(parent).await;
    }
    Ok(outcome)
}

fn is_real_directory(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_dir()
    }
}

async fn retire_real_directory(dir: &Path) -> std::io::Result<Retired> {
    let Some(parent) = dir.parent() else {
        return Ok(Retired::LeftForLater);
    };
    let graveyard = loop {
        let candidate = parent.join(format!(".gc-{}", random_suffix()));
        if tokio::fs::symlink_metadata(&candidate).await.is_err() {
            break candidate;
        }
    };
    match super::rename_with_windows_retry(dir, &graveyard).await {
        Ok(()) => {}
        // overlayfs refuses directory renames across layers: remove in place.
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            return match tokio::fs::remove_dir_all(dir).await {
                Ok(()) => Ok(Retired::Removed),
                Err(error) if is_busy_or_not_empty(&error) => Ok(Retired::LeftForLater),
                Err(error) => Err(error),
            };
        }
        Err(error) if is_refused(&error) => return Ok(Retired::LeftForLater),
        Err(error) => return Err(error),
    }
    // The name is gone after the rename; a failed removal is debris for the next sweep.
    if let Err(error) = tokio::fs::remove_dir_all(&graveyard).await {
        tracing::debug!(path = %graveyard.display(), %error, "retired directory not fully removed; left for the next sweep");
    }
    Ok(Retired::Removed)
}

fn is_refused(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::PermissionDenied || is_busy_or_not_empty(error)
}

fn is_busy_or_not_empty(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(libc::EBUSY | libc::ENOTEMPTY | libc::EPERM | libc::EACCES)
        )
    }
    #[cfg(windows)]
    {
        // ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION, ERROR_DIR_NOT_EMPTY
        matches!(error.raw_os_error(), Some(5 | 32 | 145))
    }
}

fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u64(SEQUENCE.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("{:016x}", hasher.finish())
}

/// Removes `.gc-*` and `.staging-*` siblings; failures wait for the next call.
async fn sweep_debris(parent: &Path) {
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with(".gc-") || name.starts_with(".staging-")) {
            continue;
        }
        let path: PathBuf = entry.path();
        let removal = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) if is_real_directory(&metadata) => tokio::fs::remove_dir_all(&path).await,
            // A symlink, reparse point or file goes as itself, never followed.
            Ok(_) => remove_link_or_file(&path).await,
            Err(error) => Err(error),
        };
        if let Err(error) = removal {
            tracing::debug!(path = %path.display(), %error, "debris not removed; left for the next sweep");
        }
    }
}

async fn remove_link_or_file(path: &Path) -> std::io::Result<()> {
    let removed = tokio::fs::remove_file(path).await;
    // A Windows directory link or junction needs `remove_dir`, which never follows it.
    #[cfg(windows)]
    if removed.is_err() {
        return tokio::fs::remove_dir(path).await;
    }
    removed
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

    #[tokio::test]
    async fn publish_two_concurrent_stagers_converge_on_one_whole_target() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let mut staged = Vec::new();
        for name in ["a", "b"] {
            let dir = tmp.path().join(format!("staged-{name}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("NAME"), name).unwrap();
            std::fs::write(dir.join("EXTRA"), name).unwrap();
            staged.push(dir);
        }

        let (first, second) = tokio::join!(publish_dir(&staged[0], &target), publish_dir(&staged[1], &target));
        let outcomes = [first.unwrap(), second.unwrap()];

        assert_eq!(outcomes.iter().filter(|o| **o == Published::New).count(), 1);
        assert_eq!(outcomes.iter().filter(|o| **o == Published::AlreadyThere).count(), 1);
        let winner = std::fs::read_to_string(target.join("NAME")).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("EXTRA")).unwrap(),
            winner,
            "one staged set, whole"
        );
    }

    #[tokio::test]
    async fn publish_failure_leaves_target_absent_and_retry_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let missing = tmp.path().join("missing");

        assert!(publish_dir(&missing, &target).await.is_err());
        assert!(!target.exists(), "no partial published directory");

        let staged = staged_with_file(tmp.path());
        assert_eq!(publish_dir(&staged, &target).await.unwrap(), Published::New);
        assert!(target.join("NAME").is_file());
    }

    #[tokio::test]
    async fn retire_leaves_dir_whole_when_rename_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        // SAFETY: geteuid has no preconditions and cannot fail.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("skipped: euid 0 ignores directory permissions");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let dir = staged_with_file(&parent);
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o555)).unwrap();

        let outcome = retire_dir(&dir).await;

        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(outcome.unwrap(), Retired::LeftForLater);
        assert!(dir.join("NAME").is_file(), "dir stays whole");
    }

    #[tokio::test]
    async fn retire_sweep_removes_a_gc_symlink_as_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = staged_with_file(tmp.path());
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("f"), b"z").unwrap();
        let link = tmp.path().join(".gc-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        retire_dir(&dir).await.unwrap();

        assert!(std::fs::symlink_metadata(&link).is_err(), "link removed");
        assert!(outside.join("f").is_file(), "link target untouched");
    }

    #[tokio::test]
    async fn retire_absent_dir_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(retire_dir(&tmp.path().join("gone")).await.unwrap(), Retired::Removed);
    }
}
