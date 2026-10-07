// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The representation subset: the JSON Schema constructs a published document may use, shared by the lint and
//! every later reader of the documents.

use std::collections::BTreeSet;

use serde_json::Value;

/// Every keyword a published `reports` or `errors` schema may carry. `enum` only inside an unknown arm's `not`.
pub const ALLOWLIST: &[&str] = &[
    "$schema",
    "$id",
    "$defs",
    "$ref",
    "title",
    "description",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "oneOf",
    "const",
    "not",
    "enum",
    "format",
    "pattern",
    "minimum",
    "maximum",
    "maxLength",
    "uniqueItems",
    "propertyNames",
    X_ENUM,
    X_UNKNOWN_VARIANT,
    X_OPAQUE,
];

/// The members an `x-ocx-enum` entry may carry; `value` and `description` are required.
pub const ENUM_ENTRY_MEMBERS: &[&str] = &["value", "name", "description", "category", "exit_code"];

/// Scalar-enum marker: an open enum whose entries document each known value.
pub const X_ENUM: &str = "x-ocx-enum";
/// Marks the one catch-all arm of a tagged union.
pub const X_UNKNOWN_VARIANT: &str = "x-ocx-unknown-variant";
/// Marks a leaf whose content is passed through unchanged.
pub const X_OPAQUE: &str = "x-ocx-opaque";

/// The vocabulary `$def` names: shared concepts every document spells through one definition.
pub const VOCABULARY: &[&str] = &[
    "PackageRef",
    "PinnedPackageRef",
    "Digest",
    "Platform",
    "PackageVersion",
    "Timestamp",
    "ByteSize",
    "AbsolutePath",
    "RelativePath",
    "RegistryHost",
    "RedactedUrl",
];

/// Formats only a vocabulary `$def` may carry, with the `$def` that owns each.
pub const VOCABULARY_FORMATS: &[(&str, &str)] = &[("date-time", "Timestamp")];

fn is_separated(name: &str, separator: u8) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes[0] == separator || bytes[bytes.len() - 1] == separator {
        return false;
    }
    bytes
        .windows(2)
        .all(|pair| !(pair[0] == separator && pair[1] == separator))
        && bytes
            .iter()
            .all(|&byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == separator)
}

/// `^[a-z0-9]+(_[a-z0-9]+)*$`.
pub fn is_snake_case(name: &str) -> bool {
    is_separated(name, b'_')
}

/// `^[a-z0-9]+(-[a-z0-9]+)*$`.
pub fn is_kebab_case(name: &str) -> bool {
    is_separated(name, b'-')
}

/// The `$def` name a local `$ref` (`#/$defs/<name>`) points at.
pub fn ref_target(node: &Value) -> Option<&str> {
    node.get("$ref")?.as_str()?.strip_prefix("#/$defs/")
}

/// Whether `node` is an opaque leaf.
pub fn is_opaque(node: &Value) -> bool {
    node.get(X_OPAQUE) == Some(&Value::Bool(true))
}

/// Whether `node` claims to be the unknown arm of a tagged union.
pub fn is_marked_unknown(node: &Value) -> bool {
    node.get(X_UNKNOWN_VARIANT) == Some(&Value::Bool(true))
}

/// Whether `node` is a well-formed unknown arm: marked, an object, `required: ["type"]`, `type` a string whose
/// `not.enum` is exactly the `known` tags of its sibling arms.
pub fn is_unknown_arm(node: &Value, known: &BTreeSet<&str>) -> bool {
    let tag = node.pointer("/properties/type");
    is_marked_unknown(node)
        && node.get("type") == Some(&Value::from("object"))
        && node.get("required") == Some(&serde_json::json!(["type"]))
        && tag.and_then(|tag| tag.get("type")) == Some(&Value::from("string"))
        && tag
            .and_then(|tag| tag.pointer("/not/enum"))
            .and_then(Value::as_array)
            .and_then(|values| values.iter().map(Value::as_str).collect::<Option<BTreeSet<_>>>())
            .is_some_and(|excluded| &excluded == known)
}

/// The string tag a tagged-union arm carries in `properties.type.const`, when it also requires `type`.
pub fn arm_tag(node: &Value) -> Option<&str> {
    let required = node.get("required")?.as_array()?;
    if !required.iter().any(|name| name == "type") {
        return None;
    }
    node.pointer("/properties/type/const")?.as_str()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn snake_and_kebab_case() {
        for good in ["a", "a1", "abc_def", "x_1_y"] {
            assert!(is_snake_case(good), "{good}");
        }
        for bad in ["", "_a", "a_", "a__b", "aB", "a-b", "a.b"] {
            assert!(!is_snake_case(bad), "{bad}");
        }
        assert!(is_kebab_case("out-file"));
        for bad in ["badFlag", "out_file", "-a", "a--b"] {
            assert!(!is_kebab_case(bad), "{bad}");
        }
    }

    #[test]
    fn ref_target_reads_local_defs_only() {
        assert_eq!(ref_target(&json!({"$ref": "#/$defs/Digest"})), Some("Digest"));
        assert_eq!(ref_target(&json!({"$ref": "https://x/y.json"})), None);
        assert_eq!(ref_target(&json!({"type": "string"})), None);
    }

    #[test]
    fn unknown_arm_needs_every_part() {
        let arm = json!({
            "x-ocx-unknown-variant": true,
            "type": "object",
            "required": ["type"],
            "properties": { "type": { "type": "string", "not": { "enum": ["a"] } } }
        });
        let known = BTreeSet::from(["a"]);
        assert!(is_unknown_arm(&arm, &known));
        for other in [BTreeSet::new(), BTreeSet::from(["a", "b"])] {
            assert!(!is_unknown_arm(&arm, &other), "{other:?}");
        }
        for pointer in ["/required", "/type", "/properties/type/not", "/x-ocx-unknown-variant"] {
            let mut broken = arm.clone();
            let (parent, key) = pointer.rsplit_once('/').expect("a pointer with a parent");
            broken
                .pointer_mut(parent)
                .and_then(Value::as_object_mut)
                .expect("an object parent")
                .remove(key);
            assert!(!is_unknown_arm(&broken, &known), "{pointer}");
        }
    }

    #[test]
    fn arm_tag_needs_the_tag_required() {
        let arm = json!({"properties": {"type": {"const": "a"}}, "required": ["type"]});
        assert_eq!(arm_tag(&arm), Some("a"));
        assert_eq!(arm_tag(&json!({"properties": {"type": {"const": "a"}}})), None);
    }

    #[test]
    fn allowlist_holds_the_shared_input_keywords() {
        for keyword in ["maxLength", "uniqueItems", "propertyNames"] {
            assert!(ALLOWLIST.contains(&keyword), "{keyword}");
        }
        for keyword in ["allOf", "anyOf", "minItems", "if", "default", "examples", "$comment"] {
            assert!(!ALLOWLIST.contains(&keyword), "{keyword}");
        }
    }
}
