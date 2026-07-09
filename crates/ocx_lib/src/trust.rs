// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Trust policy: identity-pinned verification config (`[[trust.policy]]`).
//!
//! A trust policy pins the expected signing identity (Fulcio certificate SAN)
//! and OIDC issuer for a scope of packages, so `ocx package verify` can reject
//! a typosquat that carries a valid-but-wrong Sigstore identity. Without
//! identity pinning, an attacker who publishes to the same registry with their
//! own valid GitHub Actions OIDC token passes signature verification.
//!
//! Policies are declared as an array-of-tables (`[[trust.policy]]`) in the
//! tiered `config.toml` (system / user / `$OCX_HOME`) and in the project
//! `ocx.toml`. All tiers **pool** (array-append, never replace): the effective
//! policy set is the union of every tier's entries. Resolution is
//! most-specific-wins (longest literal scope prefix) with **ANY-of** among
//! equal-specificity scopes, which is what makes key/workflow rotation work —
//! the old and new identity coexist during the overlap window and either one
//! passes. Because resolution is a set union + ANY-of, tier *order* never
//! changes the pass/fail outcome.
//!
//! This module is a leaf: it must not depend on `oci`. The certificate-side
//! matching that consumes a resolved [`CompiledPolicy`] lives in
//! `oci::verify::identity`. See `.claude/artifacts/adr_trust_policy.md`.

use serde::{Deserialize, Serialize};

/// Container for the `[trust]` config section (`[[trust.policy]]` entries).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    /// The declared policies. Empty when `[trust]` is present but lists none.
    #[serde(default)]
    pub policy: Vec<TrustPolicy>,
}

/// A single `[[trust.policy]]` entry.
///
/// Exactly one of `identity` / `identity_regexp` must be set — both or neither
/// is a configuration error surfaced by [`TrustPolicy::compile`] (cosign's
/// `--certificate-identity` / `--certificate-identity-regexp` precedent).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustPolicy {
    /// Package prefix this policy applies to, e.g. `ghcr.io/acme/*`.
    ///
    /// The literal prefix (the text before the first `*`) is matched against a
    /// target's canonical `registry/repository`. A scope without `*` is still a
    /// prefix — `ghcr.io/acme/tool` also covers `ghcr.io/acme/tool-cli`.
    pub scope: String,

    /// Exact expected certificate SAN (byte-equal). Mutually exclusive with
    /// [`Self::identity_regexp`].
    #[serde(default)]
    pub identity: Option<String>,

    /// Regex the certificate SAN must match in full (anchored `\A…\z`).
    /// Mutually exclusive with [`Self::identity`].
    #[serde(default)]
    pub identity_regexp: Option<String>,

    /// Exact expected OIDC issuer URL (byte-equal).
    pub oidc_issuer: String,
}

impl TrustPolicy {
    /// The literal scope prefix: everything before the first `*` (the whole
    /// scope when there is no wildcard).
    #[must_use]
    pub fn literal_prefix(&self) -> &str {
        match self.scope.find('*') {
            Some(index) => &self.scope[..index],
            None => &self.scope,
        }
    }

    /// Whether this policy's scope matches the canonical `registry/repository`
    /// target.
    ///
    /// A no-wildcard scope matches on **path-segment boundaries**: `ghcr.io/acme`
    /// matches `ghcr.io/acme` and `ghcr.io/acme/tool`, but never
    /// `ghcr.io/acmecorp`. A `*` makes it a glob on the literal prefix before
    /// the wildcard (`ghcr.io/acme/*` covers the subtree; a bare `ghcr.io/acme*`
    /// is an intentional substring glob). An empty scope is a catch-all.
    #[must_use]
    pub fn matches_scope(&self, target: &str) -> bool {
        if self.scope.is_empty() {
            return true;
        }
        match self.scope.find('*') {
            Some(index) => target.starts_with(&self.scope[..index]),
            None => target == self.scope || target.starts_with(&format!("{}/", self.scope)),
        }
    }

