// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::slug::{SLUG_MAX_LEN, SLUG_PATTERN, SLUG_PATTERN_STR};

/// A validated entrypoint name.
///
/// Must match `^[a-z0-9][a-z0-9_-]*$` and be at most
/// [`EntrypointName::MAX_LEN`] bytes. Enforced at construction and
/// deserialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EntrypointName(String);

impl EntrypointName {
    /// Maximum byte length of an entrypoint name.
    ///
    /// Caps publisher-supplied names so generated launcher filenames stay
    /// well under platform path limits (Windows `MAX_PATH = 260`, including
    /// the `.cmd` suffix and the surrounding install directory). 64 chars is
    /// generous for human-readable command names while leaving headroom.
    ///
    /// Mirrors [`slug::SLUG_MAX_LEN`] — both newtypes share the same upper bound.
    pub const MAX_LEN: usize = SLUG_MAX_LEN;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EntrypointName {
    type Error = EntrypointError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() > Self::MAX_LEN {
            return Err(EntrypointError::InvalidName { name: value });
        }
        if !SLUG_PATTERN.is_match(&value) {
            return Err(EntrypointError::InvalidName { name: value });
        }
        Ok(EntrypointName(value))
    }
}

impl TryFrom<&str> for EntrypointName {
    type Error = EntrypointError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        EntrypointName::try_from(value.to_string())
    }
}

impl std::str::FromStr for EntrypointName {
    type Err = EntrypointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EntrypointName::try_from(s.to_string())
    }
}

impl std::fmt::Display for EntrypointName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<'de> Deserialize<'de> for EntrypointName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        EntrypointName::try_from(s).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for EntrypointName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("EntrypointName")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Entrypoint name for invocation by users. Must match ^[a-z0-9][a-z0-9_-]*$ and be at most 64 characters.",
            "pattern": SLUG_PATTERN_STR,
            "maxLength": SLUG_MAX_LEN
        })
    }
}

/// A single named entrypoint for a package.
///
/// Declares a named launcher that `ocx install` generates at install time.
/// The launcher calls `ocx exec --install-dir=<content-path> -- "$@"`, preserving
/// clean-env execution semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Entrypoint {
    /// The name users will invoke (e.g. `"cmake"`). Must match `^[a-z0-9][a-z0-9_-]*$`.
    pub name: EntrypointName,

    /// Template string for the binary target within the package content.
    ///
    /// Validated at publish time to resolve under `${installPath}` or
    /// `${deps.NAME.installPath}`. Example: `"${installPath}/bin/cmake"`.
    pub target: String,
}

/// Ordered, uniqueness-validated list of entrypoints for a package.
///
/// Serializes as a JSON array (transparent wrapper over `Vec<Entrypoint>`).
/// `#[serde(default)]` means an absent `entrypoints` field deserializes
/// to an empty list; `skip_serializing_if = "Entrypoints::is_empty"` means an
/// empty list is omitted on serialization (additive-optional, forward-compat).
///
/// Deserialization enforces uniqueness via [`Entrypoints::new`] — duplicate
/// names are rejected with a descriptive error.
#[derive(Debug, Clone, Default, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct Entrypoints {
    entries: Vec<Entrypoint>,
}

impl<'de> Deserialize<'de> for Entrypoints {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = Vec::<Entrypoint>::deserialize(deserializer)?;
        Entrypoints::new(entries).map_err(serde::de::Error::custom)
    }
}

