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
    // SAFETY: `c_path` is NUL-terminated and outlives the call; `mkfifo` only reads it.
    let created = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(created, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
}

/// Lets go of a reader stuck in `open(2)` on `path`, if there is one, by handing it an EOF.
///
/// Call it before teardown: a `#[tokio::test]` runtime waits for its blocking pool on drop, so a hung read
/// hangs the test instead of failing it.
pub fn release_blocked_reader(path: &Path) {
    use std::os::unix::fs::OpenOptionsExt as _;

    // `ENXIO` means no reader was waiting, the green case.
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path);
}
