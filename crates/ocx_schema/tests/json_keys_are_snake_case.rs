// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Every JSON document ocx emits must use snake_case keys.
//!
//! This walks the generated JSON Schema for every published **JSON** kind and
//! checks every key that appears under a `properties` object (at any depth —
//! inside `$defs`/`definitions`, `items`, `oneOf`/`anyOf`/`allOf`, or a schema
//! nested under `additionalProperties`) against the snake_case grammar
//! `^[a-z0-9]+(_[a-z0-9]+)*$`. `patternProperties` keys are exempt — they are
//! regexes, not JSON keys.
//!
//! `config`, `project`, and `project-lock` describe TOML files, not JSON
//! output — TOML's own convention on this surface is kebab-case
//! (`lazy-mode`, `no-patches`) — so they are excluded from [`KINDS`].
//!
//! `execution-record` is excluded too, for the opposite reason: it is an
//! in-toto-style document, so its OCX-owned envelope is lowerCamelCase by
//! design (ADR format rule 9) and its borrowed blocks carry their own spec's
//! spelling. `borrowed_vocabulary_matches_spec.rs` covers it instead.
//!
//! A key that comes from an external wire format ocx passes through verbatim
//! is exempt, and the exemption is **derived from the specification, never
//! typed here**: the vendored OCI image-spec platform object and in-toto
//! `ResourceDescriptor` field list under `tests/specs/` supply the names, and
//! whichever of them fail the grammar are the whole allowlist. A hand-typed
//! allowlist is a claim about a specification that no longer has to be true —
//! it was one when `os.version` and `os.features` were spelled out here, and it
//! would have kept being one after upstream changed. The exemption is also
//! **scoped**: it applies only inside a definition that is one of those
//! borrowed shapes, so `mediaType` in a `ResourceDescriptor` is fine and
//! `mediaType` anywhere else is still a failure.
//!
//! An ocx-authored key that fails the grammar is never exempt: it is either
//! fixed, or — if fixing it is a wire-format decision beyond this test's scope
//! — pinned exactly in [`KNOWN_NON_SNAKE`] so it cannot grow silently.

mod spec_vocabulary;

use std::collections::BTreeSet;

use serde_json::Value;
use spec_vocabulary::{assert_vendored_specs_are_authentic, has_platform_marker, in_toto_fields, oci_platform_object};

/// The JSON kinds [`ocx_schema::schema_for`] generates — excludes the TOML
/// kinds (`config`, `project`, `project-lock`; see module doc comment).
const KINDS: &[&str] = &["metadata", "patch", "reports"];

/// Definition names that hold vocabulary borrowed from an external
/// specification, and are therefore where a spec spelling may break ocx's own
/// snake_case convention. A definition is also recognised by the OCI platform
/// description marker, so renaming the type does not silently widen or narrow
/// the exemption.
const BORROWED_DEFINITION_NAMES: &[&str] = &["Platform", "ResourceDescriptor"];

/// Pre-existing ocx-authored non-snake_case keys awaiting a wire-format
/// decision — NOT sanctioned, just pinned so the count cannot grow silently.
///
/// `(kind, json_pointer_path, key)`. Every entry must match a real offender
/// on every run (checked below) — an entry that stops offending is stale and
/// fails the test, same as a new, unlisted offender.
const KNOWN_NON_SNAKE: &[(&str, &str, &str)] = &[];

/// `^[a-z0-9]+(_[a-z0-9]+)*$` without pulling in the `regex` crate: one or
/// more lowercase-alphanumeric runs, separated by single underscores, with no
/// leading, trailing, or doubled underscore.
fn is_snake_case(key: &str) -> bool {
    if key.is_empty() || key.as_bytes()[0] == b'_' || key.as_bytes()[key.len() - 1] == b'_' {
        return false;
    }
    let mut previous_was_underscore = false;
    for byte in key.bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => previous_was_underscore = false,
            b'_' if !previous_was_underscore => previous_was_underscore = true,
            _ => return false,
        }
    }
    true
}

/// Whether `definition` holds borrowed vocabulary, by name or by the OCI
/// platform description marker its hand-authored schema carries.
fn is_borrowed_definition(name: &str, definition: &Value) -> bool {
    BORROWED_DEFINITION_NAMES.contains(&name) || has_platform_marker(definition)
}

/// The exemption list: every borrowed field name that fails ocx's own
/// snake_case grammar.
///
/// Derived from the vendored specs on every run. Today that is the OCI
/// platform's `os.version` and `os.features` plus in-toto's `downloadLocation`
/// and `mediaType`; the latter two name nothing ocx emits, which is fine — this
/// is specification data, not a record of what ocx happens to publish, so an
/// entry that matches nothing is expected rather than stale.
fn borrowed_non_snake_names() -> BTreeSet<String> {
    let platform = oci_platform_object();
    let platform_names: Vec<String> = platform
        .pointer("/properties")
        .and_then(Value::as_object)
        .expect("the vendored platform object declares `properties`")
        .keys()
        .cloned()
        .collect();

    platform_names
        .into_iter()
        .chain(in_toto_fields().into_iter().map(|(_name, json_name, _type)| json_name))
        .filter(|name| !is_snake_case(name))
        .collect()
}

