// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-traffic-host mirror configuration (`[mirrors."<host>"]`).
//!
//! Home for all `[mirrors]` settings. Each entry maps a canonical upstream
//! **traffic host** (e.g. `"ghcr.io"`, `"index.ocx.sh"`) to replacement
//! endpoints — a corporate artifact-manager remote/proxy repository — so OCX
//! routes read traffic to the mirror instead of the firewall-blocked origin.
//! The canonical identifier, content-addressed digest, and every on-disk path
//! stay keyed to the upstream host; only the transport reference is
//! rewritten.
//!
//! Since `adr_index_indirection.md` F5b, a `[mirrors]` entry is a **union
//! value**, not a fixed `{url}` table: a bare string rewrites both traffic
//! roles for that host (the common single-role host), while a
//! `{registry?, index?}` table splits per role — `registry` rewrites OCI
//! distribution traffic (`/v2`), `index` rewrites index-tree traffic
//! (`/config.json`, `/c`, `/p`). The two roles are path-disjoint, so the same
//! host may co-serve both. The registry role feeds
//! [`MirrorMap`](crate::oci::MirrorMap) (the OCI client read path) ONLY; the
//! index role feeds the index client's base-URL resolution (a later work
//! package) ONLY — never the other way around.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;

/// Minimal shape shared by [`toml::Value`] and [`serde_json::Value`] so
/// [`parse_mirror_value`] can branch on "string vs. table" **once**, then
/// reuse the same branch logic for a TOML `[mirrors]` table entry and a
/// forwarded `OCX_MIRRORS` JSON per-host value.
///
/// Deliberately **not** a `#[serde(untagged)]` enum: an untagged derive
/// reports only a generic "data did not match any variant" error on a
/// malformed entry (the class of opaque error cargo#12574 moved away from) —
/// this hand-rolled, value-first shape lets [`parse_mirror_value`] raise a
/// named, field-specific [`MirrorConfigError`] instead.
pub trait MirrorValueShape {
    /// The string value if this is a bare-string entry (`"https://..."`,
    /// which rewrites both traffic roles for the host).
    fn as_mirror_str(&self) -> Option<&str>;

    /// A named field's string value if this is a table entry
    /// (`{registry = "...", index = "..."}`).
    ///
    /// `Ok(None)` when the field is genuinely absent (that role was simply
    /// not declared); `Err(type_name)` when the field key IS present but its
    /// value is not a string — the two cases resolve to different
    /// [`MirrorConfigError`] variants downstream (an absent role never
    /// errors by itself; a present-but-wrong-typed role is always
    /// [`MirrorConfigError::NonStringRoleValue`]), so a single collapsed
    /// `Option` cannot carry the distinction [`parse_mirror_value`] needs.
    fn mirror_field(&self, field: &str) -> Result<Option<&str>, &'static str>;

    /// `true` when this value is a table (the `{registry?, index?}` shape),
    /// `false` for a bare string or any other shape.
    fn is_mirror_table(&self) -> bool;

    /// Short, human-readable type name for an unrecognized shape (e.g.
    /// `"integer"`, `"boolean"`, `"array"`), used to build
    /// [`MirrorConfigError::InvalidShape`] / [`MirrorConfigError::NonStringRoleValue`].
    fn mirror_type_name(&self) -> &'static str;
}

impl MirrorValueShape for toml::Value {
    fn as_mirror_str(&self) -> Option<&str> {
        self.as_str()
    }

    fn mirror_field(&self, field: &str) -> Result<Option<&str>, &'static str> {
        let Some(table) = self.as_table() else {
            return Ok(None);
        };
        match table.get(field) {
            None => Ok(None),
            Some(value) => value.as_str().map(Some).ok_or_else(|| value.type_str()),
        }
    }

    fn is_mirror_table(&self) -> bool {
        self.is_table()
    }

    fn mirror_type_name(&self) -> &'static str {
        self.type_str()
    }
}

impl MirrorValueShape for serde_json::Value {
    fn as_mirror_str(&self) -> Option<&str> {
        self.as_str()
    }

    fn mirror_field(&self, field: &str) -> Result<Option<&str>, &'static str> {
        let Some(object) = self.as_object() else {
            return Ok(None);
        };
        match object.get(field) {
            None => Ok(None),
            Some(value) => value.as_str().map(Some).ok_or_else(|| json_value_type_name(value)),
        }
    }

    fn is_mirror_table(&self) -> bool {
        self.is_object()
    }

    fn mirror_type_name(&self) -> &'static str {
        json_value_type_name(self)
    }
}

/// Short, human-readable type name for a [`serde_json::Value`] — the JSON
/// analog of [`toml::Value::type_str`] (which the `toml` crate provides
/// natively, `serde_json` does not).
fn json_value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Normalized `[mirrors."<host>"]` entry: one independent optional endpoint
/// per traffic role.
///
/// Re-exported from [`crate::config`] as `MirrorConfig`. Never derives
/// [`serde::Deserialize`] directly — the string-or-table union value it is
/// built from is parsed by [`parse_mirror_value`] (fed from
/// [`deserialize_mirrors_table`] on the TOML side and
/// [`crate::env::mirrors`] on the forwarded-env side), never by a derive on
/// this struct.
#[derive(Debug, Default, Clone, PartialEq, Eq, schemars::JsonSchema)]
pub struct MirrorConfig {
    /// Endpoint for OCI distribution traffic (`/v2`) to this host, if a
    /// registry role was declared for it (a bare string, `{registry: ...}`,
    /// or `{registry: ..., index: ...}`). Feeds
    /// [`MirrorMap`](crate::oci::MirrorMap) only.
    pub registry: Option<String>,

    /// Endpoint for index-tree traffic (`/config.json`, `/c`, `/p`) to this
    /// host, if an index role was declared for it. Feeds the index client's
    /// base-URL resolution only (a later work package).
    pub index: Option<String>,

    /// Runtime provenance marker: the `registry` role of this entry was
    /// declared at the SYSTEM config scope (`/etc/ocx/config.toml`), so it is
    /// NON-OVERRIDABLE by any lower tier. Independent of
    /// [`Self::index_system_locked`] — a system entry may declare only one
    /// role, leaving the other role open for a lower tier to set (F5b: the
    /// two roles are independent fields on one entry).
    ///
    /// Never serialized — set by the loader via [`Self::lock_as_system`]
    /// after parsing the system-scope file, not read from disk.
    #[schemars(skip)]
    pub registry_system_locked: bool,

    /// Runtime provenance marker: the `index` role of this entry was
    /// declared at the SYSTEM config scope. See
    /// [`Self::registry_system_locked`] for the per-role locking rationale.
    #[schemars(skip)]
    pub index_system_locked: bool,
}

/// A parsed mirror endpoint: scheme split out as `protocol`, the host, and the
/// optional repository-key path prefix.
///
/// Produced by [`parse_url`] in the config layer so the OCI client receives an
/// already-parsed value and never imports [`crate::config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMirror {
    /// URL scheme without the `://` separator, e.g. `"https"` or `"http"`.
    pub protocol: String,
    /// Mirror host (and optional port), e.g. `"company.jfrog.io"`.
    pub host: String,
    /// Repository-key path prefix, verbatim, with no leading or trailing `/`.
    /// Empty for a host-only (subdomain-method) mirror.
    pub path_prefix: String,
}

/// Error raised while parsing a mirror `url` into a [`ParsedMirror`]
/// ([`parse_url`]), while parsing a raw `[mirrors."<host>"]` union value into
/// a [`MirrorConfig`] ([`parse_mirror_value`]), or while resolving the
/// per-host mirror map in [`resolve_mirror_map`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MirrorConfigError {
    /// A role's `url` was absent or empty — a no-op mirror that would silently
    /// egress to the blocked upstream host.
    #[error("mirror url is missing or empty")]
    MissingUrl,
    /// A role's `url` had no host segment after stripping the scheme.
    #[error("mirror url has no host: {url}")]
    MissingHost {
        /// The offending `url` value.
        url: String,
    },
    /// A `[mirrors."<upstream>"]` role's `url` could not be parsed. Carries
    /// the upstream host key for context and the underlying parse error as
    /// source.
    #[error("invalid [mirrors.\"{upstream}\"] configuration")]
    InvalidEntry {
        /// The upstream host key whose mirror entry failed to parse.
        upstream: String,
        /// The underlying parse failure.
        #[source]
        source: Box<MirrorConfigError>,
    },
    /// A `[mirrors."<upstream>"]` entry declares neither a `registry` nor an
    /// `index` endpoint (an empty table, or a merged entry every tier left
    /// empty) — the union analog of a no-op mirror: silently allowing direct
    /// egress to the upstream host for every role.
    #[error(
        "mirrors.\"{upstream}\" declares neither a registry nor an index endpoint; a mirror entry \
         with no role would silently allow direct egress to the upstream host"
    )]
    EmptyEntry {
        /// The upstream host key whose mirror entry is empty.
        upstream: String,
    },
    /// A `[mirrors."<upstream>"]` value was neither a plain string nor a
    /// `{registry?, index?}` table.
    #[error("mirrors.\"{upstream}\" expected a string or {{registry?, index?}} table, found {found}")]
    InvalidShape {
        /// The upstream host key whose mirror value has an unrecognized shape.
        upstream: String,
        /// Human-readable type name of the offending value (e.g. `"integer"`).
        found: String,
    },
    /// A `[mirrors."<upstream>"]` table's `registry` or `index` field was
    /// present but not a string.
    #[error("mirrors.\"{upstream}\" field '{role}' expected a string, found {found}")]
    NonStringRoleValue {
        /// The upstream host key whose role field has a non-string value.
        upstream: String,
        /// The offending field name (`"registry"` or `"index"`).
        role: &'static str,
        /// Human-readable type name of the offending value (e.g. `"integer"`).
        found: String,
    },
    /// A mirror endpoint uses plain HTTP but its host is not listed in
    /// `OCX_INSECURE_REGISTRIES`. Replace semantics forbid silently downgrading
    /// to HTTP, so this fails loud at resolve time with an actionable hint
    /// (CWE-319) rather than as an opaque TLS error mid-transport.
    #[error(
        "mirror for '{upstream}' uses http:// but mirror host '{mirror_host}' is not in \
         OCX_INSECURE_REGISTRIES; add it to allow plain-HTTP transport"
    )]
    PlainHttpMirrorNotAllowed {
        /// The upstream host key whose mirror uses plain HTTP.
        upstream: String,
        /// The mirror host that would be contacted over plain HTTP.
        mirror_host: String,
    },
    /// The forwarded `OCX_MIRRORS` env value was not valid JSON. A malformed
    /// forwarded map is a hard error: silently degrading to an identity map
    /// would route reads to the firewall-blocked origin instead of the mirror.
    #[error("malformed OCX_MIRRORS env value")]
    MalformedEnvJson {
        /// The underlying JSON parse failure.
        #[source]
        source: serde_json::Error,
    },
}

