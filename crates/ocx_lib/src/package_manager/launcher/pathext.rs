// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher file extension + Windows `PATHEXT` helpers.
//!
//! All launcher-aware: [`LAUNCHER_EXT`] names the on-disk extension OCX
//! emits, [`includes_launcher`] tests a `PATHEXT` for it (case-insensitive),
//! [`pathext_with_launcher`] returns a value to write back when missing, and
//! [`emplace_pathext`] applies that to a child [`Env`]. Renaming the launcher
//! extension is a one-site edit of [`LAUNCHER_EXT`].

use crate::env::Env;

/// Canonical uppercase form of the launcher extension as it appears in
/// `PATHEXT`-style export contexts. Matching is case-insensitive (see
/// [`includes_launcher`]); this constant is only the form OCX emits.
pub const LAUNCHER_EXT: &str = ".CMD";

/// Lowercase form for case-insensitive PATHEXT matching on Windows.
#[cfg(target_os = "windows")]
fn launcher_extension() -> &'static str {
    ".cmd"
}

/// Returns `true` when `path_ext` lists the launcher extension
/// (case-insensitive). Always `true` on non-Windows — `PATHEXT` does not
/// exist there. Caller decides when to invoke; this never inspects package
/// metadata. See `ocx install`, `ocx select`, `ocx shell env`,
/// `ocx ci export`, `ocx shell profile load` for the consumer-boundary
/// warning pattern; `ocx exec` and `ocx env` for the auto-inject pattern.
pub fn includes_launcher(path_ext: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        let ext = launcher_extension();
        path_ext
            .split(';')
            .any(|segment| segment.trim().eq_ignore_ascii_case(ext))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path_ext;
        true
    }
}

/// Returns the `PATHEXT` value to write back, or `None` when the current
/// value already lists the launcher extension (case-insensitive).
#[cfg(target_os = "windows")]
pub(crate) fn pathext_with_launcher(current: &str) -> Option<String> {
    if includes_launcher(current) {
        return None;
    }
    Some(if current.is_empty() {
        format!("{LAUNCHER_EXT};.EXE;.BAT;.COM")
    } else {
        format!("{LAUNCHER_EXT};{current}")
    })
}

/// Ensure `env`'s `PATHEXT` lists the OCX launcher extension so generated
/// `.cmd` shims are picked up by the Windows shell resolver.
///
/// No-op on non-Windows platforms — `PATHEXT` is a Windows concept.
#[cfg(not(target_os = "windows"))]
pub fn emplace_pathext(_env: &mut Env) {}

/// Windows implementation: read the current `PATHEXT`, prepend the
/// launcher extension when missing, leave it untouched otherwise.
#[cfg(target_os = "windows")]
pub fn emplace_pathext(env: &mut Env) {
    let current = env.get("PATHEXT").and_then(|v| v.to_str()).unwrap_or("").to_string();
    if let Some(new) = pathext_with_launcher(&current) {
        env.set("PATHEXT", new);
    }
}

#[cfg(test)]
mod tests {
    // ── launcher_extension (Windows only) ────────────────────────────────

    #[cfg(windows)]
    #[test]
    fn launcher_extension_is_static_str() {
        assert_eq!(super::launcher_extension(), ".cmd");
    }

    // ── includes_launcher ─────────────────────────────────────────────────

    #[test]
    fn includes_launcher_on_non_windows_always_true() {
        // Regardless of what string we pass, non-Windows returns true.
        #[cfg(not(windows))]
        {
            assert!(super::includes_launcher(""));
            assert!(super::includes_launcher(".EXE;.BAT"));
            assert!(super::includes_launcher("anything"));
        }
    }

    #[cfg(windows)]
    mod pathext_windows {
        use super::super::{includes_launcher, pathext_with_launcher};

        #[test]
        fn detects_cmd_lowercase() {
            assert!(includes_launcher(".cmd"));
        }

        #[test]
        fn detects_cmd_uppercase() {
            assert!(includes_launcher(".CMD"));
        }

        #[test]
        fn detects_cmd_mixed_case() {
            assert!(includes_launcher(".Cmd"));
            assert!(includes_launcher(".cMd"));
        }

        #[test]
        fn detects_cmd_among_multiple_extensions() {
            assert!(includes_launcher(".EXE;.BAT;.CMD;.COM"));
        }

        #[test]
        fn detects_cmd_with_surrounding_whitespace() {
            // Some environments include spaces around the semicolons.
            assert!(includes_launcher(".EXE; .CMD ;.BAT"));
        }

        #[test]
        fn returns_false_when_cmd_absent() {
            assert!(!includes_launcher(".EXE;.BAT;.COM"));
        }

        #[test]
        fn returns_false_for_empty_string() {
            assert!(!includes_launcher(""));
        }

        #[test]
        fn returns_false_for_partial_match() {
            // ".cmdextra" must not match ".cmd"
            assert!(!includes_launcher(".cmdextra;.EXE"));
        }

        // ── pathext_with_launcher (Windows only) ────────────────────────────

        #[test]
        fn pathext_returns_none_when_already_includes_cmd() {
            assert_eq!(pathext_with_launcher(".EXE;.CMD;.BAT"), None);
        }

        #[test]
        fn pathext_returns_none_for_lowercase_cmd() {
            assert_eq!(pathext_with_launcher(".exe;.cmd;.bat"), None);
        }

        #[test]
        fn pathext_prepends_cmd_to_existing_pathext() {
            assert_eq!(pathext_with_launcher(".EXE;.BAT").as_deref(), Some(".CMD;.EXE;.BAT"));
        }

        #[test]
        fn pathext_supplies_default_when_empty() {
            assert_eq!(pathext_with_launcher("").as_deref(), Some(".CMD;.EXE;.BAT;.COM"));
        }

        #[test]
        fn pathext_does_not_double_prepend_on_partial_match() {
            // ".cmdextra" is not ".cmd"; injection should still happen.
            assert_eq!(
                pathext_with_launcher(".cmdextra;.EXE").as_deref(),
                Some(".CMD;.cmdextra;.EXE")
            );
        }
    }
}
