// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `.direct` sidecar: the private record a vendored launcher reads to start its tool.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::windows_path::check_segment;
use crate::{ApplyKind, KeyFold};

/// The only `schema_version` this release writes and reads.
pub const SCHEMA_VERSION: u32 = 1;

/// The sidecar's file extension, without the dot: the writer and the launcher both name the file `<self>.direct`.
pub const SIDECAR_EXTENSION: &str = "direct";

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
    /// An `env` value, `args` element, `program` or `self` holds a NUL byte.
    NulByte(String),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaVersion(found) => write!(f, "unsupported sidecar schema_version {found}"),
            Self::InvalidKey(key) => write!(f, "invalid environment key {key:?}"),
            Self::ReservedKey(key) => write!(f, "reserved environment key {key:?}"),
            Self::PathNotInRoot(path) => write!(f, "path {path:?} is not inside the vendor directory"),
            Self::NulByte(field) => write!(f, "NUL byte in {field}"),
        }
    }
}

// No `std::error::Error` impl yet: the CLI boundary guard requires a `families!` rung for any error a public fn returns, and the rung arrives with the vendor task.
impl DirectSidecar {
    /// Checks every rule of the sidecar contract; `platform_is_windows` selects key folding and device-name rules.
    pub fn validate(&self, platform_is_windows: bool) -> Result<(), SidecarError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(SidecarError::SchemaVersion(self.schema_version));
        }
        let fold = if platform_is_windows {
            KeyFold::AsciiCaseInsensitive
        } else {
            KeyFold::Exact
        };
        for key in self.unset.iter().chain(self.env.iter().map(|r| &r.key)) {
            if key.is_empty() || key.contains(['=', '\0']) {
                return Err(SidecarError::InvalidKey(key.clone()));
            }
        }
        // A NUL ends a Windows environment entry or command line early.
        let nul_fields = [("self", &self.self_path), ("program", &self.program)]
            .into_iter()
            .chain(self.args.iter().map(|arg| ("args", arg)))
            .chain(self.env.iter().map(|record| ("env value", &record.value)));
        for (field, text) in nul_fields {
            if text.contains('\0') {
                return Err(SidecarError::NulByte(field.to_owned()));
            }
        }
        for record in &self.env {
            let key = &record.key;
            // Windows treats `__ocx_env_state` as `__OCX_ENV_STATE`.
            let reserved = key_eq("OCX_BINARY_PIN", key, fold)
                || (platform_is_windows && key.get(..6).is_some_and(|head| head.eq_ignore_ascii_case("__OCX_")))
                || key.starts_with("__OCX_")
                || self.unset.iter().any(|u| key_eq(u, key, fold));
            if reserved {
                return Err(SidecarError::ReservedKey(key.clone()));
            }
        }
        if !relative_path_ok(&self.self_path, platform_is_windows) {
            return Err(SidecarError::PathNotInRoot(self.self_path.clone()));
        }
        let program_ok = self
            .program
            .strip_prefix(VENDOR_DIR_TOKEN)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|rest| relative_path_ok(rest, platform_is_windows));
        if !program_ok {
            return Err(SidecarError::PathNotInRoot(self.program.clone()));
        }
        Ok(())
    }
}

/// Whether two environment keys name the same variable under `fold`.
fn key_eq(left: &str, right: &str, fold: KeyFold) -> bool {
    match fold {
        KeyFold::Exact => left == right,
        KeyFold::AsciiCaseInsensitive => left.eq_ignore_ascii_case(right),
    }
}

