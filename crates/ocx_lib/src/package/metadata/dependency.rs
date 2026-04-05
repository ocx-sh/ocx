// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::oci;

use super::visibility::Visibility;

/// A pinned dependency descriptor.
///
/// The digest references either an OCI Image Index (for platform-aware
/// resolution) or a single manifest (for explicit platform pinning).
/// For cross-compilation, pin the platform-specific manifest digest directly.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Dependency {
    /// Fully qualified pinned OCX identifier with required explicit registry
    /// and digest. The tag portion is advisory (for update tooling) — only
    /// the digest is used for resolution.
    pub identifier: oci::PinnedIdentifier,

    /// Controls how this dependency's environment variables propagate.
    /// Default: `Sealed` — no env contribution. See [`Visibility`] for the
    /// four levels and their semantics.
    #[serde(default)]
    pub visibility: Visibility,
}

/// Ordered list of package dependencies.
///
/// Serializes as a JSON array. Array position defines the canonical
/// environment import order. This avoids relying on JSON object key
/// ordering, which is unordered per RFC 8259 and not preserved by
/// all parsers (e.g., Go encoding/json, jq).
///
/// Deserialization validates that each identifier contains an explicit
/// registry (no default registry fallback) and that no repository
/// appears more than once.
#[derive(Debug, Clone, Default)]
pub struct Dependencies {
    entries: Vec<Dependency>,
}

impl Dependencies {
    pub fn new(entries: Vec<Dependency>) -> Result<Self, DuplicateDependencyError> {
        let mut seen = HashSet::new();
        for dep in &entries {
            let key = (
                dep.identifier.registry().to_string(),
                dep.identifier.repository().to_string(),
            );
            if !seen.insert(key) {
                return Err(DuplicateDependencyError {
                    identifier: dep.identifier.to_string(),
                });
            }
        }
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Dependency> {
        self.entries.iter()
    }
}

impl<'a> IntoIterator for &'a Dependencies {
    type Item = &'a Dependency;
    type IntoIter = std::slice::Iter<'a, Dependency>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl Serialize for Dependencies {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.entries.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Dependencies {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = Vec::<Dependency>::deserialize(deserializer)?;
        Dependencies::new(entries).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for Dependencies {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Dependencies")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<Dependency>>::json_schema(generator)
    }
}

/// Error returned when a dependency identifier appears more than once.
#[derive(Debug, thiserror::Error)]
#[error("duplicate dependency identifier: '{identifier}'")]
pub struct DuplicateDependencyError {
    pub identifier: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha256_hex() -> String {
        "a".repeat(64)
    }

    fn make_digest() -> oci::Digest {
        oci::Digest::Sha256(sha256_hex())
    }

    // ── Dependency serde ──────────────────────────────────────────────

    #[test]
    fn dependency_roundtrip() {
        let json = format!(r#"{{"identifier":"ocx.sh/java:21@sha256:{}"}}"#, sha256_hex());
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.identifier.registry(), "ocx.sh");
        assert_eq!(dep.identifier.repository(), "java");
        assert_eq!(dep.identifier.tag(), Some("21"));
        assert_eq!(dep.identifier.digest(), make_digest());
        assert_eq!(dep.visibility, Visibility::Sealed);

        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep.identifier, dep2.identifier);
    }

