// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use crate::{oci, profile::ProfileMode};

use super::{ProfileEntry, ProfileManifest};

/// Read-only, single-load view of the profile manifest for batch reference checks.
///
/// Loaded once and passed to multiple task methods so that profile warnings
/// never trigger redundant file reads. Silently returns empty on load errors
/// (profile awareness is best-effort).
#[derive(Debug, Clone)]
pub struct ProfileSnapshot {
    manifest: ProfileManifest,
}

impl ProfileSnapshot {
    /// Loads the profile manifest from `path`. Returns an empty snapshot on
    /// any error (missing file, parse failure, etc.) so callers never need
    /// to handle profile I/O errors.
    pub fn load(path: &Path) -> Self {
        let manifest = ProfileManifest::load(path).unwrap_or_default();
        Self { manifest }
    }

    /// Creates an empty snapshot (no entries). Useful in tests.
    pub fn empty() -> Self {
        Self {
            manifest: ProfileManifest::default(),
        }
    }

    /// Returns identifiers (with digest baked in) for all content-mode profile entries.
    ///
    /// Entries without a stored digest or with an unparseable identifier are silently skipped.
    pub fn content_digests(&self) -> Vec<&oci::Identifier> {
        self.manifest
            .packages
            .iter()
            .filter(|e| e.mode == ProfileMode::Content && e.identifier.digest().is_some())
            .map(|e| &e.identifier)
            .collect()
    }

    /// Returns all entries whose identifier matches the same registry and
    /// repository as `package` (ignoring tag differences).
    pub fn entries_for(&self, package: &oci::Identifier) -> Vec<&ProfileEntry> {
        self.manifest.entries_for_repo(package)
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
        }
    }

    #[test]
    fn empty_snapshot_has_no_entries() {
        let snap = ProfileSnapshot::empty();
        assert!(snap.entries().is_empty());
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let snap = ProfileSnapshot::load(Path::new("/nonexistent/profile.json"));
        assert!(snap.entries().is_empty());
    }

    fn make_id(s: &str) -> crate::oci::Identifier {
        s.parse().unwrap()
    }

    fn make_digest() -> crate::oci::Digest {
        crate::oci::Digest::Sha256("a".repeat(64))
    }

    #[test]
    fn content_digests_returns_identifiers_with_digest() {
        let snap = snapshot_with(vec![ProfileEntry {
            identifier: make_id("ocx.sh/cmake:3.28").clone_with_digest(make_digest()),
            mode: ProfileMode::Content,
        }]);
        let ids = snap.content_digests();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].registry(), "ocx.sh");
        assert_eq!(ids[0].repository(), "cmake");
        assert!(ids[0].digest().is_some());
    }

    #[test]
    fn content_digests_ignores_non_content_entries() {
        let snap = snapshot_with(vec![ProfileEntry {
            identifier: make_id("ocx.sh/cmake:3.28").clone_with_digest(make_digest()),
            mode: ProfileMode::Candidate,
        }]);
        assert!(snap.content_digests().is_empty());
    }

    #[test]
    fn content_digests_skips_entries_without_digest() {
        let snap = snapshot_with(vec![ProfileEntry {
            identifier: make_id("ocx.sh/cmake"),
            mode: ProfileMode::Content,
        }]);
        assert!(snap.content_digests().is_empty());
    }
}
