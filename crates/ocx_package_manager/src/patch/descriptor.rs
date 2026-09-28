// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Patch descriptor types and the companion-collection algorithm.
//!
//! A **patch descriptor** is the JSON payload in the single OCI layer of the
//! `__ocx.patch` artifact ([`PATCH_MANIFEST_ARTIFACT_TYPE`] manifest,
//! [`PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE`] layer), mapping glob patterns over
//! canonical OCI identifiers to the **companion packages** that carry the site
//! environment overlay.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use ocx_oci::PackageRef;

// ── Structural limits ─────────────────────────────────────────────────────────

/// Maximum number of rules allowed in a single [`PatchDescriptor`]; bounds the
/// dedup scan cost of [`PatchDescriptor::collect_companions`].
pub const MAX_RULES: usize = 256;

/// Maximum number of companion packages per rule.
pub const MAX_PACKAGES_PER_RULE: usize = 64;

/// Maximum length (in bytes) of a single rule's `match` glob pattern; bounds the
/// O(pattern × text) matcher against a registry-controlled pattern.
pub const MAX_MATCH_PATTERN_LEN: usize = 512;

// ── Media-type constants ──────────────────────────────────────────────────────

/// OCI manifest `artifactType` for the `__ocx.patch` artifact.
pub const PATCH_MANIFEST_ARTIFACT_TYPE: &str = "application/vnd.sh.ocx.patch.v1";

/// OCI layer `mediaType` for the descriptor JSON blob, the UTF-8 JSON encoding
/// of [`PatchDescriptor`].
pub const PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE: &str = "application/vnd.sh.ocx.patch.descriptor.v1+json";

// ── Version enum ─────────────────────────────────────────────────────────────

/// Version discriminant for the patch descriptor format, serialized as a bare integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u32)]
pub enum PatchDescriptorVersion {
    /// The initial patch descriptor format (`"version": 1`).
    V1 = 1,
}

impl schemars::JsonSchema for PatchDescriptorVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PatchDescriptorVersion")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "description": "Patch descriptor format version. Currently only 1 is valid.",
            "enum": [1]
        })
    }
}

impl fmt::Display for PatchDescriptorVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", *self as u32)
    }
}

// ── PatchRule ─────────────────────────────────────────────────────────────────

/// A single rule within a patch descriptor.
///
/// Each rule declares a flat glob `match` pattern (matched over the canonical
/// identifier string) and a list of companion package identifiers to apply
/// when the pattern matches.
///
/// `required` overrides the tier default (`[patches] required`) for
/// companions produced by this rule. When absent, the tier default applies.
// The tier default is `ResolvedPatchConfig::required`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatchRule {
    /// Flat glob pattern matched over the base identifier's canonical
    /// form (`registry/repository:tag[@digest]`).
    ///
    /// `*` spans any run of characters including `/`, `:`, `@`.
    // Full semantics: `crate::patch::matcher`.
    #[serde(rename = "match")]
    pub match_pattern: String,

    /// Companion packages to apply when this rule matches.
    ///
    /// Identifier strings; one without a registry resolves against the default
    /// registry, `ocx.sh`.
    pub packages: Vec<PackageRef>,

    /// Per-rule fail posture override.
    ///
    /// When set, overrides the tier-level `[patches] required`. When absent,
    /// the tier default is used.
    ///
    /// - `true` — fail closed: abort if any companion in this rule is unavailable.
    /// - `false` — fail open: skip with a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

// ── PatchDescriptor ───────────────────────────────────────────────────────────

/// Parsed form of the JSON blob carried in a `__ocx.patch` descriptor layer.
///
/// ```json
/// { "version": 1, "rules": [
///   { "match": "*",             "packages": ["internal.company.com/certs/zscaler-root:latest"] },
///   { "match": "ocx.sh/java:*", "packages": ["internal.company.com/java-config/jdk21-truststore:1.0"],
///     "required": false }
/// ] }
/// ```
///
/// Unknown `version` values are rejected.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatchDescriptor {
    /// Descriptor format version. Only `1` is currently defined; unknown
    /// versions are rejected at deserialization.
    pub version: PatchDescriptorVersion,

    /// Ordered list of match rules. Rules are evaluated in order; all matching
    /// rules contribute companions (union), with deduplication by identifier
    /// (first rule wins for the `required` flag).
    pub rules: Vec<PatchRule>,
}

// ── Companion entry ───────────────────────────────────────────────────────────

