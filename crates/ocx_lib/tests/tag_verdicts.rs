// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cross-repo drift gate for the reserved-tag rule
//! (`adr_oci_index_only_dispatch.md` D7).
//!
//! `tests/fixtures/index_wire/tag_verdicts.json` is authored in `ocx-sh/index`
//! and vendored verbatim (pinned by `SOURCE_COMMIT`). Two implementations of the
//! one rule read it: the bot's Python predicate and this crate's
//! [`Tag::is_reserved`]. Neither generates it — a fixture derived from the code
//! under test certifies whatever that code happens to do, which is how a wrong
//! predicate once stayed green indefinitely.
//!
//! A disagreement is an ocx bug or an upstream rule change, never a reason to
//! edit the fixture here: re-vendor with `test/scripts/sync_index_conformance.sh`.

use std::path::{Path, PathBuf};

use ocx_lib::{oci::Algorithm, package::tag::Tag};

/// Rows in the vendored fixture. Asserted exactly: a fixture that silently
/// shrinks would otherwise pass vacuously, and the row set is the entire answer
/// to "do the two implementations still agree?".
const EXPECTED_ROW_COUNT: usize = 17;

fn verdict_cases() -> Vec<serde_json::Value> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/index_wire/tag_verdicts.json");
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let document: serde_json::Value = serde_json::from_slice(&bytes).expect("parse tag_verdicts.json");
    document["cases"]
        .as_array()
        .expect("tag_verdicts.json has a `cases` array")
        .clone()
}

fn tag_of(case: &serde_json::Value) -> &str {
    case["tag"].as_str().expect("case has a string `tag`")
}

#[test]
fn every_vendored_verdict_matches_both_reservation_entry_points() {
    let cases = verdict_cases();
    assert_eq!(
        cases.len(),
        EXPECTED_ROW_COUNT,
        "vendored tag_verdicts.json row count changed; re-check the fixture and this constant together"
    );

    for case in &cases {
        let tag = tag_of(case);
        let expected = case["reserved"].as_bool().expect("case has a boolean `reserved`");
        // Both entry points, because callers use both: `is_reserved_str` filters
        // listings, `Tag::from(..).is_reserved()` classifies an already-parsed tag.
        assert_eq!(
            Tag::from(tag.to_string()).is_reserved(),
            expected,
            "Tag::from({tag:?}).is_reserved()"
        );
        assert_eq!(Tag::is_reserved_str(tag), expected, "Tag::is_reserved_str({tag:?})");
    }
}

#[test]
fn the_fixture_covers_every_reserved_digest_algorithm() {
    // Reservation spans `Algorithm::ALL`, deliberately wider than D7:319's
    // sha256-only text (reserving a name costs nothing; addressability stays
    // sha256-only pending N-12). A corpus covering one of three algorithms is
    // not a drift gate for the rule ocx actually implements, so the coverage is
    // asserted rather than assumed.
    let cases = verdict_cases();
    for algorithm in Algorithm::ALL {
        let prefix = format!("{}.", algorithm.prefix());
        assert!(
            cases
                .iter()
                .any(|case| tag_of(case).starts_with(&prefix) && case["reserved"] == serde_json::Value::Bool(true)),
            "no reserved `{prefix}<hex>` row in the vendored corpus"
        );
    }
}