    /// Compile the identity constraint, enforcing the identity XOR
    /// identity_regexp invariant and the issuer being present.
    ///
    /// # Errors
    /// [`TrustPolicyError::IdentityConflict`] when both identity fields are
    /// set, [`TrustPolicyError::IdentityUnset`] when neither is, and
    /// [`TrustPolicyError::InvalidRegex`] when `identity_regexp` does not
    /// compile.
    pub fn compile(&self) -> Result<CompiledPolicy, TrustPolicyError> {
        let identity = match (&self.identity, &self.identity_regexp) {
            (Some(_), Some(_)) => {
                return Err(TrustPolicyError::IdentityConflict {
                    scope: self.scope.clone(),
                });
            }
            (None, None) => {
                return Err(TrustPolicyError::IdentityUnset {
                    scope: self.scope.clone(),
                });
            }
            (Some(exact), None) => IdentityRule::Exact(exact.clone()),
            (None, Some(pattern)) => {
                IdentityRule::compile_regex(pattern).map_err(|source| TrustPolicyError::InvalidRegex {
                    scope: self.scope.clone(),
                    source,
                })?
            }
        };
        Ok(CompiledPolicy {
            identity,
            issuer: self.oidc_issuer.clone(),
        })
    }
}

/// A compiled, ready-to-match acceptable `(identity, issuer)` constraint.
#[derive(Debug, Clone)]
pub struct CompiledPolicy {
    /// The identity constraint (exact or anchored regex).
    pub identity: IdentityRule,
    /// The exact expected OIDC issuer URL.
    pub issuer: String,
}

impl CompiledPolicy {
    /// Build a single exact `(identity, issuer)` policy — the flag-override
    /// path (`--certificate-identity` + `--certificate-oidc-issuer`).
    #[must_use]
    pub fn exact(identity: String, issuer: String) -> Self {
        Self {
            identity: IdentityRule::Exact(identity),
            issuer,
        }
    }
}

/// A compiled certificate-SAN constraint.
#[derive(Debug, Clone)]
pub enum IdentityRule {
    /// Byte-equal exact match against the certificate SAN.
    Exact(String),
    /// Anchored full-match regex against the certificate SAN.
    Regex(regex::Regex),
}

impl IdentityRule {
    /// Compile a user regex into a full-match rule by anchoring it with
    /// `\A(?:…)\z`, so the pattern must match the entire SAN (cosign's
    /// `--certificate-identity-regexp` full-string semantics). Redundant
    /// user-supplied `^`/`$` anchors stay harmless.
    ///
    /// # Errors
    /// Returns the [`regex::Error`] when the pattern does not compile.
    pub fn compile_regex(pattern: &str) -> Result<Self, regex::Error> {
        let anchored = format!(r"\A(?:{pattern})\z");
        Ok(Self::Regex(regex::Regex::new(&anchored)?))
    }

    /// Whether the certificate SAN satisfies this rule.
    #[must_use]
    pub fn matches(&self, san: &str) -> bool {
        match self {
            Self::Exact(expected) => san == expected,
            Self::Regex(regex) => regex.is_match(san),
        }
    }
}

/// Resolve the applicable policies for a canonical `registry/repository`
/// target: the matching policies with the **longest** literal scope prefix
/// (most-specific-wins), returned as a set for ANY-of evaluation. Empty when no
/// scope matches.
///
/// The input is any iterator of policy references, so callers can chain every
/// tier's entries (config.toml tiers ++ project ocx.toml) without allocating an
/// intermediate pool.
#[must_use]
pub fn resolve<'a>(policies: impl IntoIterator<Item = &'a TrustPolicy>, target: &str) -> Vec<&'a TrustPolicy> {
    let matching: Vec<&TrustPolicy> = policies
        .into_iter()
        .filter(|policy| policy.matches_scope(target))
        .collect();
    let Some(best) = matching.iter().map(|policy| policy.literal_prefix().len()).max() else {
        return Vec::new();
    };
    matching
        .into_iter()
        .filter(|policy| policy.literal_prefix().len() == best)
        .collect()
}

