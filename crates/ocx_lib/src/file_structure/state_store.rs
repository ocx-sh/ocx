// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use crate::oci;
use crate::prelude::StringExt as _;

/// Manages persistent runtime state files under `$OCX_HOME/state/`.
///
/// `StateStore` is the typed home for state files whose existence or mtime IS
/// the data.  Unlike `cache/` (regenerable bulk), files here are persistent
/// across sessions.
///
/// Layout:
/// ```text
/// {root}/
///   update-check/
///     {slug}          — zero-byte file; mtime = time of last registry probe
/// ```
#[derive(Debug, Clone)]
pub struct StateStore {
    root: PathBuf,
}

impl StateStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory of the state store (e.g., `$OCX_HOME/state`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the directory for update-check throttle state files.
    ///
    /// Path: `{root}/update-check/`
    ///
    /// Each file under this directory has the form `{slug}` where `slug` is
    /// the strict (no-dot) slug of the identifier. The mtime of the file is
    /// the only datum — content is always zero bytes.
    pub fn update_check_dir(&self) -> PathBuf {
        self.root.join("update-check")
    }

    /// Returns the throttle state file path for the given identifier.
    ///
    /// Path: `{root}/update-check/{slug}` where `{slug}` is the strict
    /// (no-dot) slug of `identifier.to_string()` — all non-alphanumeric
    /// characters replaced with `_`. For `ocx.sh/ocx/cli` this produces
    /// `ocx_sh_ocx_cli`.
    ///
    /// The file is zero-byte; its mtime is the only datum (time of last
    /// registry probe). The parent directory is created lazily on first touch.
    ///
    /// # Atomic-touch portability
    ///
    /// The throttle write path (`update_check::touch_state_atomic`) relies on
    /// `std::fs::rename` to publish a new mtime atomically without races on
    /// the destination. Platform semantics:
    ///
    /// - **Linux**: `rename(2)` follows symlinks at source, replaces the
    ///   destination inode atomically without intermediate "absent" state.
    /// - **macOS**: BSD `rename(2)` provides the same atomic-replace
    ///   semantics as Linux.
    /// - **Windows**: `std::fs::rename` shims to `MoveFileExW` with
    ///   `MOVEFILE_REPLACE_EXISTING`, which gives equivalent atomic-replace
    ///   semantics (modulo open-handle constraints on the destination).
    ///
    /// In all three cases the atomic-touch contract holds. See
    /// <https://doc.rust-lang.org/std/fs/fn.rename.html> for the cross-platform
    /// shim's documented invariants.
    pub fn update_check_file(&self, identifier: &oci::Identifier) -> PathBuf {
        let slug = identifier.to_string().to_slug();
        self.update_check_dir().join(slug)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::StateStore;
    use crate::oci;

    #[test]
    fn root_returns_store_root() {
        let store = StateStore::new("/state");
        assert_eq!(store.root(), Path::new("/state"));
    }

    #[test]
    fn update_check_dir_is_rooted_under_state() {
        let store = StateStore::new("/ocx/state");
        assert_eq!(store.update_check_dir(), PathBuf::from("/ocx/state/update-check"));
    }

    /// The state file for `ocx.sh/ocx/cli` must be rooted under `update-check/`
    /// with the slug `ocx_sh_ocx_cli` (strict no-dot encoding).
    #[test]
    fn update_check_file_produces_correct_path() {
        let store = StateStore::new("/ocx/state");
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        assert_eq!(path, PathBuf::from("/ocx/state/update-check/ocx_sh_ocx_cli"));
    }

    /// The slug must contain no dots and no forward slashes.
    #[test]
    fn update_check_file_slug_is_dot_free() {
        let store = StateStore::new("/state");
        let identifier = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
        let path = store.update_check_file(&identifier);
        let file_name = path.file_name().unwrap().to_str().unwrap();
        assert!(!file_name.contains('.'), "slug must not contain dots; got: {file_name}");
        assert!(
            !file_name.contains('/'),
            "slug must not contain slashes; got: {file_name}"
        );
    }
}
