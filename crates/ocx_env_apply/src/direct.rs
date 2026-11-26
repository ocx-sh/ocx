// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `.direct` sidecar: the private record a vendored launcher reads to start its tool.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ApplyKind;

/// The only `schema_version` this release writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

/// The token a launcher replaces with the directory it lives in.
pub const VENDOR_DIR_TOKEN: &str = "@VENDOR_DIR@";

/// One launcher's start-up record, file `<self>.direct` beside the launcher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectSidecar {
    /// Always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Equals the manifest's `vendor_id`, which binds the sidecar to its set.
    pub vendor_id: String,
    /// The launcher's path relative to the vendor directory, without `.exe`.
    #[serde(rename = "self")]
    pub self_path: String,
    /// [`VENDOR_DIR_TOKEN`] followed by a relative path; the token appears nowhere else.
    pub program: String,
    /// Baked entry-point arguments; the token is allowed.
    pub args: Vec<String>,
    /// Keys removed from the child's environment.
    pub unset: Vec<String>,
    /// Records applied in order with [`crate::apply_one`].
    pub env: Vec<EnvRecord>,
}

/// One environment record of a [`DirectSidecar`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvRecord {
    pub key: String,
    pub value: String,
    pub kind: ApplyKind,
    /// Required for `list`, absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

/// A broken sidecar rule; the writer maps it to exit 65, the decoder to 78.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarError {
    /// `schema_version` is not [`SCHEMA_VERSION`].
    SchemaVersion(u32),
    /// A key is empty or holds `=` or NUL.
    InvalidKey(String),
    /// An `env` key is in `unset`, is `OCX_BINARY_PIN` or starts with `__OCX_`.
    ReservedKey(String),
    /// `self` or `program` is not a representable path inside the vendor directory.
    PathNotInRoot(String),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaVersion(found) => write!(f, "unsupported sidecar schema_version {found}"),
            Self::InvalidKey(key) => write!(f, "invalid environment key {key:?}"),
            Self::ReservedKey(key) => write!(f, "reserved environment key {key:?}"),
            Self::PathNotInRoot(path) => write!(f, "path {path:?} is not inside the vendor directory"),
        }
    }
}

// No `std::error::Error` impl yet: the CLI boundary guard requires a `families!` rung for any error a public fn returns, and the rung arrives with the vendor task.
impl DirectSidecar {
    /// Checks every rule of the sidecar contract; `platform_is_windows` selects key folding and device-name rules.
    pub fn validate(&self, platform_is_windows: bool) -> Result<(), SidecarError> {
        let _ = platform_is_windows;
        unimplemented!("DirectSidecar::validate")
    }
}

#[cfg(test)]
mod tests {
    use super::{DirectSidecar, SidecarError};

    fn sidecar(program: &str, env_key: &str, unset: &[&str]) -> DirectSidecar {
        let record = serde_json::json!({
            "schema_version": 1,
            "vendor_id": "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a",
            "self": "selections/default/shells/default/bin/cmake",
            "program": program,
            "args": [],
            "unset": unset,
            "env": [{"key": env_key, "value": "@VENDOR_DIR@/packages/p/content/bin", "kind": "path"}],
        });
        serde_json::from_value(record).expect("fixture decodes")
    }

    const WELL_FORMED: &str = "@VENDOR_DIR@/packages/ocx.sh/sha256/1f/2e3d/content/bin/cmake";

    #[test]
    fn unknown_field_is_a_decode_error() {
        let text = r#"{"schema_version":1,"vendor_id":"v","self":"a","program":"@VENDOR_DIR@/a","args":[],"unset":[],"env":[],"extra":1}"#;
        assert!(serde_json::from_str::<DirectSidecar>(text).is_err());
    }

    /// V-62 in-control: a well-formed `program` is accepted.
    #[test]
    fn v62_well_formed_program_is_accepted() {
        assert_eq!(sidecar(WELL_FORMED, "PATH", &[]).validate(false), Ok(()));
        assert_eq!(sidecar(WELL_FORMED, "PATH", &[]).validate(true), Ok(()));
    }

    /// V-62: `program` outside the token form is refused.
    #[test]
    fn v62_program_outside_the_root_is_refused() {
        for program in ["/bin/sh", "@VENDOR_DIR@/../x", "@VENDOR_DIR@/a/@VENDOR_DIR@/b"] {
            assert!(
                matches!(
                    sidecar(program, "PATH", &[]).validate(false),
                    Err(SidecarError::PathNotInRoot(_))
                ),
                "{program}"
            );
        }
    }

    /// V-62: a Windows device name is not representable on Windows.
    #[test]
    fn v62_windows_device_name_is_refused_on_windows() {
        let con = sidecar("@VENDOR_DIR@/CON", "PATH", &[]);
        assert!(matches!(con.validate(true), Err(SidecarError::PathNotInRoot(_))));
    }

    /// V-72 in-control: an ordinary key is accepted.
    #[test]
    fn v72_ordinary_key_is_accepted() {
        assert_eq!(
            sidecar(WELL_FORMED, "CMAKE_ROOT", &["OCX_SIGNING_KEY"]).validate(false),
            Ok(())
        );
    }

    /// V-72: reserved keys and keys named in `unset` are refused.
    #[test]
    fn v72_reserved_keys_are_refused() {
        for key in ["OCX_BINARY_PIN", "__OCX_ENV_STATE", "__OCX_ANYTHING"] {
            assert!(
                matches!(
                    sidecar(WELL_FORMED, key, &[]).validate(false),
                    Err(SidecarError::ReservedKey(_))
                ),
                "{key}"
            );
        }
        assert!(matches!(
            sidecar(WELL_FORMED, "FOO", &["FOO"]).validate(false),
            Err(SidecarError::ReservedKey(_))
        ));
    }

    /// V-72: on Windows the `unset` collision folds ASCII case.
    #[test]
    fn v72_unset_collision_folds_case_on_windows() {
        assert!(matches!(
            sidecar(WELL_FORMED, "Path", &["PATH"]).validate(true),
            Err(SidecarError::ReservedKey(_))
        ));
        assert_eq!(sidecar(WELL_FORMED, "Path", &["PATH"]).validate(false), Ok(()));
    }

    #[test]
    fn wrong_schema_version_is_refused() {
        let mut bad = sidecar(WELL_FORMED, "PATH", &[]);
        bad.schema_version = 2;
        assert_eq!(bad.validate(false), Err(SidecarError::SchemaVersion(2)));
    }

    #[test]
    fn malformed_keys_are_refused() {
        for key in ["", "A=B", "A\0B"] {
            assert!(
                matches!(
                    sidecar(WELL_FORMED, key, &[]).validate(false),
                    Err(SidecarError::InvalidKey(_))
                ),
                "{key:?}"
            );
        }
    }
}
