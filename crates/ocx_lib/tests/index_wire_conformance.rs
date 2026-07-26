// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cross-language byte-parity conformance for the canonical root wire serializer
//! (cross-track contract #1). Every vector under
//! `tests/fixtures/index_wire/root/` is the exact output of `ocx-sh/index`'s
//! `bot/CONTRACTS.md` §14 Python serializer, vendored verbatim (see that
//! directory's `README.md` — never hand-edited here). This test proves the Rust
//! [`ocx_lib::oci::index::serialize_root`] port emits byte-identical bytes.
//!
//! A byte mismatch is a serializer bug — the fixtures are frozen upstream (pinned
//! by `SOURCE_COMMIT`); fix the emitter, never the fixture.

use std::path::{Path, PathBuf};

use ocx_lib::oci::index::{IndexRoot, YankMarker, serialize_root};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/index_wire")
}

/// Every `.json` file directly under `dir`, sorted for deterministic iteration.
fn json_fixtures(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read fixture dir {}: {error}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "json"))
        .collect();
    files.sort();
    files
}

/// Byte-equality with a diff-locating panic message (fixtures are the authority).
fn assert_bytes_equal(actual: &[u8], expected: &[u8], label: &str) {
    if actual == expected {
        return;
    }
    let first_diff = actual
        .iter()
        .zip(expected.iter())
        .position(|(left, right)| left != right);
    panic!(
        "byte mismatch for {label}\n  actual len   = {}\n  expected len = {}\n  first diff at = {:?}\n  actual around : {:?}\n  expected around: {:?}",
        actual.len(),
        expected.len(),
        first_diff,
        first_diff.map(|index| snippet(actual, index)),
        first_diff.map(|index| snippet(expected, index)),
    );
}

/// A short byte window around `index` for the mismatch message.
fn snippet(bytes: &[u8], index: usize) -> String {
    let start = index.saturating_sub(12);
    let end = (index + 12).min(bytes.len());
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}

#[test]
fn root_fixtures_round_trip_byte_exact() {
    let root_dir = fixtures_dir().join("root");
    let fixtures = json_fixtures(&root_dir);
    assert!(
        fixtures.len() >= 2,
        "expected at least the minimal + full-fields root vectors, found {}",
        fixtures.len()
    );

    for path in fixtures {
        let expected = std::fs::read(&path).expect("read root fixture");
        // Order-preserving parse (serde_json `preserve_order`) — fields must stay
        // in on-disk order for the insertion-order root form to reproduce.
        let value: serde_json::Value =
            serde_json::from_slice(&expected).expect("parse root fixture as order-preserving Value");
        let produced = serialize_root(&value);
        assert_bytes_equal(&produced, &expected, &path.display().to_string());
    }
}

/// Per-scalar escape parity against a CPython-generated truth table.
///
/// The vendored `root/` corpus certifies the shapes the index actually ships,
/// which is why a wrong escape boundary (DEL emitted raw) once survived it: no
/// fixture contains a `0x7F` byte. This sweep closes that by asking CPython
/// directly, scalar by scalar — dense through U+02FF, every boundary
/// neighbourhood, then strides over the BMP and the astral planes. See
/// `fixtures/index_wire/cpython/generate.py` for the exact selection and how to
/// regenerate.
#[test]
fn codepoint_escapes_match_the_cpython_truth_table() {
    let path = fixtures_dir().join("cpython/codepoint_escapes.txt");
    let table = std::fs::read_to_string(&path).expect("read codepoint truth table");

    let mut checked = 0_usize;
    for line in table.lines().filter(|line| !line.starts_with('#')) {
        let (code, expected) = line.split_once('\t').expect("<hex>\\t<literal> line");
        let scalar = u32::from_str_radix(code, 16)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or_else(|| panic!("{code} is not a Unicode scalar"));

        // A top-level string: `json.dumps` renders it identically at any indent,
        // so the vector is the whole expected document bar the trailing newline.
        let produced = serialize_root(&serde_json::Value::String(scalar.to_string()));
        assert_bytes_equal(&produced, format!("{expected}\n").as_bytes(), &format!("U+{code}"));
        checked += 1;
    }
    assert!(checked > 3000, "truth table looks truncated: {checked} scalars");
}

