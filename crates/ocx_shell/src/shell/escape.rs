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

/// Render `value` as a complete PowerShell expression that evaluates to exactly `value`.
///
/// The output is pure ASCII: Windows PowerShell 5.1 reads a script without a BOM in the ANSI code page,
/// so a literal non-ASCII character is garbled. Every char at or above U+0080 becomes `[char]0xNNNN`
/// (astral chars as their two UTF-16 surrogate halves; 5.1 has no `` `u{} `` escape). That also covers
/// U+2018..U+201B, which PowerShell reads as quote delimiters, so only the ASCII `'` needs doubling.
///
/// All-ASCII input stays a plain `'...'` literal. Otherwise the result is a parenthesised `+` chain that
/// starts with a string, so the chars concatenate as text. Callers emit the result unquoted.
#[must_use]
pub fn powershell_literal(value: &str) -> String {
    fn push_run(parts: &mut Vec<String>, run: &mut String) {
        if !run.is_empty() {
            parts.push(format!("'{}'", run.replace('\'', "''")));
            run.clear();
        }
    }

    if value.is_ascii() {
        return format!("'{}'", value.replace('\'', "''"));
    }
    let mut parts: Vec<String> = Vec::new();
    let mut run = String::new();
    for c in value.chars() {
        if c.is_ascii() {
            run.push(c);
            continue;
        }
        push_run(&mut parts, &mut run);
        let mut units = [0u16; 2];
        for unit in c.encode_utf16(&mut units) {
            parts.push(format!("[char]0x{unit:04X}"));
        }
    }
    push_run(&mut parts, &mut run);
    if !parts[0].starts_with('\'') {
        parts.insert(0, "''".to_owned());
    }
    format!("({})", parts.join("+"))
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
    fn powershell_literal_goldens() {
        assert_eq!(powershell_literal(""), "''");
        assert_eq!(powershell_literal("plain"), "'plain'");
        assert_eq!(powershell_literal("it's"), "'it''s'");
        assert_eq!(powershell_literal("'"), "''''");
        assert_eq!(powershell_literal("a\\$b`c\"d"), "'a\\$b`c\"d'");
        assert_eq!(powershell_literal("\u{00E9}"), "(''+[char]0x00E9)");
        assert_eq!(powershell_literal("\u{4E2D}\u{6587}"), "(''+[char]0x4E2D+[char]0x6587)");
        assert_eq!(powershell_literal("\u{2019}"), "(''+[char]0x2019)");
        assert_eq!(powershell_literal("\u{2019}'"), "(''+[char]0x2019+'''')");
        assert_eq!(powershell_literal("x\u{1F600}"), "('x'+[char]0xD83D+[char]0xDE00)");
        assert_eq!(powershell_literal("a\u{0451}b"), "('a'+[char]0x0451+'b')");
        assert_eq!(
            powershell_literal("a\u{0451}\u{0451}b'c"),
            "('a'+[char]0x0451+[char]0x0451+'b''c')"
        );
    }

    #[test]
    fn powershell_literal_is_always_ascii() {
        for v in [
            "\u{4E2D}\u{6587}",
            "\u{2018}\u{201B}x",
            "\u{1F600}'",
            "C:\\\u{00FC}\\bin",
        ] {
            assert!(powershell_literal(v).is_ascii(), "{v}");
        }
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
