// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `[shell]` configuration section: enablement toggles plus the activation
//! consent whitelist.
//!
//! **`[shell]` is never read from `ocx.toml`** (C-033): the project tier is
//! stripped explicitly by [`ConfigLoader::fold_project_tier`], and a `[shell]`
//! block in a project file is a parse error. The whitelist can only come from a
//! `config.toml` tier, which is what makes C-028's consent-before-parse
//! ordering structurally safe here in a way it was not for mise.
//!
//! [`ConfigLoader::fold_project_tier`]: crate::config::loader::ConfigLoader::fold_project_tier

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer};

use crate::config::ConfigTier;
use crate::log;
use crate::trust::ScopeSpec;

/// `OCX_CONSENT_PATHS` — an OS-PATH-separated list of directories that activate
/// unconditionally, unioned with `[shell.consent] paths`.
pub const OCX_CONSENT_PATHS: &str = "OCX_CONSENT_PATHS";

/// `OCX_CONSENT_NAMESPACES` — a comma-separated list of source namespaces,
/// unioned with `[shell.consent] namespaces`' `include` set.
///
/// Comma rather than the OS PATH separator: a registry may carry a port
/// (`localhost:5000/acme/*`), so `:` is unusable on Unix.
pub const OCX_CONSENT_NAMESPACES: &str = "OCX_CONSENT_NAMESPACES";

/// The `[shell]` section (C-029).
///
/// **No `deny_unknown_fields`**, like every other `Config` sub-struct: one
/// `config.toml` is read by many ocx versions at once, so a file written for a
/// newer ocx must degrade to "the parts I understand". The tolerance stops one
/// level down, at [`ShellConsent`], and only there.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ShellConfig {
    /// Rung 4 of the hook ladder (C-038). Merges unconditionally in **both**
    /// directions, including the managed tier over a user's own file (C-034) —
    /// safe only because consent (C-025) still gates every project
    /// independently. If that coupling ever weakens, this key stops being safe
    /// to merge unconditionally.
    pub hook: Option<bool>,

    /// Rung 4 of the completions ladder (C-039) — the rung completions never
    /// got. Follows `hook` a fortiori: it grants nothing and gates nothing.
    pub completions: Option<bool>,

    /// The activation whitelist (C-025 clauses 2 and 3).
    pub consent: Option<ShellConsent>,

    /// The tier that set [`Self::hook`], or `None` when no tier did (C-032).
    ///
    /// Runtime provenance, never read from disk — stamped per file by the
    /// config loader and carried through [`Self::merge`] alongside the value it
    /// describes. `ocx shell state` (C-050) reports the tier that **actually**
    /// decided the rung; it never asserts "managed" (A-32).
    #[serde(skip)]
    #[schemars(skip)]
    pub hook_tier: Option<ConfigTier>,

    /// The tier that set [`Self::completions`] — [`Self::hook_tier`]'s twin.
    #[serde(skip)]
    #[schemars(skip)]
    pub completions_tier: Option<ConfigTier>,

    /// Why a managed payload's `[shell.consent]` was dropped, when one was
    /// (C-034).
    ///
    /// Runtime provenance, never read from disk. `log::warn!` goes to a stderr
    /// the shims discard, so the reason additionally rides here: `ocx about`
    /// surfaces it, and the reconciler emits it through
    /// `Shell::emit_message` (A-21).
    #[serde(skip)]
    #[schemars(skip)]
    pub consent_strip_reason: Option<String>,
}

impl ShellConfig {
    /// Merge `other` (higher precedence) into `self` (C-032).
    ///
    /// Per-field semantics, which are the contract:
    ///
    /// | Field | Rule |
    /// |---|---|
    /// | `hook` | scalar — higher tier wins if `Some`, both directions |
    /// | `completions` | scalar, identical to `hook` |
    /// | `consent.paths` | **appends** |
    /// | `consent.namespaces` | **accumulates into one spec** — `include` ∪ `include`, `exclude` ∪ `exclude`; no tier overrides another |
    ///
    /// Two-level rule, no tier ordering: across tiers the per-tier specs
    /// accumulate, and within the accumulated spec a source is consented iff it
    /// matches at least one `include` **and no `exclude`** — carve-outs beat
    /// coverage regardless of which tier contributed either. The only thing a
    /// lower tier can do to a higher tier's grant is **remove** it.
    pub fn merge(&mut self, other: ShellConfig) {
        // The provenance travels with the value, never separately: a tier that
        // did not set the scalar must not claim to have decided it.
        if other.hook.is_some() {
            self.hook = other.hook;
            self.hook_tier = other.hook_tier;
        }
        if other.completions.is_some() {
            self.completions = other.completions;
            self.completions_tier = other.completions_tier;
        }
        if let Some(other_consent) = other.consent {
            self.consent
                .get_or_insert_with(ShellConsent::default)
                .merge(other_consent);
        }
        if other.consent_strip_reason.is_some() {
            self.consent_strip_reason = other.consent_strip_reason;
        }
    }
}

/// The `[shell.consent]` table (C-029).
///
/// **This is the one place the fleet forward-compat tolerance stops.** On a
/// consent-bearing table, dropping an unknown *narrowing* key would **widen**
/// trust rather than narrow it — the one direction forward-compat must not
/// take. An operator publishing `namespaces` plus a future narrowing key an
/// older fleet host does not know must have that host **refuse** the payload,
/// not silently drop the key and activate on the full namespace.
///
/// Recorded as the plan's single constitution deviation: `arch-principles.md`
/// forbids `deny_unknown_fields` on anything reachable from `Config`, and
/// carries a consent-bearing-table carve-out for exactly this table.
/// [`ShellConfig::hook`] / `completions` keep the tolerant behaviour; only this
/// table is strict.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellConsent {
    /// Canonical directories that activate unconditionally (C-025 clause 3).
    /// Modelled on git's `safe.directory`: an entry is one exact directory,
    /// or — with a trailing `/*` — that directory and everything beneath it,
    /// matched component-wise ([`consent_path_matches`]). git's `/*` covers
    /// only what is nested *under* the named directory; this one covers the
    /// named directory too. A bare `*` grants nothing: OCX has no
    /// trust-this-machine token.
    ///
    /// **Only the project side is canonicalized; entries are compared
    /// literally** after separator and trailing-slash normalization
    /// ([`normalize_consent_path`], C-030). Canonicalizing entries at read time
    /// would make the grant follow a symlink an attacker may control on the
    /// parent; comparing literally never matches a symlinked checkout and is
    /// then silently inert — the fail-safe direction. A-28 adds a **near-miss**
    /// row to `ocx shell state` when an entry differs from the canonical
    /// directory only by ASCII case.
    ///
    /// A **leading** `~` expands against the home directory at match time, the
    /// way git interpolates one in `safe.directory` — textually, resolving no
    /// symlink, and never rewritten into the stored entry, so `ocx shell state`
    /// still prints what the user wrote. `~user` is not supported.
    /// [`consent_entry_defect`] names every spelling that can never match.
    ///
    /// A-26 — a `paths` grant is deliberately drift-blind and writes no stamp,
    /// so revoking it is immediately effective.
    #[serde(default)]
    pub paths: Vec<PathBuf>,

    /// Source namespaces that activate a project whose whole lock is inside
    /// them (C-025 clause 2). **One [`ConsentScopeSpec`], never a `Vec`** — a
    /// flat list can only ever widen, so "everything under `ocx.sh/acme/*`
    /// except the one compromised namespace" would be unspellable.
    pub namespaces: Option<ConsentScopeSpec>,
}

impl ShellConsent {
    /// Accumulate `other` into `self` (C-032) — `paths` append, `namespaces`
    /// union into one spec.
    ///
    /// Neither direction overrides: a tier can only ever **add** an `include`
    /// or an `exclude`, and `exclude` beats `include` wherever both match.
    pub fn merge(&mut self, other: ShellConsent) {
        for path in other.paths {
            if !self.paths.contains(&path) {
                self.paths.push(path);
            }
        }
        let Some(other_namespaces) = other.namespaces else {
            return;
        };
        match self.namespaces.as_mut() {
            Some(namespaces) => namespaces.accumulate(other_namespaces),
            None => self.namespaces = Some(other_namespaces),
        }
    }
}

/// A consent-scoped [`ScopeSpec`]: same matching semantics, strict parsing.
///
/// C-029/A-27 — `ScopeSpec`'s own hand-written deserializer **drops** unknown
/// keys inside the table, commented as deliberate fleet forward-compat for
/// `[[trust.policy]]`. Reusing it verbatim here would silently drop a future
/// *narrowing* key on an older host, which **widens** consent — the exact
/// outcome C-029 forbids. This wrapper reuses `ScopeSpec`'s matching semantics,
/// its string/object grammar, `specificity_for` and its hand-written
/// `JsonSchema`, and adds only the strict-refusal deserializer.
/// **`[[trust.policy]]`'s own `ScopeSpec` is unchanged** — its tolerance is
/// deliberate there.
///
/// Every pattern is stored **normalized**: a trailing `/*` is stripped at parse
/// (C-030 rule 2), so matching takes `pattern_matches`' segment-bounded
/// no-wildcard branch and `ocx.sh/acme/*` matches exactly the set
/// `ocx.sh/acme` does.
///
/// Sharing the type couples no policy: a namespace consented for shell
/// activation is not thereby trusted for signature verification, and a
/// namespace named in `[[trust.policy]]` grants no activation consent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentScopeSpec(pub ScopeSpec);

impl ConsentScopeSpec {
    /// Whether `source` — a canonical two-component `registry/org` source
    /// (C-026) — is consented by this spec.
    #[must_use]
    pub fn matches(&self, source: &str) -> bool {
        self.0.matches(source)
    }

