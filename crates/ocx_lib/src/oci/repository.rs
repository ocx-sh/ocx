// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use super::Identifier;

/// A parsed OCI repository reference: `registry/repository` without tag or digest.
///
/// Represents the concept of "which package" independent of any specific version.
/// Use this instead of ad-hoc `(String, String)` tuples or `without_specifiers()`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Repository {
    registry: String,
    repository: String,
}

impl Repository {
    /// Creates a new repository reference from explicit registry and repository strings.
    pub fn new(registry: impl Into<String>, repository: impl Into<String>) -> Self {
        Self {
            registry: registry.into(),
            repository: repository.into(),
        }
    }

    /// Returns the registry hostname (and optional port), e.g. `"ghcr.io"`.
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// Returns the repository path within the registry, e.g. `"cmake"` or `"myorg/tool"`.
    pub fn repository(&self) -> &str {
        &self.repository
    }
}

impl std::fmt::Display for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)
    }
}

impl From<&Identifier> for Repository {
    fn from(id: &Identifier) -> Self {
        Self {
            registry: id.registry().to_owned(),
            repository: id.repository().to_owned(),
        }
    }
}

impl Serialize for Repository {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Repository {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let id = Identifier::parse(&s).map_err(serde::de::Error::custom)?;
        if id.tag().is_some() || id.digest().is_some() {
            return Err(serde::de::Error::custom("repository must not contain a tag or digest"));
        }
        Ok(Repository::from(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{Digest, PinnedIdentifier};

    #[test]
    fn construction() {
        let repo = Repository::new("ghcr.io", "cmake");
        assert_eq!(repo.registry(), "ghcr.io");
        assert_eq!(repo.repository(), "cmake");
    }

    #[test]
    fn display() {
        let repo = Repository::new("ghcr.io", "cmake");
        assert_eq!(repo.to_string(), "ghcr.io/cmake");
    }

    #[test]
    fn display_nested_repo() {
        let repo = Repository::new("ghcr.io", "myorg/tool");
        assert_eq!(repo.to_string(), "ghcr.io/myorg/tool");
    }

    #[test]
    fn from_identifier() {
        let id: Identifier = "ghcr.io/cmake:3.28".parse().unwrap();
        let repo = Repository::from(&id);
        assert_eq!(repo.registry(), "ghcr.io");
        assert_eq!(repo.repository(), "cmake");
    }

    #[test]
    fn from_identifier_strips_tag_and_digest() {
        let hex = "a".repeat(64);
        let id: Identifier = format!("ghcr.io/cmake:3.28@sha256:{hex}").parse().unwrap();
        let repo = Repository::from(&id);
        assert_eq!(repo.registry(), "ghcr.io");
        assert_eq!(repo.repository(), "cmake");
    }

    #[test]
    fn from_pinned_identifier_via_deref() {
        let id = Identifier::new_registry("cmake", "example.com")
            .clone_with_tag("3.28")
            .clone_with_digest(Digest::Sha256("a".repeat(64)));
        let pinned = PinnedIdentifier::try_from(id).unwrap();
        let repo = Repository::from(&*pinned);
        assert_eq!(repo.registry(), "example.com");
        assert_eq!(repo.repository(), "cmake");
    }

    #[test]
    fn serde_roundtrip() {
        let repo = Repository::new("ghcr.io", "cmake");
        let json = serde_json::to_string(&repo).unwrap();
        assert_eq!(json, r#""ghcr.io/cmake""#);
        let deserialized: Repository = serde_json::from_str(&json).unwrap();
        assert_eq!(repo, deserialized);
    }

    #[test]
    fn deserialize_rejects_tagged_string() {
        let err = serde_json::from_str::<Repository>(r#""ghcr.io/cmake:3.28""#).unwrap_err();
        assert!(err.to_string().contains("tag or digest"));
    }

    #[test]
    fn deserialize_rejects_digest_string() {
        let hex = "a".repeat(64);
        let json = format!(r#""ghcr.io/cmake@sha256:{hex}""#);
        let err = serde_json::from_str::<Repository>(&json).unwrap_err();
        assert!(err.to_string().contains("tag or digest"));
    }

    #[test]
    fn deserialize_rejects_missing_registry() {
        let err = serde_json::from_str::<Repository>(r#""cmake""#).unwrap_err();
        assert!(err.to_string().contains("explicit registry"));
    }

    #[test]
    fn equality_and_hash() {
        use std::collections::HashSet;
        let a = Repository::new("ghcr.io", "cmake");
        let b = Repository::new("ghcr.io", "cmake");
        let c = Repository::new("ghcr.io", "other");
        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a.clone());
        assert!(!set.insert(b));
        assert!(set.insert(c));
    }

    #[test]
    fn ord() {
        let a = Repository::new("a.io", "z");
        let b = Repository::new("b.io", "a");
        assert!(a < b);
    }
}
