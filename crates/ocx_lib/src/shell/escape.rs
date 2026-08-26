// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-shell string escapers, one owner for the whole crate.
//!
//! Every emit site — [`super::Shell`]'s env statements, `shell/hook.rs`'s
//! registration and wrapper bodies, and `ocx self activate`'s eval lines — needs
//! the same handful of quoting rules. They used to live in three copies with a
//! comment asserting the copies were identical; they were not. `shell.rs` never
//! had a fish **single**-quote escaper at all — its fish escaper is
//! [`fish_double_quoted`], a different function for a different quoting context —
//! so [`fish_single_quoted`] had no owner and no test.
//!
//! **A name here states the quoting context, never just the shell.** `fish` has
//! two escapers with incompatible bodies: inside `'…'` fish recognises `\\` and
//! `\'` and nothing else, while inside `"…"` it recognises `\\`, `\$` and `\"`.
//! Routing a value through the wrong one is a shell injection, not a cosmetic
//! defect, and two functions both called "the fish escaper" is exactly how that
//! ships.

/// Escape `value` for a POSIX `sh` **single-quoted** literal. A literal quote is
/// written by closing the quote, emitting an escaped quote, and reopening
/// (`'` → `'\''`); every other byte — `$`, backtick, `\`, `!`, glob chars,
/// spaces — is literal inside `'...'`.
///
/// Used by the bash/zsh and ash/ksh/dash move-to-front emit so the value matches
/// its existing PATH segment byte for byte (the double-quoted form would turn
/// `!` into `\!` and break the comparison, leaving a stale duplicate) and cannot
/// trigger interpolation, history expansion, or globbing.
#[must_use]
pub fn posix_single_quoted(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Escape `value` for a fish **single-quoted** literal, where `\` and `'` are
/// the only two escapes fish recognises inside `'…'`. Everything else —
/// `$`, `"`, backtick, `(`, glob chars — is literal there.
///
/// This is *not* interchangeable with [`fish_double_quoted`]: that one escapes
/// `$` and `"` and leaves `'` untouched, so feeding its output into a
/// single-quoted literal lets a value close the quote and have its remainder
/// parsed as fish source.
#[must_use]
pub fn fish_single_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Escape `value` for a fish **double-quoted** string, where `\`, `$` and `"`
/// are the only metacharacters. Backtick carries no meaning there and fish does
/// not recognise `` \` `` as an escape sequence, so escaping it would emit a
/// literal backslash.
///
/// `'` is deliberately left alone — it is inert inside `"…"`. That is what makes
/// this escaper unsafe for a single-quoted context; see [`fish_single_quoted`].
#[must_use]
pub fn fish_double_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('$', "\\$").replace('"', "\\\"")
}

/// Escape `value` for a single-quoted literal in shells where a quote is written
/// by **doubling** it (`'` → `''`): PowerShell and elvish.
///
/// Inside `'...'` neither shell interpolates, so `$`, backtick, `\`, `;`, and
/// glob metacharacters are all literal and only the quote needs escaping. This is
/// the strongest injection guard for the move-to-front emit — the value can never
/// start a subexpression — and (for elvish) it also avoids the double-quote arm's
/// `\$` / `` \` `` *invalid-escape* parse errors.
#[must_use]
pub fn single_quoted_doubled(value: &str) -> String {
    value.replace('\'', "''")
}

/// Escape `value` for a **plain, non-interpolating** nushell double-quoted
/// string — the form all four nushell emits use.
///
/// `$`, `(` and `)` are inert there, so escaping them would be corrupting rather
/// than hardening unless nushell recognised `\$` / `\(` / `\)` as escapes in the
/// plain form. Emitting no backslash is correct whether it does or not.
///
/// A nushell emit that ever adopts the interpolating `$"..."` form gets its own
/// escaper; this one is never reused there.
#[must_use]
pub fn nushell_plain_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape `value` for a `cmd.exe` `SET "KEY=…"` statement.
///
/// `%` only. The caret escapes `^`, `&`, `<`, `>` and `|` used to carry are
/// over-escaping: cmd does not process those characters inside the quoted `SET`
/// form, so the carets survived into the value and corrupted it. A `%`-bearing
/// value is refused upstream by `batch_cannot_express`, because `%%` means `%`
/// when the line is read from a `.bat` and `%%` when it is evaluated through
/// `FOR /F`; the doubling here is defence in depth for a future caller that does
/// not refuse.
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
    fn single_quoted_doubled_only_doubles_the_quote() {
        assert_eq!(single_quoted_doubled("it's"), "it''s");
        assert_eq!(single_quoted_doubled("a\\$b`c"), "a\\$b`c");
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