/// Resolve and compile the effective policies for a canonical
/// `registry/repository` target under **cross-tier precedence**.
///
/// Operator-tier policies (the merged `config.toml` — system / user /
/// `$OCX_HOME`) are **authoritative**: if any operator policy matches the
/// target, only operator policies are considered and the project `ocx.toml` is
/// ignored for that package, so a project config can never override or weaken
/// an operator pin (security ruling — see `adr_trust_policy.md`). When no
/// operator policy matches, the `project` tier applies (it may *add* trust for
/// scopes the operator has not governed). Within the chosen tier: most-specific
/// scope wins, ANY-of among equal (rotation).
///
/// Empty result = no configured identity for the target (the verify boundary
/// maps this to a usage error).
///
/// # Errors
/// Returns the first [`TrustPolicyError`] among the *matched* policies (both or
/// neither identity form set, or an uncompilable `identity_regexp`). Non-matching
/// policies are never validated, so a malformed entry for an unrelated scope
/// never fails an unrelated verify.
pub fn resolve_tiered(
    operator: &[TrustPolicy],
    project: &[TrustPolicy],
    target: &str,
) -> Result<Vec<CompiledPolicy>, TrustPolicyError> {
    let operator_match = resolve(operator, target);
    let chosen = if operator_match.is_empty() {
        resolve(project, target)
    } else {
        operator_match
    };
    chosen.into_iter().map(TrustPolicy::compile).collect()
}

/// Extract `[[trust.policy]]` from an `ocx.toml` document leniently: sections
/// other than `[trust]` (`[tools]`, `[group.*]`, `[package.*]`, including
/// semantically-invalid entries) are ignored, so an unrelated malformed section
/// never fails trust extraction. Only a TOML *syntax* error fails.
///
/// This is the narrow OCI-tier carve-out reader for `ocx package verify` — it
/// deliberately does NOT run the full `ProjectConfig` parse (which validates
/// `[tools]` identifiers and denies unknown fields).
///
/// # Errors
/// Returns the [`toml::de::Error`] when the document is not valid TOML, or when
/// a `[[trust.policy]]` entry itself is malformed at the field level.
pub fn policies_from_ocx_toml(toml_str: &str) -> Result<Vec<TrustPolicy>, toml::de::Error> {
    #[derive(Deserialize)]
    struct ProjectTrustOnly {
        trust: Option<TrustConfig>,
    }
    let parsed: ProjectTrustOnly = toml::from_str(toml_str)?;
    Ok(parsed.trust.map(|trust| trust.policy).unwrap_or_default())
}

