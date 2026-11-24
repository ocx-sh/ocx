// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Native Windows path spellings for the MSYS2 and Cygwin runtimes (Git Bash, MSYS2 and Cygwin
//! bash, zsh and fish).
//!
//! Pure string functions: they never parse through `std::path`, so they behave the same on every
//! host and are tested on Linux with Windows-form literals.

use std::borrow::Cow;

/// Drive prefix of MSYS2 and Git Bash: `C:\x` is `/c/x`.
pub(super) const MSYS_DRIVE_PREFIX: &str = "/";

/// Drive prefix of Cygwin: `C:\x` is `/proc/cygdrive/c/x`. `/proc/cygdrive` is the fixed alias of the
/// `/cygdrive` mount, which a user can rename in `fstab`.
pub(super) const CYGWIN_DRIVE_PREFIX: &str = "/proc/cygdrive/";

/// Spell `native` as a command word: `\` becomes `/`, the drive stays (`C:/Users/x/ocx.exe`).
///
/// MSYS2 bash runs a `C:/…` word as a native program; a bare `\` would be eaten by the shell.
#[must_use]
pub(super) fn command_word(native: &str) -> String {
    native.replace('\\', "/")
}

/// Spell `native` as one `PATH` element in POSIX form: `C:\a\b` becomes `<drive_prefix>c/a/b`.
///
/// Handles `/` or `\` separators in any mix, the `\\?\` verbatim prefix and UNC shares
/// (`//srv/share/x`, no drive prefix). A POSIX-form input passes through. A drive-relative `C:foo` keeps its colon
/// so the caller can refuse it: a `:` would split the POSIX `PATH`.
#[must_use]
pub(super) fn list_element<'a>(native: &'a str, drive_prefix: &str) -> Cow<'a, str> {
    if !native.contains('\\') && !has_drive(native) {
        return Cow::Borrowed(native);
    }
    let folded = native.replace('\\', "/");
    if let Some(unc) = folded
        .strip_prefix("//?/UNC/")
        .or_else(|| folded.strip_prefix("//?/unc/"))
    {
        return Cow::Owned(format!("//{unc}"));
    }
    let body = folded.strip_prefix("//?/").unwrap_or(&folded);
    if has_drive(body) {
        let letter = body[..1].to_ascii_lowercase();
        return Cow::Owned(format!("{drive_prefix}{letter}{}", &body[2..]));
    }
    Cow::Owned(body.to_string())
}

