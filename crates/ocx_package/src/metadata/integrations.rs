// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Vendor-namespaced configuration blocks for tools OCX does not model, such as
//! an editor extension list or a devcontainer fragment.
//!
//! OCX validates the container (key grammar, size, its own `${…}` tokens), never
//! the payload, and never merges two packages' blocks. ADR: `adr_package_integrations.md`.

use crate::error::Error as PackageError;
use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::template::{AllowedTokens, TemplateError, TemplateResolver};

/// Maximum compact-serialized size of one namespace's payload.
// Raise-only: the cap is on the read path, so lowering it un-resolves published packages.
pub const MAX_INTEGRATION_NAMESPACE_BYTES: usize = 8 * 1024;

/// Maximum compact-serialized size of the whole `integrations` map, keys included.
// Raise-only, like `MAX_INTEGRATION_NAMESPACE_BYTES`.
pub const MAX_INTEGRATIONS_BYTES: usize = 32 * 1024;

/// Maximum byte length of one namespace key.
pub(super) const MAX_NAMESPACE_BYTES: usize = 128;

/// Codepoints that render as nothing, refused in a namespace key: a key prints verbatim
/// into the availability hint, where one would display as another namespace (CWE-451).
// Both properties, since neither contains the other (U+3164 is only default-ignorable,
// U+0600 only `Cf`). U+2800 BRAILLE BLANK is in neither and knowingly accepted.
static INVISIBLE_CHARACTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\p{Cf}\p{Default_Ignorable_Code_Point}]").expect("valid invisible-character regex"));

/// The token classes an integrations payload may carry: `${deps.*}`, never `${self.env.*}`,
/// which would ship a `private` var's value to every consumer.
// Shared with the compose-time resolvers: a hostile registry skips the publish gate, so
// compose is the only check a published package meets.
pub const INTEGRATION_TOKENS: AllowedTokens = AllowedTokens {
    deps: true,
    self_env: false,
};

/// A package's declared `integrations` map: namespace key → opaque payload.
///
/// Absent on the wire and empty are the **same** state: nothing distinguishes
/// "declares none" from "did not say".
// Keys must iterate in lexicographic order, or composed rows, cap-violation reports and closure
// namespace lists stop being deterministic across runs and platforms.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Integrations(BTreeMap<String, serde_json::Value>);

impl Integrations {
    /// Whether the package declares no integrations at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Namespaces and raw payloads, in lexicographic namespace order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &serde_json::Value)> + use<'_> {
        self.0.iter().map(|(namespace, payload)| (namespace.as_str(), payload))
    }

    /// The raw payload declared for `namespace`, if any.
    pub fn get(&self, namespace: &str) -> Option<&serde_json::Value> {
        self.0.get(namespace)
    }

    /// Resolves every payload; `resolver` must carry the declaring package's paths, never
    /// the consuming root's.
    ///
    /// # Errors
    ///
    /// [`crate::error::Error::IntegrationInterpolation`], naming the namespace whose
    /// string leaf failed.
    pub fn resolve(&self, resolver: &TemplateResolver<'_>) -> Result<Vec<IntegrationEntry>, PackageError> {
        self.iter()
            .map(|(namespace, payload)| {
                let payload =
                    interpolate(payload, resolver).map_err(|source| crate::error::Error::IntegrationInterpolation {
                        namespace: namespace.to_owned(),
                        source,
                    })?;
                Ok(IntegrationEntry {
                    namespace: namespace.to_owned(),
                    payload,
                })
            })
            .collect()
    }
}

/// One resolved integration: the namespace and its interpolated payload; the declaring
/// package travels in the enclosing pair.
#[derive(Debug, Clone)]
pub struct IntegrationEntry {
    pub namespace: String,
    pub payload: serde_json::Value,
}

