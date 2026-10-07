// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `errors` document: the `--format json` error document and the three vocabularies it names.
//!
//! Each vocabulary is an open scalar enum: `x-ocx-enum` lists the known values, and a validator
//! still accepts a value a newer `ocx` adds.

use ocx::error_document::ErrorDocument;
use ocx_exit::{ErrorCategory, ExitCode};
use serde_json::{Map, Value, json};

/// Canonical published URL of the error document.
pub const ERRORS_ID: &str = schema_id!("errors", errors_version!());

/// Generate the error document as pretty-printed JSON.
pub fn errors_schema() -> String {
    let root = crate::reports::settings()
        .into_generator()
        .into_root_schema_for::<ErrorDocument<'static>>();
    let mut document = serde_json::to_value(&root).expect("a schemars Schema is always serializable");
    crate::reports::publish(&mut document);

    let Value::Object(object) = &mut document else {
        unreachable!("a root schema is an object")
    };
    object.insert("$id".to_owned(), Value::String(ERRORS_ID.to_owned()));
    let defs = object
        .entry("$defs")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .expect("`$defs` is an object");
    defs.insert("ExitCode".to_owned(), exit_code_def());
    defs.insert("ErrorCategory".to_owned(), error_category_def());
    defs.insert("ErrorDetail".to_owned(), error_detail_def());

    point_at(&mut document, &["properties", "exit_code"], "ExitCode");
    point_at(
        &mut document,
        &["$defs", "ErrorBody", "properties", "kind"],
        "ErrorCategory",
    );
    point_at(
        &mut document,
        &["$defs", "ErrorBody", "properties", "detail"],
        "ErrorDetail",
    );

    serde_json::to_string_pretty(&document).expect("a serde_json::Value is always serializable")
}

/// Replace the property at `path` with a reference to `$defs/<def>`, keeping its description.
///
/// # Panics
///
/// When `path` names nothing, so a renamed document field fails generation instead of publishing
/// an unreferenced vocabulary.
fn point_at(document: &mut Value, path: &[&str], def: &str) {
    let property = path
        .iter()
        .try_fold(document, |node, key| node.get_mut(*key))
        .unwrap_or_else(|| panic!("the error document has no {}", path.join(".")));
    let mut replacement = Map::new();
    replacement.insert("$ref".to_owned(), Value::String(format!("#/$defs/{def}")));
    if let Some(description) = property.get("description") {
        replacement.insert("description".to_owned(), description.clone());
    }
    *property = Value::Object(replacement);
}

fn exit_code_def() -> Value {
    let entries: Vec<Value> = ExitCode::ALL
        .iter()
        .map(|code| {
            json!({
                "value": *code as u8,
                "name": format!("{code:?}"),
                "description": code.summary(),
                "category": code.category(),
            })
        })
        .collect();
    json!({
        "type": "integer",
        "description": "The process exit code.",
        "x-ocx-enum": entries,
    })
}

fn error_category_def() -> Value {
    let entries: Vec<Value> = ErrorCategory::ALL
        .iter()
        .map(|category| {
            json!({
                "value": category,
                "name": format!("{category:?}"),
                "description": category.summary(),
            })
        })
        .collect();
    json!({
        "type": "string",
        "description": "The coarse class of an error.",
        "x-ocx-enum": entries,
    })
}

