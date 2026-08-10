// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Canonical wire-format serializer for every ocx-index document OCX writes.
//!
//! **Byte authority: `ocx-sh/index`** — `bot/CONTRACTS.md` §14 ("Root
//! serializer — client-facing byte-exact spec") for the root, and `render.py`
//! for the other two. This module is the Rust port of that repo's
//! `validate_entry.py::serialize_package_root`; its output must match the
//! Python reference byte-for-byte.
//!
//! That match is **proven for the root only**: the `index_wire_conformance`
//! integration test drives the vendored golden vectors under
//! `crates/ocx_lib/tests/fixtures/index_wire/root/` (plus the CPython escape
//! truth table under `.../cpython/`) through [`serialize_root`]. Cross-language
//! parity fixtures for the catalog and the config are **pending in WP7
//! (C-025)**. Until they land, those two serializers are pinned only by the
//! hand-written expectations in this module's tests — literals read off
//! `render.py` by a human, not bytes any Python run emitted.
//!
//! Three documents are serialized by OCX, all through the one formatter below
//! (`adr_servable_index_snapshot.md` decision F / C-025):
//!
//! - the human-diffable `p/<ns>/<pkg>.json` root ([`serialize_root`]),
//! - the `c/index.json` catalog ([`serialize_catalog`]), and
//! - `config.json` ([`serialize_config`]).
//!
//! What a tag points *at* is not among them: it is a registry's own OCI image
//! index, stored byte-for-byte as it was served — the index writes no object
//! shapes of its own (`adr_oci_index_only_dispatch.md` D1).
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

use super::wire::{CatalogDocument, IndexFormatConfig};

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
    python_json(root)
}

/// Byte-exact `c/index.json` serialization (`render.py:309`).
///
/// Output: `json.dumps(indent=2, sort_keys=False, ensure_ascii=True)` form plus
/// a single trailing `\n` — the same [`PythonJson`] emitter [`serialize_root`]
/// uses, not a second one.
///
/// This will replace `serde_json::to_vec_pretty` at the one catalog writer,
/// `CatalogTransaction::commit` (`index_store.rs:1029`), when WP5 switches it
/// over; `to_vec_pretty`'s output diverges from the Python renderer by the
/// trailing newline — one byte, but a full-file diff on every render of a tree
/// the two implementations share. `ensure_ascii` is
/// vacuous here (`PACKAGE_ID_RE` in `validate_entry.py:70-72` restricts catalog
/// keys to `[a-z0-9]` plus separators, so a non-ASCII key is unreachable
/// upstream); it comes free with the shared formatter and still matters for
/// roots (C-025).
///
/// [`CatalogDocument`]'s field order — `format_version` then `packages` — is
/// the emitted order: `sort_keys=False` on both sides.
pub fn serialize_catalog(catalog: &CatalogDocument) -> Vec<u8> {
    python_json(catalog)
}

/// Byte-exact `config.json` serialization (`render.py:334-338`).
///
/// Output: `json.dumps(indent=2, sort_keys=False, ensure_ascii=True)` form plus
/// a single trailing `\n`, through the same [`PythonJson`] emitter as the other
/// two documents. `config.json` has no string field at all, so `ensure_ascii`
/// is vacuous here.
///
/// The two producers agree on **form** and differ on **content**. Form: the
/// two-space indent, `": "`, `","`, `sort_keys=False` declaration order, and
/// the one trailing newline are the same on both sides. Content: `render.py`
/// emits `name_segments` from a module constant (`NAME_SEGMENTS = 2`,
/// `core/render.py:46`) **unconditionally**, so the reference never renders
/// `{"format_version": 1}` — while that is exactly what OCX writes, because
/// `name_segments` is an operator declaration OCX cannot derive from a tree and
/// omitting it is honest where guessing `2` would not be. That omission is
/// [`IndexFormatConfig::name_segments`]'s `skip_serializing_if`; [`IndexFormatConfig`]
/// carries no OCX-only field either way.
///
/// No churn follows from the difference: C-023 writes this config only when the
/// file is **absent**, and `regenerate` never writes one at all, so the two
/// producers never write the same file.
pub fn serialize_config(config: &IndexFormatConfig) -> Vec<u8> {
    python_json(config)
}

