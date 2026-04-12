// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub fn data_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test")
        .join("data")
}

pub fn archive_dir() -> std::path::PathBuf {
    data_dir().join("archive")
}

pub fn archive_xz() -> std::path::PathBuf {
    archive_dir().with_added_extension("tar.xz")
}
