// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `[shell]` configuration section: enablement toggles plus the activation
//! consent whitelist.
//!
//! `[shell]` is never read from `ocx.toml` ([`ConfigLoader::fold_project_tier`] strips it), so
//! the whitelist comes only from a `config.toml` tier and consent can be checked before parsing.
//!
//! [`ConfigLoader::fold_project_tier`]: crate::loader::ConfigLoader::fold_project_tier

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Deserializer};

use crate::ConfigTier;
use ocx_trust::ScopeSpec;

/// `OCX_CONSENT_PATHS` — an OS-PATH-separated list of directories that activate
/// unconditionally, unioned with `[shell.consent] paths`.
pub const OCX_CONSENT_PATHS: &str = "OCX_CONSENT_PATHS";

/// `OCX_CONSENT_NAMESPACES` — a comma-separated list of source namespaces,
/// unioned with `[shell.consent] namespaces`' `include` set.
// Comma, not the OS PATH separator: a registry port (`localhost:5000`) makes `:` unusable.
pub const OCX_CONSENT_NAMESPACES: &str = "OCX_CONSENT_NAMESPACES";

/// Every variable that can turn an inert project into an activating one without a file moving.
///
/// The watch-set fingerprint and the per-prompt hook guard both read this list; a guard blind to a
/// grant never reaches the fold that would notice it.
pub const GRANT_SIGNALS: [&str; 2] = [OCX_CONSENT_PATHS, OCX_CONSENT_NAMESPACES];

// No `deny_unknown_fields`, for the fleet forward-compat reason stated on `crate::Config`.
/// The `[shell]` section.
///
/// Unknown keys are ignored, like in every other `config.toml` table — except inside
/// `[shell.consent]`, which refuses them.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
pub struct ShellConfig {
    /// Whether the per-prompt shell hook runs.
    ///
    /// `--hook` / `--no-hook` and a truthy `OCX_NO_HOOK` take precedence; absent means it
    /// follows session interactivity. A higher tier's value wins in either direction, the managed
    /// tier over a user's own file included.
    // Merging in both directions is safe only while consent gates every project independently.
    pub hook: Option<bool>,

    /// Whether shell completions load, with the same precedence as `hook`:
    /// `--completion` / `--no-completion` and a truthy `OCX_NO_COMPLETION` win.
    pub completions: Option<bool>,

    /// Whether `ocx self setup` may write PATH surfaces — shell profiles and
    /// the session-PATH stores alike. Absent means true.
    pub modify_path: Option<bool>,

    /// Which shell profile files receive the managed ocx block.
    ///
    /// Absent means auto-detect fresh on every run; an empty list means write
    /// no profile blocks at all.
    // Never write a detection result back here, or absent silently becomes a frozen list.
    pub profiles: Option<Vec<PathBuf>>,

    /// The activation consent whitelist (`[shell.consent]`).
    pub consent: Option<ShellConsent>,

    /// The tier that set `hook`, stamped by the loader and carried with the value through `merge`.
    #[serde(skip)]
    #[schemars(skip)]
    pub hook_tier: Option<ConfigTier>,

    /// The tier that set `completions` — `hook_tier`'s twin.
    #[serde(skip)]
    #[schemars(skip)]
    pub completions_tier: Option<ConfigTier>,

    /// The tier that set `modify_path` — `hook_tier`'s twin.
    #[serde(skip)]
    #[schemars(skip)]
    pub modify_path_tier: Option<ConfigTier>,

    /// The tier that set `profiles` — `hook_tier`'s twin.
    #[serde(skip)]
    #[schemars(skip)]
    pub profiles_tier: Option<ConfigTier>,

    /// Why a managed payload's `[shell.consent]` was dropped; carried here because the shims
    /// discard the stderr its warning goes to.
    #[serde(skip)]
    #[schemars(skip)]
    pub consent_strip_reason: Option<String>,
}