/// Layout parity against CPython for the shapes the vendored corpus never
/// exercises: empty containers, an array root, deep nesting, u64/i64 extremes
/// and escape-bearing object keys. Each vector is already in the canonical form,
/// so re-serializing its parse must reproduce it byte-for-byte.
#[test]
fn cpython_structural_vectors_round_trip_byte_exact() {
    let vectors = json_fixtures(&fixtures_dir().join("cpython"));
    assert_eq!(vectors.len(), 2, "expected the layout + array-root vectors");

    for path in vectors {
        let expected = std::fs::read(&path).expect("read cpython vector");
        let value: serde_json::Value =
            serde_json::from_slice(&expected).expect("parse cpython vector as order-preserving Value");
        assert_bytes_equal(&serialize_root(&value), &expected, &path.display().to_string());
    }
}

#[test]
fn non_ascii_scalars_emit_unicode_escapes_not_raw_utf8() {
    // §3.3 hazard: `ensure_ascii`. The full-fields root carries `"Café Widget"`;
    // its serialized bytes must contain the ASCII escape `é`, never the raw
    // two-byte UTF-8 `é` (0xC3 0xA9) — the exact way `to_string_pretty` diverges.
    let path = fixtures_dir().join("root/full-fields.json");
    let expected = std::fs::read(&path).expect("read full-fields fixture");
    let value: serde_json::Value = serde_json::from_slice(&expected).expect("parse full-fields");
    let produced = serialize_root(&value);

    // The 6 ASCII bytes of the escape `é` (backslash, u, 0, 0, e, 9).
    let ascii_escape: &[u8] = b"\\u00e9";
    assert!(
        produced.windows(6).any(|window| window == ascii_escape),
        "non-ASCII scalar must serialize as the ASCII escape, not raw UTF-8"
    );
    // The raw two-byte UTF-8 encoding of `é` must never appear.
    assert!(
        !produced.windows(2).any(|window| window == [0xC3, 0xA9]),
        "raw UTF-8 (0xC3 0xA9) must never appear in ensure_ascii output"
    );
}

#[test]
fn no_churn_invariant_serialize_of_parse_is_identity() {
    // C6 unchanged short-circuit rests on this: re-serializing an already-canonical
    // committed root reproduces its exact bytes, so announce can byte-compare.
    let path = fixtures_dir().join("root/minimal.json");
    let expected = std::fs::read(&path).expect("read minimal fixture");
    let value: serde_json::Value = serde_json::from_slice(&expected).expect("parse minimal");
    assert_bytes_equal(&serialize_root(&value), &expected, "no-churn minimal.json");
}

#[test]
fn full_fields_root_parses_into_the_typed_index_root_with_the_real_yanked_object() {
    // Regression: the REAL index serves a yanked tag as an object
    // (`{"reason": "...", "at": "..."}`), never a bare boolean. Typed
    // `IndexRoot` deserialize of this frozen upstream fixture must succeed —
    // it used to fail because `RootTag::yanked` was mistyped `Option<bool>`.
    let path = fixtures_dir().join("root/full-fields.json");
    let bytes = std::fs::read(&path).expect("read full-fields fixture");
    let root: IndexRoot = serde_json::from_slice(&bytes).expect("typed IndexRoot parse of full-fields.json");

    let tag = root.tags.get("1.0.0").expect("1.0.0 tag present");
    assert_eq!(
        tag.yanked,
        Some(YankMarker {
            reason: "critical security issue".to_string(),
            at: "2026-02-01T00:00:00Z".to_string(),
        })
    );
}
