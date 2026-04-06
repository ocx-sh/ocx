// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;
pub mod manager;
pub mod snapshot;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::file_lock::FileLock;

pub use error::ProfileError;
pub use manager::{ProfileAddInput, ProfileManager};
pub use snapshot::ProfileSnapshot;

/// Whether an [`add`](ProfileManifest::add) call inserted a new entry or updated an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// A new entry was appended to the profile.
    Added,
    /// An existing entry with the same identifier was updated in place.
    /// Carries the previous mode so callers can detect and warn about mode changes.
    Updated { previous_mode: ProfileMode },
}

/// Resolution mode for a profiled package.
///
/// - `Current` — resolve via the `current` symlink (floating pointer, set by `ocx select`).
/// - `Candidate` — resolve via the `candidates/{tag}` symlink (pinned to a specific tag).
/// - `Content` — resolve via the content-addressed object store path (digest-based, changes on update).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProfileMode {
    Current,
    Candidate,
    Content,
}

impl std::fmt::Display for ProfileMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProfileMode::Current => write!(f, "current"),
            ProfileMode::Candidate => write!(f, "candidate"),
            ProfileMode::Content => write!(f, "content"),
        }
    }
}

/// A single entry in the profile manifest.
///
/// The `identifier` is the fully-qualified OCI identifier. For `Content` mode entries,
/// the identifier carries the pinned digest (set via [`Identifier::clone_with_digest`]).
/// The `mode` determines how the package content path is resolved at shell startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ProfileEntry {
    /// Fully-qualified OCI identifier. Content-mode entries include a digest for
    /// direct object store resolution without re-querying the index.
    pub identifier: crate::oci::Identifier,
    /// Resolution mode: `current` (floating), `candidate` (pinned to tag), or `content` (pinned to digest).
    pub mode: ProfileMode,
}

impl<'de> Deserialize<'de> for ProfileEntry {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            identifier: String,
            mode: ProfileMode,
        }
        let raw = Raw::deserialize(deserializer)?;
        let identifier = crate::oci::Identifier::parse_with_default_registry(
            &raw.identifier,
            crate::oci::identifier::DEFAULT_REGISTRY,
        )
        .map_err(serde::de::Error::custom)?;
        Ok(ProfileEntry {
            identifier,
            mode: raw.mode,
        })
    }
}

/// The profile manifest stored at `$OCX_HOME/profile.json`.
///
/// Contains a list of packages whose environment variables should be loaded
/// into every new shell session via `ocx shell profile load`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ProfileManifest {
    /// Schema version. Currently `1`.
    pub version: u32,
    /// Ordered list of profiled packages.
    pub packages: Vec<ProfileEntry>,
}

impl Default for ProfileManifest {
    fn default() -> Self {
        Self {
            version: 1,
            packages: Vec::new(),
        }
    }
}

impl ProfileManifest {
    const SUPPORTED_VERSION: u32 = 1;

