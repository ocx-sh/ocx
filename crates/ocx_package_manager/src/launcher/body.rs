// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `toolchain` launch record a rendered trampoline carries beside its launcher program.
//!
//! The record is data the launcher reads, never a script a shell runs, so a baked path is a JSON string and
//! cannot be expanded. See `adr_unified_launcher.md` § 3.

use std::path::{Path, PathBuf};

use ocx_env_apply::launch::{LaunchKind, LaunchRecord, SCHEMA_VERSION, ToolchainRecord, ToolchainTier};

use super::safety::LauncherSafeString;

/// Which toolchain home a rendered trampoline hands back to `ocx` — the only thing it bakes,
/// so a trampoline on disk survives every change to how a home composes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrampolineTarget {
    /// A project home, selected by `--project '<abs root>'`.
    Project(PathBuf),
    /// The global home (`$OCX_HOME`), selected by the valueless `--global`.
    Global,
}

/// The `toolchain` record of every trampoline one render writes; `windows` is the home's target.
///
/// `ocx_binary` is baked only when absolute on some host; otherwise the launcher resolves `ocx` itself. The
/// record is validated as the launcher will read it, so one it would refuse (exit 78) is never written.
///
/// # Errors
///
/// - [`crate::Error::LauncherPathNotUtf8`] when the project root or `ocx` path is not UTF-8.
/// - [`crate::Error::ToolchainHomeNotAbsolute`] when the project root is relative.
/// - [`crate::Error::LauncherUnsafeCharacter`] when either path holds a launcher-unsafe character.
/// - [`crate::Error::InternalFile`] naming `context` when the launcher would refuse the record (a Windows path
///   without a drive, a batch-file `ocx`).
pub(crate) fn toolchain_record(
    target: &TrampolineTarget,
    ocx_binary: Option<&Path>,
    windows: bool,
    context: &Path,
) -> Result<LaunchRecord, crate::Error> {
    let (tier, project_root) = match target {
        TrampolineTarget::Project(root) => (
            ToolchainTier::Project,
            Some(absolute_project_root(root)?.as_str().to_owned()),
        ),
        TrampolineTarget::Global => (ToolchainTier::Global, None),
    };
    let ocx = match ocx_binary {
        Some(path) => {
            let text = utf8_baked_path(path)?;
            is_absolute_any_host(text)
                .then(|| LauncherSafeString::new(text))
                .transpose()?
                .map(|safe| safe.as_str().to_owned())
        }
        None => None,
    };
    let record = LaunchRecord::Toolchain(ToolchainRecord {
        kind: LaunchKind::Toolchain,
        schema_version: SCHEMA_VERSION,
        tier,
        project_root,
        ocx,
    });
    super::validate_for_write(&record, windows, context)?;
    Ok(record)
}

/// Validates a baked project root: UTF-8, then absolute, then launcher-safe.
///
/// # Errors
///
/// [`crate::Error::LauncherPathNotUtf8`], [`crate::Error::ToolchainHomeNotAbsolute`] or
/// [`crate::Error::LauncherUnsafeCharacter`], in that order.
fn absolute_project_root(root: &Path) -> Result<LauncherSafeString, crate::Error> {
    let root = utf8_baked_path(root)?;
    if !is_absolute_any_host(root) {
        return Err(crate::Error::ToolchainHomeNotAbsolute {
            value: root.to_string(),
        });
    }
    LauncherSafeString::new(root)
}

/// The one conversion from a `Path` to text a generated file bakes verbatim.
///
/// Never `to_string_lossy`: its U+FFFD passes [`LauncherSafeString`] and bakes a path that does not exist.
///
/// # Errors
///
/// [`crate::Error::LauncherPathNotUtf8`], carrying the lossy rendering.
pub(super) fn utf8_baked_path(path: &Path) -> Result<&str, crate::Error> {
    path.to_str().ok_or_else(|| crate::Error::LauncherPathNotUtf8 {
        path: path.to_string_lossy().into_owned(),
    })
}

