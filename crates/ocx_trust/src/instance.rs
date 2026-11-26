// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Sigstore instances: the `[sigstore."<authority>"]` tables and the authority key that names them.
//!
//! An instance is one Fulcio and Rekor stack with its trust root. A keyless signer entry names the instance it
//! trusts by authority; the public good instance is built in and a user table under its key overrides it field by
//! field.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use ocx_util::fs::path::FileReference;

/// The authority of the public good Sigstore instance.
pub const PUBLIC_AUTHORITY: &str = "fulcio.sigstore.dev";

/// Fulcio base URL of the public good instance.
pub const PUBLIC_FULCIO_URL: &str = "https://fulcio.sigstore.dev";

/// Rekor base URL of the public good instance.
pub const PUBLIC_REKOR_URL: &str = "https://rekor.sigstore.dev";

/// Why a string is not a Sigstore authority.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid Sigstore authority `{input}`: {reason}")]
pub struct SigstoreAuthorityError {
    /// The rejected text.
    pub input: String,
    /// What is wrong with it.
    pub reason: &'static str,
}

/// The lowercase `host[:port]` that names a Sigstore instance.
///
/// Only `:443` is dropped; every other port stays, so `http://h` maps to `h:80`. A table key and a URL parse
/// through the same host parser, so they agree on spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SigstoreAuthority(String);

impl SigstoreAuthority {
    /// Parse a table key (`host[:port]`).
    ///
    /// # Errors
    /// [`SigstoreAuthorityError`] when the key is empty, carries a scheme, path, userinfo or query, ends in a dot or
    /// a colon, or is not a valid host.
    pub fn parse(key: &str) -> Result<Self, SigstoreAuthorityError> {
        let refuse = |reason| SigstoreAuthorityError {
            input: key.to_owned(),
            reason,
        };
        if key.is_empty() {
            return Err(refuse("empty"));
        }
        if key.contains("://") {
            return Err(refuse("carries a scheme"));
        }
        if key.contains(['/', '\\']) {
            return Err(refuse("carries a path"));
        }
        if key.contains('@') {
            return Err(refuse("carries userinfo"));
        }
        if key.contains(['?', '#']) {
            return Err(refuse("carries a query or fragment"));
        }
        if key.ends_with('.') || key.ends_with(':') {
            return Err(refuse("ends in a separator"));
        }
        let url = url::Url::parse(&format!("https://{key}")).map_err(|_| refuse("not a valid host"))?;
        Self::from_parsed(&url).ok_or_else(|| refuse("not a valid host"))
    }

    /// The authority of a URL, as the table key that would name it.
    ///
    /// # Errors
    /// [`SigstoreAuthorityError`] when the text is not a URL with a host.
    pub fn from_url(text: &str) -> Result<Self, SigstoreAuthorityError> {
        let refuse = |reason| SigstoreAuthorityError {
            input: text.to_owned(),
            reason,
        };
        let url = url::Url::parse(text).map_err(|_| refuse("not a URL"))?;
        Self::from_parsed(&url).ok_or_else(|| refuse("has no host"))
    }

    /// The public good instance's authority.
    #[must_use]
    pub fn public() -> Self {
        Self(PUBLIC_AUTHORITY.to_owned())
    }

    /// The `host[:port]` text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The directory name this authority's convention files live under: `<host>_<port>`, the port always written.
    ///
    /// Injective: the port is all digits and comes last, so `a:5555` (`a_5555`) and a host `a_5555` (`a_5555_443`)
    /// never share a directory. `None` for an IPv6 literal, whose brackets and colons have no portable file name; its
    /// table must name its trusted root.
    #[must_use]
    pub fn convention_dir_name(&self) -> Option<String> {
        if self.0.starts_with('[') {
            return None;
        }
        Some(match self.0.rsplit_once(':') {
            Some((host, port)) => format!("{host}_{port}"),
            None => format!("{}_443", self.0),
        })
    }

    fn from_parsed(url: &url::Url) -> Option<Self> {
        let host = url.host_str().filter(|host| !host.is_empty() && !host.ends_with('.'))?;
        // `port_or_known_default` gives http `80` and https `443`; only 443 is dropped from the name.
        match url.port_or_known_default() {
            Some(443) | None => Some(Self(host.to_owned())),
            Some(port) => Some(Self(format!("{host}:{port}"))),
        }
    }
}

