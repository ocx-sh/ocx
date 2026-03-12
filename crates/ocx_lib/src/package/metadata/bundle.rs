// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use super::env;

/// Constants of known versions of the bundle metadata format.
#[derive(Debug, Clone, Copy, Serialize_repr, Deserialize_repr, PartialEq)]
#[repr(u8)]
pub enum Version {
    V1 = 1,
}

#[cfg(feature = "jsonschema")]
impl schemars::JsonSchema for Version {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Version")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "description": "Bundle metadata format version. Currently only version 1 is supported.",
            "enum": [1]
        })
    }
}

/// Bundle package metadata.
///
/// Declares the format version, optional extraction options, and environment variables
/// that OCX should expose when running commands with this package.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub struct Bundle {
    /// The version of the bundle metadata format.
    /// This allows for future extensions and changes to the format while maintaining backward compatibility.
    pub version: Version,

    /// Number of leading path components to strip when extracting the bundle.
    /// This is a convenient feature for archives not created by OCX, which often contain a single top-level directory.
    /// By default, OCX will not strip any components, and will extract the archive as-is.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub strip_components: Option<u8>,

    #[serde(skip_serializing_if = "env::Env::is_empty", default)]
    pub env: env::Env,
}
