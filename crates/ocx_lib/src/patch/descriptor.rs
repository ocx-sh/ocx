// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Patch descriptor types and the companion-collection algorithm.
//!
//! A **patch descriptor** is the JSON payload carried in a single OCI layer of
//! the `__ocx.patch` artifact. It maps glob patterns over canonical OCI
//! identifier strings to lists of **companion packages** that carry the site
//! environment overlay.
//!
//! This module is the **domain layer** for descriptors. Config-tier types
//! (`PatchConfig`, `ResolvedPatchConfig`) live in `crates/ocx_lib/src/config/patch.rs`.
//!
//! ## Media-type constants
//!
//! Two constants define the OCI artifact type and layer media type for the
//! `__ocx.patch` artifact — used both at publish time (`ocx patch publish`,
//! Phase 6) and at pull/verify time (Phase 2 persistence):
//!
//! - [`PATCH_MANIFEST_ARTIFACT_TYPE`] — the `artifactType` field on the OCI
//!   image manifest (`application/vnd.sh.ocx.patch.v1`).
//! - [`PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE`] — the `mediaType` of the single layer
//!   that carries the descriptor JSON blob
//!   (`application/vnd.sh.ocx.patch.descriptor.v1+json`).
//!
//! ## Descriptor version
//!
//! The `version` field uses [`serde_repr`] so that an unknown version value
//! causes deserialization to fail immediately with a meaningful error. This
//! mirrors the pattern in `package/metadata/bundle.rs` (`Version` enum).
//!
//! ## Companion collection
//!
//! [`PatchDescriptor::collect_companions`] iterates rules in order, applies the
//! flat glob matcher from [`crate::patch::matcher`] over the base identifier's
//! `Display` string, unions matched companion identifiers, deduplicates by
//! identifier (first-rule wins for `required`), and carries the effective
//! `required` flag (per-rule `required` overrides the tier default passed in).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::oci::Identifier;

// ── Structural limits ─────────────────────────────────────────────────────────

/// Maximum number of rules allowed in a single [`PatchDescriptor`].
///
/// Enforced by [`PatchDescriptor::from_json_bytes`] before the descriptor is
/// used. Guards against a compromised or misconfigured registry delivering a
/// descriptor that would cause O(rules × packages × result) dedup scan cost
/// inside [`PatchDescriptor::collect_companions`].
pub const MAX_RULES: usize = 256;

/// Maximum number of companion packages per rule.
///
/// Companion packages beyond this limit in any single rule cause
/// [`crate::patch::error::PatchError::DescriptorTooLarge`] to be returned from
/// [`PatchDescriptor::from_json_bytes`].
pub const MAX_PACKAGES_PER_RULE: usize = 64;

/// Maximum length (in bytes) of a single rule's `match` glob pattern.
///
/// The flat matcher is O(pattern × text) worst case; a registry-controlled
/// pattern of unbounded length would let a compromised descriptor amplify match
/// cost across every base identifier. A real glob over a `registry/repo:tag`
/// string is far shorter than this; the cap only rejects pathological input.
pub const MAX_MATCH_PATTERN_LEN: usize = 512;

// ── Media-type constants ──────────────────────────────────────────────────────

/// OCI manifest `artifactType` for the `__ocx.patch` artifact.
///
/// The value `application/vnd.sh.ocx.patch.v1` identifies a manifest as a
/// patch descriptor. Validated by [`crate::patch::persistence`] when persisting
/// a fetched manifest and used by `ocx patch publish` when constructing the
/// manifest.
pub const PATCH_MANIFEST_ARTIFACT_TYPE: &str = "application/vnd.sh.ocx.patch.v1";

/// OCI layer `mediaType` for the descriptor JSON blob.
///
/// The value `application/vnd.sh.ocx.patch.descriptor.v1+json` is set on the
/// single layer of the `__ocx.patch` manifest. The blob itself is the UTF-8
/// JSON encoding of [`PatchDescriptor`].
pub const PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE: &str = "application/vnd.sh.ocx.patch.descriptor.v1+json";

// ── Version enum ─────────────────────────────────────────────────────────────

/// Version discriminant for the patch descriptor format.
///
/// Serialized as a bare integer (`1`) via `serde_repr`. An unknown discriminant
/// — e.g. `2` from a newer OCX version — causes deserialization to fail
/// immediately, surfacing `PatchError::UnsupportedVersion` to the caller.
///
/// Mirrors `package::metadata::bundle::Version`.
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

