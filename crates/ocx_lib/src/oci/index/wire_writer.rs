// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Canonical wire-format serializer for the ocx-index static-file format.
//!
//! **Byte authority: `ocx-sh/index` `bot/CONTRACTS.md` §14** ("Root serializer —
//! client-facing byte-exact spec"). This module is the Rust port of that repo's
//! `validate_entry.py::serialize_package_root` / `serialize_observation_object`;
//! its output must match the Python reference byte-for-byte, proven by the
//! `serializer_parity` integration test against the vendored golden fixtures
//! (`crates/ocx_lib/tests/fixtures/index_wire/`).
//!
//! There are two distinct forms, and **neither is a generic pretty-printer**
//! (`serde_json::to_string_pretty` byte-diverges on the first non-ASCII value and
//! alphabetizes nothing it should — it is banned here by design):
//!
//! - **Root** ([`serialize_root`]) — the human-diffable `p/<ns>/<pkg>.json`.
//!   Python `json.dumps(data, indent=2, sort_keys=False, ensure_ascii=True)` plus
//!   a single trailing `\n`: 2-space indent, fields in **insertion order** (never
//!   alphabetized), `\uXXXX` escapes for every non-ASCII scalar.
//! - **Observation** ([`serialize_observation`]) — the content-addressed
//!   `o/sha256/<hex>.json` CAS object, digested over its exact bytes. Python
//!   `json.dumps(data, separators=(",",":"), sort_keys=True, ensure_ascii=True)`:
//!   minified, keys **alphabetized recursively**, `\uXXXX` escapes, **no** trailing
//!   newline. `platforms[]` is sorted by the §1 tuple key before emit so registry
//!   manifest-list order never leaks into the digest.
//!
//! Both forms share one ASCII string escaper ([`escape_ascii`]) that reproduces
//! Python's `ensure_ascii=True` output exactly, and one recursive
//! [`serde_json::Value`] emitter parameterized by [`Style`].

use super::wire;
use crate::oci::native;

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
    write_value(root, Style::Pretty, 0, &mut out);
    out.push('\n');
    out.into_bytes()
}

/// Byte-exact content-addressed observation object (CONTRACTS §14 / §1).
///
/// `platforms` is first sorted by [`platform_sort_key`] (the §1 tuple), then the
/// typed observation is bridged through [`serde_json::to_value`] to reuse serde's
/// exact field-name mapping (the `os.features` wire key on [`native::Platform`]),
/// then emitted minified with recursively-alphabetized keys, `\uXXXX` escapes, and
/// **no** trailing newline — the bytes whose SHA-256 is the CAS filename.
pub fn serialize_observation(obs: &wire::Observation) -> Vec<u8> {
    let mut platforms = obs.platforms.clone();
    // Cache the (heap-allocating) sort key per element rather than recomputing it
    // on every comparison.
    platforms.sort_by_cached_key(|entry| platform_sort_key(&entry.platform));
    let sorted = wire::Observation { platforms };

    // `Observation` is a plain data aggregate — its leaves are strings, the
    // `Arch`/`Os` display-string enums, `Option`s, and `oci::Digest` (which
    // serializes to a `"algo:hex"` string). It carries no floats and no non-string
    // map keys, so serialization into an in-memory `Value` cannot fail:
    // `serde_json::to_value` only errors on NaN/Inf floats, non-string map keys, or
    // a `Serialize` impl that returns `Err`, none of which this type can produce.
    let value = serde_json::to_value(&sorted).expect("Observation serializes to a JSON value infallibly");

    let mut out = String::new();
    write_value(&value, Style::Minified, 0, &mut out);
    out.into_bytes()
}

/// The one canonical platform ordering key (CONTRACTS §1) — the tuple
/// `(architecture, os, os_version or "", variant or "", os_features, features)`.
///
/// `os_features` / `features` are compared as element-by-element string sequences,
/// **never** `","`-joined first: joining would collapse `("a,b")` and `("a","b")` to
/// the same string and silently reintroduce the registry-manifest-list-order
/// dependence this key exists to prevent (ADR-1 D4 dedup). `Vec<String>` `Ord` in
/// Rust and tuple `Ord` in Python both compare lexicographically element-by-element,
/// and UTF-8 byte order equals Unicode code-point order, so the two agree.
fn platform_sort_key(platform: &native::Platform) -> (String, String, String, String, Vec<String>, Vec<String>) {
    (
        platform.architecture.to_string(),
        platform.os.to_string(),
        platform.os_version.clone().unwrap_or_default(),
        platform.variant.clone().unwrap_or_default(),
        platform.os_features.clone().unwrap_or_default(),
        platform.features.clone().unwrap_or_default(),
    )
}

/// Which of the two byte-exact forms an emit pass produces.
#[derive(Clone, Copy)]
enum Style {
    /// Root form: 2-space indent, insertion-order keys.
    Pretty,
    /// Observation form: minified, recursively-alphabetized keys.
    Minified,
}