    #[test]
    fn dependency_visibility_public_roundtrip() {
        let json = format!(
            r#"{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"public"}}"#,
            sha256_hex()
        );
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::Public);

        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep2.visibility, Visibility::Public);
    }

    #[test]
    fn dependency_visibility_sealed_roundtrip() {
        let json = format!(
            r#"{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"sealed"}}"#,
            sha256_hex()
        );
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::Sealed);
    }

    #[test]
    fn dependency_visibility_private_roundtrip() {
        let json = format!(
            r#"{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"private"}}"#,
            sha256_hex()
        );
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::Private);

        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep2.visibility, Visibility::Private);
    }

    #[test]
    fn dependency_visibility_interface_roundtrip() {
        let json = format!(
            r#"{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"interface"}}"#,
            sha256_hex()
        );
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::Interface);

        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep2.visibility, Visibility::Interface);
    }

    #[test]
    fn dependency_omitted_visibility_defaults_to_sealed() {
        let json = format!(r#"{{"identifier":"ocx.sh/java:21@sha256:{}"}}"#, sha256_hex());
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::Sealed);
    }

    #[test]
    fn dependency_rejects_bare_name() {
        let json = format!(r#"{{"identifier":"cmake:3.28@sha256:{}"}}"#, sha256_hex());
        let err = serde_json::from_str::<Dependency>(&json).unwrap_err();
        assert!(err.to_string().contains("explicit registry"));
    }

    #[test]
    fn dependency_rejects_org_repo_without_registry() {
        let json = format!(r#"{{"identifier":"myorg/cmake:3.28@sha256:{}"}}"#, sha256_hex());
        let err = serde_json::from_str::<Dependency>(&json).unwrap_err();
        assert!(err.to_string().contains("explicit registry"));
    }

    #[test]
    fn dependency_accepts_localhost() {
        let json = format!(r#"{{"identifier":"localhost/repo:tag@sha256:{}"}}"#, sha256_hex());
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.identifier.registry(), "localhost");
    }

    #[test]
    fn dependency_accepts_registry_with_port() {
        let json = format!(r#"{{"identifier":"localhost:5000/repo:tag@sha256:{}"}}"#, sha256_hex());
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.identifier.registry(), "localhost:5000");
    }

    #[test]
    fn dependency_rejects_missing_digest() {
        let json = r#"{"identifier":"ocx.sh/java:21"}"#;
        let err = serde_json::from_str::<Dependency>(json).unwrap_err();
        assert!(err.to_string().contains("digest"));
    }

    #[test]
    fn dependency_without_tag() {
        let json = format!(r#"{{"identifier":"ocx.sh/java@sha256:{}"}}"#, sha256_hex());
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.identifier.tag(), None);
    }

    // ── Dependencies serde ────────────────────────────────────────────

    #[test]
    fn dependencies_empty_array() {
        let json = "[]";
        let deps: Dependencies = serde_json::from_str(json).unwrap();
        assert!(deps.is_empty());
        assert_eq!(deps.len(), 0);
    }

    #[test]
    fn dependencies_preserves_order() {
        let hex = sha256_hex();
        let hex2 = "b".repeat(64);
        let json = format!(
            r#"[
                {{"identifier":"ocx.sh/java:21@sha256:{hex}"}},
                {{"identifier":"ocx.sh/cmake:3.28@sha256:{hex2}"}}
            ]"#
        );
        let deps: Dependencies = serde_json::from_str(&json).unwrap();
        assert_eq!(deps.len(), 2);
        let items: Vec<_> = deps.iter().collect();
        assert_eq!(items[0].identifier.repository(), "java");
        assert_eq!(items[1].identifier.repository(), "cmake");
    }

    #[test]
    fn dependencies_rejects_duplicate_identifier() {
        let hex = sha256_hex();
        let hex2 = "b".repeat(64);
        let json = format!(
            r#"[
                {{"identifier":"ocx.sh/java:21@sha256:{hex}"}},
                {{"identifier":"ocx.sh/java:22@sha256:{hex2}"}}
            ]"#
        );
        let err = serde_json::from_str::<Dependencies>(&json).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn dependencies_allows_same_repo_different_registry() {
        let hex = sha256_hex();
        let hex2 = "b".repeat(64);
        let json = format!(
            r#"[
                {{"identifier":"ocx.sh/java:21@sha256:{hex}"}},
                {{"identifier":"ghcr.io/java:21@sha256:{hex2}"}}
            ]"#
        );
        let deps: Dependencies = serde_json::from_str(&json).unwrap();
        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn dependencies_roundtrip() {
        let hex = sha256_hex();
        let hex2 = "b".repeat(64);
        let json = format!(
            r#"[
                {{"identifier":"ocx.sh/java:21@sha256:{hex}"}},
                {{"identifier":"ocx.sh/cmake:3.28@sha256:{hex2}"}}
            ]"#
        );
        let deps: Dependencies = serde_json::from_str(&json).unwrap();
        let reserialized = serde_json::to_string(&deps).unwrap();
        let deps2: Dependencies = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(deps.len(), deps2.len());
        for (a, b) in deps.iter().zip(deps2.iter()) {
            assert_eq!(a.identifier, b.identifier);
        }
    }

    // ── Bundle backward compatibility ─────────────────────────────────

    #[test]
    fn bundle_without_dependencies_deserializes() {
        let json = r#"{"type":"bundle","version":1}"#;
        let metadata: crate::package::metadata::Metadata = serde_json::from_str(json).unwrap();
        assert!(metadata.dependencies().is_empty());
    }

    #[test]
    fn bundle_with_dependencies_roundtrip() {
        let hex = sha256_hex();
        let json = format!(
            r#"{{
                "type": "bundle",
                "version": 1,
                "dependencies": [
                    {{"identifier": "ocx.sh/java:21@sha256:{hex}", "visibility": "public"}}
                ]
            }}"#
        );
        let metadata: crate::package::metadata::Metadata = serde_json::from_str(&json).unwrap();
        assert_eq!(metadata.dependencies().len(), 1);
        let dep = metadata.dependencies().iter().next().unwrap();
        assert_eq!(dep.identifier.repository(), "java");
        assert_eq!(dep.visibility, Visibility::Public);

        let reserialized = serde_json::to_string_pretty(&metadata).unwrap();
        let metadata2: crate::package::metadata::Metadata = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(metadata2.dependencies().len(), 1);
        assert_eq!(
            metadata2.dependencies().iter().next().unwrap().visibility,
            Visibility::Public,
        );
    }

    #[test]
    fn bundle_empty_dependencies_not_serialized() {
        let json = r#"{"type":"bundle","version":1}"#;
        let metadata: crate::package::metadata::Metadata = serde_json::from_str(json).unwrap();
        let reserialized = serde_json::to_string(&metadata).unwrap();
        assert!(!reserialized.contains("dependencies"));
    }

    #[test]
    fn bundle_with_env_and_dependencies() {
        let hex = sha256_hex();
        let json = format!(
            r#"{{
                "type": "bundle",
                "version": 1,
                "env": [
                    {{"key": "PATH", "type": "path", "value": "${{installPath}}/bin", "required": true}}
                ],
                "dependencies": [
                    {{"identifier": "ocx.sh/java:21@sha256:{hex}", "visibility": "public"}}
                ]
            }}"#
        );
        let metadata: crate::package::metadata::Metadata = serde_json::from_str(&json).unwrap();
        assert!(!metadata.env().unwrap().is_empty());
        assert_eq!(metadata.dependencies().len(), 1);
        assert_eq!(
            metadata.dependencies().iter().next().unwrap().visibility,
            Visibility::Public,
        );
    }
}