    /// The `include` patterns, in declaration order.
    ///
    /// Never empty: the string form contributes one, and the table form rejects
    /// an empty `include` at parse rather than reading it as a catch-all.
    #[must_use]
    pub fn include(&self) -> &[String] {
        match &self.0 {
            ScopeSpec::Prefix(pattern) => std::slice::from_ref(pattern),
            ScopeSpec::Set { include, .. } => include,
        }
    }

    /// The `exclude` patterns, in declaration order. Empty when none was
    /// declared.
    #[must_use]
    pub fn exclude(&self) -> &[String] {
        match &self.0 {
            ScopeSpec::Prefix(_) => &[],
            ScopeSpec::Set { exclude, .. } => exclude,
        }
    }

    /// Union `other` into `self` (C-032): `include` ∪ `include`,
    /// `exclude` ∪ `exclude`, duplicates dropped, declaration order kept.
    pub fn accumulate(&mut self, other: ConsentScopeSpec) {
        let mut include = self.include().to_vec();
        let mut exclude = self.exclude().to_vec();
        extend_unique(&mut include, other.include());
        extend_unique(&mut exclude, other.exclude());
        self.0 = ScopeSpec::Set { include, exclude };
    }
}

/// Append every element of `additions` not already in `target`.
fn extend_unique(target: &mut Vec<String>, additions: &[String]) {
    for addition in additions {
        if !target.iter().any(|existing| existing == addition) {
            target.push(addition.clone());
        }
    }
}

impl<'de> Deserialize<'de> for ConsentScopeSpec {
    /// The consent-scoped visitor (A-27): mirrors `ScopeSpec`'s hand-written
    /// one, replaces its unknown-key arm with an **error**, keeps the
    /// neither-key floor, rejects an **empty `include`** rather than reading it
    /// as a catch-all, and runs [`validate_consent_pattern`] on **every**
    /// pattern before constructing `ScopeSpec::Set`.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ConsentScopeSpecVisitor;

        impl<'de> serde::de::Visitor<'de> for ConsentScopeSpecVisitor {
            type Value = ConsentScopeSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(
                    "a consent namespace pattern string, or a table with an `include` list of them and an optional \
                     `exclude` list",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let pattern = normalize_consent_pattern(value).map_err(E::custom)?;
                Ok(ConsentScopeSpec(ScopeSpec::Prefix(pattern)))
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                use serde::de::Error as _;

                let mut include: Option<Vec<String>> = None;
                let mut exclude: Option<Vec<String>> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "include" => include = Some(map.next_value()?),
                        "exclude" => exclude = Some(map.next_value()?),
                        // The deviation, in one arm: `ScopeSpec` drops an
                        // unknown key here for fleet forward-compat. On a
                        // consent table a dropped NARROWING key widens trust,
                        // so this refuses instead.
                        other => {
                            return Err(M::Error::custom(format!(
                                "unknown key '{other}' in a [shell.consent] namespaces table; a key this ocx does not \
                                 understand could only narrow consent, so the table is refused rather than read \
                                 without it"
                            )));
                        }
                    }
                }
                if include.is_none() && exclude.is_none() {
                    return Err(M::Error::custom(
                        "a [shell.consent] namespaces table needs an `include` list; there is no catch-all spelling",
                    ));
                }
                let include = normalize_all(include.unwrap_or_default()).map_err(M::Error::custom)?;
                if include.is_empty() {
                    return Err(M::Error::custom(
                        "an empty `include` grants nothing and is never a catch-all; list the namespaces to consent to",
                    ));
                }
                let exclude = normalize_all(exclude.unwrap_or_default()).map_err(M::Error::custom)?;
                Ok(ConsentScopeSpec(ScopeSpec::Set { include, exclude }))
            }
        }

        deserializer.deserialize_any(ConsentScopeSpecVisitor)
    }
}

/// Validate and normalize every pattern in `patterns`, failing on the first
/// rejection.
fn normalize_all(patterns: Vec<String>) -> Result<Vec<String>, ConsentPatternError> {
    patterns
        .iter()
        .map(|pattern| normalize_consent_pattern(pattern))
        .collect()
}

impl schemars::JsonSchema for ConsentScopeSpec {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ConsentScopeSpec")
    }

    /// Delegated to [`ScopeSpec`]'s hand-written schema: C-030 states the
    /// string form, the object form and the schema all come free from
    /// `ScopeSpec`, and only the deserializer is strict.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <ScopeSpec as schemars::JsonSchema>::json_schema(generator)
    }
}

/// Why a `[shell.consent] namespaces` pattern was refused (C-030, A-27).
///
/// Reaches the user two ways. Both fail **closed** — no consent — and neither
/// takes anything else down with it: inside a `config.toml` tier the config
/// loader's consent-refusal strip drops the whole `[shell.consent]` table and
/// records the reason on [`ShellConfig::consent_strip_reason`], while
/// `[registries]`, `[mirrors]` and `[[trust.policy]]` in that same file still
/// apply; on `OCX_CONSENT_NAMESPACES` the whole contribution is discarded with
/// one warning and the config tiers stand alone.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsentPatternError {
    /// The empty string, which `trust::pattern_matches` reads as a catch-all.
    #[error("a consent namespace pattern must not be empty")]
    Empty,

    /// An ASCII uppercase byte anywhere. An uppercase repository is refused
    /// outright by `Identifier` parsing, so such a pattern is unmatchable.
    #[error(
        "consent namespace '{0}' contains an uppercase byte; no source can ever be uppercase, so it matches nothing"
    )]
    Uppercase(String),

    /// `@` anywhere — a consent pattern names a source, never a pinned
    /// reference.
    #[error("consent namespace '{0}' contains '@'; a consent pattern names a source, never a digest-pinned reference")]
    DigestSeparator(String),

    /// A `*` anywhere other than as the final two bytes `/*`, or more than one.
    #[error("consent namespace '{0}' may use '*' only once, as a trailing '/*'")]
    Wildcard(String),

    /// An empty `/`-delimited component — a leading `/`, a `//`, or a trailing
    /// `/` with no `*`.
    #[error("consent namespace '{0}' has an empty path component")]
    EmptyComponent(String),

    /// Three or more components after stripping an optional trailing `/*`. That
    /// names a repository; a source is exactly two components (C-026).
    #[error("consent namespace '{0}' names a repository, not a source; write '<host>/<org>'")]
    TooManyComponents(String),

    /// A whole-registry grant, in either spelling.
    ///
    /// The bound a `namespaces` grant enforces is **repository-corroborated
    /// content**: to put code in front of `PATH` an attacker must get a
    /// registry to serve that digest under a repository inside a granted
    /// organisation. Clause 2 quantifies over [`verified_sources`] — the
    /// store's own `refs/origins/` record of the logical repository this host
    /// resolved and fetched — and never over the lock's claim, because
    /// `PackageStore` keys a package on
    /// registry + digest and leaves the repository out of the path, so a
    /// claim-based clause would let an attacker-authored lock pair a granted
    /// org's name with any digest the victim already holds from that host,
    /// whoever published it.
    ///
    /// What a whole-registry pattern costs is the organisation half of that
    /// bound: every organisation on the host is granted, so the one a registry
    /// served the content under carries no information — an org the attacker
    /// registered minutes ago satisfies the whitelist exactly as the operator's
    /// own does. On a host anyone can register on there is nothing left, and
    /// ocx cannot tell an open registry from a closed one. So the spelling is
    /// refused rather than documented as dangerous, and an operator who
    /// genuinely trusts a whole private registry lists its organisations or
    /// uses a `paths` grant.
    ///
    /// [`verified_sources`]: crate::project::consent::verified_sources
    #[error(
        "consent namespace '{0}' grants a whole registry; name an organisation ('<host>/<org>') — \
         a namespaces grant is bounded by a registry having served that digest under a repository inside the \
         granted organisation, and naming the whole host drops the organisation half of that bound on any host \
         anyone can publish to"
    )]
    WholeRegistry(String),

    /// The organisation component is not a legal repository path.
    #[error("consent namespace '{pattern}' has an invalid organisation component")]
    Organisation {
        pattern: String,
        #[source]
        source: crate::oci::IdentifierError,
    },
}

