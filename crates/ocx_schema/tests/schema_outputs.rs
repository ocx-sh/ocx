// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Specification tests for the schema generator's output contract.
//!
//! These tests pin the published `$id` URLs and the structural shape that
//! consumers (taplo, Schema Store, downstream LSPs) depend on. They run
//! against [`ocx_schema::schema_for`] directly — no subprocess.
//!
//! Phase 10 specification scope (per plan_project_toolchain.md lines 859–873):
//! * `project` schema must publish at `https://ocx.sh/schemas/project/v1.json`
//!   and expose object-typed `tools` and `groups` properties.
//! * `project-lock` schema must publish at
//!   `https://ocx.sh/schemas/project-lock/v1.json`, carry a top-level
//!   `$comment` flagging the format as machine-generated and subject to
//!   evolution (architect path-1 finding), and constrain the `lock_version`
//!   to `[1]`.
//! * Unknown schema kinds return `None`.
//!
//! Tests that hold today (the structural pieces already wired by the stub
//! phase — `$id`, `lock_version` enum) ensure no regression at the implement
//! gate. Tests that fail today (the architect's `$comment` requirement and
//! the `groups` rename) drive the implement-phase deliverable.

use serde_json::Value;

/// Parse a generated schema string into a JSON `Value`. Panics with a
/// descriptive message if the generator emits malformed JSON.
fn parse(kind: &str) -> Value {
    let raw = ocx_schema::schema_for(kind).unwrap_or_else(|| panic!("schema_for({kind:?}) returned None"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("schema_for({kind:?}) produced invalid JSON: {e}\n----\n{raw}"))
}

#[test]
fn project_schema_publishes_at_canonical_id() {
    let schema = parse("project");
    let id = schema
        .get("$id")
        .and_then(Value::as_str)
        .expect("project schema must carry a top-level $id");
    assert_eq!(
        id, "https://ocx.sh/schemas/project/v1.json",
        "project schema $id must match the canonical published URL"
    );
}

#[test]
fn project_schema_exposes_tools_and_groups_objects() {
    let schema = parse("project");
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("project schema must have a top-level `properties` object");

    // ProjectConfig serializes the `tools` field directly (no rename).
    let tools = properties
        .get("tools")
        .expect("project schema must declare `properties.tools`");
    assert_eq!(
        tools.get("type").and_then(Value::as_str),
        Some("object"),
        "`properties.tools.type` must be `object`"
    );

    // The named-groups field. The plan's spec calls this surface `groups`,
    // but the Rust source `rename`s to `group` in TOML/JSON via
    // `#[serde(rename = "group")]`. Accept either spelling so the
    // implement phase can decide whether to rename the JSON key or document
    // the discrepancy. Failing here means BOTH are missing — a real
    // regression.
    let groups = properties.get("groups").or_else(|| properties.get("group"));
    let groups = groups.unwrap_or_else(|| {
        panic!(
            "project schema must declare a named-groups property — \
             expected `properties.groups` (per plan spec) or \
             `properties.group` (per current `rename = \"group\"` source). \
             Properties present: {:?}",
            properties.keys().collect::<Vec<_>>()
        )
    });
    assert_eq!(
        groups.get("type").and_then(Value::as_str),
        Some("object"),
        "named-groups property must be an `object`"
    );
}

#[test]
fn project_lock_schema_publishes_at_canonical_id() {
    let schema = parse("project-lock");
    let id = schema
        .get("$id")
        .and_then(Value::as_str)
        .expect("project-lock schema must carry a top-level $id");
    assert_eq!(
        id, "https://ocx.sh/schemas/project-lock/v1.json",
        "project-lock schema $id must match the canonical published URL"
    );
}

#[test]
fn project_lock_schema_carries_machine_generated_comment() {
    // Architect path-1 finding: project-lock schema MUST carry a top-level
    // `$comment` flagging the format as machine-generated and subject to
    // evolution. The user-guide locking subsection mirrors this with a
    // callout warning users not to hand-author `ocx.lock` (see the doc-side
    // test in test/tests/test_doc_project_toolchain.py).
    let schema = parse("project-lock");
    let comment = schema.get("$comment").and_then(Value::as_str).expect(
        "project-lock schema must carry a top-level `$comment` \
             warning consumers that the format is machine-generated. \
             Architect finding: see plan_project_toolchain.md Phase 10 binding constraints.",
    );
    let lower = comment.to_lowercase();
    assert!(
        lower.contains("machine"),
        "$comment must mention `machine` (`{comment}`)"
    );
    assert!(
        lower.contains("evolve") || lower.contains("evolution") || lower.contains("evolving"),
        "$comment must indicate the format may evolve (`{comment}`)"
    );
    assert!(comment.len() >= 60, "$comment too short to be useful: {comment:?}");
}

#[test]
fn project_lock_schema_pins_lock_version_to_one() {
    // The on-disk format version is currently 1. Tightening to `enum: [1]`
    // means a v2 manuscript fed into a v1 schema fails validation — exactly
    // the desired guard so consumers don't silently accept future-format
    // files.
    let schema = parse("project-lock");

    // Walk: properties.metadata is a `$ref` to `#/$defs/LockMetadata`,
    // which holds the `lock_version` shape. The walker prefers a direct
    // inline `properties.metadata.properties.lock_version` (legal under
    // schemars' inlining strategy) but falls back to `$defs.LockMetadata`.
    let metadata = schema
        .get("properties")
        .and_then(|p| p.get("metadata"))
        .expect("project-lock schema must declare `properties.metadata`");

    let metadata_props = metadata
        .get("properties")
        .or_else(|| {
            metadata
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix("#/$defs/"))
                .and_then(|name| schema.get("$defs").and_then(|d| d.get(name)))
                .and_then(|m| m.get("properties"))
        })
        .expect(
            "project-lock metadata must expose `properties` \
             (either inline or via `$ref` into `$defs`)",
        );

    let lock_version = metadata_props
        .get("lock_version")
        .expect("metadata must declare `lock_version` property");

    // `lock_version` may be inlined or a `$ref` into `#/$defs/LockVersion`.
    let lv_obj = if let Some(r) = lock_version.get("$ref").and_then(Value::as_str) {
        let name = r
            .strip_prefix("#/$defs/")
            .expect("lock_version $ref must point into #/$defs/");
        schema
            .get("$defs")
            .and_then(|d| d.get(name))
            .unwrap_or_else(|| panic!("lock_version $ref target {name} missing from $defs"))
    } else {
        lock_version
    };

    let enum_values = lv_obj
        .get("enum")
        .and_then(Value::as_array)
        .expect("lock_version must declare an `enum` constraint");
    assert_eq!(
        enum_values.len(),
        1,
        "lock_version enum must contain exactly one value (`[1]`); got {enum_values:?}"
    );
    let value = enum_values[0]
        .as_i64()
        .expect("lock_version enum value must be an integer");
    assert_eq!(
        value, 1,
        "lock_version enum must constrain to `[1]` (rejects v2 manuscripts)"
    );
}

#[test]
fn unknown_schema_kind_returns_none() {
    assert!(
        ocx_schema::schema_for("nonsense").is_none(),
        "unknown schema kinds must return None for the binary to surface a usage error"
    );
    assert!(
        ocx_schema::schema_for("").is_none(),
        "empty schema kind must return None"
    );
}
