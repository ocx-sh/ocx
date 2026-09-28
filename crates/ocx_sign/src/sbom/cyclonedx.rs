// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! CycloneDX 1.5-1.7 parsing and summarization.

use serde::Deserialize;
use serde_json::Value;

use super::{SbomError, SbomSummary};

/// CycloneDX minor versions this reader understands.
pub(super) const ACCEPTED_SPEC_VERSIONS: [&str; 3] = ["1.5", "1.6", "1.7"];

/// The fields this reader extracts; no `deny_unknown_fields`, as producers may write a newer minor.
#[derive(Deserialize)]
struct CycloneDxDocument {
    #[serde(rename = "serialNumber")]
    serial_number: Option<String>,
    // `Option`, not `#[serde(default)]`: the default covers only an absent field, so `"components": null` would fail.
    components: Option<Vec<ComponentEntry>>,
    metadata: Option<DocumentMetadata>,
}

#[derive(Deserialize)]
struct DocumentMetadata {
    component: Option<ComponentEntry>,
}

#[derive(Deserialize)]
struct ComponentEntry {
    name: Option<String>,
}

/// Summarizes a CycloneDX 1.5-1.7 document; any other document is a refusal, never an empty summary.
///
/// # Errors
///
/// Every [`SbomError`] variant, one per failed stage.
pub fn summarize_cyclonedx(document: &[u8]) -> Result<SbomSummary, SbomError> {
    // Never enable serde_json's `unbounded_depth`: its 128 limit is what stops a nested document overflowing the stack.
    let value: Value = serde_json::from_slice(document).map_err(SbomError::NotJson)?;

    // Probe `specVersion` before the typed parse, or an old version surfaces as an opaque field error.
    let Some(root) = value.as_object() else {
        return Err(SbomError::NotAnObject);
    };
    let spec_version = match root.get("specVersion") {
        Some(Value::String(version)) => version.clone(),
        _ => return Err(SbomError::MissingSpecVersion),
    };
    if !ACCEPTED_SPEC_VERSIONS.contains(&spec_version.as_str()) {
        return Err(SbomError::UnsupportedSpecVersion { found: spec_version });
    }

    let document: CycloneDxDocument = serde_json::from_value(value).map_err(|source| SbomError::MalformedDocument {
        spec_version: spec_version.clone(),
        source,
    })?;

    Ok(SbomSummary {
        spec_version,
        serial_number: document.serial_number,
        component_count: document.components.map_or(0, |components| components.len()),
        top_level_component: document
            .metadata
            .and_then(|metadata| metadata.component)
            .and_then(|component| component.name),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// Serializes a `json!` fixture to bytes the way a real caller would
    /// hand them to [`summarize_cyclonedx`].
    fn bytes(value: Value) -> Vec<u8> {
        serde_json::to_vec(&value).expect("test fixture serializes to JSON")
    }

    /// Builds a `Value` nested `depth` levels deep under one arbitrary key,
    /// for probing the recursion-limit boundary (DATA-FMT-02 / no
    /// `unbounded_depth`).
    fn deeply_nested_value(depth: usize) -> Value {
        let mut value = json!({"leaf": true});
        for _ in 0..depth {
            value = json!({"child": value});
        }
        value
    }

    // -- Happy path, one per accepted CycloneDX minor version --------------

    #[test]
    fn accepts_spec_version_1_5() {
        let document = bytes(json!({"specVersion": "1.5"}));
        let summary = summarize_cyclonedx(&document).expect("1.5 is in the accepted range");
        assert_eq!(summary.spec_version, "1.5");
    }

    #[test]
    fn accepts_spec_version_1_6() {
        let document = bytes(json!({"specVersion": "1.6"}));
        let summary = summarize_cyclonedx(&document).expect("1.6 is in the accepted range");
        assert_eq!(summary.spec_version, "1.6");
    }

    #[test]
    fn accepts_spec_version_1_7() {
        let document = bytes(json!({"specVersion": "1.7"}));
        let summary = summarize_cyclonedx(&document).expect("1.7 is in the accepted range");
        assert_eq!(summary.spec_version, "1.7");
    }

    // -- Version refusal: below range, above range, absent, wrong type -----

    #[test]
    fn refuses_spec_version_below_accepted_range() {
        let document = bytes(json!({"specVersion": "1.4"}));
        let error = summarize_cyclonedx(&document).expect_err("1.4 predates the accepted range");
        assert!(
            matches!(&error, SbomError::UnsupportedSpecVersion { found } if found == "1.4"),
            "expected UnsupportedSpecVersion{{found: \"1.4\"}}, got {error:?}"
        );
    }

    /// Discriminates probe-before-parse from parse-first: a document with
    /// an out-of-range version AND a malformed `components` field must
    /// still be reported as the version refusal. A parse-first
    /// implementation would try the typed CycloneDxDocument parse before
    /// checking the version, and "not-an-array" would surface as a
    /// MalformedDocument serde error instead.
    #[test]
    fn version_refusal_wins_over_a_malformed_body() {
        let document = bytes(json!({
            "specVersion": "1.4",
            "components": "not-an-array"
        }));
        let error = summarize_cyclonedx(&document).expect_err("1.4 is refused before the body is parsed");
        assert!(
            matches!(&error, SbomError::UnsupportedSpecVersion { found } if found == "1.4"),
            "expected UnsupportedSpecVersion{{found: \"1.4\"}}, got {error:?}"
        );
    }

    #[test]
    fn refuses_spec_version_above_accepted_range() {
        let document = bytes(json!({"specVersion": "2.0"}));
        let error = summarize_cyclonedx(&document).expect_err("2.0 is beyond the accepted range");
        assert!(
            matches!(&error, SbomError::UnsupportedSpecVersion { found } if found == "2.0"),
            "expected UnsupportedSpecVersion{{found: \"2.0\"}}, got {error:?}"
        );
    }

    #[test]
    fn refuses_absent_spec_version() {
        let document = bytes(json!({"bomFormat": "CycloneDX"}));
        let error = summarize_cyclonedx(&document).expect_err("no specVersion field at all");
        assert!(
            matches!(error, SbomError::MissingSpecVersion),
            "expected MissingSpecVersion, got {error:?}"
        );
    }

    #[test]
    fn refuses_non_string_spec_version() {
        let document = bytes(json!({"specVersion": 1.6}));
        let error = summarize_cyclonedx(&document).expect_err("specVersion must be a JSON string");
        assert!(
            matches!(error, SbomError::MissingSpecVersion),
            "expected MissingSpecVersion, got {error:?}"
        );
    }

    // -- Non-object root -----------------------------------------------------

    #[test]
    fn refuses_array_root() {
        let document = bytes(json!([1, 2, 3]));
        let error = summarize_cyclonedx(&document).expect_err("root is an array, not an object");
        assert!(
            matches!(error, SbomError::NotAnObject),
            "expected NotAnObject, got {error:?}"
        );
    }

    #[test]
    fn refuses_scalar_root() {
        let document = bytes(json!("just a string"));
        let error = summarize_cyclonedx(&document).expect_err("root is a scalar, not an object");
        assert!(
            matches!(error, SbomError::NotAnObject),
            "expected NotAnObject, got {error:?}"
        );
    }

    // -- Field extraction per the SbomSummary contract ------------------------

    #[test]
    fn extracts_component_count_name_and_serial_number() {
        let document = bytes(json!({
            "bomFormat": "CycloneDX",
            "specVersion": "1.6",
            "serialNumber": "urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79",
            "version": 1,
            "metadata": {
                "component": {
                    "type": "application",
                    "name": "acme-cli",
                    "version": "1.0.0"
                }
            },
            "components": [
                {"type": "library", "name": "libfoo", "version": "1.2.3"},
                {"type": "library", "name": "libbar", "version": "4.5.6"}
            ]
        }));
        let summary = summarize_cyclonedx(&document).expect("well-formed 1.6 document");
        assert_eq!(summary.spec_version, "1.6");
        assert_eq!(
            summary.serial_number.as_deref(),
            Some("urn:uuid:3e671687-395b-41f5-a30f-a58921a69b79")
        );
        assert_eq!(summary.component_count, 2);
        assert_eq!(summary.top_level_component.as_deref(), Some("acme-cli"));
    }

    #[test]
    fn reports_defaults_when_optional_fields_absent() {
        // Only specVersion — the one field the summary cannot default.
        let document = bytes(json!({"specVersion": "1.5"}));
        let summary = summarize_cyclonedx(&document).expect("minimal but valid 1.5 document");
        assert_eq!(summary.spec_version, "1.5");
        assert_eq!(summary.serial_number, None);
        assert_eq!(summary.component_count, 0);
        assert_eq!(summary.top_level_component, None);
    }

    // -- Recursion-depth boundary (no unbounded_depth; default limit only) ---

    #[test]
    fn parses_deeply_nested_but_under_limit_document() {
        // 100 < serde_json's default 128-level recursion limit. Nested under
        // a field this reader does not type, proving the depth check is
        // serde_json's own default on the initial parse, not a premature
        // restriction of our own.
        let document = bytes(json!({
            "specVersion": "1.6",
            "extra": deeply_nested_value(100),
        }));
        let result = summarize_cyclonedx(&document);
        assert!(
            result.is_ok(),
            "expected an under-limit nested document to parse: {result:?}"
        );
    }

    #[test]
    fn rejects_document_exceeding_default_recursion_limit_without_panicking() {
        // 200 > serde_json's default 128-level limit. Must come back as a
        // typed error, never a stack overflow — proves the "no panics on any
        // input" requirement at the deepest-nesting edge.
        let document = bytes(json!({
            "specVersion": "1.6",
            "extra": deeply_nested_value(200),
        }));
        let error = summarize_cyclonedx(&document).expect_err("exceeds the default recursion limit");
        assert!(
            matches!(error, SbomError::NotJson(_)),
            "expected NotJson, got {error:?}"
        );
    }

    // -- Garbage bytes ---------------------------------------------------------

    #[test]
    fn rejects_garbage_bytes() {
        let document = b"not json at all {{{";
        let error = summarize_cyclonedx(document).expect_err("not valid JSON syntax");
        assert!(
            matches!(error, SbomError::NotJson(_)),
            "expected NotJson, got {error:?}"
        );
    }

    // -- Malformed document despite an accepted specVersion ---------------------

    #[test]
    fn rejects_malformed_document_with_accepted_spec_version() {
        // components must be an array; here it is a string.
        let document = bytes(json!({
            "specVersion": "1.6",
            "components": "not-an-array"
        }));
        let error = summarize_cyclonedx(&document).expect_err("components has the wrong shape");
        assert!(
            matches!(&error, SbomError::MalformedDocument { spec_version, .. } if spec_version == "1.6"),
            "expected MalformedDocument{{spec_version: \"1.6\", ..}}, got {error:?}"
        );
    }

    // -- Explicit null tolerated the same as absent ---------------------------

    #[test]
    fn null_components_is_tolerated_like_absent() {
        let document = bytes(json!({
            "specVersion": "1.6",
            "components": null
        }));
        let summary = summarize_cyclonedx(&document).expect("null components is tolerated like absent");
        assert_eq!(summary.component_count, 0);
    }
}
