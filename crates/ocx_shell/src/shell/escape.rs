// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-shell string escapers, one owner for every emit site.
//!
//! Each name states the quoting context, not just the shell: the wrong fish escaper is a shell injection.

/// Escape `value` for a POSIX `sh` **single-quoted** literal (`'` → `'\''`).
///
/// The move-to-front emit needs this form: double quotes turn `!` into `\!`, so the value misses its
/// `PATH` segment and duplicates.
#[must_use]
pub fn posix_single_quoted(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Escape `value` for a fish **single-quoted** literal, where only `\` and `'` are escapes.
#[must_use]
pub fn fish_single_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Escape `value` for a fish **double-quoted** string, where only `\`, `$` and `"` are metacharacters.
///
/// Backtick stays unescaped: fish has no `` \` `` escape, so escaping it emits a literal backslash.
#[must_use]
pub fn fish_double_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('$', "\\$").replace('"', "\\\"")
}

/// Escape `value` for a PowerShell **single-quoted** literal.
///
/// PowerShell's tokenizer reads U+2018..U+201B as single-quote delimiters too (Windows PowerShell 5.1 and
/// pwsh 7 alike), so a value carrying one closes the literal and the rest runs as code. Each is doubled
/// with itself, which PowerShell reads back as that one character; so is the ASCII `'`.
///
/// Keep emits single-quoted: double quotes let a value start a subexpression.
#[must_use]
pub fn powershell_single_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}'..='\u{201B}') {
            out.push(c);
        }
    }
    out
}

/// Escape `value` for an elvish **single-quoted** raw string (`'` → `''`).
///
/// Elvish treats U+2018..U+201B as ordinary text, so doubling them would corrupt the value: never swap in
/// the PowerShell escaper. Double quotes are no option, elvish rejects `\$` there.
#[must_use]
pub fn elvish_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

/// Escape `value` for a **plain, non-interpolating** nushell double-quoted string.
///
/// Never reuse it for an interpolating `$"..."` emit: `$` and `(` stay unescaped here.
#[must_use]
pub fn nushell_plain_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape `value` for a `cmd.exe` `SET "KEY=…"` statement.
///
/// `%` only: cmd keeps carets inside the quoted `SET` form, so caret-escaping `^&<>|` corrupts the value.
/// `batch_cannot_express` refuses `%` values upstream; the doubling is defence in depth.
#[must_use]
pub fn batch_set_value(value: &str) -> String {
    value.replace('%', "%%")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two fish escapers are for different quoting contexts and must never
    /// be swapped. The one byte that proves it is `'`: safe (inert) inside `"…"`,
    /// quote-closing inside `'…'`.
    #[test]
    fn the_two_fish_escapers_are_not_interchangeable() {
        assert_eq!(fish_single_quoted("it's"), "it\\'s");
        assert_eq!(fish_double_quoted("it's"), "it's");
        assert_eq!(fish_single_quoted("a$b"), "a$b");
        assert_eq!(fish_double_quoted("a$b"), "a\\$b");
        // Both escape the backslash, and both escape it FIRST — otherwise the
        // backslash they introduce for the quote would itself be doubled.
        assert_eq!(fish_single_quoted("a\\'b"), "a\\\\\\'b");
        assert_eq!(fish_double_quoted("a\\\"b"), "a\\\\\\\"b");
    }

    #[test]
    fn posix_single_quoted_closes_reopens_around_a_quote() {
        assert_eq!(posix_single_quoted("it's"), "it'\\''s");
        // Everything else is literal inside `'…'`, including the bytes a
        // double-quoted literal would have to escape.
        assert_eq!(posix_single_quoted("$HOME`id`\\!*"), "$HOME`id`\\!*");
    }

    #[test]
    fn powershell_single_quoted_doubles_the_ascii_and_typographic_quotes() {
        assert_eq!(powershell_single_quoted("it's"), "it''s");
        for c in ['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'] {
            assert_eq!(powershell_single_quoted(&format!("a{c}b")), format!("a{c}{c}b"));
        }
        assert_eq!(
            powershell_single_quoted("a\\$b`c\u{201C}\u{201C}"),
            "a\\$b`c\u{201C}\u{201C}"
        );
    }

    #[test]
    fn elvish_single_quoted_doubles_only_the_ascii_quote() {
        assert_eq!(elvish_single_quoted("it's"), "it''s");
        for c in ['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'] {
            let s = format!("a{c}b");
            assert_eq!(elvish_single_quoted(&s), s);
        }
        assert_eq!(elvish_single_quoted("a\\$b`c"), "a\\$b`c");
    }

    #[test]
    fn nushell_plain_string_leaves_the_inert_bytes_alone() {
        assert_eq!(nushell_plain_string("a\"b"), "a\\\"b");
        assert_eq!(nushell_plain_string("a\\b"), "a\\\\b");
        assert_eq!(nushell_plain_string("$(id)"), "$(id)");
    }

    #[test]
    fn batch_set_value_doubles_only_the_percent() {
        assert_eq!(batch_set_value("a%b"), "a%%b");
        assert_eq!(batch_set_value("C:\\a^b&c"), "C:\\a^b&c");
    }
}