/// The `[shell.consent] namespaces` pattern grammar, enforced at parse (C-030).
///
/// A-27 — **one validator, two channels**: the `config.toml` tiers and
/// `OCX_CONSENT_NAMESPACES` (C-031) share it, and only the *consequence* of a
/// rejection differs per channel.
///
/// Accepts exactly two spellings: `<host>[:<port>]/<org>` and
/// `<host>[:<port>]/<org>/*` — equivalent at source granularity, since a source
/// is exactly two components (C-026).
///
/// **There is no whole-registry spelling.** `<host>/*` and a bare `<host>` are
/// both refused ([`ConsentPatternError::WholeRegistry`]): a `namespaces` grant
/// is bounded by a registry having served the content under a repository inside
/// the granted organisation, and a whole-registry pattern drops the
/// organisation half of that bound on any host where anyone can register.
///
/// Rejects eight classes: the empty string and a bare `*`; any `*` other than
/// as the final two bytes `/*`, and any pattern with more than one `*`; a
/// trailing `/` with no `*`, and the pattern `/*`; any empty `/`-delimited
/// component; **any ASCII uppercase byte anywhere**; three or more components
/// after stripping an optional trailing `/*`; **exactly one component after that
/// strip — a bare `<host>` or a `<host>/*`, the two whole-registry spellings**;
/// and `@` anywhere or `:` after the first `/`.
///
/// # Errors
///
/// [`ConsentPatternError`], naming the offending pattern and its class. A
/// pattern left with a single `/`-delimited component once an optional trailing
/// `/*` is stripped — `ocx.sh` or `ocx.sh/*` — is
/// [`ConsentPatternError::WholeRegistry`]; every other class maps to the variant
/// named for it.
pub fn validate_consent_pattern(pattern: &str) -> Result<(), ConsentPatternError> {
    if pattern.is_empty() {
        return Err(ConsentPatternError::Empty);
    }
    if pattern.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(ConsentPatternError::Uppercase(pattern.to_string()));
    }
    if pattern.contains('@') {
        return Err(ConsentPatternError::DigestSeparator(pattern.to_string()));
    }
    // `pattern_matches` is segment-bounded only on its no-wildcard branch, and
    // its own doc comment calls a bare `ghcr.io/acme*` an intentional substring
    // glob — so `ocx.sh/acme-corp*`, one keystroke from the intended spelling,
    // would match `ocx.sh/acme-corp-evil/tool`.
    let trailing_wildcard = pattern.ends_with("/*");
    if pattern.matches('*').count() > 1 || (pattern.contains('*') && !trailing_wildcard) {
        return Err(ConsentPatternError::Wildcard(pattern.to_string()));
    }
    let body = if trailing_wildcard {
        &pattern[..pattern.len() - 2]
    } else {
        pattern
    };
    let components: Vec<&str> = body.split('/').collect();
    if components.iter().any(|component| component.is_empty()) {
        return Err(ConsentPatternError::EmptyComponent(pattern.to_string()));
    }
    match components.as_slice() {
        // Both whole-registry spellings, refused together: `<host>/*` says it
        // outright and a bare `<host>` would mean the same thing at source
        // granularity. Refusing only one would leave the other as the way to
        // spell it (ocx-sh/ocx#344).
        [_host] => Err(ConsentPatternError::WholeRegistry(pattern.to_string())),
        // The org half is validated through the shipped repository validator
        // rather than a second charset — that is also what rejects a `:` after
        // the first `/`, since `:` is not a legal repository byte.
        [_host, org] => {
            crate::oci::Identifier::validate_repository(org).map_err(|source| ConsentPatternError::Organisation {
                pattern: pattern.to_string(),
                source,
            })
        }
        _ => Err(ConsentPatternError::TooManyComponents(pattern.to_string())),
    }
}

/// Validate `pattern`, then return it with a trailing `/*` stripped (C-030
/// rule 2).
///
/// Stripping is what makes matching correct: the stored pattern then takes
/// `pattern_matches`' segment-bounded no-wildcard branch, so `ocx.sh/acme/*`
/// and `ocx.sh/acme` match the identical set — `ocx.sh/acme` and everything
/// under it, never `ocx.sh/acme-evil`.
///
/// # Errors
///
/// [`ConsentPatternError`], from [`validate_consent_pattern`].
pub fn normalize_consent_pattern(pattern: &str) -> Result<String, ConsentPatternError> {
    validate_consent_pattern(pattern)?;
    Ok(pattern.strip_suffix("/*").unwrap_or(pattern).to_string())
}

/// Render `path` for the literal `[shell.consent] paths` comparison (C-030,
/// A-28).
///
/// Separator, trailing-slash, and — **on Windows only** — ASCII-case
/// normalization. No canonicalization of the entry ever: that would make the
/// grant follow a symlink an attacker may control on the parent.
///
/// **The case rule follows the platform, because so does the filesystem.** On
/// Unix `/a/B` and `/a/b` are two directories, so folding them onto one grant
/// widens it onto a directory an attacker can create; a case-only mismatch is
/// inert there and `ocx shell state` reports it as a near-miss instead (A-28).
/// On Windows they are one directory — the filesystem itself refuses to hold
/// both — so folding merges nothing that was ever apart, and *not* folding
/// leaves an operator who wrote `C:\W\Acme` staring at an inert shell for a
/// directory Windows considers identical. `std` already folds the drive letter
/// for exactly this reason; this extends the same rule to the components after
/// it. The fold is ASCII-only, matching the drive letter's, rather than
/// Unicode case folding: a full fold is locale- and version-dependent, and a
/// trust boundary that shifts with an ICU table is not one.
///
/// The normalizer is [`Path::components`], and both halves of that choice are
/// the fix for a widening this function used to ship:
///
/// - **It never renders through a lossy `String`.** `to_string_lossy` maps every
///   non-UTF-8 byte to `U+FFFD`, so `/w/a\xFE` and `/w/a\xFF` collapsed onto one
///   key and a grant for either matched both (CWE-41).
/// - **It never rewrites `\` by hand.** `std::path` already treats `\` and `/`
///   as separators on Windows — where the two spellings genuinely name one
///   directory — and treats `\` as an ordinary filename byte everywhere else,
///   where a hand-rolled rewrite merged the two *different* Unix directories
///   `a\b` and `a/b` into one grant. `git` will happily check out a directory
///   literally named `services\api`, which is how that became reachable.
///
/// Returns an [`OsString`] rather than a `String` for the same reason: the
/// comparison operand must be the path's own bytes. Callers wanting an
/// advisory, human-facing form (`ocx shell state`'s A-28 case near-miss) may
/// render it lossily *there* — widening a diagnostic note is harmless, widening
/// a grant is not.
#[must_use]
pub fn normalize_consent_path(path: &Path) -> OsString {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            // Windows-only: `Prefix` already arrives with its drive letter
            // folded by `std`; `Normal` is what is left to fold. Pushing
            // `as_os_str()` for every other component is exactly what
            // `FromIterator<Component> for PathBuf` does, so the separator and
            // trailing-slash behaviour is unchanged — including `RootDir`,
            // which `push` resolves against a preceding `Prefix` rather than
            // replacing it.
            #[cfg(windows)]
            Component::Normal(name) => normalized.push(name.to_ascii_lowercase()),
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized.into_os_string()
}

/// Whether the `paths` entry `entry` grants the canonical directory
/// `project_dir` (C-025 clause 3).
///
/// Two spellings, borrowing git `safe.directory`'s:
///
/// - `/w/acme` — that one directory, and nothing else.
/// - `/w/acme/*` — that directory and everything beneath it.
///
/// The second deliberately widens git's by one directory: measured against git
/// 2.54, `safe.directory = /w/acme/*` refuses `/w/acme` itself and allows only
/// what is nested under it. The spelling and the component-bounded matching are
/// git's; granting the named directory as well is ours.
///
/// The subtree form matches **component-wise**, never as a string prefix, so
/// `/w/acme/*` covers `/w/acme/tools` and never the attacker-planted sibling
/// `/w/acme-evil`. That sibling is the reason a bare string prefix was refused
/// in the first place; a component-bounded one has no such reach.
///
/// A trailing `*` component is the **only** wildcard. `?`, `[…]` and every
/// other glob metacharacter is an ordinary filename byte, so `/w/acm?` grants a
/// directory literally named `acm?` and never `/w/acm3`.
///
/// A `*` that leaves no named directory behind — a bare `*`, or `/*` — is
/// **not** a subtree grant and matches nothing. git spells that "trust every
/// repository on this machine"; OCX has no such token, for the same reason
/// `namespaces` has no whole-registry one.
///
/// A **leading** `~` — `~` alone or `~/…` — expands against the home directory
/// before anything is compared, in both channels, the way git interpolates one
/// in `safe.directory`. Two forms never expand and therefore match nothing:
/// `~user`, which is not supported, and a leading `~` on a machine where no
/// home directory resolves. [`consent_entry_defect`] names both, and it is the
/// same [`expanded_entry`] seam, so the diagnostic and the grant can never
/// disagree.
///
/// **The expansion is textual.** It does not canonicalize and does not resolve
/// symlinks, so a `~/dev` whose `dev` is a symlink still fails to match the
/// canonical project directory behind it — the entry side is never
/// canonicalized and `~` is not an exception to that. `ocx shell state` prints
/// the entry as the user wrote it, never the expansion.
///
/// Only the project side is ever canonicalized. The entry is compared as its
/// own bytes after separator and trailing-slash normalization
/// ([`normalize_consent_path`], C-030), so a grant never follows a symlink an
/// attacker may control on the parent — and, conversely, an entry naming a
/// symlinked route to a project is inert rather than matching. Write the entry
/// as the canonical path `ocx shell state` prints.
///
/// **Both arms compare through [`Path`], so both fold exactly what
/// `Path::starts_with` folds and nothing else.** `Path`'s own equality is
/// `components() == components()`, and `PrefixComponent`'s equality is on its
/// *parsed* form, whose drive letter `std` ASCII-uppercases — so `c:\w\acme`
/// and `C:\w\acme` are one entry on Windows, in the exact form as well as in
/// the subtree form. The exact arm used to compare `OsString`s bytewise, which
/// left one entry style meaning two different things depending on which
/// spelling it carried.
///
/// **Ordinary components fold ASCII case on Windows and nowhere else**, which
/// [`normalize_consent_path`] applies to both operands of both arms. Windows
/// cannot hold two directories differing only by case, so the fold merges
/// nothing that was ever distinct; Unix can, so folding there would widen a
/// grant onto a directory an attacker may create, and a case-only mismatch
/// stays inert and surfaces as `ocx shell state`'s A-28 near-miss. That note is
/// therefore Unix-only in practice: on Windows the case it describes is a
/// grant.
///
/// **`project_dir` must already be canonical, and this fails closed when it is
/// not.** The subtree form is what makes that load-bearing rather than
/// cosmetic: the exact form only ever *misses* on a non-canonical directory,
/// but the subtree form is a containment test, and a `..` component **escapes**
/// the subtree it appears to sit in — `/w/acme/../../etc` is inside
/// `/w/acme/*` as far as `Path::starts_with` is concerned. Every caller derives
/// the directory through
/// [`canonical_project_dir`](crate::project::consent::canonical_project_dir), so
/// a `..` here means that premise was broken upstream: such a directory matches
/// **nothing**, in a release build exactly as in a debug one.
#[must_use]
pub fn consent_path_matches(entry: &Path, project_dir: &Path) -> bool {
    // Refuse, rather than assert: a debug-only assertion leaves the release
    // build granting the escape it names, and a trust boundary that means two
    // different things per profile is not a boundary.
    if project_dir
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return false;
    }
    // An entry whose leading `~` cannot be expanded matches nothing — never a
    // panic, and never a partial expansion that would compare a literal `~`.
    let Ok(entry) = expanded_entry(entry) else {
        return false;
    };
    let project = PathBuf::from(normalize_consent_path(project_dir));
    match subtree_prefix(&entry) {
        // `subtree_prefix` collects through `Path::components`, which is only
        // half of the normalization: the Windows ASCII-case fold lives in
        // `normalize_consent_path`. Routing the prefix through it too is what
        // keeps the subtree arm and the exact arm folding the same thing —
        // without it the subtree form would stay case-sensitive on Windows
        // while the exact form folded, which is the asymmetry this whole
        // function exists to not have.
        Some(prefix) => project.starts_with(normalize_consent_path(&prefix)),
        // Mixed operands, one comparison: `PathBuf: PartialEq<OsString>`
        // delegates to `<Path as PartialEq>::eq`, which is
        // `components() == components()` — the identical comparison
        // `starts_with` performs above, and **not** the `OsString` byte compare
        // the types suggest. That is the whole fix: a byte compare folded the
        // drive letter on neither side while the arm above folded it on both.
        None => project == normalize_consent_path(&entry),
    }
}

