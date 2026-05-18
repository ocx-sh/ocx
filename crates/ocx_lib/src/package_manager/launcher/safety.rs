// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher-safe string newtype for characters that cannot appear in generated launchers.
//!
//! The Unix `.sh` launcher and the Windows `.shim` sidecar are generated on
//! every platform (cross-platform packages), so the unsafe-character set is
//! unified rather than per-platform. Lives alongside [`super::generate`] (the launcher generator)
//! because the unsafe-character set encodes a *consumer-layer* shell template
//! constraint — not a metadata invariant. The publish-time validator
//! ([`crate::package::metadata::validation`]) still calls into this module via
//! the crate-visible re-export to surface unsafe characters at publish time as
//! defense-in-depth.

/// Characters that cannot appear in any string baked into a generated launcher.
///
/// The set is unified across the `.sh` launcher body and the `.shim` sidecar:
/// - `'` breaks Unix single-quoted shell literals (cannot be escaped inside one).
/// - `\n`, `\r`, `\0` would inject newlines/control bytes into the `.sh` body
///   or break the frozen one-line `.shim` sidecar — a code-injection vector.
/// - `"` is retained because it must be quoted/escaped for `CreateProcessW`
///   command-line assembly and for the `.sh` body's quoting.
///
/// `%` is **not** unsafe: post-cutover no consumer treats it specially — the
/// `.sh` body single-quotes the package-root literal, the `.shim` sidecar
/// carries it as a verbatim one-line value, and the native shim spawns via
/// `CreateProcessW` with no `cmd.exe` (so no `%VAR%` expansion). A Windows
/// install path containing `%` (e.g. a user folder `100%real`) must succeed.
///
/// All generated bodies exist on every platform (cross-platform packages),
/// so the character set is unified rather than per-platform.
///
/// **Reused by the `.shim` sidecar.** The Windows `.shim` sidecar written by
/// [`super::body::shim_sidecar_body`] carries the `pkg_root` as its sole content.
/// Because the `pkg_root` is already wrapped in a [`LauncherSafeString`] at the
/// `generate()` entry boundary, the sidecar body function receives a
/// pre-validated value and performs no second validation — this set is the
/// single enforcement point for both the `.sh` launcher body and the `.shim`
/// sidecar. See `adr_windows_exe_shim.md` §`.shim` Sidecar Format Contract.
///
/// **Why backslash (`\`) is intentionally NOT in the set.** Windows package
/// roots normally contain `\` (e.g. `C:\Users\…\.ocx\packages\…`), and the
/// native `.exe` shim reads that path verbatim from the single-line `.shim`
/// sidecar. `\` is an ordinary path byte there — it cannot terminate the
/// line or change parsing. Forbidding `\` would block every realistic
/// Windows install path; allowing it introduces no injection surface.
const LAUNCHER_UNSAFE_CHARS: &[char] = &['\'', '"', '\n', '\r', '\0'];

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
        // Amendment §2: `%` REMOVED from the unsafe set (post-cutover no
        // consumer treats it specially). The retained set is `' " \n \r \0`.
        for unsafe_char in ['\'', '"', '\n', '\r', '\0'] {
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

    /// Amendment §2: `%` is admissible in package roots. Post-cutover the
    /// `.sh` body single-quotes it, the `.shim` sidecar carries it as a
    /// verbatim line, and `CreateProcessW` does no cmd.exe expansion — no
    /// consumer treats `%` specially. A Windows install path containing `%`
    /// (e.g. a user folder `100%real`) must construct successfully. RED
    /// against current code: `%` is still in `LAUNCHER_UNSAFE_CHARS`.
    #[test]
    fn launcher_safe_string_accepts_percent_in_windows_package_root() {
        let p = "C:\\Users\\100%real\\.ocx\\packages\\x";
        let s = super::LauncherSafeString::new(p)
            .expect("`%` must be admissible in package roots (amendment §2 removed it from the unsafe set)");
        assert_eq!(
            s.as_str(),
            p,
            "value must round-trip verbatim — no normalization, `%` preserved"
        );
    }

    /// `%` is now accepted while the genuinely dangerous bytes stay rejected:
    /// the amendment narrowed the set to `' " \n \r \0` only.
    #[test]
    fn launcher_safe_string_accepts_percent_but_still_rejects_quote_and_control_bytes() {
        assert!(
            super::LauncherSafeString::new("path/with%percent").is_ok(),
            "`%` is admissible post-cutover (amendment §2)"
        );
        for still_unsafe in ['\'', '"', '\n', '\r', '\0'] {
            assert!(
                super::LauncherSafeString::new(format!("path{still_unsafe}x")).is_err(),
                "{still_unsafe:?} must still be rejected (retained unsafe set)"
            );
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