/// Refuses an empty or over-long key, or one holding a control, invisible or whitespace
/// character; reverse-DNS is convention only, and case-distinct keys are distinct.
///
/// # Errors
///
/// [`crate::error::Error::IntegrationNamespaceInvalid`].
// Error paths are cold, so boxing for `result_large_err` would only allocate on the hot `Ok`.
#[allow(clippy::result_large_err)]
pub(super) fn validate_namespace(namespace: &str) -> Result<(), crate::error::Error> {
    use crate::error::Error;

    let refuse = |reason| {
        Err(Error::IntegrationNamespaceInvalid {
            namespace: namespace.to_owned(),
            reason,
        })
    };

    if namespace.is_empty() {
        return refuse("empty");
    }
    if namespace.len() > MAX_NAMESPACE_BYTES {
        return refuse("longer than 128 bytes");
    }
    // Control before whitespace: U+0085 is both, and "control character" is the accurate reason.
    if namespace.chars().any(char::is_control) {
        return refuse("contains a control character");
    }
    if INVISIBLE_CHARACTER.is_match(namespace) {
        return refuse("contains an invisible character");
    }
    if namespace.chars().any(char::is_whitespace) {
        return refuse("contains whitespace");
    }

    Ok(())
}

/// Every string leaf of `payload` in document order: exactly the positions [`interpolate`] touches.
pub(super) fn string_leaves(payload: &serde_json::Value) -> Vec<&str> {
    let mut leaves = Vec::new();
    collect_string_leaves(payload, &mut leaves);
    leaves
}

fn collect_string_leaves<'a>(payload: &'a serde_json::Value, leaves: &mut Vec<&'a str>) {
    match payload {
        serde_json::Value::String(text) => leaves.push(text),
        serde_json::Value::Array(items) => items.iter().for_each(|item| collect_string_leaves(item, leaves)),
        serde_json::Value::Object(entries) => entries.values().for_each(|entry| collect_string_leaves(entry, leaves)),
        _ => {}
    }
}