/// [`list_element`] as one `PATH` element a POSIX shell can carry, or `None` when MSYS would read it
/// as a different directory than Windows does (or split it).
///
/// Refused: a bare drive `C:` (the drive's current directory), a `C:foo` or later `:` (a colon splits
/// `PATH`), a single leading `\` (rooted on the current drive), a relative result, and, for converted
/// input, a `..` segment (MSYS resolves it textually, Windows clamps at the root) or an element of
/// only slashes. POSIX-absolute input such as `/bin` passes through unchanged: it names a directory under
/// the runtime's root, which is a valid MSYS2 and Cygwin path.
#[must_use]
pub(super) fn path_element(native: &str, drive_prefix: &str) -> Option<String> {
    let verbatim = native.strip_prefix(r"\\?\").unwrap_or(native);
    let bare_drive = verbatim.len() == 2 && has_drive(verbatim);
    let current_drive_rooted = native.starts_with('\\') && !native.starts_with(r"\\");
    if bare_drive || current_drive_rooted {
        return None;
    }
    let converted = list_element(native, drive_prefix).into_owned();
    if converted.contains(':') || !converted.starts_with('/') {
        return None;
    }
    let was_converted = converted != native;
    let unspellable = converted.split('/').any(|segment| segment == "..") || converted.bytes().all(|b| b == b'/');
    (!(was_converted && unspellable)).then_some(converted)
}

/// `element` as a bash or zsh pattern that matches it under ASCII case folding, the way Windows compares
/// `PATH` entries: a letter becomes `[xX]`, a pattern-special character gets a `\`, anything else stays.
///
/// The escape set is exactly the characters special in a bash (extglob) or zsh (extendedglob, kshglob)
/// pattern: zsh keeps a `\` before any other character as a literal backslash, so the pattern would miss.
#[must_use]
pub(super) fn case_folding_pattern(element: &str) -> String {
    let mut pattern = String::with_capacity(element.len() * 2);
    for character in element.chars() {
        if character.is_ascii_alphabetic() {
            pattern.push('[');
            pattern.push(character.to_ascii_lowercase());
            pattern.push(character.to_ascii_uppercase());
            pattern.push(']');
        } else {
            if matches!(
                character,
                '\\' | '*' | '?' | '[' | ']' | '(' | ')' | '|' | '<' | '>' | '^' | '#' | '~' | '!'
            ) {
                pattern.push('\\');
            }
            pattern.push(character);
        }
    }
    pattern
}

/// `X:` followed by the end or a separator.
fn has_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes.get(2), None | Some(b'/' | b'\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msys_list_element_table() {
        let cases = [
            (r"C:\a\b", "/c/a/b"),
            ("C:/a/b", "/c/a/b"),
            (r"C:\a/b\c", "/c/a/b/c"),
            (r"D:\Tools", "/d/Tools"),
            (r"c:\a", "/c/a"),
            (r"C:\", "/c/"),
            ("C:", "/c"),
            (r"\\?\C:\a", "/c/a"),
            (r"\\?\UNC\srv\share\x", "//srv/share/x"),
            (r"\\srv\share\x", "//srv/share/x"),
            ("/c/a", "/c/a"),
            ("/usr/bin", "/usr/bin"),
            // drive-relative: colon survives so the caller refuses it
            ("C:foo", "C:foo"),
        ];
        for (input, expected) in cases {
            assert_eq!(list_element(input, MSYS_DRIVE_PREFIX), expected, "input {input}");
        }
    }

    #[test]
    fn msys_path_element_table() {
        let cases = [
            (r"C:\a\b", Some("/c/a/b")),
            (r"C:\", Some("/c/")),
            ("C:/", Some("/c/")),
            ("/c/x", Some("/c/x")),
            ("/usr/bin", Some("/usr/bin")),
            ("/usr/../bin", Some("/usr/../bin")),
            (r"\\srv\share\x", Some("//srv/share/x")),
            // drive-relative and colon-bearing
            ("C:", None),
            (r"\\?\C:", None),
            ("C:foo", None),
            (r"C:\a:stream", None),
            // rooted on the current drive, or relative
            (r"\bin", None),
            (r"\bin\x", None),
            ("rel/dir", None),
            // textual `..` and slash-only results differ from Windows
            (r"C:\a\..\..\Windows", None),
            (r"\\?\UNC\", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                path_element(input, MSYS_DRIVE_PREFIX).as_deref(),
                expected,
                "input {input}"
            );
        }
    }

    #[test]
    fn cygwin_list_element_table() {
        let cases = [
            (r"C:\a\b", "/proc/cygdrive/c/a/b"),
            ("C:/a/b", "/proc/cygdrive/c/a/b"),
            (r"D:\Tools", "/proc/cygdrive/d/Tools"),
            (r"C:\", "/proc/cygdrive/c/"),
            ("C:", "/proc/cygdrive/c"),
            (r"\\?\C:\a", "/proc/cygdrive/c/a"),
            // a share is not a drive: no prefix
            (r"\\?\UNC\srv\share\x", "//srv/share/x"),
            (r"\\srv\share\x", "//srv/share/x"),
            ("/proc/cygdrive/c/a", "/proc/cygdrive/c/a"),
            ("/usr/bin", "/usr/bin"),
            ("C:foo", "C:foo"),
        ];
        for (input, expected) in cases {
            assert_eq!(list_element(input, CYGWIN_DRIVE_PREFIX), expected, "input {input}");
        }
    }

    #[test]
    fn cygwin_path_element_table() {
        let cases = [
            (r"C:\a\b", Some("/proc/cygdrive/c/a/b")),
            (r"C:\", Some("/proc/cygdrive/c/")),
            ("/usr/bin", Some("/usr/bin")),
            (r"\\srv\share\x", Some("//srv/share/x")),
            ("C:", None),
            ("C:foo", None),
            (r"C:\a:stream", None),
            (r"\bin", None),
            ("rel/dir", None),
            (r"C:\a\..\..\Windows", None),
            (r"\\?\UNC\", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                path_element(input, CYGWIN_DRIVE_PREFIX).as_deref(),
                expected,
                "input {input}"
            );
        }
    }

    /// A driveless POSIX-absolute entry is a valid MSYS2 and Cygwin path: it resolves under the
    /// runtime's own root, so it is accepted unchanged rather than refused.
    #[test]
    fn a_driveless_posix_absolute_entry_is_accepted_unchanged() {
        for drive_prefix in [MSYS_DRIVE_PREFIX, CYGWIN_DRIVE_PREFIX] {
            for entry in ["/bin", "/usr/local/bin", "/opt/x y"] {
                assert_eq!(
                    path_element(entry, drive_prefix).as_deref(),
                    Some(entry),
                    "{drive_prefix} {entry}"
                );
            }
        }
    }

    #[test]
    fn case_folding_pattern_table() {
        let cases = [
            ("/c/Users/x", "/[cC]/[uU][sS][eE][rR][sS]/[xX]"),
            ("/c/a b/1", "/[cC]/[aA] [bB]/1"),
            (r"/x/[1]*?\(a|b)", r"/[xX]/\[1\]\*\?\\\([aA]\|[bB]\)"),
            ("/x/<^#~!>", r"/[xX]/\<\^\#\~\!\>"),
            ("/x/@+{},.:=-'$`\"", "/[xX]/@+{},.:=-'$`\""),
            ("/c/é", "/[cC]/é"),
        ];
        for (input, expected) in cases {
            assert_eq!(case_folding_pattern(input), expected, "input {input}");
        }
    }

    #[test]
    fn msys_command_word_table() {
        let cases = [
            (r"C:\Users\x\ocx.exe", "C:/Users/x/ocx.exe"),
            ("C:/Users/x/ocx.exe", "C:/Users/x/ocx.exe"),
            (r"C:\a b\ocx.exe", "C:/a b/ocx.exe"),
            ("/usr/bin/ocx", "/usr/bin/ocx"),
        ];
        for (input, expected) in cases {
            assert_eq!(command_word(input), expected, "input {input}");
        }
    }
}
