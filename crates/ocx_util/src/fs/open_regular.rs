// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Open a path for reading only when it is a regular file.

use std::path::Path;

/// Opens `path` for reading (blocking) and returns the handle with its metadata, or `None` when
/// the path is missing or is not a regular file (directory, FIFO, device). A symlink is followed.
///
/// The type is checked before the open (a writerless FIFO blocks `open(2)` forever, and Windows
/// fails a directory open as access denied) and again on the handle (the path can be swapped in
/// between), and the open uses `O_NONBLOCK` on Unix so that swap cannot block it either.
///
/// # Errors
///
/// Any I/O failure other than "not found".
pub fn open_regular(path: &Path) -> std::io::Result<Option<(std::fs::File, std::fs::Metadata)>> {
    open_regular_with(path, true)
}

/// As [`open_regular`], but a symlink leaf is `None` too: for an untrusted directory, where a link swapped in
/// after a safety walk would otherwise be read from outside it. The open uses `O_NOFOLLOW` on Unix, so the
/// swap is refused at the open and not only at the pre-stat.
///
/// # Errors
///
/// Any I/O failure other than "not found" or "is a symlink".
pub(crate) fn open_regular_no_follow(path: &Path) -> std::io::Result<Option<(std::fs::File, std::fs::Metadata)>> {
    open_regular_with(path, false)
}

fn open_regular_with(path: &Path, follow: bool) -> std::io::Result<Option<(std::fs::File, std::fs::Metadata)>> {
    let stat = if follow {
        std::fs::metadata(path)
    } else {
        std::fs::symlink_metadata(path)
    };
    match stat {
        Ok(metadata) if !metadata.is_file() => return Ok(None),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let file = match open_for_read(path, follow) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        // `O_NOFOLLOW` on a link leaf: ELOOP, or EMLINK on some BSDs.
        #[cfg(unix)]
        Err(error) if !follow && matches!(error.raw_os_error(), Some(libc::ELOOP | libc::EMLINK)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    Ok(metadata.is_file().then_some((file, metadata)))
}

#[cfg(unix)]
fn open_for_read(path: &Path, follow: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let flags = if follow {
        libc::O_NONBLOCK
    } else {
        libc::O_NONBLOCK | libc::O_NOFOLLOW
    };
    std::fs::OpenOptions::new().read(true).custom_flags(flags).open(path)
}

#[cfg(not(unix))]
fn open_for_read(path: &Path, _follow: bool) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_is_none() {
        let dir = tempfile::tempdir().expect("scratch dir");
        assert!(open_regular(&dir.path().join("absent")).expect("no error").is_none());
    }

    #[test]
    fn a_directory_is_none() {
        let dir = tempfile::tempdir().expect("scratch dir");
        assert!(open_regular(dir.path()).expect("no error").is_none());
    }

    #[test]
    fn a_regular_file_opens_with_its_metadata() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let path = dir.path().join("file");
        std::fs::write(&path, b"abc").expect("write");
        let (_, metadata) = open_regular(&path).expect("no error").expect("a regular file opens");
        assert_eq!(metadata.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_none() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let fifo = dir.path().join("pipe");
        ocx_test_support::fifo::mkfifo(&fifo);

        // A hang here means both the pre-stat and `O_NONBLOCK` are gone; the timeout turns it into a failure.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(open_regular(&fifo).map(|opened| opened.is_none()));
        });
        let outcome = receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("open_regular must not block on a FIFO");
        assert!(outcome.expect("no error"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_file_follows() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let target = dir.path().join("target");
        std::fs::write(&target, b"abc").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let (_, metadata) = open_regular(&link).expect("no error").expect("a link to a file opens");
        assert_eq!(metadata.len(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_refuses_a_symlink_to_a_file() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let target = dir.path().join("target");
        std::fs::write(&target, b"abc").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert!(open_regular_no_follow(&link).expect("no error").is_none());
        assert!(open_regular_no_follow(&target).expect("no error").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn no_follow_open_itself_refuses_a_link() {
        // Skips the pre-stat: proves the `O_NOFOLLOW` open is the second guard.
        let dir = tempfile::tempdir().expect("scratch dir");
        let target = dir.path().join("target");
        std::fs::write(&target, b"abc").expect("write");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let error = open_for_read(&link, false).expect_err("a link leaf is refused at the open");
        assert!(
            matches!(error.raw_os_error(), Some(libc::ELOOP | libc::EMLINK)),
            "{error:?}"
        );
    }
}
