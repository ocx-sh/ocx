// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The predicate-type vocabulary: cosign's `--type` alias table, the
//! `CosignPredicate` wrapper, and the SLSA builder-identity accessor.
//!
//! The alias table is cosign's, verbatim. It is a pure lookup with no policy in
//! it: bare `slsaprovenance` resolves to v0.2 here exactly as it does in cosign,
//! and the `>= v1.0` attach-side floor is enforced by the attest pipeline
//! instead. Diverging in the table would silently produce a different
//! `predicateType` than cosign for the same flag value — two tools, one word,
//! two meanings.

use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::value::RawValue;

use crate::oci::referrer::media_types::{SBOM_CYCLONEDX, SBOM_SPDX_JSON, SBOM_SPDX_TEXT};

const URI_CYCLONEDX: &str = "https://cyclonedx.org/bom";
const URI_SPDX: &str = "https://spdx.dev/Document";
const URI_SLSA_PROVENANCE_V02: &str = "https://slsa.dev/provenance/v0.2";
const URI_SLSA_PROVENANCE_V1: &str = "https://slsa.dev/provenance/v1";
const URI_LINK: &str = "https://in-toto.io/Link/v1";
const URI_VULN: &str = "https://cosign.sigstore.dev/attestation/vuln/v1";
const URI_OPENVEX: &str = "https://openvex.dev/ns";
const URI_CUSTOM: &str = "https://cosign.sigstore.dev/attestation/v1";

/// cosign's `--type` vocabulary, plus a full URI passed through unchanged.
///
/// Deliberately not `#[non_exhaustive]`: the variant set is a closed
/// vocabulary, and `ocx_cli` matches on it from another crate — an added alias
/// should break those matches rather than fall into a wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PredicateType {
    CycloneDx,
    Spdx,
    SpdxJson,
    /// Resolves to provenance **v0.2**, matching cosign's bare alias.
    SlsaProvenance,
    SlsaProvenance02,
    SlsaProvenance1,
    Link,
    Vuln,
    OpenVex,
    Custom,
    /// A full URI, stored exactly as the caller spelled it.
    Uri(String),
}

impl PredicateType {
    /// Every alias [`FromStr`] accepts, in table order.
    pub const ALIASES: &'static [&'static str] = &[
        "cyclonedx",
        "spdx",
        "spdxjson",
        "slsaprovenance",
        "slsaprovenance02",
        "slsaprovenance1",
        "link",
        "vuln",
        "openvex",
        "custom",
    ];

    /// Returns the URI written into the Statement and the referrer annotation.
    pub fn uri(&self) -> &str {
        match self {
            Self::CycloneDx => URI_CYCLONEDX,
            Self::Spdx | Self::SpdxJson => URI_SPDX,
            Self::SlsaProvenance | Self::SlsaProvenance02 => URI_SLSA_PROVENANCE_V02,
            Self::SlsaProvenance1 => URI_SLSA_PROVENANCE_V1,
            Self::Link => URI_LINK,
            Self::Vuln => URI_VULN,
            Self::OpenVex => URI_OPENVEX,
            Self::Custom => URI_CUSTOM,
            Self::Uri(uri) => uri,
        }
    }

    /// Wraps `predicate` the way cosign wraps a custom predicate, or returns it
    /// unchanged for every other type.
    ///
    /// The wrapper is built around the verbatim slice, so the wrapped form
    /// embeds the caller's original bytes too — whatever whitespace, key order
    /// and number spelling the predicate file held is what gets signed.
    ///
    /// The decision is made on the resolved URI rather than the variant, so a
    /// caller spelling `--type https://cosign.sigstore.dev/attestation/v1` in
    /// full gets the same wrapper the `custom` alias gets, as it does in cosign.
    ///
    /// # Errors
    ///
    /// [`serde_json::Error`] if the wrapper cannot be serialized. Unreachable
    /// for the shape written here — a borrowed [`RawValue`] and a `String` — but
    /// returned rather than asserted, because the alternative is a panic in
    /// library code.
    pub fn wrap(&self, predicate: &RawValue, now: DateTime<Utc>) -> Result<Box<RawValue>, serde_json::Error> {
        if self.uri() != URI_CUSTOM {
            return Ok(predicate.to_owned());
        }
        serde_json::value::to_raw_value(&CosignPredicate {
            data: predicate,
            // Seconds precision and a literal `Z`, which is what Go's
            // `time.RFC3339` produces on the cosign side.
            timestamp: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        })
    }
}

