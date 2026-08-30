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
//! A policy accepts a **set** of signers: `signers = [...]`, each entry tagged
//! `kind = "keyless"` or `kind = "key"`, while `scope` and the SLSA `builder`
//! pin stay top-level because neither depends on which backend signed. Mixing
//! kinds in one policy is legal and is how a fleet migrates between signing
//! models without touching scope. See [`TrustPolicy`].
//!
//! **Adding a signer widens acceptance; it never narrows it.** The array is an
//! ANY-of, so every entry is one more way for an artifact to pass. Most readers
//! hear "add a key policy" as tightening, which is the opposite of what happens.
//! Narrowing means *removing* entries — or, at the operator tier, a
//! `system_locked` policy that displaces the lower tiers wholesale.
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
//! This module is a leaf with respect to `oci`'s verification machinery: the
//! certificate-side matching that consumes a resolved [`CompiledPolicy`] lives
//! in `oci::verify::identity`, and nothing here reaches back into it. The one
//! permitted reference is `oci::sign::key_ref` — the `--key` grammar
//! [`compile_key_signer`] speaks, itself a leaf that imports nothing from this
//! module. Sharing it is what keeps a key reference spelled the same way on the
//! command line and in a policy; a second parser here would be the drift.
//! See `.claude/artifacts/adr_trust_policy.md`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::utility::fs::path::FileReference;

use crate::log;
use crate::oci::sign::key_ref::MAX_KEY_PEM_BYTES;

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

    /// Fleet-wide default for uploading a **key-mode** signature to the
    /// transparency log, when neither `--rekor-upload` nor `--no-rekor-upload`
    /// is given. Absent means off.
    ///
    /// **Key mode only.** Under keyless the upload is a *requirement*, not a
    /// default — a Fulcio certificate is valid for about ten minutes, and the
    /// Rekor timestamp is the only proof the signature happened inside that
    /// window — so this field is ignored there, deliberately without a warning.
    /// Erroring or warning on every keyless signature because a fleet-wide
    /// key-mode setting says `false` would let an unrelated configuration key
    /// break the default signing path.
    ///
    /// Off by default in key mode even though cosign uploads: `rekor_url`
    /// defaults to the **public** Rekor, so an on-by-default key path would
    /// publish the digest and signer of a private corporate artifact to a
    /// world-readable append-only log on first run. That is irreversible; a
    /// signature with no transparency record is fixed by re-signing.
    #[serde(default)]
    pub rekor_upload: Option<bool>,

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
        if other.rekor_upload.is_some() {
            self.rekor_upload = other.rekor_upload;
        }
    }

    /// Resolve [`Self::trusted_root`] against `config_dir` — the directory of
    /// the `config.toml` that declared it — through the shared
    /// [`FileReference`] grammar.
    ///
    /// Called by the config loader once per file tier, so `/etc/ocx/config.toml`
    /// and `$OCX_HOME/config.toml` each anchor their own relative paths and the
    /// process working directory never enters into it. The relative rule and
    /// the reason it is `!has_root()` rather than `is_relative()` both live in
    /// [`FileReference::anchored_at`]; this is the seam that applies them.
    ///
    /// **Takes `file://` as well as a bare path.** It sat three lines from
    /// `signers[].key` accepting a strictly smaller vocabulary for no stated
    /// reason ([ocx-sh/ocx#379](https://github.com/ocx-sh/ocx/issues/379)); one
    /// grammar now serves both. The stored value is always the resolved path,
    /// so every reader below this seam sees a plain absolute path and none of
    /// them learns about the spelling.
    pub fn anchor_relative_root(&mut self, config_dir: &std::path::Path) {
        if let Some(path) = self.trusted_root.as_ref() {
            // `to_string_lossy` is exact here: the value is deserialized from a
            // TOML string, so it is UTF-8 by construction.
            let written = path.to_string_lossy().into_owned();
            self.trusted_root = Some(FileReference::parse(&written).anchored_at(config_dir));
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
/// One `signers` array names everyone this policy accepts, each entry tagged
/// `kind = "keyless"` or `kind = "key"` ([`SignerSpec`]). Acceptance is ANY-of,
/// so **adding a signer always widens what verifies, never narrows it**. There is no
/// `[trust.policy.key]` sub-table and no `[trust.policy.keyless]` one — one
/// canonical spelling. [`Self::scope`] and [`Self::builder`] stay top-level
/// because neither depends on which backend produced the signature.
///
/// ```toml
/// [[trust.policy]]
/// scope = "ghcr.io/acme/*"
/// # or, to carve a subtree back out:
/// # scope = { include = ["ghcr.io/acme/*"], exclude = ["ghcr.io/acme/experimental/*"] }
///
/// signers = [
///   { kind = "keyless",
///     identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3",
///     oidc_issuer = "https://token.actions.githubusercontent.com" },
/// ]
/// ```
///
/// Every field is optional at the serde layer, for the same fleet
/// forward-compat reason unknown keys are tolerated (see [`TrustConfig`]): an
/// entry written by a newer ocx degrades to its known parts instead of failing
/// the whole file. An unknown `kind` degrades the same way, through
/// [`SignerSpec::Unknown`] — an internally-tagged enum would otherwise make it
/// a parse error for the entire document. What a *resolved* policy is allowed
/// to mean is narrowed at [`TrustPolicy::compile`] instead, which refuses a
/// policy that leaves no backend behind: an absent or empty array with
/// [`TrustPolicyError::NoSigners`], and one whose every entry named an unknown
/// kind with [`TrustPolicyError::NoUsableSigner`]. That is also what makes a
/// misspelled field loud — it parses as an unknown key, leaves the entry
/// declaring nothing, and fails closed rather than widening.
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

    /// The signers this policy accepts — an **ANY-of** set, each entry tagged
    /// `kind = "keyless"` or `kind = "key"` ([`SignerSpec`]).
    ///
    /// ```toml
    /// signers = [
    ///   { kind = "keyless", identity = "release@acme.example", oidc_issuer = "https://accounts.google.com" },
    ///   { kind = "key",     key = "etc/acme-release.pub" },
    /// ]
    /// ```
    ///
    /// **Every entry added here widens what verifies.** The set is an ANY-of, so
    /// a new signer is a new way to pass, never a new condition to satisfy.
    ///
    /// Absent reads as empty, and an empty set is a **configuration error**, not
    /// a catch-all — a policy naming no acceptable signer accepts nothing, and
    /// the permissive reading would turn a deleted line into a silent bypass.
    /// That is also where a mis-spelled entry lands: unknown keys are tolerated
    /// for the fleet forward-compat reason [`TrustPolicy`] states, so a typo
    /// leaves no signer behind and fails closed at [`Self::compile`].
    #[serde(default)]
    pub signers: Vec<SignerSpec>,

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

/// A `kind = "keyless"` signer: which Sigstore identity may sign.
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

/// One entry of a policy's `signers = [...]` array.
///
/// The **config layer**. It compiles into a [`PolicyBackend`] exactly as
/// [`KeylessMatcher`] compiles into [`CompiledKeyless`]: `PolicyBackend` holds a
/// compiled [`regex::Regex`] and deliberately carries no serde derives, so it is
/// never deserialized into directly.
///
/// Internally tagged on `kind` with newtype variants, so a signer's fields sit
/// flat beside the tag and the keyless arm reuses [`KeylessMatcher`] verbatim:
///
/// ```toml
/// [[trust.policy]]
/// scope = "ghcr.io/acme/*"
/// signers = [
///   { kind = "keyless", identity = "release@acme.example", oidc_issuer = "https://accounts.google.com" },
///   { kind = "key", key = "etc/acme-release.pub" },
/// ]
/// ```
///
/// An empty array is a configuration error, never a catch-all — see
/// [`validate_signers`]. `scope` and `builder` stay policy-level siblings for
/// the reason [`TrustPolicy::builder`] gives: neither depends on which backend
/// signed.
///
/// Reachable from `Config` through [`TrustPolicy::signers`], so the `JsonSchema`
/// derive is what puts these entries in the published `config.toml` schema —
/// which is where typo detection for a signer belongs, since the deserializer
/// deliberately tolerates unknown keys (see [`TrustConfig`]).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignerSpec {
    /// `{ kind = "keyless", identity | identity_regexp, oidc_issuer }`.
    Keyless(KeylessMatcher),
    /// `{ kind = "key", key | key_pem }`.
    Key(KeyMatcher),

    /// A `kind` this build does not know — an entry written by a newer ocx.
    ///
    /// Without this arm an internally-tagged enum makes an unrecognised `kind`
    /// a hard **parse** error for the whole file, and a managed payload that
    /// does not parse is dropped entirely (`ConfigLoader` logs it and carries
    /// on) — so one forward-looking signer would silently remove every trust
    /// policy the fleet has. That is the one direction fleet forward-compat
    /// must never take, for the same reason `[shell.consent]` carries the
    /// opposite carve-out: dropping an unknown *narrowing* declaration widens
    /// trust.
    ///
    /// It compiles to no backend, so it narrows — it accepts nobody, and a
    /// sibling `keyless` entry in the same policy keeps working. A policy whose
    /// signers are *all* unknown compiles to nothing at all and is refused by
    /// name ([`TrustPolicyError::NoUsableSigner`]) rather than left to fail as
    /// a confusing identity mismatch.
    #[serde(other)]
    Unknown,
}

/// The `kind = "key"` signer: one public key, by reference or inline.
///
/// `key` XOR `key_pem`, mirroring [`KeylessMatcher`]'s identity XOR — both
/// `Option` at the serde layer for the fleet forward-compat reason
/// [`TrustPolicy`] states, both narrowed at [`validate_signers`]. There is no
/// `key_regexp`: a public key is a fixed value, not a pattern.
///
/// `ocx config push` inlines the reference form into `key_pem` at publish time,
/// for the reason [`SigstoreTrust::trusted_root_json`] already gives — a path on
/// the operator's disk means nothing on a consumer's.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct KeyMatcher {
    /// A key reference in the [`KeyRef`](crate::oci::sign::KeyRef) grammar
    /// (`[scheme://]<rest>`) — the same spelling `--key` takes on the command
    /// line, so a KMS entry later needs no config-format change. Mutually
    /// exclusive with [`Self::key_pem`].
    ///
    /// # Legal in a local tier, rejected in a managed payload
    ///
    /// A managed-config payload is a `config.toml` shipped as a package to a
    /// fleet, so a `file:` reference in one names the *operator's* disk and
    /// means nothing on any consumer. A published payload therefore accepts
    /// [`Self::key_pem`] only, and the path form is refused with an error
    /// naming `key_pem` as the fix — the same convention `trusted_root` /
    /// `trusted_root_json` already follows, where
    /// `managed_config::publish::inline_trusted_root` reads the path form at
    /// publish time and inlines it. Rejecting it removes an incoherent state
    /// rather than adding a guard.
    ///
    /// **Local tiers (project / operator / user config on the author's own
    /// disk) leave this unrestricted.** A relative reference resolves against
    /// the directory of the config file that declared it — ordinary resolution
    /// semantics so a relative path means what its author meant. That is *not*
    /// a containment check and must not be described as one; the author owns
    /// that filesystem.
    ///
    /// The refusal lives in
    /// [`validate_managed_config_payload`](crate::managed_config::publish::validate_managed_config_payload),
    /// beside its `trusted_root` twin. A managed payload that reaches a
    /// consumer with this field set anyway — published by an older ocx — has it
    /// dropped at load time rather than read.
    #[serde(default)]
    pub key: Option<String>,

    /// Verbatim SPKI PEM. Mutually exclusive with [`Self::key`].
    #[serde(default)]
    pub key_pem: Option<String>,
}

