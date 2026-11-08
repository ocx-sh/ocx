// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture paths that stay absolute on every platform.

use std::path::PathBuf;

/// `rooted` (a `/`-led fixture path) as an absolute path: itself on Unix, drive-prefixed on Windows,
/// where a bare `/work/acme` has no drive and so is not absolute.
pub fn abs(rooted: &str) -> PathBuf {
    debug_assert!(rooted.starts_with('/'), "a fixture path is rooted: {rooted}");
    if cfg!(windows) {
        PathBuf::from(format!("C:{rooted}"))
    } else {
        PathBuf::from(rooted)
    }
}
