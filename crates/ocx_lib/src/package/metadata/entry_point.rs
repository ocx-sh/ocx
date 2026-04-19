// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

static NAME_PATTERN: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^[a-z0-9][a-z0-9_-]*$").expect("valid regex"));

/// A validated entry point name.
///
/// Must match `^[a-z0-9][a-z0-9_-]*$` and be at most
/// [`EntryPointName::MAX_LEN`] bytes. Enforced at construction and
/// deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EntryPointName(String);

impl EntryPointName {
    /// Maximum byte length of an entry point name.
    ///
    /// Caps publisher-supplied names so generated launcher filenames stay
    /// well under platform path limits (Windows `MAX_PATH = 260`, including
    /// the `.cmd` suffix and the surrounding install directory). 64 chars is
    /// generous for human-readable command names while leaving headroom.
    pub const MAX_LEN: usize = 64;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EntryPointName {
    type Error = EntryPointError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > Self::MAX_LEN {
            return Err(EntryPointError::InvalidName { name: value });
        }
        if !NAME_PATTERN.is_match(&value) {
            return Err(EntryPointError::InvalidName { name: value });
        }
        Ok(EntryPointName(value))
    }
}

impl TryFrom<&str> for EntryPointName {
    type Error = EntryPointError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        EntryPointName::try_from(value.to_string())
    }
}

impl std::str::FromStr for EntryPointName {
    type Err = EntryPointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EntryPointName::try_from(s.to_string())
    }
}

impl std::fmt::Display for EntryPointName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> Deserialize<'de> for EntryPointName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        EntryPointName::try_from(s).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for EntryPointName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("EntryPointName")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Entry point name for invocation by users. Must match ^[a-z0-9][a-z0-9_-]*$ and be at most 64 characters.",
            "pattern": "^[a-z0-9][a-z0-9_-]*$",
            "maxLength": 64
        })
    }
}

/// A single named entry point for a package.
///
/// Declares a named launcher that `ocx install` generates at install time.
/// The launcher calls `ocx exec --install-dir=<content-path> -- "$@"`, preserving
/// clean-env execution semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EntryPoint {
    /// The name users will invoke (e.g. `"cmake"`). Must match `^[a-z0-9][a-z0-9_-]*$`.
    pub name: EntryPointName,

    /// Template string for the binary target within the package content.
    ///
    /// Validated at publish time to resolve under `${installPath}` or
    /// `${deps.NAME.installPath}`. Example: `"${installPath}/bin/cmake"`.
    pub target: String,
}

/// Ordered, uniqueness-validated list of entry points for a package.
///
/// Serializes as a JSON array (transparent wrapper over `Vec<EntryPoint>`).
/// `#[serde(default)]` means an absent `entry_points` field deserializes
/// to an empty list; `skip_serializing_if = "EntryPoints::is_empty"` means an
/// empty list is omitted on serialization (additive-optional, forward-compat).
///
/// Deserialization enforces uniqueness via [`EntryPoints::new`] — duplicate
/// names are rejected with a descriptive error.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct EntryPoints {
    entries: Vec<EntryPoint>,
}

impl<'de> Deserialize<'de> for EntryPoints {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = Vec::<EntryPoint>::deserialize(deserializer)?;
        EntryPoints::new(entries).map_err(serde::de::Error::custom)
    }
}

