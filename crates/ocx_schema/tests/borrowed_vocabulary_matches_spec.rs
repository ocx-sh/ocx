// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Vocabulary ocx borrows from someone else's specification must match that
//! specification, field for field.
//!
//! Two of ocx's published schemas re-publish a shape ocx did not design: the
//! OCI image-spec **platform object**, and the in-toto v1
//! **`ResourceDescriptor`**. Both are hand-authored on the Rust side — the OCI
//! platform because its Rust type is a foreign struct that cannot carry our
//! derive, the descriptor because its field names are a wire contract — and a
//! hand-authored schema drifts silently. Nothing in the type system notices
//! when a property outlives its removal from the spec, or when a new one is
//! invented beside the borrowed ones.
//!
//! So the specs are vendored (`tests/specs/`, provenance in `SOURCES.md`) and
//! asserted against directly. The vendored copies are checksummed here as well,
//! because a conformance test whose reference data can be edited to match the
//! thing under test proves nothing.
//!
//! Both shapes are checked as a **subset** of their specification: ocx need not
//! publish every field a spec defines, but every field it does publish must be
//! one the spec defines. `ResourceDescriptor` satisfies that outright — ocx
//! fills what it has and omits `content`, `downloadLocation` and `mediaType`,
//! which name nothing it has. Platform does not: it publishes `features`, which
//! image-spec `v1.1.1` removed. That is pinned in [`KNOWN_PLATFORM_DEVIATIONS`]
//! rather than fixed, because deleting a property from a published schema is a
//! wire decision, and a conformance test is not where wire decisions get made.
//! The ratchet keeps the deviation visible and keeps a second one from joining
//! it quietly.

mod spec_vocabulary;

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use spec_vocabulary::{
    PLATFORM_DESCRIPTION_PREFIX, assert_vendored_specs_are_authentic, has_platform_marker, in_toto_fields,
    oci_platform_object,
};

/// Every kind [`ocx_schema::schema_for`] generates. Borrowed vocabulary is not
/// confined to the JSON kinds — a platform object can reach a TOML-backed
/// schema too — so this list is deliberately all of them.
const KINDS: &[&str] = &[
    "metadata",
    "config",
    "project",
    "project-lock",
    "patch",
    "reports",
    "execution-record",
];

/// protobuf `TYPE_STRING`, from `descriptor.proto`'s field-type enum.
const PROTO_TYPE_STRING: i64 = 9;

/// Properties ocx's platform schema publishes that OCI image-spec `v1.1.1`
/// does not define — NOT sanctioned, pinned so the set cannot grow silently.
/// Same semantics as `KNOWN_NON_SNAKE` in `json_keys_are_snake_case.rs`: every
/// entry must match a real deviation on every run, so an entry that stops
/// deviating is stale and fails the test exactly like an unpinned new one.
///
/// `(kind, json_pointer_path, property)`.
///
/// The single entry, `features`: image-spec removed it, and the `oci-client`
/// fork's `Platform` struct still carries it as RESERVED
/// (`external/rust-oci-client/src/manifest.rs:483`). ocx sets it to `None` at
/// every construction site, never reads it, and it is `skip_serializing_if`, so
/// it reaches no wire ocx writes — but it is published in the schema. Removing
/// it is a schema/wire break and therefore the owner's call, not this test's.
const KNOWN_PLATFORM_DEVIATIONS: &[(&str, &str, &str)] = &[("reports", "/$defs/Platform", "features")];

fn schema(kind: &str) -> Value {
    let raw = ocx_schema::schema_for(kind).unwrap_or_else(|| panic!("schema_for({kind:?}) returned None"));
    serde_json::from_str(&raw).unwrap_or_else(|error| panic!("schema_for({kind:?}) produced invalid JSON: {error}"))
}

/// The `type` and, for an array, the `items.type` of a property schema —
/// everything the OCI platform object says about any of its properties.
fn shape(property: &Value) -> (Option<&str>, Option<&str>) {
    (
        property.get("type").and_then(Value::as_str),
        property.pointer("/items/type").and_then(Value::as_str),
    )
}

