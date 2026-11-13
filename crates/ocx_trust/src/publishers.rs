// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Index-declared `publishers`: strict, fail-closed parse of a remote document into signer entries.
//!
//! The document is untrusted input: parsing never reads a file or makes a request.

use serde_json::{Map, Value};

use crate::claims::{ClaimKeyError, CompiledClaim, resolve_claim_key};
use crate::key_ref::MAX_KEY_PEM_BYTES;
use crate::{CompiledKeyless, CompiledPolicy, IdentityRule, PolicyBackend};

/// Compiled-size ceiling for a remote `identity_regexp`.
const REGEX_SIZE_LIMIT: usize = 64 * 1024;

const KEYLESS_FIELDS: [&str; 5] = ["kind", "identity", "identity_regexp", "oidc_issuer", "claims"];
const KEY_FIELDS: [&str; 2] = ["kind", "key_pem"];

/// One `publishers` array element, classified.
#[derive(Debug, Clone)]
pub enum PublisherEntry {
    /// A well-formed signer this build understands.
    Usable(PolicyBackend),
    /// Well-formed but from a newer ocx (unknown `kind`, field or claim name); narrows, never widens.
    Unrecognised {
        /// Position in the array.
        index: usize,
        /// Why the entry is not understood.
        reason: String,
    },
    /// Malformed: the entry cannot be what its author meant.
    Invalid {
        /// Position in the array.
        index: usize,
        /// Why the entry is malformed.
        reason: String,
    },
}

/// The classified entries of one `publishers` value, in document order.
#[derive(Debug, Clone)]
pub struct PublisherSet {
    /// One entry per array element.
    pub entries: Vec<PublisherEntry>,
}

/// Why a set compiles to no policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishersRefusal {
    /// No usable entry and at least one unrecognised.
    Unrecognised {
        /// The entries' reasons.
        reasons: Vec<String>,
    },
    /// No usable entry and every entry invalid.
    Invalid {
        /// The entries' reasons.
        reasons: Vec<String>,
    },
}

/// Classify every entry of a raw `publishers` value.
#[must_use]
pub fn parse_publishers(value: &serde_json::Value) -> PublisherSet {
    let entries = match value.as_array() {
        Some(elements) if !elements.is_empty() => elements
            .iter()
            .enumerate()
            .map(|(index, element)| match classify(element) {
                Ok(backend) => PublisherEntry::Usable(backend),
                Err(Refusal::Unrecognised(reason)) => PublisherEntry::Unrecognised { index, reason },
                Err(Refusal::Invalid(reason)) => PublisherEntry::Invalid { index, reason },
            })
            .collect(),
        _ => vec![PublisherEntry::Invalid {
            index: 0,
            reason: "publishers must be a non-empty array".to_owned(),
        }],
    };
    PublisherSet { entries }
}

enum Refusal {
    Unrecognised(String),
    Invalid(String),
}

fn invalid<T>(reason: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::Invalid(reason.into()))
}

fn unrecognised<T>(reason: impl Into<String>) -> Result<T, Refusal> {
    Err(Refusal::Unrecognised(reason.into()))
}

fn classify(element: &Value) -> Result<PolicyBackend, Refusal> {
    let Some(object) = element.as_object() else {
        return invalid("entry is not an object");
    };
    let Some(kind) = object.get("kind").and_then(Value::as_str) else {
        return invalid("entry has no string `kind`");
    };
    match kind {
        "keyless" => classify_keyless(object),
        "key" => classify_key(object),
        other => unrecognised(format!("unknown kind `{other}`")),
    }
}

fn unknown_field(object: &Map<String, Value>, known: &[&str]) -> Result<(), Refusal> {
    match object.keys().find(|field| !known.contains(&field.as_str())) {
        Some(field) => unrecognised(format!("unknown field `{field}`")),
        None => Ok(()),
    }
}

fn string_field<'a>(object: &'a Map<String, Value>, field: &str) -> Result<Option<&'a str>, Refusal> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text)),
        Some(_) => invalid(format!("`{field}` must be a string")),
    }
}