impl ShellConfig {
    /// Merge `other` (higher precedence) into `self`; a lower tier can only remove a higher tier's
    /// consent grant.
    ///
    /// | Field | Rule |
    /// |---|---|
    /// | `hook`, `completions`, `modify_path` | scalar: a higher `Some` wins, both directions |
    /// | `profiles` | whole list: a higher `Some` **replaces**, never unions; `None` never clears |
    /// | `consent.paths` | **appends** |
    /// | `consent.namespaces` | **one accumulated spec**: `include`s and `exclude`s unioned |
    pub fn merge(&mut self, other: ShellConfig) {
        // Provenance travels with the value: a tier that did not set it must not claim it.
        if other.hook.is_some() {
            self.hook = other.hook;
            self.hook_tier = other.hook_tier;
        }
        if other.completions.is_some() {
            self.completions = other.completions;
            self.completions_tier = other.completions_tier;
        }
        if other.modify_path.is_some() {
            self.modify_path = other.modify_path;
            self.modify_path_tier = other.modify_path_tier;
        }
        // Replace, never union: naming explicit profiles excludes the others.
        if other.profiles.is_some() {
            self.profiles = other.profiles;
            self.profiles_tier = other.profiles_tier;
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

// Strict, the `arch-principles.md` carve-out: a dropped unknown narrowing key would widen trust.
/// The `[shell.consent]` table.
///
/// Unknown keys are refused here, unlike every other `config.toml` table: a host that meets a
/// future narrowing key it does not know **refuses** the payload rather than silently dropping the
/// key and activating on the full namespace.
#[derive(Debug, Clone, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellConsent {
    /// Directories that activate unconditionally, modelled on git's `safe.directory`.
    ///
    /// An entry is one exact directory, or — with a trailing `/*` — that directory and everything
    /// beneath it, matched component-wise (unlike git, `/*` covers the named directory too). A bare `*`
    /// grants nothing: there is no trust-this-machine token. Entries are compared **literally**, after
    /// separator and trailing-slash normalization, against the canonical project directory, so an
    /// entry naming a symlinked checkout never matches; `ocx shell state` shows a near-miss when an
    /// entry differs only by ASCII case. A **leading** `~` expands against the home directory at match
    /// time, textually, resolving no symlink and never rewritten into the stored entry; `~user` is not
    /// supported. A `paths` grant is drift-blind and writes no stamp, so revoking it is immediate.
    #[serde(default)]
    pub paths: Vec<PathBuf>,

    /// Source namespaces that activate a project whose whole lock is inside them.
    ///
    /// One pattern, or one `{include, exclude}` table where `exclude` beats `include`, so
    /// "everything under `ocx.sh/acme/*` except the one compromised namespace" is spellable.
    // One spec, never a `Vec`: a flat list can only widen, so exclusion would be unspellable.
    pub namespaces: Option<ConsentScopeSpec>,
}

impl ShellConsent {
    /// Accumulate `other` into `self`: `paths` append, `namespaces` union into one spec, so a tier
    /// can only add an `include` or an `exclude`.
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

/// A consent-scoped [`ScopeSpec`]: same matching, strict parsing, patterns stored without a
/// trailing `/*`.
///
/// `ScopeSpec` drops unknown table keys for fleet forward-compat; here a dropped narrowing key
/// would widen consent on an older host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentScopeSpec(pub ScopeSpec);

impl ConsentScopeSpec {
    /// Whether `source`, a canonical two-component `registry/org` source, is
    /// consented by this spec.
    #[must_use]
    pub fn matches(&self, source: &str) -> bool {
        self.0.matches(source)
    }

    /// The `include` patterns, in declaration order; never empty.
    #[must_use]
    pub fn include(&self) -> &[String] {
        match &self.0 {
            ScopeSpec::Prefix(pattern) => std::slice::from_ref(pattern),
            ScopeSpec::Set { include, .. } => include,
        }
    }

    /// The `exclude` patterns, in declaration order.
    #[must_use]
    pub fn exclude(&self) -> &[String] {
        match &self.0 {
            ScopeSpec::Prefix(_) => &[],
            ScopeSpec::Set { exclude, .. } => exclude,
        }
    }

    /// Union `other`'s `include` and `exclude` into `self`'s, deduplicated, in declaration order.
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
    /// `ScopeSpec`'s visitor, but refusing unknown keys and an empty `include`, and validating
    /// every pattern.
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
                        // `ScopeSpec` drops an unknown key here; on consent that would widen trust.
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

/// Validate and normalize every pattern, failing on the first rejection.
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

    /// Delegated to [`ScopeSpec`]; only the deserializer is strict.
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <ScopeSpec as schemars::JsonSchema>::json_schema(generator)
    }
}

/// Why a `[shell.consent] namespaces` pattern was refused.
///
/// Fails closed without taking the rest down: a config tier loses its whole `[shell.consent]`
/// table ([`ShellConfig::consent_strip_reason`]), the env channel its whole contribution.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsentPatternError {
    /// The empty string, which `ocx_trust::pattern_matches` reads as a catch-all.
    #[error("a consent namespace pattern must not be empty")]
    Empty,

    /// An ASCII uppercase byte; `PackageRef` refuses uppercase, so the pattern matches nothing.
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
    /// names a repository; a source is exactly two components.
    #[error("consent namespace '{0}' names a repository, not a source; write '<host>/<org>'")]
    TooManyComponents(String),

    /// A whole-registry grant, in either spelling.
    ///
    /// A grant is bounded by the organisation that served the content (`adr_shell_env_overhaul.md`),
    /// so a whole registry would admit an org an attacker registered minutes ago.
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
        source: ocx_oci::IdentifierError,
    },
}

