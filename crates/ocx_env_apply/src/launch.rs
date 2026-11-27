// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `NAME.launch` record: the private file a launcher reads to learn what to start, tagged by its `kind` key.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::windows_path::check_segment;
use crate::{ApplyKind, KeyFold};

/// The one record schema version; also an input of `LauncherGeneration`.
pub const SCHEMA_VERSION: u32 = 1;

/// The record's file extension, without the dot.
pub const RECORD_EXTENSION: &str = "launch";

/// The largest record `decode` reads.
pub const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// The bound `peek_kind` reads under; a toolchain record is a few hundred bytes.
pub const MAX_PEEK_BYTES: u64 = 64 * 1024;

/// The token a launcher replaces with the directory it lives in.
pub const VENDOR_DIR_TOKEN: &str = "@VENDOR_DIR@";

/// What a launcher starts: the record's `kind` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchKind {
    Direct,
    Package,
    Deferred,
    Toolchain,
}

/// The decoded record; not a serde type itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRecord {
    Direct(DirectRecord),
    Package(PackageRecord),
    Deferred(DeferredRecord),
    Toolchain(ToolchainRecord),
}

/// A vendored launcher's start-up record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectRecord {
    /// Always [`LaunchKind::Direct`]; checked against the peek.
    pub kind: LaunchKind,
    /// Always [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Equals the manifest's `vendor_id`, which binds the record to its set.
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

/// A home entry point: starts the package at `package_root`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRecord {
    pub kind: LaunchKind,
    pub schema_version: u32,
    pub package_root: String,
}

/// A deferred entry point: starts the tool of one pinned package, installing it first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeferredRecord {
    pub kind: LaunchKind,
    pub schema_version: u32,
    pub identifier: String,
}

/// A toolchain trampoline: asks `ocx` for the project's or the global tool set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainRecord {
    pub kind: LaunchKind,
    pub schema_version: u32,
    pub tier: ToolchainTier,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ocx: Option<String>,
}

/// Which tool set a [`ToolchainRecord`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolchainTier {
    Project,
    Global,
}

/// One environment record of a [`DirectRecord`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvRecord {
    pub key: String,
    pub value: String,
    pub kind: ApplyKind,
    /// The list separator; `validate` checks neither its presence nor its use, and `apply_one` defaults a missing
    /// list separator to a space and ignores one on `path` and `constant`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}

/// A broken record rule; the writer maps it to exit 65, the decoder to 78.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// The bytes are not a record: serde's message (unknown field, missing or unknown `kind`, duplicate key, type mismatch).
    Malformed(String),
    /// More than [`MAX_RECORD_BYTES`].
    TooLarge,
    /// The struct's `kind` differs from the peeked one.
    KindMismatch,
    /// `schema_version` is not [`SCHEMA_VERSION`].
    SchemaVersion(u32),
    /// A key is empty or holds `=` or NUL.
    InvalidKey(String),
    /// An `env` key is in `unset`, is `OCX_BINARY_PIN` or starts with `__OCX_`.
    ReservedKey(String),
    /// `self` or `program` is not a representable path inside the vendor directory.
    PathNotInRoot(String),
    /// A string holds a NUL byte.
    NulByte(String),
    /// [`VENDOR_DIR_TOKEN`] in a field of a kind other than `direct`.
    TokenOutsideDirect(String),
    /// A path field that must be absolute is not.
    NotAbsolute(String),
    /// An identifier that is not pinned to a digest.
    NotPinned(String),
    /// `tier = project` without `project_root`, or `global` with one.
    TierMismatch,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(message) => write!(f, "malformed record: {message}"),
            Self::TooLarge => write!(f, "record larger than {MAX_RECORD_BYTES} bytes"),
            Self::KindMismatch => write!(f, "record kind differs from its peeked kind"),
            Self::SchemaVersion(found) => write!(f, "unsupported record schema_version {found}"),
            Self::InvalidKey(key) => write!(f, "invalid environment key {key:?}"),
            Self::ReservedKey(key) => write!(f, "reserved environment key {key:?}"),
            Self::PathNotInRoot(path) => write!(f, "path {path:?} is not inside the vendor directory"),
            Self::NulByte(field) => write!(f, "NUL byte in {field}"),
            Self::TokenOutsideDirect(field) => {
                write!(f, "{VENDOR_DIR_TOKEN} in {field} of a record that is not direct")
            }
            Self::NotAbsolute(path) => write!(f, "path {path:?} is not absolute"),
            Self::NotPinned(identifier) => write!(f, "identifier {identifier:?} is not pinned to a digest"),
            Self::TierMismatch => write!(f, "toolchain tier and project_root disagree"),
        }
    }
}

impl std::error::Error for RecordError {}

