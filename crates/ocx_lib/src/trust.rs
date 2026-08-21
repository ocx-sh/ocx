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
//! A policy's signer matchers are nested per backend: keyless (Sigstore)
//! matchers live under `[trust.policy.keyless]`, while `scope` and the SLSA
//! `builder` pin stay top-level because neither depends on which backend
//! signed. See [`TrustPolicy`].
//!
//! Policies are declared as an array-of-tables (`[[trust.policy]]`) in the
//! operator `config.toml` (system / user / `$OCX_HOME`, which array-append into
//! one operator set) and in the project `ocx.toml`. Resolution is
//! **operator-authoritative** ([`resolve_tiered`]): if any operator policy
//! matches the target, only operator policies apply and the project `ocx.toml`
//! is ignored for that package, so a project config can never override or weaken
//! an operator pin. When no operator policy matches, the project tier applies
//! and may *add* trust for scopes the operator has not governed. Within the
//! chosen tier, resolution is most-specific-wins with **ANY-of** among
//! equal-specificity scopes, which is what makes key/workflow rotation work —
//! the old and new identity coexist during the overlap window and either one
//! passes. Specificity is measured **against the target**
//! ([`ScopeSpec::specificity_for`]): the literal prefix length for a string
//! scope, and for an include/exclude set the longest literal prefix among the
//! includes that actually matched.
//!
//! The operator tier itself pools three `config.toml` files, and one of them is
//! privileged: a policy declared at the SYSTEM scope carries
//! [`TrustPolicy::system_locked`], which makes it **admission-authoritative**
//! for the scopes it matches (see [`resolve`]). Lower tiers — including the
//! untrusted managed-config payload — can neither outbid it with a more
//! specific scope nor join its ANY-of set at the same scope: a system pin names
//! every identity that may sign, and rotation happens by editing the system
//! config that declares it.
//!
//! This module is a leaf: it must not depend on `oci`. The certificate-side
//! matching that consumes a resolved [`CompiledPolicy`] lives in
//! `oci::verify::identity`. See `.claude/artifacts/adr_trust_policy.md`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::log;

/// Container for the `[trust]` config section (`[[trust.policy]]` entries).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct TrustConfig {
    /// The declared policies. Empty when `[trust]` is present but lists none.
    #[serde(default)]
    pub policy: Vec<TrustPolicy>,

    /// Sigstore trust material for a self-hosted stack (`[trust.sigstore]`).
    ///
    /// Read from the operator `config.toml` tiers only. The project `ocx.toml`
    /// also deserializes into [`TrustConfig`], but its `sigstore` sub-table is
    /// never consulted: a repository that could name its own Fulcio CA would
    /// verify its own signatures, which is the whole trust decision.
    #[serde(default)]
    pub sigstore: Option<SigstoreTrust>,
}

impl TrustConfig {
    /// Mark every declared policy as system-scope — see
    /// [`TrustPolicy::system_locked`].
    ///
    /// Called by the config loader on the system-scope file
    /// (`/etc/ocx/config.toml`) after parsing and before folding higher tiers
    /// in. Unconditional, like
    /// [`RegistryDefaults::lock_as_system`](crate::config::RegistryDefaults::lock_as_system):
    /// `[trust]` has no opt-out field, so a system-scope policy is
    /// authoritative by itself. The flag rides on each entry rather than on the
    /// section, because trust policies array-append across tiers — the section
    /// itself does not survive the fold, the entries do.
    pub fn lock_as_system(&mut self) {
        for policy in &mut self.policy {
            policy.system_locked = true;
        }
        if let Some(sigstore) = self.sigstore.as_mut() {
            sigstore.lock_as_system();
        }
    }

    /// Merge `other` (the higher-precedence tier) into `self`.
    ///
    /// The two halves of `[trust]` merge by opposite rules, which is why this
    /// is a method and not a field-wise fold at the call site. `[[trust.policy]]`
    /// **array-appends** — every tier's entries pool into one set and masking
    /// happens at resolution time ([`resolve`]). `[trust.sigstore]` is scalar
    /// and cannot: two Fulcio CAs is not a merge, it is an ambiguity, so it
    /// **replaces** field-by-field and honours the system lock, following the
    /// [`RegistryDefaults`](crate::config::RegistryDefaults) precedent.
    pub fn merge(&mut self, other: TrustConfig) {
        self.policy.extend(other.policy);
        if let Some(other_sigstore) = other.sigstore {
            match self.sigstore.as_mut() {
                Some(sigstore) => sigstore.merge(other_sigstore),
                None => self.sigstore = Some(other_sigstore),
            }
        }
    }
}

/// The `[trust.sigstore]` sub-table: where verification gets its trust root
/// when the stack is self-hosted rather than the Sigstore public good.
///
/// Every field is optional and the whole sub-table may be absent — omitting it
/// reproduces the public-good behaviour exactly. Its purpose is fleet
/// distribution: an operator running an internal Fulcio/Rekor publishes one
/// `config.toml` through the `[managed]` tier and every machine verifies
/// against the internal CA with no env var and no file copy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SigstoreTrust {
    /// Path to a Sigstore trusted-root JSON, or a directory holding
    /// `trusted_root.json`.
    ///
    /// A relative path resolves against the directory of the `config.toml`
    /// that declared it — rewritten to absolute at load time, so the same
    /// value means the same file regardless of the process working directory.
    /// Mutually exclusive with [`Self::trusted_root_json`].
    #[serde(default)]
    pub trusted_root: Option<PathBuf>,

    /// The trusted-root document inlined verbatim.
    ///
    /// This is the form a fleet receives: `ocx config push` reads a path-form
    /// [`Self::trusted_root`] at publish time and inlines it here, because a
    /// path on the operator's disk means nothing on a consumer's. Mutually
    /// exclusive with [`Self::trusted_root`].
    #[serde(default)]
    pub trusted_root_json: Option<String>,

    /// Default Fulcio base URL for `ocx package sign` when `--fulcio-url` is
    /// omitted.
    #[serde(default)]
    pub fulcio_url: Option<String>,

    /// Default Rekor base URL for `ocx package sign` / `verify` when
    /// `--rekor-url` is omitted.
    #[serde(default)]
    pub rekor_url: Option<String>,

    /// Runtime provenance marker: declared at the SYSTEM config scope
    /// (`/etc/ocx/config.toml`), making the whole sub-table non-overridable by
    /// the user, home, or managed tiers.
    ///
    /// Never serialized on either side — set by the loader via
    /// [`TrustConfig::lock_as_system`], not read from disk. The skip is the
    /// security boundary: a managed-config payload writing
    /// `system_locked = true` parses as an unknown key and is dropped, so it
    /// cannot promote its own trust root to system authority.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

impl SigstoreTrust {
    /// Mark this sub-table as system-scope — non-overridable by lower tiers.
    ///
    /// Unconditional, like
    /// [`RegistryDefaults::lock_as_system`](crate::config::RegistryDefaults::lock_as_system):
    /// there is no opt-out field to gate on, and an operator who pins a trust
    /// root at `/etc` has said everything that needs saying.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Merge `other` (higher precedence) into `self`, field-by-field.
    ///
    /// A system-locked `self` ignores every lower-tier override — the lock is
    /// per-table, not per-field. The flag is ADOPTED from `other` for the same
    /// reason [`RegistryConfig::merge`](crate::config::RegistryConfig::merge)
    /// adopts it: the system tier folds in as `other` on the very first merge,
    /// so without the adoption the lock would be dropped before it ever
    /// applied.
    pub fn merge(&mut self, other: SigstoreTrust) {
        if self.system_locked {
            return;
        }
        self.system_locked = other.system_locked;
        // A trust root is one decision in two spellings: taking either field
        // from a higher tier must drop the other, or a tier that switches from
        // a path to an inline document would leave both set and trip the XOR.
        if other.trusted_root.is_some() || other.trusted_root_json.is_some() {
            self.trusted_root = other.trusted_root;
            self.trusted_root_json = other.trusted_root_json;
        }
        if other.fulcio_url.is_some() {
            self.fulcio_url = other.fulcio_url;
        }
        if other.rekor_url.is_some() {
            self.rekor_url = other.rekor_url;
        }
    }