impl FromStr for PredicateType {
    type Err = PredicateTypeParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "cyclonedx" => Self::CycloneDx,
            "spdx" => Self::Spdx,
            "spdxjson" => Self::SpdxJson,
            "slsaprovenance" => Self::SlsaProvenance,
            "slsaprovenance02" => Self::SlsaProvenance02,
            "slsaprovenance1" => Self::SlsaProvenance1,
            "link" => Self::Link,
            "vuln" => Self::Vuln,
            "openvex" => Self::OpenVex,
            "custom" => Self::Custom,
            // Parsed only to establish that this is an absolute URI; the
            // caller's own spelling is what gets stored, because `Url` would
            // normalize it (a bare host gains a trailing slash) and the
            // predicateType is published, annotated and hashed.
            _ if url::Url::parse(value).is_ok() => Self::Uri(value.to_owned()),
            _ => {
                return Err(PredicateTypeParseError {
                    found: value.to_owned(),
                });
            }
        })
    }
}

/// cosign's wrapper for a custom predicate.
///
/// The field names are capitalized because Go marshals exported struct fields
/// under their own names and this shape is on the wire.
#[derive(Debug, Serialize)]
struct CosignPredicate<'a> {
    #[serde(rename = "Data")]
    data: &'a RawValue,
    #[serde(rename = "Timestamp")]
    timestamp: String,
}

/// Returns the SLSA builder identity, dispatched on the resolved provenance
/// version.
///
/// v0.2 puts it at `builder.id`; v1 moved it to `runDetails.builder.id`. The two
/// schemas share no path, so a single accessor would be wrong for one of the two
/// shapes verify accepts. `None` means the field is absent or unreadable, which
/// a `builder`-carrying policy must treat as a refusal rather than a skip —
/// otherwise the pin is silently inert on exactly the documents it exists for.
/// Whether `predicate_type` names a SLSA provenance predicate.
///
/// Dispatches on the resolved URI, not the variant, so a caller spelling
/// `Uri("https://slsa.dev/provenance/v0.2")` in full is provenance exactly as
/// the `slsaprovenance02` alias is — the same rule [`builder_id`] and
/// [`PredicateType::wrap`] follow.
///
/// Separate from `builder_id(..).is_some()`, which cannot tell "not provenance"
/// from "provenance carrying no readable builder". The trust policy's `builder`
/// pin needs both answers and treats them oppositely: the first is out of the
/// pin's scope, the second is a refusal.
pub fn is_provenance(predicate_type: &PredicateType) -> bool {
    matches!(predicate_type.uri(), URI_SLSA_PROVENANCE_V02 | URI_SLSA_PROVENANCE_V1)
}

/// Whether `predicate_type` resolves to a SLSA provenance URI **below** v1.0.
///
/// The attach-side floor ([#102](https://github.com/ocx-sh/ocx/issues/102),
/// checklist row 21) refuses exactly this set. It lives here rather than in the
/// pipeline so the URI literals stay in one file: `slsaprovenance`,
/// `slsaprovenance02` and a full `Uri("https://slsa.dev/provenance/v0.2")` all
/// resolve to the same URI, and a floor matching on variants would let the
/// third through.
///
/// Narrower than [`is_provenance`], which the verify-side `builder` pin uses:
/// verify still accepts v0.2 from external producers, because cosign writes it.
/// The floor is attach-only.
pub(crate) fn is_provenance_below_v1(predicate_type: &PredicateType) -> bool {
    predicate_type.uri() == URI_SLSA_PROVENANCE_V02
}