/// A single rule within a [`PatchDescriptor`].
///
/// Each rule declares a flat glob `match` pattern (matched over the canonical
/// OCI identifier `Display` string) and a list of companion package identifiers
/// to apply when the pattern matches.
///
/// `required` overrides the tier default (`ResolvedPatchConfig::required`) for
/// companions produced by this rule. When absent, the tier default applies.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatchRule {
    /// Flat glob pattern matched over the base identifier's canonical
    /// `Display` form (`registry/repository:tag[@digest]`).
    ///
    /// `*` spans any run of characters including `/`, `:`, `@`. See
    /// [`crate::patch::matcher`] for full semantics.
    #[serde(rename = "match")]
    pub match_pattern: String,

    /// Companion packages to apply when this rule matches.
    ///
    /// Identifiers are stored as strings on the wire and parsed on
    /// deserialization. The project-default registry (`ocx.sh`) is used as
    /// the fallback when no registry is present in the string.
    pub packages: Vec<Identifier>,

    /// Per-rule fail posture override.
    ///
    /// When `Some`, overrides the tier-level `required` from
    /// `ResolvedPatchConfig`. When `None`, the tier default is used.
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
/// Constructed by deserializing the raw bytes from the `__ocx.patch` layer.
/// The `version` field is a [`PatchDescriptorVersion`] enum — unknown values
/// are rejected by `serde_repr` before this struct is constructed.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PatchDescriptor {
    /// Descriptor format version. Only [`PatchDescriptorVersion::V1`] is
    /// currently defined; unknown versions are rejected at deserialization.
    pub version: PatchDescriptorVersion,

    /// Ordered list of match rules. Rules are evaluated in order; all matching
    /// rules contribute companions (union), with deduplication by identifier
    /// (first rule wins for the `required` flag).
    pub rules: Vec<PatchRule>,
}

// ── Companion entry ───────────────────────────────────────────────────────────

/// A resolved companion entry produced by [`PatchDescriptor::collect_companions`].
///
/// Carries the companion package [`Identifier`] and the effective `required`
/// flag after applying per-rule overrides and the tier default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanionEntry {
    /// The companion package to load.
    pub identifier: Identifier,
    /// Effective fail posture for this companion.
    ///
    /// `true` = fail closed (abort if unavailable); `false` = fail open (warn
    /// and skip). Derived from the matching rule's `required` field, falling
    /// back to the `tier_required_default` passed to
    /// [`PatchDescriptor::collect_companions`].
    pub required: bool,
    /// The `match` glob of the rule that admitted this companion (provenance).
    ///
    /// Under dedup (first-match-wins by identifier) this is the pattern of the
    /// **first** rule that produced the companion. Carried so an overlay entry
    /// can be traced back to the descriptor rule that selected its companion —
    /// surfaced by `--show-patches`, `ocx patch test`, and `ocx patch why`.
    /// Descriptive only; it does not affect matching or fail posture.
    pub rule_match: String,
}

// ── PatchDescriptor::collect_companions ──────────────────────────────────────

impl PatchDescriptor {
    /// Deserialize a [`PatchDescriptor`] from raw JSON bytes.
    ///
    /// Two-step parse: first extracts the raw `version` integer from the JSON
    /// value to detect unknown versions before the full struct parse runs. This
    /// allows returning [`crate::patch::error::PatchError::UnsupportedVersion`]
    /// with the actual numeric value rather than a generic serde error.
    ///
    /// After a successful version check the full struct is deserialized, and
    /// structural limits ([`MAX_RULES`], [`MAX_PACKAGES_PER_RULE`]) are
    /// validated to bound dedup scan cost in [`Self::collect_companions`].
    ///
    /// # Errors
    ///
    /// - [`crate::patch::error::PatchError::InvalidDescriptorJson`] — the bytes
    ///   are not valid JSON, the `version` field is missing or is not a number,
    ///   or the struct schema is otherwise invalid.
    /// - [`crate::patch::error::PatchError::UnsupportedVersion`] — the `version`
    ///   field holds a numeric value that does not correspond to any known
    ///   [`PatchDescriptorVersion`] discriminant (i.e. `!= 1`).
    /// - [`crate::patch::error::PatchError::DescriptorTooLarge`] — the rules
    ///   count exceeds [`MAX_RULES`] or a rule's packages count exceeds
    ///   [`MAX_PACKAGES_PER_RULE`].
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, crate::patch::error::PatchError> {
        use crate::patch::error::PatchError;

        // Step 1: Parse to a raw JSON value to extract the version integer
        // before committing to a full struct parse. This lets us return a
        // typed UnsupportedVersion error rather than an opaque serde error.
        let raw: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|source| PatchError::InvalidDescriptorJson { source })?;