impl Entrypoints {
    /// Constructs a validated `Entrypoints`, rejecting duplicate names.
    ///
    /// # Errors
    ///
    /// - [`EntrypointError::InvalidName`] if any name fails the slug regex (returned
    ///   by deserialization of individual [`EntrypointName`]s, surfaced here on
    ///   programmatic construction).
    /// - [`EntrypointError::DuplicateName`] if two entries share the same name.
    pub fn new(entries: Vec<Entrypoint>) -> Result<Self, EntrypointError> {
        let mut seen: HashSet<String> = HashSet::new();
        for entry in &entries {
            if !seen.insert(entry.name.0.clone()) {
                return Err(EntrypointError::DuplicateName {
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

    pub fn iter(&self) -> impl Iterator<Item = &Entrypoint> {
        self.entries.iter()
    }
}

/// Errors that can occur when constructing or validating [`Entrypoints`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EntrypointError {
    /// An entrypoint name fails the slug regex `^[a-z0-9][a-z0-9_-]*$`.
    #[error("invalid entrypoint name '{name}': must match ^[a-z0-9][a-z0-9_-]*$")]
    InvalidName { name: String },
    /// Two entrypoints share the same name.
    #[error("duplicate entrypoint name '{name}'")]
    DuplicateName { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 3.1 EntrypointName slug validation ─────────────────────────────────

    #[test]
    fn name_accepts_simple_lowercase() {
        assert!(EntrypointName::try_from("cmake").is_ok());
    }

    #[test]
    fn name_accepts_alphanumeric_with_dash_underscore() {
        assert!(EntrypointName::try_from("a1_2-3").is_ok());
        assert!(EntrypointName::try_from("ctest-2").is_ok());
        assert!(EntrypointName::try_from("gcc12").is_ok());
    }

    #[test]
    fn name_rejects_empty() {
        let err = EntrypointName::try_from("").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_uppercase() {
        let err = EntrypointName::try_from("Cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
        let err = EntrypointName::try_from("CMAKE").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_leading_digit() {
        // Slug pattern requires starting char to be [a-z0-9] but the regex
        // is ^[a-z0-9][a-z0-9_-]*$ which DOES allow a leading digit.
        // ADR §1 says "slug regex reused from dependency.rs:12-13" which is
        // ^[a-z0-9][a-z0-9_-]*$. So leading digit IS allowed per the ADR contract.
        // This test confirms leading-digit names are ACCEPTED (not rejected).
        assert!(EntrypointName::try_from("1abc").is_ok());
    }

    #[test]
    fn name_rejects_leading_underscore() {
        // Underscore is not in the leading character class [a-z0-9]
        let err = EntrypointName::try_from("_cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_leading_dash() {
        // Dash is not in the leading character class [a-z0-9]
        let err = EntrypointName::try_from("-cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_path_traversal() {
        let err = EntrypointName::try_from("../../bin/sh").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_slash() {
        let err = EntrypointName::try_from("bin/cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_unicode() {
        let err = EntrypointName::try_from("cmaké").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_accepts_64_char_slug() {
        // Plan §3.13 (F.3): names exactly at `MAX_LEN = 64` must remain accepted.
        let at_cap: String = "a".repeat(EntrypointName::MAX_LEN);
        assert!(
            EntrypointName::try_from(at_cap.as_str()).is_ok(),
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
        let err = EntrypointName::try_from(long_name.as_str()).unwrap_err();
        assert!(
            matches!(err, EntrypointError::InvalidName { .. }),
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
            EntrypointName::try_from(at_cap.as_str()).is_ok(),
            "64-char slug at the cap must remain accepted",
        );
        let over_cap: String = "a".repeat(65);
        let err = EntrypointName::try_from(over_cap.as_str()).expect_err("65-char slug must be rejected");
        assert!(
            matches!(err, EntrypointError::InvalidName { .. }),
            "65-char slug must surface as InvalidName, got: {err:?}",
        );
    }

    // ── 3.1 Entrypoints::new duplicate-name uniqueness ─────────────────────

    #[test]
    fn entrypoints_new_accepts_unique_names() {
        let entries = vec![
            Entrypoint {
                name: EntrypointName::try_from("cmake").unwrap(),
                target: "${installPath}/bin/cmake".to_string(),
            },
            Entrypoint {
                name: EntrypointName::try_from("ctest").unwrap(),
                target: "${installPath}/bin/ctest".to_string(),
            },
        ];
        assert!(Entrypoints::new(entries).is_ok());
    }

    #[test]
    fn entrypoints_new_rejects_duplicate_name() {
        let entries = vec![
            Entrypoint {
                name: EntrypointName::try_from("cmake").unwrap(),
                target: "${installPath}/bin/cmake".to_string(),
            },
            Entrypoint {
                name: EntrypointName::try_from("cmake").unwrap(),
                target: "${installPath}/bin/cmake2".to_string(),
            },
        ];
        let err = Entrypoints::new(entries).unwrap_err();
        assert!(matches!(err, EntrypointError::DuplicateName { name } if name == "cmake"));
    }

    #[test]
    fn entrypoints_new_accepts_empty() {
        let ep = Entrypoints::new(vec![]).unwrap();
        assert!(ep.is_empty());
    }

    // ── 3.1 Entrypoint serde round-trip ────────────────────────────────────

    #[test]
    fn entrypoint_round_trip_via_serde() {
        let json = r#"{"name":"cmake","target":"${installPath}/bin/cmake"}"#;
        let ep: Entrypoint = serde_json::from_str(json).unwrap();
        assert_eq!(ep.name.as_str(), "cmake");
        assert_eq!(ep.target, "${installPath}/bin/cmake");
        let back = serde_json::to_string(&ep).unwrap();
        assert_eq!(back, json);
    }

    #[test]
    fn entrypoint_name_deserialization_rejects_invalid() {
        let json = r#"{"name":"","target":"${installPath}/bin/cmake"}"#;
        assert!(serde_json::from_str::<Entrypoint>(json).is_err());
    }

    // ── 3.1 Entrypoints::is_empty ──────────────────────────────────────────

    #[test]
    fn entrypoints_is_empty_for_default() {
        let ep = Entrypoints::default();
        assert!(ep.is_empty());
    }

    #[test]
    fn entrypoints_is_not_empty_when_populated() {
        let entries = vec![Entrypoint {
            name: EntrypointName::try_from("cmake").unwrap(),
            target: "${installPath}/bin/cmake".to_string(),
        }];
        let ep = Entrypoints::new(entries).unwrap();
        assert!(!ep.is_empty());
    }

    // ── 3.1 Bundle TOML/JSON round-trips with entrypoints ──────────────────

    #[test]
    fn bundle_without_entrypoints_round_trips() {
        // Old JSON without entrypoints — must deserialize and default to empty.
        let json = r#"{"version":1}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(bundle.entrypoints.is_empty());
        // When serializing with empty entrypoints, the field is skipped.
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(!serialized.contains("entrypoints"));
    }

    #[test]
    fn bundle_with_empty_entrypoints_skip_serialized() {
        // Explicit empty array → serialized without the field.
        let json = r#"{"version":1,"entrypoints":[]}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(bundle.entrypoints.is_empty());
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(
            !serialized.contains("entrypoints"),
            "empty entrypoints should be skipped: {serialized}"
        );
    }

    #[test]
    fn bundle_with_populated_entrypoints_round_trips() {
        let json = r#"{"version":1,"entrypoints":[{"name":"cmake","target":"${installPath}/bin/cmake"}]}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(!bundle.entrypoints.is_empty());
        assert_eq!(bundle.entrypoints.iter().next().unwrap().name.as_str(), "cmake");
        // Serialized back must include entrypoints.
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(
            serialized.contains("entrypoints"),
            "populated entrypoints must serialize: {serialized}"
        );
        assert!(serialized.contains("cmake"));
    }

    #[test]
    fn entrypoints_deserialization_error_surfaces_for_duplicate_names() {
        // Two entries with the same name — custom Deserialize calls Entrypoints::new()
        // which enforces uniqueness, so duplicate names are rejected at serde time.
        let json = r#"[{"name":"cmake","target":"bin/cmake"},{"name":"cmake","target":"bin/cmake2"}]"#;
        let result: Result<Entrypoints, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "duplicate entrypoint names must be rejected during deserialization"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cmake") || msg.contains("duplicate"),
            "error must mention 'cmake' or 'duplicate': {msg}"
        );
    }
}
