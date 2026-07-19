// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Central sharded lock directory: cross-process advisory locks keyed by the
//! *file identity* of a guarded directory rather than by a sidecar written next
//! to the guarded data.
//!
//! [`lock_scoped`] hashes `(file-identity, scope, discriminator)` into a
//! content-addressed path under a machine-global `locks/` root, so a lock file
//! never lands inside the directory it protects. That matters when the guarded
//! tree may be a user-committed or read-only shipped copy (e.g. a project's
//! `.ocx/index/`): a lock sidecar there would be in-tree litter or an outright
//! write failure. One machine-global `locks/` root (`$OCX_HOME/locks`) serves
//! every guarded location.
//!
//! **File identity, not path.** The primary key is the guarded directory's
//! `(device, inode)` on Unix / `(volume serial, file index)` on Windows, so two
//! path aliases of one directory (a symlink, a bind mount) contend on the same
//! lock. When the identity cannot be read the key falls back to the
//! canonicalized path bytes ([`dunce::canonicalize`]), logged at debug — correct
//! for the common case, weaker only across path aliases the fallback cannot see.
//!
//! **Caveats (documented, not fixed).**
//! - An NFS export double-mounted at two mount points can report *distinct*
//!   `st_dev` for the same files, splitting the key. `flock` semantics over NFS
//!   are themselves unreliable, so this split is accepted rather than worked
//!   around.
//! - Lock files are permanent — never unlinked. Unlinking on release would race
//!   a second acquirer that has already opened the same path, so the empty files
//!   are left in place; they are safe to delete when no ocx process is running.

use std::path::Path;
use std::time::Duration;

use super::LockedFile;

/// Acquire an exclusive cross-process lock scoped to `guarded_dir`'s file
/// identity, plus `scope` and `discriminator`, under the machine-global
/// `locks_root`. Blocks until the lock is acquired or `timeout` elapses.
///
/// The returned [`LockedFile`] holds the advisory lock for its lifetime; drop
/// releases it. The lock file lives at
/// `locks_root/<first-2-hex>/<remaining-hex>.lock`, where the hex is the
/// SHA-256 of the composed key — so it is never written inside `guarded_dir`.
///
/// `guarded_dir` must already exist: its `(device, inode)` identity is the
/// primary key material. A caller that may need to create the directory should
/// `create_dir_all` it before locking. If the identity cannot be read the key
/// falls back to the canonicalized path (see the module docs).
///
/// `scope` names the lock's purpose (e.g. `"index-catalog"`); `discriminator`
/// distinguishes locks that share a `guarded_dir` and `scope` (e.g. the
/// specific file being guarded). A different `scope` or `discriminator` yields
/// an independent lock on the same directory.
///
/// # Errors
///
/// Returns an error only when the lock file cannot be created or the lock
/// cannot be acquired within `timeout` (see
/// [`LockedFile::open_exclusive_with_timeout`]).
pub async fn lock_scoped(
    locks_root: &Path,
    scope: &str,
    guarded_dir: &Path,
    discriminator: &str,
    timeout: Duration,
) -> crate::Result<LockedFile> {
    let key = key_material(guarded_dir, scope, discriminator).await;
    let hex = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(key.as_bytes()));
    let lock_path = locks_root.join(&hex[..2]).join(&hex[2..]).with_added_extension("lock");
    LockedFile::open_exclusive_with_timeout(lock_path, timeout).await
}

/// Compose the SHA-256 pre-image for the lock key. The primary form binds the
/// directory's filesystem identity
/// (`{device}:{inode}:{scope}:{discriminator}`); the fallback binds its
/// canonicalized path (`path:{canonical}:{scope}:{discriminator}`) when the
/// identity is unreadable.
async fn key_material(guarded_dir: &Path, scope: &str, discriminator: &str) -> String {
    if let Some((device, inode)) = file_identity(guarded_dir).await {
        return format!("{device}:{inode}:{scope}:{discriminator}");
    }
    let canonical = canonicalize(guarded_dir)
        .await
        .unwrap_or_else(|| guarded_dir.to_path_buf());
    crate::log::debug!(
        "scoped lock for '{}' falls back to a path-keyed lock (file identity unavailable)",
        guarded_dir.display()
    );
    format!("path:{}:{scope}:{discriminator}", canonical.to_string_lossy())
}