/// Validate a `signers` array's shape, independent of compiling it into
/// backends.
///
/// Every entry must name a signer completely: the array is non-empty, every
/// `key` entry declares exactly one of `key` / `key_pem`, and every `keyless`
/// entry satisfies the identity XOR identity_regexp rule with an issuer set.
/// The keyless half is not restated here — it runs through
/// [`compile_keyless_matcher`], the same function `TrustPolicy::compile_keyless`
/// calls, and the compiled matcher is discarded. Reading the rule instead of
/// calling it would be a second copy, and a `{ kind = "keyless" }` signer that
/// names nobody must fail closed for the same reason `signers = []` does.
///
/// `scope` is the offending policy's rendered scope, as `TrustPolicy::scope_label`
/// produces it — a config declares many policies, so a diagnostic that does not
/// name one is unactionable.
///
/// # Errors
/// [`TrustPolicyError::NoSigners`] for an empty array — **fail closed, never a
/// catch-all**: a policy that names no acceptable signer accepts nothing, and
/// reading it as "anyone may sign" turns a deleted line into a silent bypass.
/// [`TrustPolicyError::KeyConflict`] when both key forms are set;
/// [`TrustPolicyError::KeyUnset`] when neither is. From a keyless entry,
/// whatever [`compile_keyless_matcher`] raises:
/// [`TrustPolicyError::IdentityConflict`], [`TrustPolicyError::IdentityUnset`],
/// [`TrustPolicyError::IssuerUnset`] or [`TrustPolicyError::InvalidRegex`].
pub fn validate_signers(signers: &[SignerSpec], scope: &str) -> Result<(), TrustPolicyError> {
    if signers.is_empty() {
        return Err(TrustPolicyError::NoSigners {
            scope: scope.to_owned(),
        });
    }
    for signer in signers {
        let key = match signer {
            SignerSpec::Keyless(keyless) => {
                compile_keyless_matcher(keyless, scope)?;
                continue;
            }
            SignerSpec::Key(key) => key,
            // Nothing to validate: this build knows none of its fields. The
            // narrowing happens at compile time, where it yields no backend.
            SignerSpec::Unknown => continue,
        };
        match (&key.key, &key.key_pem) {
            (Some(_), Some(_)) => {
                return Err(TrustPolicyError::KeyConflict {
                    scope: scope.to_owned(),
                });
            }
            (None, None) => {
                return Err(TrustPolicyError::KeyUnset {
                    scope: scope.to_owned(),
                });
            }
            (Some(_), None) | (None, Some(_)) => {}
        }
    }
    Ok(())
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

    /// Resolve this entry into the ready-to-match [`CompiledPolicy`]: validate
    /// the signer set, then compile every entry into a [`PolicyBackend`].
    ///
    /// Shape validation runs first, over the whole array, through the same
    /// [`validate_signers`] a managed payload is checked with — so a policy that
    /// would refuse at publish time refuses here too, for the same reason and
    /// with the same message.
    ///
    /// # Errors
    /// [`TrustPolicyError::NoSigners`] when the array is empty or absent;
    /// from a `kind = "keyless"` entry, [`TrustPolicyError::IdentityConflict`]
    /// when both identity forms are set, [`TrustPolicyError::IdentityUnset`]
    /// when neither is, [`TrustPolicyError::IssuerUnset`] when `oidc_issuer` is
    /// absent, and [`TrustPolicyError::InvalidRegex`] when `identity_regexp`
    /// does not compile; from a `kind = "key"` entry,
    /// [`TrustPolicyError::KeyConflict`], [`TrustPolicyError::KeyUnset`],
    /// [`TrustPolicyError::KeyReferenceInvalid`],
    /// [`TrustPolicyError::KeyUnreadable`] and
    /// [`TrustPolicyError::KeyMalformed`].
    pub fn compile(&self) -> Result<CompiledPolicy, TrustPolicyError> {
        let scope = self.scope_label();
        validate_signers(&self.signers, &scope)?;
        let backends = self
            .signers
            .iter()
            .filter_map(|signer| match signer {
                SignerSpec::Keyless(keyless) => {
                    Some(compile_keyless_matcher(keyless, &scope).map(PolicyBackend::Keyless))
                }
                SignerSpec::Key(key) => Some(compile_key_matcher(key, &scope).map(PolicyBackend::Key)),
                // Dropped, not refused: a signer this build cannot evaluate
                // accepts nobody, which is the safe direction, and its siblings
                // in the same policy still apply.
                SignerSpec::Unknown => None,
            })
            .collect::<Result<Vec<_>, TrustPolicyError>>()?;
        if backends.is_empty() {
            return Err(TrustPolicyError::NoUsableSigner { scope });
        }
        Ok(CompiledPolicy {
            builder: self.builder.clone(),
            backends,
        })
    }

    /// Rewrite each relative path-form signer key to be absolute against
    /// `config_dir` — the directory of the `config.toml` that declared it.
    ///
    /// The twin of [`SigstoreTrust::anchor_relative_root`], called from the same
    /// loader seam and once per file tier, so `/etc/ocx/config.toml` and
    /// `$OCX_HOME/config.toml` each anchor their own values and the process
    /// working directory never enters into it.
    ///
    /// **Ordinary path resolution, not a containment check.** A relative
    /// reference means what its author meant, and the author owns that
    /// filesystem; nothing here restricts where a key may live, and describing
    /// it as a guard would invite someone to rely on it as one.
    ///
    /// Non-path references and unparseable ones are left untouched —
    /// [`Self::compile`] is where a reference is judged, and rewriting one this
    /// function cannot understand would corrupt the diagnostic it produces.
    pub fn anchor_relative_keys(&mut self, config_dir: &std::path::Path) {
        for signer in &mut self.signers {
            let SignerSpec::Key(matcher) = signer else {
                continue;
            };
            let Some(reference) = matcher.key.as_deref() else {
                continue;
            };
            let Ok(parsed) = crate::oci::sign::KeyRef::parse(reference) else {
                continue;
            };
            let Some(file) = parsed.as_file() else {
                continue;
            };
            // One rule, in one place — [`FileReference::anchored_at`], the same
            // seam [`SigstoreTrust::anchor_relative_root`] goes through. Here
            // the drift it prevents costs more than a moved path: a rooted
            // `/etc/ocx/acme.pub` that came back joined to the config directory
            // would name a file its author never wrote.
            matcher.key = Some(file.anchored_at(config_dir).display().to_string());
        }
    }
}

/// Enforce the keyless invariants — identity XOR identity_regexp, and an
/// issuer present — and compile the result.
///
/// The single implementation of that rule. It is a free function taking `scope`
/// rather than a `TrustPolicy` method reading `self.scope_label()` because
/// [`validate_signers`] enforces the same rule over a bare [`SignerSpec`], which
/// has no policy to ask for a label. A second copy of the rule there is exactly
/// how [`TrustPolicy::compile`] and [`validate_signers`] would drift into
/// accepting different things.
///
/// # Errors
/// [`TrustPolicyError::IdentityConflict`] when both identity forms are set,
/// [`TrustPolicyError::IdentityUnset`] when neither is,
/// [`TrustPolicyError::IssuerUnset`] when `oidc_issuer` is absent, and
/// [`TrustPolicyError::InvalidRegex`] when `identity_regexp` does not compile.
fn compile_keyless_matcher(keyless: &KeylessMatcher, scope: &str) -> Result<CompiledKeyless, TrustPolicyError> {
    let identity = match (&keyless.identity, &keyless.identity_regexp) {
        (Some(_), Some(_)) => {
            return Err(TrustPolicyError::IdentityConflict {
                scope: scope.to_owned(),
            });
        }
        (None, None) => {
            return Err(TrustPolicyError::IdentityUnset {
                scope: scope.to_owned(),
            });
        }
        (Some(exact), None) => IdentityRule::Exact(exact.clone()),
        (None, Some(pattern)) => {
            IdentityRule::compile_regex(pattern).map_err(|source| TrustPolicyError::InvalidRegex {
                scope: scope.to_owned(),
                source,
            })?
        }
    };
    let issuer = keyless
        .oidc_issuer
        .clone()
        .ok_or_else(|| TrustPolicyError::IssuerUnset {
            scope: scope.to_owned(),
        })?;
    Ok(CompiledKeyless { identity, issuer })
}

/// A compiled, ready-to-match policy: every acceptable signer, plus the pins
/// that hold whichever backend signed.
#[derive(Debug, Clone)]
pub struct CompiledPolicy {
    /// The SLSA provenance builder identity this policy pins, if any. Enforced
    /// by attestation verify; inert in signature mode.
    pub builder: Option<String>,
    /// The verification backends this policy resolved to — an **ANY-of** set,
    /// in declaration order. Never empty: [`validate_signers`] refuses an empty
    /// `signers` array before a single backend is built.
    pub backends: Vec<PolicyBackend>,
}