impl LaunchRecord {
    /// Reads at most [`MAX_RECORD_BYTES`]: peeks the kind, decodes that kind's struct, checks its `kind`
    /// against the peek, validates it and scans every string for NUL.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first rule the bytes break.
    pub fn decode(bytes: &[u8], windows: bool) -> Result<Self, RecordError> {
        if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_RECORD_BYTES) {
            return Err(RecordError::TooLarge);
        }
        let kind = peek(bytes).map_err(RecordError::Malformed)?;
        // A later schema may add keys the strict shape below refuses, so its version is named before that read.
        if let Ok(PeekVersion {
            schema_version: Some(found),
        }) = serde_json::from_slice(bytes)
            && let Some(found) = found.as_u64().and_then(|version| u32::try_from(version).ok())
            && found != SCHEMA_VERSION
        {
            return Err(RecordError::SchemaVersion(found));
        }
        let malformed = |error: serde_json::Error| RecordError::Malformed(error.to_string());
        // The struct's own `kind` equals the peek or `validate` returns `KindMismatch`.
        let record = match kind {
            LaunchKind::Direct => Self::Direct(serde_json::from_slice(bytes).map_err(malformed)?),
            LaunchKind::Package => Self::Package(serde_json::from_slice(bytes).map_err(malformed)?),
            LaunchKind::Deferred => Self::Deferred(serde_json::from_slice(bytes).map_err(malformed)?),
            LaunchKind::Toolchain => Self::Toolchain(serde_json::from_slice(bytes).map_err(malformed)?),
        };
        record.validate(windows)?;
        Ok(record)
    }

    /// Compact JSON of the kind's struct (`kind` first, fields in declaration order), then `\n`.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let json = match self {
            Self::Direct(record) => serde_json::to_vec(record),
            Self::Package(record) => serde_json::to_vec(record),
            Self::Deferred(record) => serde_json::to_vec(record),
            Self::Toolchain(record) => serde_json::to_vec(record),
        };
        // These structs hold only strings, integers and enums with string keys: serialisation cannot fail.
        let mut bytes = json.unwrap_or_default();
        bytes.push(b'\n');
        bytes
    }

    /// Checks the rules of the record's kind.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first rule broken.
    pub fn validate(&self, windows: bool) -> Result<(), RecordError> {
        match self {
            Self::Direct(record) => record.validate(windows),
            Self::Package(record) => record.validate(windows),
            Self::Deferred(record) => record.validate(windows),
            Self::Toolchain(record) => record.validate(windows),
        }?;
        // A NUL ends a Windows environment entry or command line early, in any string of any kind.
        match self.strings().find(|(_, text)| text.contains('\0')) {
            Some((field, _)) => Err(RecordError::NulByte(field.to_owned())),
            None => Ok(()),
        }
    }

    /// Every string the record holds, with the name of its field.
    fn strings(&self) -> Box<dyn Iterator<Item = (&'static str, &str)> + '_> {
        match self {
            Self::Direct(record) => Box::new(
                [
                    ("vendor_id", record.vendor_id.as_str()),
                    ("self", record.self_path.as_str()),
                    ("program", record.program.as_str()),
                ]
                .into_iter()
                .chain(record.args.iter().map(|arg| ("args", arg.as_str())))
                .chain(record.unset.iter().map(|key| ("unset", key.as_str())))
                .chain(record.env.iter().flat_map(|entry| {
                    [("env key", entry.key.as_str()), ("env value", entry.value.as_str())]
                        .into_iter()
                        .chain(entry.separator.as_deref().map(|separator| ("env separator", separator)))
                })),
            ),
            Self::Package(record) => Box::new(std::iter::once(("package_root", record.package_root.as_str()))),
            Self::Deferred(record) => Box::new(std::iter::once(("identifier", record.identifier.as_str()))),
            Self::Toolchain(record) => Box::new(
                record
                    .project_root
                    .as_deref()
                    .map(|root| ("project_root", root))
                    .into_iter()
                    .chain(record.ocx.as_deref().map(|ocx| ("ocx", ocx))),
            ),
        }
    }

    /// The record's kind.
    #[must_use]
    pub fn kind(&self) -> LaunchKind {
        match self {
            Self::Direct(_) => LaunchKind::Direct,
            Self::Package(_) => LaunchKind::Package,
            Self::Deferred(_) => LaunchKind::Deferred,
            Self::Toolchain(_) => LaunchKind::Toolchain,
        }
    }
}

/// The `kind` of any JSON object holding a valid one, every other key ignored (a later schema included);
/// `None` otherwise. The one non-validating reader.
#[must_use]
pub fn peek_kind(bytes: &[u8]) -> Option<LaunchKind> {
    if u64::try_from(bytes.len()).map_or(true, |len| len > MAX_PEEK_BYTES) {
        return None;
    }
    peek(bytes).ok()
}

/// The one key `peek` reads; serde ignores every other key and refuses a duplicate `kind`.
#[derive(Deserialize)]
struct Peek {
    kind: LaunchKind,
}

