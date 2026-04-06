// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::oci;

/// Known versions of the tag lock format.
#[derive(Debug, Clone, Copy, Serialize_repr, Deserialize_repr, PartialEq)]
#[repr(u8)]
pub(crate) enum Version {
    V1 = 1,
}

/// Versioned envelope for persisted tag-to-digest lock maps.
///
/// ```json
/// {
///   "version": 1,
///   "repository": "ghcr.io/cmake",
///   "tags": {
///     "3.28": "sha256:abc123...",
///     "latest": "sha256:def456..."
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TagLock {
    pub version: Version,
    pub repository: oci::Repository,
    pub tags: HashMap<String, oci::Digest>,
}

impl TagLock {
    /// Creates a new tag lock from an identifier and its tags.
    pub fn new(identifier: &oci::Identifier, tags: HashMap<String, oci::Digest>) -> Self {
        Self {
            version: Version::V1,
            repository: oci::Repository::from(identifier),
            tags,
        }
    }

    /// Validates the repository, then returns the inner tags map.
    pub fn into_tags(self, expected: &oci::Identifier, path: &Path) -> crate::Result<HashMap<String, oci::Digest>> {
        let expected_repo = oci::Repository::from(expected);
        if self.repository != expected_repo {
            return Err(super::super::error::Error::TagLockRepositoryMismatch {
                path: path.to_path_buf(),
                expected: expected_repo,
                found: self.repository,
            }
            .into());
        }
        Ok(self.tags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id() -> oci::Identifier {
        oci::Identifier::new_registry("cmake", "ghcr.io").clone_with_tag("3.28")
    }

    fn make_tags() -> HashMap<String, oci::Digest> {
        let mut tags = HashMap::new();
        tags.insert("3.28".to_string(), oci::Digest::Sha256("a".repeat(64)));
        tags.insert("latest".to_string(), oci::Digest::Sha256("b".repeat(64)));
        tags
    }

    #[test]
    fn new_sets_version_and_repository() {
        let id = make_id();
        let tags = make_tags();
        let lock = TagLock::new(&id, tags.clone());
        assert_eq!(lock.version, Version::V1);
        assert_eq!(lock.repository, oci::Repository::new("ghcr.io", "cmake"));
        assert_eq!(lock.tags, tags);
    }

    #[test]
    fn serde_roundtrip() {
        let lock = TagLock::new(&make_id(), make_tags());
        let json = serde_json::to_string_pretty(&lock).unwrap();
        let deserialized: TagLock = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.version, lock.version);
        assert_eq!(deserialized.repository, lock.repository);
        assert_eq!(deserialized.tags, lock.tags);
    }

    #[test]
    fn into_tags_succeeds_on_match() {
        let id = make_id();
        let tags = make_tags();
        let lock = TagLock::new(&id, tags.clone());
        let result = lock.into_tags(&id, Path::new("/test/tags.json")).unwrap();
        assert_eq!(result, tags);
    }

    #[test]
    fn deserialize_rejects_unknown_version() {
        let json = r#"{"version":99,"repository":"ghcr.io/cmake","tags":{}}"#;
        let result = serde_json::from_str::<TagLock>(json);
        assert!(result.is_err());
    }

    #[test]
    fn into_tags_rejects_wrong_repository() {
        let id = make_id();
        let lock = TagLock::new(&id, make_tags());
        let other_id = oci::Identifier::new_registry("other", "ghcr.io");
        let err = lock.into_tags(&other_id, Path::new("/test/tags.json")).unwrap_err();
        assert!(err.to_string().contains("mismatch"));
    }

    #[test]
    fn json_shape_matches_spec() {
        let lock = TagLock::new(&make_id(), make_tags());
        let value: serde_json::Value = serde_json::to_value(&lock).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["repository"], "ghcr.io/cmake");
        assert!(value["tags"].is_object());
    }
}