/// Recursively emit a [`serde_json::Value`] in the chosen [`Style`].
fn write_value(value: &serde_json::Value, style: Style, depth: usize, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(true) => out.push_str("true"),
        serde_json::Value::Bool(false) => out.push_str("false"),
        // A `serde_json::Number`'s `Display` is the canonical JSON form and matches
        // Python's `repr` for the integer values this grammar carries (ids,
        // timestamps and digests are strings; the wire format has no floats).
        serde_json::Value::Number(number) => out.push_str(&number.to_string()),
        serde_json::Value::String(text) => escape_ascii(text, out),
        serde_json::Value::Array(items) => write_array(items, style, depth, out),
        serde_json::Value::Object(map) => write_object(map, style, depth, out),
    }
}

fn write_array(items: &[serde_json::Value], style: Style, depth: usize, out: &mut String) {
    if items.is_empty() {
        out.push_str("[]");
        return;
    }
    match style {
        Style::Minified => {
            out.push('[');
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                write_value(item, style, depth, out);
            }
            out.push(']');
        }
        Style::Pretty => {
            out.push_str("[\n");
            let inner = indent(depth + 1);
            for (position, item) in items.iter().enumerate() {
                if position > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&inner);
                write_value(item, style, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&indent(depth));
            out.push(']');
        }
    }
}

fn write_object(map: &serde_json::Map<String, serde_json::Value>, style: Style, depth: usize, out: &mut String) {
    if map.is_empty() {
        out.push_str("{}");
        return;
    }
    match style {
        Style::Minified => {
            // `sort_keys=True` — alphabetize recursively by the raw key bytes.
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|left, right| left.0.cmp(right.0));
            out.push('{');
            for (position, (key, value)) in entries.iter().enumerate() {
                if position > 0 {
                    out.push(',');
                }
                escape_ascii(key, out);
                out.push(':');
                write_value(value, style, depth, out);
            }
            out.push('}');
        }
        Style::Pretty => {
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
                write_value(value, style, depth + 1, out);
            }
            out.push('\n');
            out.push_str(&indent(depth));
            out.push('}');
        }
    }
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

    /// A minimal, valid `sha256:<64 hex>` digest string for building platforms.
    fn digest(fill: char) -> crate::oci::Digest {
        crate::oci::Digest::Sha256(fill.to_string().repeat(64))
    }

    fn platform(architecture: &str, os_features: &[&str]) -> native::Platform {
        native::Platform {
            architecture: architecture.into(),
            os: "linux".into(),
            os_version: None,
            os_features: (!os_features.is_empty())
                .then(|| os_features.iter().map(|feature| (*feature).to_string()).collect()),
            variant: None,
            features: None,
        }
    }

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

    // ── platform sort key ────────────────────────────────────────────────────

    #[test]
    fn platform_sort_orders_glibc_before_musl() {
        // The dual-libc case: identical os/arch, differ only in `os.features`.
        // glibc < musl lexicographically, so glibc must sort first.
        let obs = wire::Observation {
            platforms: vec![
                wire::ObservationPlatform {
                    platform: platform("amd64", &["libc.musl"]),
                    digest: digest('b'),
                },
                wire::ObservationPlatform {
                    platform: platform("amd64", &["libc.glibc"]),
                    digest: digest('a'),
                },
            ],
        };
        let bytes = serialize_observation(&obs);
        let text = String::from_utf8(bytes).unwrap();
        let glibc = text.find("libc.glibc").unwrap();
        let musl = text.find("libc.musl").unwrap();
        assert!(glibc < musl, "glibc platform must serialize before musl: {text}");
    }

    #[test]
    fn platform_sort_key_compares_os_features_as_sequence_not_joined() {
        // `("a","b")` and `("a,b")` must not collide. Distinct single-element
        // feature lists whose join would alias still order deterministically.
        let joined = platform_sort_key(&platform("amd64", &["a,b"]));
        let split = platform_sort_key(&platform("amd64", &["a", "b"]));
        assert_ne!(joined, split);
    }

    // ── empty-container edge cases (root/pretty form) ────────────────────────

    #[test]
    fn pretty_emits_empty_containers_inline() {
        let value = serde_json::json!({ "tags": {}, "list": [] });
        let bytes = serialize_root(&value);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "{\n  \"tags\": {},\n  \"list\": []\n}\n");
    }

    #[test]
    fn pretty_preserves_insertion_order_and_appends_single_newline() {
        // `preserve_order` keeps `zebra` before `apple` despite the alphabet.
        let value: serde_json::Value = serde_json::from_str(r#"{"zebra":1,"apple":2}"#).unwrap();
        let bytes = serialize_root(&value);
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text, "{\n  \"zebra\": 1,\n  \"apple\": 2\n}\n");
        assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
    }

    #[test]
    fn minified_alphabetizes_keys_and_has_no_trailing_newline() {
        let obs = wire::Observation {
            platforms: vec![wire::ObservationPlatform {
                platform: platform("amd64", &["libc.glibc"]),
                digest: digest('c'),
            }],
        };
        let bytes = serialize_observation(&obs);
        let text = String::from_utf8(bytes).unwrap();
        // `digest` before `platform` (alphabetized); `architecture` before `os`
        // before `os.features` inside the platform object.
        assert!(text.starts_with(r#"{"platforms":[{"digest":"#), "{text}");
        assert!(
            text.contains(r#""architecture":"amd64","os":"linux","os.features":"#),
            "{text}"
        );
        assert!(!text.ends_with('\n'), "observation form has no trailing newline");
    }
}
