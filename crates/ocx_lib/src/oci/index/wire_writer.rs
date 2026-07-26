// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Canonical wire-format serializer for the ocx-index root document.
//!
//! **Byte authority: `ocx-sh/index` `bot/CONTRACTS.md` §14** ("Root serializer —
//! client-facing byte-exact spec"). This module is the Rust port of that repo's
//! `validate_entry.py::serialize_package_root`; its output must match the Python
//! reference byte-for-byte, proven by the `index_wire_conformance` integration
//! test against the vendored golden fixtures
//! (`crates/ocx_lib/tests/fixtures/index_wire/root/`).
//!
//! One document is ever serialized by OCX: the human-diffable
//! `p/<ns>/<pkg>.json` root ([`serialize_root`]). What a tag points at is a
//! registry's own OCI image index, stored byte-for-byte as it was served — the
//! index writes no object shapes of its own, so there is nothing else here to
//! emit (`adr_oci_index_only_dispatch.md` D1).
//!
//! The form is Python `json.dumps(data, indent=2, sort_keys=False,
//! ensure_ascii=True)` plus a single trailing `\n`. Everything structural about
//! that — 2-space indent, `": "` between key and value, inline `{}` / `[]`,
//! insertion order, number spelling — is [`serde_json::ser::PrettyFormatter`],
//! delegated to wholesale. OCX owns exactly one rule the JSON ecosystem does not
//! implement: `ensure_ascii`, escaping every scalar outside printable ASCII
//! (serde-rs/json#907 declined it). That one rule is [`PythonJson`]'s two escape
//! methods; nothing else here is hand-rolled.

use std::io;

use serde::Serialize;
use serde_json::ser::{CharEscape, Formatter, PrettyFormatter};

/// Byte-exact package-root serialization (CONTRACTS §14).
///
/// The input is an **order-preserving** [`serde_json::Value`] — parse the committed
/// root with `serde_json` compiled with `preserve_order` (enabled crate-wide in
/// `Cargo.toml`) so object fields stay in on-disk order. Announce mutates only the
/// `tags` field; every human-governed field (`name`, `owners`, `desc`, `upstream`,
/// …) rides through the `Value` verbatim, so nothing this writer does not touch can
/// drift.
///
/// Output: 2-space indent, insertion-order fields, `\uXXXX` for every non-ASCII
/// scalar, a single trailing `\n`. Empty `{}` / `[]` emit inline.
pub fn serialize_root(root: &serde_json::Value) -> Vec<u8> {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PythonJson::new());
    root.serialize(&mut serializer)
        .expect("serializing a Value into a Vec cannot fail");
    out.push(b'\n');
    out
}

/// `PrettyFormatter`'s layout with Python's `ensure_ascii=True` escape policy
/// owned here, not inherited.
struct PythonJson<'a> {
    layout: PrettyFormatter<'a>,
}

impl PythonJson<'_> {
    fn new() -> Self {
        Self {
            layout: PrettyFormatter::with_indent(b"  "),
        }
    }
}

/// Forward a [`Formatter`] layout method to the wrapped [`PrettyFormatter`].
///
/// Covers every method `PrettyFormatter` overrides in serde_json 1.0.150, plus
/// `end_object_key` (a no-op default today) so a future override is inherited
/// rather than silently dropped.
macro_rules! delegate_layout {
    ($($name:ident($($arg:ident: $ty:ty),*)),* $(,)?) => {$(
        fn $name<W: ?Sized + io::Write>(&mut self, writer: &mut W $(, $arg: $ty)*) -> io::Result<()> {
            self.layout.$name(writer $(, $arg)*)
        }
    )*};
}