    /// Rewrite a relative [`Self::trusted_root`] to be absolute against
    /// `config_dir` — the directory of the `config.toml` that declared it.
    ///
    /// Called by the config loader once per file tier, so `/etc/ocx/config.toml`
    /// and `$OCX_HOME/config.toml` each anchor their own relative paths and the
    /// process working directory never enters into it.
    pub fn anchor_relative_root(&mut self, config_dir: &std::path::Path) {
        if let Some(path) = self.trusted_root.as_ref()
            && path.is_relative()
        {
            self.trusted_root = Some(config_dir.join(path));
        }
    }
}

/// The `scope` value of a `[[trust.policy]]` entry: one prefix pattern, or an
/// include/exclude set of them.
///
/// ```toml
/// scope = "ghcr.io/acme/*"
/// scope = { include = ["ghcr.io/acme/*"], exclude = ["ghcr.io/acme/experimental/*"] }
/// ```
///
/// Both forms are built from the same per-pattern rule — a pattern with no `*`
/// matches on `/`-separated path-segment boundaries, a pattern with one is a
/// literal-prefix glob, an empty pattern matches everything (see
/// [`pattern_matches`]). The set form only says how several of them combine: a
/// target matches when it matches at least one `include` (or `include` is
/// empty, which reads as a catch-all) and no `exclude`. `exclude` therefore
/// beats `include` whenever both match, which is what makes the headline
/// carve-out — govern a whole registry, exempt one subtree — expressible in one
/// entry.
///
/// A table must carry `include` or `exclude`; one naming neither is refused
/// rather than read as a catch-all. Unknown keys are still dropped (fleet
/// forward-compat), so without that floor a typo'd `includ` would parse to an
/// empty table and silently widen a narrow pin to every package — the failure
/// direction that loses trust instead of failing loudly. `scope = ""`, or
/// omitting `scope`, is the catch-all spelling.
///
/// No regex form: `identity_regexp` is the only regex surface in `[trust]`, and
/// a scope pattern picks which packages a pin *covers*, where an over-broad
/// pattern silently widens trust instead of failing loudly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ScopeSpec {
    /// The string form: one prefix pattern.
    Prefix(String),
    /// The object form: patterns to cover, minus patterns to carve out.
    ///
    /// One of the two keys is required; the other defaults to empty. A table
    /// carrying only keys a newer ocx understands is therefore refused, not
    /// read as an accidental catch-all — the one place the fleet forward-compat
    /// tolerance [`TrustConfig`] describes stops, because here dropping the
    /// unknown key would *widen* trust rather than narrow it.
    Set {
        /// Patterns the policy covers. Empty is a catch-all.
        include: Vec<String>,
        /// Patterns carved back out. Beats [`Self::Set::include`].
        exclude: Vec<String>,
    },
}

impl schemars::JsonSchema for ScopeSpec {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ScopeSpec")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Hand-written for the same reason `ProjectEnv`'s is: a derive reads
        // the Rust type, so it cannot see what the hand-rolled `Deserialize`
        // above actually accepts. Two divergences would otherwise ship — the
        // derive emits `anyOf` where the shared union helper emits `oneOf`,
        // and it cannot express "one of `include`/`exclude` is required",
        // which is the whole point of the refusal. An editor bound to this
        // schema would then show no error for `scope = {}` and ocx would exit
        // 78 on the same file.
        crate::utility::schema::string_or_table(
            "One scope pattern. Segment-bounded without a `*`, literal-prefix glob with one; the empty string is a catch-all.",
            serde_json::json!({
                "type": "object",
                "description": "Patterns to cover, minus patterns to carve back out. A target matches when it matches at least one `include` (an empty `include` is a catch-all) and no `exclude`.",
                "properties": {
                    "include": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Patterns the policy covers. Empty is a catch-all."
                    },
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Patterns carved back out. Beats `include` wherever both match."
                    }
                },
                "anyOf": [
                    { "required": ["include"] },
                    { "required": ["exclude"] }
                ]
            }),
        )
    }
}

impl<'de> Deserialize<'de> for ScopeSpec {
    /// Hand-written rather than `#[serde(untagged)]`, for two reasons the derive
    /// cannot give: a malformed value reports what a scope may be instead of
    /// `data did not match any variant of untagged enum ScopeSpec`, and a table
    /// naming neither `include` nor `exclude` is refused instead of defaulting
    /// both lists to empty and becoming a catch-all.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct ScopeSpecVisitor;

        impl<'de> serde::de::Visitor<'de> for ScopeSpecVisitor {
            type Value = ScopeSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter
                    .write_str("a scope pattern string, or a table with an `include` and/or `exclude` list of them")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ScopeSpec::Prefix(value.to_string()))
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut include: Option<Vec<String>> = None;
                let mut exclude: Option<Vec<String>> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "include" => include = Some(map.next_value()?),
                        "exclude" => exclude = Some(map.next_value()?),
                        // Fleet forward-compat, same as one level up: a key a
                        // newer ocx added is dropped, never a hard failure.
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                if include.is_none() && exclude.is_none() {
                    return Err(serde::de::Error::custom(
                        "a table scope needs `include` or `exclude`; write `scope = \"\"`, or omit `scope`, for a \
                         catch-all",
                    ));
                }
                Ok(ScopeSpec::Set {
                    include: include.unwrap_or_default(),
                    exclude: exclude.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_any(ScopeSpecVisitor)
    }
}

/// Whether one scope pattern matches the canonical `registry/repository`
/// target.
///
/// A no-wildcard pattern matches on **path-segment boundaries**: `ghcr.io/acme`
/// matches `ghcr.io/acme` and `ghcr.io/acme/tool`, but never `ghcr.io/acmecorp`.
/// A `*` makes it a glob on the literal prefix before the wildcard
/// (`ghcr.io/acme/*` covers the subtree; a bare `ghcr.io/acme*` is an
/// intentional substring glob). An empty pattern is a catch-all.
#[must_use]
pub fn pattern_matches(pattern: &str, target: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    match pattern.find('*') {
        Some(index) => target.starts_with(&pattern[..index]),
        None => target == pattern || target.starts_with(&format!("{pattern}/")),
    }
}

/// The literal prefix of one scope pattern: everything before the first `*`
/// (the whole pattern when there is no wildcard). Its length is the pattern's
/// specificity.
fn pattern_literal_prefix(pattern: &str) -> &str {
    match pattern.find('*') {
        Some(index) => &pattern[..index],
        None => pattern,
    }
}

impl ScopeSpec {
    /// Whether this scope matches the canonical `registry/repository` target.
    #[must_use]
    pub fn matches(&self, target: &str) -> bool {
        match self {
            Self::Prefix(pattern) => pattern_matches(pattern, target),
            Self::Set { include, exclude } => {
                let covered = include.is_empty() || include.iter().any(|pattern| pattern_matches(pattern, target));
                covered && !exclude.iter().any(|pattern| pattern_matches(pattern, target))
            }
        }
    }