/// The `artifactType` an **unsigned** SBOM referrer carries for this predicate
/// type, or `None` when the type is not an SBOM at all.
///
/// An unsigned attach has no DSSE envelope to carry a `predicateType`, so the
/// referrer is typed by the document's own media type instead — what `cosign
/// attach sbom`, `oras attach` and `syft` all write. `None` is the entire floor
/// on that path: a predicate with no SBOM media type has nowhere to record what
/// it is, and is refused rather than published as an untyped blob.
///
/// The one dispatch in this file that reads the **variant** rather than the
/// resolved URI, and it has to: `spdx` and `spdxjson` share one predicateType
/// URI and do not share a serialization, so the URI cannot tell tag-value text
/// from JSON. A full-URI spelling of the SPDX predicate resolves to the JSON
/// form — the one every producer in the wild writes.
pub(crate) fn sbom_artifact_type(predicate_type: &PredicateType) -> Option<&'static str> {
    match predicate_type {
        PredicateType::Spdx => Some(SBOM_SPDX_TEXT),
        PredicateType::SpdxJson => Some(SBOM_SPDX_JSON),
        other => match other.uri() {
            URI_CYCLONEDX => Some(SBOM_CYCLONEDX),
            URI_SPDX => Some(SBOM_SPDX_JSON),
            _ => None,
        },
    }
}

/// The `predicateType` URI an unsigned referrer's `artifactType` stands for, or
/// `None` when it is not an SBOM type.
///
/// The inverse of [`sbom_artifact_type`] over the URIs it can express — not over
/// its inputs, because the two SPDX serializations collapse onto one URI. This
/// is what labels an unverified listing entry and what `--type` narrows against,
/// so an unsigned entry is narrowed by exactly the value a signed one carries.
pub(crate) fn sbom_predicate_type_uri(artifact_type: &str) -> Option<&'static str> {
    match artifact_type {
        SBOM_CYCLONEDX => Some(URI_CYCLONEDX),
        SBOM_SPDX_JSON | SBOM_SPDX_TEXT => Some(URI_SPDX),
        _ => None,
    }
}

pub fn builder_id<'a>(predicate_type: &PredicateType, predicate: &'a serde_json::Value) -> Option<&'a str> {
    match predicate_type.uri() {
        URI_SLSA_PROVENANCE_V1 => predicate.get("runDetails")?.get("builder")?.get("id")?.as_str(),
        URI_SLSA_PROVENANCE_V02 => predicate.get("builder")?.get("id")?.as_str(),
        _ => None,
    }
}

/// The spelling `--type` carried, when it was neither a cosign alias nor a URI.
///
/// Carries `found` structurally rather than pre-formatting a message, so a
/// caller can fold it into its own typed error. Today there is one: clap's
/// value parser, which renders this as a usage failure (exit 64).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown predicate type `{found}`: expected an absolute URI or one of {known}", known = PredicateType::ALIASES.join(", "))]
pub struct PredicateTypeParseError {
    /// The unrecognized spelling, verbatim.
    pub found: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A predicate spelled so that a `Value` round-trip is observable on three
    /// axes: a trailing-zero float, an escaped non-ASCII codepoint, and
    /// irregular whitespace. Key order is deliberately NOT one of them —
    /// `serde_json`'s `preserve_order` feature is on workspace-wide, so key
    /// order survives a round-trip and discriminates nothing.
    const AWKWARD_PREDICATE: &str = concat!(
        "{\n",
        "  \"specVersion\" : \"1.5\",\n",
        "    \"zebra\": 1.50,\n",
        "  \"unicode\": \"caf\\u00e9\",\n",
        "  \"nested\": [ 1,2 ,3 ]\n",
        "}"
    );

