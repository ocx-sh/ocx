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

use ocx_oci::{Algorithm, Digest, tag::is_reserved_tag, tag::referrer_fallback_tag, tag::sbom_sidecar_tag};
use ocx_package::{tag::Tag, version::Version};

/// Rows in the vendored fixture. Asserted exactly: a fixture that silently
/// shrinks would otherwise pass vacuously, and the row set is the entire answer
/// to "do the two implementations still agree?".
const EXPECTED_ROW_COUNT: usize = 19;

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

/// D-020 — the split did not move the line between reserved and not.
///
/// `is_reserved_tag` is the registry-side rule stated without the version
/// grammar; `Tag::is_reserved` reaches the same verdict by parsing the tag and
/// asking the parsed value. Which tags are reserved is wire-visible — a
/// reserved tag is never offered back as a package version — so the rule was
/// allowed to move to `ocx_oci` only on condition that the two answers stay
/// identical. This runs the whole vendored corpus through both, against the
/// fixture's own stated verdict rather than against each other, so a pair that
/// agreed on the wrong answer would still red above.
#[test]
fn the_version_free_rule_answers_exactly_what_the_parsed_verdict_answers() {
    let cases = verdict_cases();
    assert_eq!(
        cases.len(),
        EXPECTED_ROW_COUNT,
        "vendored tag_verdicts.json row count changed; re-check the fixture and this constant together"
    );

    for case in &cases {
        let tag = tag_of(case);
        let expected = case["reserved"].as_bool().expect("case has a boolean `reserved`");
        assert_eq!(is_reserved_tag(tag), expected, "is_reserved_tag({tag:?})");
        assert_eq!(
            is_reserved_tag(tag),
            Tag::from(tag.to_string()).is_reserved(),
            "the version-free rule and the parsed verdict disagree on {tag:?}"
        );
    }
}

/// The one way the two rules could ever diverge, closed by assertion.
///
/// `Tag::from` runs `Version::parse` at step 3 — *ahead* of both digest-shaped
/// arms — so any string the version grammar accepts classifies as
/// `Tag::Version` and is **not** reserved, however digest-shaped it looks.
/// `is_reserved_tag` never asks the version grammar anything. The two therefore
/// agree only while no `parse_keep` or referrer-fallback form parses as a
/// version, and that is arithmetic rather than design: every `hex_len()` is 64,
/// 96 or 128, and a decimal number that long overflows the `u32` the version
/// parser wants. Add a shorter-digest algorithm and `shaN-12345678` would parse
/// as a version, un-reserving it on one side only — silently, since every row
/// of the corpus above would still agree.
///
/// Both separators and every cosign suffix are covered, over every algorithm,
/// with the all-digit body that is the only one the version grammar could take.
#[test]
fn no_keep_or_fallback_form_parses_as_a_version() {
    for algorithm in Algorithm::ALL {
        for body in ["1", "0", "a"] {
            let hex = body.repeat(algorithm.hex_len());
            let truncated: String = hex.chars().take(64).collect();
            let mut forms = vec![
                format!("{}.{hex}", algorithm.prefix()),
                format!("{}-{hex}", algorithm.prefix()),
                format!("{}-{truncated}", algorithm.prefix()),
                format!("__ocx.keep.{}-{hex}", algorithm.prefix()),
            ];
            for suffix in [".sig", ".att", ".sbom"] {
                forms.push(format!("{}-{hex}{suffix}", algorithm.prefix()));
            }
            for form in forms {
                assert!(
                    Version::parse(&form).is_none(),
                    "{form} must not parse as a version — the day it does, `Tag::from` classifies \
                     it `Version` and un-reserves it while `is_reserved_tag` still reserves it"
                );
                assert!(
                    is_reserved_tag(&form),
                    "{form} is a reserved registry form and must stay reserved"
                );
                assert!(
                    Tag::from(form.clone()).is_reserved(),
                    "{form} is a reserved registry form and must stay reserved through the parse"
                );
            }
        }
    }
}

/// The two tags `ocx_oci` writes are the two strings the classifier refuses to
/// read back as a version — asserted through the writers, not through a
/// hand-spelled shape, so the pair cannot drift apart across the split.
#[test]
fn every_tag_the_oci_writers_emit_is_reserved_on_both_sides() {
    for algorithm in Algorithm::ALL {
        let digest = Digest::try_from(format!("{}:{}", algorithm.prefix(), "a".repeat(algorithm.hex_len())))
            .expect("a full-length hex body is a valid digest");
        for tag in [referrer_fallback_tag(&digest), sbom_sidecar_tag(&digest)] {
            assert!(is_reserved_tag(&tag), "is_reserved_tag({tag:?})");
            assert!(Tag::from(tag.clone()).is_reserved(), "Tag::from({tag:?}).is_reserved()");
        }
    }
}
