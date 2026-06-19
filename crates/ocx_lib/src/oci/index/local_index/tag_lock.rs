// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashMap};
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
/// `tags` is a [`BTreeMap`] so the serialized JSON emits keys in sorted
/// order. A `HashMap` would serialize in per-process SipHash iteration
/// order, churning the on-disk bytes on every `ocx index update` even when
/// no tag changed — git noise on committed index snapshots.
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
    pub(crate) version: Version,
    pub(crate) repository: oci::Repository,
    pub(crate) tags: BTreeMap<String, oci::Digest>,
}

impl TagLock {
    /// Creates a new tag lock from an identifier and its tags. The in-memory
    /// `HashMap` is collected into a sorted `BTreeMap` for deterministic
    /// serialization.
    pub(crate) fn new(identifier: &oci::Identifier, tags: HashMap<String, oci::Digest>) -> Self {
        Self {
            version: Version::V1,
            repository: oci::Repository::from(identifier),
            tags: tags.into_iter().collect(),
        }
    }

    /// Validates the repository, then returns the inner tags map.
    pub(crate) fn into_tags(
        self,
        expected: &oci::Identifier,
        path: &Path,
    ) -> crate::Result<HashMap<String, oci::Digest>> {
        let expected_repo = oci::Repository::from(expected);
        if self.repository != expected_repo {
            return Err(super::super::error::Error::TagLockRepositoryMismatch {
                path: path.to_path_buf(),
                expected: expected_repo,
                found: self.repository,
            }
            .into());
        }
        Ok(self.tags.into_iter().collect())
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
        assert_eq!(lock.tags, tags.into_iter().collect::<BTreeMap<_, _>>());
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
    fn tags_serialize_in_sorted_key_order() {
        // Regression: TagLock previously stored tags in a HashMap, so the
        // serialized key order followed HashMap iteration order — randomized
        // per process by the SipHash seed. Identical tag data produced
        // different bytes on every `ocx index update`, causing git churn on
        // committed index snapshots. The on-disk representation must be
        // deterministic; sorted keys are the contract.
        // Insert in a deliberately unsorted order so the test visibly demonstrates
        // that serialization — not insertion — establishes the key order. (HashMap
        // iteration is SipHash-randomized regardless of insertion order, but the
        // explicit shuffle keeps the intent legible, matching the api/data tests.)
        let id = make_id();
        let names = [
            "tag07", "tag01", "tag13", "tag04", "tag10", "tag02", "tag15", "tag08", "tag05", "tag11", "tag03", "tag14",
            "tag06", "tag09", "tag00", "tag12",
        ];
        let mut tags = HashMap::new();
        for name in names {
            tags.insert(name.to_string(), oci::Digest::Sha256("a".repeat(64)));
        }
        let lock = TagLock::new(&id, tags);
        let json = serde_json::to_string_pretty(&lock).unwrap();

        let mut by_position: Vec<&str> = names.to_vec();
        by_position.sort_by_key(|key| json.find(&format!("\"{key}\"")).expect("key present in json"));

        let mut sorted = names.to_vec();
        sorted.sort_unstable();

        assert_eq!(
            by_position, sorted,
            "tag keys must serialize in deterministic sorted order"
        );
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
