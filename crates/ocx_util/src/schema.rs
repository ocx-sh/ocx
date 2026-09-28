// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hand-built JSON Schema fragments for config surfaces `schemars` cannot infer.
//!
//! A derive never sees a `deserialize_with` union, so a string-or-table field publishes a table-only schema.

/// A `oneOf` of a bare-string shorthand and `table`, the object arm verbatim.
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