/// A trust-policy configuration error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrustPolicyError {
    /// Both `identity` and `identity_regexp` are set on one entry.
    #[error("trust policy for scope {scope:?} sets both identity and identity_regexp (choose one)")]
    IdentityConflict {
        /// The offending policy's scope.
        scope: String,
    },
    /// Neither `identity` nor `identity_regexp` is set on one entry.
    #[error("trust policy for scope {scope:?} sets neither identity nor identity_regexp")]
    IdentityUnset {
        /// The offending policy's scope.
        scope: String,
    },
    /// `identity_regexp` did not compile.
    #[error("trust policy for scope {scope:?} has an invalid identity_regexp")]
    InvalidRegex {
        /// The offending policy's scope.
        scope: String,
        /// The underlying regex compile error.
        #[source]
        source: regex::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(scope: &str, identity: Option<&str>, regexp: Option<&str>, issuer: &str) -> TrustPolicy {
        TrustPolicy {
            scope: scope.to_string(),
            identity: identity.map(str::to_string),
            identity_regexp: regexp.map(str::to_string),
            oidc_issuer: issuer.to_string(),
        }
    }

    #[test]
    fn literal_prefix_stops_at_wildcard() {
        assert_eq!(
            policy("ghcr.io/acme/*", None, None, "i").literal_prefix(),
            "ghcr.io/acme/"
        );
        assert_eq!(
            policy("ghcr.io/acme/tool", None, None, "i").literal_prefix(),
            "ghcr.io/acme/tool"
        );
    }

    #[test]
    fn no_wildcard_scope_matches_on_segment_boundary() {
        let scope = policy("ghcr.io/acme", Some("i"), None, "iss");
        assert!(scope.matches_scope("ghcr.io/acme"));
        assert!(scope.matches_scope("ghcr.io/acme/tool"));
        // Must NOT match a sibling repo that merely shares the prefix text.
        assert!(!scope.matches_scope("ghcr.io/acmecorp/x"));

        let tool = policy("ghcr.io/acme/tool", Some("i"), None, "iss");
        assert!(tool.matches_scope("ghcr.io/acme/tool"));
        assert!(!tool.matches_scope("ghcr.io/acme/tool-cli"));
    }

    #[test]
    fn wildcard_scope_and_empty_catch_all_still_work() {
        assert!(policy("ghcr.io/acme/*", Some("i"), None, "iss").matches_scope("ghcr.io/acme/tool"));
        assert!(!policy("ghcr.io/acme/*", Some("i"), None, "iss").matches_scope("ghcr.io/acmecorp/x"));
        assert!(policy("", Some("i"), None, "iss").matches_scope("anything/at/all"));
    }

    #[test]
    fn policies_from_ocx_toml_ignores_unrelated_malformed_sections() {
        // `[tools]` has a value that is invalid for the real ProjectConfig
        // (integer, not an identifier string) — the trust-only view ignores it,
        // so a valid [[trust.policy]] is still extracted and verify can proceed.
        let toml = r#"
[tools]
cmake = 12345

[[trust.policy]]
scope = "ghcr.io/acme/*"
identity = "id"
oidc_issuer = "iss"
"#;
        let policies = policies_from_ocx_toml(toml).expect("unrelated malformed section is ignored");
        assert_eq!(policies.len(), 1);
    }

    #[test]
    fn most_specific_scope_wins() {
        let broad = policy("ghcr.io/acme/*", Some("broad"), None, "iss");
        let narrow = policy("ghcr.io/acme/tool*", Some("narrow"), None, "iss");
        let policies = [broad, narrow];
        let resolved = resolve(&policies, "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].identity.as_deref(), Some("narrow"));
    }

    #[test]
    fn any_of_among_equal_scopes_for_rotation() {
        // Two policies at the identical winning scope: the old and new signing
        // identity coexist during a rotation window — resolution returns both.
        let old = policy("ghcr.io/acme/tool", Some("old-identity"), None, "iss");
        let new = policy("ghcr.io/acme/tool", Some("new-identity"), None, "iss");
        let policies = [old, new];
        let resolved = resolve(&policies, "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn no_matching_scope_resolves_empty() {
        let policies = [policy("ghcr.io/acme/*", Some("x"), None, "iss")];
        assert!(resolve(&policies, "ghcr.io/other/tool").is_empty());
    }

    #[test]
    fn compile_rejects_both_identity_forms() {
        let both = policy("s", Some("exact"), Some(".*"), "iss");
        assert!(matches!(both.compile(), Err(TrustPolicyError::IdentityConflict { .. })));
    }

    #[test]
    fn compile_rejects_neither_identity_form() {
        let neither = policy("s", None, None, "iss");
        assert!(matches!(neither.compile(), Err(TrustPolicyError::IdentityUnset { .. })));
    }

    #[test]
    fn compile_rejects_invalid_regex() {
        let bad = policy("s", None, Some("("), "iss");
        assert!(matches!(bad.compile(), Err(TrustPolicyError::InvalidRegex { .. })));
    }

    #[test]
    fn resolve_tiered_returns_matched_and_ignores_unrelated_malformed() {
        // A malformed policy for an UNRELATED scope must not fail resolution
        // for a target it does not cover.
        let good = policy("ghcr.io/acme/*", Some("id"), None, "iss");
        let unrelated_bad = policy("ghcr.io/other/*", Some("x"), Some("y"), "iss");
        let operator = [good, unrelated_bad];
        let compiled = resolve_tiered(&operator, &[], "ghcr.io/acme/tool").expect("only matched policies compiled");
        assert_eq!(compiled.len(), 1);
    }

    #[test]
    fn resolve_tiered_surfaces_matched_malformed_policy() {
        let operator = [policy("ghcr.io/acme/*", Some("x"), Some("y"), "iss")];
        assert!(matches!(
            resolve_tiered(&operator, &[], "ghcr.io/acme/tool"),
            Err(TrustPolicyError::IdentityConflict { .. })
        ));
    }

    #[test]
    fn operator_tier_is_authoritative_over_project() {
        // Operator config.toml pins identity X for a broad scope; the project
        // ocx.toml adds a MORE-SPECIFIC policy with identity Y. Because an
        // operator policy matches, the project override is IGNORED — verify
        // trusts only X. Security ruling: a project can never weaken an
        // operator pin.
        let operator = [policy("ghcr.io/acme/*", Some("operator-X"), None, "iss")];
        let project = [policy("ghcr.io/acme/tool", Some("project-Y"), None, "iss")];
        let compiled = resolve_tiered(&operator, &project, "ghcr.io/acme/tool").expect("operator policy compiles");
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0].identity, IdentityRule::Exact(id) if id == "operator-X"));
    }

    #[test]
    fn project_tier_adds_trust_for_ungoverned_scopes() {
        // No operator policy covers this package, so the project ocx.toml may
        // add trust for it.
        let operator = [policy("ghcr.io/acme/*", Some("operator-X"), None, "iss")];
        let project = [policy("ghcr.io/other/tool", Some("project-Z"), None, "iss")];
        let compiled = resolve_tiered(&operator, &project, "ghcr.io/other/tool").expect("project policy compiles");
        assert_eq!(compiled.len(), 1);
        assert!(matches!(&compiled[0].identity, IdentityRule::Exact(id) if id == "project-Z"));
    }

    #[test]
    fn exact_identity_is_byte_equal() {
        let rule = IdentityRule::Exact("you@example.com".to_string());
        assert!(rule.matches("you@example.com"));
        assert!(!rule.matches("you@example.com.evil.test"));
        assert!(!rule.matches("YOU@example.com"));
    }

    #[test]
    fn regex_identity_is_full_match_anchored() {
        // A substring match must NOT pass: anchoring is the whole point — an
        // unanchored `acme` would otherwise match `evil/acme-lookalike`.
        let rule = IdentityRule::compile_regex(
            r"https://github\.com/acme/[^/]+/\.github/workflows/release\.yml@refs/tags/v[0-9.]+",
        )
        .expect("valid regex");
        assert!(rule.matches("https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3"));
        // Trailing junk after the match must fail (\z anchor): `evil` is not [0-9.].
        assert!(!rule.matches("https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3-evil"));
        // Leading junk before the match must fail (\A anchor).
        assert!(!rule.matches("evil-https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1"));
    }

    #[test]
    fn trust_config_parses_array_of_tables() {
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"
identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3"
oidc_issuer = "https://token.actions.githubusercontent.com"

[[trust.policy]]
scope = "ghcr.io/other/*"
identity_regexp = "^https://example\\.com/.*$"
oidc_issuer = "https://example.com"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("parse");
        assert_eq!(root.trust.policy.len(), 2);
        assert_eq!(root.trust.policy[0].scope, "ghcr.io/acme/*");
        assert!(root.trust.policy[1].identity_regexp.is_some());
    }
}
