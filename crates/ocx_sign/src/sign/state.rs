// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The signing subsystem's paths under the state root, kept here so `ocx_sign` never depends on `ocx_store`.

use std::path::{Path, PathBuf};

use ocx_util::prelude::StringExt as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningStatePaths {
    root: PathBuf,
}

impl SigningStatePaths {
    /// Anchor the layout at `root` — `StateStore::root`, never a subdirectory of it.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root these paths are derived under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `{root}/referrers/{slug}.json`, the Referrers-API capability cache for `registry`.
    ///
    /// The relaxed slug stops a hostile registry string escaping the store root.
    #[must_use]
    pub fn referrers_capability_file(&self, registry: &str) -> PathBuf {
        self.root
            .join("referrers")
            .join(format!("{}.json", registry.to_relaxed_slug()))
    }

    /// `{root}/trust_root/{slug}.json`, the offline-verify trust-root cache for a Rekor `host[:port]`.
    ///
    /// The relaxed slug stops a hostile authority string escaping the store root.
    #[must_use]
    pub fn trust_root_file(&self, rekor_authority: &str) -> PathBuf {
        self.root
            .join("trust_root")
            .join(format!("{}.json", rekor_authority.to_relaxed_slug()))
    }

    /// Checkout directory for the TUF client that fetches the public-good Sigstore trust root.
    #[must_use]
    pub fn tuf_cache_dir(&self) -> PathBuf {
        self.root.join("tuf")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The referrers capability cache for `ghcr.io` lands under `referrers/`
    /// with the relaxed slug (dots preserved) plus a `.json` suffix.
    ///
    /// Moved verbatim from `file_structure::state_store`, literal included:
    /// this is the byte-identity claim ADR 1.9 makes, and it can only be made
    /// against the expectation the old accessor was pinned to.
    #[test]
    fn referrers_capability_file_produces_correct_path() {
        let paths = SigningStatePaths::new("/ocx/state");
        assert_eq!(
            paths.referrers_capability_file("ghcr.io"),
            PathBuf::from("/ocx/state/referrers/ghcr.io.json")
        );
    }

    /// The trust-root cache is keyed by Rekor authority under `trust_root/`;
    /// the `:` in `host:port` is neutralised to `_` by the relaxed slug. Moved
    /// verbatim from `file_structure::state_store`.
    #[test]
    fn trust_root_file_produces_correct_path() {
        let paths = SigningStatePaths::new("/ocx/state");
        assert_eq!(
            paths.trust_root_file("rekor.example:443"),
            PathBuf::from("/ocx/state/trust_root/rekor.example_443.json")
        );
    }

    /// The TUF checkout is `{root}/tuf`, keyed by nothing.
    ///
    /// The one derivation `state_store.rs` never pinned: it was asserted only
    /// through `resolve_trust_root`, which passes whatever it is handed, so a
    /// repointed accessor would have kept every test green. Pinned here because
    /// the directory is a cache an operator finds on disk.
    #[test]
    fn tuf_cache_dir_produces_correct_path() {
        let paths = SigningStatePaths::new("/ocx/state");
        assert_eq!(paths.tuf_cache_dir(), PathBuf::from("/ocx/state/tuf"));
    }

    /// A hostile registry / authority string must never escape the store root:
    /// the relaxed slug replaces `/` and `..`-forming dots so every produced
    /// path stays a direct child of the subsystem directory. Moved verbatim
    /// from `file_structure::state_store`, which had it from `capability.rs`.
    #[test]
    fn cache_files_reject_hostile_slug() {
        let paths = SigningStatePaths::new("/ocx/state");
        for hostile in ["../evil", "/etc/passwd", "..", "../../etc/shadow", "foo/../bar"] {
            for file in [paths.referrers_capability_file(hostile), paths.trust_root_file(hostile)] {
                let name = file.file_name().unwrap().to_str().unwrap();
                assert!(!name.contains('/'), "slug must not contain slashes; got: {name}");
                // The only path components below the store root are the subsystem
                // dir and the single slugged filename — no `..` traversal.
                let tail: Vec<_> = file.strip_prefix("/ocx/state").unwrap().components().collect();
                assert_eq!(tail.len(), 2, "expected {{subsystem}}/{{slug}}.json, got {file:?}");
            }
        }
    }
}