fn error_detail_def() -> Value {
    let mut entries: Vec<_> = ocx::exit::detail_registry();
    entries.sort_by_key(|entry| entry.slug);
    entries.dedup_by_key(|entry| entry.slug);
    let entries: Vec<Value> = entries
        .iter()
        .map(|entry| {
            json!({
                "value": entry.slug,
                "description": entry.summary,
                "exit_code": entry.exit_code as u8,
            })
        })
        .collect();
    json!({
        "type": "string",
        "description": "The specific error within its class.",
        "x-ocx-enum": entries,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn document() -> Value {
        serde_json::from_str(&errors_schema()).expect("the error document is JSON")
    }

    fn entries<'a>(document: &'a Value, def: &str) -> &'a Vec<Value> {
        document
            .pointer(&format!("/$defs/{def}/x-ocx-enum"))
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{def} has no x-ocx-enum"))
    }

    fn keys(entry: &Value) -> BTreeSet<&str> {
        entry
            .as_object()
            .expect("an entry is an object")
            .keys()
            .map(String::as_str)
            .collect()
    }

    #[test]
    fn the_document_carries_its_canonical_id() {
        assert_eq!(document().get("$id").and_then(Value::as_str), Some(ERRORS_ID));
    }

    /// Open scalar enums: the known values live in `x-ocx-enum` only, so a value a newer `ocx` adds
    /// still validates.
    #[test]
    fn the_vocabularies_are_open_scalar_enums() {
        let document = document();
        for (def, scalar) in [
            ("ExitCode", "integer"),
            ("ErrorCategory", "string"),
            ("ErrorDetail", "string"),
        ] {
            let schema = document.pointer(&format!("/$defs/{def}")).expect("defined");
            assert_eq!(schema.get("type").and_then(Value::as_str), Some(scalar), "{def}");
            for closed in ["enum", "oneOf", "anyOf", "const"] {
                assert!(schema.get(closed).is_none(), "{def} carries `{closed}`");
            }
        }
    }

    #[test]
    fn every_exit_code_is_listed_with_its_category() {
        let document = document();
        let entries = entries(&document, "ExitCode");
        assert_eq!(entries.len(), ExitCode::ALL.len());
        for (entry, code) in entries.iter().zip(ExitCode::ALL) {
            assert_eq!(
                keys(entry),
                BTreeSet::from(["value", "name", "description", "category"])
            );
            assert_eq!(entry["value"], json!(*code as u8));
            assert_eq!(entry["category"], json!(code.category()));
        }
        assert!(
            entries
                .iter()
                .any(|entry| entry["value"] == json!(64) && entry["name"] == json!("UsageError"))
        );
    }

    #[test]
    fn every_category_is_listed_by_its_wire_value() {
        let document = document();
        let entries = entries(&document, "ErrorCategory");
        assert_eq!(entries.len(), ErrorCategory::ALL.len());
        for entry in entries {
            assert_eq!(keys(entry), BTreeSet::from(["value", "name", "description"]));
        }
        assert!(entries.iter().any(|entry| entry["value"] == json!("usage_error")));
    }

    #[test]
    fn every_registered_slug_is_listed_once_with_its_exit_code() {
        let document = document();
        let entries = entries(&document, "ErrorDetail");
        let registry = ocx::exit::detail_registry();
        let slugs: BTreeSet<&str> = registry.iter().map(|entry| entry.slug).collect();
        assert_eq!(entries.len(), slugs.len(), "one entry per distinct slug");
        assert!(entries.len() >= 10, "only {} slugs", entries.len());
        for entry in entries {
            assert_eq!(keys(entry), BTreeSet::from(["value", "description", "exit_code"]));
            let slug = entry["value"].as_str().expect("a slug is a string");
            let registered = registry.iter().find(|row| row.slug == slug).expect("registered");
            assert_eq!(entry["exit_code"], json!(registered.exit_code as u8));
        }
    }

    #[test]
    fn the_document_points_at_the_vocabularies() {
        let document = document();
        for (pointer, def) in [
            ("/properties/exit_code/$ref", "ExitCode"),
            ("/$defs/ErrorBody/properties/kind/$ref", "ErrorCategory"),
            ("/$defs/ErrorBody/properties/detail/$ref", "ErrorDetail"),
            ("/$defs/ErrorContext/properties/identifier/$ref", "PackageRef"),
            ("/$defs/ErrorContext/properties/source/$ref", "PackageRef"),
            ("/$defs/ErrorContext/properties/target/$ref", "PackageRef"),
        ] {
            assert_eq!(
                document.pointer(pointer).and_then(Value::as_str),
                Some(format!("#/$defs/{def}").as_str()),
                "{pointer}"
            );
        }
    }
}
