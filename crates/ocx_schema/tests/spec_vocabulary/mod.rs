// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Reads the vendored upstream specifications under `tests/specs/`.
//!
//! Shared by `borrowed_vocabulary_matches_spec.rs` (which asserts our published
//! schemas against these) and `json_keys_are_snake_case.rs` (which derives its
//! snake_case exemptions from them instead of hand-typing them). Both need the
//! same parse, and a second copy of it would be a second thing to keep in step
//! with the vendored files.
//!
//! Every file is pulled in with `include_str!`, so a missing or moved spec is a
//! compile error rather than a test that quietly reads nothing and passes.
//!
//! The public surface is deliberately narrow and every item is used by **both**
//! consumers: this module is compiled separately into each test binary, so an
//! item only one of them calls is dead code in the other and fails the build
//! under `-D warnings`. Anything one consumer needs alone belongs in that
//! consumer's own file, built on top of what is exported here.

use std::collections::BTreeMap;

use ocx_lib::oci::Algorithm;
use serde_json::Value;

/// OCI image-spec `v1.1.1`, `schema/image-index-schema.json`. Only the platform
/// object under `manifests.items` is consumed; it carries no `$ref`, so no
/// sibling image-spec files are vendored.
const OCI_IMAGE_INDEX_SCHEMA: &str = include_str!("../specs/oci/image-index-schema.json");

/// The in-toto v1 `ResourceDescriptor` field list, generated from the
/// `in-toto-attestation` reference package's protobuf descriptor.
const IN_TOTO_FIELDS: &str = include_str!("../specs/in-toto/resource_descriptor.fields.json");

/// Vendored for humans; no test reads its contents, but it is checksummed with
/// the rest so it cannot drift away from the field list generated beside it.
const IN_TOTO_PROTO: &str = include_str!("../specs/in-toto/resource_descriptor.proto");

/// Provenance and checksums for all three files above.
const SOURCES: &str = include_str!("../specs/SOURCES.md");

/// A definition carrying this description prefix is the OCI platform object,
/// whatever schemars happened to name it. Written by the hand-authored
/// `impl JsonSchema for Platform` in `ocx_lib::oci::platform`.
pub const PLATFORM_DESCRIPTION_PREFIX: &str = "An OCI image-spec platform object";

fn parse(label: &str, raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|error| panic!("vendored spec {label} is not valid JSON: {error}"))
}

/// The OCI image-spec platform object itself — the whole schema node, so each
/// consumer can take the part it needs (`properties`, `required`) without this
/// module growing an accessor per caller.
pub fn oci_platform_object() -> Value {
    let schema = parse("oci/image-index-schema.json", OCI_IMAGE_INDEX_SCHEMA);
    let platform = schema
        .pointer("/properties/manifests/items/properties/platform")
        .expect("the vendored image-index schema declares a platform object under manifests.items");
    assert!(
        platform
            .pointer("/properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| !properties.is_empty()),
        "the vendored platform object declares no properties — every comparison against it would be vacuous"
    );
    platform.clone()
}

/// `(name, json_name, type)` per in-toto `ResourceDescriptor` field, in
/// descriptor order. `json_name` is the wire spelling; `type` is the protobuf
/// field-type number.
pub fn in_toto_fields() -> Vec<(String, String, i64)> {
    let document = parse("in-toto/resource_descriptor.fields.json", IN_TOTO_FIELDS);
    let fields = document
        .get("fields")
        .and_then(Value::as_array)
        .expect("the generated in-toto field list has a `fields` array");
    assert!(
        !fields.is_empty(),
        "the generated in-toto field list is empty — every subset assertion downstream would pass vacuously"
    );
    fields
        .iter()
        .map(|field| {
            let text = |key: &str| {
                field
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("in-toto field entry is missing a string `{key}`: {field}"))
                    .to_owned()
            };
            let kind = field
                .get("type")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("in-toto field entry is missing an integer `type`: {field}"));
            (text("name"), text("json_name"), kind)
        })
        .collect()
}

/// Whether `value` is a schema object whose `description` opens with the OCI
/// platform marker.
pub fn has_platform_marker(value: &Value) -> bool {
    value
        .get("description")
        .and_then(Value::as_str)
        .is_some_and(|description| description.starts_with(PLATFORM_DESCRIPTION_PREFIX))
}

/// Fail unless every vendored spec still hashes to the value `SOURCES.md`
/// records for it.
///
/// Both consumers call this, and neither is meaningful without it: a
/// conformance test whose reference data can be hand-edited to match the thing
/// under test proves nothing, and an allowlist derived from an editable file is
/// not derived from a specification.
pub fn assert_vendored_specs_are_authentic() {
    let vendored: [(&str, &str); 3] = [
        ("oci/image-index-schema.json", OCI_IMAGE_INDEX_SCHEMA),
        ("in-toto/resource_descriptor.proto", IN_TOTO_PROTO),
        ("in-toto/resource_descriptor.fields.json", IN_TOTO_FIELDS),
    ];
    let recorded = recorded_checksums();

    for (path, contents) in vendored {
        let actual = Algorithm::Sha256.hash(contents.as_bytes());
        let expected = recorded
            .get(path)
            .unwrap_or_else(|| panic!("SOURCES.md records no checksum for the vendored file {path}"));
        assert_eq!(
            actual.hex(),
            expected,
            "vendored spec {path} does not match its recorded checksum — either it was edited by hand (never do \
             that: it is someone else's specification, and editing it turns the tests that read it into \
             tautologies) or it was refreshed without rewriting SOURCES.md. Use `task schema:specs:refresh`."
        );
    }

    let unprotected: Vec<&String> = recorded
        .keys()
        .filter(|path| !vendored.iter().any(|(known, _)| *known == path.as_str()))
        .collect();
    assert!(
        unprotected.is_empty(),
        "SOURCES.md records checksums for files no test reads, so they are unprotected: {unprotected:?}"
    );
}

/// The checksums recorded in `SOURCES.md`, keyed by path relative to
/// `tests/specs/`.
///
/// Parsed out of the `sha256:begin`/`sha256:end` marked block — the same block
/// `task schema:specs:refresh` rewrites — in `sha256sum` output format.
fn recorded_checksums() -> BTreeMap<String, String> {
    let block = SOURCES
        .split_once("sha256:begin")
        .and_then(|(_before, rest)| rest.split_once("sha256:end"))
        .map(|(block, _after)| block)
        .expect("SOURCES.md carries a checksum block between the sha256:begin and sha256:end markers");

    let checksums: BTreeMap<String, String> = block
        .lines()
        .filter_map(|line| line.trim().split_once("  "))
        .map(|(hex, path)| (path.trim().to_owned(), hex.trim().to_owned()))
        .collect();
    assert!(
        !checksums.is_empty(),
        "SOURCES.md's checksum block parsed to nothing — the integrity check would pass vacuously"
    );
    checksums
}