fn classify_keyless(object: &Map<String, Value>) -> Result<PolicyBackend, Refusal> {
    unknown_field(object, &KEYLESS_FIELDS)?;
    let Some(issuer) = string_field(object, "oidc_issuer")? else {
        return invalid("keyless entry has no `oidc_issuer`");
    };
    let identity = string_field(object, "identity")?;
    let pattern = string_field(object, "identity_regexp")?;
    let (claims, unknown_claim) = parse_claims(object.get("claims"))?;
    let rule = match (identity, pattern) {
        (Some(_), Some(_)) => return invalid("`identity` and `identity_regexp` are mutually exclusive"),
        (Some(exact), None) => Some(IdentityRule::Exact(exact.to_owned())),
        (None, Some(pattern)) => match IdentityRule::compile_regex_bounded(pattern, REGEX_SIZE_LIMIT) {
            Ok(rule) => Some(rule),
            Err(error) => return invalid(format!("`identity_regexp` does not compile: {error}")),
        },
        (None, None) => None,
    };
    // A bad claim key above is `Invalid` and wins; an unknown name narrows, and counts as a claim present.
    if let Some(name) = unknown_claim {
        return unrecognised(format!("unknown claim name `{name}`"));
    }
    if rule.is_none() && claims.is_empty() {
        return invalid("keyless entry needs `identity`, `identity_regexp` or non-empty `claims`");
    }
    Ok(PolicyBackend::Keyless(CompiledKeyless {
        identity: rule,
        issuer: issuer.to_owned(),
        claims,
    }))
}

/// The compiled claims, plus the first unknown claim name (which makes the entry unrecognised).
fn parse_claims(value: Option<&Value>) -> Result<(Vec<CompiledClaim>, Option<String>), Refusal> {
    let Some(value) = value else {
        return Ok((Vec::new(), None));
    };
    let Some(table) = value.as_object() else {
        return invalid("`claims` must be an object");
    };
    let mut compiled = Vec::with_capacity(table.len());
    let mut unknown = None;
    for (key, expected) in table {
        let Some(expected) = expected.as_str() else {
            return invalid(format!("claim `{key}` must be a string"));
        };
        match resolve_claim_key(key) {
            Ok(selector) => compiled.push(CompiledClaim {
                selector,
                expected: expected.to_owned(),
            }),
            Err(ClaimKeyError::UnknownName(name)) => {
                unknown.get_or_insert(name);
            }
            Err(error) => return invalid(error.to_string()),
        }
    }
    Ok((compiled, unknown))
}

fn classify_key(object: &Map<String, Value>) -> Result<PolicyBackend, Refusal> {
    // Before the unknown-field rule: a remote document never names a local file, so this is malformed, not newer.
    if object.contains_key("key") {
        return invalid("a remote `key` entry may not name a file; use `key_pem`");
    }
    unknown_field(object, &KEY_FIELDS)?;
    let Some(pem) = string_field(object, "key_pem")? else {
        return invalid("key entry has no `key_pem`");
    };
    if pem.len() as u64 > MAX_KEY_PEM_BYTES {
        return invalid("`key_pem` is larger than any public key");
    }
    match sigstore::crypto::CosignVerificationKey::try_from_pem(pem.as_bytes()) {
        Ok(key) => Ok(PolicyBackend::Key(key)),
        Err(sigstore::errors::SigstoreError::PublicKeyUnsupportedAlgorithmError(reason)) => {
            unrecognised(format!("unsupported key type: {reason}"))
        }
        Err(error) => invalid(format!("`key_pem` is not a public key: {error}")),
    }
}

