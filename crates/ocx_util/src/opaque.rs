// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publisher-supplied JSON that OCX carries but never models.

use std::borrow::Cow;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

/// Any JSON value: `null`s, key spelling and key order are preserved; numbers are re-emitted in serde_json's form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueJson(pub serde_json::Value);

impl JsonSchema for OpaqueJson {
    fn schema_name() -> Cow<'static, str> {
        "OpaqueJson".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "ocx::OpaqueJson".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "x-ocx-opaque": true,
            "description": "Publisher-supplied JSON, passed through unchanged."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nulls_and_non_snake_keys_round_trip_byte_identically() {
        let input = r#"{"zFirst":null,"Kebab-Key":[null,{"camelCase":null,"a":1}],"aLast":"x"}"#;
        let value: OpaqueJson = serde_json::from_str(input).expect("parses");
        assert_eq!(serde_json::to_string(&value).expect("serialises"), input);
    }

    #[test]
    fn schema_is_an_opaque_leaf() {
        #[derive(JsonSchema)]
        #[allow(dead_code)]
        struct Holder {
            payload: OpaqueJson,
        }
        let schema = serde_json::to_value(schemars::schema_for!(Holder)).expect("schema serialises");
        let def = &schema["$defs"]["OpaqueJson"];
        assert_eq!(def["x-ocx-opaque"], true);
        assert!(def.get("type").is_none(), "an opaque leaf admits any JSON type");
        assert_eq!(OpaqueJson::schema_id(), "ocx::OpaqueJson");
    }
}