impl MirrorConfig {
    /// Mark this entry as system-locked — non-overridable by lower tiers for
    /// whichever role(s) it declares.
    ///
    /// Called by the config loader on each entry of the system-scope file's
    /// (`/etc/ocx/config.toml`) `[mirrors]` table, after parsing and before
    /// folding higher tiers in. **Per-role**, unlike
    /// [`RegistryConfig::lock_as_system`](crate::config::RegistryConfig::lock_as_system):
    /// only the role(s) this entry actually sets (`registry.is_some()` /
    /// `index.is_some()`) become non-overridable — a system entry that
    /// declares only `registry` leaves `index` open for a lower tier to set
    /// (F5b: the two roles are independent fields on one entry).
    pub fn lock_as_system(&mut self) {
        if self.registry.is_some() {
            self.registry_system_locked = true;
        }
        if self.index.is_some() {
            self.index_system_locked = true;
        }
    }

    /// Merge `other` into `self`, field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    ///
    /// Each role merges independently, honoring its own lock: `registry`
    /// merges under [`Self::registry_system_locked`], `index` merges under
    /// [`Self::index_system_locked`] — a system entry locking only one role
    /// still lets a lower tier set the other (F5b).
    pub fn merge(&mut self, other: MirrorConfig) {
        if !self.registry_system_locked && other.registry.is_some() {
            self.registry = other.registry;
        }
        if !self.index_system_locked && other.index.is_some() {
            self.index = other.index;
        }
    }
}

/// Parses a raw `[mirrors."<host>"]` union value (a bare string, or a
/// `{registry?, index?}` table) into a normalized [`MirrorConfig`].
///
/// The single shared branch backing both `[mirrors]` TOML-table entries
/// ([`deserialize_mirrors_table`]) and forwarded `OCX_MIRRORS` JSON per-host
/// entries ([`crate::env::mirrors`]) — fed [`toml::Value`] from the former and
/// [`serde_json::Value`] from the latter, generic over [`MirrorValueShape`] so
/// neither caller needs a second copy of the branch logic.
///
/// # Errors
///
/// Returns [`MirrorConfigError::InvalidShape`] when `value` is neither a
/// string nor a table, [`MirrorConfigError::NonStringRoleValue`] when a table
/// field is present but not a string, and [`MirrorConfigError::EmptyEntry`]
/// when a table declares neither `registry` nor `index`.
pub fn parse_mirror_value<V: MirrorValueShape>(upstream: &str, value: &V) -> Result<MirrorConfig, MirrorConfigError> {
    // Bare string: both traffic roles rewrite to the same endpoint.
    if let Some(url) = value.as_mirror_str() {
        return Ok(MirrorConfig {
            registry: Some(url.to_string()),
            index: Some(url.to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        });
    }

    if !value.is_mirror_table() {
        return Err(MirrorConfigError::InvalidShape {
            upstream: upstream.to_string(),
            found: value.mirror_type_name().to_string(),
        });
    }

    let mut config = MirrorConfig::default();
    for role in ["registry", "index"] {
        match value.mirror_field(role) {
            Ok(Some(url)) => {
                if role == "registry" {
                    config.registry = Some(url.to_string());
                } else {
                    config.index = Some(url.to_string());
                }
            }
            Ok(None) => {}
            Err(found) => {
                return Err(MirrorConfigError::NonStringRoleValue {
                    upstream: upstream.to_string(),
                    role,
                    found: found.to_string(),
                });
            }
        }
    }

    if config.registry.is_none() && config.index.is_none() {
        return Err(MirrorConfigError::EmptyEntry {
            upstream: upstream.to_string(),
        });
    }

    Ok(config)
}

/// Deserializes the `[mirrors]` TOML table into normalized [`MirrorConfig`]
/// entries, keyed by upstream host.
///
/// Wired onto [`crate::config::Config::mirrors`] via `#[serde(deserialize_with
/// = "mirror::deserialize_mirrors_table")]` instead of a plain derive: each
/// per-host TOML value is read as a generic [`toml::Value`] first (this
/// function has the map-key context [`parse_mirror_value`] itself lacks), so a
/// malformed entry raises a **named** per-host error via
/// [`MirrorConfigError::InvalidEntry`]-style wrapping instead of an opaque
/// "data did not match any variant".
pub fn deserialize_mirrors_table<'de, D>(deserializer: D) -> Result<Option<HashMap<String, MirrorConfig>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<HashMap<String, toml::Value>>::deserialize(deserializer)?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    let mut result = HashMap::with_capacity(raw.len());
    for (host, value) in raw {
        let config = parse_mirror_value(&host, &value).map_err(serde::de::Error::custom)?;
        result.insert(host, config);
    }
    Ok(Some(result))
}

/// Parses a mirror endpoint `url` into a [`ParsedMirror`].
///
/// A free function (not a method — a [`MirrorConfig`] entry may declare two
/// role URLs, so there is no single `self.url` to parse) so
/// [`resolve_mirror_map`] can call it once per declared role.
///
/// Dedicated split: strip the scheme into `protocol`, take the first `/`
/// boundary as the host/path-prefix split, keep the remainder verbatim as
/// the path prefix, and trim one trailing `/`. Deliberately does **not**
/// reuse `auth::registry_url::canonicalize_registry`, whose `/vN` strip and
/// `docker.io` special-case would corrupt a repo-key prefix or a
/// `docker.io` mirror host.
///
/// # Errors
///
/// Returns [`MirrorConfigError::MissingUrl`] when `url` is empty, and
/// [`MirrorConfigError::MissingHost`] when the `url` has no host segment.
pub fn parse_url(url: &str) -> Result<ParsedMirror, MirrorConfigError> {
    if url.is_empty() {
        return Err(MirrorConfigError::MissingUrl);
    }

    // 1. Strip the scheme into `protocol`, normalized to ASCII-lowercase so a
    //    mixed-case scheme (`HTTP://`) cannot bypass the plain-HTTP gate
    //    downstream, which compares against lowercase `"http"` (CWE-319 — the
    //    `url` crate lowercases the scheme on the wire, so an un-normalized
    //    `"HTTP"` would gate as "not http" yet egress plaintext). Default to
    //    https when no scheme is present (matches the registry-auth convention
    //    without pulling in `canonicalize_registry`'s /vN and docker.io
    //    special-cases).
    let (protocol, rest) = match url.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("https".to_string(), url),
    };

    // 2. First `/` boundary splits host from the verbatim path prefix.
    let (host, raw_prefix) = match rest.split_once('/') {
        Some((host, prefix)) => (host, prefix),
        None => (rest, ""),
    };

    if host.is_empty() {
        return Err(MirrorConfigError::MissingHost { url: url.to_string() });
    }

    // 3. Trim exactly one trailing `/` from the prefix (verbatim otherwise).
    let path_prefix = raw_prefix.strip_suffix('/').unwrap_or(raw_prefix);

    Ok(ParsedMirror {
        protocol,
        host: host.to_string(),
        path_prefix: path_prefix.to_string(),
    })
}

