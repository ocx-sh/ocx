// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Where the managed-config tier keeps its state on disk.
//!
//! The loader's discovery path and the persister's write target both derive from here, or a
//! snapshot is written where nothing looks for it.

use std::path::{Path, PathBuf};

// Renaming any segment silently orphans every existing `$OCX_HOME/state/managed-config/`.
const DIR: &str = "managed-config";
const SNAPSHOT_FILE: &str = "snapshot.json";
const PAYLOAD_FILE: &str = "config.toml";
const REFRESH_MARKER_FILE: &str = ".last-refresh-check";
const PAUSE_FILE: &str = "pause.json";

/// The managed-config tier's on-disk layout, rooted at `$OCX_HOME/state`, not `$OCX_HOME`
/// (use [`Self::for_ocx_home`] for that).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedConfigPaths {
    state_root: PathBuf,
}

impl ManagedConfigPaths {
    /// Layout under an explicit `state/` root.
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: state_root.into(),
        }
    }

    /// Layout under `$OCX_HOME`, joining `state/` itself.
    pub fn for_ocx_home(ocx_home: &Path) -> Self {
        Self::new(ocx_home.join("state"))
    }

    /// `{state_root}/managed-config/`
    pub fn dir(&self) -> PathBuf {
        self.state_root.join(DIR)
    }

    /// `{state_root}/managed-config/snapshot.json`, the snapshot metadata describing
    /// [`Self::toml_file`].
    pub fn snapshot_file(&self) -> PathBuf {
        self.dir().join(SNAPSHOT_FILE)
    }

    /// `{state_root}/managed-config/config.toml`, the raw payload bytes.
    pub fn toml_file(&self) -> PathBuf {
        self.dir().join(PAYLOAD_FILE)
    }

    /// `{state_root}/managed-config/.last-refresh-check`, the refresh tick's freshness marker.
    ///
    /// Kept apart from the snapshot so a throttled probe never races the content file.
    pub fn refresh_marker(&self) -> PathBuf {
        self.dir().join(REFRESH_MARKER_FILE)
    }

    /// `{state_root}/managed-config/pause.json`, written by `ocx config update --pause`.
    pub fn pause_file(&self) -> PathBuf {
        self.dir().join(PAUSE_FILE)
    }

    /// The payload path beside `snapshot_path`; must match [`Self::toml_file`] or reader and
    /// writer drift apart.
    pub fn toml_beside_snapshot(snapshot_path: &Path) -> PathBuf {
        snapshot_path.with_file_name(PAYLOAD_FILE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **These five paths are on real users' disks.** A changed segment does not
    /// fail — it orphans the state silently and the tier re-fetches into a new
    /// directory, losing the pause and the refresh throttle with it.
    ///
    /// So the expected values here are written out segment by segment rather
    /// than derived from [`ManagedConfigPaths`], and they are the derivations
    /// the five deleted `StateStore::managed_config_*` accessors performed:
    /// `root.join("managed-config")` and four names joined onto it. Asserting
    /// `paths.snapshot_file() == paths.dir().join("snapshot.json")` would agree
    /// with any renaming of `dir()` and pin nothing.
    #[test]
    fn the_five_managed_config_paths_are_pinned() {
        let state_root = Path::new("/fixed/ocx-home/state");
        let paths = ManagedConfigPaths::new(state_root);

        let dir = Path::new("/fixed/ocx-home/state").join("managed-config");
        assert_eq!(paths.dir(), dir, "the tier's directory moved");
        assert_eq!(
            paths.snapshot_file(),
            Path::new("/fixed/ocx-home/state")
                .join("managed-config")
                .join("snapshot.json"),
            "the snapshot metadata file moved"
        );
        assert_eq!(
            paths.toml_file(),
            Path::new("/fixed/ocx-home/state")
                .join("managed-config")
                .join("config.toml"),
            "the payload sibling moved"
        );
        assert_eq!(
            paths.refresh_marker(),
            Path::new("/fixed/ocx-home/state")
                .join("managed-config")
                .join(".last-refresh-check"),
            "the refresh throttle marker moved"
        );
        assert_eq!(
            paths.pause_file(),
            Path::new("/fixed/ocx-home/state")
                .join("managed-config")
                .join("pause.json"),
            "the pause file moved"
        );
    }

    /// `for_ocx_home` carries the `state/` join the loader used to write by
    /// hand, and lands on the same snapshot path the old pure associated fn
    /// `StateStore::managed_config_snapshot_path(ocx_home)` produced:
    /// `ocx_home.join("state").join("managed-config").join("snapshot.json")`.
    #[test]
    fn for_ocx_home_adds_the_state_segment() {
        let home = Path::new("/fixed/ocx-home");

        assert_eq!(
            ManagedConfigPaths::for_ocx_home(home).snapshot_file(),
            Path::new("/fixed/ocx-home")
                .join("state")
                .join("managed-config")
                .join("snapshot.json"),
            "the loader's discovery candidate moved away from the persister's target"
        );
    }

    /// The sibling derivation, as `StateStore::managed_config_toml_path_for_snapshot`
    /// performed it: replace the file name, keep every parent segment.
    #[test]
    fn the_payload_sits_beside_whatever_snapshot_it_is_given() {
        let snapshot = Path::new("/somewhere/else/managed-config").join("snapshot.json");

        assert_eq!(
            ManagedConfigPaths::toml_beside_snapshot(&snapshot),
            Path::new("/somewhere/else/managed-config").join("config.toml"),
            "the reader stopped looking beside the snapshot it was handed"
        );
    }
}