/// The `[shell.consent] namespaces` grammar, shared by the `config.toml` tiers and
/// `OCX_CONSENT_NAMESPACES`: exactly `<host>[:<port>]/<org>`, optionally with a trailing `/*`.
///
/// # Errors
///
/// [`ConsentPatternError`], naming the offending pattern and its class.
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
    // Only a trailing `/*`: `pattern_matches` globs substrings, so `ocx.sh/acme-corp*` would match
    // `ocx.sh/acme-corp-evil/tool`.
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
        // `<host>` and `<host>/*` are refused together, or one stays a way to spell a whole registry.
        [_host] => Err(ConsentPatternError::WholeRegistry(pattern.to_string())),
        // The repository validator is also what rejects a `:` after the first `/`.
        [_host, org] => {
            ocx_oci::PackageRef::validate_repository(org).map_err(|source| ConsentPatternError::Organisation {
                pattern: pattern.to_string(),
                source,
            })
        }
        _ => Err(ConsentPatternError::TooManyComponents(pattern.to_string())),
    }
}

/// Validate `pattern`, then return it with a trailing `/*` stripped.
///
/// Stripping sends matching down `pattern_matches`' segment-bounded branch, or `ocx.sh/acme/*`
/// would match `ocx.sh/acme-evil`.
///
/// # Errors
///
/// [`ConsentPatternError`], from [`validate_consent_pattern`].
pub fn normalize_consent_pattern(pattern: &str) -> Result<String, ConsentPatternError> {
    validate_consent_pattern(pattern)?;
    Ok(pattern.strip_suffix("/*").unwrap_or(pattern).to_string())
}

/// Render `path` for the literal `[shell.consent] paths` comparison: separator, trailing-slash
/// and, on Windows only, ASCII-case normalization.
///
/// Never canonicalizes, or a grant follows a symlink an attacker controls on the parent.
/// Folding case on Unix would widen a grant onto a directory an attacker can create.
/// An `OsString`, since a lossy `String` collapses distinct non-UTF-8 names (CWE-41).
#[must_use]
pub fn normalize_consent_path(path: &Path) -> OsString {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            // ASCII-only, like `std`'s drive-letter fold: a Unicode fold is locale-dependent.
            #[cfg(windows)]
            Component::Normal(name) => normalized.push(name.to_ascii_lowercase()),
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized.into_os_string()
}

/// Whether `paths` entry `entry` grants the canonical `project_dir`: `/w/acme` grants that
/// directory, `/w/acme/*` it and everything beneath, component-wise (never `/w/acme-evil`).
///
/// A leading `~` expands against the home directory. An entry [`consent_entry_defect`] flags, or
/// a `project_dir` holding `..`, matches nothing.
#[must_use]
pub fn consent_path_matches(entry: &Path, project_dir: &Path) -> bool {
    // Refused, not asserted: `/w/acme/../../etc` starts with `/w/acme`, and a debug assert would
    // leave release builds granting it.
    if project_dir
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return false;
    }
    let Ok(entry) = expanded_entry(entry) else {
        return false;
    };
    let project = PathBuf::from(normalize_consent_path(project_dir));
    match subtree_prefix(&entry) {
        // Normalized too, or the subtree arm stays case-sensitive on Windows while the exact arm folds.
        Some(prefix) => project.starts_with(normalize_consent_path(&prefix)),
        // Compares components like `starts_with`, not bytes, so both arms fold the drive letter alike.
        None => project == normalize_consent_path(&entry),
    }
}

/// The directory a subtree entry names, or `None` for an exact grant or a `*` naming no
/// directory (a whole-filesystem grant is not expressible).
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

/// Why a `paths` entry can never match any canonical directory; one defect, most specific first.
///
/// The `*` classes misread only a directory literally named `*`.
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
    /// A relative entry; a canonical directory is always absolute.
    RelativePath,
}