/// A resolved companion entry produced by [`PatchDescriptor::collect_companions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionEntry {
    /// The companion package to load.
    pub identifier: PackageRef,
    /// Effective fail posture: `true` = fail closed, `false` = warn and skip.
    pub required: bool,
    /// The `match` glob of the **first** rule that admitted this companion;
    /// provenance only, it does not affect matching or fail posture.
    pub rule_match: String,
}

// ── PatchDescriptor::collect_companions ──────────────────────────────────────

impl PatchDescriptor {
    /// Deserialize a [`PatchDescriptor`] from raw JSON bytes.
    ///
    /// # Errors
    ///
    /// - [`crate::patch::error::PatchError::InvalidDescriptorJson`] — invalid
    ///   JSON, a missing or non-numeric `version`, or an invalid shape.
    /// - [`crate::patch::error::PatchError::UnsupportedVersion`] — an unknown
    ///   numeric `version`.
    /// - [`crate::patch::error::PatchError::DescriptorTooLarge`] — a structural
    ///   limit ([`MAX_RULES`], [`MAX_PACKAGES_PER_RULE`], [`MAX_MATCH_PATTERN_LEN`]) exceeded.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, crate::patch::error::PatchError> {
        use crate::patch::error::PatchError;

        // Version read from a raw value first, or an unknown version surfaces as an opaque serde error.
        let raw: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|source| PatchError::InvalidDescriptorJson { source })?;

        match raw.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v == PatchDescriptorVersion::V1 as u64 => {}
            Some(v) => {
                return Err(PatchError::UnsupportedVersion { version: v as u32 });
            }
            None => {
                return serde_json::from_value(raw).map_err(|source| PatchError::InvalidDescriptorJson { source });
            }
        }

        let descriptor: Self =
            serde_json::from_slice(bytes).map_err(|source| PatchError::InvalidDescriptorJson { source })?;

        if descriptor.rules.len() > MAX_RULES {
            return Err(PatchError::DescriptorTooLarge {
                detail: format!("rules count {} exceeds maximum {}", descriptor.rules.len(), MAX_RULES),
            });
        }
        for (index, rule) in descriptor.rules.iter().enumerate() {
            if rule.packages.len() > MAX_PACKAGES_PER_RULE {
                return Err(PatchError::DescriptorTooLarge {
                    detail: format!(
                        "rule[{index}] packages count {} exceeds maximum {MAX_PACKAGES_PER_RULE}",
                        rule.packages.len()
                    ),
                });
            }
            if rule.match_pattern.len() > MAX_MATCH_PATTERN_LEN {
                return Err(PatchError::DescriptorTooLarge {
                    detail: format!(
                        "rule[{index}] match pattern length {} exceeds maximum {MAX_MATCH_PATTERN_LEN}",
                        rule.match_pattern.len()
                    ),
                });
            }
        }

        Ok(descriptor)
    }

    /// Serialize this descriptor to canonical JSON bytes, the inverse of
    /// [`Self::from_json_bytes`].
    ///
    /// # Errors
    ///
    /// [`crate::patch::error::PatchError::InvalidDescriptorJson`] if serialization fails.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, crate::patch::error::PatchError> {
        serde_json::to_vec(self).map_err(|source| crate::patch::error::PatchError::InvalidDescriptorJson { source })
    }

    /// Collect all companion entries that apply to `base_identifier`: rules in
    /// order, matched with [`crate::patch::matcher::glob_match`], unioned and
    /// deduplicated by identifier (the **first** matching rule wins `required`;
    /// `tier_required_default` applies where a rule has `required: None`).
    ///
    /// Each pattern is matched against both the canonical form and the
    /// digest-excluded tag form.
    pub fn collect_companions(&self, base_identifier: &PackageRef, tier_required_default: bool) -> Vec<CompanionEntry> {
        use crate::patch::matcher::glob_match;

        let base_str = base_identifier.to_string();
        // Match the tag form too, or a `required` overlay matched at install (tag-form base)
        // is silently dropped at exec (tag + digest).
        let base_tag_form = base_identifier.without_digest().to_string();

        let mut result: Vec<CompanionEntry> = Vec::new();

        for rule in &self.rules {
            if !glob_match(&rule.match_pattern, &base_str) && !glob_match(&rule.match_pattern, &base_tag_form) {
                continue;
            }

            let effective_required = rule.required.unwrap_or(tier_required_default);

            for package_id in &rule.packages {
                let already_seen = result.iter().any(|e| &e.identifier == package_id);
                if !already_seen {
                    result.push(CompanionEntry {
                        identifier: package_id.clone(),
                        required: effective_required,
                        rule_match: rule.match_pattern.clone(),
                    });
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::PatchError;

    // ── ADR example JSON ──────────────────────────────────────────────────────

    /// Build the two-rule descriptor from the ADR §"Patch descriptor" example.
    ///
    /// ```json
    /// { "version": 1, "rules": [
    ///   { "match": "*",             "packages": ["internal.company.com/certs/zscaler-root:latest"] },
    ///   { "match": "ocx.sh/java:*", "packages": ["internal.company.com/java-config/jdk21-truststore:1.0"],
    ///     "required": false }
    /// ] }
    /// ```
    fn adr_example_json() -> Vec<u8> {
        serde_json::json!({
            "version": 1,
            "rules": [
                {
                    "match": "*",
                    "packages": ["internal.company.com/certs/zscaler-root:latest"]
                },
                {
                    "match": "ocx.sh/java:*",
                    "packages": ["internal.company.com/java-config/jdk21-truststore:1.0"],
                    "required": false
                }
            ]
        })
        .to_string()
        .into_bytes()
    }

    /// The ADR example descriptor parses with version = V1 and two rules.
    #[test]
    fn adr_example_parses_version_and_rule_count() {
        let descriptor = PatchDescriptor::from_json_bytes(&adr_example_json()).expect("ADR example must parse");
        assert_eq!(
            descriptor.version,
            PatchDescriptorVersion::V1,
            "version must deserialize as V1"
        );
        assert_eq!(descriptor.rules.len(), 2, "ADR example has two rules");
    }

    /// The `"match"` JSON key maps to the `match_pattern` field (serde rename).
    #[test]
    fn serde_rename_match_to_match_pattern() {
        let descriptor = PatchDescriptor::from_json_bytes(&adr_example_json()).expect("must parse");
        assert_eq!(
            descriptor.rules[0].match_pattern, "*",
            "first rule match_pattern must be '*'"
        );
        assert_eq!(
            descriptor.rules[1].match_pattern, "ocx.sh/java:*",
            "second rule match_pattern must be 'ocx.sh/java:*'"
        );
    }

    /// Companion identifiers inside `packages` arrays are deserialized as `PackageRef`.
    #[test]
    fn packages_parse_to_identifier() {
        let descriptor = PatchDescriptor::from_json_bytes(&adr_example_json()).expect("must parse");

        let rule0_pkg = &descriptor.rules[0].packages;
        assert_eq!(rule0_pkg.len(), 1, "first rule must have one companion");
        assert_eq!(
            rule0_pkg[0].to_string(),
            "internal.company.com/certs/zscaler-root:latest",
            "first companion identifier must round-trip through Display"
        );

        let rule1_pkg = &descriptor.rules[1].packages;
        assert_eq!(rule1_pkg.len(), 1, "second rule must have one companion");
        assert_eq!(
            rule1_pkg[0].to_string(),
            "internal.company.com/java-config/jdk21-truststore:1.0",
            "second companion identifier must round-trip through Display"
        );
    }

    /// Per-rule `required` is present in the second rule (false) and absent in the first.
    #[test]
    fn per_rule_required_present_and_absent() {
        let descriptor = PatchDescriptor::from_json_bytes(&adr_example_json()).expect("must parse");

        assert_eq!(
            descriptor.rules[0].required, None,
            "first rule has no required override — must be None"
        );
        assert_eq!(
            descriptor.rules[1].required,
            Some(false),
            "second rule has required = false"
        );
    }

    /// A descriptor with `required: true` on a rule also deserializes correctly.
    #[test]
    fn per_rule_required_true_deserializes() {
        let json = serde_json::json!({
            "version": 1,
            "rules": [
                { "match": "*", "packages": ["internal.company.com/certs/zscaler-root:latest"], "required": true }
            ]
        })
        .to_string()
        .into_bytes();
        let descriptor = PatchDescriptor::from_json_bytes(&json).expect("must parse");
        assert_eq!(descriptor.rules[0].required, Some(true));
    }

    /// An unknown `version` value (99) is rejected by the two-step parse in
    /// `from_json_bytes` before the full struct deserialize runs, surfacing
    /// `PatchError::UnsupportedVersion { version: 99 }`.
    #[test]
    fn unknown_version_rejected() {
        let json = serde_json::json!({ "version": 99, "rules": [] })
            .to_string()
            .into_bytes();
        let result = PatchDescriptor::from_json_bytes(&json);
        assert!(
            matches!(result, Err(PatchError::UnsupportedVersion { version: 99 })),
            "version 99 must be rejected with UnsupportedVersion {{ version: 99 }}, got: {result:?}"
        );
    }

    // ── collect_companions spec ───────────────────────────────────────────────

    /// A base identifier matching the catch-all `"*"` rule yields the companion
    /// from that rule.
    #[test]
    fn collect_companions_catch_all_rule() {
        let descriptor = PatchDescriptor::from_json_bytes(&adr_example_json()).expect("must parse");
        let base = PackageRef::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        // cmake matches "*" (rule 0) but NOT "ocx.sh/java:*" (rule 1).
        let companions = descriptor.collect_companions(&base, true);
        assert_eq!(companions.len(), 1, "only the catch-all companion should match");
        assert_eq!(
            companions[0].identifier.to_string(),
            "internal.company.com/certs/zscaler-root:latest"
        );
        // Rule 0 has no required override; tier default = true applies.
        assert!(companions[0].required, "tier default required=true must be effective");
    }

    /// A Java identifier matches both rules; companions are unioned with dedup by identifier.
    #[test]
    fn collect_companions_both_rules_match_java() {
        let descriptor = PatchDescriptor::from_json_bytes(&adr_example_json()).expect("must parse");
        let base = PackageRef::parse("ocx.sh/java:21").expect("valid identifier");
        // Matches rule 0 (*) → zscaler-root:latest with tier default required
        // AND rule 1 (ocx.sh/java:*) → jdk21-truststore:1.0 with required=false
        let companions = descriptor.collect_companions(&base, true);
        assert_eq!(companions.len(), 2, "both companions should match for Java");
        // zscaler-root comes first (rule 0 matched first)
        assert_eq!(
            companions[0].identifier.to_string(),
            "internal.company.com/certs/zscaler-root:latest"
        );
        assert!(
            companions[0].required,
            "rule 0 has no override, tier default true applies"
        );
        // jdk21-truststore comes second (rule 1)
        assert_eq!(
            companions[1].identifier.to_string(),
            "internal.company.com/java-config/jdk21-truststore:1.0"
        );
        assert!(!companions[1].required, "rule 1 overrides required=false");
        // Provenance: each companion carries the `match` glob of the rule that
        // admitted it (the catch-all `*` for companion 0, the Java rule for 1).
        assert_eq!(
            companions[0].rule_match, "*",
            "companion 0 came from the catch-all rule"
        );
        assert_eq!(
            companions[1].rule_match, "ocx.sh/java:*",
            "companion 1 came from the java-specific rule"
        );
    }

    /// Match-form regression: an END-ANCHORED tag rule (the
    /// ADR's `*:21` shape — no trailing glob to absorb a digest) must match a
    /// base that carries an install DIGEST, which is the form the COMPOSE-time
    /// overlay sees (the admitted set carries `registry/repo:tag@digest`). Before
    /// the fix, compose matched against the advisory-stripped `registry/repo@digest`
    /// form, so a `required` overlay that matched at install was silently dropped
    /// at exec — a tool ran without its mandated companion env (fail-open, not
    /// fail-closed).
    #[test]
    fn collect_companions_end_anchored_tag_rule_matches_digest_bearing_base() {
        let descriptor = PatchDescriptor::from_json_bytes(
            br#"{"version":1,"rules":[{"match":"*:21","packages":["internal.company.com/jdk-trust:1.0"],"required":true}]}"#,
        )
        .expect("must parse");
        // Compose-time base id: tag AND install digest (what `admitted` carries).
        let digest = format!("sha256:{}", "a".repeat(64));
        let base = PackageRef::parse(&format!("ocx.sh/java:21@{digest}")).expect("valid digest-pinned identifier");
        let companions = descriptor.collect_companions(&base, true);
        assert_eq!(
            companions.len(),
            1,
            "end-anchored tag rule `*:21` must match the digest-bearing compose form (C7); got {companions:?}"
        );
        assert!(
            companions[0].required,
            "the required overlay must survive into the compose-time match"
        );

        // And it must ALSO match the digest-less discovery form — both phases
        // resolve identically (no drift).
        let discovery_base = PackageRef::parse("ocx.sh/java:21").expect("valid");
        assert_eq!(
            descriptor.collect_companions(&discovery_base, true).len(),
            1,
            "discovery (digest-less) and compose (digest-bearing) must match identically"
        );
    }

    /// When the same identifier appears in two rules, the first rule wins its
    /// `required` value (dedup = first-match-wins).
    #[test]
    fn collect_companions_dedup_first_rule_wins_required() {
        // Descriptor with the same companion in both rules, but different required values.
        let json = serde_json::json!({
            "version": 1,
            "rules": [
                { "match": "*", "packages": ["internal.company.com/certs/ca-bundle:latest"], "required": true },
                { "match": "*", "packages": ["internal.company.com/certs/ca-bundle:latest"], "required": false }
            ]
        })
        .to_string()
        .into_bytes();
        let descriptor = PatchDescriptor::from_json_bytes(&json).expect("must parse");
        let base = PackageRef::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        let companions = descriptor.collect_companions(&base, false);
        // Dedup: only one companion despite appearing in both rules.
        assert_eq!(companions.len(), 1, "dedup must yield exactly one entry");
        // First rule wins required=true (not the second rule's false).
        assert!(companions[0].required, "first-rule-wins: required=true from rule 0");
    }

    /// `collect_companions` with a tier default of `false` propagates the default
    /// to rules without an explicit `required` override.
    #[test]
    fn collect_companions_tier_default_false_propagates() {
        // Single rule with no `required` field; tier default = false.
        let json = serde_json::json!({
            "version": 1,
            "rules": [
                { "match": "*", "packages": ["internal.company.com/certs/ca-bundle:latest"] }
            ]
        })
        .to_string()
        .into_bytes();
        let descriptor = PatchDescriptor::from_json_bytes(&json).expect("must parse");
        let base = PackageRef::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        // Tier default = false (fail-open)
        let companions = descriptor.collect_companions(&base, false);
        assert_eq!(companions.len(), 1);
        assert!(
            !companions[0].required,
            "tier default false must propagate when rule has no required override"
        );
    }

    /// A base identifier that matches NO rules returns an empty companion list.
    #[test]
    fn collect_companions_no_match_returns_empty() {
        // Descriptor that only patches Java; cmake should get no companions.
        let json = serde_json::json!({
            "version": 1,
            "rules": [
                { "match": "ocx.sh/java:*", "packages": ["internal.company.com/java-config/jdk21-truststore:1.0"] }
            ]
        })
        .to_string()
        .into_bytes();
        let descriptor = PatchDescriptor::from_json_bytes(&json).expect("must parse");
        let base = PackageRef::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        let companions = descriptor.collect_companions(&base, true);
        assert!(companions.is_empty(), "cmake must not match a java-only descriptor");
    }

    // ── to_json_bytes round-trip ──────────────────────────────────────────────

    /// `from_json_bytes ∘ to_json_bytes` is the identity on the parsed
    /// descriptor: re-encoding then re-parsing yields an equivalent descriptor
    /// (same version, same rule shape).
    #[test]
    fn to_json_bytes_round_trips_through_from_json_bytes() {
        let original = PatchDescriptor::from_json_bytes(&adr_example_json()).expect("ADR example must parse");
        let encoded = original.to_json_bytes().expect("descriptor must serialize");
        let reparsed = PatchDescriptor::from_json_bytes(&encoded).expect("re-encoded descriptor must parse");

        assert_eq!(reparsed.version, original.version, "version must survive round-trip");
        assert_eq!(
            reparsed.rules.len(),
            original.rules.len(),
            "rule count must survive round-trip"
        );
        for (a, b) in reparsed.rules.iter().zip(original.rules.iter()) {
            assert_eq!(
                a.match_pattern, b.match_pattern,
                "match pattern must survive round-trip"
            );
            assert_eq!(a.required, b.required, "required flag must survive round-trip");
            assert_eq!(
                a.packages.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                b.packages.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
                "companion identifiers must survive round-trip"
            );
        }
    }

    // ── Media-type constants ──────────────────────────────────────────────────

    /// The media-type constants have the values specified in the ADR.
    #[test]
    fn media_type_constants_values() {
        assert_eq!(PATCH_MANIFEST_ARTIFACT_TYPE, "application/vnd.sh.ocx.patch.v1");
        assert_eq!(
            PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE,
            "application/vnd.sh.ocx.patch.descriptor.v1+json"
        );
    }

    // ── Version display ───────────────────────────────────────────────────────

    #[test]
    fn patch_descriptor_version_display() {
        assert_eq!(PatchDescriptorVersion::V1.to_string(), "1");
    }

    // ── Structural limits ─────────────────────────────────────────────────────

    /// A descriptor with exactly MAX_RULES rules parses successfully.
    #[test]
    fn structural_limit_max_rules_allowed() {
        let rules: Vec<serde_json::Value> = (0..MAX_RULES)
            .map(|i| {
                serde_json::json!({
                    "match": format!("ocx.sh/tool-{i}:*"),
                    "packages": ["internal.company.com/certs/ca-bundle:latest"]
                })
            })
            .collect();
        let json = serde_json::json!({ "version": 1, "rules": rules })
            .to_string()
            .into_bytes();
        assert!(
            PatchDescriptor::from_json_bytes(&json).is_ok(),
            "exactly MAX_RULES={MAX_RULES} rules must be accepted"
        );
    }

    /// A descriptor with MAX_RULES + 1 rules is rejected with DescriptorTooLarge.
    #[test]
    fn structural_limit_rules_count_exceeded() {
        let rules: Vec<serde_json::Value> = (0..=MAX_RULES)
            .map(|i| {
                serde_json::json!({
                    "match": format!("ocx.sh/tool-{i}:*"),
                    "packages": ["internal.company.com/certs/ca-bundle:latest"]
                })
            })
            .collect();
        let json = serde_json::json!({ "version": 1, "rules": rules })
            .to_string()
            .into_bytes();
        let result = PatchDescriptor::from_json_bytes(&json);
        assert!(
            matches!(result, Err(PatchError::DescriptorTooLarge { .. })),
            "MAX_RULES+1 rules must be rejected with DescriptorTooLarge, got: {result:?}"
        );
    }

    /// A rule with MAX_PACKAGES_PER_RULE + 1 packages is rejected with DescriptorTooLarge.
    #[test]
    fn structural_limit_packages_per_rule_exceeded() {
        let packages: Vec<String> = (0..=MAX_PACKAGES_PER_RULE)
            .map(|i| format!("internal.company.com/certs/ca-bundle-{i}:latest"))
            .collect();
        let json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": packages }]
        })
        .to_string()
        .into_bytes();
        let result = PatchDescriptor::from_json_bytes(&json);
        assert!(
            matches!(result, Err(PatchError::DescriptorTooLarge { .. })),
            "MAX_PACKAGES_PER_RULE+1 packages must be rejected with DescriptorTooLarge, got: {result:?}"
        );
    }

    /// A rule whose `match` pattern exceeds MAX_MATCH_PATTERN_LEN is rejected
    /// with DescriptorTooLarge, bounding the matcher's worst-case cost against a
    /// registry-controlled pattern.
    #[test]
    fn structural_limit_match_pattern_too_long() {
        let long_pattern = "*".repeat(MAX_MATCH_PATTERN_LEN + 1);
        let json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": long_pattern, "packages": ["internal.company.com/certs/ca:latest"] }]
        })
        .to_string()
        .into_bytes();
        let result = PatchDescriptor::from_json_bytes(&json);
        assert!(
            matches!(result, Err(PatchError::DescriptorTooLarge { .. })),
            "over-long match pattern must be rejected with DescriptorTooLarge, got: {result:?}"
        );
    }

    /// A pattern exactly at MAX_MATCH_PATTERN_LEN is accepted (boundary).
    #[test]
    fn structural_limit_match_pattern_at_boundary_accepted() {
        let pattern = "a".repeat(MAX_MATCH_PATTERN_LEN);
        let json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": pattern, "packages": ["internal.company.com/certs/ca:latest"] }]
        })
        .to_string()
        .into_bytes();
        assert!(
            PatchDescriptor::from_json_bytes(&json).is_ok(),
            "pattern at exactly MAX_MATCH_PATTERN_LEN must be accepted"
        );
    }

    /// The two-step parse returns UnsupportedVersion with the actual version number.
    #[test]
    fn unsupported_version_carries_numeric_value() {
        let json = serde_json::json!({ "version": 42, "rules": [] })
            .to_string()
            .into_bytes();
        let result = PatchDescriptor::from_json_bytes(&json);
        assert!(
            matches!(result, Err(PatchError::UnsupportedVersion { version: 42 })),
            "version 42 must surface as UnsupportedVersion {{ version: 42 }}, got: {result:?}"
        );
    }

    /// Missing `version` field surfaces as InvalidDescriptorJson.
    #[test]
    fn missing_version_field_is_invalid_json() {
        let json = serde_json::json!({ "rules": [] }).to_string().into_bytes();
        let result = PatchDescriptor::from_json_bytes(&json);
        assert!(
            matches!(result, Err(PatchError::InvalidDescriptorJson { .. })),
            "missing version field must be InvalidDescriptorJson, got: {result:?}"
        );
    }
}