/// One verification backend a compiled policy resolved to.
///
/// An enum rather than a set of optional fields so that a new backend is a
/// compile error at every site that consumes a policy, instead of a silent
/// `None` nobody handles. Deliberately not `#[non_exhaustive]`: the binary is
/// the only consumer, and in-crate matches staying total is the whole point.
#[derive(Debug, Clone)]
pub enum PolicyBackend {
    /// Keyless Sigstore: a Fulcio certificate SAN plus its OIDC issuer.
    Keyless(CompiledKeyless),
    /// A pinned public key: the artifact was signed with the matching private
    /// key, and no certificate is involved.
    ///
    /// Holds the parsed key directly rather than a wrapper struct — there is
    /// nothing else to compile for a key. The algorithm was auto-detected from
    /// the PEM at parse time, so a signature is checked by verifying it, never
    /// by comparing a fingerprint.
    Key(sigstore::crypto::CosignVerificationKey),
}

/// A compiled `kind = "keyless"` signer.
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
            backends: vec![PolicyBackend::Keyless(CompiledKeyless {
                identity: IdentityRule::Exact(identity),
                issuer,
            })],
        }
    }
}

/// Compile a `--key` reference into a single-signer policy — the flag-override
/// path for key mode, the twin of [`CompiledPolicy::exact`].
///
/// It carries no builder pin, for the same reason `exact` carries none: the flag
/// names a signer, not a build.
///
/// # Errors
/// [`TrustPolicyError::KeyReferenceInvalid`] when the reference names a backend
/// with no implementation, [`TrustPolicyError::KeyUnreadable`] when the file
/// cannot be read, and [`TrustPolicyError::KeyMalformed`] when its bytes are not
/// an SPKI public key.
pub fn compile_key_signer(key: &crate::oci::sign::key_ref::KeyRef) -> Result<CompiledPolicy, TrustPolicyError> {
    // `--key` names one signer with no scope, so the diagnostic names the flag
    // rather than a policy that does not exist.
    Ok(CompiledPolicy {
        builder: None,
        backends: vec![PolicyBackend::Key(compile_key_reference(key, "--key")?)],
    })
}

/// Compile a `kind = "key"` signer's declared material into a verification key.
///
/// `key_pem` is taken verbatim; `key` is resolved through the shared
/// [`KeyRef`](crate::oci::sign::KeyRef) grammar and read from disk. The XOR
/// between them is [`validate_signers`]'s job and has already run by the time
/// this is reached, so the unset case here is defensive rather than expected.
fn compile_key_matcher(
    matcher: &KeyMatcher,
    scope: &str,
) -> Result<sigstore::crypto::CosignVerificationKey, TrustPolicyError> {
    if let Some(pem) = matcher.key_pem.as_deref() {
        return parse_verification_key(pem.as_bytes(), scope, KeyFault::ConfigText);
    }
    let reference = matcher.key.as_deref().ok_or_else(|| TrustPolicyError::KeyUnset {
        scope: scope.to_owned(),
    })?;
    let parsed =
        crate::oci::sign::KeyRef::parse(reference).map_err(|source| TrustPolicyError::KeyReferenceInvalid {
            scope: scope.to_owned(),
            source,
        })?;
    compile_key_reference(&parsed, scope)
}

/// Read and parse the public key a resolved [`KeyRef`](crate::oci::sign::KeyRef)
/// names.
///
/// **Synchronous, and deliberately so.** [`compile_key_signer`]'s signature is
/// sync, `TrustPolicy::compile` is sync, and both must resolve key material the
/// same way — a second async twin is exactly the drift
/// [`compile_keyless_matcher`] exists to prevent. The read is one config-scale
/// PEM per policy resolution, on a path that is already parsing config files.
fn compile_key_reference(
    key: &crate::oci::sign::KeyRef,
    scope: &str,
) -> Result<sigstore::crypto::CosignVerificationKey, TrustPolicyError> {
    // A recognised-but-unimplemented backend must say so by name. Reading its
    // `rest` as a filename is how `awskms://alias/release` becomes "no such file
    // or directory", which sends the operator to the wrong problem entirely.
    if let Some(path) = key.as_path() {
        return read_key_file(path, scope).and_then(|pem| parse_verification_key(&pem, scope, KeyFault::FileBytes));
    }
    if let Some(variable) = key.as_env_var() {
        // The same three rules the signing half applies (`read_key_env`), and
        // the same two exit codes: nothing there is 74, something there that
        // no key can be is 65. A `key = "env://VAR"` in a policy that answered
        // differently from `--key env://VAR` would be the sign/verify drift
        // the shared reader exists to prevent.
        let pem = crate::oci::sign::key_ref::read_key_env(variable).map_err(|error| {
            use crate::oci::sign::key_ref::KeyEnvError;

            let (reason, fault) = match &error {
                KeyEnvError::Unset { .. } => (format!("absent: {error}"), KeyFault::Path),
                KeyEnvError::TooLarge { .. } => (format!("unusable: {error}"), KeyFault::FileBytes),
            };
            TrustPolicyError::KeyMalformed {
                scope: scope.to_owned(),
                reason,
                fault,
            }
        })?;
        return parse_verification_key(pem.as_bytes(), scope, KeyFault::FileBytes);
    }
    Err(TrustPolicyError::KeyReferenceInvalid {
        scope: scope.to_owned(),
        source: crate::oci::sign::KeyRefError::UnsupportedBackend { scheme: key.scheme() },
    })
}

/// Read a public-key PEM, bounded and refusing anything that is not a regular
/// file.
///
/// Both guards live in [`read_bounded`](crate::utility::fs::read_bounded) — one
/// implementation, shared with `crate::oci::sign::key_backend`'s *private* half.
/// Only the wording differs: this side names the offending signer's scope, the
/// signing side says "cannot read key material".
///
/// They are load-bearing because this path is reachable from a **project**
/// `ocx.toml` — a file a cloned repository supplies. `resolve_tiered` compiles a
/// matched project policy on the `ocx package verify` / `ocx package sbom`
/// trust-policy carve-out (auto-verify passes an empty project set today, #99),
/// so `key = "/dev/zero"` in someone else's repository would otherwise read
/// until memory ran out (CWE-400). The cap is not a containment check and must
/// not be read as one: a local tier may name any path its author likes, and the
/// *operator* tiers legitimately do.
fn read_key_file(path: &std::path::Path, scope: &str) -> Result<Vec<u8>, TrustPolicyError> {
    use crate::utility::fs::BoundedReadError;

    crate::utility::fs::read_bounded(path, MAX_KEY_PEM_BYTES).map_err(|error| match error {
        BoundedReadError::Io { source, .. } => TrustPolicyError::KeyUnreadable {
            scope: scope.to_owned(),
            path: path.to_path_buf(),
            source,
        },
        BoundedReadError::TooLarge { cap, .. } => TrustPolicyError::KeyMalformed {
            scope: scope.to_owned(),
            reason: format!("larger than {cap} bytes, which no public key is"),
            fault: KeyFault::FileBytes,
        },
        not_regular => TrustPolicyError::KeyMalformed {
            scope: scope.to_owned(),
            reason: not_regular.to_string(),
            fault: KeyFault::Path,
        },
    })
}

/// Parse SPKI PEM into a verification key. **No decryption anywhere**: verifying
/// needs only the public half, so the encrypted cosign envelope never appears on
/// this side.
fn parse_verification_key(
    pem: &[u8],
    scope: &str,
    fault: KeyFault,
) -> Result<sigstore::crypto::CosignVerificationKey, TrustPolicyError> {
    sigstore::crypto::CosignVerificationKey::try_from_pem(pem).map_err(|error| {
        // `KeyMalformed` carries a description, not a `#[source]`, so the
        // sigstore error goes to the log rather than being flattened into a
        // message an operator cannot act on.
        log::debug!("public key for scope `{scope}` rejected by sigstore: {error}");
        TrustPolicyError::KeyMalformed {
            scope: scope.to_owned(),
            reason: "not a PEM-encoded SPKI public key".to_owned(),
            fault,
        }
    })
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
/// `config_dir` is the directory of the `ocx.toml` the text came from, and
/// every relative `file:` signer key is anchored against it before the policies
/// are returned — the same rule `Config::anchor_relative_paths` applies to the
/// `config.toml` tiers. It is a **parameter rather than a later call** because
/// which public key admits a signature must not depend on where the operator
/// happened to `cd`, and an anchoring step a caller can forget is how that
/// dependency gets reintroduced.
///
/// # Errors
/// [`TrustPolicyError::DocumentInvalid`] when the document is not valid TOML,
/// or when a `[[trust.policy]]` entry itself is malformed at the field level.
pub fn policies_from_ocx_toml(
    toml_str: &str,
    config_dir: &std::path::Path,
) -> Result<Vec<TrustPolicy>, TrustPolicyError> {
    #[derive(Deserialize)]
    struct ProjectTrustOnly {
        trust: Option<TrustConfig>,
    }
    let parsed: ProjectTrustOnly =
        toml::from_str(toml_str).map_err(|source| TrustPolicyError::DocumentInvalid { source })?;
    let mut policies = parsed.trust.map(|trust| trust.policy).unwrap_or_default();
    for policy in &mut policies {
        policy.anchor_relative_keys(config_dir);
    }
    Ok(policies)
}

/// A trust-policy configuration error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrustPolicyError {
    /// A `kind = "keyless"` signer omits `oidc_issuer`.
    #[error("trust policy for scope {scope:?} declares a keyless signer with no oidc_issuer")]
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
    /// A policy declares `signers = []`. Refused rather than read as a
    /// catch-all: an empty set of acceptable signers accepts nothing, so the
    /// permissive reading would turn a deleted line into "anyone may sign".
    #[error(
        "policy for scope `{scope}` declares an empty `signers` array; an empty set of signers accepts nothing \
         and is a configuration error, not a catch-all"
    )]
    NoSigners {
        /// The offending policy's scope.
        scope: String,
    },
    /// Every signer in the policy declares a `kind` this build does not know,
    /// so the policy compiles to no backend at all.
    ///
    /// Distinct from [`Self::NoSigners`]: the array is not empty, the operator
    /// wrote signers, and this ocx is simply too old to evaluate any of them.
    /// Naming that is the difference between "upgrade ocx" and "your config is
    /// wrong".
    #[error(
        "policy for scope `{scope}` declares no signer this build understands; every entry names a `kind` this \
         version of ocx does not implement"
    )]
    NoUsableSigner {
        /// The offending policy's scope.
        scope: String,
    },
    /// A `kind = "key"` signer sets both `key` and `key_pem`, so it names two
    /// keys and pins neither.
    #[error("signer for scope `{scope}` sets both `key` and `key_pem`; set exactly one")]
    KeyConflict {
        /// The offending policy's scope.
        scope: String,
    },
    /// A `kind = "key"` signer sets neither `key` nor `key_pem`, so it names no
    /// key at all.
    #[error("signer for scope `{scope}` sets neither `key` nor `key_pem`; set exactly one")]
    KeyUnset {
        /// The offending policy's scope.
        scope: String,
    },
    /// A `key` reference is not one this build can resolve — most usefully, a
    /// recognised KMS scheme with no implementation.
    ///
    /// Distinct from [`Self::KeyUnreadable`] on purpose: `awskms://alias/release`
    /// is not a missing file, and reporting it as one sends the operator to
    /// their filesystem instead of to the unimplemented backend.
    #[error("signer for scope `{scope}` names a key reference this build cannot resolve")]
    KeyReferenceInvalid {
        /// The offending policy's scope.
        scope: String,
        /// Which part of the reference grammar refused it — its `Display` names
        /// the scheme.
        #[source]
        source: crate::oci::sign::KeyRefError,
    },
    /// The file a `file:` key reference names could not be read.
    #[error("signer for scope `{scope}` names a key file that cannot be read: {}", path.display())]
    KeyUnreadable {
        /// The offending policy's scope.
        scope: String,
        /// The path that could not be read, as resolved.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The project `ocx.toml` carrying `[[trust.policy]]` is not valid TOML, or
    /// an entry is malformed at the field level.
    ///
    /// Typed rather than surfaced as a bare `toml::de::Error`: the classifier's
    /// downcast ladder has no rung for a foreign type, so the bare error fell
    /// through to exit 1 `internal` and reported an operator's typo as an ocx
    /// bug — while the identical malformation in `config.toml` has always
    /// exited 78. One malformed trust policy, one exit code, whichever file it
    /// is written in.
    #[error("project config declares a `[[trust.policy]]` section that is not valid TOML")]
    DocumentInvalid {
        /// What TOML rejected, and where.
        #[source]
        source: toml::de::Error,
    },
    /// Key material a signer named could not be turned into a public key.
    #[error("signer for scope `{scope}` declares key material that is {reason}")]
    KeyMalformed {
        /// The offending policy's scope.
        scope: String,
        /// What about the material was rejected.
        reason: String,
        /// Which of the three things was unusable — the exit code follows this
        /// and nothing else.
        fault: KeyFault,
    },
}

