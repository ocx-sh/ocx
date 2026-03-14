// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use crate::{log, oci, profile::ProfileMode};

use super::{ProfileEntry, ProfileManifest};

/// Read-only, single-load view of the profile manifest for batch reference checks.
///
/// Loaded once and passed to multiple task methods so that profile warnings
/// never trigger redundant file reads. Silently returns empty on load errors
/// (profile awareness is best-effort).
#[derive(Debug, Clone)]
pub struct ProfileSnapshot {
    manifest: ProfileManifest,
    default_registry: String,
}

impl ProfileSnapshot {
    /// Loads the profile manifest from `path`. Returns an empty snapshot on
    /// any error (missing file, parse failure, etc.) so callers never need
    /// to handle profile I/O errors.
    pub fn load(path: &Path, default_registry: impl Into<String>) -> Self {
        let manifest = ProfileManifest::load(path).unwrap_or_default();
        Self {
            manifest,
            default_registry: default_registry.into(),
        }
    }

    /// Creates an empty snapshot (no entries). Useful in tests.
    pub fn empty() -> Self {
        Self {
            manifest: ProfileManifest::default(),
            default_registry: crate::oci::identifier::DEFAULT_REGISTRY.to_string(),
        }
    }

    /// Warns if any candidate-mode entry has an identifier that exactly matches
    /// `package`. Returns `true` if at least one warning was emitted.
    pub fn warn_if_candidate_referenced(&self, package: &oci::Identifier) -> bool {
        let package_str = package.to_string();
        let mut warned = false;
        for entry in &self.manifest.packages {
            if entry.mode == ProfileMode::Candidate && entry.identifier == package_str {
                log::warn!(
                    "`{}` is in your shell profile (candidate mode). \
                     Run `ocx install {}` to restore, \
                     or `ocx shell profile remove {}` to clean up.",
                    entry.identifier,
                    entry.identifier,
                    entry.identifier,
                );
                warned = true;
            }
        }
        warned
    }

    /// Warns if any current-mode entry shares the same registry/repository as
    /// `package`. Returns `true` if at least one warning was emitted.
    pub fn warn_if_current_referenced(&self, package: &oci::Identifier) -> bool {
        let mut warned = false;
        for entry in self.manifest.entries_for_repo(package, &self.default_registry) {
            if entry.mode == ProfileMode::Current {
                log::warn!(
                    "`{}` is in your shell profile (current mode). \
                     Run `ocx select {}` to restore, \
                     or switch to candidate mode with `ocx shell profile add --candidate {}`.",
                    entry.identifier,
                    entry.identifier,
                    entry.identifier,
                );
                warned = true;
            }
        }
        warned
    }

    /// Warns if any content-mode entry shares the same registry/repository as
    /// `package`. Returns `true` if at least one warning was emitted.
    pub fn warn_if_content_referenced(&self, package: &oci::Identifier) -> bool {
        let mut warned = false;
        for entry in self.manifest.entries_for_repo(package, &self.default_registry) {
            if entry.mode == ProfileMode::Content {
                log::warn!(
                    "`{}` is in your shell profile (content mode). \
                     Run `ocx install {}` to restore, \
                     or `ocx shell profile remove {}` to clean up.",
                    entry.identifier,
                    entry.identifier,
                    entry.identifier,
                );
                warned = true;
            }
        }
        warned
    }

    /// Checks whether any content-mode entry's `content_digest` matches the
    /// given partial digest prefix (as reconstructed from an object store path).
    ///
    /// Uses prefix matching because the sharded object path only encodes the
    /// first 32 hex characters of the digest.
    pub fn references_digest(&self, partial_digest: &str) -> bool {
        self.manifest.packages.iter().any(|e| {
            e.mode == ProfileMode::Content && e.content_digest.as_ref().is_some_and(|d| d.starts_with(partial_digest))
        })
    }

    /// Returns all entries whose identifier matches the same registry and
    /// repository as `package` (ignoring tag differences).
    pub fn entries_for(&self, package: &oci::Identifier) -> Vec<&ProfileEntry> {
        self.manifest.entries_for_repo(package, &self.default_registry)
    }

    /// Returns all entries in the snapshot.
    pub fn entries(&self) -> &[ProfileEntry] {
        &self.manifest.packages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_with(entries: Vec<ProfileEntry>) -> ProfileSnapshot {
        ProfileSnapshot {
            manifest: ProfileManifest {
                version: 1,
                packages: entries,
            },
            default_registry: crate::oci::identifier::DEFAULT_REGISTRY.to_string(),
        }
    }

    #[test]
    fn empty_snapshot_has_no_entries() {
        let snap = ProfileSnapshot::empty();
        assert!(snap.entries().is_empty());
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let snap = ProfileSnapshot::load(Path::new("/nonexistent/profile.json"), "ocx.sh");
        assert!(snap.entries().is_empty());
    }

    #[test]
    fn references_digest_matches_prefix() {
        let snap = snapshot_with(vec![ProfileEntry {
            identifier: "ocx.sh/cmake:3.28".to_string(),
            mode: ProfileMode::Content,
            content_digest: Some("sha256:43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9".to_string()),
        }]);
        assert!(snap.references_digest("sha256:43567c07f1a6b07b5e8dc052108c9d4c"));
        assert!(!snap.references_digest("sha256:00000000f1a6b07b5e8dc052108c9d4c"));
    }

    #[test]
    fn references_digest_ignores_non_content_entries() {
        let snap = snapshot_with(vec![ProfileEntry {
            identifier: "ocx.sh/cmake:3.28".to_string(),
            mode: ProfileMode::Candidate,
            content_digest: Some("sha256:43567c07f1a6b07b5e8dc052108c9d4c".to_string()),
        }]);
        assert!(!snap.references_digest("sha256:43567c07f1a6b07b5e8dc052108c9d4c"));
    }

    #[test]
    fn references_digest_handles_none_digest() {
        let snap = snapshot_with(vec![ProfileEntry {
            identifier: "ocx.sh/cmake".to_string(),
            mode: ProfileMode::Content,
            content_digest: None,
        }]);
        assert!(!snap.references_digest("sha256:anything"));
    }
}