/// The directory a subtree entry names, or `None` when `entry` is an exact
/// grant.
///
/// `None` for a `*` that names no directory (bare `*`, `/*`, `C:\*`): a grant
/// over the whole filesystem is not expressible.
fn subtree_prefix(entry: &Path) -> Option<PathBuf> {
    let mut components = entry.components();
    if components.next_back()? != Component::Normal(OsStr::new("*")) {
        return None;
    }
    let prefix: PathBuf = components.collect();
    prefix
        .components()
        .any(|component| matches!(component, Component::Normal(_)))
        .then_some(prefix)
}

/// Why a `paths` entry can never match any canonical directory.
///
/// One entry, one defect: the first one found is reported, most specific first,
/// so `~alice/../x` is a `~user` problem rather than a `..` problem. Every
/// variant is derived from the same [`expanded_entry`] and [`subtree_prefix`]
/// the matcher uses, so the two cannot drift apart.
///
/// **The one thing that can defeat the `*` classes** is a directory whose name
/// is literally `*` — `mkdir '*'` is legal on Unix, illegal on Windows — for
/// which `/w/acme*` or `/w/*/tools` would be a legitimate exact entry. The
/// classes read `*` as the wildcard the grammar defines it to be, which is what
/// every author of such an entry meant; the other classes hold without
/// exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryDefect {
    /// A `*` component that is not the entry's last component (`/w/*/tools`).
    StarNotLast,
    /// A `*` inside a component carrying other characters (`/w/acme*`).
    StarInsideComponent,
    /// A `*` leaving no named directory behind (`*`, `/*`, `C:\*`).
    StarNamesNoDirectory,
    /// A `..` component — a canonical directory never carries one.
    ParentDirComponent,
    /// A leading `~` that no home directory could expand.
    UnresolvableHome,
    /// A `~user` form, which is not supported.
    UnsupportedTildeUser,
    /// A relative entry — a canonical directory is always absolute, so no
    /// comparison of either arm can ever succeed against one.
    ///
    /// **Beyond the six variants the plan named**, and reported here because it
    /// is genuinely unmatchable rather than merely suspicious: `Path` equality
    /// and `Path::starts_with` both compare from the first component, and a
    /// canonical directory always leads with a root (and, on Windows, a drive
    /// prefix) that a relative entry has nothing to put beside.
    RelativePath,
}

impl std::fmt::Display for EntryDefect {
    /// One lower-case fragment naming the defect and the spelling the author
    /// most likely meant — this is what `ocx shell state` prints next to the
    /// entry, for someone looking at their own `config.toml`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            EntryDefect::StarNotLast => {
                "'*' is a wildcard only as the entry's last component; write '<directory>/*' to grant a subtree"
            }
            EntryDefect::StarInsideComponent => {
                "'*' is a whole component, never part of one; write '<directory>/*' to grant everything beneath \
                 '<directory>'"
            }
            EntryDefect::StarNamesNoDirectory => {
                "'*' with no directory before it would grant every directory on this machine, which has no \
                 spelling; name the directory to grant"
            }
            EntryDefect::ParentDirComponent => {
                "'..' never appears in a canonical directory; write the path the way 'ocx shell state' prints it"
            }
            EntryDefect::UnresolvableHome => {
                "a leading '~' needs a home directory and none resolved on this machine; write the path out in full"
            }
            EntryDefect::UnsupportedTildeUser => "'~user' is never expanded; write that user's directory out in full",
            EntryDefect::RelativePath => {
                "a relative entry never matches; a canonical project directory is always absolute, so write the \
                 full path"
            }
        })
    }
}

/// `Some(defect)` when `entry` can never match any canonical directory,
/// `None` when it is a well-formed exact or subtree entry.
///
/// The diagnostic half of [`consent_path_matches`], and the reason a silently
/// inert entry is now a reported one: `ocx shell state` renders the returned
/// defect's `Display` beside the entry **as the user wrote it**, expansion and
/// all left out of the rendering.
///
/// Agreement with the matcher is by construction, not by parallel
/// re-derivation: this routes through the same [`expanded_entry`] and the same
/// [`subtree_prefix`], so an entry this refuses is one the matcher's own
/// `let Ok(entry) = …` / `subtree_prefix` arms already refuse. See
/// [`EntryDefect`] for the one literal-`*`-directory reading the `*` classes
/// assume.
#[must_use]
pub fn consent_entry_defect(entry: &Path) -> Option<EntryDefect> {
    let entry = match expanded_entry(entry) {
        Ok(entry) => entry,
        Err(defect) => return Some(defect),
    };
    let components: Vec<Component<'_>> = entry.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            continue;
        };
        if !name.as_encoded_bytes().contains(&b'*') {
            continue;
        }
        if *name != OsStr::new("*") {
            return Some(EntryDefect::StarInsideComponent);
        }
        if index + 1 != components.len() {
            return Some(EntryDefect::StarNotLast);
        }
        // The last component is a bare `*`; whether that is a subtree grant is
        // `subtree_prefix`'s decision and not a second copy of its rule.
        if subtree_prefix(&entry).is_none() {
            return Some(EntryDefect::StarNamesNoDirectory);
        }
    }
    if components.contains(&Component::ParentDir) {
        return Some(EntryDefect::ParentDirComponent);
    }
    if !entry.is_absolute() {
        return Some(EntryDefect::RelativePath);
    }
    None
}

/// `entry` with a leading `~` expanded against this machine's home directory.
///
/// The one seam the grant path and the `ocx shell state` diagnostic share, so
/// neither can expand an entry the other would not. Textual: it joins, and it
/// canonicalizes nothing.
fn expanded_entry(entry: &Path) -> Result<PathBuf, EntryDefect> {
    expand_against(entry, crate::file_structure::home_directory().as_deref())
}

/// [`expanded_entry`] against an explicit home directory.
///
/// `home` is a parameter because the interesting branch is the machine that has
/// no home directory, and a test cannot produce one by any other means without
/// mutating the process environment out from under every concurrent test.
///
/// Only a **leading** `~` component expands: `/w/~/dev` names a directory
/// literally called `~`, which is a legal name, and rewriting it would be the
/// same class of widening as rewriting `\` on Unix.
fn expand_against(entry: &Path, home: Option<&Path>) -> Result<PathBuf, EntryDefect> {
    let mut components = entry.components();
    let Some(Component::Normal(first)) = components.next() else {
        return Ok(entry.to_path_buf());
    };
    if first.as_encoded_bytes().first() != Some(&b'~') {
        return Ok(entry.to_path_buf());
    }
    if first != OsStr::new("~") {
        return Err(EntryDefect::UnsupportedTildeUser);
    }
    let home = home.ok_or(EntryDefect::UnresolvableHome)?;
    Ok(home.join(components.as_path()))
}

/// The `OCX_CONSENT_*` env channel's contribution (C-031).
///
/// Additive: unioned with the config tiers, never a replacement and never
/// higher-precedence. A hostile parent process setting these is out of scope,
/// consistent with every surveyed tool — and with `--config` / `OCX_CONFIG`,
/// the third consent-bearing channel (A-33).
///
/// **Empty tokens are discarded, never converted to a pattern.**
/// `trust::pattern_matches` returns `true` for an empty pattern, so without
/// this rule a trailing comma would contribute one empty pattern consenting to
/// **every** namespace, through a channel a devcontainer image writes by hand.
/// An all-empty value contributes nothing and is **never an error**: an unset
/// var and an empty one are the same situation, and no channel may break a
/// prompt.
///
/// A single malformed non-empty pattern discards the **whole**
/// `OCX_CONSENT_NAMESPACES` contribution with one warning; the config tiers
/// stand alone. Neither channel activates on a partially-parsed spec.
#[must_use]
pub fn env_channel(paths: Option<&str>, namespaces: Option<&str>) -> ShellConsent {
    ShellConsent {
        paths: paths.map(parse_consent_paths).unwrap_or_default(),
        namespaces: namespaces.and_then(parse_consent_namespaces),
    }
}