/// Whether `value` is an absolute path on any host, not on the rendering one; the record's own
/// `validate` then applies the target's rule.
fn is_absolute_any_host(value: &str) -> bool {
    let bytes = value.as_bytes();
    if value.starts_with("\\\\") {
        return true;
    }
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
    {
        return true;
    }
    value.starts_with('/')
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{TrampolineTarget, toolchain_record};

    const INSTALLED_OCX: &str = "/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx";

    fn project(root: &str) -> TrampolineTarget {
        TrampolineTarget::Project(PathBuf::from(root))
    }

    fn encoded(target: &TrampolineTarget, ocx: Option<&str>, windows: bool) -> Result<String, crate::Error> {
        toolchain_record(target, ocx.map(Path::new), windows, Path::new("/home"))
            .map(|record| String::from_utf8(record.encode()).expect("a record is UTF-8"))
    }

    /// § 3.3's two examples, byte for byte: the launcher reads exactly these keys.
    #[test]
    fn the_record_is_the_decision_records_golden() {
        assert_eq!(
            encoded(&project("/work/app"), Some(INSTALLED_OCX), false).expect("legal"),
            format!(
                "{{\"kind\":\"toolchain\",\"schema_version\":1,\"tier\":\"project\",\"project_root\":\"/work/app\",\"ocx\":\"{INSTALLED_OCX}\"}}\n"
            )
        );
        assert_eq!(
            encoded(&TrampolineTarget::Global, Some(INSTALLED_OCX), false).expect("legal"),
            format!(
                "{{\"kind\":\"toolchain\",\"schema_version\":1,\"tier\":\"global\",\"ocx\":\"{INSTALLED_OCX}\"}}\n"
            )
        );
    }

    /// Without an absolute `ocx` the key is omitted and the launcher resolves `ocx` itself.
    #[test]
    fn a_relative_or_absent_ocx_is_not_baked() {
        for ocx in [None, Some("ocx"), Some("bin/ocx")] {
            assert_eq!(
                encoded(&TrampolineTarget::Global, ocx, false).expect("legal"),
                "{\"kind\":\"toolchain\",\"schema_version\":1,\"tier\":\"global\"}\n",
                "{ocx:?}"
            );
        }
    }

    /// A shell metacharacter in the root is data: the record carries it verbatim.
    #[test]
    fn a_hostile_root_is_carried_verbatim_as_json() {
        let root = "/w/he$(touch pwned)`id`re";
        let text = encoded(&project(root), None, false).expect("`$`, `(` and a backtick are launcher-safe");
        let value: serde_json::Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(value["project_root"], root);
    }

    #[test]
    fn a_relative_project_root_is_refused() {
        assert!(matches!(
            encoded(&project("w/proj"), None, false),
            Err(crate::Error::ToolchainHomeNotAbsolute { value }) if value == "w/proj"
        ));
    }

    #[test]
    fn a_launcher_unsafe_character_in_the_root_is_refused() {
        for character in ['\'', '"', '\n', '\r', '\0'] {
            let root = format!("/w/a{character}b");
            assert!(
                matches!(
                    encoded(&project(&root), None, false),
                    Err(crate::Error::LauncherUnsafeCharacter { character: found, .. }) if found == character
                ),
                "{character:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt as _;
        let root = PathBuf::from(std::ffi::OsStr::from_bytes(b"/w/\xff"));
        assert!(matches!(
            toolchain_record(
                &TrampolineTarget::Project(root.clone()),
                None,
                false,
                Path::new("/home")
            ),
            Err(crate::Error::LauncherPathNotUtf8 { .. })
        ));
        assert!(matches!(
            toolchain_record(&TrampolineTarget::Global, Some(&root), false, Path::new("/home")),
            Err(crate::Error::LauncherPathNotUtf8 { .. })
        ));
    }

    /// LA: the launcher exits 78 on a driveless Windows path and on a batch-file `ocx`; such a record is refused
    /// at render, never written.
    #[test]
    fn a_record_the_windows_launcher_refuses_is_never_built() {
        assert!(matches!(
            encoded(&project("/w/proj"), None, true),
            Err(crate::Error::InternalFile(..))
        ));
        assert!(matches!(
            encoded(&TrampolineTarget::Global, Some("C:\\ocx\\ocx.cmd"), true),
            Err(crate::Error::InternalFile(..))
        ));
        assert!(encoded(&project("C:\\w\\proj"), Some("C:\\ocx\\ocx.exe"), true).is_ok());
    }
}