/// The manifest path rules: relative, `/`-separated, no empty/`.`/`..` segment, no token; on Windows also
/// no `:` or `\`, no device name and no segment ending in `.` or a space.
fn relative_path_ok(path: &str, windows: bool) -> bool {
    if path.contains(VENDOR_DIR_TOKEN) || path.contains('\0') {
        return false;
    }
    path.split('/').all(|segment| {
        if matches!(segment, "" | "." | "..") {
            return false;
        }
        !windows || check_segment(segment).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::{DirectSidecar, SidecarError, key_eq};
    use crate::KeyFold;

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

    /// In control: a well-formed `program` is accepted.
    #[test]
    fn v62_well_formed_program_is_accepted() {
        assert_eq!(sidecar(WELL_FORMED, "PATH", &[]).validate(false), Ok(()));
        assert_eq!(sidecar(WELL_FORMED, "PATH", &[]).validate(true), Ok(()));
    }

    /// `program` outside the token form is refused.
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

    /// A Windows device name is not representable on Windows.
    #[test]
    fn v62_windows_device_name_is_refused_on_windows() {
        let con = sidecar("@VENDOR_DIR@/CON", "PATH", &[]);
        assert!(matches!(con.validate(true), Err(SidecarError::PathNotInRoot(_))));
    }

    /// In control: an ordinary key is accepted.
    #[test]
    fn v72_ordinary_key_is_accepted() {
        assert_eq!(
            sidecar(WELL_FORMED, "CMAKE_ROOT", &["OCX_SIGNING_KEY"]).validate(false),
            Ok(())
        );
    }

    /// Reserved keys and keys named in `unset` are refused.
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

    /// On Windows the `unset` collision folds ASCII case.
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

    /// The Windows fold sees `PATH` and `Path` as one key; the exact control sees two.
    #[test]
    fn v73_windows_fold_unifies_path_keys() {
        assert!(!key_eq("PATH", "Path", KeyFold::Exact));
        assert!(key_eq("PATH", "Path", KeyFold::AsciiCaseInsensitive));
    }

    /// Multi-byte neighbours of device names never panic and are not device names.
    #[test]
    fn multibyte_device_lookalikes_do_not_panic() {
        for segment in [
            "CO\u{e9}",
            "CON\u{e9}",
            "LPT1\u{e9}",
            "\u{c4}UX",
            "COM\u{e9}\u{e9}",
            "\u{e9}",
        ] {
            assert!(
                sidecar(&format!("@VENDOR_DIR@/{segment}"), "PATH", &[])
                    .validate(true)
                    .is_ok()
            );
        }
        assert!(sidecar("@VENDOR_DIR@/con.\u{e9}", "PATH", &[]).validate(true).is_err());
    }

    /// The `__OCX_` prefix folds ASCII case on Windows and stays exact on Unix.
    #[test]
    fn reserved_prefix_folds_case_on_windows_only() {
        for key in ["__ocx_env_state", "Ocx_Binary_Pin"] {
            assert!(matches!(
                sidecar(WELL_FORMED, key, &[]).validate(true),
                Err(SidecarError::ReservedKey(_))
            ));
        }
        assert_eq!(sidecar(WELL_FORMED, "__ocx_env_state", &[]).validate(false), Ok(()));
    }

    /// A NUL in any value, argument, `program` or `self` is refused on every platform.
    #[test]
    fn nul_bytes_are_refused() {
        let mut value = sidecar(WELL_FORMED, "PATH", &[]);
        value.env[0].value = "a\0b".into();
        let mut arg = sidecar(WELL_FORMED, "PATH", &[]);
        arg.args = vec!["a\0b".into()];
        let program = sidecar("@VENDOR_DIR@/a\0b", "PATH", &[]);
        let mut own = sidecar(WELL_FORMED, "PATH", &[]);
        own.self_path = "a\0b".into();
        for bad in [value, arg, program, own] {
            for windows in [false, true] {
                assert!(
                    matches!(bad.validate(windows), Err(SidecarError::NulByte(_))),
                    "{bad:?}"
                );
            }
        }
    }

    #[test]
    fn windows_path_rules() {
        for p in [
            "@VENDOR_DIR@/a\\b",
            "@VENDOR_DIR@/C:/x",
            "@VENDOR_DIR@/a./b",
            "@VENDOR_DIR@/lpt9.txt",
            "@VENDOR_DIR@/COM0",
            "@VENDOR_DIR@/lpt0.txt",
        ] {
            assert!(sidecar(p, "PATH", &[]).validate(true).is_err(), "{p}");
        }
        assert!(sidecar("@VENDOR_DIR@/console", "PATH", &[]).validate(true).is_ok());
    }
}