    /// How specifically this scope matches `target` — the resolution rank
    /// [`resolve`] takes its winning level from.
    ///
    /// Per-target, not a property of the scope alone: a set can match one
    /// target through a long `include` and another through a short one, and the
    /// rank must reflect which pattern actually did the covering. For a string
    /// scope that collapses to the literal-prefix length, unchanged. Excludes
    /// never contribute — they subtract coverage, and letting a carve-out raise
    /// a policy's rank would let one lower-tier `exclude` outbid the pin it was
    /// carving out of.
    #[must_use]
    pub fn specificity_for(&self, target: &str) -> usize {
        match self {
            Self::Prefix(pattern) => pattern_literal_prefix(pattern).len(),
            Self::Set { include, .. } => include
                .iter()
                .filter(|pattern| pattern_matches(pattern, target))
                .map(|pattern| pattern_literal_prefix(pattern).len())
                .max()
                // No include matched: the set covered this target as a
                // catch-all, which ranks 0 exactly like `scope = ""`.
                .unwrap_or_default(),
        }
    }
}

impl std::fmt::Display for ScopeSpec {
    /// Renders a scope for the one place it reaches a human: a
    /// [`TrustPolicyError`]'s `scope` field, and the refused-entry debug log in
    /// [`resolve`]. The string form is verbatim, so every existing diagnostic
    /// reads exactly as before.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prefix(pattern) => formatter.write_str(pattern),
            Self::Set { include, exclude } => {
                write!(
                    formatter,
                    "include=[{}] exclude=[{}]",
                    include.join(", "),
                    exclude.join(", ")
                )
            }
        }
    }
}

/// A single `[[trust.policy]]` entry.
///
/// The matchers naming an acceptable signer are **nested per backend**: keyless
/// (Sigstore/Fulcio) matchers live under `[trust.policy.keyless]`, and a future
/// key-based backend gets its own `[trust.policy.key]` sub-table beside it.
/// [`Self::scope`] and [`Self::builder`] stay top-level because neither depends
/// on which backend produced the signature.
///
/// ```toml
/// [[trust.policy]]
/// scope = "ghcr.io/acme/*"
/// # or, to carve a subtree back out:
/// # scope = { include = ["ghcr.io/acme/*"], exclude = ["ghcr.io/acme/experimental/*"] }
///
/// [trust.policy.keyless]
/// identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3"
/// oidc_issuer = "https://token.actions.githubusercontent.com"
/// ```
///
/// Every field is optional at the serde layer, for the same fleet
/// forward-compat reason unknown keys are tolerated (see [`TrustConfig`]): an
/// entry written by a newer ocx degrades to its known parts instead of failing
/// the whole file. What a *resolved* policy is allowed to mean is narrowed at
/// [`TrustPolicy::compile`] instead, which refuses an entry declaring no
/// backend. That is also what makes a flat-form typo loud: the pre-nesting
/// spelling parses as unknown keys, leaves no backend behind, and fails closed
/// with [`TrustPolicyError::NoBackend`] naming the sub-table it expected.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct TrustPolicy {
    /// Which packages this policy applies to — one prefix pattern, or an
    /// include/exclude set of them ([`ScopeSpec`]).
    ///
    /// A pattern with no `*` matches the target's canonical
    /// `registry/repository` on `/`-separated path-segment boundaries:
    /// `ghcr.io/acme/tool` matches the exact repo `ghcr.io/acme/tool` and any
    /// sub-path under it (`ghcr.io/acme/tool/plugin`), but NOT a sibling that
    /// merely shares the prefix text — `ghcr.io/acme/tool` does **not** cover
    /// `ghcr.io/acme/tool-cli`. A trailing `/*` covers the subtree; a mid-string
    /// `*` is a literal-prefix substring glob (see [`pattern_matches`]).
    ///
    /// ```toml
    /// scope = "ghcr.io/acme/*"
    /// scope = { include = ["ghcr.io/acme/*"], exclude = ["ghcr.io/acme/experimental/*"] }
    /// ```
    ///
    /// Absent reads exactly like `""`: a catch-all. So does an object form with
    /// an empty `include` and no `exclude`.
    #[serde(default)]
    pub scope: Option<ScopeSpec>,

    /// Expected SLSA provenance builder identity, matched against the
    /// provenance predicate during attestation verify.
    ///
    /// Backend-independent — it names who *built* the artifact, not how the
    /// signature was made — so it is a sibling of [`Self::scope`] rather than a
    /// member of a backend sub-table. Inert in signature mode: a pin on a
    /// policy that never verifies provenance is forward configuration, not an
    /// error.
    #[serde(default)]
    pub builder: Option<String>,

    /// The keyless (Sigstore) matcher sub-table, `[trust.policy.keyless]`.
    #[serde(default)]
    pub keyless: Option<KeylessMatcher>,

    // A key-based backend lands here as `pub key: Option<KeyMatcher>` beside a
    // `PolicyBackend::Key` variant. It is deliberately not parsed yet: a config
    // surface with no verifier behind it is a promise that reads as a feature.
    /// Runtime provenance marker: this policy was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`), so it pins the specificity level for the
    /// scopes it matches — a lower tier can join its ANY-of set only at equal
    /// specificity, never outbid it with a narrower scope (see [`resolve`]).
    /// Mirrors [`RegistryDefaults`](crate::config::RegistryDefaults)'s lock, and
    /// is unconditional for the same reason: `[trust]` has no opt-out field.
    ///
    /// Never serialized on either side — set by the loader via
    /// [`TrustConfig::lock_as_system`] after parsing the system-scope file, not
    /// read from disk. The skip is the security boundary, not a formatting
    /// choice: a managed-config payload that writes `system_locked = true` is
    /// parsed as an unknown key and dropped, so it cannot promote itself.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

/// The `[trust.policy.keyless]` sub-table: which Sigstore identity may sign.
///
/// Exactly one of `identity` / `identity_regexp` must be set — both or neither
/// is a configuration error surfaced by [`TrustPolicy::compile`] (cosign's
/// `--certificate-identity` / `--certificate-identity-regexp` precedent) — and
/// `oidc_issuer` must be present. All three are `Option` at the serde layer and
/// mandatory at compile, for the tolerance reason [`TrustPolicy`] states.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct KeylessMatcher {
    /// Exact expected certificate SAN (byte-equal). Mutually exclusive with
    /// [`Self::identity_regexp`].
    #[serde(default)]
    pub identity: Option<String>,

    /// Regex the certificate SAN must match in full (anchored `\A…\z`).
    /// Mutually exclusive with [`Self::identity`].
    #[serde(default)]
    pub identity_regexp: Option<String>,

    /// Exact expected OIDC issuer URL (byte-equal).
    #[serde(default)]
    pub oidc_issuer: Option<String>,
}

impl TrustPolicy {
    /// The declared scope rendered for a diagnostic, with an absent one read as
    /// the empty catch-all.
    fn scope_label(&self) -> String {
        self.scope.as_ref().map(ScopeSpec::to_string).unwrap_or_default()
    }

    /// Whether this policy's scope matches the canonical `registry/repository`
    /// target. An absent scope is a catch-all.
    #[must_use]
    pub fn matches_scope(&self, target: &str) -> bool {
        self.scope.as_ref().is_none_or(|scope| scope.matches(target))
    }

    /// How specifically this policy matches `target` — the rank [`resolve`]
    /// picks its winning level from. An absent scope ranks 0, like `""`.
    #[must_use]
    pub fn specificity_for(&self, target: &str) -> usize {
        self.scope.as_ref().map_or(0, |scope| scope.specificity_for(target))
    }

    /// Resolve this entry into the ready-to-match [`CompiledPolicy`]: pick the
    /// declared backend and enforce that backend's invariants.
    ///
    /// # Errors
    /// [`TrustPolicyError::NoBackend`] when the entry declares no backend
    /// sub-table; from a `[trust.policy.keyless]` table,
    /// [`TrustPolicyError::IdentityConflict`] when both identity forms are set,
    /// [`TrustPolicyError::IdentityUnset`] when neither is,
    /// [`TrustPolicyError::IssuerUnset`] when `oidc_issuer` is absent, and
    /// [`TrustPolicyError::InvalidRegex`] when `identity_regexp` does not
    /// compile.
    pub fn compile(&self) -> Result<CompiledPolicy, TrustPolicyError> {
        // Exactly one backend is a property of the type, not of a runtime
        // count: one `Option` field can only declare one, and a second backend
        // arrives as another field here plus a `PolicyBackend` variant, whose
        // exhaustive matches then force every consumer to be revisited. A
        // "more than one declared" refusal is written with that field, against
        // a config that can express the conflict — written now it could never
        // fail, which is not a check.
        let keyless = self.keyless.as_ref().ok_or_else(|| TrustPolicyError::NoBackend {
            scope: self.scope_label(),
        })?;
        Ok(CompiledPolicy {
            builder: self.builder.clone(),
            backend: PolicyBackend::Keyless(self.compile_keyless(keyless)?),
        })
    }