/// Locate every definition in `document` that is the OCI platform object,
/// returning `(json_pointer_path, definition)`.
fn platform_definitions(document: &Value) -> Vec<(String, &Value)> {
    let mut found = Vec::new();
    for container in ["$defs", "definitions"] {
        let Some(definitions) = document.get(container).and_then(Value::as_object) else {
            continue;
        };
        for (name, definition) in definitions {
            if name == "Platform" || has_platform_marker(definition) {
                found.push((format!("/{container}/{name}"), definition));
            }
        }
    }
    // An inline platform object (no definition of its own) still carries the
    // description marker, so it is caught wherever schemars placed it.
    if has_platform_marker(document) {
        found.push((String::new(), document));
    }
    found
}

/// The vendored specs are the reference data for every other assertion in this
/// file. If they can be edited, those assertions describe nothing.
#[test]
fn vendored_specs_match_the_checksums_recorded_in_sources() {
    assert_vendored_specs_are_authentic();
}

/// Every platform object ocx publishes stays inside the OCI platform
/// vocabulary: its properties are a **subset** of the spec's, the ones it
/// shares carry the spec's types, and `required` matches exactly.
///
/// Subset rather than equality, on two different grounds. Omitting a spec
/// property is a real choice — ocx need not publish everything OCI defines —
/// so a missing one is not drift. Publishing one OCI does **not** define is
/// drift, and the one instance of it today is pinned in
/// [`KNOWN_PLATFORM_DEVIATIONS`] rather than fixed here, because deleting a
/// property from a published schema is a wire decision the owner makes, not a
/// thing a conformance test does on its way past.
///
/// `required` stays equality: it is what a consumer relies on to know a field
/// will be there, so narrowing or widening it silently is a break in either
/// direction.
#[test]
fn every_platform_definition_matches_the_oci_image_spec() {
    assert_vendored_specs_are_authentic();

    let specification = oci_platform_object();
    let expected_properties: BTreeMap<String, Value> = specification
        .pointer("/properties")
        .and_then(Value::as_object)
        .expect("the vendored platform object declares `properties`")
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let expected_required: BTreeSet<String> = specification
        .pointer("/required")
        .and_then(Value::as_array)
        .expect("the vendored platform object declares `required`")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();

    let mut actual_ratchet: BTreeSet<(&str, String, String)> = BTreeSet::new();
    let mut checked = 0_usize;

    for &kind in KINDS {
        let document = schema(kind);
        for (path, definition) in platform_definitions(&document) {
            let where_ = format!("{kind}{path}");
            let properties = definition
                .get("properties")
                .and_then(Value::as_object)
                .unwrap_or_else(|| panic!("{where_}: a platform object must declare `properties`"));

            for (name, actual) in properties {
                // Not in the spec at all: a deviation, collected for the
                // ratchet below rather than compared against nothing.
                let Some(expected) = expected_properties.get(name) else {
                    actual_ratchet.insert((kind, path.clone(), name.clone()));
                    continue;
                };
                assert_eq!(
                    shape(actual),
                    shape(expected),
                    "{where_}: property `{name}` has type {:?}, OCI image-spec says {:?}",
                    shape(actual),
                    shape(expected)
                );
            }

            let actual_required: BTreeSet<String> = definition
                .get("required")
                .and_then(Value::as_array)
                .map(|entries| entries.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default();
            assert_eq!(
                actual_required, expected_required,
                "{where_}: `required` diverged from OCI image-spec"
            );

            checked += 1;
        }
    }

    let expected_ratchet: BTreeSet<(&str, String, String)> = KNOWN_PLATFORM_DEVIATIONS
        .iter()
        .map(|(kind, path, property)| (*kind, path.to_string(), property.to_string()))
        .collect();

    let undeclared: Vec<_> = actual_ratchet.difference(&expected_ratchet).collect();
    let stale: Vec<_> = expected_ratchet.difference(&actual_ratchet).collect();
    assert!(
        undeclared.is_empty() && stale.is_empty(),
        "KNOWN_PLATFORM_DEVIATIONS drifted from reality — properties ocx publishes that OCI image-spec does not \
         define and that are not pinned: {undeclared:?}; pinned entries that no longer deviate (removed — delete \
         them from the list): {stale:?}. Do not add an entry to silence a new one: a property OCI does not define \
         is a wire decision, so raise it rather than ratchet it."
    );

    assert!(
        checked > 0,
        "no platform definition was found in any schema, so this test asserted nothing. Either the definition was \
         renamed away from `Platform` and lost its `{PLATFORM_DESCRIPTION_PREFIX}` description marker, or it stopped \
         being published — decide which, do not delete this assertion."
    );
}

/// The `packages[]` item is an in-toto `ResourceDescriptor` and stays inside
/// that vocabulary: every key it publishes is a real in-toto `json_name`, and
/// the three ocx fills carry the shapes in-toto gives them.
///
/// A subset, not equality — see the module comment. What this refuses is a key
/// invented beside the borrowed ones: anything ocx needs per package belongs in
/// `annotations` under `sh.ocx.*`, which is exactly what an in-toto
/// `annotations` object is for.
#[test]
fn package_descriptors_stay_inside_the_in_toto_vocabulary() {
    assert_vendored_specs_are_authentic();

    let document = schema("execution-record");

    let reference = document
        .pointer("/properties/packages/items/$ref")
        .and_then(Value::as_str)
        .expect("`packages` items must $ref a definition");
    let name = reference
        .strip_prefix("#/$defs/")
        .unwrap_or_else(|| panic!("`packages` items $ref must point into #/$defs/, got {reference}"));
    let descriptor = document
        .pointer(&format!("/$defs/{name}"))
        .unwrap_or_else(|| panic!("$ref target {name} missing from $defs"));
    let properties = descriptor
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("$defs/{name} must declare `properties`"));

    let published: BTreeSet<&str> = properties.keys().map(String::as_str).collect();
    assert!(!published.is_empty(), "$defs/{name} publishes no properties at all");

    let proto_types: BTreeMap<String, i64> = in_toto_fields()
        .into_iter()
        .map(|(_name, json_name, kind)| (json_name, kind))
        .collect();
    let vocabulary: BTreeSet<&str> = proto_types.keys().map(String::as_str).collect();
    let invented: Vec<&&str> = published.iter().filter(|key| !vocabulary.contains(**key)).collect();
    assert!(
        invented.is_empty(),
        "$defs/{name} publishes {invented:?}, which in-toto v1 does not define. A descriptor must stay liftable into \
         an attestation's resolved-dependency list; per-package ocx data belongs in `annotations` under `sh.ocx.*`. \
         in-toto's vocabulary is {vocabulary:?}"
    );

    // in-toto types the three fields ocx fills. `name` and `uri` are proto
    // strings; `digest` is a `map<string, string>`, so no value may be anything
    // but a string; `annotations` is a `google.protobuf.Struct` — a JSON object
    // with arbitrary values.
    for key in ["name", "uri"] {
        assert_eq!(
            proto_types.get(key),
            Some(&PROTO_TYPE_STRING),
            "in-toto no longer types `{key}` as a string; this test's expectation is stale, not the schema"
        );
        if let Some(property) = properties.get(key) {
            assert_eq!(
                property.get("type").and_then(Value::as_str),
                Some("string"),
                "$defs/{name}.{key} must be a string, as in-toto types it"
            );
        }
    }

    let digest = properties
        .get("digest")
        .unwrap_or_else(|| panic!("$defs/{name} must publish `digest` — it is the only unconditional identity"));
    assert_eq!(
        digest.get("type").and_then(Value::as_str),
        Some("object"),
        "$defs/{name}.digest must be an object — in-toto's DigestSet is a map"
    );
    assert_eq!(
        digest.pointer("/additionalProperties/type").and_then(Value::as_str),
        Some("string"),
        "$defs/{name}.digest values must all be strings — in-toto types DigestSet as map<string, string>, so a \
         structured value would not round-trip"
    );

    let annotations = properties
        .get("annotations")
        .unwrap_or_else(|| panic!("$defs/{name} must publish `annotations`"));
    assert_eq!(
        annotations.get("type").and_then(Value::as_str),
        Some("object"),
        "$defs/{name}.annotations must be an object — in-toto types it as a protobuf Struct"
    );
}