impl std::fmt::Display for SigstoreAuthority {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for SigstoreAuthority {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SigstoreAuthority {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for SigstoreAuthority {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("SigstoreAuthority")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A Sigstore instance name: the lowercase `host[:port]` of its Fulcio, without scheme or path. A `:443` port is dropped."
        })
    }
}

/// One `[sigstore."<authority>"]` table: where verification and signing get their trust material for one
/// instance.
///
/// Every field is optional. No `deny_unknown_fields`: a table written for a newer ocx degrades to its known parts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SigstoreInstance {
    /// Path to a Sigstore trusted-root JSON, or a directory holding `trusted_root.json`.
    ///
    /// A relative path resolves against the directory of the `config.toml` that declared it.
    /// Mutually exclusive with `trusted_root_json`.
    #[serde(default)]
    pub trusted_root: Option<PathBuf>,

    /// The trusted-root document inlined verbatim; the form a fleet receives from `ocx config push`.
    /// Mutually exclusive with `trusted_root`.
    #[serde(default)]
    pub trusted_root_json: Option<String>,

    /// Fulcio base URL. Its authority must equal the table key; absent, it defaults to `https://<key>`.
    #[serde(default)]
    pub fulcio_url: Option<String>,

    /// Rekor base URL, needed only to sign with a Rekor upload.
    #[serde(default)]
    pub rekor_url: Option<String>,

    // Off by default, unlike cosign: on, it would irreversibly publish a private artifact's digest and signer to the
    // public Rekor on first run.
    /// Whether a key-mode signature is uploaded to the transparency log when neither `--rekor-upload` nor
    /// `--no-rekor-upload` is given. Keyless signing always uploads and ignores this field.
    #[serde(default)]
    pub rekor_upload: Option<bool>,

    /// Declared in the system config, so lower tiers cannot override the table; set by `lock_as_system`, never
    /// serialized.
    // The skip is the security boundary: a managed payload writing `system_locked = true` is dropped as an unknown key.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

impl SigstoreInstance {
    /// The built-in public good instance: public Fulcio and Rekor, trust root over TUF.
    #[must_use]
    pub fn public_builtin() -> Self {
        Self {
            fulcio_url: Some(PUBLIC_FULCIO_URL.to_owned()),
            rekor_url: Some(PUBLIC_REKOR_URL.to_owned()),
            ..Self::default()
        }
    }