/// One key found under a `properties` object, with its JSON-pointer path
/// (into the generated schema) for a failure message that names exactly
/// where to fix it.
struct Found {
    path: String,
    key: String,
    /// Whether this key sits inside a borrowed-vocabulary definition, and is
    /// therefore eligible for the derived exemption.
    borrowed: bool,
}

/// Walk `value`, collecting every key that lives directly under a
/// `properties` object.
///
/// `patternProperties` keys are skipped (they are regexes), but the schemas
/// nested under both `properties` and `patternProperties` values are still
/// walked, so a key nested arbitrarily deep — inside `$defs`, `items`,
/// `oneOf`/`anyOf`/`allOf`, or a schema hanging off `additionalProperties` —
/// is still visited. `$defs`/`definitions` entries and (for the `reports`
/// kind) the `reports` root-name map are walked into but never checked
/// themselves: those keys are schema/type bookkeeping names, not JSON keys
/// any emitted document carries.
///
/// `borrowed` latches on when the walk enters a borrowed-vocabulary definition
/// and stays on for that whole subtree — a spec spelling is exempt where the
/// spec put it, not everywhere.
fn collect_property_keys(value: &Value, path: &str, borrowed: bool, found: &mut Vec<Found>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let child_path = format!("{path}/{key}");
                match key.as_str() {
                    "$defs" | "definitions" => {
                        if let Value::Object(definitions) = child {
                            for (name, definition) in definitions {
                                collect_property_keys(
                                    definition,
                                    &format!("{child_path}/{name}"),
                                    borrowed || is_borrowed_definition(name, definition),
                                    found,
                                );
                            }
                        }
                    }
                    "properties" => {
                        if let Value::Object(properties) = child {
                            for (property_key, property_schema) in properties {
                                let property_path = format!("{child_path}/{property_key}");
                                found.push(Found {
                                    path: property_path.clone(),
                                    key: property_key.clone(),
                                    borrowed,
                                });
                                collect_property_keys(
                                    property_schema,
                                    &property_path,
                                    borrowed || has_platform_marker(property_schema),
                                    found,
                                );
                            }
                        }
                    }
                    "patternProperties" => {
                        if let Value::Object(properties) = child {
                            for pattern_schema in properties.values() {
                                collect_property_keys(
                                    pattern_schema,
                                    &child_path,
                                    borrowed || has_platform_marker(pattern_schema),
                                    found,
                                );
                            }
                        }
                    }
                    _ => collect_property_keys(child, &child_path, borrowed || has_platform_marker(child), found),
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_property_keys(
                    item,
                    &format!("{path}/{index}"),
                    borrowed || has_platform_marker(item),
                    found,
                );
            }
        }
        _ => {}
    }
}

#[test]
fn every_json_key_ocx_emits_is_snake_case() {
    // The exemption list is only as trustworthy as the vendored specs it comes
    // from, so those are checked against their recorded checksums first.
    assert_vendored_specs_are_authentic();
    let allowed = borrowed_non_snake_names();
    assert!(
        !allowed.is_empty(),
        "no borrowed name failed the snake_case grammar, so the derivation broke — this test would then flag every \
         legitimate spec spelling as an offender rather than silently passing, but fix the derivation, not the \
         ratchet"
    );

    let mut actual_ratchet: BTreeSet<(&str, String, String)> = BTreeSet::new();

    for &kind in KINDS {
        let raw = ocx_schema::schema_for(kind).unwrap_or_else(|| panic!("schema_for({kind:?}) returned None"));
        let value: Value = serde_json::from_str(&raw)
            .unwrap_or_else(|error| panic!("schema_for({kind:?}) produced invalid JSON: {error}"));

        let mut found = Vec::new();
        collect_property_keys(&value, "", false, &mut found);

        for entry in found {
            if is_snake_case(&entry.key) {
                continue;
            }
            if entry.borrowed && allowed.contains(&entry.key) {
                continue;
            }
            actual_ratchet.insert((kind, entry.path, entry.key));
        }
    }

    let expected_ratchet: BTreeSet<(&str, String, String)> = KNOWN_NON_SNAKE
        .iter()
        .map(|(kind, path, key)| (*kind, path.to_string(), key.to_string()))
        .collect();

    let new_offenders: Vec<_> = actual_ratchet.difference(&expected_ratchet).collect();
    let stale_entries: Vec<_> = expected_ratchet.difference(&actual_ratchet).collect();
    assert!(
        new_offenders.is_empty() && stale_entries.is_empty(),
        "KNOWN_NON_SNAKE drifted from reality — new unratcheted offenders: {new_offenders:?}; stale entries that no \
         longer offend (fixed — remove from the list): {stale_entries:?}"
    );
}
