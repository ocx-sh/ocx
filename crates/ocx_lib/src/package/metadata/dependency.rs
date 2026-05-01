// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::oci;

use super::slug::{SLUG_MAX_LEN, SLUG_PATTERN, SLUG_PATTERN_STR};
use super::visibility::Visibility;

/// A validated dependency name used for `${deps.NAME.field}` interpolation.
///
/// Must match `^[a-z0-9][a-z0-9_-]*$` and be at most [`SLUG_MAX_LEN`] bytes.
/// Enforced at construction and deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DependencyName(String);

impl DependencyName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DependencyName {
    type Error = DependencyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > SLUG_MAX_LEN || !SLUG_PATTERN.is_match(&value) {
            return Err(DependencyError::InvalidName { name: value });
        }
        Ok(DependencyName(value))
    }
}

impl TryFrom<&str> for DependencyName {
    type Error = DependencyError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        DependencyName::try_from(value.to_string())
    }
}

impl std::fmt::Display for DependencyName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::borrow::Borrow<str> for DependencyName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DependencyName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        DependencyName::try_from(s).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for DependencyName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DependencyName")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Interpolation name for this dependency. Must match ^[a-z0-9][a-z0-9_-]*$ (max 64 chars).",
            "pattern": SLUG_PATTERN_STR,
            "maxLength": SLUG_MAX_LEN
        })
    }
}

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

    /// Optional name for this dependency used in `${deps.NAME.installPath}` interpolation.
    ///
    /// When set, overrides the default name (last path segment of the OCI repository).
    /// Use to resolve collisions when two dependencies share the same repository basename.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<DependencyName>,
}