impl Formatter for PythonJson<'_> {
    delegate_layout!(
        begin_array(),
        end_array(),
        begin_array_value(first: bool),
        end_array_value(),
        begin_object(),
        end_object(),
        begin_object_key(first: bool),
        end_object_key(),
        begin_object_value(),
        end_object_value(),
    );

    /// Escape every scalar from `0x7F` up — Python's `json.encoder` keeps only
    /// `[\x20-\x7e]` raw, so **DEL is escaped**, not printable. Astral scalars
    /// become UTF-16 surrogate pairs, as `ensure_ascii` has no other spelling.
    ///
    /// Only unescaped runs reach here: serde_json splits the string on its own
    /// escape table (`"`, `\`, and every C0 control), which is a strict subset of
    /// Python's, so the remainder is exactly the range this method must decide.
    fn write_string_fragment<W: ?Sized + io::Write>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()> {
        // Copy raw runs wholesale; only the escaped scalars interrupt the memcpy.
        let mut run_start = 0;
        for (offset, character) in fragment.char_indices() {
            let code = character as u32;
            if code < 0x7F {
                continue;
            }
            writer.write_all(&fragment.as_bytes()[run_start..offset])?;
            match code.checked_sub(0x1_0000) {
                None => write!(writer, "\\u{code:04x}")?,
                Some(rest) => write!(
                    writer,
                    "\\u{:04x}\\u{:04x}",
                    0xD800 + (rest >> 10),
                    0xDC00 + (rest & 0x3FF)
                )?,
            }
            run_start = offset + character.len_utf8();
        }
        writer.write_all(&fragment.as_bytes()[run_start..])
    }

    /// Python's escape spellings for the scalars serde_json hands off pre-classified.
    ///
    /// `Solidus` is deliberately raw: serde_json reserves the `\/` escape, Python
    /// never emits it. serde_json's own escape table never produces this variant
    /// today — the arm is the guard against that changing under us.
    fn write_char_escape<W: ?Sized + io::Write>(&mut self, writer: &mut W, escape: CharEscape) -> io::Result<()> {
        match escape {
            CharEscape::Quote => writer.write_all(b"\\\""),
            CharEscape::ReverseSolidus => writer.write_all(b"\\\\"),
            CharEscape::Solidus => writer.write_all(b"/"),
            CharEscape::Backspace => writer.write_all(b"\\b"),
            CharEscape::FormFeed => writer.write_all(b"\\f"),
            CharEscape::LineFeed => writer.write_all(b"\\n"),
            CharEscape::CarriageReturn => writer.write_all(b"\\r"),
            CharEscape::Tab => writer.write_all(b"\\t"),
            // Lowercase, zero-padded — Python's `\u00XX` for the remaining C0 controls.
            CharEscape::AsciiControl(byte) => write!(writer, "\\u{byte:04x}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── ensure_ascii truth table (via the public entry point) ────────────────

    /// The JSON string literal `text` serializes to, unwrapped from the root form.
    fn escaped(text: &str) -> String {
        let bytes = serialize_root(&serde_json::Value::String(text.to_string()));
        String::from_utf8(bytes)
            .expect("ensure_ascii output is ASCII")
            .trim_end_matches('\n')
            .to_string()
    }

    #[test]
    fn short_escapes_use_pythons_spellings() {
        assert_eq!(escaped("a\"b"), r#""a\"b""#);
        assert_eq!(escaped("a\\b"), r#""a\\b""#);
        assert_eq!(escaped("a\nb"), r#""a\nb""#);
        assert_eq!(escaped("a\rb"), r#""a\rb""#);
        assert_eq!(escaped("a\tb"), r#""a\tb""#);
        assert_eq!(escaped("a\u{08}b"), r#""a\bb""#);
        assert_eq!(escaped("a\u{0c}b"), r#""a\fb""#);
    }

    #[test]
    fn control_chars_use_lowercase_u00xx() {
        // NUL and unit-separator (0x1F) — the C0 controls without a short escape.
        assert_eq!(escaped("\u{00}"), "\"\\u0000\"");
        assert_eq!(escaped("\u{1f}"), "\"\\u001f\"");
    }

    /// The printable-ASCII boundary, pinned on both sides.
    ///
    /// Python's `json.encoder` escapes `[^\ -~]`, so the raw range ends at `~`
    /// (`0x7E`) and DEL (`0x7F`) **is** escaped. An earlier reading of that
    /// bound as `> 0x7F` emitted DEL raw and diverged from the Python reference
    /// on exactly one code point — invisible to the golden corpus, which
    /// contains no `0x7F` byte. Verified against CPython:
    /// `json.dumps("\u{7f}", ensure_ascii=True) == '"\\u007f"'`.
    #[test]
    fn printable_boundary_is_tilde() {
        assert_eq!(escaped("~"), r#""~""#, "0x7E is the last raw scalar");
        assert_eq!(escaped("\u{7f}"), "\"\\u007f\"", "DEL is escaped, not raw");
    }

    #[test]
    fn slash_is_raw() {
        // Python has no solidus escape; `serde_json` reserves one (`CharEscape::Solidus`,
        // spelled `\/`), which is why this writer owns its own escape spelling.
        assert_eq!(escaped("a/b"), r#""a/b""#);
    }

    #[test]
    fn non_ascii_bmp_becomes_uxxxx() {
        // U+00E9 (é) — the CONTRACTS §14 worked example. The input carries the
        // real char; the output must be the 6 ASCII bytes of the escape.
        assert_eq!(escaped("caf\u{e9}"), "\"caf\\u00e9\"");
        // Non-Latin BMP scalar U+4E2D.
        assert_eq!(escaped("\u{4e2d}"), "\"\\u4e2d\"");
    }

    #[test]
    fn astral_becomes_surrogate_pair() {
        // U+1F600 GRINNING FACE encodes as the surrogate pair D83D DE00.
        assert_eq!(escaped("\u{1f600}"), "\"\\ud83d\\ude00\"");
    }

    #[test]
    fn keys_take_the_same_escape_policy_as_values() {
        let value = serde_json::json!({ "caf\u{e9}\u{7f}": 1 });
        let text = String::from_utf8(serialize_root(&value)).unwrap();
        assert_eq!(text, "{\n  \"caf\\u00e9\\u007f\": 1\n}\n");
    }

    // ── layout edge cases (root form) ────────────────────────────────────────

    #[test]
    fn root_emits_empty_containers_inline() {
        let value = serde_json::json!({ "tags": {}, "list": [] });
        let bytes = serialize_root(&value);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "{\n  \"tags\": {},\n  \"list\": []\n}\n");
    }

    #[test]
    fn root_preserves_insertion_order_and_appends_single_newline() {
        // `preserve_order` keeps `zebra` before `apple` despite the alphabet.
        let value: serde_json::Value = serde_json::from_str(r#"{"zebra":1,"apple":2}"#).unwrap();
        let bytes = serialize_root(&value);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "{\n  \"zebra\": 1,\n  \"apple\": 2\n}\n");
        assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
    }
}