/// Resolves tokens in every string leaf of `payload`; keys and scalars pass through.
///
/// # Errors
///
/// The bare [`TemplateError`]; [`Integrations::resolve`] attaches the namespace.
// Substituted in place, never re-parsed as JSON, or a resolved path could change the structure.
// `result_large_err`: see `validate_namespace`.
#[allow(clippy::result_large_err)]
fn interpolate(
    payload: &serde_json::Value,
    resolver: &TemplateResolver<'_>,
) -> Result<serde_json::Value, TemplateError> {
    Ok(match payload {
        serde_json::Value::String(text) => serde_json::Value::String(resolver.resolve(text)?),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|item| interpolate(item, resolver))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        // Keys are cloned, never resolved: only the payload recurses.
        serde_json::Value::Object(members) => serde_json::Value::Object(
            members
                .iter()
                .map(|(key, member)| Ok((key.clone(), interpolate(member, resolver)?)))
                .collect::<Result<serde_json::Map<_, _>, _>>()?,
        ),
        scalar => scalar.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tempfile::TempDir;

    use super::*;
    use crate::error::Error;
    use crate::metadata::dependency::DependencyName;
    use crate::metadata::env::dep_context::DependencyContext;
    use crate::metadata::env::entry::Entry;
    use crate::metadata::env::modifier::ModifierKind;
    use crate::metadata::template::SelfEnvScope;

    // ── C-005: namespace key grammar ────────────────────────────────────────

    #[test]
    fn empty_namespace_is_rejected() {
        let err = validate_namespace("").expect_err("empty namespace must be rejected");
        assert!(
            matches!(&err, Error::IntegrationNamespaceInvalid { namespace, reason }
                if namespace.is_empty() && reason.contains("empty")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn namespace_at_128_bytes_is_accepted() {
        let namespace = "a".repeat(MAX_NAMESPACE_BYTES);
        assert!(
            validate_namespace(&namespace).is_ok(),
            "exactly 128 bytes must pass (inclusive boundary)"
        );
    }

    #[test]
    fn namespace_over_128_bytes_is_rejected() {
        let namespace = "a".repeat(MAX_NAMESPACE_BYTES + 1);
        let err = validate_namespace(&namespace).expect_err("129 bytes must be rejected");
        assert!(
            matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("128")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn namespace_with_c0_control_character_is_rejected() {
        for namespace in ["a\nb", "a\tb", "a\0b"] {
            let err = validate_namespace(namespace).expect_err("C0 control char must be rejected");
            assert!(
                matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("control")),
                "unexpected error for {namespace:?}: {err}"
            );
        }
    }

    #[test]
    fn namespace_with_del_is_rejected() {
        let err = validate_namespace("a\u{007F}b").expect_err("DEL must be rejected");
        assert!(
            matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("control")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn namespace_with_c1_control_character_is_rejected() {
        // U+0085 (NEL) is a C1 control character — Unicode char::is_control() is
        // true for it even though it sits outside the ASCII C0/DEL ranges.
        let err = validate_namespace("a\u{0085}b").expect_err("C1 control char must be rejected");
        assert!(
            matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("control")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn namespace_with_bidi_override_is_rejected() {
        // U+202E RIGHT-TO-LEFT OVERRIDE — the Trojan Source (CWE-451) example
        // named in the ADR's security-review rationale.
        let err = validate_namespace("com.evil\u{202E}txt.moc").expect_err("bidi override must be rejected");
        assert!(
            matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("invisible")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn namespace_with_any_invisible_character_is_rejected() {
        // An INDEPENDENT list, deliberately not read from anything the
        // implementation consults: a guard that iterates the very table the
        // check consults passes identically whether that table holds twelve
        // entries or two, so it can never red on an omitted codepoint.
        //
        // Two properties, because neither contains the other and a list drawn
        // from `Cf` alone goes green against a `\p{Cf}`-only check — which is
        // how the HANGUL FILLER spoof survived the first fix. The trailing
        // entries are default-ignorable and NOT `Cf`: `Lo` fillers (the
        // blank-character trick) and `Mn` variation selectors.
        //
        // None is `char::is_control` (Cc is disjoint from both properties) and
        // none is `char::is_whitespace` (no Cf codepoint carries White_Space —
        // U+200B and U+180E both lost it — and Default_Ignorable_Code_Point
        // subtracts it by definition), so each one either reaches the
        // invisible-character check or nothing catches it at all.
        const INVISIBLE_CODEPOINTS: &[(char, &str)] = &[
            ('\u{00AD}', "SOFT HYPHEN"),
            ('\u{061C}', "ARABIC LETTER MARK"),
            ('\u{070F}', "SYRIAC ABBREVIATION MARK"),
            ('\u{180E}', "MONGOLIAN VOWEL SEPARATOR"),
            ('\u{200B}', "ZERO WIDTH SPACE"),
            ('\u{200C}', "ZERO WIDTH NON-JOINER"),
            ('\u{200D}', "ZERO WIDTH JOINER"),
            ('\u{200E}', "LEFT-TO-RIGHT MARK"),
            ('\u{200F}', "RIGHT-TO-LEFT MARK"),
            ('\u{202A}', "LEFT-TO-RIGHT EMBEDDING"),
            ('\u{202B}', "RIGHT-TO-LEFT EMBEDDING"),
            ('\u{202C}', "POP DIRECTIONAL FORMATTING"),
            ('\u{202D}', "LEFT-TO-RIGHT OVERRIDE"),
            ('\u{202E}', "RIGHT-TO-LEFT OVERRIDE"),
            ('\u{2060}', "WORD JOINER"),
            ('\u{2061}', "FUNCTION APPLICATION"),
            ('\u{2062}', "INVISIBLE TIMES"),
            ('\u{2063}', "INVISIBLE SEPARATOR"),
            ('\u{2064}', "INVISIBLE PLUS"),
            ('\u{2066}', "LEFT-TO-RIGHT ISOLATE"),
            ('\u{2067}', "RIGHT-TO-LEFT ISOLATE"),
            ('\u{2068}', "FIRST STRONG ISOLATE"),
            ('\u{2069}', "POP DIRECTIONAL ISOLATE"),
            ('\u{FEFF}', "ZERO WIDTH NO-BREAK SPACE (BOM)"),
            ('\u{FFF9}', "INTERLINEAR ANNOTATION ANCHOR"),
            ('\u{FFFA}', "INTERLINEAR ANNOTATION SEPARATOR"),
            ('\u{FFFB}', "INTERLINEAR ANNOTATION TERMINATOR"),
            ('\u{110BD}', "KAITHI NUMBER SIGN"),
            ('\u{E0001}', "LANGUAGE TAG"),
            ('\u{E0061}', "TAG LATIN SMALL LETTER A"),
            // Default_Ignorable_Code_Point, outside Cf.
            ('\u{115F}', "HANGUL CHOSEONG FILLER (Lo)"),
            ('\u{1160}', "HANGUL JUNGSEONG FILLER (Lo)"),
            ('\u{17B4}', "KHMER VOWEL INHERENT AQ (Mn)"),
            ('\u{3164}', "HANGUL FILLER (Lo)"),
            ('\u{FE00}', "VARIATION SELECTOR-1 (Mn)"),
            ('\u{FE0F}', "VARIATION SELECTOR-16 (Mn)"),
            ('\u{FFA0}', "HALFWIDTH HANGUL FILLER (Lo)"),
            ('\u{E0100}', "VARIATION SELECTOR-17 (Mn)"),
        ];

        for &(codepoint, name) in INVISIBLE_CODEPOINTS {
            // The spoof this closes: `com.microsoft<invisible>.vscode` renders
            // as the real key in the plain availability hint and the closure
            // tree.
            let namespace = format!("com.microsoft{codepoint}.vscode");
            let Err(err) = validate_namespace(&namespace) else {
                panic!("U+{:04X} {name} must be rejected", codepoint as u32);
            };
            assert!(
                matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("invisible")),
                "U+{:04X} {name} must be refused as an invisible character: {err}",
                codepoint as u32
            );
        }
    }

    #[test]
    fn namespace_with_ascii_space_is_rejected() {
        let err = validate_namespace("com.foo bar").expect_err("space must be rejected");
        assert!(
            matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("whitespace")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn namespace_with_unicode_nbsp_is_rejected() {
        // U+00A0 NO-BREAK SPACE — Unicode char::is_whitespace() is true, not
        // caught by an ASCII-only whitespace check.
        let err = validate_namespace("com.foo\u{00A0}bar").expect_err("NBSP must be rejected");
        assert!(
            matches!(&err, Error::IntegrationNamespaceInvalid { reason, .. } if reason.contains("whitespace")),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn valid_reverse_dns_namespace_is_accepted() {
        for namespace in ["com.microsoft.vscode", "com.jetbrains", "sh.ocx.completions"] {
            assert!(validate_namespace(namespace).is_ok(), "{namespace:?} must be accepted");
        }
    }

    #[test]
    fn non_reverse_dns_but_otherwise_legal_namespaces_are_accepted() {
        // D6: reverse-DNS is a documented convention, not an enforced grammar.
        // The non-ASCII entries double as the over-rejection canary for the
        // invisible-character check: `com.微软` is Lo, `cafe\u{0301}` carries a
        // combining mark (Mn) and `com.🚀` is So — none is `Cf` and none is
        // default-ignorable, and a check that reached for "non-ASCII", or for
        // the general categories the refused set happens to span, would refuse
        // all three. `Lo` and `Mn` are exactly the categories the widened set
        // reaches into (the fillers and the variation selectors), so these two
        // are the entries that red on a category-shaped over-reach.
        //
        // U+2800 BRAILLE PATTERN BLANK is the knowingly-accepted one: it renders
        // blank in most fonts but is in neither refused property, and it is a
        // legitimate Braille character. Pinned here rather than left as prose,
        // since the grammar is loosen-only — refusing it later is not available.
        for namespace in [
            "vscode",
            "VSCode",
            "com.微软",
            "com.cafe\u{0301}",
            "com.\u{2800}",
            "com.🚀",
            "a",
            "x/y",
            "123",
        ] {
            assert!(validate_namespace(namespace).is_ok(), "{namespace:?} must be accepted");
        }
    }

    #[test]
    fn case_distinct_namespaces_are_both_legal() {
        // Case is preserved; case-distinct keys are two distinct namespaces —
        // unlike BinaryName, there is no case-fold-collision check.
        assert!(validate_namespace("Foo").is_ok());
        assert!(validate_namespace("foo").is_ok());
    }

    // ── C-006: cap constants (ADR §3.3 — 8 KiB / 32 KiB) ────────────────────
    //
    // Pins the ADR-specified values; the boundary CHECK itself is exercised
    // end-to-end against `ValidMetadata::try_from` in `validation.rs` — the
    // cap check lives inline in `validate_integrations`, not as a
    // standalone function here.

    #[test]
    fn cap_constants_match_the_adr_values() {
        assert_eq!(MAX_INTEGRATION_NAMESPACE_BYTES, 8 * 1024);
        assert_eq!(MAX_INTEGRATIONS_BYTES, 32 * 1024);
    }

    // ── Integrations::is_empty / get ──────────────────────────────────────

    #[test]
    fn default_integrations_is_empty() {
        assert!(Integrations::default().is_empty());
    }

    #[test]
    fn duplicate_namespace_keys_use_last_wins_semantics() {
        // A hand-authored metadata.json could declare a namespace key twice.
        // serde_json's map builder is last-wins for duplicate object keys, and
        // `Integrations` does not special-case this the way `Entrypoints`'s
        // custom `Deserialize` rejects duplicates — a documented constitution
        // deviation. Built from a raw JSON string (not `serde_json::json!`,
        // whose own map builder would already collapse the duplicate before
        // `Integrations` ever saw it).
        let json = r#"{"com.foo":{"a":1},"com.foo":{"b":2}}"#;
        let integrations: Integrations = serde_json::from_str(json).unwrap();
        assert_eq!(
            integrations.iter().count(),
            1,
            "duplicate namespace keys must collapse to one composed row"
        );
        assert_eq!(
            integrations.get("com.foo"),
            Some(&serde_json::json!({ "b": 2 })),
            "the last-declared payload must win"
        );
    }

    #[test]
    fn get_returns_the_declared_payload() {
        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": { "a": 1 } })).unwrap();
        let payload = integrations.get("com.example").expect("namespace declared");
        assert_eq!(payload, &serde_json::json!({ "a": 1 }));
    }

    #[test]
    fn get_returns_none_for_an_undeclared_namespace() {
        let integrations: Integrations = serde_json::from_value(serde_json::json!({ "com.example": "value" })).unwrap();
        assert_eq!(integrations.get("com.other"), None);
    }

    // ── C-009: interpolation walker (via Integrations::resolve) ──────────

    #[test]
    fn resolve_interpolates_string_leaves_only() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let integrations: Integrations = serde_json::from_value(serde_json::json!({
            "com.example": {
                "path": "${installPath}/bin",
                "count": 3,
                "enabled": true,
                "missing": null,
            }
        }))
        .unwrap();

        let resolved = integrations.resolve(&resolver).expect("resolves");
        let entry = resolved
            .iter()
            .find(|e| e.namespace == "com.example")
            .expect("namespace present");
        let install = dir.path().to_string_lossy();
        assert_eq!(entry.payload["path"], serde_json::json!(format!("{install}/bin")));
        assert_eq!(entry.payload["count"], serde_json::json!(3));
        assert_eq!(entry.payload["enabled"], serde_json::json!(true));
        assert_eq!(entry.payload["missing"], serde_json::Value::Null);
    }

    #[test]
    fn resolve_interpolates_deps_tokens_from_a_non_empty_dep_context_map() {
        // C-008: every other resolve test in this module builds `HashMap::new()`
        // for `dep_contexts` — so if the caller that assembles the real map
        // (wiring the declaring package's own direct dependencies) passed an
        // empty one instead, no test here would notice: `${deps.*}` would
        // hard-fail `UnknownDependencyRef` in every one of them alike. This
        // test uses a REAL, non-empty context and asserts two things a
        // hard-coded or swapped map would get wrong: the `${deps.*}` leaf
        // resolves to the DEPENDENCY's install path, and `${installPath}` in
        // the SAME payload still resolves to the DECLARING package's own path
        // (mirrors `template.rs::dep_install_path_substitution`).
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();
        let hex = "a".repeat(64);
        let id: ocx_oci::PackageRef = format!("ocx.sh/dep1:1.0@sha256:{hex}").parse().unwrap();
        let pinned = ocx_oci::PinnedPackageRef::try_from(id).unwrap();

        let mut contexts = HashMap::new();
        contexts.insert(
            DependencyName::try_from("dep1").unwrap(),
            DependencyContext::path_only(pinned, dep_dir.path().to_path_buf()),
        );
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let integrations: Integrations = serde_json::from_value(serde_json::json!({
            "com.example": {
                "depPath": "${deps.dep1.installPath}/bin",
                "ownPath": "${installPath}/share",
            }
        }))
        .unwrap();

        let resolved = integrations.resolve(&resolver).expect("resolves");
        let entry = &resolved[0];
        let dep_path = dep_dir.path().to_string_lossy();
        let self_path = self_dir.path().to_string_lossy();
        assert_eq!(entry.payload["depPath"], serde_json::json!(format!("{dep_path}/bin")));
        assert_eq!(
            entry.payload["ownPath"],
            serde_json::json!(format!("{self_path}/share"))
        );
    }

    #[test]
    fn resolve_error_names_the_declaring_namespace_not_a_constant_or_wrong_one() {
        // `resolve`'s map_err attaches `namespace` from the SAME loop
        // iteration that produced the fault. Replacing it with a constant
        // string (or the first namespace, or a stale one) reds nothing unless
        // a test uses two namespaces where only the LATER one fails —
        // BTreeMap iterates lexicographically, so "aaa.first" resolves clean
        // before "zzz.offender" fails.
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let integrations: Integrations = serde_json::from_value(serde_json::json!({
            "aaa.first": "no tokens here",
            "zzz.offender": "${deps.missing.installPath}",
        }))
        .unwrap();

        let err = integrations
            .resolve(&resolver)
            .expect_err("undeclared dep ref must fail");
        let message = err.to_string();
        assert!(
            message.contains("zzz.offender"),
            "expected the OFFENDING namespace named: {message}"
        );
        assert!(
            !message.contains("aaa.first"),
            "must not name the wrong (non-failing) namespace: {message}"
        );
    }

    #[test]
    fn resolve_leaves_object_keys_untouched() {
        // Object keys are never interpolated, even if they look like a token —
        // only string VALUES are walked (C-009 / D11).
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": { "${installPath}": "literal-payload" } }))
                .unwrap();

        let resolved = integrations.resolve(&resolver).expect("resolves");
        let entry = &resolved[0];
        assert!(
            entry.payload.as_object().unwrap().contains_key("${installPath}"),
            "object key must not be interpolated: {:?}",
            entry.payload
        );
    }

    // ── E-06b / E-07: an unrecognized token OUTSIDE a string leaf ──────────
    //
    // The closed-world grammar (D3) refuses an unrecognized `${…}` — but only
    // where the engine actually resolves, and `collect_string_leaves` walks
    // `Object(entries).values()`, never keys. `validate_integration_tokens`
    // scans `string_leaves(payload)` per namespace, so an object key and the
    // namespace key itself are both outside the scanned set by construction.
    // These two pin that invariant against a future "the walker misses keys"
    // completeness fix, which would start refusing payloads that publish today.

    #[test]
    fn an_unrecognized_token_in_an_object_key_is_not_a_leaf() {
        let payload = serde_json::json!({ "${workspaceFolder}": "literal-payload" });

        assert_eq!(
            string_leaves(&payload),
            vec!["literal-payload"],
            "an object key must never be yielded as a string leaf: {payload:?}"
        );

        // Same fact from the resolution side: a payload whose only `${…}` sits
        // in a key resolves clean, no refusal, key preserved verbatim.
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);
        let integrations: Integrations = serde_json::from_value(serde_json::json!({ "com.example": payload })).unwrap();

        let resolved = integrations
            .resolve(&resolver)
            .expect("a token in an object key must not be refused");
        assert!(
            resolved[0]
                .payload
                .as_object()
                .unwrap()
                .contains_key("${workspaceFolder}"),
            "the key must survive verbatim: {:?}",
            resolved[0].payload
        );
    }

    #[test]
    fn an_unrecognized_token_in_a_namespace_key_is_not_a_leaf() {
        // The namespace grammar admits `$`, `{` and `}` — nothing in C-005
        // refuses them — so this key is legal, and no scan reaches it.
        let namespace = "com.${workspaceFolder}";
        assert!(
            validate_namespace(namespace).is_ok(),
            "the namespace grammar has no token rule"
        );

        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);
        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ namespace: "no tokens here" })).unwrap();

        let payload = integrations.get(namespace).expect("namespace declared");
        assert_eq!(
            string_leaves(payload),
            vec!["no tokens here"],
            "the namespace key is not part of the payload it maps to"
        );

        let resolved = integrations
            .resolve(&resolver)
            .expect("a token in a namespace key must not be refused");
        assert_eq!(
            resolved[0].namespace, namespace,
            "the namespace must survive verbatim, uninterpolated"
        );
    }

    #[test]
    fn resolve_interpolates_nested_array_and_object_leaves() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let integrations: Integrations = serde_json::from_value(serde_json::json!({
            "com.example": {
                "list": ["${installPath}/a", "literal", "${installPath}/b"],
                "nested": { "inner": ["${installPath}/c"] },
            }
        }))
        .unwrap();

        let resolved = integrations.resolve(&resolver).expect("resolves");
        let entry = &resolved[0];
        let install = dir.path().to_string_lossy();
        assert_eq!(
            entry.payload["list"],
            serde_json::json!([format!("{install}/a"), "literal", format!("{install}/b")])
        );
        assert_eq!(
            entry.payload["nested"]["inner"][0],
            serde_json::json!(format!("{install}/c"))
        );
    }

    #[test]
    fn resolve_preserves_json_structure() {
        // Structure invariance (§5.5): key set, array lengths, payload types are
        // identical before and after interpolation — the output is never
        // re-parsed as JSON.
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let payload = serde_json::json!({
            "a": ["${installPath}", 1, false, null],
            "b": { "c": "${installPath}" },
        });
        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": payload.clone() })).unwrap();

        let resolved = integrations.resolve(&resolver).expect("resolves");
        let entry = &resolved[0];
        assert_eq!(
            entry.payload["a"].as_array().unwrap().len(),
            payload["a"].as_array().unwrap().len()
        );
        assert!(entry.payload["a"][1].is_number());
        assert!(entry.payload["a"][2].is_boolean());
        assert!(entry.payload["a"][3].is_null());
        assert!(entry.payload["b"].is_object());
    }

    #[test]
    fn resolve_handles_a_bare_string_payload() {
        // §3.2: a non-object payload is legal. A bare-string payload is a
        // single string leaf, interpolated like any other.
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": "${installPath}/x" })).unwrap();

        let resolved = integrations.resolve(&resolver).expect("resolves");
        let install = dir.path().to_string_lossy();
        assert_eq!(resolved[0].payload, serde_json::json!(format!("{install}/x")));
    }

    #[test]
    fn resolve_refuses_an_unrecognized_token_rather_than_passing_it_through() {
        // A payload is not exempt from the closed-world grammar: OCX claims
        // every `${…}` in package metadata, so a foreign token — a VS Code
        // `${workspaceFolder}`, a devcontainer `${localEnv:HOME}` — is refused
        // here exactly as it is in an env payload. The author writes `$${…}` to
        // mean a literal; see the escape sibling directly below.
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": "${workspaceFolder}" })).unwrap();

        let err = integrations
            .resolve(&resolver)
            .expect_err("an unrecognized token must be refused, not passed through");
        let message = err.to_string();
        assert!(
            message.contains("${workspaceFolder}"),
            "the refusal must name the offending token: {message}"
        );
    }

    // ── H-2: `${self.env.*}` is refused by the GATE, not by an empty scope ──

    #[test]
    fn resolve_refuses_a_self_env_token_against_a_scope_that_defines_the_key() {
        // Every other refusal of this token is ambiguous: the composer builds
        // its resolver with no self-env scope, so `${self.env.KEY}` fails as
        // `UndefinedSelfEnvRef` whether or not anything gates the class. This
        // one supplies a scope that DOES define the key, which leaves
        // `INTEGRATION_TOKENS` as the only reason to refuse — drop the
        // `.usage(...)` and the payload resolves, publishing a payload the author
        // declared `private` on the interface surface (CWE-200).
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let mut scope = SelfEnvScope::new();
        scope.push(Entry {
            key: "SECRET".to_owned(),
            value: "s3cr3t".to_owned(),
            kind: ModifierKind::Constant,
            separator: None,
        });
        let resolver = TemplateResolver::new(dir.path(), &contexts)
            .usage(INTEGRATION_TOKENS)
            .with_self_env(&scope);

        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": "${self.env.SECRET}" })).unwrap();

        let err = integrations
            .resolve(&resolver)
            .expect_err("a self-env token must be refused on the integrations surface");
        let message = err.to_string();
        assert!(
            message.contains("not permitted"),
            "expected the capability gate's refusal: {message}"
        );
        assert!(
            !message.contains("s3cr3t"),
            "the referenced payload must never be substituted: {message}"
        );
    }

    #[test]
    fn resolve_still_permits_a_deps_token_under_the_integrations_gate() {
        // The over-narrowing canary for the constant above: `${deps.*}` is the
        // point of the feature — a digest-derived path no publisher can
        // hand-write — so a gate that refused it would be the same class of
        // regression in the other direction, and no test on `self_env` alone
        // would notice.
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();
        let hex = "a".repeat(64);
        let id: ocx_oci::PackageRef = format!("ocx.sh/dep1:1.0@sha256:{hex}").parse().unwrap();
        let pinned = ocx_oci::PinnedPackageRef::try_from(id).unwrap();

        let mut contexts = HashMap::new();
        contexts.insert(
            DependencyName::try_from("dep1").unwrap(),
            DependencyContext::path_only(pinned, dep_dir.path().to_path_buf()),
        );
        let resolver = TemplateResolver::new(self_dir.path(), &contexts).usage(INTEGRATION_TOKENS);

        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": "${deps.dep1.installPath}/bin" })).unwrap();

        let resolved = integrations.resolve(&resolver).expect("a deps token must resolve");
        let dep_path = dep_dir.path().to_string_lossy();
        assert_eq!(resolved[0].payload, serde_json::json!(format!("{dep_path}/bin")));
    }

    // ── C-009b / D10: `$${...}` escape, exercised through the walker ───────

    #[test]
    fn resolve_escapes_doubled_dollar_to_a_literal_token() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let integrations: Integrations =
            serde_json::from_value(serde_json::json!({ "com.example": "$${installPath}" })).unwrap();

        let resolved = integrations.resolve(&resolver).expect("resolves");
        assert_eq!(
            resolved[0].payload,
            serde_json::json!("${installPath}"),
            "doubled $ must escape to a literal token, not substitute"
        );
    }
}