impl Dependency {
    /// Returns the interpolation name for this dependency.
    ///
    /// Returns the explicit `name` when set; otherwise the last path segment of the OCI
    /// repository (e.g. `"cmake"` for `"myorg/cmake"`).
    pub fn name(&self) -> DependencyName {
        if let Some(n) = &self.name {
            return n.clone();
        }
        let basename = self.identifier.name();
        // SAFETY-invariant: OCI repository basenames are already guaranteed by the OCI
        // identifier parser to be lowercase alphanumeric with dashes/underscores — they
        // satisfy the slug pattern by construction.
        DependencyName::try_from(basename).expect("repository basename is a valid slug")
    }
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
    pub fn new(entries: Vec<Dependency>) -> Result<Self, DependencyError> {
        let mut seen_ids = HashSet::new();
        let mut seen_names: HashSet<DependencyName> = HashSet::new();
        for dep in &entries {
            if let Some(name) = &dep.name
                && !seen_names.insert(name.clone())
            {
                return Err(DependencyError::DuplicateName { name: name.to_string() });
            }
            // Validate unique (registry, repository).
            let key = (
                dep.identifier.registry().to_string(),
                dep.identifier.repository().to_string(),
            );
            if !seen_ids.insert(key) {
                return Err(DependencyError::DuplicateIdentifier {
                    identifier: dep.identifier.clone(),
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

/// Errors that can occur when constructing or validating [`Dependencies`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DependencyError {
    /// A dependency identifier appears more than once.
    #[error("duplicate dependency identifier: '{identifier}'")]
    DuplicateIdentifier { identifier: oci::PinnedIdentifier },
    /// A name is not a valid slug (`^[a-z0-9][a-z0-9_-]*$`, max 64 chars).
    #[error("invalid dependency name '{name}': must match ^[a-z0-9][a-z0-9_-]*$ (max 64 chars)")]
    InvalidName { name: String },
    /// Two dependencies share the same explicit name.
    #[error("duplicate dependency name '{name}'")]
    DuplicateName { name: String },
}

impl schemars::JsonSchema for Dependencies {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Dependencies")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<Dependency>>::json_schema(generator)
    }
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
        assert_eq!(dep.visibility, Visibility::SEALED);

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
        assert_eq!(dep.visibility, Visibility::PUBLIC);

        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep2.visibility, Visibility::PUBLIC);
    }

    #[test]
    fn dependency_visibility_sealed_roundtrip() {
        let json = format!(
            r#"{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"sealed"}}"#,
            sha256_hex()
        );
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::SEALED);
    }

    #[test]
    fn dependency_visibility_private_roundtrip() {
        let json = format!(
            r#"{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"private"}}"#,
            sha256_hex()
        );
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::PRIVATE);

        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep2.visibility, Visibility::PRIVATE);
    }

    #[test]
    fn dependency_visibility_interface_roundtrip() {
        let json = format!(
            r#"{{"identifier":"ocx.sh/java:21@sha256:{}","visibility":"interface"}}"#,
            sha256_hex()
        );
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::INTERFACE);

        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep2.visibility, Visibility::INTERFACE);
    }

    #[test]
    fn dependency_omitted_visibility_defaults_to_sealed() {
        let json = format!(r#"{{"identifier":"ocx.sh/java:21@sha256:{}"}}"#, sha256_hex());
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(dep.visibility, Visibility::SEALED);
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
        assert_eq!(dep.visibility, Visibility::PUBLIC);

        let reserialized = serde_json::to_string_pretty(&metadata).unwrap();
        let metadata2: crate::package::metadata::Metadata = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(metadata2.dependencies().len(), 1);
        assert_eq!(
            metadata2.dependencies().iter().next().unwrap().visibility,
            Visibility::PUBLIC,
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
            Visibility::PUBLIC,
        );
    }

    // ── DependencyName + name() — Phase 3.2 spec tests ────────────────────────

    fn make_dep(repo: &str, name: Option<&str>) -> Dependency {
        let hex = sha256_hex();
        let json = match name {
            Some(n) => format!(r#"{{"identifier":"ocx.sh/{repo}:1@sha256:{hex}","name":"{n}"}}"#),
            None => format!(r#"{{"identifier":"ocx.sh/{repo}:1@sha256:{hex}"}}"#),
        };
        serde_json::from_str(&json).unwrap()
    }

    #[test]
    fn name_returns_explicit_name_when_set() {
        let dep = make_dep("myorg/cmake", Some("my-cmake"));
        assert_eq!(dep.name().as_str(), "my-cmake");
    }

    #[test]
    fn name_returns_repository_basename_when_no_name() {
        let dep = make_dep("myorg/cmake", None);
        assert_eq!(dep.name().as_str(), "cmake");
    }

    #[test]
    fn name_returns_single_segment_repo_basename() {
        let dep = make_dep("python", None);
        assert_eq!(dep.name().as_str(), "python");
    }

    #[test]
    fn name_valid_forms() {
        for n in &["foo", "my-dep", "dep-1", "dep_1", "a0"] {
            let dep = make_dep("cmake", Some(n));
            assert_eq!(dep.name().as_str(), *n);
        }
    }

    fn dep_with_name(repo: &str, name: &str) -> Result<Dependency, serde_json::Error> {
        let hex = sha256_hex();
        let json = format!(r#"{{"identifier":"ocx.sh/{repo}:1@sha256:{hex}","name":"{name}"}}"#);
        serde_json::from_str::<Dependency>(&json)
    }

    fn deps_with_name(repo: &str, name: &str) -> Result<Dependencies, serde_json::Error> {
        let hex = sha256_hex();
        let json = format!(r#"[{{"identifier":"ocx.sh/{repo}:1@sha256:{hex}","name":"{name}"}}]"#);
        serde_json::from_str::<Dependencies>(&json)
    }

    #[test]
    fn name_invalid_empty_rejected() {
        // Validation fires at Dependency deserialization (DependencyName newtype), not only at Dependencies::new().
        let err = dep_with_name("cmake", "").unwrap_err();
        assert!(err.to_string().contains("name"), "expected name error, got: {err}");
        let err = deps_with_name("cmake", "").unwrap_err();
        assert!(err.to_string().contains("name"), "expected name error, got: {err}");
    }

    #[test]
    fn name_invalid_uppercase_rejected() {
        let err = dep_with_name("cmake", "Cmake").unwrap_err();
        assert!(err.to_string().contains("name"), "expected name error, got: {err}");
        let err = deps_with_name("cmake", "Cmake").unwrap_err();
        assert!(err.to_string().contains("name"), "expected name error, got: {err}");
    }

    #[test]
    fn name_invalid_slash_rejected() {
        let err = deps_with_name("cmake", "my/name").unwrap_err();
        assert!(err.to_string().contains("name"), "expected name error, got: {err}");
    }

    #[test]
    fn name_invalid_too_long_rejected() {
        let long_name = "a".repeat(65);
        let err = dep_with_name("cmake", &long_name).unwrap_err();
        assert!(
            err.to_string().contains("name"),
            "expected name error for 65-char name, got: {err}"
        );
    }

    #[test]
    fn name_valid_max_length_accepted() {
        let at_cap = "a".repeat(64);
        assert!(dep_with_name("cmake", &at_cap).is_ok(), "64-char name must be accepted");
    }

    #[test]
    fn duplicate_name_rejected() {
        let hex = sha256_hex();
        let hex2 = "b".repeat(64);
        let json = format!(
            r#"[
                {{"identifier":"ocx.sh/cmake:1@sha256:{hex}","name":"tool"}},
                {{"identifier":"ocx.sh/ninja:1@sha256:{hex2}","name":"tool"}}
            ]"#
        );
        let err = serde_json::from_str::<Dependencies>(&json).unwrap_err();
        assert!(
            err.to_string().contains("name") || err.to_string().contains("duplicate"),
            "expected duplicate name error, got: {err}"
        );
    }

    #[test]
    fn name_roundtrips_through_serde() {
        let hex = sha256_hex();
        let json = format!(r#"{{"identifier":"ocx.sh/cmake:1@sha256:{hex}","name":"my-cmake"}}"#);
        let dep: Dependency = serde_json::from_str(&json).unwrap();
        let reserialized = serde_json::to_string(&dep).unwrap();
        let dep2: Dependency = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(dep2.name.as_ref().map(DependencyName::as_str), Some("my-cmake"));
    }

    #[test]
    fn no_name_not_serialized() {
        let dep = make_dep("cmake", None);
        let json = serde_json::to_string(&dep).unwrap();
        assert!(!json.contains("\"name\""), "name should not appear when None: {json}");
    }
}