    /// cosign's `--type` table, verbatim. Diverging on any row silently
    /// publishes a different `predicateType` than cosign for the same flag.
    const ALIAS_TABLE: &[(&str, &str)] = &[
        ("cyclonedx", "https://cyclonedx.org/bom"),
        ("spdx", "https://spdx.dev/Document"),
        ("spdxjson", "https://spdx.dev/Document"),
        ("slsaprovenance", "https://slsa.dev/provenance/v0.2"),
        ("slsaprovenance02", "https://slsa.dev/provenance/v0.2"),
        ("slsaprovenance1", "https://slsa.dev/provenance/v1"),
        ("link", "https://in-toto.io/Link/v1"),
        ("vuln", "https://cosign.sigstore.dev/attestation/vuln/v1"),
        ("openvex", "https://openvex.dev/ns"),
        ("custom", "https://cosign.sigstore.dev/attestation/v1"),
    ];

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_owned()).expect("test input is valid JSON")
    }

    fn fixed_time() -> DateTime<Utc> {
        "2026-08-20T09:41:07Z".parse().expect("literal is RFC 3339")
    }

    #[test]
    fn alias_table_matches_cosign() {
        for (alias, expected_uri) in ALIAS_TABLE {
            let parsed: PredicateType = alias.parse().unwrap_or_else(|_| panic!("`{alias}` must parse"));
            assert_eq!(parsed.uri(), *expected_uri, "alias `{alias}` resolved to the wrong URI");
        }
    }

    #[test]
    fn alias_list_covers_exactly_the_table() {
        let mut listed: Vec<&str> = PredicateType::ALIASES.to_vec();
        let mut tabled: Vec<&str> = ALIAS_TABLE.iter().map(|(alias, _)| *alias).collect();
        listed.sort_unstable();
        tabled.sort_unstable();
        assert_eq!(listed, tabled, "ALIASES drifted from the parse table");
    }

    #[test]
    fn unknown_value_is_rejected_with_a_typed_error() {
        // The last four are plausible-but-unintended spellings. They pin the
        // direction `alias_list_covers_exactly_the_table` cannot see: a stray
        // extra arm in `from_str` would accept a word cosign does not, and
        // diverge from its vocabulary with `ALIASES` still matching the table.
        for value in [
            "cyclonedxx",
            "CycloneDX",
            "",
            "slsa provenance",
            "sbom",
            "spdx-json",
            "slsa",
            "cyclone",
        ] {
            let error = PredicateType::from_str(value).expect_err("a non-alias, non-URI value must not parse");
            assert_eq!(error.found, value);
            assert!(
                error.to_string().contains("cyclonedx"),
                "the message must list the accepted aliases: {error}"
            );
        }
    }

    #[test]
    fn absolute_uri_passes_through_unchanged() {
        // No trailing slash is appended, no case is folded, no default port is
        // elided: the URI is validated, then the caller's own bytes are kept.
        for value in [
            "https://example.test/my/Predicate",
            "https://openvex.dev",
            "https://slsa.dev/provenance/v1.1",
        ] {
            let parsed = PredicateType::from_str(value).unwrap_or_else(|_| panic!("`{value}` is a URI"));
            assert_eq!(parsed, PredicateType::Uri(value.to_owned()));
            assert_eq!(parsed.uri(), value);
        }
    }

    #[test]
    fn only_the_custom_predicate_type_is_wrapped() {
        let predicate = raw(r#"{"a":1}"#);
        for (alias, _) in ALIAS_TABLE {
            let parsed: PredicateType = alias.parse().unwrap_or_else(|_| panic!("`{alias}` must parse"));
            let wrapped = parsed.wrap(&predicate, fixed_time()).expect("wrap must not fail");
            if *alias == "custom" {
                assert_ne!(wrapped.get(), predicate.get(), "`custom` must wrap");
            } else {
                assert_eq!(wrapped.get(), predicate.get(), "`{alias}` must not wrap");
            }
        }
    }

    #[test]
    fn a_full_custom_uri_wraps_like_the_alias() {
        // cosign compares the *resolved* type against the custom URI, so
        // `--type https://cosign.sigstore.dev/attestation/v1` wraps too.
        let predicate = raw(r#"{"a":1}"#);
        let parsed = PredicateType::from_str("https://cosign.sigstore.dev/attestation/v1").expect("a URI parses");
        let wrapped = parsed.wrap(&predicate, fixed_time()).expect("wrap must not fail");
        assert!(wrapped.get().starts_with(r#"{"Data":"#), "got {}", wrapped.get());
    }

    #[test]
    fn wrapping_embeds_the_predicate_bytes_verbatim() {
        let predicate = raw(AWKWARD_PREDICATE);
        let wrapped = PredicateType::Custom
            .wrap(&predicate, fixed_time())
            .expect("wrap must not fail");
        assert_eq!(
            wrapped.get(),
            format!("{{\"Data\":{AWKWARD_PREDICATE},\"Timestamp\":\"2026-08-20T09:41:07Z\"}}")
        );
    }

    #[test]
    fn unwrapped_types_return_the_predicate_bytes_verbatim() {
        let predicate = raw(AWKWARD_PREDICATE);
        let wrapped = PredicateType::CycloneDx
            .wrap(&predicate, fixed_time())
            .expect("wrap must not fail");
        assert_eq!(wrapped.get(), AWKWARD_PREDICATE);
    }

    #[test]
    fn an_empty_object_is_a_valid_predicate() {
        let predicate = raw("{}");
        assert_eq!(
            PredicateType::CycloneDx
                .wrap(&predicate, fixed_time())
                .expect("wrap must not fail")
                .get(),
            "{}"
        );
        assert_eq!(
            PredicateType::Custom
                .wrap(&predicate, fixed_time())
                .expect("wrap must not fail")
                .get(),
            r#"{"Data":{},"Timestamp":"2026-08-20T09:41:07Z"}"#
        );
    }

    /// Pins the dependency contract `wrap` relies on: `RawValue` is the
    /// validate-then-splice primitive, so a caller that built one has already
    /// proved the bytes are one well-formed JSON document, and `wrap` never
    /// re-parses them.
    #[test]
    fn raw_value_refuses_malformed_json() {
        for text in ["", "{", "{\"a\":}", "not json", "{} {}"] {
            assert!(
                RawValue::from_string(text.to_owned()).is_err(),
                "`{text}` must not become a predicate"
            );
        }
    }

    #[test]
    fn wrap_timestamp_is_rfc3339_with_z() {
        let wrapped = PredicateType::Custom
            .wrap(&raw("{}"), fixed_time())
            .expect("wrap must not fail");
        assert!(
            wrapped.get().ends_with(r#""Timestamp":"2026-08-20T09:41:07Z"}"#),
            "got {}",
            wrapped.get()
        );
    }

    #[test]
    fn builder_id_dispatches_on_the_provenance_version() {
        // The two schemas share no path, so each fixture must resolve under its
        // own version and stay `None` under the other. A single fixture cannot
        // tell a real dispatch from a try-both-paths lookup.
        let v1: serde_json::Value =
            serde_json::from_str(r#"{"runDetails":{"builder":{"id":"https://ci.test/v1"}}}"#).expect("fixture parses");
        let v02: serde_json::Value =
            serde_json::from_str(r#"{"builder":{"id":"https://ci.test/v02"}}"#).expect("fixture parses");

        assert_eq!(
            builder_id(&PredicateType::SlsaProvenance1, &v1),
            Some("https://ci.test/v1")
        );
        assert_eq!(builder_id(&PredicateType::SlsaProvenance1, &v02), None);
        assert_eq!(
            builder_id(&PredicateType::SlsaProvenance, &v02),
            Some("https://ci.test/v02")
        );
        assert_eq!(
            builder_id(&PredicateType::SlsaProvenance02, &v02),
            Some("https://ci.test/v02")
        );
        assert_eq!(builder_id(&PredicateType::SlsaProvenance, &v1), None);

        // Dispatch is on the resolved URI, so a full-URI `--type` behaves the
        // same as the alias it spells.
        let as_uri = PredicateType::Uri("https://slsa.dev/provenance/v1".to_owned());
        assert_eq!(builder_id(&as_uri, &v1), Some("https://ci.test/v1"));
    }

    #[test]
    fn builder_id_is_none_when_absent_unreadable_or_not_provenance() {
        let v1: serde_json::Value =
            serde_json::from_str(r#"{"runDetails":{"builder":{"id":"https://ci.test/v1"}}}"#).expect("fixture parses");
        // Not a provenance predicate at all.
        assert_eq!(builder_id(&PredicateType::CycloneDx, &v1), None);
        // Provenance shape, builder missing or the wrong JSON type.
        for text in [
            r#"{"runDetails":{}}"#,
            r#"{"runDetails":{"builder":{}}}"#,
            r#"{"runDetails":{"builder":{"id":42}}}"#,
            r#"{}"#,
            r#"[]"#,
        ] {
            let value: serde_json::Value = serde_json::from_str(text).expect("fixture parses");
            assert_eq!(builder_id(&PredicateType::SlsaProvenance1, &value), None, "for {text}");
        }
    }
}