/// Per-role, per-purpose output of [`resolve_mirror_map`].
#[derive(Debug, Clone, Default)]
pub struct ResolvedMirrors {
    /// Parsed registry-role (OCI distribution, `/v2`) mirror endpoints, keyed
    /// by upstream traffic host — feeds
    /// [`MirrorMap::new`](crate::oci::MirrorMap::new) on the client side.
    /// Never carries an index-only entry.
    pub registry: BTreeMap<String, ParsedMirror>,
    /// Parsed index-role (index-tree traffic: `/config.json`, `/c`, `/p`)
    /// mirror endpoints, keyed by upstream traffic host — feeds the index
    /// client's base-URL resolution (a later work package). Never carries a
    /// registry-only entry.
    pub index: BTreeMap<String, ParsedMirror>,
    /// The merged (`[mirrors]` config union the inherited `OCX_MIRRORS`, env
    /// wins per host) but not-yet-role-parsed entries, keyed by upstream
    /// host — forwarded verbatim via
    /// [`OcxConfigView::mirrors`](crate::env::OcxConfigView::mirrors) so a
    /// child ocx re-parses and re-validates the same map instead of
    /// inheriting pre-split [`ParsedMirror`] internals.
    pub merged: BTreeMap<String, MirrorConfig>,
}

/// Resolves the per-host mirror map from `[mirrors]` config merged with the
/// inherited `OCX_MIRRORS` env, then parses and validates every resolved
/// entry, splitting the result by traffic role.
///
/// This is the single Config→mirror-map transform shared by the CLI
/// (`Context::try_init`) and the OCI client (`ClientBuilder::from_env*`). It
/// owns the one-way-door precedence rule and the plain-HTTP security gate so
/// they cannot drift between layers:
///
/// - **Per-host merge** — `env_entries` (the inherited, already-normalized
///   `OCX_MIRRORS`) win per-host key; a host present only in `[mirrors]`
///   config survives. Order is stable (a [`BTreeMap`]), so forwarding to a
///   child ocx is deterministic.
/// - **Parse + validate** — each merged entry's declared role(s) are parsed
///   via [`parse_url`]; a missing/empty role `url` is a hard error (F9: a
///   no-op mirror silently egresses to the blocked upstream host); an entry
///   with no role at all is [`MirrorConfigError::EmptyEntry`].
/// - **Plain-HTTP gate (CWE-319)** — an `http://` mirror whose host is not in
///   `insecure_hosts` is a hard error naming `OCX_INSECURE_REGISTRIES`, so the
///   failure surfaces loud at resolve time rather than as an opaque TLS error
///   mid-transport.
/// - **Role-only filtering** — an index-only entry never appears in
///   [`ResolvedMirrors::registry`], and a registry-only entry never appears in
///   [`ResolvedMirrors::index`]: an index-only mirror must NOT rewrite OCI
///   traffic, and vice versa.
///
/// # Errors
///
/// Returns [`MirrorConfigError::InvalidEntry`] when a configured mirror role's
/// `url` is missing/empty or has no host, [`MirrorConfigError::EmptyEntry`]
/// when an entry declares neither role, and
/// [`MirrorConfigError::PlainHttpMirrorNotAllowed`] when a mirror uses plain
/// HTTP without its host in `insecure_hosts`.
pub fn resolve_mirror_map(
    config: &crate::config::Config,
    env_entries: Vec<(String, MirrorConfig)>,
    insecure_hosts: &[String],
) -> Result<ResolvedMirrors, MirrorConfigError> {
    // Per-host merge: config first, then env entries win per host key
    // (whole-value replace — an env entry is already a fully-merged
    // `MirrorConfig`, not something to field-merge against).
    let mut merged: BTreeMap<String, MirrorConfig> = config
        .mirrors
        .iter()
        .flatten()
        .map(|(host, entry)| (host.clone(), entry.clone()))
        .collect();
    for (host, entry) in env_entries {
        merged.insert(host, entry);
    }

    let mut registry = BTreeMap::new();
    let mut index = BTreeMap::new();

    for (host, entry) in &merged {
        if entry.registry.is_none() && entry.index.is_none() {
            return Err(MirrorConfigError::EmptyEntry { upstream: host.clone() });
        }
        if let Some(url) = &entry.registry {
            registry.insert(host.clone(), resolve_mirror_role(host, url, insecure_hosts)?);
        }
        if let Some(url) = &entry.index {
            index.insert(host.clone(), resolve_mirror_role(host, url, insecure_hosts)?);
        }
    }

    Ok(ResolvedMirrors {
        registry,
        index,
        merged,
    })
}