/// Which thing a [`TrustPolicyError::KeyMalformed`] found unusable.
///
/// One rule, keyed on *what failed* rather than on who called: a path that is
/// not a readable file is an I/O problem, bytes that are not a key are a data
/// problem, and config text that is not a key is a config problem. Carrying it
/// on the error is what lets one variant answer all three without the
/// classifier having to know which door it came through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFault {
    /// The path named something that is not a readable regular file. 74
    /// `io_error` — the same code `--config` already answers for a path that
    /// "exists but cannot be read (permission denied, not a regular file)".
    Path,
    /// A regular file was read, and its bytes are not a key. 65 `data_error`,
    /// matching what `ocx package sign` answers for the same file.
    FileBytes,
    /// An inline `key_pem` in a config document is not a key. 78
    /// `config_error`: the config text itself is what is wrong.
    ConfigText,
}

impl TrustPolicyError {
    /// Whether this refusal is the unsupported-key-backend verdict wearing a
    /// second hat.
    ///
    /// `--key awskms://alias/release`, `key = "awskms://alias/release"` in a
    /// `config.toml` signer, and the same line in a managed-config payload are
    /// the **same error through three doors**: all build
    /// [`KeyRefError::UnsupportedBackend`], and the flag door answers 85
    /// `unsupported_key_backend`. Each of the other two flattened onto 78
    /// `config_error`, which tells a fleet script "your config is malformed"
    /// for a backend that is simply not built yet — the one distinction 85
    /// exists to make, and the one it could never fire on.
    ///
    /// A method on the error rather than a predicate copied into each
    /// classifier: an exit code that drifts from its own slug is the failure
    /// mode a second copy invites, and this initiative has produced three
    /// Blocks from two spellings of one concept.
    #[must_use]
    pub fn names_unsupported_backend(&self) -> bool {
        matches!(
            self,
            Self::KeyReferenceInvalid {
                source: crate::oci::sign::KeyRefError::UnsupportedBackend { .. },
                ..
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn policy(scope: &str, identity: Option<&str>, regexp: Option<&str>, issuer: &str) -> TrustPolicy {
        TrustPolicy {
            scope: Some(ScopeSpec::Prefix(scope.to_string())),
            builder: None,
            signers: vec![SignerSpec::Keyless(KeylessMatcher {
                identity: identity.map(str::to_string),
                identity_regexp: regexp.map(str::to_string),
                oidc_issuer: Some(issuer.to_string()),
            })],
            system_locked: false,
        }
    }

    /// The whole refusal an operator reads: every link of the source chain,
    /// joined the way anyhow's `{err:#}` renders it at the CLI boundary.
    ///
    /// `policies_from_ocx_toml` answers a document that will not parse with
    /// `TrustPolicyError::DocumentInvalid`, whose own sentence names the file
    /// and keeps serde's message as its `#[source]`. Asserting on the head
    /// alone stops reading exactly where the actionable half begins — and, for
    /// a `!contains` assertion, passes for that reason alone.
    fn rendered_chain(error: &dyn std::error::Error) -> String {
        std::iter::successors(Some(error), |e| e.source())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ")
    }

    /// The exact identity a parsed policy's first keyless signer pins — for
    /// assertions that only care which entry won resolution.
    fn pinned_identity(policy: &TrustPolicy) -> Option<&str> {
        policy.signers.iter().find_map(|signer| match signer {
            SignerSpec::Keyless(keyless) => keyless.identity.as_deref(),
            SignerSpec::Key(_) | SignerSpec::Unknown => None,
        })
    }

    /// The identity rule a compiled policy's single keyless backend resolved to.
    /// Asserts there is exactly one, so a helper written for a one-signer policy
    /// cannot silently answer for the first of several.
    fn compiled_identity(policy: &CompiledPolicy) -> &IdentityRule {
        let [PolicyBackend::Keyless(keyless)] = policy.backends.as_slice() else {
            panic!("expected exactly one keyless backend, got {:?}", policy.backends);
        };
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
signers = [{ kind = "keyless", identity = "a", oidc_issuer = "iss" }]

[[trust.policy]]
scope = { include = ["ghcr.io/acme/*", "ocx.sh/cmake"], exclude = ["ghcr.io/acme/experimental/*"], future_key = "newer ocx" }
signers = [{ kind = "keyless", identity = "b", oidc_issuer = "iss" }]
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
signers = [{ kind = "keyless", identity = "id", oidc_issuer = "iss" }]
"#;
        let policies = policies_from_ocx_toml(toml, std::path::Path::new("/project"))
            .expect("the object form parses in an ocx.toml");
        assert_eq!(policies.len(), 1);
        assert!(policies[0].matches_scope("ocx.sh/cmake"));
        assert!(!policies[0].matches_scope("ocx.sh/experimental"));
    }

    /// A relative `file:` key in a project `ocx.toml` anchors on the file that
    /// declared it, never on the process working directory.
    ///
    /// The `config.toml` tiers get this from `Config::anchor_relative_paths`;
    /// the project tier is read by a different function, and until it took the
    /// directory as a parameter the same reference named a different key
    /// depending on where the operator ran `ocx package verify` from — which
    /// public key admits a signature is not a `cd`-dependent question (CWE-426).
    #[test]
    fn a_relative_project_key_anchors_on_the_ocx_toml_dir_not_the_cwd() {
        let toml = r#"
[[trust.policy]]
scope = "ocx.sh/*"
signers = [
  { kind = "key", key = "keys/release.pub" },
  { kind = "key", key = "/etc/ocx/absolute.pub" },
]
"#;
        let policies =
            policies_from_ocx_toml(toml, std::path::Path::new("/srv/project")).expect("the signer array parses");
        let keys: Vec<Option<&str>> = policies[0]
            .signers
            .iter()
            .map(|signer| match signer {
                SignerSpec::Key(matcher) => matcher.key.as_deref(),
                other => panic!("expected two key signers, got {other:?}"),
            })
            .collect();
        // As paths: `join` writes the platform separator, so on Windows the
        // anchored value is `/srv/project\keys/release.pub`.
        let anchored = std::path::Path::new("/srv/project").join("keys").join("release.pub");
        assert_eq!(
            keys[0].map(std::path::Path::new),
            Some(anchored.as_path()),
            "a relative key must be rewritten against the declaring file's directory"
        );
        // Still bytes, and deliberately: the claim is that this reference is
        // returned exactly as written — which is a string question, not a path
        // one.
        assert_eq!(
            keys[1],
            Some("/etc/ocx/absolute.pub"),
            "a rooted reference is already unambiguous and must be left untouched"
        );
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
            signers: Vec::new(),
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
signers = [{ kind = "keyless", identity = "ci@acme.example", oidc_issuer = "iss" }]
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
                "[[trust.policy]]\n{scope}\nsigners = [{{ kind = \"keyless\", identity = \"id\", oidc_issuer = \"iss\" }}]\n"
            );
            let refusal = policies_from_ocx_toml(&toml, std::path::Path::new("/project"))
                .expect_err("a table naming neither list must not parse");
            let error = rendered_chain(&refusal);
            assert!(
                error.contains("needs `include` or `exclude`"),
                "the refusal must name the fix; got: {error}"
            );
        }

        // The floor is "one recognized key", not "no unknown keys" — an unknown
        // key riding ALONGSIDE a real one still degrades to the known parts.
        let toml = "[[trust.policy]]\nscope = { include = [\"ghcr.io/acme/*\"], future_key = 1 }\nsigners = [{ kind = \"keyless\", identity = \"id\", oidc_issuer = \"iss\" }]\n";
        let policies = policies_from_ocx_toml(toml, std::path::Path::new("/project"))
            .expect("an unknown key beside a real one is tolerated");
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
                "[[trust.policy]]\n{scope}\nsigners = [{{ kind = \"keyless\", identity = \"id\", oidc_issuer = \"iss\" }}]\n"
            );
            let refusal = policies_from_ocx_toml(&toml, std::path::Path::new("/project"))
                .expect_err("a malformed scope must not parse");
            let error = rendered_chain(&refusal);
            assert!(
                !error.contains("untagged"),
                "the untagged-enum wording must not reach an operator; got: {error}"
            );
        }

