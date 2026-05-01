// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use super::{dependency::Dependencies, entrypoint::Entrypoints, env};

/// Known versions of the bundle metadata format.
///
/// Single variant today; the field exists so future schema bumps can extend
/// the format without breaking existing readers via `serde_repr`'s
/// reject-unknown-on-deserialize behaviour.
#[derive(Debug, Clone, Copy, Serialize_repr, Deserialize_repr, PartialEq, Default)]
#[repr(u8)]
pub enum Version {
    #[default]
    V1 = 1,
}

impl schemars::JsonSchema for Version {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Version")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "integer",
            "description": "Bundle metadata format version.",
            "enum": [1]
        })
    }
}

/// Bundle package metadata.
///
/// Declares the format version, optional extraction options, environment variables,
/// dependencies, and entrypoints that OCX should expose when running commands with
/// this package.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Bundle {
    /// The version of the bundle metadata format.
    /// Reserved for future schema evolution.
    pub version: Version,

    /// Number of leading path components to strip when extracting the bundle.
    /// This is a convenient feature for archives not created by OCX, which often contain a single top-level directory.
    /// By default, OCX will not strip any components, and will extract the archive as-is.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub strip_components: Option<u8>,

    #[serde(skip_serializing_if = "env::Env::is_empty", default)]
    pub env: env::Env,

    /// Ordered list of package dependencies, pinned by digest.
    /// Array order defines environment import order.
    #[serde(skip_serializing_if = "Dependencies::is_empty", default)]
    pub dependencies: Dependencies,

    /// Named entrypoints that `ocx install` generates launchers for.
    ///
    /// Each entry produces a Unix `.sh` script and a Windows `.cmd` batch file
    /// under the package's `entrypoints/` sibling directory at install time.
    /// Absent or empty means no launchers are generated (backward-compat default).
    #[serde(skip_serializing_if = "Entrypoints::is_empty", default)]
    pub entrypoints: Entrypoints,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::visibility::Visibility;

    /// Default `Var.visibility` when absent in serialized form is `private` —
    /// matches ADR `archive/adr_visibility_two_axis_and_exec_modes.md` Tension 1
    /// (decision A, post-research-flip default).
    #[test]
    fn bundle_with_absent_visibility_defaults_to_private() {
        let json = r#"{
            "version": 1,
            "env": [
                { "key": "PATH", "type": "path", "value": "${installPath}/bin", "required": true }
            ]
        }"#;
        let bundle: Bundle = serde_json::from_str(json).expect("metadata parses");
        assert_eq!(bundle.version, Version::V1);
        let vars: Vec<_> = bundle.env.into_iter().collect();
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].key, "PATH");
        assert_eq!(
            vars[0].visibility,
            Visibility::PRIVATE,
            "absent Var.visibility must default to private",
        );
    }
}