/// Canonicalize `path` off the async runtime (the syscall is blocking).
/// `None` on any failure — the caller then keys off the raw path.
async fn canonicalize(path: &Path) -> Option<std::path::PathBuf> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || dunce::canonicalize(&path).ok())
        .await
        .ok()
        .flatten()
}

/// The `(device, inode)` identity of `dir`, or `None` if it cannot be read.
#[cfg(unix)]
async fn file_identity(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = tokio::fs::metadata(dir).await.ok()?;
    Some((metadata.dev(), metadata.ino()))
}

/// The `(volume serial, file index)` identity of `dir`, or `None` if it cannot
/// be read.
#[cfg(windows)]
async fn file_identity(dir: &Path) -> Option<(u64, u64)> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || file_identity_blocking(&dir))
        .await
        .ok()
        .flatten()
}

#[cfg(windows)]
fn file_identity_blocking(dir: &Path) -> Option<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, GetFileInformationByHandle,
    };

    // FILE_FLAG_BACKUP_SEMANTICS is required to open a *directory* handle;
    // a plain read open of a directory fails on Windows. The RAII `File`
    // closes the handle on drop.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .ok()?;

    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `file.as_raw_handle()` is a valid open handle that outlives this
    // call, and `&mut info` points to a writable, correctly-sized structure.
    // On a zero (failure) return we discard `info` untouched.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
    if ok == 0 {
        return None;
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Some((u64::from(info.dwVolumeSerialNumber), index))
}