/// The `schema_version` of a record, every other key ignored; the strict read reports one that is not an integer.
#[derive(Deserialize)]
struct PeekVersion {
    #[serde(default)]
    schema_version: Option<serde_json::Value>,
}

/// [`peek_kind`] without the size bound, so `decode` can peek a record of up to [`MAX_RECORD_BYTES`].
fn peek(bytes: &[u8]) -> Result<LaunchKind, String> {
    // A derived struct also reads a JSON array positionally, so a record must open as an object.
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err("a record is a JSON object".to_owned());
    }
    serde_json::from_slice::<Peek>(bytes)
        .map(|peeked| peeked.kind)
        .map_err(|error| error.to_string())
}

/// The checks every kind shares: its own `kind` and `schema_version`.
fn check_header(kind: LaunchKind, expected: LaunchKind, schema_version: u32) -> Result<(), RecordError> {
    if kind != expected {
        return Err(RecordError::KindMismatch);
    }
    if schema_version != SCHEMA_VERSION {
        return Err(RecordError::SchemaVersion(schema_version));
    }
    Ok(())
}

/// An absolute path field outside `direct`: no CR or LF, no [`VENDOR_DIR_TOKEN`]; absolute on any host, and for a
/// Windows launcher fully qualified, because a path without a drive or share follows the caller's current drive.
fn check_absolute_path(field: &str, path: &str, windows: bool) -> Result<(), RecordError> {
    let absolute = if windows {
        is_fully_qualified(path, true)
    } else {
        is_absolute_any_host(path)
    };
    if !absolute {
        return Err(RecordError::NotAbsolute(path.to_owned()));
    }
    if path.contains(['\r', '\n']) {
        return Err(RecordError::Malformed(format!("{field} holds CR or LF")));
    }
    check_no_token(field, path)
}

fn check_no_token(field: &str, text: &str) -> Result<(), RecordError> {
    if text.contains(VENDOR_DIR_TOKEN) {
        return Err(RecordError::TokenOutsideDirect(field.to_owned()));
    }
    Ok(())
}

/// `C:\`, `C:/`, `\\server\share`, `\\?\` or a leading `/`, judged the same on every host.
#[must_use]
pub fn is_absolute_any_host(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\")
        || path.starts_with('/')
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'))
}

/// Whether `path` names one file whatever the working directory: on Windows a drive letter and a separator, or two
/// separators; a lone `\x` or `/x` reads on the current drive (CWE-427). On Unix a leading `/`.
#[must_use]
pub fn is_fully_qualified(path: &str, windows: bool) -> bool {
    if !windows {
        return path.starts_with('/');
    }
    let separator = |byte: u8| matches!(byte, b'/' | b'\\');
    match path.as_bytes() {
        [first, second, ..] if separator(*first) && separator(*second) => true,
        [drive, b':', third, ..] => drive.is_ascii_alphabetic() && separator(*third),
        _ => false,
    }
}

