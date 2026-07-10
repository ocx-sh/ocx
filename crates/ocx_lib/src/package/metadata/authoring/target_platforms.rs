// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use crate::oci::Platform;

/// The package's target-platform set embedded by `ocx package create`.
///
/// Sidecar-only field: it never appears in published metadata (the published
/// [`Bundle`](crate::package::metadata::bundle::Bundle) has no such field, so
/// it is stripped by construction when projecting). `ocx package push` reads
/// it to decide which platforms to fan out to.
///
/// Serialized as a non-empty JSON array of canonical platform strings
/// (e.g. `["linux/amd64", "any"]`). Duplicates are rejected on deserialize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPlatforms(Vec<Platform>);

impl TargetPlatforms {
    /// Builds a target set from a non-empty, duplicate-free platform list.
    pub fn new(platforms: Vec<Platform>) -> Result<Self, TargetPlatformsError> {
        if platforms.is_empty() {
            return Err(TargetPlatformsError::Empty);
        }
        let mut seen = std::collections::HashSet::new();
        for platform in &platforms {
            if !seen.insert(platform.clone()) {
                return Err(TargetPlatformsError::Duplicate {
                    platform: platform.to_string(),
                });
            }
        }
        Ok(Self(platforms))
    }

    /// The first platform in the set (the set is non-empty by construction).
    pub fn first(&self) -> &Platform {
        self.0.first().expect("TargetPlatforms is non-empty by construction")
    }

    pub fn contains(&self, platform: &Platform) -> bool {
        self.0.contains(platform)
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Platform> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        // Non-empty by construction; kept for API symmetry with collections.
        self.0.is_empty()
    }

    pub fn as_slice(&self) -> &[Platform] {
        &self.0
    }
}

impl<'a> IntoIterator for &'a TargetPlatforms {
    type Item = &'a Platform;
    type IntoIter = std::slice::Iter<'a, Platform>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

/// Errors constructing a [`TargetPlatforms`] set.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TargetPlatformsError {
    /// The platform set is empty.
    #[error("target platform set must not be empty")]
    Empty,
    /// A platform appears more than once.
    #[error("duplicate target platform '{platform}'")]
    Duplicate { platform: String },
}

impl Serialize for TargetPlatforms {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(self.0.iter().map(|platform| platform.to_string()))
    }
}

impl<'de> Deserialize<'de> for TargetPlatforms {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = Vec::<String>::deserialize(deserializer)?;
        let platforms = raw
            .iter()
            .map(|value| value.parse::<Platform>().map_err(serde::de::Error::custom))
            .collect::<Result<Vec<_>, _>>()?;
        TargetPlatforms::new(platforms).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for TargetPlatforms {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("TargetPlatforms")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "array",
            "description": "Target-platform set written by `ocx package create` (authoring sidecar only; stripped at publish). Non-empty array of canonical platform strings such as 'linux/amd64' or 'any'.",
            "items": { "type": "string" },
            "minItems": 1
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_platform_strings() {
        let json = r#"["linux/amd64", "any"]"#;
        let set: TargetPlatforms = serde_json::from_str(json).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains(&Platform::any()));
        assert!(set.contains(&"linux/amd64".parse().unwrap()));

        let reserialized = serde_json::to_string(&set).unwrap();
        let set2: TargetPlatforms = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(set, set2);
    }

    #[test]
    fn rejects_empty_set() {
        let err = serde_json::from_str::<TargetPlatforms>("[]").unwrap_err();
        assert!(err.to_string().contains("empty"), "unexpected: {err}");
    }

    #[test]
    fn rejects_duplicates() {
        let err = serde_json::from_str::<TargetPlatforms>(r#"["any", "any"]"#).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "unexpected: {err}");
    }

    #[test]
    fn rejects_invalid_platform() {
        let err = serde_json::from_str::<TargetPlatforms>(r#"["not-a/real/platform/x/y"]"#).unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn feature_bearing_platform_roundtrips() {
        let json = r#"["linux/amd64+libc.glibc"]"#;
        let set: TargetPlatforms = serde_json::from_str(json).unwrap();
        let reserialized = serde_json::to_string(&set).unwrap();
        let set2: TargetPlatforms = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(set, set2);
    }
}