    /// Mark this table as system-scope, non-overridable by lower tiers.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Merge `other` (higher precedence) into `self`, field by field; a system-locked `self` ignores it whole.
    ///
    /// The lock is adopted from `other`: the system tier folds in as `other` on the first merge, so without the
    /// adoption the lock never applies.
    pub fn merge(&mut self, other: SigstoreInstance) {
        if self.system_locked {
            return;
        }
        self.system_locked = other.system_locked;
        // Take both trust-root fields together, or switching from path to inline leaves both set and trips the XOR.
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

    /// Resolve [`Self::trusted_root`] (bare or `file://`) against `config_dir`, the declaring `config.toml`'s
    /// directory, via [`FileReference::anchored_at`]; called once per file tier, so the working directory never counts.
    pub fn anchor_relative_root(&mut self, config_dir: &Path) {
        if let Some(path) = self.trusted_root.as_ref() {
            // `to_string_lossy` is exact: the value came from a TOML string.
            let written = path.to_string_lossy().into_owned();
            self.trusted_root = Some(FileReference::parse(&written).anchored_at(config_dir));
        }
    }
}

/// The configured `[sigstore."<authority>"]` tables, keyed by instance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct SigstoreInstances(BTreeMap<SigstoreAuthority, SigstoreInstance>);

impl Deref for SigstoreInstances {
    type Target = BTreeMap<SigstoreAuthority, SigstoreInstance>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl SigstoreInstances {
    /// A table set whose only entry overrides the public good instance.
    #[must_use]
    pub fn with_public(instance: SigstoreInstance) -> Self {
        Self(BTreeMap::from([(SigstoreAuthority::public(), instance)]))
    }

    /// The user-declared table for `authority`; the built-in public table is not reported here.
    #[must_use]
    pub fn get(&self, authority: &SigstoreAuthority) -> Option<&SigstoreInstance> {
        self.0.get(authority)
    }

    /// The public good instance: the built-in table with any user table under its key laid over it field by field.
    #[must_use]
    pub fn public(&self) -> SigstoreInstance {
        let mut instance = SigstoreInstance::public_builtin();
        if let Some(declared) = self.0.get(&SigstoreAuthority::public()) {
            instance.merge(declared.clone());
        }
        instance
    }

    /// The instance a Fulcio URL selects: the table keyed by the URL's authority, or the public instance when that
    /// is its authority. `None` when the URL has no authority or no table names it.
    #[must_use]
    pub fn select_by_fulcio_url(&self, fulcio_url: &str) -> Option<(SigstoreAuthority, SigstoreInstance)> {
        let authority = SigstoreAuthority::from_url(fulcio_url).ok()?;
        if authority == SigstoreAuthority::public() {
            return Some((authority, self.public()));
        }
        let instance = self.0.get(&authority)?.clone();
        Some((authority, instance))
    }

    /// Mark every table as system-scope.
    pub fn lock_as_system(&mut self) {
        for instance in self.0.values_mut() {
            instance.lock_as_system();
        }
    }

    /// Merge `other` (the higher-precedence tier) into `self`, table by table and field by field.
    pub fn merge(&mut self, other: SigstoreInstances) {
        for (authority, instance) in other.0 {
            match self.0.get_mut(&authority) {
                Some(existing) => existing.merge(instance),
                None => {
                    self.0.insert(authority, instance);
                }
            }
        }
    }

    /// Anchor every table's relative `trusted_root` against the declaring file's directory.
    pub fn anchor_relative_roots(&mut self, config_dir: &Path) {
        for instance in self.0.values_mut() {
            instance.anchor_relative_root(config_dir);
        }
    }

    /// Keep only the system-locked tables.
    pub fn retain_system_locked(&mut self) {
        self.0.retain(|_, instance| instance.system_locked);
    }

    /// Mutable access to every table, for the managed-payload guard.
    pub fn instances_mut(&mut self) -> impl Iterator<Item = &mut SigstoreInstance> {
        self.0.values_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_accepts_host_and_host_port() {
        assert_eq!(
            SigstoreAuthority::parse("Fulcio.Example").expect("host").as_str(),
            "fulcio.example"
        );
        assert_eq!(
            SigstoreAuthority::parse("fulcio.example:5555").expect("port").as_str(),
            "fulcio.example:5555"
        );
    }

    #[test]
    fn authority_drops_only_port_443() {
        assert_eq!(SigstoreAuthority::parse("h:443").expect("443").as_str(), "h");
        assert_eq!(SigstoreAuthority::parse("h:80").expect("80").as_str(), "h:80");
        assert_eq!(SigstoreAuthority::from_url("https://h/").expect("https").as_str(), "h");
        assert_eq!(
            SigstoreAuthority::from_url("https://h:443/x")
                .expect("https 443")
                .as_str(),
            "h"
        );
        assert_eq!(SigstoreAuthority::from_url("http://h").expect("http").as_str(), "h:80");
        assert_eq!(
            SigstoreAuthority::from_url("http://h:8080/p")
                .expect("http port")
                .as_str(),
            "h:8080"
        );
    }

    #[test]
    fn authority_key_and_url_agree() {
        let from_key = SigstoreAuthority::parse("fulcio.example:5555").expect("key");
        let from_url = SigstoreAuthority::from_url("https://FULCIO.example:5555/api/v2").expect("url");
        assert_eq!(from_key, from_url);
    }

    #[test]
    fn authority_applies_idna() {
        assert_eq!(
            SigstoreAuthority::parse("bücher.example").expect("idna").as_str(),
            "xn--bcher-kva.example"
        );
    }

    #[test]
    fn authority_refuses_malformed_keys() {
        for key in [
            "",
            "https://h",
            "h/path",
            "u@h",
            "h?q=1",
            "h#f",
            "h.",
            "h:",
            "h:notaport",
            "a b",
            "h\\p",
        ] {
            assert!(SigstoreAuthority::parse(key).is_err(), "{key:?} must be refused");
        }
    }

    #[test]
    fn instance_table_deserializes_without_deny_unknown_fields() {
        let table: SigstoreInstances =
            toml::from_str("[\"fulcio.example:5555\"]\nfulcio_url = \"https://fulcio.example:5555\"\nfuture_key = 1\n")
                .expect("parse");
        let authority = SigstoreAuthority::parse("fulcio.example:5555").expect("authority");
        assert_eq!(
            table
                .get(&authority)
                .and_then(|instance| instance.fulcio_url.as_deref()),
            Some("https://fulcio.example:5555")
        );
    }

    #[test]
    fn table_key_that_is_not_an_authority_fails_to_parse() {
        assert!(toml::from_str::<SigstoreInstances>("[\"https://fulcio.example\"]\n").is_err());
    }

    #[test]
    fn public_is_builtin_overridden_field_by_field() {
        let mut instances = SigstoreInstances::default();
        assert_eq!(instances.public().fulcio_url.as_deref(), Some(PUBLIC_FULCIO_URL));
        instances = SigstoreInstances::with_public(SigstoreInstance {
            rekor_url: Some("https://rekor.example".into()),
            ..SigstoreInstance::default()
        });
        let public = instances.public();
        assert_eq!(public.fulcio_url.as_deref(), Some(PUBLIC_FULCIO_URL));
        assert_eq!(public.rekor_url.as_deref(), Some("https://rekor.example"));
    }

    #[test]
    fn select_by_fulcio_url_finds_public_and_declared_tables() {
        let authority = SigstoreAuthority::parse("fulcio.example").expect("authority");
        let instances = SigstoreInstances(BTreeMap::from([(authority.clone(), SigstoreInstance::default())]));
        let (public, _) = instances.select_by_fulcio_url(PUBLIC_FULCIO_URL).expect("public");
        assert_eq!(public, SigstoreAuthority::public());
        let (selected, _) = instances
            .select_by_fulcio_url("https://fulcio.example/")
            .expect("declared");
        assert_eq!(selected, authority);
        assert!(instances.select_by_fulcio_url("https://other.example").is_none());
        assert!(instances.select_by_fulcio_url("not a url").is_none());
    }

    #[test]
    fn merge_is_per_table_and_per_field_with_system_lock() {
        let key = SigstoreAuthority::parse("fulcio.example").expect("authority");
        let mut lower = SigstoreInstances(BTreeMap::from([(
            key.clone(),
            SigstoreInstance {
                fulcio_url: Some("https://fulcio.example".into()),
                rekor_url: Some("https://rekor.one".into()),
                ..SigstoreInstance::default()
            },
        )]));
        lower.merge(SigstoreInstances(BTreeMap::from([(
            key.clone(),
            SigstoreInstance {
                rekor_url: Some("https://rekor.two".into()),
                ..SigstoreInstance::default()
            },
        )])));
        let merged = lower.get(&key).expect("table");
        assert_eq!(merged.fulcio_url.as_deref(), Some("https://fulcio.example"));
        assert_eq!(merged.rekor_url.as_deref(), Some("https://rekor.two"));

        lower.lock_as_system();
        lower.merge(SigstoreInstances(BTreeMap::from([(
            key.clone(),
            SigstoreInstance {
                rekor_url: Some("https://rekor.three".into()),
                ..SigstoreInstance::default()
            },
        )])));
        assert_eq!(
            lower.get(&key).and_then(|i| i.rekor_url.as_deref()),
            Some("https://rekor.two")
        );
    }

    #[test]
    fn convention_dir_name_always_writes_the_port_and_is_injective() {
        let name = |key: &str| SigstoreAuthority::parse(key).expect("authority").convention_dir_name();
        assert_eq!(name("fulcio.example").as_deref(), Some("fulcio.example_443"));
        assert_eq!(name("fulcio.example:5555").as_deref(), Some("fulcio.example_5555"));
        assert_eq!(name("localhost:5555").as_deref(), Some("localhost_5555"));
        assert_ne!(name("a:5555"), name("a_5555"));
        assert_eq!(name("a_5555").as_deref(), Some("a_5555_443"));
        assert_eq!(name("[::1]:5555"), None, "an IPv6 literal has no convention directory");
        assert_eq!(name("[::1]"), None);
    }
}