/// Split `value` on the OS PATH separator, dropping every empty token.
///
/// `std::env::split_paths` owns the split so Windows quoting behaves the way it
/// does for `PATH` itself. Only the empty-token rule is applied on top: an
/// empty entry would become an empty `PathBuf`, which normalizes toward a root
/// rather than toward nothing. The surviving bytes are kept verbatim — trimming
/// a path's own bytes would silently rename a legitimate directory, and a
/// leading `~` is expanded at match time by [`consent_path_matches`] rather
/// than here, so both channels get the interpolation and neither loses the
/// entry the user wrote.
fn parse_consent_paths(value: &str) -> Vec<PathBuf> {
    std::env::split_paths(value)
        .filter(|path| !path.as_os_str().is_empty() && !path.to_string_lossy().trim().is_empty())
        .collect()
}

/// Split `value` on commas, drop empty tokens, and validate what remains.
///
/// Returns `None` — contributing nothing — for an all-empty value and for a
/// value carrying any malformed pattern; the latter warns once.
fn parse_consent_namespaces(value: &str) -> Option<ConsentScopeSpec> {
    let mut include = Vec::new();
    for token in value.split(',').map(str::trim).filter(|token| !token.is_empty()) {
        match normalize_consent_pattern(token) {
            Ok(pattern) => {
                if !include.iter().any(|existing| existing == &pattern) {
                    include.push(pattern);
                }
            }
            Err(source) => {
                log::warn!(
                    "{OCX_CONSENT_NAMESPACES} was ignored in full because one pattern is invalid; the config tiers \
                     still apply ({source})"
                );
                return None;
            }
        }
    }
    if include.is_empty() {
        return None;
    }
    Some(ConsentScopeSpec(ScopeSpec::Set {
        include,
        exclude: Vec::new(),
    }))
}

