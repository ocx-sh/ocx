// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher-safe string newtype for characters that cannot appear in generated launchers.
//!
//! Both Unix `.sh` and Windows `.cmd` launchers are generated on every platform
//! (cross-platform packages), so the unsafe-character set is unified rather than
//! per-platform. Lives alongside [`super::generate`] (the launcher generator)
//! because the unsafe-character set encodes a *consumer-layer* shell template
//! constraint — not a metadata invariant. The publish-time validator
//! ([`crate::package::metadata::validation`]) still calls into this module via
//! the crate-visible re-export to surface unsafe characters at publish time as
//! defense-in-depth.

/// Characters that cannot appear in any string baked into a generated launcher.
///
/// The set is the union of constraints from both supported platforms:
/// - `'` breaks Unix single-quoted shell literals (cannot be escaped inside one).
/// - `%` triggers `cmd.exe` variable expansion (`%X%`) inside the `.cmd` body.
/// - `"` would close the `SET "var=value"` statement on Windows.
/// - `\n`, `\r`, `\0` would inject newlines/control bytes into either script
///   body — a code-injection vector.
///
/// Both launchers are generated on every platform (cross-platform packages),
/// so the character set is unified rather than per-platform.
///
/// **Why backslash (`\`) is intentionally NOT in the set.** Windows package
/// roots normally contain `\` (e.g. `C:\Users\…\.ocx\packages\…`), and the
/// `.cmd` template embeds the package root inside a double-quoted path
/// on the `ocx launcher exec` line. Inside `cmd.exe` double quotes, `\` is a
/// literal byte — not an escape character — so a `\`-bearing path cannot
/// terminate the string or change the parse. Forbidding `\` would block
/// every realistic Windows install path; allowing it does not introduce a
/// new injection surface beyond the residual `%*` argv risk documented in
/// `.claude/artifacts/adr_windows_cmd_argv_injection.md`.
const LAUNCHER_UNSAFE_CHARS: &[char] = &['\'', '%', '"', '\n', '\r', '\0'];

/// A `String` proven free of characters that would corrupt a generated launcher.
///
/// Construction via [`LauncherSafeString::new`] is the only way to obtain one;
/// the launcher body functions accept `&LauncherSafeString` so the unsafe-char
/// check happens once, at the entry boundary, not per platform.
#[derive(Debug, Clone)]
pub(crate) struct LauncherSafeString(String);

impl LauncherSafeString {
    /// Validates `value` against [`LAUNCHER_UNSAFE_CHARS`] and wraps it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::LauncherUnsafeCharacter`] if the input contains
    /// any of the unsafe characters listed above.
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, crate::Error> {
        let value = value.into();
        if let Some(c) = value.chars().find(|c| LAUNCHER_UNSAFE_CHARS.contains(c)) {
            return Err(crate::Error::LauncherUnsafeCharacter { value, character: c });
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    // ── LauncherSafeString — unified character rejection ──────────────────

    #[test]
    fn launcher_safe_string_rejects_all_unsafe_characters() {
        for unsafe_char in ['\'', '%', '"', '\n', '\r', '\0'] {
            let input = format!("prefix{unsafe_char}suffix");
            let err = super::LauncherSafeString::new(input.clone())
                .expect_err(&format!("must reject {unsafe_char:?} (input {input:?})"));
            match err {
                crate::Error::LauncherUnsafeCharacter { character, value } => {
                    assert_eq!(character, unsafe_char, "must report the offending char");
                    assert_eq!(value, input, "must echo the rejected value");
                }
                other => panic!("expected LauncherUnsafeCharacter, got {other:?}"),
            }
        }
    }

    #[test]
    fn launcher_safe_string_rejects_double_quote_even_though_unix_tolerates_it() {
        // Both launchers are generated on every platform, so the constraint is
        // unified: `"` would break the Windows `SET "var=value"` form even on
        // a Linux install where only the `.sh` launcher is used.
        assert!(super::LauncherSafeString::new("/path/with\"dq").is_err());
    }

    #[test]
    fn launcher_safe_string_accepts_path_with_spaces() {
        let s = super::LauncherSafeString::new("/path with spaces/bin/tool").unwrap();
        assert_eq!(s.as_str(), "/path with spaces/bin/tool");
    }

    /// S3: cross-platform-publisher acceptance for the canonical Windows
    /// install-path shape — `C:\Program Files\…` with both spaces and
    /// backslashes. Backslash is intentionally NOT in the unsafe set (see
    /// `LAUNCHER_UNSAFE_CHARS` doc comment); this test pins the contract so
    /// a future tightening of the unsafe set cannot silently break Windows
    /// publishers.
    #[test]
    fn launcher_safe_string_accepts_windows_path_with_spaces_and_backslashes() {
        let p = "C:\\Program Files\\App\\bin\\tool.exe";
        let s = super::LauncherSafeString::new(p).expect("Windows path with spaces + backslashes must be accepted");
        assert_eq!(
            s.as_str(),
            p,
            "value must round-trip verbatim — no normalization applied"
        );
    }
}