    /// Enforce the keyless invariants: identity XOR identity_regexp, and an
    /// issuer present.
    fn compile_keyless(&self, keyless: &KeylessMatcher) -> Result<CompiledKeyless, TrustPolicyError> {
        let identity = match (&keyless.identity, &keyless.identity_regexp) {
            (Some(_), Some(_)) => {
                return Err(TrustPolicyError::IdentityConflict {
                    scope: self.scope_label(),
                });
            }
            (None, None) => {
                return Err(TrustPolicyError::IdentityUnset {
                    scope: self.scope_label(),
                });
            }
            (Some(exact), None) => IdentityRule::Exact(exact.clone()),
            (None, Some(pattern)) => {
                IdentityRule::compile_regex(pattern).map_err(|source| TrustPolicyError::InvalidRegex {
                    scope: self.scope_label(),
                    source,
                })?
            }
        };
        let issuer = keyless
            .oidc_issuer
            .clone()
            .ok_or_else(|| TrustPolicyError::IssuerUnset {
                scope: self.scope_label(),
            })?;
        Ok(CompiledKeyless { identity, issuer })
    }
}

/// A compiled, ready-to-match policy: the acceptable signer, plus the pins that
/// hold whichever backend signed.
#[derive(Debug, Clone)]
pub struct CompiledPolicy {
    /// The SLSA provenance builder identity this policy pins, if any. Enforced
    /// by attestation verify; inert in signature mode.
    pub builder: Option<String>,
    /// The verification backend this policy resolved to.
    pub backend: PolicyBackend,
}

/// The verification backend a compiled policy resolved to.
///
/// One variant today. It is an enum rather than a set of optional fields so
/// that adding a key-based backend is a compile error at every site that
/// consumes a policy, instead of a silent `None` nobody handles. Deliberately
/// not `#[non_exhaustive]`: the binary is the only consumer, and in-crate
/// matches staying total is the whole point.
#[derive(Debug, Clone)]
pub enum PolicyBackend {
    /// Keyless Sigstore: a Fulcio certificate SAN plus its OIDC issuer.
    Keyless(CompiledKeyless),
}

/// A compiled `[trust.policy.keyless]` matcher.
#[derive(Debug, Clone)]
pub struct CompiledKeyless {
    /// The identity constraint (exact or anchored regex).
    pub identity: IdentityRule,
    /// The exact expected OIDC issuer URL.
    pub issuer: String,
}

impl CompiledPolicy {
    /// Build a single exact keyless `(identity, issuer)` policy — the
    /// flag-override path (`--certificate-identity` +
    /// `--certificate-oidc-issuer`). It carries no builder pin: the flags name
    /// a signer, not a build.
    #[must_use]
    pub fn exact(identity: String, issuer: String) -> Self {
        Self {
            builder: None,
            backend: PolicyBackend::Keyless(CompiledKeyless {
                identity: IdentityRule::Exact(identity),
                issuer,
            }),
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
/// target: the matching policies at the **winning specificity level**, returned
/// as a set for ANY-of evaluation. Empty when no scope matches.
///
/// The winning level is the highest [per-target specificity][ScopeSpec::specificity_for]
/// among the matching policies (most-specific-wins) — **unless** a
/// [system-locked][TrustPolicy::system_locked] policy matches, in which case
/// only the locked policies govern the target at all: the level is the highest
/// specificity among them, and every unlocked match is dropped whatever its
/// scope. A system pin is therefore
/// admission-authoritative, not a specificity floor. Equal-scope array-append
/// across tiers is otherwise a signer-enrollment channel — a user-writable
/// `config.toml`, or the untrusted managed payload, could add its own identity
/// to the operator's ANY-of set and every covered package would verify against
/// it. Rotation for a locked scope is done in the system config that owns the
/// pin, where old and new identity coexist as two locked entries.
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
    let locked_pin = matching
        .iter()
        .filter(|policy| policy.system_locked)
        .max_by_key(|policy| policy.specificity_for(target));
    let locked_max = locked_pin.map(|policy| policy.specificity_for(target));
    let Some(level) = locked_max.or_else(|| matching.iter().map(|policy| policy.specificity_for(target)).max()) else {
        return Vec::new();
    };
    // A matching system pin governs the target alone. Without the log the
    // operator sees only an eventual IdentityMismatch, with nothing naming the
    // pin that discarded the entry they authored. Silent on the no-op path: a
    // pin that drops nothing is the ordinary case.
    if let Some(pin) = locked_pin {
        let refused: Vec<String> = matching
            .iter()
            .filter(|policy| !policy.system_locked)
            .map(|policy| policy.scope_label())
            .collect();
        if !refused.is_empty() {
            log::debug!(
                "system-locked trust scope '{}' governs '{target}' alone; lower-tier scopes {refused:?} are \
                 refused — rotate the identity in the system config that declares the pin",
                pin.scope_label()
            );
        }
        return matching
            .into_iter()
            .filter(|policy| policy.system_locked && policy.specificity_for(target) == level)
            .collect();
    }
    matching
        .into_iter()
        .filter(|policy| policy.specificity_for(target) == level)
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
/// scope wins, ANY-of among equal (rotation) — except that a system-locked
/// policy pins the specificity level for its scopes, so the lower `config.toml`
/// tiers pooled into `operator` cannot outbid it either ([`resolve`]).
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
    /// The entry declares no backend sub-table, so it names no acceptable
    /// signer. Also what the pre-nesting flat spelling degrades to: its keys
    /// are tolerated as unknown, and this is where the entry fails closed.
    #[error(
        "trust policy for scope {scope:?} declares no verification backend (expected a [trust.policy.keyless] sub-table)"
    )]
    NoBackend {
        /// The offending policy's scope.
        scope: String,
    },
    /// A `[trust.policy.keyless]` table omits `oidc_issuer`.
    #[error("trust policy for scope {scope:?} sets no oidc_issuer under [trust.policy.keyless]")]
    IssuerUnset {
        /// The offending policy's scope.
        scope: String,
    },
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
    use std::path::Path;

    use super::*;

    fn policy(scope: &str, identity: Option<&str>, regexp: Option<&str>, issuer: &str) -> TrustPolicy {
        TrustPolicy {
            scope: Some(ScopeSpec::Prefix(scope.to_string())),
            builder: None,
            keyless: Some(KeylessMatcher {
                identity: identity.map(str::to_string),
                identity_regexp: regexp.map(str::to_string),
                oidc_issuer: Some(issuer.to_string()),
            }),
            system_locked: false,
        }
    }

    /// The exact identity a parsed policy pins — for assertions that only care
    /// which entry won resolution.
    fn pinned_identity(policy: &TrustPolicy) -> Option<&str> {
        policy.keyless.as_ref()?.identity.as_deref()
    }

    /// The identity rule a compiled policy resolved to. The irrefutable
    /// destructure is deliberate: a second [`PolicyBackend`] variant must break
    /// this line rather than silently skip it.
    fn compiled_identity(policy: &CompiledPolicy) -> &IdentityRule {
        let PolicyBackend::Keyless(keyless) = &policy.backend;
        &keyless.identity
    }

    /// The same entry as [`policy`], but marked as declared at the system config
    /// scope — what `TrustConfig::lock_as_system` does to a parsed system tier.
    fn locked_policy(scope: &str, identity: &str, issuer: &str) -> TrustPolicy {
        TrustPolicy {
            system_locked: true,
            ..policy(scope, Some(identity), None, issuer)
        }
    }