/// Structural admissibility of a pinned identifier, deliberately not an OCI reference parser: printable ASCII, no
/// leading `-`, and `<algorithm>:<hex>` after the last `@`. `ocx` re-parses the value on the receiving end.
fn is_pinned_identifier(value: &str) -> bool {
    if !value.bytes().all(|byte| matches!(byte, 0x21..=0x7E)) || value.starts_with('-') {
        return false;
    }
    // The last `@`, not the first; a digest requirement stops a tag a registry controls.
    let Some(at) = value.rfind('@').filter(|at| *at > 0) else {
        return false;
    };
    let Some((algorithm, hex)) = value[at + 1..].split_once(':') else {
        return false;
    };
    !algorithm.is_empty()
        && algorithm.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9'))
        && !hex.is_empty()
        && hex.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether `program` is a batch file, which `CreateProcessW` would hand to `cmd.exe`.
///
/// Windows ignores trailing dots and spaces in a file name, so `tool.cmd. ` still runs as `tool.cmd`.
#[must_use]
pub fn is_batch_target(program: &str) -> bool {
    let name = program.trim_end_matches(['.', ' ']);
    [".bat", ".cmd"].iter().any(|extension| {
        name.len()
            .checked_sub(extension.len())
            .and_then(|cut| name.get(cut..))
            .is_some_and(|tail| tail.eq_ignore_ascii_case(extension))
    })
}

/// The record path for a launcher path: its directory, its file name minus one trailing `.exe`
/// (ASCII case-insensitive) when `windows`, then `.launch`.
#[must_use]
pub fn record_path_for(launcher: &str, windows: bool) -> String {
    let split = if windows {
        launcher.rfind(['/', '\\'])
    } else {
        launcher.rfind('/')
    };
    let (directory, name) = launcher.split_at(split.map_or(0, |at| at + 1));
    let stem = match name.len().checked_sub(4) {
        Some(at) if windows && name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(".exe") => &name[..at],
        _ => name,
    };
    format!("{directory}{stem}.{RECORD_EXTENSION}")
}

impl PackageRecord {
    /// Checks `schema_version`, `kind`, an absolute `package_root` and the absence of CR, LF and the token.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first rule broken.
    pub fn validate(&self, windows: bool) -> Result<(), RecordError> {
        check_header(self.kind, LaunchKind::Package, self.schema_version)?;
        check_absolute_path("package_root", &self.package_root, windows)
    }
}

impl DeferredRecord {
    /// Checks `schema_version`, `kind`, a pinned `identifier` and the absence of the token.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first rule broken.
    pub fn validate(&self, _windows: bool) -> Result<(), RecordError> {
        check_header(self.kind, LaunchKind::Deferred, self.schema_version)?;
        if !is_pinned_identifier(&self.identifier) {
            return Err(RecordError::NotPinned(self.identifier.clone()));
        }
        check_no_token("identifier", &self.identifier)
    }
}

impl ToolchainRecord {
    /// Checks `schema_version`, `kind`, the tier/`project_root` pairing, an absolute non-batch `ocx` and the
    /// absence of CR, LF and the token.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first rule broken.
    pub fn validate(&self, windows: bool) -> Result<(), RecordError> {
        check_header(self.kind, LaunchKind::Toolchain, self.schema_version)?;
        match (self.tier, self.project_root.as_deref()) {
            (ToolchainTier::Project, Some(root)) => check_absolute_path("project_root", root, windows)?,
            (ToolchainTier::Project, None) | (ToolchainTier::Global, Some(_)) => return Err(RecordError::TierMismatch),
            (ToolchainTier::Global, None) => {}
        }
        if let Some(ocx) = self.ocx.as_deref() {
            check_absolute_path("ocx", ocx, windows)?;
            if is_batch_target(ocx) {
                return Err(RecordError::Malformed("ocx is a batch file".to_owned()));
            }
        }
        Ok(())
    }
}

impl DirectRecord {
    /// Checks every rule of the direct record; `platform_is_windows` selects key folding and device-name rules.
    ///
    /// # Errors
    ///
    /// The [`RecordError`] of the first rule broken.
    pub fn validate(&self, platform_is_windows: bool) -> Result<(), RecordError> {
        if self.kind != LaunchKind::Direct {
            return Err(RecordError::KindMismatch);
        }
        if self.schema_version != SCHEMA_VERSION {
            return Err(RecordError::SchemaVersion(self.schema_version));
        }
        let fold = if platform_is_windows {
            KeyFold::AsciiCaseInsensitive
        } else {
            KeyFold::Exact
        };
        for key in self.unset.iter().chain(self.env.iter().map(|r| &r.key)) {
            if key.is_empty() || key.contains(['=', '\0']) {
                return Err(RecordError::InvalidKey(key.clone()));
            }
        }
        // A NUL ends a Windows environment entry or command line early.
        let nul_fields = [("self", &self.self_path), ("program", &self.program)]
            .into_iter()
            .chain(self.args.iter().map(|arg| ("args", arg)))
            .chain(self.env.iter().map(|record| ("env value", &record.value)));
        for (field, text) in nul_fields {
            if text.contains('\0') {
                return Err(RecordError::NulByte(field.to_owned()));
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
                return Err(RecordError::ReservedKey(key.clone()));
            }
        }
        if !relative_path_ok(&self.self_path, platform_is_windows) {
            return Err(RecordError::PathNotInRoot(self.self_path.clone()));
        }
        let program_ok = self
            .program
            .strip_prefix(VENDOR_DIR_TOKEN)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|rest| relative_path_ok(rest, platform_is_windows));
        if !program_ok {
            return Err(RecordError::PathNotInRoot(self.program.clone()));
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
    use super::{
        DeferredRecord, DirectRecord, EnvRecord, LaunchKind, LaunchRecord, PackageRecord, RecordError, ToolchainRecord,
        ToolchainTier, key_eq, peek_kind, record_path_for,
    };
    use crate::{ApplyKind, KeyFold};

    fn sidecar(program: &str, env_key: &str, unset: &[&str]) -> DirectRecord {
        let record = serde_json::json!({
            "kind": "direct",
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
        let text = r#"{"kind":"direct","schema_version":1,"vendor_id":"v","self":"a","program":"@VENDOR_DIR@/a","args":[],"unset":[],"env":[],"extra":1}"#;
        assert!(serde_json::from_str::<DirectRecord>(text).is_err());
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
                    Err(RecordError::PathNotInRoot(_))
                ),
                "{program}"
            );
        }
    }

    /// A Windows device name is not representable on Windows.
    #[test]
    fn v62_windows_device_name_is_refused_on_windows() {
        let con = sidecar("@VENDOR_DIR@/CON", "PATH", &[]);
        assert!(matches!(con.validate(true), Err(RecordError::PathNotInRoot(_))));
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
                    Err(RecordError::ReservedKey(_))
                ),
                "{key}"
            );
        }
        assert!(matches!(
            sidecar(WELL_FORMED, "FOO", &["FOO"]).validate(false),
            Err(RecordError::ReservedKey(_))
        ));
    }

    /// On Windows the `unset` collision folds ASCII case.
    #[test]
    fn v72_unset_collision_folds_case_on_windows() {
        assert!(matches!(
            sidecar(WELL_FORMED, "Path", &["PATH"]).validate(true),
            Err(RecordError::ReservedKey(_))
        ));
        assert_eq!(sidecar(WELL_FORMED, "Path", &["PATH"]).validate(false), Ok(()));
    }

    #[test]
    fn wrong_schema_version_is_refused() {
        let mut bad = sidecar(WELL_FORMED, "PATH", &[]);
        bad.schema_version = 2;
        assert_eq!(bad.validate(false), Err(RecordError::SchemaVersion(2)));
    }

    #[test]
    fn malformed_keys_are_refused() {
        for key in ["", "A=B", "A\0B"] {
            assert!(
                matches!(
                    sidecar(WELL_FORMED, key, &[]).validate(false),
                    Err(RecordError::InvalidKey(_))
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
                Err(RecordError::ReservedKey(_))
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
                assert!(matches!(bad.validate(windows), Err(RecordError::NulByte(_))), "{bad:?}");
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

    // ── Contract tests (adr_unified_launcher § 3): written from the contract, not from the code ──

    const GOLDEN_DIRECT: &str = r#"{"kind":"direct","schema_version":1,"vendor_id":"sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a","self":"l/cmake","program":"@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/bin/cmake","args":[],"unset":["OCX_ANNOUNCE_GIT_TOKEN","OCX_BINARY_PIN","OCX_IDENTITY_TOKEN","OCX_KEY_PASSWORD","OCX_SIGNING_KEY","OCX_TELEMETRY_HEADERS","__OCX_ENV_STATE"],"env":[{"key":"PATH","value":"@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/bin","kind":"path"},{"key":"CMAKE_ROOT","value":"@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/share/cmake-3.31","kind":"constant"}]}"#;
    const GOLDEN_PACKAGE: &str = r#"{"kind":"package","schema_version":1,"package_root":"/home/u/.ocx/packages/ocx.sh/sha256/5a/0c1e9d2b7f3a4c5a0c1e9d2b7f3a4c"}"#;
    const GOLDEN_DEFERRED: &str = r#"{"kind":"deferred","schema_version":1,"identifier":"ocx.sh/kitware/cmake:3.31.6@sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988"}"#;
    const GOLDEN_PROJECT: &str = r#"{"kind":"toolchain","schema_version":1,"tier":"project","project_root":"/work/app","ocx":"/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"}"#;
    const GOLDEN_GLOBAL: &str = r#"{"kind":"toolchain","schema_version":1,"tier":"global","ocx":"/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"}"#;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn golden_direct() -> LaunchRecord {
        LaunchRecord::Direct(DirectRecord {
            kind: LaunchKind::Direct,
            schema_version: 1,
            vendor_id: "sha256:0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a0b5f2a4e9c7d3b1a".into(),
            self_path: "l/cmake".into(),
            program: "@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/bin/cmake".into(),
            args: Vec::new(),
            unset: strings(&[
                "OCX_ANNOUNCE_GIT_TOKEN",
                "OCX_BINARY_PIN",
                "OCX_IDENTITY_TOKEN",
                "OCX_KEY_PASSWORD",
                "OCX_SIGNING_KEY",
                "OCX_TELEMETRY_HEADERS",
                "__OCX_ENV_STATE",
            ]),
            env: vec![
                EnvRecord {
                    key: "PATH".into(),
                    value: "@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/bin".into(),
                    kind: ApplyKind::Path,
                    separator: None,
                },
                EnvRecord {
                    key: "CMAKE_ROOT".into(),
                    value: "@VENDOR_DIR@/p/1f2e3d4c5b6a7988/c/share/cmake-3.31".into(),
                    kind: ApplyKind::Constant,
                    separator: None,
                },
            ],
        })
    }

    fn golden_package() -> LaunchRecord {
        LaunchRecord::Package(PackageRecord {
            kind: LaunchKind::Package,
            schema_version: 1,
            package_root: "/home/u/.ocx/packages/ocx.sh/sha256/5a/0c1e9d2b7f3a4c5a0c1e9d2b7f3a4c".into(),
        })
    }

    fn golden_deferred() -> LaunchRecord {
        LaunchRecord::Deferred(DeferredRecord {
            kind: LaunchKind::Deferred,
            schema_version: 1,
            identifier:
                "ocx.sh/kitware/cmake:3.31.6@sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988"
                    .into(),
        })
    }

    const OCX_PATH: &str = "/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx";

    fn toolchain(tier: ToolchainTier, project_root: Option<&str>, ocx: Option<&str>) -> ToolchainRecord {
        ToolchainRecord {
            kind: LaunchKind::Toolchain,
            schema_version: 1,
            tier,
            project_root: project_root.map(str::to_owned),
            ocx: ocx.map(str::to_owned),
        }
    }

    fn golden_project() -> LaunchRecord {
        LaunchRecord::Toolchain(toolchain(ToolchainTier::Project, Some("/work/app"), Some(OCX_PATH)))
    }

    fn golden_global() -> LaunchRecord {
        LaunchRecord::Toolchain(toolchain(ToolchainTier::Global, None, Some(OCX_PATH)))
    }

    /// Every kind, its golden text and its record.
    fn goldens() -> Vec<(&'static str, LaunchRecord)> {
        vec![
            (GOLDEN_DIRECT, golden_direct()),
            (GOLDEN_PACKAGE, golden_package()),
            (GOLDEN_DEFERRED, golden_deferred()),
            (GOLDEN_PROJECT, golden_project()),
            (GOLDEN_GLOBAL, golden_global()),
        ]
    }

    /// `encode` is compact JSON, `kind` first, then a newline, byte-equal to the specified examples.
    #[test]
    fn contract_encode_is_byte_equal_to_the_four_examples() {
        for (text, record) in goldens() {
            assert_eq!(String::from_utf8(record.encode()).expect("utf-8"), format!("{text}\n"));
        }
    }

    /// One valid record per kind decodes to the record that encodes to it.
    #[test]
    fn contract_decode_accepts_one_valid_record_per_kind() {
        for (text, record) in goldens() {
            assert_eq!(
                LaunchRecord::decode(text.as_bytes(), false),
                Ok(record.clone()),
                "{text}"
            );
            assert_eq!(LaunchRecord::decode(&record.encode(), false), Ok(record), "{text}");
        }
    }

    /// A key the kind's struct lacks is refused, whether unknown or a sibling kind's.
    #[test]
    fn contract_decode_refuses_extra_key() {
        let extra = GOLDEN_PACKAGE.replace('}', r#","extra":1}"#);
        assert!(matches!(
            LaunchRecord::decode(extra.as_bytes(), false),
            Err(RecordError::Malformed(_))
        ));
    }

    #[test]
    fn contract_decode_refuses_a_key_of_a_sibling_kind() {
        let sibling = GOLDEN_PACKAGE.replace('}', r#","identifier":"x"}"#);
        assert!(matches!(
            LaunchRecord::decode(sibling.as_bytes(), false),
            Err(RecordError::Malformed(_))
        ));
    }

    #[test]
    fn contract_decode_refuses_an_unknown_key_in_an_env_entry() {
        let bad = GOLDEN_DIRECT.replace(r#""kind":"path"}"#, r#""kind":"path","extra":1}"#);
        assert_ne!(bad, GOLDEN_DIRECT, "the mutation landed");
        assert!(matches!(
            LaunchRecord::decode(bad.as_bytes(), false),
            Err(RecordError::Malformed(_))
        ));
    }

    #[test]
    fn contract_decode_refuses_a_missing_unknown_or_duplicate_kind() {
        let missing = GOLDEN_PACKAGE.replace(r#""kind":"package","#, "");
        let unknown = GOLDEN_PACKAGE.replace(r#""kind":"package""#, r#""kind":"launcher""#);
        let duplicate = GOLDEN_PACKAGE.replace('}', r#","kind":"package"}"#);
        for (name, text) in [("missing", missing), ("unknown", unknown), ("duplicate", duplicate)] {
            assert_ne!(text, GOLDEN_PACKAGE, "{name}: the mutation landed");
            assert!(
                matches!(
                    LaunchRecord::decode(text.as_bytes(), false),
                    Err(RecordError::Malformed(_))
                ),
                "{name}"
            );
        }
    }

    /// The struct's `kind` must equal the peek; a direct record whose own `kind` says otherwise is refused.
    #[test]
    fn contract_validate_refuses_a_struct_kind_other_than_the_peek() {
        let LaunchRecord::Direct(mut record) = golden_direct() else {
            unreachable!("the direct golden is direct")
        };
        record.kind = LaunchKind::Package;
        assert_eq!(record.validate(false), Err(RecordError::KindMismatch));
    }

    #[test]
    fn contract_decode_refuses_schema_version_other_than_one() {
        let two = GOLDEN_PACKAGE.replace(r#""schema_version":1"#, r#""schema_version":2"#);
        assert_ne!(two, GOLDEN_PACKAGE, "the mutation landed");
        assert_eq!(
            LaunchRecord::decode(two.as_bytes(), false),
            Err(RecordError::SchemaVersion(2))
        );
    }

    /// A newer schema may add keys; the refusal names the version, not the first key the strict shape rejects.
    #[test]
    fn decode_reports_a_newer_schema_version_before_it_reads_the_strict_shape() {
        let newer = GOLDEN_PACKAGE.replace(r#""schema_version":1"#, r#""schema_version":2,"added_in_v2":true"#);
        assert!(newer.contains("added_in_v2"), "the mutation landed");
        assert_eq!(
            LaunchRecord::decode(newer.as_bytes(), false),
            Err(RecordError::SchemaVersion(2))
        );
        let unknown_key = GOLDEN_PACKAGE.replace(r#""schema_version":1"#, r#""schema_version":1,"added":true"#);
        assert!(
            matches!(
                LaunchRecord::decode(unknown_key.as_bytes(), false),
                Err(RecordError::Malformed(_))
            ),
            "an unknown key under the current version stays malformed"
        );
    }

    #[test]
    fn contract_decode_refuses_more_than_one_mebibyte() {
        let padding = " ".repeat(1024 * 1024);
        let big = format!("{GOLDEN_PACKAGE}{padding}");
        assert_eq!(LaunchRecord::decode(big.as_bytes(), false), Err(RecordError::TooLarge));
    }

    #[test]
    fn contract_decode_refuses_a_nul_in_any_string() {
        let direct_args = GOLDEN_DIRECT.replace(r#""args":[]"#, r#""args":["a\u0000b"]"#);
        let direct_separator = GOLDEN_DIRECT.replace(r#""kind":"constant"}"#, r#""kind":"list","separator":"\u0000"}"#);
        let package = GOLDEN_PACKAGE.replace("/home/u", "/home/\\u0000u");
        for (name, text) in [
            ("args", direct_args),
            ("separator", direct_separator),
            ("root", package),
        ] {
            assert!(
                matches!(
                    LaunchRecord::decode(text.as_bytes(), false),
                    Err(RecordError::NulByte(_))
                ),
                "{name}: {text}"
            );
        }
    }

    /// `tier = project` holds exactly when `project_root` is present and absolute.
    #[test]
    fn contract_toolchain_tier_mismatch_is_refused() {
        let project_without_root = toolchain(ToolchainTier::Project, None, None);
        let global_with_root = toolchain(ToolchainTier::Global, Some("/work/app"), None);
        assert_eq!(project_without_root.validate(false), Err(RecordError::TierMismatch));
        assert_eq!(global_with_root.validate(false), Err(RecordError::TierMismatch));
        assert!(matches!(
            toolchain(ToolchainTier::Project, Some("work/app"), None).validate(false),
            Err(RecordError::NotAbsolute(_))
        ));
        assert_eq!(
            toolchain(ToolchainTier::Project, Some("/work/app"), Some(OCX_PATH)).validate(false),
            Ok(())
        );
        assert_eq!(toolchain(ToolchainTier::Global, None, None).validate(false), Ok(()));
    }

    /// `ocx`, when present, is absolute and not a batch file.
    #[test]
    fn contract_toolchain_ocx_is_absolute_and_not_a_batch_file() {
        let relative = toolchain(ToolchainTier::Global, None, Some("bin/ocx"));
        let batch = toolchain(ToolchainTier::Global, None, Some("C:\\tools\\ocx.cmd"));
        assert!(matches!(relative.validate(true), Err(RecordError::NotAbsolute(_))));
        assert!(batch.validate(true).is_err());
    }

    /// The vendor token is refused in every kind but `direct`.
    #[test]
    fn contract_token_outside_direct_is_refused() {
        let package = PackageRecord {
            kind: LaunchKind::Package,
            schema_version: 1,
            package_root: "/@VENDOR_DIR@/p".into(),
        };
        let deferred = DeferredRecord {
            kind: LaunchKind::Deferred,
            schema_version: 1,
            identifier:
                "ocx.sh/a@VENDOR_DIR@:1@sha256:1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c5b6a7988".into(),
        };
        let toolchain_record = toolchain(ToolchainTier::Project, Some("/@VENDOR_DIR@"), None);
        assert!(matches!(
            package.validate(false),
            Err(RecordError::TokenOutsideDirect(_))
        ));
        assert!(matches!(
            deferred.validate(false),
            Err(RecordError::TokenOutsideDirect(_))
        ));
        assert!(matches!(
            toolchain_record.validate(false),
            Err(RecordError::TokenOutsideDirect(_))
        ));
    }

    /// `package_root` is absolute on any host; `deferred.identifier` is pinned.
    #[test]
    fn contract_package_root_is_absolute_and_identifier_is_pinned() {
        let relative = PackageRecord {
            kind: LaunchKind::Package,
            schema_version: 1,
            package_root: "packages/p".into(),
        };
        assert!(matches!(relative.validate(false), Err(RecordError::NotAbsolute(_))));
        assert!(matches!(relative.validate(true), Err(RecordError::NotAbsolute(_))));
        let unpinned = DeferredRecord {
            kind: LaunchKind::Deferred,
            schema_version: 1,
            identifier: "ocx.sh/kitware/cmake:3.31.6".into(),
        };
        assert!(matches!(unpinned.validate(false), Err(RecordError::NotPinned(_))));
        assert_eq!(golden_package().validate(false), Ok(()));
        assert_eq!(golden_deferred().validate(false), Ok(()));
    }

    /// The record path drops one `.exe` (any case) on Windows only.
    #[test]
    fn contract_record_path_for_windows_drops_the_exe() {
        assert_eq!(record_path_for("C:\\a\\cmake.exe", true), "C:\\a\\cmake.launch");
        assert_eq!(record_path_for("C:\\a\\CMAKE.EXE", true), "C:\\a\\CMAKE.launch");
        assert_eq!(record_path_for("C:/a/cmake.exe.exe", true), "C:/a/cmake.exe.launch");
        assert_eq!(record_path_for("cmake", true), "cmake.launch");
    }

    #[test]
    fn contract_record_path_for_unix_keeps_the_name() {
        assert_eq!(record_path_for("/a/foo.exe", false), "/a/foo.exe.launch");
        assert_eq!(record_path_for("/a/foo", false), "/a/foo.launch");
        assert_eq!(record_path_for("/a.exe/foo", false), "/a.exe/foo.launch");
    }

    /// `peek_kind` reads the `kind` of any object holding a valid one and validates nothing else.
    #[test]
    fn contract_peek_kind_ignores_every_other_key() {
        for (text, record) in goldens() {
            assert_eq!(peek_kind(text.as_bytes()), Some(record.kind()), "{text}");
        }
        let later = GOLDEN_PACKAGE.replace(r#""schema_version":1"#, r#""schema_version":2,"future":[1,2]"#);
        assert_eq!(peek_kind(later.as_bytes()), Some(LaunchKind::Package));
    }

    #[test]
    fn contract_peek_kind_is_none_without_a_valid_kind() {
        for text in [
            "",
            "not json",
            "[]",
            r#"{"schema_version":1}"#,
            r#"{"kind":"launcher"}"#,
            r#"{"kind":7}"#,
        ] {
            assert_eq!(peek_kind(text.as_bytes()), None, "{text:?}");
        }
    }

    /// A derived struct reads a JSON array positionally; a record must still be an object.
    #[test]
    fn contract_decode_and_peek_refuse_a_json_array() {
        let array = r#"["package",1,"/home/u"]"#;
        assert_eq!(peek_kind(array.as_bytes()), None);
        assert!(matches!(
            LaunchRecord::decode(array.as_bytes(), false),
            Err(RecordError::Malformed(_))
        ));
    }

    /// Absolute means `/`, a drive letter or a UNC prefix on every host; a lone backslash is neither.
    #[test]
    fn contract_package_root_absolute_forms_are_host_independent() {
        for (root, absolute) in [
            ("/p", true),
            ("C:\\p", true),
            ("C:/p", true),
            ("\\\\server\\share", true),
            ("\\p", false),
            ("C:p", false),
            ("p", false),
        ] {
            let record = PackageRecord {
                kind: LaunchKind::Package,
                schema_version: 1,
                package_root: root.into(),
            };
            assert_eq!(record.validate(false).is_ok(), absolute, "{root}");
        }
    }

    /// On Windows a path with no drive or share (`\p`, `/p`) names a file on the current drive, so every absolute path
    /// field of every home kind refuses it; the forms that name one drive or share stay.
    #[test]
    fn a_windows_record_refuses_a_path_on_the_current_drive() {
        for (path, qualified) in [
            ("C:\\p", true),
            ("C:/p", true),
            ("\\\\server\\share", true),
            ("//server/share", true),
            ("\\p", false),
            ("/p", false),
            ("C:p", false),
            ("p", false),
        ] {
            let package = format!(r#"{{"kind":"package","schema_version":1,"package_root":{path:?}}}"#);
            let project =
                format!(r#"{{"kind":"toolchain","schema_version":1,"tier":"project","project_root":{path:?}}}"#);
            let ocx = format!(r#"{{"kind":"toolchain","schema_version":1,"tier":"global","ocx":{path:?}}}"#);
            for (field, text) in [("package_root", package), ("project_root", project), ("ocx", ocx)] {
                let decoded = LaunchRecord::decode(text.as_bytes(), true);
                assert_eq!(decoded.is_ok(), qualified, "{field} {path}: {decoded:?}");
            }
        }
        // The same text on a Unix host is judged by the shared rule: a leading `/` is absolute.
        let rooted = r#"{"kind":"package","schema_version":1,"package_root":"/p"}"#;
        assert!(LaunchRecord::decode(rooted.as_bytes(), false).is_ok());
    }
}