        // The three scalar shapes reach the visitor and get the full sentence;
        // a bad `include` element type is serde's own typed message one level
        // down, which is already actionable, so it is asserted separately.
        let toml = "[[trust.policy]]\nscope = 42\nsigners = [{ kind = \"keyless\", identity = \"id\", oidc_issuer = \"iss\" }]\n";
        let refusal = policies_from_ocx_toml(toml, std::path::Path::new("/project"))
            .expect_err("an integer scope must not parse");
        let error = rendered_chain(&refusal);
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
signers = [{ kind = "keyless", identity = "id", oidc_issuer = "iss" }]
"#;
        let policies = policies_from_ocx_toml(toml, std::path::Path::new("/project"))
            .expect("unrelated malformed section is ignored");
        assert_eq!(policies.len(), 1);
    }

    /// A malformed project `ocx.toml` is an operator's typo, and must exit 78
    /// `trust_policy_invalid` — the same code the identical malformation in
    /// `config.toml` has always produced.
    ///
    /// The load-bearing half is the *classifier*, not the variant name. A bare
    /// `toml::de::Error` is a foreign type with no rung in the downcast ladder,
    /// so bubbling it fell through to exit 1 `internal` and reported the typo as
    /// an ocx bug. So the exit code is taken through `classify_error` over the
    /// wrapper its one caller builds, not through a direct `exit_code()` call:
    /// exit 1 is produced by the ladder finding nothing, which only the ladder
    /// can show.
    #[test]
    fn a_malformed_ocx_toml_classifies_as_a_trust_policy_refusal_not_an_internal_error() {
        // Control: the same document, well formed, parses.
        let well_formed = "[[trust.policy]]\nscope = \"ghcr.io/acme/*\"\n\
                           signers = [{ kind = \"keyless\", identity = \"id\", oidc_issuer = \"iss\" }]\n";
        assert_eq!(
            policies_from_ocx_toml(well_formed, Path::new("/project"))
                .expect("the control document parses")
                .len(),
            1,
            "the control must actually yield a policy"
        );

        // An unterminated string: broken at the document level, so no
        // field-level `[[trust.policy]]` check can reach it.
        let malformed = "[[trust.policy]]\nscope = \"ghcr.io/acme/*\n";
        let error = policies_from_ocx_toml(malformed, Path::new("/project"))
            .expect_err("a document that is not valid TOML must be refused");
        assert!(
            matches!(error, TrustPolicyError::DocumentInvalid { .. }),
            "the refusal must be typed, not a bare toml::de::Error: {error:?}"
        );

        let wrapped = crate::oci::verify::VerifyError::new(
            crate::oci::Identifier::parse("registry.example/pkg:1.0").expect("parse test identifier"),
            crate::oci::verify::VerifyErrorKind::TrustPolicyInvalid(error),
        );
        assert_eq!(
            crate::cli::classify_error(&wrapped),
            crate::cli::ExitCode::ConfigError,
            "a malformed ocx.toml is 78 `config_error`, never 1 `internal`"
        );
        assert_eq!(
            crate::cli::ClassifyErrorKind::kind_detail(&wrapped.kind),
            "trust_policy_invalid"
        );
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
signers = [{ kind = "keyless", identity = "attacker@example.test", oidc_issuer = "https://example.test" }]
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
    fn a_keyless_signer_compiles_to_a_keyless_backend() {
        // The shape a user writes: a `signers` array beside `scope`. Everything
        // below depends on this parsing at all.
        let toml = r#"
[[trust.policy]]
scope = "ghcr.io/acme/*"
signers = [
  { kind = "keyless", identity = "release@acme.example", oidc_issuer = "https://token.actions.githubusercontent.com" },
]
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("the signers form parses");
        let compiled = root.trust.policy[0]
            .compile()
            .expect("a complete keyless signer compiles");
        let [PolicyBackend::Keyless(keyless)] = compiled.backends.as_slice() else {
            panic!(
                "one keyless signer compiles to one keyless backend, got {:?}",
                compiled.backends
            );
        };
        assert!(matches!(&keyless.identity, IdentityRule::Exact(id) if id == "release@acme.example"));
        assert_eq!(keyless.issuer, "https://token.actions.githubusercontent.com");
    }

    #[test]
    fn the_superseded_keyless_sub_table_declares_no_signer_and_says_so() {
        // Pre-1.0, `signers` replaced `[trust.policy.keyless]` outright — no
        // dual-form parsing. Unknown keys stay tolerated fleet-wide, so the old
        // spelling still *parses*, and that is precisely why the refusal has to
        // come from compilation. Silently reading a signer-less entry as "no
        // policy" would leave a user who believes they pinned an identity with
        // no pin and no signal.
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
        let root: Root = toml::from_str(toml).expect("the old sub-table is tolerated as unknown, not rejected");
        let policy = &root.trust.policy[0];
        assert!(
            policy.signers.is_empty(),
            "the superseded sub-table must not populate `signers`"
        );

        let error = policy
            .compile()
            .expect_err("an entry naming no signer cannot govern a scope");
        assert!(matches!(error, TrustPolicyError::NoSigners { .. }));
        let rendered = error.to_string();
        assert!(
            rendered.contains("signers"),
            "the refusal must name the array the matchers belong in; got: {rendered}"
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
            signers: Vec::new(),
            system_locked: false,
        };
        assert!(matches!(bare.compile(), Err(TrustPolicyError::NoSigners { .. })));
    }

    #[test]
    fn compile_rejects_a_keyless_matcher_without_an_issuer() {
        // `oidc_issuer` is Option at the serde layer for fleet forward-compat
        // and mandatory here: an identity with no issuer would accept the same
        // SAN minted by any OIDC provider.
        let no_issuer = TrustPolicy {
            scope: Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())),
            builder: None,
            signers: vec![SignerSpec::Keyless(KeylessMatcher {
                identity: Some("release@acme.example".to_string()),
                identity_regexp: None,
                oidc_issuer: None,
            })],
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
signers = [
  { kind = "keyless", identity = "release@acme.example", oidc_issuer = "https://token.actions.githubusercontent.com" },
]
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
            signers: Vec::new(),
            system_locked: false,
        };
        assert!(matches!(
            builder_only.compile(),
            Err(TrustPolicyError::NoSigners { .. })
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
            signers: vec![SignerSpec::Keyless(KeylessMatcher {
                identity: Some("release@acme.example".to_string()),
                identity_regexp: None,
                oidc_issuer: Some("https://token.actions.githubusercontent.com".to_string()),
            })],
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
        let [PolicyBackend::Keyless(keyless)] = flags.backends.as_slice() else {
            panic!(
                "the flag override names exactly one keyless signer, got {:?}",
                flags.backends
            );
        };
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
signers = [
  { kind = "keyless", identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3", oidc_issuer = "https://token.actions.githubusercontent.com" },
]

[[trust.policy]]
scope = "ghcr.io/other/*"
signers = [
  { kind = "keyless", identity_regexp = "^https://example\\.com/.*$", oidc_issuer = "https://example.com" },
]
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
            pinned_identity(&root.trust.policy[0]),
            Some("https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3")
        );
        assert!(matches!(
            root.trust.policy[1].signers.as_slice(),
            [SignerSpec::Keyless(keyless)] if keyless.identity_regexp.is_some()
        ));
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
signers = [
  { kind = "keyless", identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3", oidc_issuer = "https://token.actions.githubusercontent.com", nested_future_field = "added by a newer ocx, inside the signer entry" },
]
"#;
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let root: Root = toml::from_str(toml).expect("unknown field is tolerated, not rejected");
        assert_eq!(root.trust.policy.len(), 1);
        let policy = &root.trust.policy[0];
        assert_eq!(policy.scope, Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())));
        let [SignerSpec::Keyless(keyless)] = policy.signers.as_slice() else {
            panic!("the signer entry survives the unknown fields, got {:?}", policy.signers);
        };
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
signers = [
  { kind = "keyless", identity = "https://github.com/acme/tool/.github/workflows/release.yml@refs/tags/v1.2.3", oidc_issuer = "https://token.actions.githubusercontent.com" },
]
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

    /// S-015 / C-083 — `[trust.sigstore] trusted_root` takes the `file://`
    /// spelling, the one it sat three lines from `signers[].key` accepting
    /// without it, for no stated reason (#379).
    ///
    /// The spelling is consumed at this seam, so no reader below the loader
    /// learns about it: the stored value is always a plain resolved path.
    #[test]
    fn anchor_relative_root_takes_the_file_url_spelling() {
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }

        let parsed: Root =
            toml::from_str("[trust.sigstore]\ntrusted_root = \"file:///opt/sigstore/trusted-root.json\"\n")
                .expect("the file:// spelling parses");
        let mut sigstore = parsed.trust.sigstore.expect("sigstore table");
        sigstore.anchor_relative_root(Path::new("/etc/ocx"));
        assert_eq!(
            sigstore.trusted_root.as_deref(),
            Some(Path::new("/opt/sigstore/trusted-root.json")),
            "an absolute file:// root resolves to the path it names"
        );

        // The relative half takes the same rule a bare path does — one
        // grammar, one anchoring seam.
        let mut relative = SigstoreTrust {
            trusted_root: Some(PathBuf::from("file://sigstore/trusted-root.json")),
            ..SigstoreTrust::default()
        };
        relative.anchor_relative_root(Path::new("/etc/ocx"));
        assert_eq!(
            relative.trusted_root.as_deref(),
            Some(Path::new("/etc/ocx").join("sigstore/trusted-root.json").as_path())
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

    /// A `kind = "key"` signer with the two forms set as given.
    fn key_signer(key: Option<&str>, key_pem: Option<&str>) -> [SignerSpec; 1] {
        [SignerSpec::Key(KeyMatcher {
            key: key.map(str::to_string),
            key_pem: key_pem.map(str::to_string),
        })]
    }

    const SPKI_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA\n-----END PUBLIC KEY-----\n";

    #[test]
    fn an_empty_signers_array_is_refused() {
        // Fail closed. A policy that names no acceptable signer accepts
        // nothing; reading `signers = []` as a catch-all would turn a deleted
        // line into "trust anyone", which is the whole failure this validator
        // exists to prevent.
        let empty = validate_signers(&[], "ghcr.io/acme/*");
        assert!(
            matches!(&empty, Err(TrustPolicyError::NoSigners { scope }) if scope == "ghcr.io/acme/*"),
            "an empty signers array must be refused, naming the scope; got {empty:?}"
        );

        // The positive half, and it carries as much weight as the refusal: a
        // validator that rejected *every* array would pass the assertion above
        // while accepting nothing at all.
        let one = [SignerSpec::Keyless(KeylessMatcher {
            identity: Some("release@acme.example".to_string()),
            identity_regexp: None,
            oidc_issuer: Some("https://accounts.google.com".to_string()),
        })];
        assert!(
            validate_signers(&one, "ghcr.io/acme/*").is_ok(),
            "a one-element signers array must be accepted"
        );
    }

    #[test]
    fn a_key_signer_declares_exactly_one_form() {
        let scope = "ghcr.io/acme/*";

        // Both: names two keys and pins neither. Ambiguity in a trust decision
        // is refused, not resolved by precedence.
        let both = validate_signers(&key_signer(Some("acme.pub"), Some(SPKI_PEM)), scope);
        assert!(
            matches!(&both, Err(TrustPolicyError::KeyConflict { scope: s }) if s == scope),
            "both key forms must be refused; got {both:?}"
        );

        // Neither: names no key at all — same fail-closed reason as an empty
        // array.
        let neither = validate_signers(&key_signer(None, None), scope);
        assert!(
            matches!(&neither, Err(TrustPolicyError::KeyUnset { scope: s }) if s == scope),
            "a key signer with neither form must be refused; got {neither:?}"
        );

        // Each alone is the accepted shape. Without these two rows, a validator
        // that refused every key signer would pass both assertions above.
        assert!(
            validate_signers(&key_signer(Some("acme.pub"), None), scope).is_ok(),
            "`key` alone is a complete key signer"
        );
        assert!(
            validate_signers(&key_signer(None, Some(SPKI_PEM)), scope).is_ok(),
            "`key_pem` alone is a complete key signer"
        );
    }

    /// A `kind = "keyless"` signer that names nobody.
    fn keyless_signer(identity: Option<&str>, regexp: Option<&str>, issuer: Option<&str>) -> [SignerSpec; 1] {
        [SignerSpec::Keyless(KeylessMatcher {
            identity: identity.map(str::to_string),
            identity_regexp: regexp.map(str::to_string),
            oidc_issuer: issuer.map(str::to_string),
        })]
    }

    #[test]
    fn a_keyless_signer_declares_an_identity() {
        let scope = "ghcr.io/acme/*";

        // Neither identity form: the entry names no signer at all. Accepting it
        // would let `signers = [{ kind = "keyless" }]` pass shape validation and
        // reach the matcher as "any certificate" — the same silent bypass an
        // empty array would be, spelled one line longer.
        let unset = validate_signers(&keyless_signer(None, None, Some("https://accounts.google.com")), scope);
        assert!(
            matches!(&unset, Err(TrustPolicyError::IdentityUnset { scope: s }) if s == scope),
            "a keyless signer naming no identity must be refused; got {unset:?}"
        );

        // Both forms: two identities pinned, neither authoritative. Ambiguity in
        // a trust decision is refused, not resolved by precedence — the same
        // rule `TrustPolicy::compile` enforces, because it is the same function
        // enforcing it.
        let both = validate_signers(
            &keyless_signer(
                Some("release@acme.example"),
                Some("^ci-.*$"),
                Some("https://accounts.google.com"),
            ),
            scope,
        );
        assert!(
            matches!(&both, Err(TrustPolicyError::IdentityConflict { scope: s }) if s == scope),
            "a keyless signer setting both identity forms must be refused; got {both:?}"
        );

        // Each form alone is complete. Without these, a validator that refused
        // every keyless signer would satisfy both assertions above.
        assert!(
            validate_signers(
                &keyless_signer(Some("release@acme.example"), None, Some("https://accounts.google.com")),
                scope
            )
            .is_ok(),
            "`identity` alone is a complete keyless signer"
        );
        assert!(
            validate_signers(
                &keyless_signer(None, Some("^ci-.*$"), Some("https://accounts.google.com")),
                scope
            )
            .is_ok(),
            "`identity_regexp` alone is a complete keyless signer"
        );
    }

    #[test]
    fn a_keyless_signer_declares_an_issuer() {
        let scope = "ghcr.io/acme/*";

        // An identity without an issuer pins a SAN string any issuer could mint.
        // The issuer is half the identity, so an entry missing it is incomplete,
        // not permissive.
        let missing = validate_signers(&keyless_signer(Some("release@acme.example"), None, None), scope);
        assert!(
            matches!(&missing, Err(TrustPolicyError::IssuerUnset { scope: s }) if s == scope),
            "a keyless signer naming no issuer must be refused; got {missing:?}"
        );

        // The positive control: the identical entry with an issuer is accepted,
        // so the refusal above is about the missing issuer and nothing else.
        assert!(
            validate_signers(
                &keyless_signer(Some("release@acme.example"), None, Some("https://accounts.google.com")),
                scope
            )
            .is_ok(),
            "the same entry with an issuer is a complete keyless signer"
        );
    }

    #[test]
    fn signer_entries_parse_from_the_frozen_kind_tagged_spelling() {
        // The config surface is a wire format: `kind` and the two variant
        // spellings are what an operator writes in config.toml, so a rename here
        // silently stops matching every already-deployed file.
        let toml = r#"
signers = [
  { kind = "keyless", identity = "release@acme.example", oidc_issuer = "https://accounts.google.com" },
  { kind = "keyless", identity_regexp = "^ci-.*@acme\\.example$", oidc_issuer = "https://token.actions.githubusercontent.com" },
  { kind = "key", key = "etc/acme-release.pub" },
  { kind = "key", key_pem = "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA\n-----END PUBLIC KEY-----\n" },
]
"#;
        #[derive(Deserialize)]
        struct Doc {
            signers: Vec<SignerSpec>,
        }
        let doc: Doc = toml::from_str(toml).expect("the frozen signers spelling parses");
        assert_eq!(doc.signers.len(), 4);

        let SignerSpec::Keyless(exact) = &doc.signers[0] else {
            panic!("row 1 is the keyless form")
        };
        assert_eq!(exact.identity.as_deref(), Some("release@acme.example"));
        let SignerSpec::Keyless(pattern) = &doc.signers[1] else {
            panic!("row 2 is the keyless form")
        };
        assert_eq!(pattern.identity_regexp.as_deref(), Some(r"^ci-.*@acme\.example$"));
        let SignerSpec::Key(by_reference) = &doc.signers[2] else {
            panic!("row 3 is the key form")
        };
        assert_eq!(by_reference.key.as_deref(), Some("etc/acme-release.pub"));
        let SignerSpec::Key(inline) = &doc.signers[3] else {
            panic!("row 4 is the key form")
        };
        assert_eq!(inline.key_pem.as_deref(), Some(SPKI_PEM));

        assert!(validate_signers(&doc.signers, "ghcr.io/acme/*").is_ok());

        // The tag survives the write side too: `ocx config push` re-serializes a
        // policy, and a `kind` that only existed on the read path would publish
        // an object no ocx can parse back.
        let round_tripped = serde_json::to_value(&doc.signers[2]).expect("a signer serializes");
        assert_eq!(round_tripped["kind"], "key");
        assert_eq!(round_tripped["key"], "etc/acme-release.pub");
    }

    #[test]
    fn a_key_signers_reference_is_the_same_grammar_the_key_flag_parses() {
        // One grammar, one parser (`oci::sign::key_ref`). A second parser here
        // is how a `key` in a policy and a `--key` on the command line drift
        // into meaning different things — including on the spelling this
        // grammar *refuses*, asserted below.
        let parsed = crate::oci::sign::KeyRef::parse("etc/acme-release.pub").expect("the frozen spelling parses");
        assert_eq!(parsed.scheme(), crate::oci::sign::Scheme::File);
        assert_eq!(parsed.rest(), "etc/acme-release.pub");
        // And the removed spelling is refused on this door too, by the same
        // parser: a policy that still accepted `file:` while `--key` refused it
        // would be two grammars again.
        assert!(
            matches!(
                crate::oci::sign::KeyRef::parse("file:etc/acme-release.pub"),
                Err(crate::oci::sign::KeyRefError::FileColonPrefix { .. })
            ),
            "a policy `key` takes the same refusal `--key` does"
        );
    }

    // ── Key signers (WP9a) ───────────────────────────────────────────────────

    /// The public half of the golden cosign pair. `include_str!` rather than a
    /// runtime read: a moved fixture becomes a compile error.
    const GOLDEN_PUBLIC_KEY_PEM: &str = include_str!("../../../test/tests/fixtures/golden/keys/cosign.pub");

    fn key_policy(scope: &str, matcher: KeyMatcher) -> TrustPolicy {
        TrustPolicy {
            scope: Some(ScopeSpec::Prefix(scope.to_string())),
            builder: None,
            signers: vec![SignerSpec::Key(matcher)],
            system_locked: false,
        }
    }

    /// The inline form a fleet receives: `key_pem` verbatim, no file to find.
    #[test]
    fn an_inline_key_pem_compiles_to_a_key_backend() {
        let compiled = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: None,
                key_pem: Some(GOLDEN_PUBLIC_KEY_PEM.to_string()),
            },
        )
        .compile()
        .expect("a valid SPKI PEM compiles");
        assert!(
            matches!(compiled.backends.as_slice(), [PolicyBackend::Key(_)]),
            "one key signer compiles to one key backend, got {:?}",
            compiled.backends
        );
    }

    /// A project `ocx.toml` is a file a *cloned repository* supplies, and the
    /// auto-verify gate compiles its matched policies on every install surface.
    /// An unbounded read there turns `key = "/dev/zero"` in someone else's
    /// repository into an out-of-memory kill (CWE-400), so the read is capped.
    #[test]
    fn a_key_file_larger_than_the_cap_is_refused() {
        let directory = tempfile::tempdir().expect("tempdir");
        let key_path = directory.path().join("enormous.pub");
        std::fs::write(&key_path, vec![b'x'; (MAX_KEY_PEM_BYTES + 1) as usize]).expect("write the key");

        let error = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some(key_path.display().to_string()),
                key_pem: None,
            },
        )
        .compile()
        .expect_err("a file past the cap must be refused, not read");
        assert!(
            matches!(&error, TrustPolicyError::KeyMalformed { reason, .. } if reason.contains("larger than")),
            "got {error:?}"
        );
    }

    /// The other half of the same guard, and the one a size cap alone misses: a
    /// character device reports length 0 and yields forever, so the refusal has
    /// to be on the file *type*, not on a stat'd length.
    #[cfg(unix)]
    #[test]
    fn a_key_reference_naming_a_character_device_is_refused() {
        let error = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some("/dev/zero".to_string()),
                key_pem: None,
            },
        )
        .compile()
        .expect_err("/dev/zero is not a key file");
        assert!(
            matches!(&error, TrustPolicyError::KeyMalformed { reason, .. } if reason.contains("not a regular file")),
            "got {error:?}"
        );
    }

    /// **Fleet forward-compat.** A signer kind a newer ocx invents must not make
    /// the whole `config.toml` unparseable — a managed payload that fails to
    /// parse is dropped wholesale, which would delete every trust policy the
    /// fleet has. The unknown entry narrows to nothing; its keyless sibling in
    /// the same policy still compiles and still matches.
    #[test]
    fn an_unknown_signer_kind_parses_and_leaves_its_siblings_alone() {
        let document: TrustPolicy = toml::from_str(
            "scope = \"ghcr.io/acme/*\"\n\
             signers = [\n\
               { kind = \"kms\", uri = \"awskms://alias/release\" },\n\
               { kind = \"keyless\", identity = \"ci@acme.example\", oidc_issuer = \"https://iss.example\" },\n\
             ]\n",
        )
        .expect("an unknown kind must not fail the parse");
        assert_eq!(document.signers[0], SignerSpec::Unknown);

        let compiled = document.compile().expect("the keyless sibling still compiles");
        assert!(
            matches!(compiled.backends.as_slice(), [PolicyBackend::Keyless(_)]),
            "the unknown kind must contribute no backend: {:?}",
            compiled.backends
        );
    }

    /// Dropping the unknown entry must never leave a policy that accepts
    /// everyone. A policy this build understands nothing of is refused by name,
    /// so the operator reads "upgrade ocx" rather than a confusing identity
    /// mismatch at verify time.
    #[test]
    fn a_policy_of_only_unknown_signers_is_refused_by_name() {
        let document: TrustPolicy = toml::from_str(
            "scope = \"ghcr.io/acme/*\"\nsigners = [{ kind = \"kms\", uri = \"awskms://alias/release\" }]\n",
        )
        .expect("an unknown kind must not fail the parse");
        let error = document
            .compile()
            .expect_err("a policy with no usable signer accepts nobody");
        assert!(
            matches!(error, TrustPolicyError::NoUsableSigner { .. }),
            "got {error:?}"
        );
    }

    /// The path form, which is what a local tier writes. Legal here — the
    /// managed-payload refusal lives in `managed_config::publish`, not in a
    /// containment check on this side.
    #[test]
    fn a_file_key_reference_is_read_from_disk() {
        let directory = tempfile::tempdir().expect("tempdir");
        let key_path = directory.path().join("acme-release.pub");
        std::fs::write(&key_path, GOLDEN_PUBLIC_KEY_PEM).expect("write the key");

        let compiled = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some(key_path.display().to_string()),
                key_pem: None,
            },
        )
        .compile()
        .expect("a readable SPKI PEM at a path reference compiles");
        assert!(matches!(compiled.backends.as_slice(), [PolicyBackend::Key(_)]));
    }

    /// **The mixed policy.** A keyless entry and a key entry side by side is how
    /// a fleet migrates between signing models without touching scope — and both
    /// backends must survive compilation, in declaration order.
    #[test]
    fn a_policy_mixing_keyless_and_key_signers_compiles_both() {
        let mixed = TrustPolicy {
            scope: Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())),
            builder: None,
            signers: vec![
                SignerSpec::Keyless(KeylessMatcher {
                    identity: Some("release@acme.example".to_string()),
                    identity_regexp: None,
                    oidc_issuer: Some("https://accounts.google.com".to_string()),
                }),
                SignerSpec::Key(KeyMatcher {
                    key: None,
                    key_pem: Some(GOLDEN_PUBLIC_KEY_PEM.to_string()),
                }),
            ],
            system_locked: false,
        };
        let compiled = mixed.compile().expect("mixed kinds in one policy are legal");
        assert!(
            matches!(
                compiled.backends.as_slice(),
                [PolicyBackend::Keyless(_), PolicyBackend::Key(_)]
            ),
            "both signers compile, in declaration order; got {:?}",
            compiled.backends
        );
    }

    /// `compile()` must refuse an empty array, not just `validate_signers` in
    /// isolation — the guard is only worth anything on the path config actually
    /// takes.
    #[test]
    fn compile_refuses_a_policy_that_names_no_signer() {
        let bare = TrustPolicy {
            scope: Some(ScopeSpec::Prefix("ghcr.io/acme/*".to_string())),
            builder: None,
            signers: Vec::new(),
            system_locked: false,
        };
        let error = bare.compile().expect_err("an empty signer set accepts nothing");
        assert!(matches!(error, TrustPolicyError::NoSigners { .. }), "got {error:?}");
        assert!(
            error.to_string().contains("not a catch-all"),
            "the refusal must say why it is not permissive; got: {error}"
        );
    }

    /// A recognised KMS scheme must be refused **by name**. Reading its `rest`
    /// as a filename is how `awskms://alias/release` becomes "no such file or
    /// directory", which sends the operator to their filesystem instead of to
    /// the unimplemented backend.
    #[test]
    fn an_unimplemented_key_backend_is_refused_by_name_not_as_a_missing_file() {
        let error = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some("awskms://alias/acme-release".to_string()),
                key_pem: None,
            },
        )
        .compile()
        .expect_err("no KMS backend is implemented");

        let TrustPolicyError::KeyReferenceInvalid { source, .. } = &error else {
            panic!("a KMS scheme is a reference refusal, not an I/O one; got {error:?}");
        };
        assert!(
            source.to_string().contains("awskms"),
            "the refusal must name the scheme; got: {source}"
        );
        assert!(
            !error.to_string().contains("No such file"),
            "a KMS reference must never be reported as a missing file"
        );
    }

    /// An absent key file is an I/O refusal that names the path, distinct from
    /// the scheme refusal above — the two send an operator to different places.
    #[test]
    fn an_unreadable_key_file_is_refused_with_its_path() {
        let directory = tempfile::tempdir().expect("tempdir");
        let missing = directory.path().join("absent.pub");
        let error = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some(missing.display().to_string()),
                key_pem: None,
            },
        )
        .compile()
        .expect_err("an absent key file cannot compile");

        assert!(matches!(error, TrustPolicyError::KeyUnreadable { .. }), "got {error:?}");
        assert!(
            error.to_string().contains("absent.pub"),
            "the refusal must name the path it could not read; got: {error}"
        );
    }

    /// Bytes that are not a public key fail at compile, not later at verify —
    /// a policy that cannot verify anything must refuse while the operator is
    /// still looking at their config.
    #[test]
    fn key_material_that_is_not_a_public_key_is_refused_at_compile() {
        let error = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: None,
                key_pem: Some("-----BEGIN PUBLIC KEY-----\nnot base64 at all\n-----END PUBLIC KEY-----\n".to_string()),
            },
        )
        .compile()
        .expect_err("garbage is not a key");
        assert!(matches!(error, TrustPolicyError::KeyMalformed { .. }), "got {error:?}");
    }

    /// The verify side needs **only the public key** — no decryption anywhere.
    /// Handing it the encrypted private half must fail, which is what proves the
    /// two sides really are different code paths.
    #[test]
    fn an_encrypted_private_key_is_not_accepted_as_a_policy_key() {
        let private = include_str!("../../../test/tests/fixtures/golden/keys/cosign.key");
        let error = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: None,
                key_pem: Some(private.to_string()),
            },
        )
        .compile()
        .expect_err("verification takes a public key, and never decrypts");
        assert!(matches!(error, TrustPolicyError::KeyMalformed { .. }), "got {error:?}");
    }

    /// `--key` compiles to the same one-key policy a `signers` entry does, with
    /// no builder pin — the flag names a signer, not a build.
    #[test]
    fn the_key_flag_override_compiles_to_a_single_key_policy() {
        let directory = tempfile::tempdir().expect("tempdir");
        let key_path = directory.path().join("cosign.pub");
        std::fs::write(&key_path, GOLDEN_PUBLIC_KEY_PEM).expect("write the key");

        let reference = crate::oci::sign::KeyRef::parse(&key_path.display().to_string()).expect("a bare path parses");
        let compiled = compile_key_signer(&reference).expect("the flag override compiles");
        assert!(compiled.builder.is_none(), "the flag names a signer, not a build");
        assert!(matches!(compiled.backends.as_slice(), [PolicyBackend::Key(_)]));
    }

    /// C-031, verify half: `env://VAR` compiles to a verification key from the
    /// variable's own bytes, with no file anywhere.
    ///
    /// The same reference grammar the `--key` flag parses, so a policy and a
    /// flag naming one variable resolve to one key.
    #[test]
    fn an_env_key_reference_compiles_from_the_variable() {
        let env = crate::test::env::lock();
        env.set("OCX_TEST_POLICY_KEY", GOLDEN_PUBLIC_KEY_PEM);

        let reference = crate::oci::sign::KeyRef::parse("env://OCX_TEST_POLICY_KEY").expect("env:// parses");
        compile_key_reference(&reference, "ghcr.io/acme/*").expect("the variable holds an SPKI PEM");
    }

    /// C-033, verify half: unset and over-cap answer with the **same two exit
    /// classes the signing half answers**, so one bad `env://` gets one verdict
    /// whichever verb reads it.
    ///
    /// `KeyFault::Path` is 74 (`io_error`) and `KeyFault::FileBytes` is 65
    /// (`data_error`) — the codes a missing and an over-cap key *file* already
    /// produce. Both messages name the variable, which is all an operator has
    /// to go on when there is no path in the refusal.
    #[test]
    fn an_env_key_reference_refuses_unset_and_oversized_values_by_name() {
        let env = crate::test::env::lock();
        let reference = crate::oci::sign::KeyRef::parse("env://OCX_TEST_POLICY_KEY").expect("env:// parses");

        env.remove("OCX_TEST_POLICY_KEY");
        let unset = compile_key_reference(&reference, "ghcr.io/acme/*").expect_err("an unset variable holds no key");
        let TrustPolicyError::KeyMalformed { reason, fault, .. } = &unset else {
            panic!("got {unset:?}");
        };
        assert_eq!(*fault, KeyFault::Path, "nothing to read is the I/O class: {reason}");
        assert!(
            reason.contains("OCX_TEST_POLICY_KEY"),
            "the refusal must name it: {reason}"
        );

        let cap = usize::try_from(MAX_KEY_PEM_BYTES).expect("the cap fits a usize");
        env.set("OCX_TEST_POLICY_KEY", "k".repeat(cap + 1));
        let oversized = compile_key_reference(&reference, "ghcr.io/acme/*").expect_err("over the cap");
        let TrustPolicyError::KeyMalformed { reason, fault, .. } = &oversized else {
            panic!("got {oversized:?}");
        };
        assert_eq!(
            *fault,
            KeyFault::FileBytes,
            "an over-cap value is a data fault: {reason}"
        );
        assert!(
            reason.contains("OCX_TEST_POLICY_KEY"),
            "the refusal must name it: {reason}"
        );
    }

    /// **A `KeyRef` can never name an unimplemented backend**, and that is what
    /// makes the two `--key` and `signers` paths agree: `KeyRef::parse` refuses
    /// a recognised-but-unimplemented scheme up front, so
    /// [`compile_key_signer`], which takes an already-parsed reference, cannot
    /// be reached with one. The config path meets the same refusal because it
    /// calls the same parser — see
    /// `an_unimplemented_key_backend_is_refused_by_name_not_as_a_missing_file`.
    ///
    /// This is why `compile_key_reference`'s trailing `UnsupportedBackend`
    /// answer is unreachable rather than merely unlikely: every `KeyRef` that
    /// exists resolves through one of the accessors above it. It stays as the
    /// fail-closed answer, never a fall-through.
    ///
    /// C-031 widened "one accessor" from `as_path` alone to `as_path` **xor**
    /// `as_env_var`. The exclusivity is the load-bearing half: two accessors
    /// answering for one reference would make the branch order decide what a
    /// `--key` value means.
    #[test]
    fn a_key_reference_naming_an_unimplemented_backend_never_becomes_a_key_ref() {
        let error = crate::oci::sign::KeyRef::parse("gcpkms://projects/p/locations/l/keyRings/r/cryptoKeys/k")
            .expect_err("a recognised scheme with no backend is refused at parse");
        assert!(
            matches!(error, crate::oci::sign::KeyRefError::UnsupportedBackend { .. }),
            "got {error:?}"
        );
        assert!(
            error.to_string().contains("gcpkms"),
            "the refusal names the scheme: {error}"
        );

        for scheme in crate::oci::sign::Scheme::SPELLINGS {
            let reference = format!("{scheme}://whatever");
            if let Ok(parsed) = crate::oci::sign::KeyRef::parse(&reference) {
                let answered = usize::from(parsed.as_path().is_some()) + usize::from(parsed.as_env_var().is_some());
                assert_eq!(
                    answered, 1,
                    "`{reference}` parsed, so exactly one accessor must answer for it — the unreachable arm in \
                     `compile_key_reference` depends on exactly this"
                );
            }
        }
    }

    // ── Relative key anchoring ───────────────────────────────────────────────

    /// The whole point of the relative form: `/etc/ocx/config.toml` naming
    /// `acme.pub` means the file beside it, not a path relative to whatever
    /// directory the process happens to be in. Ordinary resolution semantics —
    /// **not** a containment check.
    #[test]
    fn anchor_relative_keys_resolves_against_the_declaring_config_dir() {
        let mut policy = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some("acme.pub".to_string()),
                key_pem: None,
            },
        );
        policy.anchor_relative_keys(Path::new("/etc/ocx"));
        let SignerSpec::Key(matcher) = &policy.signers[0] else {
            panic!("still a key signer");
        };
        // Compared as paths, not as bytes: `join` writes the platform separator
        // between two segments spelled with `/`, so on Windows the answer is
        // `/etc/ocx\acme.pub`. Which file the reference names is the invariant;
        // which separator spells it is not.
        let anchored = Path::new("/etc/ocx").join("acme.pub");
        assert_eq!(matcher.key.as_deref().map(Path::new), Some(anchored.as_path()));
    }

    /// The removed `file:` spelling does not parse, so anchoring must leave it
    /// exactly as written and let [`TrustPolicy::compile`] name it. Rewriting it
    /// would hand the operator a diagnostic quoting a directory-joined string
    /// they never typed, in place of the one that carries the fix.
    #[test]
    fn anchor_relative_keys_leaves_the_removed_file_colon_spelling_alone() {
        let mut policy = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some("file:acme.pub".to_string()),
                key_pem: None,
            },
        );
        policy.anchor_relative_keys(Path::new("/etc/ocx"));
        let SignerSpec::Key(matcher) = &policy.signers[0] else {
            panic!("still a key signer");
        };
        assert_eq!(matcher.key.as_deref(), Some("file:acme.pub"));
        let error = policy.compile().expect_err("the removed spelling cannot compile");
        assert!(
            error.to_string().contains("acme.pub") || format!("{error:?}").contains("FileColonPrefix"),
            "compile must be the one that names it; got {error:?}"
        );
    }

    /// An absolute path already names one file; rewriting it would break it.
    #[test]
    fn anchor_relative_keys_leaves_an_absolute_path_alone() {
        let absolute = Path::new("/srv/keys/acme.pub").display().to_string();
        let mut policy = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some(absolute.clone()),
                key_pem: None,
            },
        );
        policy.anchor_relative_keys(Path::new("/etc/ocx"));
        let SignerSpec::Key(matcher) = &policy.signers[0] else {
            panic!("still a key signer");
        };
        assert_eq!(matcher.key.as_deref(), Some(absolute.as_str()));
    }

    /// A KMS reference is not a path and must survive anchoring untouched —
    /// otherwise the refusal that names its scheme would be reporting a
    /// directory-joined string the operator never wrote.
    #[test]
    fn anchor_relative_keys_leaves_a_non_file_scheme_untouched() {
        let mut policy = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: Some("awskms://alias/acme-release".to_string()),
                key_pem: None,
            },
        );
        policy.anchor_relative_keys(Path::new("/etc/ocx"));
        let SignerSpec::Key(matcher) = &policy.signers[0] else {
            panic!("still a key signer");
        };
        assert_eq!(matcher.key.as_deref(), Some("awskms://alias/acme-release"));
    }

    /// The inline form carries its own material, so there is no path to anchor
    /// — and anchoring must not mistake PEM text for one. This is the shape a
    /// managed payload arrives in, which is the shape that must survive a
    /// mechanism written for the *other* spelling.
    #[test]
    fn anchor_relative_keys_leaves_an_inline_key_alone() {
        let mut policy = key_policy(
            "ghcr.io/acme/*",
            KeyMatcher {
                key: None,
                key_pem: Some(SPKI_PEM.to_string()),
            },
        );
        let before = policy.signers.clone();
        policy.anchor_relative_keys(Path::new("/etc/ocx"));
        assert_eq!(policy.signers, before, "an inline key names no path to anchor");
    }

    /// Anchoring must not invent a key for a keyless signer.
    #[test]
    fn anchor_relative_keys_leaves_a_keyless_signer_alone() {
        let mut keyless = policy("ghcr.io/acme/*", Some("release@acme.example"), None, "iss");
        let before = keyless.signers.clone();
        keyless.anchor_relative_keys(Path::new("/etc/ocx"));
        assert_eq!(keyless.signers, before);
    }

    // ── [trust.sigstore] rekor_upload ────────────────────────────────────────

    /// The fleet-wide key-mode default parses, and absent stays absent — the
    /// resolver distinguishes "unset" from "false", so `Option` must survive.
    #[test]
    fn rekor_upload_parses_and_absent_stays_absent() {
        #[derive(Deserialize)]
        struct Root {
            trust: TrustConfig,
        }
        let configured: Root = toml::from_str("[trust.sigstore]\nrekor_upload = true\n").expect("the field parses");
        assert_eq!(
            configured.trust.sigstore.expect("sigstore table").rekor_upload,
            Some(true)
        );

        let absent: Root = toml::from_str("[trust.sigstore]\nrekor_url = \"https://rekor.example\"\n")
            .expect("the table parses without the field");
        assert_eq!(absent.trust.sigstore.expect("sigstore table").rekor_upload, None);
    }

    /// It merges like its siblings: a higher tier that sets it wins, and one
    /// that does not leaves the lower tier's answer in place.
    #[test]
    fn rekor_upload_merges_like_its_siblings() {
        let mut lower = SigstoreTrust {
            rekor_upload: Some(true),
            ..SigstoreTrust::default()
        };
        lower.merge(SigstoreTrust {
            rekor_url: Some("https://rekor.example".to_string()),
            ..SigstoreTrust::default()
        });
        assert_eq!(lower.rekor_upload, Some(true), "an unset higher tier overrides nothing");

        lower.merge(SigstoreTrust {
            rekor_upload: Some(false),
            ..SigstoreTrust::default()
        });
        assert_eq!(lower.rekor_upload, Some(false), "a set higher tier wins");
    }
}
