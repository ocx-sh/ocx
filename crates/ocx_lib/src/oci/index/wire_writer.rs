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
//! The form is **not** a generic pretty-printer (`serde_json::to_string_pretty`
//! byte-diverges on the first non-ASCII value — it is banned here by design):
//! Python `json.dumps(data, indent=2, sort_keys=False, ensure_ascii=True)` plus
//! a single trailing `\n`, i.e. 2-space indent, fields in **insertion order**
//! (never alphabetized), `\uXXXX` escapes for every non-ASCII scalar via
//! [`escape_ascii`].

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
    let mut out = String::new();
    write_value(root, 0, &mut out);
    out.push('\n');
    out.into_bytes()
}

/// Recursively emit a [`serde_json::Value`] in the root form.
fn write_value(value: &serde_json::Value, depth: usize, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        // A `serde_json::Number`'s `Display` is the canonical JSON form and matches
        // Python's `repr` for the integer values this grammar carries (ids,
        // timestamps and digests are strings; the wire format has no floats).
        serde_json::Value::Number(number) => out.push_str(&number.to_string()),
        serde_json::Value::String(text) => escape_ascii(text, out),
        serde_json::Value::Array(items) => write_array(items, depth, out),
        serde_json::Value::Object(map) => write_object(map, depth, out),
    }
}

fn write_array(items: &[serde_json::Value], depth: usize, out: &mut String) {
    if items.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[\n");
    let inner = indent(depth + 1);
    for (position, item) in items.iter().enumerate() {
        if position > 0 {
            out.push_str(",\n");
        }
        out.push_str(&inner);
        write_value(item, depth + 1, out);
    }
    out.push('\n');
    out.push_str(&indent(depth));
    out.push(']');
}

fn write_object(map: &serde_json::Map<String, serde_json::Value>, depth: usize, out: &mut String) {
    if map.is_empty() {
        out.push_str("{}");
        return;
    }
    // Insertion order — the `preserve_order` `Map` iterates in on-disk order.
    out.push_str("{\n");
    let inner = indent(depth + 1);
    for (position, (key, value)) in map.iter().enumerate() {
        if position > 0 {
            out.push_str(",\n");
        }
        out.push_str(&inner);
        escape_ascii(key, out);
        out.push_str(": ");
        write_value(value, depth + 1, out);
    }
    out.push('\n');
    out.push_str(&indent(depth));
    out.push('}');
}

/// Two spaces per level — the fixed indent width of the root form.
fn indent(depth: usize) -> String {
    "  ".repeat(depth)
}

/// Emit a JSON string literal reproducing Python's `json.dumps(ensure_ascii=True)`
/// escaping exactly: the seven short escapes (`\"` `\\` `\n` `\r` `\t` `\b` `\f`),
/// `\u00XX` for the remaining C0 control chars, and `\uXXXX` (lowercase hex,
/// surrogate-paired above the BMP) for every scalar with a code point `> 0x7F`.
///
/// `0x7F` (DEL) is emitted raw — Python escapes only `code < 0x20 || code > 0x7F`,
/// and `/` is never escaped.
fn escape_ascii(text: &str, out: &mut String) {
    use std::fmt::Write as _;

    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if (control as u32) < 0x20 => {
                // Remaining C0 controls: `\u00XX`, lowercase, zero-padded.
                let _ = write!(out, "\\u{:04x}", control as u32);
            }
            ascii if (ascii as u32) <= 0x7f => out.push(ascii),
            non_ascii => {
                let code = non_ascii as u32;
                if code <= 0xFFFF {
                    let _ = write!(out, "\\u{code:04x}");
                } else {
                    // Astral plane: encode as a UTF-16 surrogate pair.
                    let offset = code - 0x1_0000;
                    let high = 0xD800 + (offset >> 10);
                    let low = 0xDC00 + (offset & 0x3FF);
                    let _ = write!(out, "\\u{high:04x}\\u{low:04x}");
                }
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── escape_ascii truth table ─────────────────────────────────────────────

    fn escaped(text: &str) -> String {
        let mut out = String::new();
        escape_ascii(text, &mut out);
        out
    }

    #[test]
    fn escape_ascii_short_escapes() {
        assert_eq!(escaped("a\"b"), r#""a\"b""#);
        assert_eq!(escaped("a\\b"), r#""a\\b""#);
        assert_eq!(escaped("a\nb"), r#""a\nb""#);
        assert_eq!(escaped("a\rb"), r#""a\rb""#);
        assert_eq!(escaped("a\tb"), r#""a\tb""#);
        assert_eq!(escaped("a\u{08}b"), r#""a\bb""#);
        assert_eq!(escaped("a\u{0c}b"), r#""a\fb""#);
    }

    #[test]
    fn escape_ascii_control_chars_use_lowercase_u00xx() {
        // NUL and unit-separator (0x1F) — the C0 controls without a short escape.
        assert_eq!(escaped("\u{00}"), "\"\\u0000\"");
        assert_eq!(escaped("\u{1f}"), "\"\\u001f\"");
    }

    #[test]
    fn escape_ascii_del_and_slash_are_raw() {
        // Python escapes only `< 0x20 || > 0x7F`: DEL (0x7F) and `/` stay raw.
        assert_eq!(escaped("\u{7f}"), "\"\u{7f}\"");
        assert_eq!(escaped("a/b"), r#""a/b""#);
    }

    #[test]
    fn escape_ascii_non_ascii_bmp_becomes_uxxxx() {
        // U+00E9 (é) — the CONTRACTS §14 worked example. The input carries the
        // real char; the output must be the 6 ASCII bytes of the escape.
        assert_eq!(escaped("caf\u{e9}"), "\"caf\\u00e9\"");
        // Non-Latin BMP scalar U+4E2D.
        assert_eq!(escaped("\u{4e2d}"), "\"\\u4e2d\"");
    }

    #[test]
    fn escape_ascii_astral_becomes_surrogate_pair() {
        // U+1F600 GRINNING FACE encodes as the surrogate pair D83D DE00.
        assert_eq!(escaped("\u{1f600}"), "\"\\ud83d\\ude00\"");
    }

    // ── empty-container edge cases (root form) ───────────────────────────────

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
