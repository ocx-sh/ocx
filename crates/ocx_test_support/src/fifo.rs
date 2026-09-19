// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Named pipes for the "a path that is not a regular file" test rows.

use std::path::Path;

/// `mkfifo(2)`, the one filesystem object `std::fs` cannot create.
#[track_caller]
pub fn mkfifo(path: &Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let c_path = CString::new(path.as_os_str().as_bytes()).expect("a tempdir path holds no NUL");
    // SAFETY: `c_path` is a NUL-terminated C string alive for the whole
    // call, and `mkfifo` only reads it.
    let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(created, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
}

/// Lets go of a reader stuck in `open(2)` on `path`, if there is one.
///
/// A `#[tokio::test]` runtime waits for its blocking pool on drop, so a test
/// whose red is "the unbounded read hung on the FIFO" would hang at teardown
/// instead of failing. Opening the write end with `O_NONBLOCK` succeeds only
/// when a reader is waiting (`ENXIO` otherwise — the green case, ignored) and
/// closing it again hands that reader an immediate EOF.
pub fn release_blocked_reader(path: &Path) {
    use std::os::unix::fs::OpenOptionsExt as _;

    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path);
}