        // Step 2: Extract and validate the version integer.
        match raw.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v == PatchDescriptorVersion::V1 as u64 => {}
            Some(v) => {
                return Err(PatchError::UnsupportedVersion { version: v as u32 });
            }
            None => {
                // Missing or non-integer version field — let the struct
                // deserializer produce a descriptive error.
                return serde_json::from_value(raw).map_err(|source| PatchError::InvalidDescriptorJson { source });
            }
        }

        // Step 3: Full struct deserialize now that the version is confirmed.
        let descriptor: Self =
            serde_json::from_slice(bytes).map_err(|source| PatchError::InvalidDescriptorJson { source })?;

        // Step 4: Enforce structural limits to bound dedup scan cost.
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

    /// Serialize this descriptor to canonical JSON bytes.
    ///
    /// The inverse of [`Self::from_json_bytes`]: a descriptor round-trips
    /// through `from_json_bytes(&descriptor.to_json_bytes()?)`. Used by
    /// `ocx patch publish` to encode an in-memory descriptor for the
    /// `__ocx.patch` layer blob.
    ///
    /// # Errors
    ///
    /// - [`crate::patch::error::PatchError::InvalidDescriptorJson`] — the
    ///   descriptor cannot be serialized to JSON (in practice unreachable for
    ///   a well-formed descriptor, but surfaced as a typed error to keep the
    ///   library free of panics).
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, crate::patch::error::PatchError> {
        serde_json::to_vec(self).map_err(|source| crate::patch::error::PatchError::InvalidDescriptorJson { source })
    }

    /// Collect all companion entries that apply to `base_identifier`.
    ///
    /// Iterates rules in order, applies the flat glob matcher from
    /// [`crate::patch::matcher::glob_match`] over the base identifier's canonical
    /// `Display` string, unions all matched companion identifiers, and
    /// deduplicates by identifier (the **first** rule that matched a given
    /// identifier wins its `required` value).
    ///
    /// # Match form (C7 phase consistency)
    ///
    /// Each rule pattern is matched against BOTH the full canonical Display
    /// (`registry/repository:tag[@digest]`) AND the digest-excluded tag-form
    /// (`registry/repository:tag`). This keeps install-time discovery (which sees
    /// a tag-form base id) and compose-time overlay (which sees a tag + install
    /// digest) matching IDENTICALLY: a tag-anchored rule such as the ADR's `*:21`
    /// matches whether or not an install digest is present, so a `required`
    /// overlay that matched at install can never be silently dropped at exec.
    /// Matching against an extra form only ever ADMITS more — and a companion
    /// admitted but absent locally still fails closed downstream — so this cannot
    /// open a fail-open path.
    ///
    /// The `tier_required_default` argument supplies the fallback fail posture
    /// from `ResolvedPatchConfig::required` — it is applied when a rule has
    /// `required: None`.
    ///
    /// The caller is responsible for not reading config inside `compose` or GC
    /// leaf paths — this is a pure function over the already-loaded descriptor.
    pub fn collect_companions(&self, base_identifier: &Identifier, tier_required_default: bool) -> Vec<CompanionEntry> {
        use crate::patch::matcher::glob_match;

        let base_str = base_identifier.to_string();
        // Digest-excluded tag-form so tag-anchored patterns (`*:21`) match
        // regardless of an install digest — see the "Match form" doc above.
        // `without_digest` keeps the tag and drops the `@sha256:...` suffix; when
        // the base carries no digest the two strings are equal (one extra cheap
        // glob, no behaviour change).
        let base_tag_form = base_identifier.without_digest().to_string();

        // Accumulate companions in rule order, deduplicating by identifier.
        // A Vec + linear scan is fine here: descriptors have O(10) rules and
        // O(10) packages per rule — far below any allocation threshold.
        let mut result: Vec<CompanionEntry> = Vec::new();

        for rule in &self.rules {
            if !glob_match(&rule.match_pattern, &base_str) && !glob_match(&rule.match_pattern, &base_tag_form) {
                continue;
            }

            // Effective `required` for this rule: per-rule override else tier default.
            let effective_required = rule.required.unwrap_or(tier_required_default);

            for package_id in &rule.packages {
                // Dedup by identifier (structural equality via PartialEq; first-match
                // wins for the `required` value). Identifier derives PartialEq which
                // compares all fields — semantically identical to Display equality but
                // without heap allocation on every comparison.
                let already_seen = result.iter().any(|e| &e.identifier == package_id);
                if !already_seen {
                    result.push(CompanionEntry {
                        identifier: package_id.clone(),
                        required: effective_required,
                        rule_match: rule.match_pattern.clone(),
                    });
                }
                // If already present, skip — first rule wins (no update).
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

    /// Companion identifiers inside `packages` arrays are deserialized as `Identifier`.
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
        let base = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
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
        let base = Identifier::parse("ocx.sh/java:21").expect("valid identifier");
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

    /// C7 regression (Codex final-pass BLOCK): an END-ANCHORED tag rule (the
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
        let base = Identifier::parse(&format!("ocx.sh/java:21@{digest}")).expect("valid digest-pinned identifier");
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
        let discovery_base = Identifier::parse("ocx.sh/java:21").expect("valid");
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
        let base = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
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
        let base = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
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
        let base = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
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