/// The whitelist activation is actually gated on: the `config.toml` tiers plus
/// the `OCX_CONSENT_*` env channel (C-031).
///
/// `OCX_NO_CONFIG=1` does **not** prune this — it empties the discovered chain
/// and suppresses the managed fold, but touches neither the explicit tiers nor
/// the env channel. Only `OCX_NO_HOOK=1` makes a shell wholly inert (A-33).
#[must_use]
pub fn effective_consent(configured: Option<&ShellConfig>) -> ShellConsent {
    let mut consent = configured.and_then(|shell| shell.consent.clone()).unwrap_or_default();
    consent.merge(env_channel(
        crate::env::var(OCX_CONSENT_PATHS).as_deref(),
        crate::env::var(OCX_CONSENT_NAMESPACES).as_deref(),
    ));
    consent
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> Result<ShellConfig, toml::de::Error> {
        toml::from_str(toml)
    }

    /// A POSIX-spelled fixture, as this platform actually spells an absolute
    /// path.
    ///
    /// A canonical project directory comes out of `dunce::canonicalize`, so on
    /// Windows it always carries a drive prefix — which is exactly why a
    /// driveless `/w/acme` is `EntryDefect::RelativePath` there. Fixtures
    /// standing in for a canonical directory carry the prefix; the ones
    /// asserting the defect deliberately do not. Case is preserved: the A-28
    /// rows turn on it.
    fn abs(posix: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{}", posix.replace('/', "\\")))
        } else {
            PathBuf::from(posix)
        }
    }

    fn include_of(consent: &ShellConsent) -> Vec<String> {
        consent
            .namespaces
            .as_ref()
            .map(|spec| spec.include().to_vec())
            .unwrap_or_default()
    }

    // ── C-029 — the deny_unknown_fields split ───────────────────────────────

    /// C-029, S-035: the tolerance split is the contract. `[shell]` keeps fleet
    /// forward-compat; `[shell.consent]` refuses.
    #[test]
    fn c029_unknown_key_is_tolerated_on_shell_and_refused_on_consent() {
        let tolerated = parse("hook = true\nfuturekey = 1\n").expect("[shell] must tolerate an unknown key");
        assert_eq!(tolerated.hook, Some(true), "the known key must still take effect");

        let refused = parse("[consent]\nfuturekey = 1\n");
        assert!(
            refused.is_err(),
            "[shell.consent] must refuse an unknown key — dropping a narrowing key widens trust"
        );
    }

    /// C-029 named red state, and fault injection 2: a `namespaces` table
    /// carrying `include` **plus one unknown key** must fail to deserialize.
    /// Deleting the strict `ConsentScopeSpec` wrapper makes it start
    /// deserializing, which is the failure direction.
    /// EC-GRANT-008 — an unknown key inside the namespaces table is refused, never dropped: dropping a narrowing key would widen trust.
    #[test]
    fn c029_namespaces_table_with_include_plus_unknown_key_is_refused() {
        let result = parse("[consent.namespaces]\ninclude = [\"ocx.sh/acme\"]\nrequire_signed = [\"x\"]\n");
        let error = result.expect_err(
            "an unknown key beside `include` must refuse the table; the shipped ScopeSpec would drop it and activate \
             on the full namespace",
        );
        assert!(
            error.to_string().contains("require_signed"),
            "the refusal must name the key it refused, got: {error}"
        );
    }

    /// The shipped `ScopeSpec` is unchanged: `[[trust.policy]]` keeps its
    /// tolerant behaviour. Without this the strict wrapper could be "delivered"
    /// by tightening the shared type, which is not the contract.
    #[test]
    fn c029_trust_policy_scope_spec_still_drops_unknown_keys() {
        let spec: ScopeSpec = toml::from_str("value = { include = [\"ghcr.io/acme/*\"], require_signed = [\"x\"] }")
            .map(|wrapper: std::collections::HashMap<String, ScopeSpec>| wrapper["value"].clone())
            .expect("[[trust.policy]] scope must still tolerate an unknown key");
        assert!(
            matches!(spec, ScopeSpec::Set { ref include, .. } if include == &["ghcr.io/acme/*"]),
            "the trust-policy scope must be unchanged, got {spec:?}"
        );
    }

    // ── C-030 / A-27 — the grammar, at parse ────────────────────────────────

    /// C-030, A-27, S-043: the two accepted spellings, and every rejected
    /// class. One row per form so a regression names which one moved.
    /// EC-GRANT-003, EC-GRANT-004, EC-GRANT-005, EC-GRANT-006 — the accepted spellings, the rejected `*/*`, whole-registry and bare-host forms, and the uppercase class A-27 makes unmatchable.
    #[test]
    fn c030_a27_grammar_accepts_two_spellings_and_rejects_eight_classes() {
        for accepted in ["ocx.sh/acme", "ocx.sh/acme/*", "localhost:5000/acme/*"] {
            assert!(
                validate_consent_pattern(accepted).is_ok(),
                "'{accepted}' is one of the two accepted spellings"
            );
        }
        for rejected in [
            "",                     // empty string
            "*",                    // bare star
            "ocx.sh/acme-corp*",    // star not in final `/*` position
            "ocx.sh/*/tool",        // star not in final position
            "ocx.sh/*/*",           // more than one star
            "ocx.sh/acme/",         // trailing `/` with no `*`
            "/*",                   // the whole-registry form with no host
            "/ocx.sh/acme",         // leading `/` — empty component
            "ocx.sh//acme",         // `//` — empty component
            "ocx.sh/Acme",          // ASCII uppercase
            "OCX.SH/acme",          // ASCII uppercase in the host
            "ocx.sh/acme/team",     // three components — a repository, not a source
            "ocx.sh/acme/team/*",   // three components under a wildcard
            "ocx.sh/*",             // whole-registry grant, said outright
            "ocx.sh",               // whole-registry grant reached by dropping a segment
            "ocx.sh/acme@sha256:0", // `@`
            "ocx.sh/acme:1",        // `:` after the first `/`
        ] {
            assert!(
                validate_consent_pattern(rejected).is_err(),
                "'{rejected}' must be refused at parse"
            );
        }
    }

    /// A-27: `ocx.sh/acme/*` and `ocx.sh/acme` match the identical set, because
    /// a source is exactly two components. Neither matches `ocx.sh/acme-evil`.
    ///
    /// Red state: delete the trailing-`/*` strip in
    /// [`normalize_consent_pattern`] and the wildcard spelling stops matching
    /// `ocx.sh/acme`; delete the wildcard-position check in
    /// [`validate_consent_pattern`] and `ocx.sh/acme-corp*` starts matching
    /// `ocx.sh/acme-corp-evil`.
    #[test]
    fn c030_a27_descendant_form_is_vacuous_at_source_granularity() {
        for spelling in ["ocx.sh/acme", "ocx.sh/acme/*"] {
            let config = parse(&format!("[consent]\nnamespaces = \"{spelling}\"\n")).expect("spelling parses");
            let namespaces = config
                .consent
                .expect("consent present")
                .namespaces
                .expect("namespaces present");
            assert!(namespaces.matches("ocx.sh/acme"), "'{spelling}' must match ocx.sh/acme");
            assert!(
                !namespaces.matches("ocx.sh/acme-evil"),
                "'{spelling}' must never match the sibling org ocx.sh/acme-evil"
            );
        }
    }

    /// C-030, S-043, ocx-sh/ocx#344: there is no whole-registry grant, in either
    /// spelling.
    ///
    /// The bound a `namespaces` grant has is the organisation this host
    /// resolved and fetched the content under, and a whole-registry pattern voids
    /// that half of it on any host where anyone can register.
    /// Both spellings are refused together: leaving the bare-host one would make
    /// it the way to spell what `/*` no longer says.
    #[test]
    fn c030_344_a_whole_registry_grant_is_refused_in_both_spellings() {
        for spelling in ["ocx.sh/*", "ocx.sh", "ghcr.io/*", "localhost:5000/*"] {
            let error = validate_consent_pattern(spelling).expect_err("a whole-registry grant must be refused");
            assert!(
                matches!(error, ConsentPatternError::WholeRegistry(_)),
                "'{spelling}' must be refused as a whole-registry grant, not by accident; got {error:?}"
            );
            // The config channel erases the variant into a serde string, so
            // the match is on that variant's own `Display`: a bare `is_err()`
            // here is satisfied by any parse failure, including a typo in this
            // fixture's inline TOML.
            let through_config = parse(&format!("[consent]\nnamespaces = \"{spelling}\"\n"))
                .expect_err("a whole-registry grant must be refused through the config channel too");
            let expected = ConsentPatternError::WholeRegistry(spelling.to_string()).to_string();
            assert!(
                through_config.to_string().contains(&expected),
                "'{spelling}' must be refused through the config channel AS a whole-registry grant, not by some \
                 other parse failure; wanted '{expected}', got: {through_config}"
            );
        }
        // The positive control: the narrower spelling this pushes people to is
        // still accepted, so the refusal is not the whole grammar going red.
        assert!(validate_consent_pattern("ocx.sh/acme").is_ok());
    }

    /// C-030, S-043: `{ include = [], exclude = [...] }` and a table naming
    /// neither key are refused — never read as a catch-all.
    #[test]
    fn c030_empty_include_and_neither_key_are_refused() {
        assert!(
            parse("[consent.namespaces]\ninclude = []\nexclude = [\"x\"]\n").is_err(),
            "an empty `include` must be refused, not read as a catch-all"
        );
        assert!(
            parse("[consent.namespaces]\n").is_err(),
            "a table naming neither key must be refused"
        );
        assert!(
            parse("[consent.namespaces]\nexclude = [\"ocx.sh/acme\"]\n").is_err(),
            "an exclude-only table is a catch-all minus one org, and must be refused"
        );
    }

    /// S-043: carve-outs are at source granularity — an org subtracted from a
    /// multi-org include. The repository-granularity spelling is refused.
    ///
    /// The carve-out used to be demonstrated against a whole-registry include;
    /// that spelling is gone (ocx-sh/ocx#344), so the subtraction is shown where
    /// it still has work to do — a tier that appended an org another tier
    /// withdraws, which is what `accumulate`'s exclusion-wins rule is for.
    #[test]
    fn c030_s043_carve_out_is_source_granular() {
        let config = parse(
            "[consent.namespaces]\ninclude = [\"ocx.sh/acme\", \"ocx.sh/acme-compromised\"]\nexclude = [\"ocx.sh/acme-compromised\"]\n",
        )
        .expect("a source-granular carve-out parses");
        let namespaces = config.consent.unwrap().namespaces.unwrap();
        assert!(namespaces.matches("ocx.sh/acme"));
        assert!(!namespaces.matches("ocx.sh/acme-compromised"));

        assert!(
            parse("[consent.namespaces]\ninclude = [\"ocx.sh/acme/*\"]\nexclude = [\"ocx.sh/acme/compromised\"]\n")
                .is_err(),
            "a three-component exclude names a repository and must be refused at parse"
        );
    }

    /// A-28: `paths` entries normalize separators and a trailing slash — and,
    /// on Windows only, ASCII case, because the filesystem folds it there too.
    /// On a case-sensitive filesystem a case-only difference stays a mismatch.
    #[test]
    fn c030_a28_paths_normalize_separators_and_only_windows_folds_case() {
        let expected_trimmed = if cfg!(windows) {
            r"\home\u\project"
        } else {
            "/home/u/project"
        };
        assert_eq!(normalize_consent_path(Path::new("/home/u/project/")), expected_trimmed);
        let upper = normalize_consent_path(Path::new("/Users/u/Repo"));
        let lower = normalize_consent_path(Path::new("/Users/u/repo"));
        if cfg!(windows) {
            assert_eq!(
                upper, lower,
                "Windows folds ASCII case because its filesystem cannot hold both directories at once"
            );
        } else {
            assert_ne!(
                upper, lower,
                "case folding would merge two directories into one grant on a case-sensitive filesystem"
            );
        }
        let expected_root = if cfg!(windows) { r"\" } else { "/" };
        assert_eq!(
            normalize_consent_path(Path::new("/")),
            expected_root,
            "the root must not normalize to nothing"
        );
    }

    /// S2 axis (b) / CWE-41 — `\` is a **separator on Windows and an ordinary
    /// filename byte everywhere else**, and the normalizer must follow the
    /// platform rather than rewrite the byte unconditionally.
    ///
    /// The unconditional rewrite this replaced made a Unix directory literally
    /// named `services\api` — one legal name `git` will happily check out —
    /// satisfy a `paths` grant for `/workspaces/mono/services/api`.
    ///
    /// Red state: restore `path.to_string_lossy().replace('\\', "/")` and the
    /// Unix arm's `assert_ne!` flips.
    #[test]
    fn s2_a_backslash_is_a_separator_only_where_the_platform_says_so() {
        let slashed = normalize_consent_path(Path::new("/workspaces/mono/services/api"));
        let backslashed = normalize_consent_path(Path::new(r"/workspaces/mono/services\api"));

        if cfg!(windows) {
            assert_eq!(
                slashed, backslashed,
                "on Windows the two spellings name one directory and must share one grant"
            );
        } else {
            assert_ne!(
                slashed, backslashed,
                r"on Unix `services\api` is a single legal directory name, not two path components; \
                  conflating them widens a `paths` grant onto a directory an attacker can create"
            );
        }
    }

    /// S2 axis (a) / CWE-41 — the comparison operand is the path's own bytes,
    /// never a lossy `String`.
    ///
    /// `to_string_lossy` maps **every** non-UTF-8 byte to one `U+FFFD`, so two
    /// distinct directories differing only in such a byte collapsed onto one
    /// grant key. Unix-only because that is where a non-UTF-8 path is
    /// constructible: Windows paths are UTF-16, and `OsString` there cannot
    /// hold an arbitrary lone byte.
    ///
    /// Red state: restore the `to_string_lossy()` body and the `assert_ne!`
    /// flips — both operands render as `/w/a\u{FFFD}`.
    #[cfg(unix)]
    #[test]
    fn s2_a_non_utf8_path_never_collapses_onto_another() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let first = normalize_consent_path(Path::new(OsStr::from_bytes(b"/w/a\xFE")));
        let second = normalize_consent_path(Path::new(OsStr::from_bytes(b"/w/a\xFF")));

        assert_ne!(
            first, second,
            "two directories differing only in a non-UTF-8 byte must not share one grant key"
        );
        assert_eq!(
            first,
            OsStr::from_bytes(b"/w/a\xFE"),
            "the surviving bytes are the path's own, not a replacement character"
        );
    }

    // ── the entry side: drive letters, `~`, and the defect predicate ─────────

    /// Both arms of [`consent_path_matches`] fold the same thing as each other,
    /// and what that is follows the platform's own filesystem.
    ///
    /// `Path::starts_with` compares `Component`s, and `PrefixComponent`'s
    /// equality is on its *parsed* form, whose drive letter `std`
    /// ASCII-uppercases (`library/std/src/sys/path/windows_prefix.rs`,
    /// `parse_drive`). The exact arm used to compare `OsString`s bytewise, so
    /// `c:\w\acme\*` granted `C:\w\acme\sub` while `c:\w\acme` did not
    /// grant `C:\w\acme` — one entry style, two case rules.
    ///
    /// Ordinary components then fold ASCII case on Windows, where the
    /// filesystem cannot hold two directories that differ only by case, and
    /// never on Unix, where it can and folding would widen a grant onto a
    /// directory an attacker may create.
    ///
    /// EC-GRANT-025 — an ordinary component's ASCII case folds on Windows and
    /// nowhere else, in both arms.
    ///
    /// Red state, Unix half: lowercase both operands of the exact arm
    /// (`…to_string_lossy().to_ascii_lowercase()`) and the ordinary-component
    /// refusals flip. Red state, Windows half: drop the `Component::Normal` arm
    /// from [`normalize_consent_path`] and the two case-folded grants flip.
    /// Neither half's red state is reachable from the other's host, which is
    /// why each is pinned separately rather than behind one `cfg!` expression.
    #[test]
    fn s2_both_arms_fold_the_same_thing_as_each_other_and_the_platform() {
        // The positive control: an exact entry grants its own directory, so
        // every refusal below is this rule and not a dead clause.
        assert!(consent_path_matches(Path::new("/w/acme"), Path::new("/w/acme")));

        #[cfg(windows)]
        {
            assert!(
                consent_path_matches(Path::new(r"c:\w\acme"), Path::new(r"C:\w\acme")),
                "the exact arm must fold the drive letter, because the subtree arm already does"
            );
            assert!(
                consent_path_matches(Path::new(r"c:\w\acme\*"), Path::new(r"C:\w\acme\sub")),
                "the subtree arm's drive-letter folding is the behaviour being matched, not changed"
            );
            // Windows cannot hold `C:\w\Acme` and `C:\w\acme` at once, so
            // folding them onto one grant merges nothing that was ever apart —
            // and refusing to fold would leave an operator inert on a directory
            // the OS considers identical to the one they wrote.
            assert!(
                consent_path_matches(Path::new(r"C:\w\Acme"), Path::new(r"C:\w\acme")),
                "an ordinary component folds ASCII case on Windows, in the exact arm"
            );
            assert!(
                consent_path_matches(Path::new(r"C:\w\Acme\*"), Path::new(r"C:\w\acme\sub")),
                "an ordinary component folds ASCII case on Windows, in the subtree arm too"
            );
            // `/` and `\` are both separators on Windows, so the two spellings
            // of one directory must be one grant — in both arms, since each
            // splits through `Path::components`.
            assert!(
                consent_path_matches(Path::new("C:/w/acme"), Path::new(r"C:\w\acme")),
                "a forward-slash entry names the same directory as a backslash one"
            );
            assert!(
                consent_path_matches(Path::new("C:/w/acme/*"), Path::new(r"C:\w\acme\sub")),
                "the subtree arm splits on the same separators the exact arm does"
            );
        }
        #[cfg(not(windows))]
        {
            // Unix holds `/w/Acme` and `/w/acme` as two directories, so folding
            // them would hand a grant to whichever one an attacker got to
            // create first.
            assert!(
                !consent_path_matches(Path::new("/w/Acme"), Path::new("/w/acme")),
                "the exact arm must keep an ordinary component's own bytes on a case-sensitive filesystem"
            );
            assert!(
                !consent_path_matches(Path::new("/w/Acme/*"), Path::new("/w/acme/sub")),
                "the subtree arm must keep an ordinary component's own bytes too"
            );
            assert!(
                !consent_path_matches(Path::new(r"c:\w\acme"), Path::new(r"C:\w\acme")),
                "with no Prefix component there is nothing to fold; these are two ordinary directory names"
            );
        }
    }

    /// The grammar has exactly one wildcard — a whole trailing `*` component.
    /// Every other glob metacharacter is an ordinary filename byte.
    ///
    /// Worth pinning because the entry is a plain TOML string with no load-time
    /// grammar, so `?` looks like it might mean something: a reader who writes
    /// `/w/acm?` gets a silently inert entry, not a one-character wildcard, and
    /// nobody may later "fix" that by routing the entry through a glob matcher —
    /// `?` and `[…]` match arbitrary siblings, which is the whole reach the
    /// component-bounded design exists to deny.
    ///
    /// EC-GRANT-026 — a trailing `*` component is the only wildcard; every
    /// other glob metacharacter is a literal filename byte.
    ///
    /// Red state: match the entry with any glob engine and the four refusals
    /// below flip; the two literal-identity assertions are the positive control
    /// that keeps them from passing for want of a match altogether.
    #[test]
    fn s2_only_a_trailing_star_is_a_wildcard_and_every_other_glob_byte_is_literal() {
        for (entry, project) in [
            ("/w/acm?", "/w/acm3"),
            ("/w/acme?", "/w/acme"),
            ("/w/acm?/*", "/w/acm3/sub"),
            ("/w/[ab]", "/w/a"),
        ] {
            assert!(
                !consent_path_matches(Path::new(entry), Path::new(project)),
                "'{entry}' must not match '{project}' — the only wildcard is a trailing `*` component"
            );
        }

        // …and each of those bytes still matches itself, so an entry naming a
        // directory that legitimately carries one is a working grant rather
        // than a shape the matcher refuses.
        for path in ["/w/we?rd", "/w/[ab]"] {
            assert!(
                consent_path_matches(Path::new(path), Path::new(path)),
                "'{path}' is a legal directory name and must grant itself literally"
            );
        }
    }

    /// A leading `~` expands against the home directory, in **both** channels,
    /// and the entry itself is stored as the user wrote it.
    ///
    /// git interpolates one in `safe.directory`; before this, `~/dev/*` was
    /// silently inert, which is the failure class this whole branch is about.
    ///
    /// EC-GRANT-022 — a leading `~` expands; the expansion is textual and
    /// resolves no symlink.
    ///
    /// Red state: return `Ok(entry.to_path_buf())` unconditionally from
    /// [`expand_against`] — the pre-fix behaviour — and every assertion below
    /// flips except the literal-`~`-component one.
    #[test]
    fn a28_a_leading_tilde_expands_against_the_home_directory() {
        let home = Path::new("/home/u");

        assert_eq!(
            expand_against(Path::new("~/dev/*"), Some(home)),
            Ok(PathBuf::from("/home/u/dev/*")),
            "`~/…` joins onto the home directory, wildcard and all"
        );
        assert_eq!(
            expand_against(Path::new("~"), Some(home)),
            Ok(PathBuf::from("/home/u")),
            "a bare `~` is the home directory itself"
        );
        assert_eq!(
            expand_against(Path::new("~alice/dev"), Some(home)),
            Err(EntryDefect::UnsupportedTildeUser),
            "`~user` is not supported and must never expand to this user's home"
        );
        assert_eq!(
            expand_against(Path::new("~/dev"), None),
            Err(EntryDefect::UnresolvableHome),
            "no home directory means the entry matches nothing, never a literal `~` compare"
        );
        assert_eq!(
            expand_against(Path::new("/w/~/dev"), Some(home)),
            Ok(PathBuf::from("/w/~/dev")),
            "only a LEADING `~` expands; `~` elsewhere is a legal directory name"
        );

        // Both channels carry the entry as written — the expansion happens at
        // match time, so `ocx shell state` can still print `~/dev/*`.
        assert_eq!(
            env_channel(Some("~/dev/*"), None).paths,
            vec![PathBuf::from("~/dev/*")],
            "the env channel stores the entry verbatim"
        );
        assert_eq!(
            parse("[consent]\npaths = [\"~/dev/*\"]\n")
                .expect("parses")
                .consent
                .expect("consent present")
                .paths,
            vec![PathBuf::from("~/dev/*")],
            "the config channel stores the entry verbatim"
        );

        // End to end, against this machine's own home. Both branches assert:
        // a machine with no home directory must make the entry inert, not
        // make this test vacuous.
        match crate::file_structure::home_directory() {
            Some(home) => {
                assert!(
                    consent_path_matches(Path::new("~/dev/*"), &home.join("dev").join("acme")),
                    "a checkout under the granted tree activates through the expansion"
                );
                assert!(
                    !consent_path_matches(Path::new("~/dev/*"), Path::new("/elsewhere/dev/acme")),
                    "the expansion is not a licence to match outside the home directory"
                );
            }
            None => assert_eq!(
                consent_entry_defect(Path::new("~/dev/*")),
                Some(EntryDefect::UnresolvableHome),
                "with no home directory the entry is reported rather than silently inert"
            ),
        }
    }

    /// [`consent_entry_defect`] and [`consent_path_matches`] agree: every
    /// entry the predicate calls defective is one the matcher refuses, and
    /// every entry it calls well-formed is one the matcher grants.
    ///
    /// A defect-free entry that never matches, or a "defective" entry the
    /// matcher would grant, is the failure this pairing exists to catch — so
    /// each row carries the directories a reader would expect it to cover.
    ///
    /// EC-GRANT-024 — every entry the diagnostic calls defective is one the
    /// matcher refuses, and every defect-free entry is one it can grant.
    ///
    /// Red state: drop the `!entry.is_absolute()` arm of
    /// [`consent_entry_defect`] and the two relative rows report `None` while
    /// the matcher still refuses them — the disagreement in its cheapest form.
    #[test]
    fn c030_the_entry_defect_predicate_agrees_with_the_matcher() {
        // (entry, its defect, directories a reader might expect it to cover)
        let defective: &[(&str, EntryDefect, &[&str])] = &[
            (
                "/w/*/tools",
                EntryDefect::StarNotLast,
                &["/w/acme/tools", "/w/tools", "/w"],
            ),
            (
                "/w/acme*",
                EntryDefect::StarInsideComponent,
                &["/w/acme", "/w/acme-corp", "/w/acme/sub"],
            ),
            ("*", EntryDefect::StarNamesNoDirectory, &["/w/acme", "/"]),
            ("/*", EntryDefect::StarNamesNoDirectory, &["/w/acme", "/"]),
            ("/w/acme/../etc", EntryDefect::ParentDirComponent, &["/w/etc", "/etc"]),
            (
                "~alice/dev",
                EntryDefect::UnsupportedTildeUser,
                &["/home/alice/dev", "/w/dev"],
            ),
            ("dev/tools", EntryDefect::RelativePath, &["/w/dev/tools", "/dev/tools"]),
            ("dev/*", EntryDefect::RelativePath, &["/w/dev/tools", "/dev/tools"]),
        ];
        for (entry, defect, never_granted) in defective {
            assert_eq!(
                consent_entry_defect(Path::new(entry)),
                Some(*defect),
                "'{entry}' must be reported as {defect:?}"
            );
            for directory in *never_granted {
                assert!(
                    !consent_path_matches(Path::new(entry), Path::new(directory)),
                    "'{entry}' is reported as {defect:?}, so the matcher must not grant '{directory}'"
                );
            }
        }

        // The other direction: a well-formed entry is reported clean AND
        // actually grants what it names. Without these rows the assertions
        // above are satisfied by a predicate that condemns everything.
        //
        // These rows carry a drive prefix on Windows because they stand in for
        // a **canonical** directory, which `dunce::canonicalize` always spells
        // with one. A driveless `/w/acme` is `RelativePath` there — correctly,
        // and that is the row above, not this one.
        for (entry, granted) in [
            ("/w/acme", "/w/acme"),
            ("/w/acme/", "/w/acme"),
            ("/w/acme/*", "/w/acme"),
            ("/w/acme/*", "/w/acme/tools/deep"),
            ("/w/acme-corp", "/w/acme-corp"),
        ] {
            let (entry, granted) = (abs(entry), abs(granted));
            assert_eq!(
                consent_entry_defect(&entry),
                None,
                "'{}' is a well-formed entry",
                entry.display()
            );
            assert!(
                consent_path_matches(&entry, &granted),
                "'{}' is reported clean, so it must grant '{}'",
                entry.display(),
                granted.display()
            );
        }

        // `UnresolvableHome` is the one variant whose input is the machine
        // rather than the entry. It is asserted through the seam both the
        // predicate and the matcher route through, which is what makes the
        // agreement structural: an entry this refuses is one the matcher's own
        // `let Ok(entry) = …` arm refuses.
        assert_eq!(
            expand_against(Path::new("~/dev/*"), None),
            Err(EntryDefect::UnresolvableHome)
        );

        // Windows-only, because `*` is not a legal filename byte there and the
        // drive prefix is the component that leaves the `*` naming nothing.
        #[cfg(windows)]
        {
            assert_eq!(
                consent_entry_defect(Path::new(r"C:\*")),
                Some(EntryDefect::StarNamesNoDirectory)
            );
            assert!(!consent_path_matches(Path::new(r"C:\*"), Path::new(r"C:\w\acme")));
        }
    }

    // ── C-031 / S-037 — the env channel ─────────────────────────────────────

    /// C-031, S-037, and fault injection 1: empty tokens are dropped **before**
    /// any pattern is constructed. The assertion is on the parsed `include` set
    /// itself, not on a downstream match — an empty token could otherwise leak
    /// through the parser and be filtered later, giving a false green.
    ///
    /// Red state: keep empty tokens in [`parse_consent_namespaces`] and the
    /// `include` set gains an empty pattern here.
    /// EC-GRANT-013, EC-GRANT-014 — a single `,` and a `,,` run both yield no pattern; asserted on the parsed set, before any match is evaluated.
    #[test]
    fn c031_s037_env_channel_drops_empty_tokens_before_any_pattern_exists() {
        for (value, expected) in [
            ("ocx.sh/acme/*,", vec!["ocx.sh/acme"]),
            ("ocx.sh/a,,ocx.sh/b", vec!["ocx.sh/a", "ocx.sh/b"]),
            (",", vec![]),
            ("", vec![]),
            (" , ocx.sh/acme , ", vec!["ocx.sh/acme"]),
        ] {
            let consent = env_channel(None, Some(value));
            assert_eq!(
                include_of(&consent),
                expected,
                "'{value}' must parse to exactly its non-empty patterns, with no empty pattern in the include set"
            );
            assert!(
                !include_of(&consent).iter().any(String::is_empty),
                "'{value}' leaked an empty pattern, which trust::pattern_matches reads as a catch-all"
            );
            assert!(
                !consent
                    .namespaces
                    .as_ref()
                    .is_some_and(|spec| spec.matches("ghcr.io/evil/tool")),
                "'{value}' must never consent to an untrusted source"
            );
        }
    }

    /// C-031, S-037: an empty token must never become an empty `PathBuf`, which
    /// normalizes toward a root rather than toward nothing.
    /// EC-GRANT-016 — an empty OS-PATH token grants nothing.
    #[test]
    fn c031_s037_env_channel_drops_empty_path_tokens() {
        let separator = if cfg!(windows) { ';' } else { ':' };
        let value = format!("{separator}/home/u/project{separator}{separator}");
        let consent = env_channel(Some(&value), None);
        assert_eq!(consent.paths, vec![PathBuf::from("/home/u/project")]);
        assert!(
            !consent.paths.iter().any(|path| path.as_os_str().is_empty()),
            "an empty token must never become a PathBuf"
        );
    }

    /// C-031 + the subtree form's primary use case: a devcontainer or CI image
    /// writes `OCX_CONSENT_PATHS=/w/acme/*` into the image and every checkout
    /// that lands under `/w/acme` later activates, without the image knowing
    /// their names.
    ///
    /// Asserted end to end through the env channel rather than on
    /// [`consent_path_matches`] alone: `split_paths` owns the split, so a
    /// channel that mangled the `*` token would leave the matcher's own tests
    /// green while the documented use case never worked.
    ///
    /// Red state: strip the entry's last component in [`parse_consent_paths`]
    /// (or trim `*` off it) and the first assertion goes inert.
    #[test]
    fn c031_the_env_channel_carries_a_subtree_entry_intact() {
        let consent = env_channel(Some("/w/acme/*"), None);
        let [entry] = consent.paths.as_slice() else {
            panic!("the channel must contribute exactly one entry, got {:?}", consent.paths);
        };

        assert!(
            consent_path_matches(entry, Path::new("/w/acme/tools")),
            "a checkout under the granted tree is covered by the image's entry"
        );
        assert!(
            consent_path_matches(entry, Path::new("/w/acme")),
            "the named directory is inside its own subtree"
        );
        assert!(
            !consent_path_matches(entry, Path::new("/w/acme-evil")),
            "the sibling a string prefix would have caught is component-bounded out"
        );
    }

    /// C-031, A-27: a single malformed non-empty pattern discards the **whole**
    /// contribution with no error — a hard error would break every prompt.
    #[test]
    fn c031_env_channel_discards_the_whole_contribution_on_one_bad_pattern() {
        let consent = env_channel(None, Some("ocx.sh/acme,ocx.sh/acme-corp*"));
        assert!(
            consent.namespaces.is_none(),
            "a malformed pattern must discard the whole contribution, never partially parse"
        );
    }

    // ── C-032 — merge semantics + tier provenance ───────────────────────────

    /// C-032: `hook` and `completions` are scalar-wins-if-`Some` in both
    /// directions, and the provenance travels with the value.
    #[test]
    fn c032_scalars_win_if_present_and_carry_their_tier() {
        let mut lower = ShellConfig {
            hook: Some(true),
            hook_tier: Some(ConfigTier::User),
            completions: Some(true),
            completions_tier: Some(ConfigTier::User),
            ..ShellConfig::default()
        };
        lower.merge(ShellConfig {
            hook: Some(false),
            hook_tier: Some(ConfigTier::Managed),
            ..ShellConfig::default()
        });
        assert_eq!(lower.hook, Some(false), "a higher tier wins in the off direction too");
        assert_eq!(
            lower.hook_tier,
            Some(ConfigTier::Managed),
            "the deciding tier is recorded"
        );
        assert_eq!(
            lower.completions,
            Some(true),
            "a None in the higher tier does not clobber"
        );
        assert_eq!(
            lower.completions_tier,
            Some(ConfigTier::User),
            "a tier that did not set the scalar must not claim to have decided it"
        );
    }

    /// C-032: `paths` append and `namespaces` accumulate — no tier overrides
    /// another, and an `exclude` from either side beats an `include` from
    /// either side.
    #[test]
    fn c032_consent_accumulates_and_exclusion_wins_regardless_of_tier() {
        let mut lower =
            parse("[consent]\npaths = [\"/a\"]\nnamespaces = { include = [\"ocx.sh/good\", \"ocx.sh/bad\"] }\n")
                .expect("lower tier parses");
        let higher = parse(
            "[consent]\npaths = [\"/b\"]\nnamespaces = { include = [\"ghcr.io/acme\"], exclude = [\"ocx.sh/bad\"] }\n",
        )
        .expect("higher tier parses");
        lower.merge(higher);

        let consent = lower.consent.expect("consent present");
        assert_eq!(consent.paths, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
        let namespaces = consent.namespaces.expect("namespaces present");
        assert_eq!(namespaces.include(), ["ocx.sh/good", "ocx.sh/bad", "ghcr.io/acme"]);
        assert!(namespaces.matches("ocx.sh/good"), "the lower tier's grant survives");
        assert!(namespaces.matches("ghcr.io/acme"), "the higher tier's grant applies");
        assert!(
            !namespaces.matches("ocx.sh/bad"),
            "an exclude beats an include contributed by another tier"
        );
    }

    /// C-032: the same union in the other tier order produces the same spec —
    /// accumulation is order-independent by construction, which is what "no
    /// tier overrides another" means.
    #[test]
    fn c032_accumulation_is_symmetric_in_what_it_grants() {
        let mut reversed =
            parse("[consent]\nnamespaces = { include = [\"ghcr.io/acme\"], exclude = [\"ocx.sh/bad\"] }\n")
                .expect("parses");
        reversed
            .merge(parse("[consent]\nnamespaces = { include = [\"ocx.sh/good\", \"ocx.sh/bad\"] }\n").expect("parses"));
        let namespaces = reversed.consent.unwrap().namespaces.unwrap();
        assert!(namespaces.matches("ocx.sh/good"));
        assert!(!namespaces.matches("ocx.sh/bad"));
    }

    // ── A-33 — the env channel unions with the config tiers ─────────────────

    /// A-33/C-031: [`effective_consent`] is the config tiers plus the env
    /// channel, additively. The env channel never replaces a config grant.
    #[test]
    fn a33_effective_consent_unions_config_tiers_with_the_env_channel() {
        // `effective_consent` reads the process environment, so exercise the
        // union through the pure halves it composes rather than mutating it.
        let configured = parse("[consent]\nnamespaces = \"ocx.sh/acme\"\n").expect("parses");
        let mut consent = configured.consent.expect("consent present");
        consent.merge(env_channel(None, Some("ghcr.io/team/*")));

        let namespaces = consent.namespaces.expect("namespaces present");
        assert!(namespaces.matches("ocx.sh/acme"), "the config tier's grant survives");
        assert!(namespaces.matches("ghcr.io/team"), "the env channel's grant is added");
        assert!(!namespaces.matches("ghcr.io/evil"));
    }
}