#[cfg(not(any(unix, windows)))]
async fn file_identity(_dir: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(30);

    /// Recursively collect every `*.lock` file under `root`.
    fn lock_files(root: &Path) -> Vec<std::path::PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "lock") {
                    found.push(path);
                }
            }
        }
        found
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exclusive_lock_blocks_second_acquirer_same_key() {
        let temp = tempfile::tempdir().unwrap();
        let locks_root = temp.path().join("locks");
        let guarded = temp.path().join("guarded");
        std::fs::create_dir(&guarded).unwrap();

        let first = lock_scoped(&locks_root, "scope", &guarded, "disc", TIMEOUT)
            .await
            .unwrap();

        let locks_root_clone = locks_root.clone();
        let guarded_clone = guarded.clone();
        let second =
            tokio::spawn(async move { lock_scoped(&locks_root_clone, "scope", &guarded_clone, "disc", TIMEOUT).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !second.is_finished(),
            "a second acquirer of the same key must block behind the first"
        );

        drop(first);
        second.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn different_scope_yields_independent_locks() {
        let temp = tempfile::tempdir().unwrap();
        let locks_root = temp.path().join("locks");
        let guarded = temp.path().join("guarded");
        std::fs::create_dir(&guarded).unwrap();

        // Both must be held at once: distinct scopes are distinct keys, so the
        // second acquire never contends with the first.
        let _first = lock_scoped(&locks_root, "scope-a", &guarded, "disc", TIMEOUT)
            .await
            .unwrap();
        let _second = lock_scoped(&locks_root, "scope-b", &guarded, "disc", TIMEOUT)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn different_discriminator_yields_independent_locks() {
        let temp = tempfile::tempdir().unwrap();
        let locks_root = temp.path().join("locks");
        let guarded = temp.path().join("guarded");
        std::fs::create_dir(&guarded).unwrap();

        let _first = lock_scoped(&locks_root, "scope", &guarded, "disc-a", TIMEOUT)
            .await
            .unwrap();
        let _second = lock_scoped(&locks_root, "scope", &guarded, "disc-b", TIMEOUT)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lock_file_uses_two_char_shard_prefix_layout() {
        let temp = tempfile::tempdir().unwrap();
        let locks_root = temp.path().join("locks");
        let guarded = temp.path().join("guarded");
        std::fs::create_dir(&guarded).unwrap();

        let _guard = lock_scoped(&locks_root, "scope", &guarded, "disc", TIMEOUT)
            .await
            .unwrap();

        let locks = lock_files(&locks_root);
        assert_eq!(locks.len(), 1, "exactly one lock file must be created: {locks:?}");
        let lock = &locks[0];
        let stem = lock.file_stem().unwrap().to_str().unwrap();
        let shard = lock.parent().unwrap().file_name().unwrap().to_str().unwrap();
        assert_eq!(shard.len(), 2, "shard prefix must be two hex chars");
        assert_eq!(stem.len(), 62, "remaining hash must be 62 hex chars");
        assert!(shard.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(stem.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// A symlink alias of the guarded directory must resolve to the SAME lock
    /// file — mutual exclusion holds across path aliases because the key binds
    /// `(device, inode)`, not the path text.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn symlink_alias_contends_on_same_lock() {
        let temp = tempfile::tempdir().unwrap();
        let locks_root = temp.path().join("locks");
        let guarded = temp.path().join("guarded");
        std::fs::create_dir(&guarded).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&guarded, &alias).unwrap();

        let first = lock_scoped(&locks_root, "scope", &guarded, "disc", TIMEOUT)
            .await
            .unwrap();

        let locks_root_clone = locks_root.clone();
        let alias_clone = alias.clone();
        let via_alias =
            tokio::spawn(async move { lock_scoped(&locks_root_clone, "scope", &alias_clone, "disc", TIMEOUT).await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !via_alias.is_finished(),
            "locking via a symlink alias must contend with the lock taken via the real path"
        );

        drop(first);
        via_alias.await.unwrap().unwrap();

        // Only one lock file exists — both paths hashed to the same key.
        assert_eq!(lock_files(&locks_root).len(), 1);
    }

    /// On Unix the lock path is the SHA-256 of `{dev}:{ino}:{scope}:{disc}`,
    /// sharded — this locks the exact key grammar.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn lock_file_path_matches_sha256_of_identity_key() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let locks_root = temp.path().join("locks");
        let guarded = temp.path().join("guarded");
        std::fs::create_dir(&guarded).unwrap();

        let _guard = lock_scoped(&locks_root, "scope", &guarded, "disc", TIMEOUT)
            .await
            .unwrap();

        let metadata = std::fs::metadata(&guarded).unwrap();
        let key = format!("{}:{}:scope:disc", metadata.dev(), metadata.ino());
        let hex = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(key.as_bytes()));
        let expected = locks_root.join(&hex[..2]).join(format!("{}.lock", &hex[2..]));
        assert!(
            expected.is_file(),
            "lock file must live at the sharded SHA-256 path of the identity key: {}",
            expected.display()
        );
    }

    /// A guarded directory whose file identity cannot be read (here: a
    /// since-removed dir) drops to the path-keyed fallback in `key_material` —
    /// `path:{canonical}:{scope}:{disc}`. `dunce::canonicalize` fails on the
    /// absent path, so the raw path is used verbatim. The lock is still acquired
    /// and lands at the SHA-256 of the path pre-image, not the identity one.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn absent_guarded_dir_falls_back_to_path_keyed_lock() {
        let temp = tempfile::tempdir().unwrap();
        let locks_root = temp.path().join("locks");
        let guarded = temp.path().join("since-removed");
        std::fs::create_dir(&guarded).unwrap();
        std::fs::remove_dir(&guarded).unwrap();

        let _guard = lock_scoped(&locks_root, "scope", &guarded, "disc", TIMEOUT)
            .await
            .expect("the path-keyed fallback must still acquire the lock");

        // The canonicalize fails for the absent path, so `key_material` uses the
        // raw path — mirror that here to locate the expected lock file.
        let key = format!("path:{}:scope:disc", guarded.to_string_lossy());
        let hex = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(key.as_bytes()));
        let expected = locks_root.join(&hex[..2]).join(format!("{}.lock", &hex[2..]));
        assert!(
            expected.is_file(),
            "the path-keyed fallback lock must live at the sha256 of the path pre-image: {}",
            expected.display()
        );
        // Exactly one lock file — proof no identity-keyed path was also taken.
        assert_eq!(lock_files(&locks_root).len(), 1);
    }
}