    /// Loads the profile manifest from the given path.
    ///
    /// Returns an empty manifest if the file does not exist.
    /// Returns an error if the file exists but cannot be parsed or has an unsupported version.
    pub fn load(path: &Path) -> Result<Self, ProfileError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path).map_err(|e| ProfileError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let manifest: Self = serde_json::from_str(&contents).map_err(|e| ProfileError::Json {
            path: path.to_path_buf(),
            source: e,
        })?;
        if manifest.version != Self::SUPPORTED_VERSION {
            return Err(ProfileError::UnsupportedVersion {
                path: path.to_path_buf(),
                version: manifest.version,
                supported: Self::SUPPORTED_VERSION,
            });
        }
        Ok(manifest)
    }

    /// Loads the profile manifest while holding an exclusive file lock.
    ///
    /// Use this for load-modify-save operations to prevent concurrent mutation.
    /// The returned [`FileLock`] must be held until after [`save`](Self::save) completes.
    pub fn load_exclusive(path: &Path) -> Result<(Self, FileLock), ProfileError> {
        let lock_path = path.with_extension("json.lock");
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProfileError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let lock_file = std::fs::File::create(&lock_path).map_err(|e| ProfileError::Io {
            path: lock_path.clone(),
            source: e,
        })?;
        let lock = FileLock::try_exclusive(lock_file).map_err(|_| ProfileError::Locked { path: lock_path })?;
        let manifest = Self::load(path)?;
        Ok((manifest, lock))
    }

    /// Saves the profile manifest to the given path using atomic write (temp + rename).
    pub fn save(&self, path: &Path) -> Result<(), ProfileError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ProfileError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| ProfileError::Json {
            path: path.to_path_buf(),
            source: e,
        })?;

        // Atomic write: write to temp file in same directory, then rename
        let parent = path
            .parent()
            .expect("profile manifest path must have a parent directory");
        let tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| ProfileError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
        std::fs::write(tmp.path(), json.as_bytes()).map_err(|e| ProfileError::Io {
            path: tmp.path().to_path_buf(),
            source: e,
        })?;
        tmp.persist(path).map_err(|e| ProfileError::Io {
            path: path.to_path_buf(),
            source: e.into(),
        })?;
        Ok(())
    }

    /// Adds a package entry to the profile. If an entry with the same
    /// registry+repo+tag already exists (ignoring digest), it is updated in place.
    pub fn add(&mut self, entry: ProfileEntry) -> AddOutcome {
        let key = entry.identifier.without_digest();
        if let Some(existing) = self.packages.iter_mut().find(|e| e.identifier.without_digest() == key) {
            let previous_mode = existing.mode;
            *existing = entry;
            AddOutcome::Updated { previous_mode }
        } else {
            self.packages.push(entry);
            AddOutcome::Added
        }
    }

    /// Returns all entries whose identifier matches the same registry and repository
    /// as the given identifier (ignoring tag differences).
    pub fn entries_for_repo(&self, identifier: &crate::oci::Identifier) -> Vec<&ProfileEntry> {
        let target = crate::oci::Repository::from(identifier);
        self.packages
            .iter()
            .filter(|e| crate::oci::Repository::from(&e.identifier) == target)
            .collect()
    }

    /// Removes all entries matching the given identifier.
    /// Returns `true` if any entry was removed.
    pub fn remove(&mut self, identifier: &crate::oci::Identifier) -> bool {
        let before = self.packages.len();
        self.packages.retain(|e| e.identifier != *identifier);
        self.packages.len() < before
    }

    /// Returns `true` if the profile contains an entry with the given identifier.
    pub fn contains(&self, identifier: &crate::oci::Identifier) -> bool {
        self.packages.iter().any(|e| e.identifier == *identifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_manifest_has_version_1() {
        let manifest = ProfileManifest::default();
        assert_eq!(manifest.version, 1);
        assert!(manifest.packages.is_empty());
    }

    fn make_id(s: &str) -> crate::oci::Identifier {
        s.parse().unwrap()
    }

    fn make_entry(identifier: &str, mode: ProfileMode) -> ProfileEntry {
        ProfileEntry {
            identifier: make_id(identifier),
            mode,
        }
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let manifest = ProfileManifest::load(Path::new("/nonexistent/profile.json")).unwrap();
        assert_eq!(manifest.version, 1);
        assert!(manifest.packages.is_empty());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");

        let mut manifest = ProfileManifest::default();
        manifest.add(make_entry("ocx.sh/cmake", ProfileMode::Current));
        manifest.add(make_entry("ocx.sh/node:18", ProfileMode::Candidate));
        manifest.save(&path).unwrap();

        let loaded = ProfileManifest::load(&path).unwrap();
        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.packages.len(), 2);
        assert_eq!(loaded.packages[0].identifier, make_id("ocx.sh/cmake"));
        assert_eq!(loaded.packages[0].mode, ProfileMode::Current);
        assert_eq!(loaded.packages[1].identifier, make_id("ocx.sh/node:18"));
        assert_eq!(loaded.packages[1].mode, ProfileMode::Candidate);
    }

    #[test]
    fn add_duplicate_updates_mode() {
        let mut manifest = ProfileManifest::default();
        let outcome1 = manifest.add(make_entry("ocx.sh/cmake", ProfileMode::Current));
        assert_eq!(outcome1, AddOutcome::Added);

        let outcome2 = manifest.add(make_entry("ocx.sh/cmake", ProfileMode::Candidate));
        assert_eq!(
            outcome2,
            AddOutcome::Updated {
                previous_mode: ProfileMode::Current
            }
        );

        assert_eq!(manifest.packages.len(), 1);
        assert_eq!(manifest.packages[0].mode, ProfileMode::Candidate);
    }

    #[test]
    fn remove_existing_entry() {
        let mut manifest = ProfileManifest::default();
        manifest.add(make_entry("ocx.sh/cmake", ProfileMode::Current));
        assert!(manifest.remove(&make_id("ocx.sh/cmake")));
        assert!(manifest.packages.is_empty());
    }

    #[test]
    fn remove_nonexistent_returns_false() {
        let mut manifest = ProfileManifest::default();
        assert!(!manifest.remove(&make_id("ocx.sh/cmake")));
    }

    #[test]
    fn contains_check() {
        let mut manifest = ProfileManifest::default();
        manifest.add(make_entry("ocx.sh/cmake", ProfileMode::Current));
        assert!(manifest.contains(&make_id("ocx.sh/cmake")));
        assert!(!manifest.contains(&make_id("ocx.sh/node")));
    }

    #[test]
    fn serde_json_format() {
        let mut manifest = ProfileManifest::default();
        manifest.add(make_entry("ocx.sh/cmake", ProfileMode::Current));
        let json = serde_json::to_string_pretty(&manifest).unwrap();
        assert!(json.contains(r#""version": 1"#));
        assert!(json.contains(r#""mode": "current""#));
    }

    #[test]
    fn load_unsupported_version_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        std::fs::write(&path, r#"{"version": 2, "packages": []}"#).unwrap();

        let err = ProfileManifest::load(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unsupported profile manifest version 2"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn content_digest_on_identifier_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");
        let digest = crate::oci::Digest::Sha256("a".repeat(64));

        let mut manifest = ProfileManifest::default();
        manifest.add(ProfileEntry {
            identifier: make_id("ocx.sh/cmake:3.28").clone_with_digest(digest.clone()),
            mode: ProfileMode::Content,
        });
        manifest.save(&path).unwrap();

        let loaded = ProfileManifest::load(&path).unwrap();
        assert_eq!(loaded.packages[0].identifier.digest(), Some(digest));
    }

    #[test]
    fn load_exclusive_prevents_concurrent_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.json");

        let (_manifest, _lock) = ProfileManifest::load_exclusive(&path).unwrap();

        // A second exclusive lock on the same path should fail
        let result = ProfileManifest::load_exclusive(&path);
        assert!(result.is_err(), "second exclusive lock should fail");
    }
}
