// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The shared fixture tree under `crates/ocx_test_support/data/`.
//!
//! `CARGO_MANIFEST_DIR` expands at compile time to the crate being compiled,
//! which is this one wherever the helper is called from — so a consumer crate
//! never has to know where the fixtures live.

pub fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}

pub fn archive_dir() -> std::path::PathBuf {
    data_dir().join("archive")
}

pub fn archive_xz() -> std::path::PathBuf {
    archive_dir().with_added_extension("tar.xz")
}