impl PublisherSet {
    /// One ANY-of policy over the usable entries, scoped to the package.
    ///
    /// # Errors
    /// [`PublishersRefusal`] when no entry is usable.
    pub fn compile(&self) -> Result<CompiledPolicy, PublishersRefusal> {
        let mut backends = Vec::new();
        let mut unrecognised = Vec::new();
        let mut invalid = Vec::new();
        for entry in &self.entries {
            match entry {
                PublisherEntry::Usable(backend) => backends.push(backend.clone()),
                PublisherEntry::Unrecognised { index, reason } => {
                    log::debug!("publishers[{index}] dropped as unrecognised: {reason}");
                    unrecognised.push(reason.clone());
                }
                PublisherEntry::Invalid { index, reason } => {
                    log::debug!("publishers[{index}] dropped as invalid: {reason}");
                    invalid.push(reason.clone());
                }
            }
        }
        if !backends.is_empty() {
            return Ok(CompiledPolicy {
                builder: None,
                backends,
            });
        }
        if unrecognised.is_empty() {
            Err(PublishersRefusal::Invalid { reasons: invalid })
        } else {
            unrecognised.extend(invalid);
            Err(PublishersRefusal::Unrecognised { reasons: unrecognised })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    const ISSUER: &str = "https://token.actions.githubusercontent.com";
    const P256_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEHKf0zqUEudwCix/9rS4Fn7nEjpG3\nIJt/JQukjC1J2EuX6CcB6cX/2wtIoc1arumQbrlNjFWMT6QAz3SEHOOcjA==\n-----END PUBLIC KEY-----\n";
    const ED448_PEM: &str = "-----BEGIN PUBLIC KEY-----\nMEMwBQYDK2VxAzoAVXdw6t7ayeK2mu4c+aWDASnG8VT7IpdgP4jk+eHt1ayxWfy2\nrQUXvvdQBCRmU+9KDj3pp6oszTkA\n-----END PUBLIC KEY-----\n";

    #[derive(Debug, PartialEq, Eq)]
    enum Class {
        Usable,
        Unrecognised,
        Invalid,
    }

    fn classes(value: &Value) -> Vec<Class> {
        parse_publishers(value)
            .entries
            .iter()
            .map(|entry| match entry {
                PublisherEntry::Usable(_) => Class::Usable,
                PublisherEntry::Unrecognised { .. } => Class::Unrecognised,
                PublisherEntry::Invalid { .. } => Class::Invalid,
            })
            .collect()
    }

    fn one(entry: Value) -> Class {
        let mut all = classes(&json!([entry]));
        assert_eq!(all.len(), 1);
        all.remove(0)
    }

    /// The 64 KiB cap, not the regex crate's 10 MiB default, refuses this pattern.
    ///
    /// *Red-reachability:* raising `REGEX_SIZE_LIMIT` to the default makes the entry usable.
    #[test]
    fn identity_regexp_between_the_cap_and_the_default_limit_is_refused() {
        const PATTERN: &str = "(a{100}){100}";
        regex::Regex::new(PATTERN).expect("compiles under the default size limit");
        assert_eq!(
            one(json!({"kind": "keyless", "oidc_issuer": ISSUER, "identity_regexp": PATTERN})),
            Class::Invalid
        );
    }

    /// one row per rule of the design's table.
    #[test]
    fn c004_entry_classification_table() {
        let keyless = |extra: Value| {
            let mut entry = json!({"kind": "keyless", "oidc_issuer": ISSUER});
            entry
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            entry
        };
        let rows: Vec<(&str, Value, Class)> = vec![
            ("identity", keyless(json!({"identity": "a@b.example"})), Class::Usable),
            ("regexp", keyless(json!({"identity_regexp": "^a$"})), Class::Usable),
            (
                "claims alone",
                keyless(json!({"claims": {"source_repository_identifier": "1"}})),
                Class::Usable,
            ),
            (
                "claim by raw OID",
                keyless(json!({"claims": {"1.3.6.1.4.1.57264.1.15": "1"}})),
                Class::Usable,
            ),
            (
                "claims beside identity",
                keyless(json!({"identity": "a", "claims": {"build_trigger": "push"}})),
                Class::Usable,
            ),
            ("key_pem", json!({"kind": "key", "key_pem": P256_PEM}), Class::Usable),
            ("unknown kind", json!({"kind": "future"}), Class::Unrecognised),
            (
                "unknown field (sigstore)",
                keyless(json!({"identity": "a", "sigstore": {}})),
                Class::Unrecognised,
            ),
            ("not an object", json!("keyless"), Class::Invalid),
            ("no kind", json!({"identity": "a"}), Class::Invalid),
            ("no issuer", json!({"kind": "keyless", "identity": "a"}), Class::Invalid),
            (
                "identity and regexp",
                keyless(json!({"identity": "a", "identity_regexp": "a"})),
                Class::Invalid,
            ),
            ("no identity form", keyless(json!({})), Class::Invalid),
            ("empty claims", keyless(json!({"claims": {}})), Class::Invalid),
            ("non-string identity", keyless(json!({"identity": 7})), Class::Invalid),
            ("bad regexp", keyless(json!({"identity_regexp": "("})), Class::Invalid),
            (
                "unknown claim name",
                keyless(json!({"claims": {"future_claim": "x"}})),
                Class::Unrecognised,
            ),
            (
                "claim outside the arc",
                keyless(json!({"claims": {"1.2.3": "x"}})),
                Class::Invalid,
            ),
            (
                "claim .1.7",
                keyless(json!({"claims": {"1.3.6.1.4.1.57264.1.7": "x"}})),
                Class::Invalid,
            ),
            (
                "non-string claim value",
                keyless(json!({"claims": {"build_trigger": 1}})),
                Class::Invalid,
            ),
            (
                "claims not an object",
                keyless(json!({"identity": "a", "claims": ["x"]})),
                Class::Invalid,
            ),
            (
                "regexp over the size limit",
                keyless(json!({"identity_regexp": "(a{1000}){1000}"})),
                Class::Invalid,
            ),
            (
                "bad claim beside an unknown one",
                keyless(json!({"claims": {"future_claim": "x", "1.2.3": "y"}})),
                Class::Invalid,
            ),
            (
                "key field beside an unknown field",
                json!({"kind": "key", "key": "/etc/k.pub", "future": 1}),
                Class::Invalid,
            ),
            (
                "key with key_pem and a path",
                json!({"kind": "key", "key": "/etc/k.pub", "key_pem": P256_PEM}),
                Class::Invalid,
            ),
            (
                "key unknown field",
                json!({"kind": "key", "key_pem": P256_PEM, "future": 1}),
                Class::Unrecognised,
            ),
            (
                "unsupported key type",
                json!({"kind": "key", "key_pem": ED448_PEM}),
                Class::Unrecognised,
            ),
            (
                "non-string key_pem",
                json!({"kind": "key", "key_pem": 5}),
                Class::Invalid,
            ),
            (
                "key with a path",
                json!({"kind": "key", "key": "/etc/k.pub"}),
                Class::Invalid,
            ),
            ("key without key_pem", json!({"kind": "key"}), Class::Invalid),
            (
                "unparsable key_pem",
                json!({"kind": "key", "key_pem": "not a pem"}),
                Class::Invalid,
            ),
        ];
        for (name, entry, expected) in rows {
            assert_eq!(one(entry), expected, "{name}");
        }
    }

    /// the value itself must be a non-empty array; a mixed list keeps order.
    #[test]
    fn c004_value_shape_and_mixed_list() {
        for value in [json!(null), json!({}), json!("x"), json!(7), json!([])] {
            assert_eq!(classes(&value), vec![Class::Invalid], "{value}");
        }
        assert_eq!(
            classes(&json!([
                {"kind": "future"},
                {"kind": "keyless", "oidc_issuer": ISSUER, "identity": "a"},
                5
            ])),
            vec![Class::Unrecognised, Class::Usable, Class::Invalid]
        );
    }

    /// usable beside unrecognised compiles; only unrecognised and only invalid refuse by kind.
    #[test]
    fn c005_compile_refusals() {
        let usable = json!({"kind": "keyless", "oidc_issuer": ISSUER, "identity": "a"});
        let policy = parse_publishers(&json!([{"kind": "future"}, usable]))
            .compile()
            .expect("a usable entry compiles beside an unrecognised one");
        assert_eq!(policy.backends.len(), 1);
        assert!(policy.builder.is_none());
        assert!(matches!(
            parse_publishers(&json!([{"kind": "future"}])).compile(),
            Err(PublishersRefusal::Unrecognised { .. })
        ));
        assert!(matches!(
            parse_publishers(&json!([5])).compile(),
            Err(PublishersRefusal::Invalid { .. })
        ));
        assert!(matches!(
            parse_publishers(&json!([5, {"kind": "future"}])).compile(),
            Err(PublishersRefusal::Unrecognised { .. })
        ));
    }

    /// a remote entry naming a file is refused without reading it, whatever the path is.
    #[test]
    fn c004_key_field_never_reads_a_file() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), P256_PEM).unwrap();
        let entry = json!({"kind": "key", "key": file.path().to_str().unwrap()});
        assert_eq!(one(entry), Class::Invalid);
    }
}