    #[test]
    fn specificity_is_the_literal_prefix_length_and_stops_at_the_wildcard() {
        // The rank a string scope contributes to `resolve`: everything before
        // the first `*`, or the whole scope when there is none.
        assert_eq!(
            policy("ghcr.io/acme/*", None, None, "i").specificity_for("ghcr.io/acme/tool"),
            "ghcr.io/acme/".len()
        );
        assert_eq!(
            policy("ghcr.io/acme/tool", None, None, "i").specificity_for("ghcr.io/acme/tool"),
            "ghcr.io/acme/tool".len()
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

    /// An object-scope policy pinning `identity`, built without going through TOML.
    fn set_policy(include: &[&str], exclude: &[&str], identity: &str) -> TrustPolicy {
        TrustPolicy {
            scope: Some(ScopeSpec::Set {
                include: include.iter().map(|pattern| (*pattern).to_string()).collect(),
                exclude: exclude.iter().map(|pattern| (*pattern).to_string()).collect(),
            }),
            ..policy("", Some(identity), None, "iss")
        }
    }

    #[test]
    fn both_scope_forms_deserialize_and_an_object_survives_an_unknown_key() {
        // The untagged enum has to admit the string form verbatim AND the
        // object form, and the object form inherits the fleet forward-compat
        // tolerance the rest of `[trust]` has: a key a newer ocx added must
        // degrade to the known parts, not fail the entry — otherwise one
        // fleet-distributed config bricks every older binary that reads it.
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity = "a"
oidc_issuer = "iss"

[[trust.policy]]
scope = { include = ["ghcr.io/acme/*", "ocx.sh/cmake"], exclude = ["ghcr.io/acme/experimental/*"], future_key = "newer ocx" }

[trust.policy.keyless]
identity = "b"
oidc_issuer = "iss"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("both forms parse, unknown key tolerated");
        assert_eq!(
            root.trust.policy[0].scope,
            Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string()))
        );
        assert_eq!(
            root.trust.policy[1].scope,
            Some(ScopeSpec::Set {
                include: vec!["ghcr.io/acme/*".to_string(), "ocx.sh/cmake".to_string()],
                exclude: vec!["ghcr.io/acme/experimental/*".to_string()],
            }),
            "the unknown key is dropped and the known parts survive intact"
        );
    }

    #[test]
    fn policies_from_ocx_toml_reads_the_object_scope_form() {
        // The lenient project-tier reader shares the serde surface, so this
        // rides along for free — pinned because "for free" is exactly the kind
        // of coupling a later refactor can quietly sever.
        let toml = r#"
[[trust.policy]]
scope = { include = ["ocx.sh/*"], exclude = ["ocx.sh/experimental"] }

[trust.policy.keyless]
identity = "id"
oidc_issuer = "iss"
"#;
        let policies = policies_from_ocx_toml(toml).expect("the object form parses in an ocx.toml");
        assert_eq!(policies.len(), 1);
        assert!(policies[0].matches_scope("ocx.sh/cmake"));
        assert!(!policies[0].matches_scope("ocx.sh/experimental"));
    }

    #[test]
    fn include_is_any_of_and_exclude_beats_it() {
        // Two unrelated subtrees under one pin (ANY-of), minus a carve-out that
        // sits INSIDE one of them — so `exclude` and `include` both match the
        // carved target and `exclude` has to win. Ordering the other way would
        // make every carve-out inert while still parsing.
        let scope = set_policy(
            &["ghcr.io/acme/*", "ocx.sh/cmake"],
            &["ghcr.io/acme/experimental/*"],
            "ci",
        );
        assert!(scope.matches_scope("ghcr.io/acme/tool"), "first include matches");
        assert!(scope.matches_scope("ocx.sh/cmake"), "second include matches");
        assert!(!scope.matches_scope("ghcr.io/other/tool"), "neither include matches");
        assert!(
            !scope.matches_scope("ghcr.io/acme/experimental/thing"),
            "exclude beats a matching include"
        );
    }

    #[test]
    fn exclude_patterns_honour_segment_boundaries() {
        // An exclude uses the SAME per-pattern rule as a string scope, so a
        // no-wildcard exclude must not carve out a sibling that merely shares
        // the prefix text. Getting this wrong silently un-governs a package
        // nobody named — the failure direction that loses trust, not the one
        // that fails loudly.
        let scope = set_policy(&["ghcr.io/acme/*"], &["ghcr.io/acme/tool"], "ci");
        assert!(
            !scope.matches_scope("ghcr.io/acme/tool"),
            "the exact repo is carved out"
        );
        assert!(
            !scope.matches_scope("ghcr.io/acme/tool/plugin"),
            "and so is everything under it"
        );
        assert!(
            scope.matches_scope("ghcr.io/acme/tool-cli"),
            "but NOT a sibling sharing the prefix text"
        );
    }