impl std::fmt::Display for EntryDefect {
    /// The defect and the likely intended spelling, as `ocx shell state` prints it.
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

/// `Some(defect)` when `entry` can never match, `None` for a well-formed entry.
///
/// Agrees with [`consent_path_matches`] only by routing through the same `expanded_entry` and
/// `subtree_prefix`.
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
        // `subtree_prefix` decides; a second copy of its rule would drift from the matcher.
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

/// `entry` with a leading `~` expanded textually against this machine's home directory.
fn expanded_entry(entry: &Path) -> Result<PathBuf, EntryDefect> {
    expand_against(entry, ocx_env::home_dir().as_deref())
}

/// [`expanded_entry`] against an explicit home directory.
///
/// Only a leading `~` component expands: `/w/~/dev` names a directory literally called `~`.
pub(crate) fn expand_against(entry: &Path, home: Option<&Path>) -> Result<PathBuf, EntryDefect> {
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

/// The `OCX_CONSENT_*` env channel's contribution, unioned with the config tiers; never an
/// error, since no channel may break a prompt.
///
/// Empty tokens are discarded, or a trailing comma would consent to every namespace.
/// One malformed namespace pattern discards the whole namespace contribution, never part of it.
#[must_use]
pub fn env_channel(paths: Option<&str>, namespaces: Option<&str>) -> ShellConsent {
    ShellConsent {
        paths: paths.map(parse_consent_paths).unwrap_or_default(),
        namespaces: namespaces.and_then(parse_consent_namespaces),
    }
}

/// Split `value` on the OS PATH separator, dropping empty tokens, which would normalize toward a
/// root.
///
/// Surviving bytes stay verbatim: trimming would rename a legitimate directory.
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
/// the `OCX_CONSENT_*` env channel.
///
/// `OCX_NO_CONFIG=1` does **not** prune this — it empties the discovered chain
/// and suppresses the managed fold, but touches neither the explicit tiers nor
/// the env channel. Only `OCX_NO_HOOK=1` makes a shell wholly inert.
#[must_use]
pub fn effective_consent(configured: Option<&ShellConfig>) -> ShellConsent {
    let mut consent = configured.and_then(|shell| shell.consent.clone()).unwrap_or_default();
    consent.merge(env_channel(
        ocx_env::OCX_CONSENT_PATHS.get().as_deref(),
        ocx_env::OCX_CONSENT_NAMESPACES.get().as_deref(),
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
    /// asserting the defect deliberately do not. Case is preserved: the
    /// path-normalization rows turn on it.
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

    // ── the deny_unknown_fields split ───────────────────────────────────────

    /// The tolerance split is the contract. `[shell]` keeps fleet
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

    /// Named red state: a `namespaces` table
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

    // ── the grammar, at parse ───────────────────────────────────────────────

    /// The two accepted spellings, and every rejected
    /// class. One row per form so a regression names which one moved.
    /// EC-GRANT-003, EC-GRANT-004, EC-GRANT-005, EC-GRANT-006 —
    /// the accepted spellings, the rejected `*/*`, whole-registry and bare-host
    /// forms, and the uppercase class the grammar makes unmatchable.
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

    /// `ocx.sh/acme/*` and `ocx.sh/acme` match the identical set, because
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

    /// There is no whole-registry grant, in either
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

    /// `{ include = [], exclude = [...] }` and a table naming
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

    /// Carve-outs are at source granularity — an org subtracted from a
    /// multi-org include. The repository-granularity spelling is refused.
    ///
    /// The carve-out used to be demonstrated against a whole-registry include;
    /// that spelling is gone, so the subtraction is shown where
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

    /// `paths` entries normalize separators and a trailing slash — and,
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
        match ocx_env::home_dir() {
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

    // ── the env channel ─────────────────────────────────────────────────────

    /// Empty tokens are dropped **before**
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
                "'{value}' leaked an empty pattern, which ocx_trust::pattern_matches reads as a catch-all"
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

    /// An empty token must never become an empty `PathBuf`, which
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

    /// The subtree form's primary use case: a devcontainer or CI image
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

    /// A single malformed non-empty pattern discards the **whole**
    /// contribution with no error — a hard error would break every prompt.
    #[test]
    fn c031_env_channel_discards_the_whole_contribution_on_one_bad_pattern() {
        let consent = env_channel(None, Some("ocx.sh/acme,ocx.sh/acme-corp*"));
        assert!(
            consent.namespaces.is_none(),
            "a malformed pattern must discard the whole contribution, never partially parse"
        );
    }

    // ── merge semantics + tier provenance ───────────────────────────────────

    /// `hook` and `completions` are scalar-wins-if-`Some` in both
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

    /// `paths` append and `namespaces` accumulate — no tier overrides
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

    /// The same union in the other tier order produces the same spec —
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

    // ── the env channel unions with the config tiers ────────────────────────

    /// [`effective_consent`] is the config tiers plus the env
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

    // ── modify_path / profiles ───────────────────────────────────────────────

    /// The fleet-wide "may setup touch PATH" toggle parses, and absent stays
    /// absent — modelled on `rekor_upload_parses_and_absent_stays_absent`
    /// (`trust.rs`), the canonical shape for an `Option<bool>` in this repo.
    #[test]
    fn modify_path_parses_and_absent_stays_absent() {
        let configured = parse("modify_path = true\n").expect("the field parses");
        assert_eq!(configured.modify_path, Some(true));

        let absent = parse("hook = true\n").expect("[shell] parses without the field");
        assert_eq!(absent.modify_path, None);
    }

    /// `profiles` parses to a list, and absent stays absent — same shape as
    /// [`modify_path_parses_and_absent_stays_absent`].
    #[test]
    fn profiles_parses_and_absent_stays_absent() {
        let configured = parse("profiles = [\"/a\", \"/b\"]\n").expect("the field parses");
        assert_eq!(
            configured.profiles,
            Some(vec![PathBuf::from("/a"), PathBuf::from("/b")])
        );

        let absent = parse("hook = true\n").expect("[shell] parses without the field");
        assert_eq!(absent.profiles, None);
    }

    /// Absent and empty are different answers to different questions: absent
    /// means auto-detect fresh on every run, `profiles = []` means write no
    /// profile blocks at all. Collapsing them into a bare `Vec<PathBuf>` would
    /// erase that distinction.
    ///
    /// Red state: change `profiles` to `#[serde(default)] Vec<PathBuf>` — both
    /// branches below then observe `vec![]` for the absent case too, and the
    /// second assertion fails to distinguish it from the explicit empty list.
    #[test]
    fn profiles_distinguishes_absent_from_empty() {
        let absent = parse("hook = true\n").expect("[shell] parses without the field");
        assert_eq!(
            absent.profiles, None,
            "no key at all means auto-detect, not an empty list"
        );

        let empty = parse("profiles = []\n").expect("an empty list parses");
        assert_eq!(
            empty.profiles,
            Some(Vec::new()),
            "an explicit empty list means write no profile blocks, and must stay Some"
        );
    }

    /// `merge` treats `profiles` the same way `hook` treats a scalar: the
    /// higher tier wins only when it actually set something, and a whole-list
    /// replace, never a union — a tier naming explicit profiles is excluding
    /// every other one, so folding a lower tier's entries back in would defeat
    /// the key's purpose.
    ///
    /// Two red states: (1) drop the `is_some()` guard on the `profiles` arm —
    /// the `None`-leaves-intact case below then fails, since a higher tier's
    /// absent value would clear the lower tier's list; (2) make the arm extend
    /// (`self.profiles.get_or_insert_with(Vec::new).extend(...)`) instead of
    /// replace — the `Some`-replaces case below then fails, since the lower
    /// tier's entry would survive alongside the higher tier's.
    #[test]
    fn merge_is_per_key_and_replaces_never_unions() {
        let mut lower = parse("profiles = [\"/a\"]\n").expect("parses");

        // A higher tier that never mentions `profiles` leaves the lower
        // tier's list intact.
        let silent_higher = parse("hook = true\n").expect("parses");
        lower.merge(silent_higher);
        assert_eq!(
            lower.profiles,
            Some(vec![PathBuf::from("/a")]),
            "an unset higher tier must not clear the lower tier's list"
        );

        // A higher tier that does set `profiles` replaces the lower tier's
        // list wholesale — the lower tier's `/a` must not survive alongside
        // the higher tier's `/b`.
        let naming_higher = parse("profiles = [\"/b\"]\n").expect("parses");
        lower.merge(naming_higher);
        assert_eq!(
            lower.profiles,
            Some(vec![PathBuf::from("/b")]),
            "a higher tier naming explicit profiles must replace, never union with, the lower tier's list"
        );
    }
}
