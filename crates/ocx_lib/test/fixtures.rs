// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use crate::file_structure::ObjectStore;

/// Creates a temporary directory and returns the TempDir guard (for lifetime)
/// and the canonicalized root path.
///
/// Uses `dunce::canonicalize` for Windows compatibility (avoids UNC paths).
pub fn temp_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(dir.path()).unwrap();
    (dir, root)
}

/// Creates a temporary directory with an [`ObjectStore`] rooted inside it.
pub fn temp_object_store() -> (tempfile::TempDir, PathBuf, ObjectStore) {
    let (dir, root) = temp_root();
    let store = ObjectStore::new(&root);
    (dir, root, store)
}