/// Parses and validates a single declared role `url` for `upstream`: wraps a
/// [`parse_url`] failure in [`MirrorConfigError::InvalidEntry`] naming the
/// upstream host, then enforces the plain-HTTP gate (CWE-319).
fn resolve_mirror_role(
    upstream: &str,
    url: &str,
    insecure_hosts: &[String],
) -> Result<ParsedMirror, MirrorConfigError> {
    let parsed = parse_url(url).map_err(|source| MirrorConfigError::InvalidEntry {
        upstream: upstream.to_string(),
        source: Box::new(source),
    })?;
    if parsed.protocol == "http" && !insecure_hosts.contains(&parsed.host) {
        return Err(MirrorConfigError::PlainHttpMirrorNotAllowed {
            upstream: upstream.to_string(),
            mirror_host: parsed.host,
        });
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_config_merge_none_in_higher_does_not_clobber_lower_url() {
        let mut lower = MirrorConfig {
            registry: Some("https://company.jfrog.io/ghcr-remote".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        let higher = MirrorConfig {
            registry: None,
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_deref(),
            Some("https://company.jfrog.io/ghcr-remote"),
            "None in higher should not clobber lower's Some(registry)"
        );
    }

    #[test]
    fn mirror_config_merge_some_in_higher_overrides_lower_url() {
        let mut lower = MirrorConfig {
            registry: Some("https://old.example/remote".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        let higher = MirrorConfig {
            registry: Some("https://new.example/remote".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_deref(),
            Some("https://new.example/remote"),
            "Some in higher should override lower's registry"
        );
    }

    // ── MirrorConfig system lock (per role, F5b) ─────────────────────────────

    /// `lock_as_system` locks only the role(s) this entry declares — a
    /// registry-only entry must NOT lock a not-yet-declared `index` role.
    #[test]
    fn mirror_config_lock_as_system_sets_locked() {
        let mut mirror = MirrorConfig {
            registry: Some("https://mirror.corp/ghcr-remote".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        mirror.lock_as_system();
        assert!(
            mirror.registry_system_locked,
            "lock_as_system must lock the declared registry role"
        );
    }

    /// A system-locked `MirrorConfig` entry ignores a lower-tier override on
    /// its locked role; the lock flag stays sticky after merge.
    #[test]
    fn mirror_config_merge_system_locked_ignores_lower_tier() {
        let mut system = MirrorConfig {
            registry: Some("https://system-mirror.corp/ghcr-remote".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        system.lock_as_system();
        assert!(system.registry_system_locked);

        let user = MirrorConfig {
            registry: Some("https://user-mirror.corp/ghcr-remote".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        system.merge(user);

        assert_eq!(
            system.registry.as_deref(),
            Some("https://system-mirror.corp/ghcr-remote"),
            "locked system mirror registry must not be redirected by a lower tier"
        );
        assert!(system.registry_system_locked, "lock flag stays sticky after merge");
    }

    // ── MirrorConfig tier-merge matrix (Url × Split × system-lock, F5b) ──────

    /// Url × Url: both tiers declare a bare-string entry (both roles set to
    /// the same URL) — the higher tier's URL wins for BOTH roles.
    #[test]
    fn mirror_config_merge_matrix_url_times_url() {
        let mut lower = MirrorConfig {
            registry: Some("https://old.corp/both".to_string()),
            index: Some("https://old.corp/both".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        let higher = MirrorConfig {
            registry: Some("https://new.corp/both".to_string()),
            index: Some("https://new.corp/both".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_deref(), Some("https://new.corp/both"));
        assert_eq!(lower.index.as_deref(), Some("https://new.corp/both"));
    }

    /// Url × Split: the lower tier is a bare-string entry (both roles equal);
    /// the higher tier is a split entry declaring only `index`. The `index`
    /// role is overridden field-wise while `registry` — untouched by the
    /// higher tier — survives from the lower tier.
    #[test]
    fn mirror_config_merge_matrix_url_times_split() {
        let mut lower = MirrorConfig {
            registry: Some("https://old.corp/both".to_string()),
            index: Some("https://old.corp/both".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        let higher = MirrorConfig {
            registry: None,
            index: Some("https://new.corp/index-only".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(
            lower.registry.as_deref(),
            Some("https://old.corp/both"),
            "registry role untouched by the higher tier must survive from the lower tier"
        );
        assert_eq!(lower.index.as_deref(), Some("https://new.corp/index-only"));
    }

    /// Split × Url: the lower tier is a split entry declaring only
    /// `registry`; the higher tier is a bare-string entry (both roles equal).
    /// Both roles end up set from the higher tier — `registry` overridden,
    /// `index` newly populated.
    #[test]
    fn mirror_config_merge_matrix_split_times_url() {
        let mut lower = MirrorConfig {
            registry: Some("https://old.corp/registry-only".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        let higher = MirrorConfig {
            registry: Some("https://new.corp/both".to_string()),
            index: Some("https://new.corp/both".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_deref(), Some("https://new.corp/both"));
        assert_eq!(lower.index.as_deref(), Some("https://new.corp/both"));
    }

    /// Split × Split: each tier declares a different, non-overlapping role.
    /// The merge result carries both roles, each sourced from its own tier.
    #[test]
    fn mirror_config_merge_matrix_split_times_split() {
        let mut lower = MirrorConfig {
            registry: Some("https://old.corp/registry-only".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        let higher = MirrorConfig {
            registry: None,
            index: Some("https://new.corp/index-only".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        lower.merge(higher);
        assert_eq!(lower.registry.as_deref(), Some("https://old.corp/registry-only"));
        assert_eq!(lower.index.as_deref(), Some("https://new.corp/index-only"));
    }

    /// System-lock orientation 1: a system entry declaring (and thus locking)
    /// only `registry` still lets a bare-string overlay (which sets BOTH
    /// roles) merge its `index` role, while `registry` ignores the overlay.
    #[test]
    fn mirror_config_merge_registry_locked_ignores_overlay_but_index_role_still_merges() {
        let mut system = MirrorConfig {
            registry: Some("https://system-mirror.corp/registry-side".to_string()),
            index: None,
            registry_system_locked: false,
            index_system_locked: false,
        };
        system.lock_as_system();
        assert!(
            system.registry_system_locked,
            "declaring only registry must lock only the registry role"
        );
        assert!(
            !system.index_system_locked,
            "index was not declared, so it must not be locked"
        );

        let overlay = MirrorConfig {
            registry: Some("https://user.corp/registry-side".to_string()),
            index: Some("https://user.corp/index-side".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        system.merge(overlay);

        assert_eq!(
            system.registry.as_deref(),
            Some("https://system-mirror.corp/registry-side"),
            "the locked registry role must ignore the bare-string overlay"
        );
        assert_eq!(
            system.index.as_deref(),
            Some("https://user.corp/index-side"),
            "the unlocked index role must still merge from the same overlay"
        );
    }

    /// System-lock orientation 2 (the mirror image of the above): a system
    /// entry declaring (and thus locking) only `index` still lets a
    /// bare-string overlay merge its `registry` role.
    #[test]
    fn mirror_config_merge_index_locked_ignores_overlay_but_registry_role_still_merges() {
        let mut system = MirrorConfig {
            registry: None,
            index: Some("https://system-mirror.corp/index-side".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        system.lock_as_system();
        assert!(
            system.index_system_locked,
            "declaring only index must lock only the index role"
        );
        assert!(
            !system.registry_system_locked,
            "registry was not declared, so it must not be locked"
        );

        let overlay = MirrorConfig {
            registry: Some("https://user.corp/registry-side".to_string()),
            index: Some("https://user.corp/index-side".to_string()),
            registry_system_locked: false,
            index_system_locked: false,
        };
        system.merge(overlay);

        assert_eq!(
            system.index.as_deref(),
            Some("https://system-mirror.corp/index-side"),
            "the locked index role must ignore the bare-string overlay"
        );
        assert_eq!(
            system.registry.as_deref(),
            Some("https://user.corp/registry-side"),
            "the unlocked registry role must still merge from the same overlay"
        );
    }

    // ── Step 3.1 specification tests ─────────────────────────────────────────

    /// `[mirrors] "ghcr.io" = "https://..."` (bare string, F5b: both roles)
    /// round-trips through TOML into a [`MirrorConfig`] with `registry` AND
    /// `index` both set to the same URL.
    #[test]
    fn mirror_config_toml_roundtrip() {
        let toml_str = "[mirrors]\n\"ghcr.io\" = \"https://company.jfrog.io/ghcr-remote\"\n";
        let config: crate::config::Config = toml::from_str(toml_str).expect("valid TOML must parse");
        let mirrors = config.mirrors.expect("mirrors table must be present");
        let cfg = mirrors.get("ghcr.io").expect("ghcr.io key must exist");
        assert_eq!(
            cfg.registry.as_deref(),
            Some("https://company.jfrog.io/ghcr-remote"),
            "a bare-string entry must set the registry role"
        );
        assert_eq!(
            cfg.index.as_deref(),
            Some("https://company.jfrog.io/ghcr-remote"),
            "a bare-string entry must ALSO set the index role (F5b: both roles)"
        );
    }

    /// An unrecognized field inside a `{registry?, index?}` table
    /// (e.g. `urll = "..."`) must be rejected — a typo in a known section
    /// fails fast, matching the `deny_unknown_fields` convention every other
    /// config table follows.
    #[test]
    fn mirror_config_unknown_field_rejected() {
        let toml_str = "[mirrors.\"ghcr.io\"]\nurll = \"https://company.jfrog.io/ghcr-remote\"\n";
        let result = toml::from_str::<crate::config::Config>(toml_str);
        assert!(
            result.is_err(),
            "unrecognized field 'urll' inside a [mirrors.\"<host>\"] table must be rejected, but parse succeeded"
        );
    }

    /// A malformed `[mirrors."<host>"]` entry (neither role declared — the
    /// same `EmptyEntry` shape `parse_mirror_value` raises directly) surfaces
    /// through the FULL `toml::from_str::<Config>` deserialize path with the
    /// upstream host still named in the error message, proving
    /// `deserialize_mirrors_table`'s `serde::de::Error::custom` wrapping does
    /// not lose the host that `parse_mirror_value` attached.
    #[test]
    fn mirror_config_deserialize_error_names_host_via_serde_custom() {
        let toml_str = "[mirrors.\"ghcr.io\"]\n";
        let result = toml::from_str::<crate::config::Config>(toml_str);
        let err = result.expect_err("an entry declaring neither role must be rejected");
        let message = err.to_string();
        assert!(
            message.contains("ghcr.io"),
            "serde::de::Error::custom wrapping must preserve the upstream host in the message; got: {message}"
        );
    }

    // ── parse_mirror_value: value-first shape matrix (F5b) ───────────────────
    //
    // `parse_mirror_value` is generic over `MirrorValueShape`, fed
    // `toml::Value` from a `[mirrors]` TOML table entry
    // ([`deserialize_mirrors_table`]) and `serde_json::Value` from a
    // forwarded `OCX_MIRRORS` JSON per-host value ([`crate::env::mirrors`]) —
    // one shared branch backing both. Every case below is exercised through
    // BOTH concrete value types to prove the branch logic does not diverge
    // between them (the whole point of the hand-rolled, value-first shape).
    //
    // Replaces the retired `parse_url_errors_on_none_url` (wp-b specify): the
    // old "no URL provided" case was a `parse_url` concern under the single-
    // `url`-field shape; under the F5b union it is a presence check one layer
    // up, in `resolve_mirror_map` (see `resolve_mirror_map_absent_role_yields_no_rewrite_for_that_role`
    // below), never a `parse_mirror_value` or `parse_url` error.

    /// A bare TOML string sets both roles to the same URL.
    #[test]
    fn parse_mirror_value_toml_string_sets_both_roles() {
        let value = toml::Value::String("https://company.jfrog.io/ghcr-remote".to_string());
        let config = parse_mirror_value("ghcr.io", &value).expect("a bare string must parse");
        assert_eq!(config.registry.as_deref(), Some("https://company.jfrog.io/ghcr-remote"));
        assert_eq!(config.index.as_deref(), Some("https://company.jfrog.io/ghcr-remote"));
    }

    /// A bare JSON string sets both roles to the same URL — the same shape
    /// `toml::Value` handles above, proving the shared branch behaves
    /// identically for both concrete value types.
    #[test]
    fn parse_mirror_value_json_string_sets_both_roles() {
        let value = serde_json::Value::String("https://company.jfrog.io/ghcr-remote".to_string());
        let config = parse_mirror_value("ghcr.io", &value).expect("a bare string must parse");
        assert_eq!(config.registry.as_deref(), Some("https://company.jfrog.io/ghcr-remote"));
        assert_eq!(config.index.as_deref(), Some("https://company.jfrog.io/ghcr-remote"));
    }

    /// A TOML table with only `registry` sets the registry role and leaves
    /// `index` unset.
    #[test]
    fn parse_mirror_value_toml_table_registry_only() {
        let mut table = toml::Table::new();
        table.insert(
            "registry".to_string(),
            toml::Value::String("https://mirror.corp/reg".to_string()),
        );
        let value = toml::Value::Table(table);
        let config = parse_mirror_value("ghcr.io", &value).expect("a registry-only table must parse");
        assert_eq!(config.registry.as_deref(), Some("https://mirror.corp/reg"));
        assert!(config.index.is_none());
    }

    /// The JSON analog of the above — a `{registry}` object sets only the
    /// registry role.
    #[test]
    fn parse_mirror_value_json_table_registry_only() {
        let value = serde_json::json!({"registry": "https://mirror.corp/reg"});
        let config = parse_mirror_value("ghcr.io", &value).expect("a registry-only object must parse");
        assert_eq!(config.registry.as_deref(), Some("https://mirror.corp/reg"));
        assert!(config.index.is_none());
    }

    /// A TOML table with only `index` sets the index role and leaves
    /// `registry` unset.
    #[test]
    fn parse_mirror_value_toml_table_index_only() {
        let mut table = toml::Table::new();
        table.insert(
            "index".to_string(),
            toml::Value::String("https://artifactory.corp/ocx-index".to_string()),
        );
        let value = toml::Value::Table(table);
        let config = parse_mirror_value("index.ocx.sh", &value).expect("an index-only table must parse");
        assert_eq!(config.index.as_deref(), Some("https://artifactory.corp/ocx-index"));
        assert!(config.registry.is_none());
    }

    /// The JSON analog of the above — a `{index}` object sets only the index
    /// role.
    #[test]
    fn parse_mirror_value_json_table_index_only() {
        let value = serde_json::json!({"index": "https://artifactory.corp/ocx-index"});
        let config = parse_mirror_value("index.ocx.sh", &value).expect("an index-only object must parse");
        assert_eq!(config.index.as_deref(), Some("https://artifactory.corp/ocx-index"));
        assert!(config.registry.is_none());
    }

    /// A TOML table declaring both `registry` and `index` (with distinct
    /// URLs) sets both roles independently.
    #[test]
    fn parse_mirror_value_toml_table_both_fields() {
        let mut table = toml::Table::new();
        table.insert(
            "registry".to_string(),
            toml::Value::String("https://mirror.corp/reg".to_string()),
        );
        table.insert(
            "index".to_string(),
            toml::Value::String("https://mirror.corp/idx".to_string()),
        );
        let value = toml::Value::Table(table);
        let config = parse_mirror_value("index.ocx.sh", &value).expect("a both-field table must parse");
        assert_eq!(config.registry.as_deref(), Some("https://mirror.corp/reg"));
        assert_eq!(config.index.as_deref(), Some("https://mirror.corp/idx"));
    }

    /// The JSON analog of the above — a `{registry, index}` object sets both
    /// roles independently.
    #[test]
    fn parse_mirror_value_json_table_both_fields() {
        let value = serde_json::json!({"registry": "https://mirror.corp/reg", "index": "https://mirror.corp/idx"});
        let config = parse_mirror_value("index.ocx.sh", &value).expect("a both-field object must parse");
        assert_eq!(config.registry.as_deref(), Some("https://mirror.corp/reg"));
        assert_eq!(config.index.as_deref(), Some("https://mirror.corp/idx"));
    }

    /// An empty TOML table declares neither role — `EmptyEntry` naming the
    /// upstream host.
    #[test]
    fn parse_mirror_value_toml_empty_table_is_empty_entry() {
        let value = toml::Value::Table(toml::Table::new());
        let err = parse_mirror_value("ghcr.io", &value).expect_err("an empty table must error");
        assert!(matches!(err, MirrorConfigError::EmptyEntry { ref upstream } if upstream == "ghcr.io"));
    }

    /// The JSON analog of the above — an empty object declares neither role.
    #[test]
    fn parse_mirror_value_json_empty_table_is_empty_entry() {
        let value = serde_json::json!({});
        let err = parse_mirror_value("ghcr.io", &value).expect_err("an empty object must error");
        assert!(matches!(err, MirrorConfigError::EmptyEntry { ref upstream } if upstream == "ghcr.io"));
    }

    /// A TOML table whose only key is unrecognized (a typo, e.g. `urll`)
    /// declares neither known role, so it degrades to the same `EmptyEntry`
    /// as a genuinely empty table — no unrecognized field is silently
    /// accepted as a role.
    #[test]
    fn parse_mirror_value_toml_unknown_key_alone_is_empty_entry() {
        let mut table = toml::Table::new();
        table.insert(
            "urll".to_string(),
            toml::Value::String("https://company.jfrog.io/ghcr-remote".to_string()),
        );
        let value = toml::Value::Table(table);
        let err = parse_mirror_value("ghcr.io", &value).expect_err("an unrecognized-only field must error");
        assert!(matches!(err, MirrorConfigError::EmptyEntry { ref upstream } if upstream == "ghcr.io"));
    }

    /// The JSON analog of the above.
    #[test]
    fn parse_mirror_value_json_unknown_key_alone_is_empty_entry() {
        let value = serde_json::json!({"urll": "https://company.jfrog.io/ghcr-remote"});
        let err = parse_mirror_value("ghcr.io", &value).expect_err("an unrecognized-only field must error");
        assert!(matches!(err, MirrorConfigError::EmptyEntry { ref upstream } if upstream == "ghcr.io"));
    }

    /// A TOML table's `registry` field is present but not a string —
    /// `NonStringRoleValue` names both the upstream host and the offending
    /// field (backend-first UX: both are asserted as substrings of the
    /// rendered `Display` message).
    #[test]
    fn parse_mirror_value_toml_non_string_role_value_names_host_and_role() {
        let mut table = toml::Table::new();
        table.insert("registry".to_string(), toml::Value::Integer(42));
        let value = toml::Value::Table(table);
        let err = parse_mirror_value("ghcr.io", &value).expect_err("a non-string role value must error");
        assert!(matches!(
            err,
            MirrorConfigError::NonStringRoleValue { ref upstream, role, .. }
                if upstream == "ghcr.io" && role == "registry"
        ));
        let message = err.to_string();
        assert!(
            message.contains("ghcr.io"),
            "message must name the upstream host; got: {message}"
        );
        assert!(
            message.contains("registry"),
            "message must name the offending field; got: {message}"
        );
    }

    /// The JSON analog of the above, using the `index` field.
    #[test]
    fn parse_mirror_value_json_non_string_role_value_names_host_and_role() {
        let value = serde_json::json!({"index": 42});
        let err = parse_mirror_value("index.ocx.sh", &value).expect_err("a non-string role value must error");
        assert!(matches!(
            err,
            MirrorConfigError::NonStringRoleValue { ref upstream, role, .. }
                if upstream == "index.ocx.sh" && role == "index"
        ));
        let message = err.to_string();
        assert!(
            message.contains("index.ocx.sh"),
            "message must name the upstream host; got: {message}"
        );
        assert!(
            message.contains("index"),
            "message must name the offending field; got: {message}"
        );
    }

    /// A TOML table's `registry` field is present but is an array (not an
    /// integer this time) — a second non-string-role-value shape through the
    /// SAME table branch, proving `NonStringRoleValue` fires for any
    /// non-string TOML value type, not just `Integer`.
    #[test]
    fn parse_mirror_value_toml_array_role_value_is_non_string_role_value() {
        let mut table = toml::Table::new();
        table.insert(
            "registry".to_string(),
            toml::Value::Array(vec![toml::Value::String("https://mirror.corp/reg".to_string())]),
        );
        let value = toml::Value::Table(table);
        let err = parse_mirror_value("ghcr.io", &value).expect_err("an array role value must error");
        assert!(matches!(
            err,
            MirrorConfigError::NonStringRoleValue { ref upstream, role, .. }
                if upstream == "ghcr.io" && role == "registry"
        ));
    }

    /// A TOML value that is neither a string nor a table — `InvalidShape`
    /// names the upstream host.
    #[test]
    fn parse_mirror_value_toml_non_string_non_table_is_invalid_shape() {
        let value = toml::Value::Integer(42);
        let err = parse_mirror_value("ghcr.io", &value).expect_err("a bare integer must error");
        assert!(matches!(err, MirrorConfigError::InvalidShape { ref upstream, .. } if upstream == "ghcr.io"));
        let message = err.to_string();
        assert!(
            message.contains("ghcr.io"),
            "message must name the upstream host; got: {message}"
        );
        assert!(
            message.contains("expected a string or"),
            "message must describe the expected shape; got: {message}"
        );
    }

    /// The JSON analog of the above — a bare JSON number is neither a string
    /// nor an object.
    #[test]
    fn parse_mirror_value_json_non_string_non_table_is_invalid_shape() {
        let value = serde_json::json!(42);
        let err = parse_mirror_value("ghcr.io", &value).expect_err("a bare number must error");
        assert!(matches!(err, MirrorConfigError::InvalidShape { ref upstream, .. } if upstream == "ghcr.io"));
        let message = err.to_string();
        assert!(
            message.contains("ghcr.io"),
            "message must name the upstream host; got: {message}"
        );
        assert!(
            message.contains("expected a string or"),
            "message must describe the expected shape; got: {message}"
        );
    }

    /// `url = ""` (empty string) → `parse_url` returns
    /// `MirrorConfigError::MissingUrl`.
    ///
    /// Traces: plan review F9 — empty url is the same footgun as absent url.
    #[test]
    fn parse_url_errors_on_empty_url() {
        let result = parse_url("");
        assert!(
            matches!(result, Err(MirrorConfigError::MissingUrl)),
            "url='' must yield MirrorConfigError::MissingUrl, got: {result:?}"
        );
    }

    /// `https://host/a/b/c` → protocol `https`, host `host`, prefix `a/b/c`.
    ///
    /// Traces: plan Testing Strategy — url split `https://h/a/b/c → (https,h,a/b/c)`.
    #[test]
    fn parse_url_splits_scheme_host_and_path_prefix() {
        let parsed = parse_url("https://host/a/b/c").expect("valid url must parse");
        assert_eq!(parsed.protocol, "https");
        assert_eq!(parsed.host, "host");
        assert_eq!(parsed.path_prefix, "a/b/c");
    }

    /// Host-only url `https://ghcr-remote.jfrog.io` → empty prefix.
    ///
    /// Traces: plan Testing Strategy — "host-only → empty prefix".
    #[test]
    fn parse_url_host_only_yields_empty_prefix() {
        let parsed = parse_url("https://ghcr-remote.jfrog.io").expect("valid host-only url must parse");
        assert_eq!(parsed.host, "ghcr-remote.jfrog.io");
        assert_eq!(parsed.path_prefix, "", "host-only url must yield empty prefix");
    }

    /// `http://` scheme is preserved verbatim in `protocol`.
    ///
    /// Traces: plan Testing Strategy — "http:// scheme preserved".
    #[test]
    fn parse_url_http_scheme_preserved() {
        let parsed = parse_url("http://local-mirror.corp/docker-remote").expect("http url must parse");
        assert_eq!(
            parsed.protocol, "http",
            "http scheme must be preserved, not forced to https"
        );
        assert_eq!(parsed.host, "local-mirror.corp");
        assert_eq!(parsed.path_prefix, "docker-remote");
    }

    /// A mixed-case `HTTP://` scheme normalizes to lowercase `http` so the
    /// plain-HTTP gate (`protocol == "http"`) cannot be bypassed by casing
    /// (CWE-319). Without normalization the gate would read `"HTTP"` as
    /// not-http and let plaintext traffic egress un-gated.
    #[test]
    fn parse_url_uppercase_http_scheme_normalized_to_lowercase() {
        let parsed = parse_url("HTTP://local-mirror.corp/docker-remote").expect("mixed-case http url must parse");
        assert_eq!(
            parsed.protocol, "http",
            "HTTP:// must normalize to lowercase http so the plain-HTTP gate matches"
        );
        assert_eq!(parsed.host, "local-mirror.corp");
    }

    /// A mixed-case `HtTpS://` scheme normalizes to lowercase `https`.
    #[test]
    fn parse_url_mixed_case_https_scheme_normalized_to_lowercase() {
        let parsed = parse_url("HtTpS://mirror.corp/proxy").expect("mixed-case https url must parse");
        assert_eq!(parsed.protocol, "https", "HtTpS:// must normalize to lowercase https");
        assert_eq!(parsed.host, "mirror.corp");
    }

    /// The mixed-case bypass is closed at the resolve gate too: a `HTTP://`
    /// mirror whose host is not in the insecure list is refused
    /// (`PlainHttpMirrorNotAllowed`), exactly as a lowercase `http://` one is.
    #[test]
    fn resolve_mirror_role_refuses_uppercase_http_scheme_without_insecure_host() {
        let error = resolve_mirror_role("ghcr.io", "HTTP://mirror.corp/reg", &[])
            .expect_err("a mixed-case HTTP:// mirror must still hit the plain-HTTP gate");
        assert!(
            matches!(error, MirrorConfigError::PlainHttpMirrorNotAllowed { ref mirror_host, .. } if mirror_host == "mirror.corp"),
            "expected PlainHttpMirrorNotAllowed for a mixed-case HTTP:// scheme, got: {error:?}"
        );
    }

    /// Artifactory repo-key prefix survives byte-for-byte — no `/vN` strip,
    /// no docker.io special-case.
    ///
    /// Traces: plan Testing Strategy — "Artifactory repo-key prefix survives
    /// byte-for-byte (`https://art.corp/ghcr-remote` → prefix `ghcr-remote`,
    /// NOT `/vN`-stripped)"; ADR review A4 — dedicated split, no
    /// `canonicalize_registry`.
    #[test]
    fn parse_url_artifactory_repo_key_survives_verbatim() {
        let parsed = parse_url("https://art.corp/ghcr-remote").expect("Artifactory url must parse");
        assert_eq!(parsed.host, "art.corp");
        assert_eq!(
            parsed.path_prefix, "ghcr-remote",
            "repo-key prefix must survive verbatim; /vN stripping must NOT apply"
        );
    }

    /// A `docker.io` *mirror host* is NOT special-cased or rewritten.
    ///
    /// Proves that `canonicalize_registry` (which expands `docker.io` →
    /// `index.docker.io`) is NOT in the path — a corporate mirror whose
    /// hostname happens to be `docker.io` must pass through unchanged.
    ///
    /// Traces: plan Testing Strategy — "a `docker.io` *mirror host* is NOT
    /// rewritten (proves canonicalize_registry absent)"; ADR review A4.
    #[test]
    fn parse_url_docker_io_mirror_host_not_special_cased() {
        let parsed = parse_url("https://docker.io/my-proxy").expect("docker.io mirror url must parse");
        // The host must remain "docker.io", NOT "index.docker.io".
        assert_eq!(
            parsed.host, "docker.io",
            "docker.io mirror host must NOT be rewritten to index.docker.io"
        );
        assert_eq!(parsed.path_prefix, "my-proxy");
    }

    /// Trailing slash on the prefix is trimmed.
    ///
    /// Traces: plan Testing Strategy — "trim one trailing `/`".
    #[test]
    fn parse_url_trailing_slash_trimmed_from_prefix() {
        let parsed = parse_url("https://mirror.corp/proxy-repo/").expect("url with trailing slash must parse");
        assert_eq!(
            parsed.path_prefix, "proxy-repo",
            "trailing slash must be stripped from path prefix"
        );
    }

    /// T-A1: double trailing slash — `strip_suffix('/')` is single-slash-only, NOT
    /// greedy (`trim_end_matches('/')`). A url like `"https://mirror.corp/proxy-repo//"`
    /// must yield `path_prefix == "proxy-repo/"` (one slash remains), not `"proxy-repo"`.
    ///
    /// This guards against a future regression where `strip_suffix` is swapped for
    /// `trim_end_matches`, which would greedily strip all trailing slashes and corrupt
    /// the bearer-token auth scope when the prefix genuinely ends with `/`.
    #[test]
    fn parse_url_double_trailing_slash_only_strips_one() {
        let parsed = parse_url("https://mirror.corp/proxy-repo//").expect("url with double trailing slash must parse");
        assert_eq!(
            parsed.host, "mirror.corp",
            "host must be unaffected by double trailing slash"
        );
        assert_eq!(
            parsed.path_prefix, "proxy-repo/",
            "double trailing slash must strip only one slash, leaving proxy-repo/ (not proxy-repo)"
        );
    }

    /// T-A1 (host-only variant): `"https://host/"` — the path after the host is
    /// an empty string `""`, which exercises the `split_once('/') → ("host", "")`
    /// branch. The empty raw_prefix has no trailing slash, so `strip_suffix` is a
    /// no-op, and `path_prefix` must be empty (not `"/"` or any other artifact).
    #[test]
    fn parse_url_host_with_single_trailing_slash_yields_empty_prefix() {
        let parsed = parse_url("https://host/").expect("host-only url with trailing slash must parse");
        assert_eq!(parsed.host, "host", "host must be extracted correctly");
        assert_eq!(
            parsed.path_prefix, "",
            "a host-only url with a single trailing slash must yield an empty path_prefix"
        );
    }

    /// T-A5: `"https://"` (scheme present, host absent) → `MirrorConfigError::MissingHost`.
    ///
    /// After stripping `"https://"` the remainder is `""`, so
    /// `split_once('/')` returns `None` and `rest` itself is `""`, which fails
    /// the `host.is_empty()` guard. This proves that the guard fires on an
    /// actually-empty host, not just on a `split_once` miss.
    #[test]
    fn parse_url_errors_on_scheme_only_missing_host() {
        let result = parse_url("https://");
        assert!(
            matches!(result, Err(MirrorConfigError::MissingHost { ref url }) if url == "https://"),
            "url with scheme but no host must yield MirrorConfigError::MissingHost, got: {result:?}"
        );
    }

    /// T-A5 (with prefix): `"https:///prefix"` — host segment between `://` and
    /// the first `/` is empty. Must yield `MirrorConfigError::MissingHost`.
    #[test]
    fn parse_url_errors_on_empty_host_before_prefix() {
        let result = parse_url("https:///prefix");
        assert!(
            matches!(result, Err(MirrorConfigError::MissingHost { .. })),
            "url with empty host segment before prefix must yield MirrorConfigError::MissingHost, got: {result:?}"
        );
    }

    /// T-A5 (resolve path): `MissingHost` surfaced through `resolve_mirror_map`
    /// must be wrapped in `InvalidEntry` naming the upstream host, parallel to the
    /// existing `resolve_errors_on_missing_url_naming_upstream` test for
    /// `EmptyEntry`.
    #[test]
    fn resolve_errors_on_missing_host_naming_upstream() {
        // A url with a scheme but no host: parse_url returns MissingHost, which
        // resolve_mirror_map must wrap in InvalidEntry naming the upstream key.
        let config = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "docker.io".to_string(),
                    MirrorConfig {
                        registry: Some("https://".to_string()),
                        index: None,
                        registry_system_locked: false,
                        index_system_locked: false,
                    },
                );
                m
            }),
            ..Default::default()
        };

        let resolved =
            resolve_mirror_map(&config, Vec::new(), &[]).expect_err("a registry role url with no host must error");
        assert!(
            matches!(resolved, MirrorConfigError::InvalidEntry { ref upstream, .. } if upstream == "docker.io"),
            "expected InvalidEntry naming docker.io as the upstream, got: {resolved:?}"
        );
        // Source of the InvalidEntry must be MissingHost.
        let message = resolved.to_string();
        assert!(
            message.contains("docker.io"),
            "error message must name the upstream host; got: {message}"
        );
    }

    /// Tier merge: higher tier (`Some(registry)`) replaces the lower tier's
    /// entry for the same host key when merged into `Config`.
    ///
    /// Traces: plan Testing Strategy — "tier merge (higher tier replaces host
    /// key-by-key)".
    #[test]
    fn config_mirrors_tier_merge_higher_wins_per_host_key() {
        use std::collections::HashMap;

        use crate::config::Config;

        let mut lower = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        registry: Some("https://old-mirror.corp/ghcr-remote".to_string()),
                        index: None,
                        registry_system_locked: false,
                        index_system_locked: false,
                    },
                );
                m
            }),
            ..Default::default()
        };

        let higher = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        registry: Some("https://new-mirror.corp/ghcr-remote".to_string()),
                        index: None,
                        registry_system_locked: false,
                        index_system_locked: false,
                    },
                );
                m
            }),
            ..Default::default()
        };

        lower.merge(higher);

        let merged_url = lower
            .mirrors
            .as_ref()
            .and_then(|m| m.get("ghcr.io"))
            .and_then(|c| c.registry.as_deref());

        assert_eq!(
            merged_url,
            Some("https://new-mirror.corp/ghcr-remote"),
            "higher-tier registry url must win over lower-tier url for the same host key"
        );
    }

    // ── resolve_mirror_map: precedence + plain-HTTP gate ─────────────────────

    use std::collections::HashMap;

    use crate::config::Config;

    /// Build a `Config` whose `[mirrors]` table maps each `(host, url)` to a
    /// registry-role-only entry.
    fn config_with_mirrors(entries: &[(&str, &str)]) -> Config {
        let mut map = HashMap::new();
        for (host, url) in entries {
            map.insert(
                (*host).to_string(),
                MirrorConfig {
                    registry: Some((*url).to_string()),
                    index: None,
                    registry_system_locked: false,
                    index_system_locked: false,
                },
            );
        }
        Config {
            mirrors: Some(map),
            ..Default::default()
        }
    }

    /// `OCX_MIRRORS` (env) wins over `[mirrors]` (config) per-host key, and a
    /// host present in only one source survives. This is the one-way-door
    /// public-contract precedence rule — relocated from `context.rs` to live
    /// next to the resolver that now owns the merge.
    ///
    /// Config: `{ghcr.io→A, docker.io→C}`; env: `{ghcr.io→B}`.
    /// Result: `{ghcr.io→B (env wins), docker.io→C (config survives)}`.
    #[test]
    fn env_mirrors_win_over_config_per_host_key() {
        let config = config_with_mirrors(&[
            ("ghcr.io", "https://config-mirror.corp/ghcr-A"),
            ("docker.io", "https://config-mirror.corp/docker-C"),
        ]);
        let env_entries = vec![(
            "ghcr.io".to_string(),
            MirrorConfig {
                registry: Some("https://env-mirror.corp/ghcr-B".to_string()),
                index: None,
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];

        let resolved = resolve_mirror_map(&config, env_entries, &[])
            .expect("https mirrors must resolve without the insecure gate");

        assert_eq!(
            resolved.merged.get("ghcr.io").and_then(|c| c.registry.as_deref()),
            Some("https://env-mirror.corp/ghcr-B"),
            "env must win over config for a host present in both"
        );
        assert_eq!(
            resolved.merged.get("docker.io").and_then(|c| c.registry.as_deref()),
            Some("https://config-mirror.corp/docker-C"),
            "a host present only in config must survive the env overlay"
        );
        assert_eq!(resolved.merged.len(), 2, "no spurious hosts; only ghcr.io + docker.io");
    }

    /// Cross-role env-over-config precedence is WHOLE-VALUE replace, not a
    /// field-wise merge: config declares a split entry (`registry="A",
    /// index="B"`), env declares a registry-only overlay for the same host
    /// (`registry="C"`) — the env entry replaces the config entry entirely,
    /// so the merged result is `registry=C, index=None` (config's `index="B"`
    /// does NOT survive).
    ///
    /// This is the pre-existing documented `OCX_MIRRORS` contract: env
    /// forwards an already-fully-resolved snapshot from the parent ocx (see
    /// `resolve_mirror_map`'s doc comment — "env_entries win per-host key"),
    /// so a host present in both sources takes the ENTIRE env entry, not a
    /// per-role splice. A field-wise cross-role merge instead of whole-value
    /// replace is a deferred design question (recorded in
    /// `plan_one_index.md`), not implemented here.
    #[test]
    fn env_mirrors_replace_is_whole_value_not_field_wise_across_roles() {
        let mut mirrors = HashMap::new();
        mirrors.insert(
            "h".to_string(),
            MirrorConfig {
                registry: Some("A".to_string()),
                index: Some("B".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        );
        let config = Config {
            mirrors: Some(mirrors),
            ..Default::default()
        };
        let env_entries = vec![(
            "h".to_string(),
            MirrorConfig {
                registry: Some("https://mirror.corp/C".to_string()),
                index: None,
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];

        let resolved = resolve_mirror_map(&config, env_entries, &[]).expect("registry-only entry must resolve");

        let merged = resolved.merged.get("h").expect("host h must be present after merge");
        assert_eq!(
            merged.registry.as_deref(),
            Some("https://mirror.corp/C"),
            "env's registry value must win"
        );
        assert!(
            merged.index.is_none(),
            "current contract: whole-value replace — config's index=\"B\" must NOT survive \
             alongside env's registry=\"C\" (field-wise cross-role merge is a deferred design question)"
        );
    }

    /// An `http://` mirror whose host is NOT in `insecure_hosts` fails loud at
    /// resolve time (CWE-319) with an error that NAMES `OCX_INSECURE_REGISTRIES`
    /// and the offending mirror host — not an opaque TLS failure mid-transport.
    ///
    /// Traces: Finding B (security) — fail loud + actionable on plain-HTTP
    /// mirror not in the insecure list.
    #[test]
    fn resolve_errors_on_http_mirror_host_absent_from_insecure_list() {
        let config = config_with_mirrors(&[("ghcr.io", "http://local-mirror.corp:5001/ghcr-remote")]);

        let err = resolve_mirror_map(&config, Vec::new(), &[])
            .expect_err("http mirror without its host in the insecure list must error");

        assert!(
            matches!(
                err,
                MirrorConfigError::PlainHttpMirrorNotAllowed { ref mirror_host, .. }
                    if mirror_host == "local-mirror.corp:5001"
            ),
            "expected PlainHttpMirrorNotAllowed naming the mirror host, got: {err:?}"
        );

        let message = err.to_string();
        assert!(
            message.contains("OCX_INSECURE_REGISTRIES"),
            "error must name OCX_INSECURE_REGISTRIES so the operator knows the fix; got: {message}"
        );
        assert!(
            message.contains("local-mirror.corp:5001"),
            "error must name the offending mirror host; got: {message}"
        );
        assert!(
            message.contains("ghcr.io"),
            "error must name the upstream host the mirror serves; got: {message}"
        );
    }

    /// The same `http://` mirror resolves cleanly once its host IS listed in
    /// `insecure_hosts` — the gate is host-membership, not scheme alone.
    ///
    /// Traces: Finding B — scenario 6a (http mirror WITH host in
    /// OCX_INSECURE_REGISTRIES must still succeed).
    #[test]
    fn resolve_allows_http_mirror_host_in_insecure_list() {
        let config = config_with_mirrors(&[("ghcr.io", "http://local-mirror.corp:5001/ghcr-remote")]);
        let insecure = vec!["local-mirror.corp:5001".to_string()];

        let resolved = resolve_mirror_map(&config, Vec::new(), &insecure)
            .expect("http mirror with host in insecure list resolves");

        assert_eq!(resolved.registry.len(), 1, "the single mirror entry must resolve");
        let parsed = resolved.registry.get("ghcr.io").expect("ghcr.io must resolve");
        assert_eq!(parsed.protocol, "http");
        assert_eq!(parsed.host, "local-mirror.corp:5001");
        assert_eq!(parsed.path_prefix, "ghcr-remote");
    }

    /// An entry declaring neither role surfaces as `EmptyEntry` naming the
    /// upstream host (F9 — no silent egress).
    #[test]
    fn resolve_errors_on_missing_url_naming_upstream() {
        let config = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        registry: None,
                        index: None,
                        registry_system_locked: false,
                        index_system_locked: false,
                    },
                );
                m
            }),
            ..Default::default()
        };

        let err = resolve_mirror_map(&config, Vec::new(), &[]).expect_err("an empty entry must error (F9)");
        assert!(
            matches!(err, MirrorConfigError::EmptyEntry { ref upstream } if upstream == "ghcr.io"),
            "expected EmptyEntry naming ghcr.io, got: {err:?}"
        );
    }

    /// Tier merge across multiple hosts: a host present only in the lower tier
    /// survives, while the higher tier wins for a host present in both.
    ///
    /// Lower: `{ghcr.io, docker.io}`; higher: `{ghcr.io}`. Merged must keep
    /// `docker.io` (lower-only survives) AND take the higher tier's `ghcr.io`.
    ///
    /// Traces: plan Testing Strategy — "tier merge (higher tier replaces host
    /// key-by-key)", lower-tier-only-host-survives case.
    #[test]
    fn config_mirrors_tier_merge_lower_only_host_survives() {
        use std::collections::HashMap;

        use crate::config::Config;

        let mut lower = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        registry: Some("https://old-mirror.corp/ghcr-remote".to_string()),
                        index: None,
                        registry_system_locked: false,
                        index_system_locked: false,
                    },
                );
                m.insert(
                    "docker.io".to_string(),
                    MirrorConfig {
                        registry: Some("https://old-mirror.corp/dockerhub-remote".to_string()),
                        index: None,
                        registry_system_locked: false,
                        index_system_locked: false,
                    },
                );
                m
            }),
            ..Default::default()
        };

        let higher = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        registry: Some("https://new-mirror.corp/ghcr-remote".to_string()),
                        index: None,
                        registry_system_locked: false,
                        index_system_locked: false,
                    },
                );
                m
            }),
            ..Default::default()
        };

        lower.merge(higher);

        let mirrors = lower.mirrors.as_ref().expect("merged config must keep mirrors");
        assert_eq!(
            mirrors.get("ghcr.io").and_then(|c| c.registry.as_deref()),
            Some("https://new-mirror.corp/ghcr-remote"),
            "higher tier must win for ghcr.io (present in both tiers)"
        );
        assert_eq!(
            mirrors.get("docker.io").and_then(|c| c.registry.as_deref()),
            Some("https://old-mirror.corp/dockerhub-remote"),
            "docker.io is only in the lower tier and must survive the merge"
        );
        assert_eq!(mirrors.len(), 2, "merge must keep both host keys, no host dropped");
    }

    // ── resolve_mirror_map: config-only / env-only / per-role split ─────────

    /// Config-only resolve (no env entries): a single registry-role entry
    /// populates `resolved.registry` and leaves `resolved.index` empty.
    #[test]
    fn resolve_mirror_map_config_only_populates_registry_role() {
        let config = config_with_mirrors(&[("ghcr.io", "https://config-mirror.corp/ghcr-remote")]);
        let resolved = resolve_mirror_map(&config, Vec::new(), &[]).expect("a config-only resolve must succeed");
        assert_eq!(resolved.registry.len(), 1);
        assert_eq!(
            resolved.registry.get("ghcr.io").map(|p| p.host.as_str()),
            Some("config-mirror.corp")
        );
        assert!(
            resolved.index.is_empty(),
            "a registry-only config entry must not populate the index role"
        );
    }

    /// Env-only resolve (no `[mirrors]` config at all): the env entry alone
    /// resolves.
    #[test]
    fn resolve_mirror_map_env_only_populates_registry_role() {
        let config = Config::default();
        let env_entries = vec![(
            "ghcr.io".to_string(),
            MirrorConfig {
                registry: Some("https://env-mirror.corp/ghcr-remote".to_string()),
                index: None,
                registry_system_locked: false,
                index_system_locked: false,
            },
        )];
        let resolved = resolve_mirror_map(&config, env_entries, &[]).expect("an env-only resolve must succeed");
        assert_eq!(resolved.registry.len(), 1);
        assert_eq!(
            resolved.registry.get("ghcr.io").map(|p| p.host.as_str()),
            Some("env-mirror.corp")
        );
    }

    /// A registry-only entry never appears in `resolved.index`, and an
    /// index-only entry never appears in `resolved.registry` — the two
    /// per-role output maps never cross-contaminate.
    #[test]
    fn resolve_mirror_map_splits_per_role_never_cross_contaminates() {
        let mut mirrors = HashMap::new();
        mirrors.insert(
            "ghcr.io".to_string(),
            MirrorConfig {
                registry: Some("https://mirror.corp/ghcr-remote".to_string()),
                index: None,
                registry_system_locked: false,
                index_system_locked: false,
            },
        );
        mirrors.insert(
            "index.ocx.sh".to_string(),
            MirrorConfig {
                registry: None,
                index: Some("https://mirror.corp/ocx-index".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        );
        let config = Config {
            mirrors: Some(mirrors),
            ..Default::default()
        };

        let resolved = resolve_mirror_map(&config, Vec::new(), &[]).expect("a mixed-role config must resolve");

        assert!(resolved.registry.contains_key("ghcr.io"));
        assert!(
            !resolved.registry.contains_key("index.ocx.sh"),
            "an index-only entry must never rewrite OCI traffic"
        );
        assert!(resolved.index.contains_key("index.ocx.sh"));
        assert!(
            !resolved.index.contains_key("ghcr.io"),
            "a registry-only entry must never rewrite index traffic"
        );
    }

    /// Replaces the retired `parse_url_errors_on_none_url` (wp-b specify):
    /// under the F5b union shape, an absent role is no longer a `parse_url`
    /// error at all — `resolve_mirror_map` skips a `None` role before ever
    /// calling `parse_url`, so it simply yields no rewrite for that role
    /// while the present role on the same host still resolves.
    #[test]
    fn resolve_mirror_map_absent_role_yields_no_rewrite_for_that_role() {
        let mut mirrors = HashMap::new();
        mirrors.insert(
            "index.ocx.sh".to_string(),
            MirrorConfig {
                registry: None,
                index: Some("https://artifactory.corp/ocx-index".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        );
        let config = Config {
            mirrors: Some(mirrors),
            ..Default::default()
        };

        let resolved =
            resolve_mirror_map(&config, Vec::new(), &[]).expect("an absent role must be skipped, not an error");

        assert!(
            !resolved.registry.contains_key("index.ocx.sh"),
            "an absent registry role must yield no rewrite for that role"
        );
        assert_eq!(
            resolved.index.get("index.ocx.sh").map(|p| p.host.as_str()),
            Some("artifactory.corp"),
            "the present index role must still resolve"
        );
    }

    /// The plain-HTTP gate (CWE-319) applies per role: an `http://` index
    /// mirror whose host is not in `insecure_hosts` fails loud, naming
    /// `OCX_INSECURE_REGISTRIES` and the offending mirror host — parallel to
    /// the registry-role case above.
    #[test]
    fn resolve_errors_on_http_index_mirror_host_absent_from_insecure_list() {
        let mut mirrors = HashMap::new();
        mirrors.insert(
            "index.ocx.sh".to_string(),
            MirrorConfig {
                registry: None,
                index: Some("http://local-index-mirror.corp:5002/ocx-index".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        );
        let config = Config {
            mirrors: Some(mirrors),
            ..Default::default()
        };

        let err = resolve_mirror_map(&config, Vec::new(), &[])
            .expect_err("an http index mirror without its host in the insecure list must error");

        assert!(
            matches!(
                err,
                MirrorConfigError::PlainHttpMirrorNotAllowed { ref mirror_host, .. }
                    if mirror_host == "local-index-mirror.corp:5002"
            ),
            "expected PlainHttpMirrorNotAllowed naming the mirror host, got: {err:?}"
        );
        let message = err.to_string();
        assert!(message.contains("OCX_INSECURE_REGISTRIES"));
        assert!(message.contains("local-index-mirror.corp:5002"));
    }

    /// The same `http://` index mirror resolves cleanly once its host IS
    /// listed in `insecure_hosts` — the gate is host-membership, not scheme
    /// alone, mirrored per role.
    #[test]
    fn resolve_allows_http_index_mirror_host_in_insecure_list() {
        let mut mirrors = HashMap::new();
        mirrors.insert(
            "index.ocx.sh".to_string(),
            MirrorConfig {
                registry: None,
                index: Some("http://local-index-mirror.corp:5002/ocx-index".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        );
        let config = Config {
            mirrors: Some(mirrors),
            ..Default::default()
        };
        let insecure = vec!["local-index-mirror.corp:5002".to_string()];

        let resolved = resolve_mirror_map(&config, Vec::new(), &insecure)
            .expect("http index mirror with host in insecure list resolves");

        assert_eq!(resolved.index.len(), 1);
        let parsed = resolved.index.get("index.ocx.sh").expect("index.ocx.sh must resolve");
        assert_eq!(parsed.protocol, "http");
        assert_eq!(parsed.host, "local-index-mirror.corp:5002");
    }

    /// `resolved.merged` forwards the pre-split union entry, not just the
    /// role that happened to parse into a given output map — a split entry
    /// declaring both roles keeps both in `merged`.
    #[test]
    fn resolve_mirror_map_merged_field_preserves_both_roles_for_a_split_entry() {
        let mut mirrors = HashMap::new();
        mirrors.insert(
            "index.ocx.sh".to_string(),
            MirrorConfig {
                registry: Some("https://mirror.corp/registry-side".to_string()),
                index: Some("https://mirror.corp/index-side".to_string()),
                registry_system_locked: false,
                index_system_locked: false,
            },
        );
        let config = Config {
            mirrors: Some(mirrors),
            ..Default::default()
        };

        let resolved = resolve_mirror_map(&config, Vec::new(), &[]).expect("a split-role entry must resolve");

        let merged = resolved
            .merged
            .get("index.ocx.sh")
            .expect("merged must forward the pre-split union entry");
        assert_eq!(merged.registry.as_deref(), Some("https://mirror.corp/registry-side"));
        assert_eq!(merged.index.as_deref(), Some("https://mirror.corp/index-side"));
    }
}
