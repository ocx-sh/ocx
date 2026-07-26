// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hand-built JSON Schema fragments for config surfaces `schemars` cannot
//! infer.
//!
//! `#[derive(JsonSchema)]` reads a field's **Rust type**, so it never sees a
//! `#[serde(deserialize_with = ...)]` hand-rolled union. A normalized struct
//! that accepts "bare string OR table" on the wire therefore publishes a
//! table-only schema, and every correct file using the string shorthand gets
//! red-underlined in a taplo-enabled editor. Confirmed shipped defect for
//! `$defs.MirrorConfig` in the `config` schema.
//!
//! [`string_or_table`] is the one shared fix. Reach for it from a
//! `#[schemars(schema_with = ...)]` field attribute or a manual
//! `impl JsonSchema`, never re-derive a second union style by hand.

/// A `oneOf` of a bare-string shorthand and a fully-specified table.
///
/// `string_description` documents what the shorthand means (it is the arm a
/// derive would silently drop). `table` is the object arm verbatim — the
/// caller owns its `properties` / `required` / `additionalProperties`, since
/// the two known unions disagree on all three.
pub fn string_or_table(string_description: &str, table: serde_json::Value) -> schemars::Schema {
    schemars::json_schema!({
        "oneOf": [
            {
                "type": "string",
                "description": string_description
            },
            table
        ]
    })
}