/// The one emitter behind all three public serializers.
///
/// Generic over `T: Serialize` rather than taking [`serde_json::Value`]: the
/// catalog and config are modelled types, and routing them through a `Value`
/// would re-sort or re-shape what the wire pins. Roots stay a `Value` because
/// they carry human-governed fields OCX does not model and must ride through
/// verbatim.
fn python_json<T: Serialize>(document: &T) -> Vec<u8> {
    let mut out = Vec::new();
    let mut serializer = serde_json::Serializer::with_formatter(&mut out, PythonJson::new());
    document
        .serialize(&mut serializer)
        .expect("serializing an index document into a Vec cannot fail");
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
    use std::collections::BTreeMap;
    use std::num::NonZeroU32;

    use super::*;

    // ── C-025 · one formatter, three documents ───────────────────────────────
    //
    // Every assertion below is on the emitted **bytes**. Parsing the output and
    // comparing structures cannot see a trailing newline, an indent width, or a
    // field order — which is the entire divergence this contract closes.

    /// C-001/C-025 — `skip_serializing_if` omits an absent `name_segments`
    /// entirely, so the config the update path will write is exactly
    /// `{"format_version": 1}` rather than a superset of it carrying a `null`.
    /// This is a hand-written expectation, not cross-language parity: the
    /// Python renderer never emits these bytes (it always writes
    /// `name_segments`) — see [`serialize_config`] for why the two producers
    /// differ on content and never collide.
    #[test]
    fn config_omits_an_absent_name_segments() {
        let bytes = serialize_config(&IndexFormatConfig {
            format_version: 1,
            name_segments: None,
        });
        assert_eq!(
            String::from_utf8(bytes).expect("config.json is ASCII"),
            "{\n  \"format_version\": 1\n}\n"
        );
    }

    /// C-001 — declaration order is wire order (`sort_keys=False` on both
    /// sides, `render.py:334-338`). Asserted on the bytes: a parsed comparison
    /// discards the very ordering the contract pins, so it would be vacuous.
    #[test]
    fn config_emits_format_version_before_name_segments() {
        let bytes = serialize_config(&IndexFormatConfig {
            format_version: 1,
            name_segments: NonZeroU32::new(2),
        });
        assert_eq!(
            String::from_utf8(bytes).expect("config.json is ASCII"),
            "{\n  \"format_version\": 1,\n  \"name_segments\": 2\n}\n"
        );
    }

    /// C-025 — `c/index.json` in Python `indent=2` form: two-space indent,
    /// `": "` between key and value, `","` between items, one trailing newline.
    /// `serde_json::to_vec_pretty` — the emitter being replaced — produces this
    /// layout without the final byte.
    #[test]
    fn catalog_emits_the_python_indent_two_form() {
        let packages = BTreeMap::from([
            ("kitware/cmake".to_string(), "sha256:1111".to_string()),
            ("stable/tool".to_string(), "sha256:2222".to_string()),
        ]);
        let bytes = serialize_catalog(&CatalogDocument::new(packages));
        assert_eq!(
            String::from_utf8(bytes).expect("a catalog of ASCII package ids is ASCII"),
            "{\n  \"format_version\": 1,\n  \"packages\": {\n    \"kitware/cmake\": \"sha256:1111\",\n    \"stable/tool\": \"sha256:2222\"\n  }\n}\n"
        );
    }

    /// C-025 — an index with nothing published yet still emits the envelope,
    /// with the empty map inline (`{}`), exactly as `json.dumps` renders an
    /// empty container under `indent=2`.
    #[test]
    fn catalog_with_no_packages_emits_an_inline_empty_object() {
        let bytes = serialize_catalog(&CatalogDocument::new(BTreeMap::new()));
        assert_eq!(
            String::from_utf8(bytes).expect("an empty catalog is ASCII"),
            "{\n  \"format_version\": 1,\n  \"packages\": {}\n}\n"
        );
    }

    /// C-025 — exactly one trailing `\n` on every document. The catalog's
    /// missing newline is the whole divergence being closed; a second one would
    /// be just as wrong, and `\n\n` is what a naive `push(b'\n')` on top of an
    /// already-terminated emitter produces.
    #[test]
    fn every_document_ends_with_exactly_one_newline() {
        let documents: [(&str, Vec<u8>); 3] = [
            ("root", serialize_root(&serde_json::json!({ "format_version": 1 }))),
            ("catalog", serialize_catalog(&CatalogDocument::new(BTreeMap::new()))),
            (
                "config",
                serialize_config(&IndexFormatConfig {
                    format_version: 1,
                    name_segments: None,
                }),
            ),
        ];
        for (document, bytes) in documents {
            let text = String::from_utf8(bytes).expect("ensure_ascii output is ASCII");
            assert!(
                text.ends_with('\n'),
                "{document} must end with a trailing newline, got {text:?}"
            );
            assert!(
                !text.ends_with("\n\n"),
                "{document} must end with exactly one newline, got {text:?}"
            );
        }
    }

    /// C-025 — the catalog and the root emit the same bytes for the same
    /// document.
    ///
    /// Both halves assert against one literal, so what this genuinely catches
    /// is either emitter drifting off the Python form — plus the root
    /// characterization, that generalizing the private body did not move
    /// `serialize_root`'s bytes. It does **not** observe that the two share one
    /// formatter: a byte-identical second emitter would pass it. That is a
    /// structural property, and the only thing that pins it is the one-line
    /// body of [`serialize_catalog`].
    #[test]
    fn catalog_and_root_emit_the_same_bytes() {
        let catalog = CatalogDocument::new(BTreeMap::from([(
            "kitware/cmake".to_string(),
            "sha256:1111".to_string(),
        )]));
        let expected =
            "{\n  \"format_version\": 1,\n  \"packages\": {\n    \"kitware/cmake\": \"sha256:1111\"\n  }\n}\n";

        let through_root = serialize_root(&serde_json::to_value(&catalog).expect("a catalog is a JSON object"));
        assert_eq!(
            String::from_utf8(through_root).expect("the root emitter emits ASCII"),
            expected,
            "serialize_root's bytes must be unchanged"
        );

        let through_catalog = serialize_catalog(&catalog);
        assert_eq!(
            String::from_utf8(through_catalog).expect("the catalog emitter emits ASCII"),
            expected
        );
    }

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