impl EntryPoints {
    /// Constructs a validated `EntryPoints`, rejecting duplicate names.
    ///
    /// # Errors
    ///
    /// - [`EntryPointError::InvalidName`] if any name fails the slug regex (returned
    ///   by deserialization of individual [`EntryPointName`]s, surfaced here on
    ///   programmatic construction).
    /// - [`EntryPointError::DuplicateName`] if two entries share the same name.
    pub fn new(entries: Vec<EntryPoint>) -> Result<Self, EntryPointError> {
        let mut seen: HashSet<String> = HashSet::new();
        for entry in &entries {
            if !seen.insert(entry.name.0.clone()) {
                return Err(EntryPointError::DuplicateName {
                    name: entry.name.0.clone(),
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

    pub fn iter(&self) -> impl Iterator<Item = &EntryPoint> {
        self.entries.iter()
    }
}

/// Errors that can occur when constructing or validating [`EntryPoints`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EntryPointError {
    /// An entry point name fails the slug regex `^[a-z0-9][a-z0-9_-]*$`.
    #[error("invalid entry point name '{name}': must match ^[a-z0-9][a-z0-9_-]*$")]
    InvalidName { name: String },
    /// Two entry points share the same name.
    #[error("duplicate entry point name '{name}'")]
    DuplicateName { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 3.1 EntryPointName slug validation ─────────────────────────────────

    #[test]
    fn name_accepts_simple_lowercase() {
        assert!(EntryPointName::try_from("cmake").is_ok());
    }

    #[test]
    fn name_accepts_alphanumeric_with_dash_underscore() {
        assert!(EntryPointName::try_from("a1_2-3").is_ok());
        assert!(EntryPointName::try_from("ctest-2").is_ok());
        assert!(EntryPointName::try_from("gcc12").is_ok());
    }

    #[test]
    fn name_rejects_empty() {
        let err = EntryPointName::try_from("").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_uppercase() {
        let err = EntryPointName::try_from("Cmake").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
        let err = EntryPointName::try_from("CMAKE").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_leading_digit() {
        // Slug pattern requires starting char to be [a-z0-9] but the regex
        // is ^[a-z0-9][a-z0-9_-]*$ which DOES allow a leading digit.
        // ADR §1 says "slug regex reused from dependency.rs:12-13" which is
        // ^[a-z0-9][a-z0-9_-]*$. So leading digit IS allowed per the ADR contract.
        // This test confirms leading-digit names are ACCEPTED (not rejected).
        assert!(EntryPointName::try_from("1abc").is_ok());
    }

    #[test]
    fn name_rejects_leading_underscore() {
        // Underscore is not in the leading character class [a-z0-9]
        let err = EntryPointName::try_from("_cmake").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_leading_dash() {
        // Dash is not in the leading character class [a-z0-9]
        let err = EntryPointName::try_from("-cmake").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_path_traversal() {
        let err = EntryPointName::try_from("../../bin/sh").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_slash() {
        let err = EntryPointName::try_from("bin/cmake").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_unicode() {
        let err = EntryPointName::try_from("cmaké").unwrap_err();
        assert!(matches!(err, EntryPointError::InvalidName { .. }));
    }

    #[test]
    fn name_accepts_64_char_slug() {
        // Plan §3.13 (F.3): names exactly at `MAX_LEN = 64` must remain accepted.
        let at_cap: String = "a".repeat(EntryPointName::MAX_LEN);
        assert!(
            EntryPointName::try_from(at_cap.as_str()).is_ok(),
            "64-char slug at the cap must be accepted",
        );
    }

    #[test]
    fn name_rejects_70_char_slug_at_boundary() {
        // Plan §3.13 (F.3): names longer than the documented `MAX_LEN = 64` cap
        // must be rejected. A 70-character all-`a` slug exceeds the cap and
        // currently passes (no length check); after F.3 lands, this assertion
        // holds.
        let long_name: String = "a".repeat(70);
        let err = EntryPointName::try_from(long_name.as_str()).unwrap_err();
        assert!(
            matches!(err, EntryPointError::InvalidName { .. }),
            "70-char slug must be rejected as InvalidName, got: {err:?}",
        );
    }

    /// Plan §3.13 (F.3): boundary test at MAX_LEN+1 = 65. A 64-char name must
    /// remain accepted; the first character past the cap (65 chars) must be
    /// rejected. Currently no length cap exists, so 65 chars is accepted —
    /// this test fails until F.3 introduces the cap.
    #[test]
    fn name_rejects_65_char_slug() {
        let at_cap: String = "a".repeat(64);
        assert!(
            EntryPointName::try_from(at_cap.as_str()).is_ok(),
            "64-char slug at the cap must remain accepted",
        );
        let over_cap: String = "a".repeat(65);
        let err = EntryPointName::try_from(over_cap.as_str()).expect_err("65-char slug must be rejected");
        assert!(
            matches!(err, EntryPointError::InvalidName { .. }),
            "65-char slug must surface as InvalidName, got: {err:?}",
        );
    }

    // ── 3.1 EntryPoints::new duplicate-name uniqueness ─────────────────────

    #[test]
    fn entry_points_new_accepts_unique_names() {
        let entries = vec![
            EntryPoint {
                name: EntryPointName::try_from("cmake").unwrap(),
                target: "${installPath}/bin/cmake".to_string(),
            },
            EntryPoint {
                name: EntryPointName::try_from("ctest").unwrap(),
                target: "${installPath}/bin/ctest".to_string(),
            },
        ];
        assert!(EntryPoints::new(entries).is_ok());
    }

    #[test]
    fn entry_points_new_rejects_duplicate_name() {
        let entries = vec![
            EntryPoint {
                name: EntryPointName::try_from("cmake").unwrap(),
                target: "${installPath}/bin/cmake".to_string(),
            },
            EntryPoint {
                name: EntryPointName::try_from("cmake").unwrap(),
                target: "${installPath}/bin/cmake2".to_string(),
            },
        ];
        let err = EntryPoints::new(entries).unwrap_err();
        assert!(matches!(err, EntryPointError::DuplicateName { name } if name == "cmake"));
    }

    #[test]
    fn entry_points_new_accepts_empty() {
        let ep = EntryPoints::new(vec![]).unwrap();
        assert!(ep.is_empty());
    }

    // ── 3.1 EntryPoint serde round-trip ────────────────────────────────────

    #[test]
    fn entry_point_round_trip_via_serde() {
        let json = r#"{"name":"cmake","target":"${installPath}/bin/cmake"}"#;
        let ep: EntryPoint = serde_json::from_str(json).unwrap();
        assert_eq!(ep.name.as_str(), "cmake");
        assert_eq!(ep.target, "${installPath}/bin/cmake");
        let back = serde_json::to_string(&ep).unwrap();
        assert_eq!(back, json);
    }

    #[test]
    fn entry_point_name_deserialization_rejects_invalid() {
        let json = r#"{"name":"","target":"${installPath}/bin/cmake"}"#;
        assert!(serde_json::from_str::<EntryPoint>(json).is_err());
    }

    // ── 3.1 EntryPoints::is_empty ──────────────────────────────────────────

    #[test]
    fn entry_points_is_empty_for_default() {
        let ep = EntryPoints::default();
        assert!(ep.is_empty());
    }

    #[test]
    fn entry_points_is_not_empty_when_populated() {
        let entries = vec![EntryPoint {
            name: EntryPointName::try_from("cmake").unwrap(),
            target: "${installPath}/bin/cmake".to_string(),
        }];
        let ep = EntryPoints::new(entries).unwrap();
        assert!(!ep.is_empty());
    }

    // ── 3.1 Bundle TOML/JSON round-trips with entry_points ─────────────────

    #[test]
    fn bundle_without_entry_points_round_trips() {
        // Old JSON without entry_points — must deserialize and default to empty.
        let json = r#"{"version":1}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(bundle.entry_points.is_empty());
        // When serializing with empty entry_points, the field is skipped.
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(!serialized.contains("entry_points"));
    }

    #[test]
    fn bundle_with_empty_entry_points_skip_serialized() {
        // Explicit empty array → serialized without the field.
        let json = r#"{"version":1,"entry_points":[]}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(bundle.entry_points.is_empty());
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(
            !serialized.contains("entry_points"),
            "empty entry_points should be skipped: {serialized}"
        );
    }

    #[test]
    fn bundle_with_populated_entry_points_round_trips() {
        let json = r#"{"version":1,"entry_points":[{"name":"cmake","target":"${installPath}/bin/cmake"}]}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(!bundle.entry_points.is_empty());
        assert_eq!(bundle.entry_points.iter().next().unwrap().name.as_str(), "cmake");
        // Serialized back must include entry_points.
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(
            serialized.contains("entry_points"),
            "populated entry_points must serialize: {serialized}"
        );
        assert!(serialized.contains("cmake"));
    }

    #[test]
    fn entry_points_deserialization_error_surfaces_for_duplicate_names() {
        // Two entries with the same name — custom Deserialize calls EntryPoints::new()
        // which enforces uniqueness, so duplicate names are rejected at serde time.
        let json = r#"[{"name":"cmake","target":"bin/cmake"},{"name":"cmake","target":"bin/cmake2"}]"#;
        let result: Result<EntryPoints, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "duplicate entry point names must be rejected during deserialization"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cmake") || msg.contains("duplicate"),
            "error must mention 'cmake' or 'duplicate': {msg}"
        );
    }
}
