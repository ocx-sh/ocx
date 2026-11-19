// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The installation id: a random UUID v4 kept in one file under the state home.

use std::io::Write as _;
use std::path::Path;

use ocx_util::fs::{PersistOutcome, persist_temp_file_noclobber};

/// A hyphenated UUID is 36 bytes; this leaves room for one newline and nothing else.
const MAX_BYTES: u64 = 128;

/// What the path holds.
enum Existing {
    Absent,
    Valid(String),
    /// Present but not ours to read or replace: left untouched.
    Refused,
}

/// Reads the id at `path`, creating it when the file is absent.
///
/// `None` — the attribute is omitted and the file left untouched — for a symlink, a file that is
/// not regular, larger than 128 bytes, unreadable or not a lowercase v4 UUID (one trailing newline
/// allowed). A failure to create the file or its parent is `None` as well and never reaches the
/// command. Creation publishes with no-clobber semantics, so two first runs agree on one value.
///
/// Call it only for an armed run that has a destination: an unarmed run, or one with nowhere to
/// send, must leave no state behind.
pub(crate) async fn installation_id(path: &Path) -> Option<String> {
    let path = path.to_path_buf();
    detached(move || read_or_create(&path)).await
}

/// [`crate::detached::run`] for the id read; `None` for a panic as well as a failed read.
async fn detached<Work>(work: Work) -> Option<String>
where
    Work: FnOnce() -> Option<String> + Send + 'static,
{
    crate::detached::run("ocx-telemetry-id", work).await.ok().flatten()
}

fn read_or_create(path: &Path) -> Option<String> {
    match read_existing(path) {
        Existing::Valid(id) => Some(id),
        Existing::Refused => None,
        Existing::Absent => create(path),
    }
}

/// Refuses a link or a non-regular file, then reads from the one handle it opened.
///
/// The `symlink_metadata` look first keeps a FIFO from blocking the open; the checks that count
/// run on the handle, so a swap after the open cannot change what is read.
fn read_existing(path: &Path) -> Existing {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_BYTES => {}
        Ok(_) => return Existing::Refused,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Existing::Absent,
        Err(_) => return Existing::Refused,
    }
    match open_without_following(path) {
        Ok(file) => read_handle(file, path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Existing::Absent,
        Err(_) => Existing::Refused,
    }
}

