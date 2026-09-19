// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Where the managed-config tier keeps its state on disk.
//!
//! Five paths under one directory, derived from the `state/` root. They used to
//! be five accessors on `StateStore`, which forced the config loader — which
//! holds no store and must never construct one — to reach into
//! `ocx_store::file_structure` just to find the snapshot it reads at startup. That
//! is the `ocx_config → ocx_store` edge the crate map forbids, and the layout
//! is config's to own anyway: the loader's discovery candidate and the
//! persister's write target have to be one path, or a snapshot is written where
//! nothing looks for it.
//!
//! `StateStore::managed_config()` hands one of these out for its own root, so
//! the store still answers "where" for callers that hold one, and the
//! derivation lives in exactly one place either way.

use std::path::{Path, PathBuf};

/// The directory name under the `state/` root, and the four file names in it.
///
/// Named constants rather than inline literals because two of them are
/// *external* contracts in the weak sense that matters here: a user's existing
/// `$OCX_HOME/state/managed-config/` is on disk right now, and renaming a
/// segment orphans it silently rather than failing.
const DIR: &str = "managed-config";
const SNAPSHOT_FILE: &str = "snapshot.json";
const PAYLOAD_FILE: &str = "config.toml";
const REFRESH_MARKER_FILE: &str = ".last-refresh-check";
const PAUSE_FILE: &str = "pause.json";

/// The managed-config tier's on-disk layout, rooted at the `state/` directory.
///
/// `state_root` is `$OCX_HOME/state` — the same root `StateStore` is
/// constructed with, not `$OCX_HOME` itself. Use [`Self::for_ocx_home`] when
/// all you hold is the home.
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

    /// Layout under `$OCX_HOME`, which carries the `state/` join itself.
    ///
    /// The config loader's only handle is the home, and it must not construct a
    /// `StateStore` to get from there to the snapshot.
    pub fn for_ocx_home(ocx_home: &Path) -> Self {
        Self::new(ocx_home.join("state"))
    }

    /// The directory holding the managed-config tier's persistent state.
    ///
    /// Path: `{state_root}/managed-config/`
    pub fn dir(&self) -> PathBuf {
        self.state_root.join(DIR)
    }

    /// The managed-config snapshot metadata file (`ManagedConfigSnapshot`,
    /// written atomically by `persist_managed_config`). The payload it describes
    /// lives in the sibling [`Self::toml_file`].
    ///
    /// Path: `{state_root}/managed-config/snapshot.json`
    pub fn snapshot_file(&self) -> PathBuf {
        self.dir().join(SNAPSHOT_FILE)
    }

    /// The managed-config payload file — the raw `config.toml` bytes the
    /// metadata snapshot describes, written as a readable sibling of
    /// `snapshot.json` by `persist_managed_config`.
    ///
    /// Path: `{state_root}/managed-config/config.toml`
    pub fn toml_file(&self) -> PathBuf {
        self.dir().join(PAYLOAD_FILE)
    }

    /// The zero-byte freshness marker touched by the background refresh tick
    /// (separate from the snapshot file itself so a throttled probe never has to
    /// touch — and risk racing — the content file).
    ///
    /// Path: `{state_root}/managed-config/.last-refresh-check`
    pub fn refresh_marker(&self) -> PathBuf {
        self.dir().join(REFRESH_MARKER_FILE)
    }

    /// The content-bearing pause file for the managed-config background tick
    /// (`ocx config update --pause` — see `managed_config::pause`).
    ///
    /// Path: `{state_root}/managed-config/pause.json`
    pub fn pause_file(&self) -> PathBuf {
        self.dir().join(PAUSE_FILE)
    }

    /// The payload path sitting beside the metadata snapshot at `snapshot_path`.
    ///
    /// A pure sibling derivation for the reader that holds only the snapshot
    /// path (`read_managed_config_snapshot_at`). It and [`Self::toml_file`]
    /// resolve to one path, so reader and writer can never drift.
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
