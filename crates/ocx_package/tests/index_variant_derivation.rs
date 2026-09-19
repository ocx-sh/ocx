// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cross-language parity for the index `variants` **derivation**.
//!
//! This test pins two crates against each other: the vendored `ocx-sh/index`
//! vectors that `ocx_index` owns, and the tag-to-variant derivation,
//! [`ocx_package::version::variant_names`], that owns them here. `ocx_index`
//! cannot host it — `crate_map.toml` puts `ocx_package` above `ocx_index`, so
//! the edge it would need runs the wrong way, and the `[dev]` section legalises
//! no back-edge that would let it. WP-28 parked it in `ocx_lib` for exactly one
//! batch; WP-30 moved it here with its subject, and the fixture reach below is
//! now the ordinary downward one that WP-28 predicted.
//!
//! Split out of `crates/ocx_index/tests/index_wire_conformance.rs` when the
//! index tier was extracted; the serializer round-trips stayed there.

use std::path::{Path, PathBuf};

/// The vectors live with the crate that owns the wire format, one directory up
/// and across. A wrong path here fails loudly in `json_fixtures` (`read_dir`
/// panics naming the directory), never silently as an empty corpus — and the
/// `vectors_with_variants` floor at the bottom is the second guard.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ocx_index/tests/fixtures/index_wire")
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

/// Cross-language parity for the `variants` **derivation**, not just its bytes.
///
/// The round-trip test above proves the serializer reproduces whatever the
/// vector says; it would pass just as happily if the Python bot and this crate
/// disagreed about which tags name a variant. This one closes that gap: every
/// vendored vector's recorded `variants` must equal what
/// [`ocx_package::version::variant_names`] derives from that same vector's
/// own `tags`. The two implementations are pinned to each other through bytes
/// neither of them produced in this process.
///
/// `with-variants.json` is the vector that makes it bite — its tag set carries
/// a prerelease-bearing `musl-3.13.1-rc1` (which the bot's narrower
/// `_VERSION_RE` would reject, dropping `musl`), a bare `slim` rolling tag
/// (not a version, contributes nothing), and an unprefixed `3.13.1`/`latest`
/// pair (the default variant, which has no name and must not appear).
#[test]
fn recorded_variants_match_this_crates_derivation_over_the_same_tags() {
    let root_dir = fixtures_dir().join("root");
    let mut vectors_with_variants = 0;

    for path in json_fixtures(&root_dir) {
        let bytes = std::fs::read(&path).expect("read root fixture");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse root fixture");
        let tags: Vec<&str> = value["tags"]
            .as_object()
            .expect("every root vector carries a tags object")
            .keys()
            .map(String::as_str)
            .collect();
        let derived = ocx_package::version::variant_names(tags);

        let recorded: Vec<String> = match value.get("variants") {
            // Absent is the wire spelling of "no variants" — never `[]`.
            None => Vec::new(),
            Some(array) => {
                vectors_with_variants += 1;
                array
                    .as_array()
                    .expect("variants is an array")
                    .iter()
                    .map(|name| name.as_str().expect("a variant name is a string").to_string())
                    .collect()
            }
        };

        assert_eq!(
            recorded,
            derived,
            "{}: the vendored root records {recorded:?} but this crate derives {derived:?} from its own tags",
            path.display()
        );
    }

    assert_eq!(
        vectors_with_variants, 1,
        "the corpus must keep a vector that actually exercises a non-empty variant set, \
         or this test passes by asserting empty == empty for every vector"
    );
}