    #[test]
    fn specificity_is_measured_per_target_against_the_matching_include() {
        // A set has no single literal prefix — only the include that actually
        // covered THIS target does. Both directions are asserted from one
        // policy pair, because a per-scope (target-blind) rank would have to
        // pick one number for the whole set and would therefore get one of the
        // two targets wrong.
        let broad = policy("ghcr.io/*", Some("broad"), None, "iss");
        let set = set_policy(&["ghcr.io/acme/tool", "ghcr.io"], &[], "set");

        let resolved = resolve([&broad, &set], "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            pinned_identity(resolved[0]),
            Some("set"),
            "the long include (17) outbids the string scope (8) for this target"
        );

        let resolved = resolve([&broad, &set], "ghcr.io/x/thing");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            pinned_identity(resolved[0]),
            Some("broad"),
            "the same set matches the other target through a SHORT include (7), \
             so the string scope (8) wins there — one rank per set could not do both"
        );
    }

    #[test]
    fn an_exclude_never_raises_a_policys_specificity() {
        // Excludes subtract coverage; letting one contribute to the rank would
        // hand a lower tier a way to outbid a pin with a long carve-out string
        // that governs nothing. The set below matches only via its empty
        // include (rank 0), so the len-8 string scope must win outright.
        let broad = policy("ghcr.io/*", Some("broad"), None, "iss");
        let carve = set_policy(&[], &["ghcr.io/acme/experimental/a-very-long-carve-out"], "carve");
        let resolved = resolve([&broad, &carve], "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 1);
        assert_eq!(pinned_identity(resolved[0]), Some("broad"));
    }

    #[test]
    fn a_system_locked_object_scope_still_governs_its_targets_alone() {
        // The twin of `system_locked_pin_refuses_a_more_specific_unlocked_entry`
        // with the pin written in the object form: the lock rides on the entry,
        // not on how its scope is spelled, so a narrower lower-tier entry is
        // refused exactly as before. Its carve-out is still honoured — an
        // excluded target is simply not one the pin matches, so the lower tier
        // governs there with full authority.
        let system = TrustPolicy {
            system_locked: true,
            ..set_policy(&["ghcr.io/acme/*"], &["ghcr.io/acme/experimental/*"], "system-X")
        };
        let managed = policy("ghcr.io/acme/tool", Some("managed-Y"), None, "iss");

        let resolved = resolve([&system, &managed], "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 1, "the locked object scope governs alone");
        assert_eq!(pinned_identity(resolved[0]), Some("system-X"));

        let carved = policy("ghcr.io/acme/experimental/thing", Some("project-Z"), None, "iss");
        let resolved = resolve([&system, &carved], "ghcr.io/acme/experimental/thing");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            pinned_identity(resolved[0]),
            Some("project-Z"),
            "the pin does not match a target it carved out, so it locks nothing there"
        );
    }

    #[test]
    fn an_object_scope_renders_compactly_in_a_refusal() {
        // `TrustPolicyError` carries the scope as text, so the object form
        // needs a rendering — and the string form must still come through
        // verbatim, or every existing diagnostic changes wording.
        let bare = TrustPolicy {
            keyless: None,
            ..set_policy(&["ghcr.io/acme/*"], &["ghcr.io/acme/experimental/*"], "unused")
        };
        let rendered = bare.compile().expect_err("no backend").to_string();
        assert!(
            rendered.contains("include=[ghcr.io/acme/*] exclude=[ghcr.io/acme/experimental/*]"),
            "the refusal must name which entry is at fault; got: {rendered}"
        );
        assert_eq!(
            ScopeSpec::Prefix("ghcr.io/acme/*".to_string()).to_string(),
            "ghcr.io/acme/*",
            "the string form renders verbatim"
        );
    }

    #[test]
    fn an_include_free_object_scope_is_a_catch_all_minus_its_exclusions() {
        // The headline carve-out: trust everything, except one subtree. An
        // empty `include` must not read as "matches nothing" — that inversion
        // would silently un-govern the entire fleet.
        let toml = r#"
[[trust.policy]]
scope = { exclude = ["ghcr.io/acme/experimental/*"] }

[trust.policy.keyless]
identity = "ci@acme.example"
oidc_issuer = "iss"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("an object scope parses");
        let policies = root.trust.policy;
        assert_eq!(
            resolve(&policies, "ghcr.io/other/tool").len(),
            1,
            "catch-all still covers everything else"
        );
        assert_eq!(
            resolve(&policies, "ghcr.io/acme/tool").len(),
            1,
            "a sibling of the carve-out stays governed"
        );
        assert!(
            resolve(&policies, "ghcr.io/acme/experimental/thing").is_empty(),
            "the excluded subtree must fall through resolve() to an empty set"
        );
    }

    #[test]
    fn a_table_scope_naming_neither_list_is_refused_not_read_as_a_catch_all() {
        // The one place the fleet forward-compat tolerance stops. Dropping an
        // unknown key normally narrows nothing, but inside a scope table it
        // would leave both lists empty — turning a typo'd `includ` into a pin
        // over every package. Refusing costs no expressiveness: `scope = ""`
        // and an absent `scope` both already spell the catch-all.
        for scope in [
            r#"scope = { includ = ["ghcr.io/acme/*"] }"#,
            r#"scope = { future_only = "added by a newer ocx" }"#,
            r#"scope = {}"#,
        ] {
            let toml = format!(
                "[[trust.policy]]\n{scope}\n\n[trust.policy.keyless]\nidentity = \"id\"\noidc_issuer = \"iss\"\n"
            );
            let error = policies_from_ocx_toml(&toml)
                .expect_err("a table naming neither list must not parse")
                .to_string();
            assert!(
                error.contains("needs `include` or `exclude`"),
                "the refusal must name the fix; got: {error}"
            );
        }

        // The floor is "one recognized key", not "no unknown keys" — an unknown
        // key riding ALONGSIDE a real one still degrades to the known parts.
        let toml = "[[trust.policy]]\nscope = { include = [\"ghcr.io/acme/*\"], future_key = 1 }\n\n[trust.policy.keyless]\nidentity = \"id\"\noidc_issuer = \"iss\"\n";
        let policies = policies_from_ocx_toml(toml).expect("an unknown key beside a real one is tolerated");
        assert_eq!(
            policies[0].scope,
            Some(ScopeSpec::Set {
                include: vec!["ghcr.io/acme/*".to_string()],
                exclude: Vec::new(),
            })
        );
    }

    #[test]
    fn a_malformed_scope_names_the_two_accepted_forms() {
        // `#[serde(untagged)]` reports only "data did not match any variant of
        // untagged enum ScopeSpec" for every one of these, which tells an
        // operator nothing about what to write instead.
        for scope in [
            "scope = 42",
            "scope = true",
            r#"scope = ["a", "b"]"#,
            r#"scope = { include = "notalist" }"#,
        ] {
            let toml = format!(
                "[[trust.policy]]\n{scope}\n\n[trust.policy.keyless]\nidentity = \"id\"\noidc_issuer = \"iss\"\n"
            );
            let error = policies_from_ocx_toml(&toml)
                .expect_err("a malformed scope must not parse")
                .to_string();
            assert!(
                !error.contains("untagged"),
                "the untagged-enum wording must not reach an operator; got: {error}"
            );
        }

        // The three scalar shapes reach the visitor and get the full sentence;
        // a bad `include` element type is serde's own typed message one level
        // down, which is already actionable, so it is asserted separately.
        let toml = "[[trust.policy]]\nscope = 42\n\n[trust.policy.keyless]\nidentity = \"id\"\noidc_issuer = \"iss\"\n";
        let error = policies_from_ocx_toml(toml)
            .expect_err("an integer scope must not parse")
            .to_string();
        assert!(
            error.contains("scope pattern string") && error.contains("include"),
            "the refusal must name both accepted forms; got: {error}"
        );
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

[trust.policy.keyless]
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
        assert_eq!(pinned_identity(resolved[0]), Some("narrow"));
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
    fn system_locked_pin_refuses_a_more_specific_unlocked_entry() {
        // The security case: an untrusted managed-config payload (or any lower
        // tier) declares a NARROWER scope than the system pin, naming its own
        // identity. Pure most-specific-wins would let it displace the pin for
        // that package; the system lock fixes the specificity level at 13, so
        // the len-17 entry is refused outright.
        let system = locked_policy("ghcr.io/acme/*", "system-X", "iss");
        let managed = policy("ghcr.io/acme/tool", Some("managed-Y"), None, "iss");
        let policies = [system, managed];
        let resolved = resolve(&policies, "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 1, "only the locked pin may govern the scope it matches");
        assert_eq!(pinned_identity(resolved[0]), Some("system-X"));
    }

    #[test]
    fn equal_specificity_entry_cannot_join_the_locked_any_of_set() {
        // The enrollment case. An equal-scope entry from a lower tier is the
        // one shape a specificity floor cannot refuse, which would make
        // array-append a signer-enrollment channel: a user-writable
        // `config.toml` (or the untrusted managed payload) declaring the system
        // pin's own scope with its own identity, and every covered package then
        // verifying against it under ANY-of. The pin governs alone instead.
        let system = locked_policy("ghcr.io/acme/*", "old-ci", "iss");
        let managed = policy("ghcr.io/acme/*", Some("attacker"), None, "iss");
        let policies = [system, managed];
        let resolved = resolve(&policies, "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 1, "only system-scope entries govern a locked scope");
        assert_eq!(pinned_identity(resolved[0]), Some("old-ci"));
    }

    #[test]
    fn rotation_under_a_locked_scope_declares_both_identities_in_the_system_tier() {
        // The rotation case, relocated: old and new identity coexist as two
        // SYSTEM entries during the overlap window. Rotation keeps working; it
        // just cannot be driven from a tier the operator does not control.
        let old = locked_policy("ghcr.io/acme/*", "old-ci", "iss");
        let new = locked_policy("ghcr.io/acme/*", "new-ci", "iss");
        let policies = [old, new];
        let resolved = resolve(&policies, "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 2, "equal-specificity locked entries combine as ANY-of");
        assert!(resolved.iter().any(|p| pinned_identity(p) == Some("new-ci")));
    }

    #[test]
    fn a_locked_policy_that_does_not_match_leaves_specificity_alone() {
        // The lock is scoped to the target it matches. A system pin for an
        // UNRELATED scope must not drag the winning level down for a target it
        // never covers — that would silently widen every unrelated verify to the
        // broadest entry in the pool.
        let unrelated_system = locked_policy("ghcr.io/other/*", "system-X", "iss");
        let broad = policy("ghcr.io/acme/*", Some("broad"), None, "iss");
        let narrow = policy("ghcr.io/acme/tool", Some("narrow"), None, "iss");
        let policies = [unrelated_system, broad, narrow];
        let resolved = resolve(&policies, "ghcr.io/acme/tool");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            pinned_identity(resolved[0]),
            Some("narrow"),
            "with no locked policy in the matching set, global most-specific-wins is unchanged"
        );
    }

    #[test]
    fn system_locked_cannot_be_set_from_toml() {
        // The escalation guard. Unknown keys are tolerated fleet-wide (see
        // `trust_config_tolerates_unknown_fields_from_newer_ocx`), so a managed
        // payload writing `system_locked = true` must PARSE — and be ignored.
        // If the field ever became readable from disk, the untrusted tier could
        // promote itself to the authority it is supposed to be bounded by.
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"
system_locked = true

[trust.policy.keyless]
identity = "attacker@example.test"
oidc_issuer = "https://example.test"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("the key is tolerated, not rejected");
        assert_eq!(root.trust.policy.len(), 1);
        assert!(
            !root.trust.policy[0].system_locked,
            "system_locked is set by the loader, never read from a config file"
        );
    }

    #[test]
    fn lock_as_system_marks_every_declared_policy() {
        let mut config = TrustConfig {
            policy: vec![
                policy("ghcr.io/acme/*", Some("a"), None, "iss"),
                policy("ghcr.io/other/*", Some("b"), None, "iss"),
            ],
            sigstore: None,
        };
        config.lock_as_system();
        assert!(config.policy.iter().all(|policy| policy.system_locked));
    }

    #[test]
    fn nested_keyless_matcher_compiles_to_a_keyless_backend() {
        // The shape a user writes: matchers under `[trust.policy.keyless]`,
        // scope beside it. Everything below depends on this parsing at all.
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity = "release@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("the nested form parses");
        let compiled = root.trust.policy[0]
            .compile()
            .expect("a complete keyless matcher compiles");
        let PolicyBackend::Keyless(keyless) = &compiled.backend;
        assert!(matches!(&keyless.identity, IdentityRule::Exact(id) if id == "release@acme.example"));
        assert_eq!(keyless.issuer, "https://token.actions.githubusercontent.com");
    }

    #[test]
    fn the_flat_spelling_declares_no_backend_and_says_so() {
        // Typo-loudness for the pre-nesting spelling. Unknown keys stay
        // tolerated fleet-wide, so this parses — and that is precisely why the
        // refusal has to come from compilation instead. Silently treating a
        // scope-only entry as "no policy" would leave a user who believes they
        // pinned an identity with no pin and no signal.
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"
identity = "release@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("the flat keys are tolerated as unknown, not rejected");
        let policy = &root.trust.policy[0];
        assert!(policy.keyless.is_none(), "flat keys must not populate the sub-table");

        let error = policy
            .compile()
            .expect_err("an entry with no backend cannot govern a scope");
        assert!(matches!(error, TrustPolicyError::NoBackend { .. }));
        let rendered = error.to_string();
        assert!(
            rendered.contains("[trust.policy.keyless]"),
            "the refusal must name the sub-table the matchers belong in; got: {rendered}"
        );
        assert!(
            rendered.contains("ghcr.io/acme/*"),
            "the refusal must name which entry is at fault; got: {rendered}"
        );
    }

    #[test]
    fn compile_rejects_an_entry_declaring_no_backend() {
        // Zero backends is the only arity a one-backend schema can get wrong.
        // "More than one" is unwritable until a second backend field exists,
        // and a check that cannot fail is not a check — see `compile`.
        let bare = TrustPolicy {
            scope: Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())),
            builder: None,
            keyless: None,
            system_locked: false,
        };
        assert!(matches!(bare.compile(), Err(TrustPolicyError::NoBackend { .. })));
    }

    #[test]
    fn compile_rejects_a_keyless_matcher_without_an_issuer() {
        // `oidc_issuer` is Option at the serde layer for fleet forward-compat
        // and mandatory here: an identity with no issuer would accept the same
        // SAN minted by any OIDC provider.
        let no_issuer = TrustPolicy {
            scope: Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())),
            builder: None,
            keyless: Some(KeylessMatcher {
                identity: Some("release@acme.example".to_string()),
                identity_regexp: None,
                oidc_issuer: None,
            }),
            system_locked: false,
        };
        assert!(matches!(no_issuer.compile(), Err(TrustPolicyError::IssuerUnset { .. })));
    }

    #[test]
    fn builder_is_a_backend_independent_sibling_of_scope() {
        // #103: the SLSA builder pin sits beside `scope`, not inside a backend
        // sub-table, and rides through compilation for attestation verify.
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"
builder = "https://github.com/acme/tool/.github/workflows/release.yml@refs/heads/main"

[trust.policy.keyless]
identity = "release@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("a top-level builder parses");
        let compiled = root.trust.policy[0]
            .compile()
            .expect("a builder pin does not block compilation");
        assert_eq!(
            compiled.builder.as_deref(),
            Some("https://github.com/acme/tool/.github/workflows/release.yml@refs/heads/main")
        );
    }

    #[test]
    fn a_builder_pin_is_not_a_backend() {
        // A builder pin says who built the artifact, never who may sign it, so
        // it cannot stand in for the missing matcher.
        let builder_only = TrustPolicy {
            scope: Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())),
            builder: Some("https://github.com/acme/tool/.github/workflows/release.yml@refs/heads/main".to_string()),
            keyless: None,
            system_locked: false,
        };
        assert!(matches!(
            builder_only.compile(),
            Err(TrustPolicyError::NoBackend { .. })
        ));
    }

    #[test]
    fn an_absent_scope_is_the_same_catch_all_as_an_empty_one() {
        // `scope` is optional so an entry from a newer ocx still parses. Absent
        // must mean what `""` already means, or the two spellings of one
        // intent would govern different package sets.
        let catch_all = TrustPolicy {
            scope: None,
            builder: None,
            keyless: Some(KeylessMatcher {
                identity: Some("release@acme.example".to_string()),
                identity_regexp: None,
                oidc_issuer: Some("https://token.actions.githubusercontent.com".to_string()),
            }),
            system_locked: false,
        };
        assert!(catch_all.matches_scope("anything/at/all"));
        assert_eq!(catch_all.specificity_for("anything/at/all"), 0);
        assert!(catch_all.compile().is_ok(), "an unscoped policy still names a signer");
    }

    #[test]
    fn the_flag_override_policy_carries_no_builder_pin() {
        // `--certificate-identity` + `--certificate-oidc-issuer` name a signer,
        // not a build; inventing a builder pin from them would refuse every
        // provenance the flags never spoke about.
        let flags = CompiledPolicy::exact("you@example.com".to_string(), "https://issuer.example".to_string());
        assert!(flags.builder.is_none());
        let PolicyBackend::Keyless(keyless) = &flags.backend;
        assert_eq!(keyless.issuer, "https://issuer.example");
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
        assert!(matches!(compiled_identity(&compiled[0]), IdentityRule::Exact(id) if id == "operator-X"));
    }

    #[test]
    fn project_tier_adds_trust_for_ungoverned_scopes() {
        // No operator policy covers this package, so the project ocx.toml may
        // add trust for it.
        let operator = [policy("ghcr.io/acme/*", Some("operator-X"), None, "iss")];
        let project = [policy("ghcr.io/other/tool", Some("project-Z"), None, "iss")];
        let compiled = resolve_tiered(&operator, &project, "ghcr.io/other/tool").expect("project policy compiles");
        assert_eq!(compiled.len(), 1);
        assert!(matches!(compiled_identity(&compiled[0]), IdentityRule::Exact(id) if id == "project-Z"));
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

[trust.policy.keyless]
identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3"
oidc_issuer = "https://token.actions.githubusercontent.com"

[[trust.policy]]
scope = "ghcr.io/other/*"

[trust.policy.keyless]
identity_regexp = "^https://example\\.com/.*$"
oidc_issuer = "https://example.com"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("parse");
        assert_eq!(root.trust.policy.len(), 2);
        assert_eq!(
            root.trust.policy[0].scope,
            Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string()))
        );
        assert_eq!(
            root.trust.policy[0]
                .keyless
                .as_ref()
                .and_then(|keyless| keyless.identity.as_deref()),
            Some("https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3")
        );
        assert!(
            root.trust.policy[1]
                .keyless
                .as_ref()
                .is_some_and(|keyless| keyless.identity_regexp.is_some())
        );
    }

    #[test]
    fn trust_config_tolerates_unknown_fields_from_newer_ocx() {
        // Fleet forward-compat: the `[managed]` tier deserializes a
        // fleet-distributed config.toml as `Config`, which reaches
        // `TrustConfig`/`TrustPolicy`. A payload written by a newer ocx must
        // degrade to its known fields on an older binary, not fail the whole
        // file (see arch-principles.md "Fleet forward-compat on fleet-read
        // config"). With `#[serde(deny_unknown_fields)]` restored on either
        // struct, this parse fails — that is the regression this test guards.
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"
future_field = "added by a newer ocx"

[trust.policy.keyless]
identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3"
oidc_issuer = "https://token.actions.githubusercontent.com"
nested_future_field = "added by a newer ocx, inside the backend sub-table"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("unknown field is tolerated, not rejected");
        assert_eq!(root.trust.policy.len(), 1);
        let policy = &root.trust.policy[0];
        assert_eq!(policy.scope, Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())));
        let keyless = policy.keyless.as_ref().expect("the backend sub-table survives");
        assert_eq!(
            keyless.identity.as_deref(),
            Some("https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3")
        );
        assert_eq!(
            keyless.oidc_issuer.as_deref(),
            Some("https://token.actions.githubusercontent.com")
        );
    }

    #[test]
    fn trust_config_tolerates_unknown_top_level_field_from_newer_ocx() {
        // Same fleet forward-compat concern as
        // `trust_config_tolerates_unknown_fields_from_newer_ocx`, but the unknown
        // key sits directly under `[trust]` — a sibling of `policy` — so this
        // exercises `TrustConfig` itself, not `TrustPolicy`. Restoring
        // `#[serde(deny_unknown_fields)]` on `TrustConfig` alone (leaving
        // `TrustPolicy` untouched) fails this parse; that is the regression this
        // test guards, and it is the gap the sibling test above cannot catch
        // because its unknown field lives inside `[[trust.policy]]`.
        let toml = r#"
[trust]
future_field = "added by a newer ocx"

[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3"
oidc_issuer = "https://token.actions.githubusercontent.com"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("unknown top-level field is tolerated, not rejected");
        assert_eq!(root.trust.policy.len(), 1);
        assert_eq!(
            root.trust.policy[0].scope,
            Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string()))
        );
    }

    #[test]
    fn sigstore_trust_tolerates_unknown_fields_from_newer_ocx() {
        // Third fleet forward-compat guard, one nesting level below the two
        // above: the unknown key sits inside `[trust.sigstore]`. Neither
        // sibling test can catch a `deny_unknown_fields` restored on
        // `SigstoreTrust` — one exercises `TrustPolicy`, the other
        // `TrustConfig` itself.
        let toml = r#"
[trust.sigstore]
trusted_root = "sigstore/trusted-root.json"
future_field = "added by a newer ocx"
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("unknown field inside [trust.sigstore] is tolerated");
        let sigstore = root.trust.sigstore.expect("sub-table parses");
        assert_eq!(
            sigstore.trusted_root.as_deref(),
            Some(Path::new("sigstore/trusted-root.json"))
        );
    }

    #[test]
    fn system_scope_sigstore_survives_a_lower_tier_override() {
        // `[[trust.policy]]` pools across tiers; a scalar trust root cannot —
        // two Fulcio CAs is an ambiguity, not a merge. Follows the
        // `[registry]` precedent: system scope replaces AND locks.
        let mut system = TrustConfig {
            sigstore: Some(SigstoreTrust {
                fulcio_url: Some("https://fulcio.corp.example".to_string()),
                ..SigstoreTrust::default()
            }),
            ..TrustConfig::default()
        };
        system.lock_as_system();

        let home = TrustConfig {
            sigstore: Some(SigstoreTrust {
                fulcio_url: Some("https://fulcio.attacker.example".to_string()),
                ..SigstoreTrust::default()
            }),
            ..TrustConfig::default()
        };

        let mut merged = system;
        merged.merge(home);
        assert_eq!(
            merged.sigstore.expect("sigstore survives").fulcio_url.as_deref(),
            Some("https://fulcio.corp.example"),
            "a system-locked [trust.sigstore] is not overridable by a lower tier"
        );
    }

    #[test]
    fn a_higher_tier_switching_trust_root_spelling_clears_the_other() {
        // The trust root is ONE decision in two spellings. A tier that moves
        // from a path to an inline document must not leave both fields set —
        // that is exactly the ambiguity the resolver refuses with exit 78, and
        // it would be reached through a merge nobody wrote by hand.
        let user = TrustConfig {
            sigstore: Some(SigstoreTrust {
                trusted_root: Some(PathBuf::from("/etc/ocx/sigstore/trusted-root.json")),
                rekor_url: Some("https://rekor.corp.example".to_string()),
                ..SigstoreTrust::default()
            }),
            ..TrustConfig::default()
        };
        let home = TrustConfig {
            sigstore: Some(SigstoreTrust {
                trusted_root_json: Some("{}".to_string()),
                ..SigstoreTrust::default()
            }),
            ..TrustConfig::default()
        };

        let mut merged = user;
        merged.merge(home);
        let sigstore = merged.sigstore.expect("sigstore survives");
        assert_eq!(
            sigstore.trusted_root, None,
            "the path spelling is dropped, not left alongside"
        );
        assert_eq!(sigstore.trusted_root_json.as_deref(), Some("{}"));
        assert_eq!(
            sigstore.rekor_url.as_deref(),
            Some("https://rekor.corp.example"),
            "an unrelated field the higher tier said nothing about survives"
        );
    }

    #[test]
    fn absent_sigstore_in_a_higher_tier_leaves_the_lower_one_standing() {
        let user = TrustConfig {
            sigstore: Some(SigstoreTrust {
                fulcio_url: Some("https://fulcio.corp.example".to_string()),
                ..SigstoreTrust::default()
            }),
            ..TrustConfig::default()
        };
        let mut merged = user;
        merged.merge(TrustConfig::default());
        assert_eq!(
            merged.sigstore.expect("sigstore survives").fulcio_url.as_deref(),
            Some("https://fulcio.corp.example"),
            "a tier that says nothing about sigstore must not clear it"
        );
    }

    #[test]
    fn anchor_relative_root_resolves_against_the_declaring_config_dir() {
        // The whole point of the relative form: `/etc/ocx/config.toml` naming
        // `sigstore/trusted-root.json` means `/etc/ocx/sigstore/...`, never
        // a path relative to whatever directory the process happens to be in.
        let mut sigstore = SigstoreTrust {
            trusted_root: Some(PathBuf::from("sigstore/trusted-root.json")),
            ..SigstoreTrust::default()
        };
        sigstore.anchor_relative_root(Path::new("/etc/ocx"));
        assert_eq!(
            sigstore.trusted_root.as_deref(),
            Some(Path::new("/etc/ocx/sigstore/trusted-root.json"))
        );
    }

    #[test]
    fn anchor_relative_root_leaves_an_absolute_path_alone() {
        let absolute = if cfg!(windows) {
            PathBuf::from("C:/opt/sigstore/trusted-root.json")
        } else {
            PathBuf::from("/opt/sigstore/trusted-root.json")
        };
        let mut sigstore = SigstoreTrust {
            trusted_root: Some(absolute.clone()),
            ..SigstoreTrust::default()
        };
        sigstore.anchor_relative_root(Path::new("/etc/ocx"));
        assert_eq!(sigstore.trusted_root.as_deref(), Some(absolute.as_path()));
    }
}