/// Windows opens a link as the link itself, which then fails the regular-file check on the handle.
#[cfg(windows)]
fn open_without_following(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// `O_NOFOLLOW` needs a libc dependency this crate does not carry, so the open may follow a link
/// and [`names_the_handle`] refuses afterwards.
#[cfg(not(windows))]
fn open_without_following(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Whether `path` still names the file `opened` describes: a link, or a different file, means the
/// path changed under the open.
#[cfg(unix)]
fn names_the_handle(opened: &std::fs::Metadata, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    std::fs::symlink_metadata(path).is_ok_and(|named| {
        !named.file_type().is_symlink() && (named.dev(), named.ino()) == (opened.dev(), opened.ino())
    })
}

#[cfg(not(unix))]
fn names_the_handle(_opened: &std::fs::Metadata, _path: &Path) -> bool {
    true
}

/// The id read from `file`, a regular file of at most 128 bytes that `path` still names.
fn read_handle(file: std::fs::File, path: &Path) -> Existing {
    use std::io::Read as _;

    let Ok(metadata) = file.metadata() else {
        return Existing::Refused;
    };
    if !metadata.is_file() || metadata.len() > MAX_BYTES || !names_the_handle(&metadata, path) {
        return Existing::Refused;
    }
    // `take`, not the length: the file can grow between the stat and the read.
    let mut bytes = Vec::new();
    if file.take(MAX_BYTES + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > MAX_BYTES {
        return Existing::Refused;
    }
    parse(&bytes).map_or(Existing::Refused, Existing::Valid)
}

/// The id when `bytes` is a lowercase hyphenated v4 UUID with at most one trailing newline.
fn parse(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let uuid = uuid::Uuid::parse_str(text).ok()?;
    (uuid.get_version_num() == 4 && uuid.hyphenated().to_string() == text).then(|| text.to_owned())
}

fn create(path: &Path) -> Option<String> {
    let parent = path.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let id = uuid::Uuid::new_v4().hyphenated().to_string();
    // A temp file is created 0600 on Unix, and the publish keeps that mode.
    let mut temp = tempfile::NamedTempFile::new_in(parent).ok()?;
    temp.write_all(id.as_bytes()).ok()?;
    temp.flush().ok()?;
    match persist_temp_file_noclobber(temp, path) {
        Ok(PersistOutcome::Published) => Some(id),
        // A concurrent first run won: its value is the installation's.
        Ok(PersistOutcome::Occupied(_)) => match read_existing(path) {
            Existing::Valid(winner) => Some(winner),
            Existing::Absent | Existing::Refused => None,
        },
        Err(error) => {
            tracing::debug!(target: "ocx_telemetry", kind = ?error.kind(), "installation id not written");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const VALID: &str = "3f0c2a52-8d1e-4b7a-9c3d-5e6f7a8b9c0d";

    fn id_file(home: &Path) -> PathBuf {
        home.join("state").join("telemetry").join("installation-id")
    }

    fn seed(home: &Path, content: &[u8]) -> PathBuf {
        let path = id_file(home);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn identity_absent_is_created_private_and_reused() {
        let home = tempfile::tempdir().unwrap();
        let path = id_file(home.path());
        let first = installation_id(&path).await.expect("created");
        assert_eq!(parse(first.as_bytes()).as_deref(), Some(first.as_str()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(installation_id(&path).await.as_deref(), Some(first.as_str()));
    }

    #[tokio::test]
    async fn identity_valid_is_reused_unchanged() {
        for content in [VALID.to_owned(), format!("{VALID}\n")] {
            let home = tempfile::tempdir().unwrap();
            let path = seed(home.path(), content.as_bytes());
            assert_eq!(installation_id(&path).await.as_deref(), Some(VALID));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn identity_symlink_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("elsewhere");
        std::fs::write(&target, VALID).unwrap();
        let path = id_file(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(installation_id(&path).await, None);
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), VALID);
    }

    /// A file symlink is a reparse point: the id file's path must not lead anywhere else.
    #[cfg(windows)]
    #[tokio::test]
    async fn identity_reparse_point_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let target = home.path().join("elsewhere");
        std::fs::write(&target, VALID).unwrap();
        let path = id_file(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        match std::os::windows::fs::symlink_file(&target, &path) {
            Ok(()) => {}
            // Windows grants symlink creation to elevated users and Developer Mode only; a CI runner
            // that cannot is a broken runner, and a skip there would hide the refusal untested.
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                let ci = crate::ci::CiEnvironment::from_env();
                assert!(
                    !(ci.ci || ci.provider().is_some()),
                    "this runner may not create symlinks: {error}"
                );
                eprintln!("skipped: this account may not create symlinks: {error}");
                return;
            }
            Err(error) => panic!("creating the symlink failed: {error}"),
        }
        assert_eq!(installation_id(&path).await, None);
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), VALID);
    }

    #[cfg(unix)]
    #[test]
    fn a_handle_opened_before_the_path_became_a_link_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let path = seed(home.path(), VALID.as_bytes());
        // The same handle is read while the path still names it.
        let handle = std::fs::File::open(&path).unwrap();
        assert!(matches!(read_handle(handle, &path), Existing::Valid(id) if id == VALID));

        // The swap an attacker with write access to the directory would make after the open.
        let opened = std::fs::File::open(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let elsewhere = home.path().join("elsewhere");
        std::fs::write(&elsewhere, VALID).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(matches!(read_handle(opened, &path), Existing::Refused));

        // A different regular file at the path is refused as well.
        let opened = std::fs::File::open(&elsewhere).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, VALID).unwrap();
        assert!(matches!(read_handle(opened, &path), Existing::Refused));
    }

    #[test]
    fn a_stalled_id_read_never_holds_the_runtime_past_the_deadline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (release, hold) = std::sync::mpsc::channel::<()>();

        let started = std::time::Instant::now();
        let waited = runtime.block_on(async {
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                detached(move || {
                    // Never answers until the test lets go, the way a hung filesystem does.
                    let _ = hold.recv();
                    None
                }),
            )
            .await
        });
        assert!(waited.is_err(), "the stalled read must hit the deadline");
        assert!(started.elapsed() < std::time::Duration::from_millis(1000));

        // Dropped on its own thread so a runtime that waits for the stalled read fails this test
        // at the bound instead of hanging it.
        let (dropped, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(runtime);
            let _ = dropped.send(());
        });
        assert!(
            done.recv_timeout(std::time::Duration::from_millis(500)).is_ok(),
            "a runtime that ran a stalled read must still drop promptly"
        );
        drop(release);
    }

    #[tokio::test]
    async fn a_panicking_id_read_is_no_id() {
        assert_eq!(detached(|| panic!("a broken filesystem driver")).await, None);
    }

    #[tokio::test]
    async fn identity_unusable_content_is_none_and_untouched() {
        let oversized = format!("{VALID}{}", " ".repeat(200));
        let cases: [(&str, Vec<u8>); 8] = [
            ("empty", Vec::new()),
            ("malformed", b"not-a-uuid".to_vec()),
            ("uppercase", VALID.to_uppercase().into_bytes()),
            ("no hyphens", VALID.replace('-', "").into_bytes()),
            ("two newlines", format!("{VALID}\n\n").into_bytes()),
            ("surrounding space", format!(" {VALID}").into_bytes()),
            ("oversized", oversized.into_bytes()),
            // Version 1, not 4.
            ("not v4", b"3f0c2a52-8d1e-1b7a-9c3d-5e6f7a8b9c0d".to_vec()),
        ];
        for (label, content) in cases {
            let home = tempfile::tempdir().unwrap();
            let path = seed(home.path(), &content);
            assert_eq!(installation_id(&path).await, None, "{label}");
            assert_eq!(std::fs::read(&path).unwrap(), content, "{label}");
        }
    }

    #[tokio::test]
    async fn identity_non_regular_file_is_none_and_untouched() {
        let home = tempfile::tempdir().unwrap();
        let path = id_file(home.path());
        std::fs::create_dir_all(&path).unwrap();
        assert_eq!(installation_id(&path).await, None);
        assert!(path.is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn identity_read_only_parent_is_none() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("state");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // A superuser writes through a read-only directory; the case then proves nothing.
        let writable = std::fs::File::create(dir.join("probe")).is_ok();
        let id = installation_id(&id_file(home.path())).await;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !writable {
            assert_eq!(id, None);
            assert!(!id_file(home.path()).exists());
        }
    }

    #[tokio::test]
    async fn identity_parent_blocked_by_a_file_is_none() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("state"), "a file, not a directory").unwrap();
        assert_eq!(installation_id(&id_file(home.path())).await, None);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn identity_create_never_clobbers() {
        for _ in 0..25 {
            let home = tempfile::tempdir().unwrap();
            let path = id_file(home.path());
            let (first, second) = tokio::join!(installation_id(&path), installation_id(&path));
            let first = first.expect("a valid id");
            let second = second.expect("a valid id");
            let stored = std::fs::read_to_string(&path).unwrap();
            assert_eq!(first, stored);
            assert_eq!(second, stored);
            let leftovers = std::fs::read_dir(path.parent().unwrap()).unwrap().count();
            assert_eq!(leftovers, 1, "a losing racer must not strand its temp file");
        }
    }
}
