// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::*;
use crate::exec_state::types::MAX_HEADER;

fn narrowing(offline: bool, frozen: bool, no_consent: bool, credential_helper: bool) -> Narrowing {
    Narrowing {
        offline,
        frozen,
        no_consent,
        credential_helper,
    }
}

fn abs_home() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\ocx-home")
    } else {
        PathBuf::from("/ocx-home")
    }
}

fn h_segment(path: &str) -> String {
    format!("h={}", URL_SAFE_NO_PAD.encode(path))
}

#[test]
fn plain_major_with_no_segments_is_valid() {
    let parsed = parse(b"1.");
    assert!(parsed.valid);
    assert_eq!(parsed.view.major, Some(1));
    assert_eq!(parsed.view.narrowing, Narrowing::default());
    assert_eq!(parsed.view.home, None);
    assert_eq!(parsed.payload_start, Some(1));
}

#[test]
fn payload_start_points_at_the_first_dot() {
    let raw = b"2;n=offline.abc.def";
    let parsed = parse(raw);
    let start = parsed.payload_start.expect("dot found");
    assert_eq!(raw[start], b'.');
    assert_eq!(&raw[..start], b"2;n=offline");
}

#[test]
fn all_narrowing_names_parse() {
    let parsed = parse(b"2;n=offline,frozen,no_consent,credential_helper.");
    assert!(parsed.valid);
    assert_eq!(parsed.view.narrowing, narrowing(true, true, true, true));
}

#[test]
fn credential_helper_does_not_set_offline_in_the_header() {
    let parsed = parse(b"2;n=credential_helper.");
    assert_eq!(parsed.view.narrowing, narrowing(false, false, false, true));
}

#[test]
fn two_n_segments_in_either_order_are_both_honoured() {
    let a = parse(b"2;n=offline;n=frozen.");
    let b = parse(b"2;n=frozen;n=offline.");
    assert_eq!(a.view.narrowing, narrowing(true, true, false, false));
    assert_eq!(b.view.narrowing, a.view.narrowing);
}

#[test]
fn unknown_segment_before_n_does_not_hide_it() {
    let parsed = parse(b"2;zz=whatever;n=frozen.");
    assert!(parsed.valid);
    assert_eq!(parsed.view.narrowing, narrowing(false, true, false, false));
}

#[test]
fn unknown_narrowing_names_are_ignored_and_known_ones_kept() {
    let parsed = parse(b"2;n=future_thing,offline,.");
    assert_eq!(parsed.view.narrowing, narrowing(true, false, false, false));
}

#[test]
fn non_ascii_byte_ends_only_its_own_segment() {
    let mut raw = b"2;n=offline,fro".to_vec();
    raw.push(0xff);
    raw.extend_from_slice(b"zen;n=no_consent.");
    let parsed = parse(&raw);
    assert!(parsed.valid);
    assert!(parsed.view.narrowing.offline, "name before the byte is kept");
    assert!(!parsed.view.narrowing.frozen, "the name the byte cut is not honoured");
    assert!(parsed.view.narrowing.no_consent, "a later segment is untouched");
}

#[test]
fn non_ascii_segment_before_n_does_not_hide_it() {
    let mut raw = b"2;x=".to_vec();
    raw.extend_from_slice("é".as_bytes());
    raw.extend_from_slice(b";n=frozen.");
    let parsed = parse(&raw);
    assert!(parsed.view.narrowing.frozen);
}

#[test]
fn valid_h_is_read() {
    let home = abs_home();
    let raw = format!("2;{}.", h_segment(home.to_str().unwrap()));
    let parsed = parse(raw.as_bytes());
    assert_eq!(parsed.view.home.as_deref(), Some(home.as_path()));
}

#[test]
fn bad_h_is_dropped_alone() {
    for bad_h in [
        "h=!!!not-base64".to_owned(),
        format!("{}=", h_segment("/abs")),
        h_segment("relative/dir"),
        format!("h={}", URL_SAFE_NO_PAD.encode([0xff, 0xfe, 0xfd])),
        "h=".to_owned(),
    ] {
        let raw = format!("2;n=offline;{bad_h};n=frozen.");
        let parsed = parse(raw.as_bytes());
        assert!(parsed.valid, "{bad_h}");
        assert_eq!(parsed.view.home, None, "{bad_h}");
        assert_eq!(parsed.view.narrowing, narrowing(true, true, false, false), "{bad_h}");
    }
}

#[test]
fn h_with_non_ascii_byte_is_dropped() {
    let mut raw = format!("2;{}", h_segment(abs_home().to_str().unwrap())).into_bytes();
    raw.push(0xc3);
    raw.extend_from_slice(b".");
    assert_eq!(parse(&raw).view.home, None);
}

#[test]
fn eleven_digit_major_is_invalid_but_keeps_narrowing() {
    let parsed = parse(b"12345678901;n=offline.");
    assert!(!parsed.valid);
    assert_eq!(parsed.view.major, None);
    assert!(parsed.view.narrowing.offline);
    assert_eq!(parsed.payload_start, None);
}

#[test]
fn major_beyond_u32_is_invalid() {
    let parsed = parse(b"4294967296;n=frozen.");
    assert!(!parsed.valid);
    assert_eq!(parsed.view.major, None);
    assert!(parsed.view.narrowing.frozen);
}

#[test]
fn ten_digit_major_within_u32_is_valid() {
    let parsed = parse(b"4294967295.");
    assert!(parsed.valid);
    assert_eq!(parsed.view.major, Some(u32::MAX));
}

#[test]
fn empty_or_non_numeric_major_is_invalid() {
    for raw in [&b".payload"[..], b"x;n=offline.", b"+1.", b"1a.", b";n=offline."] {
        let parsed = parse(raw);
        assert!(!parsed.valid, "{raw:?}");
        assert_eq!(parsed.view.major, None, "{raw:?}");
    }
    assert!(parse(b";n=offline.").view.narrowing.offline);
}

#[test]
fn no_dot_within_max_header_is_invalid_but_keeps_narrowing() {
    let raw = format!("2;n=offline;{}", "a".repeat(MAX_HEADER));
    let parsed = parse(raw.as_bytes());
    assert!(!parsed.valid);
    assert_eq!(parsed.payload_start, None);
    assert!(parsed.view.narrowing.offline);
}

#[test]
fn no_dot_at_all_is_invalid() {
    let parsed = parse(b"2;n=frozen");
    assert!(!parsed.valid);
    assert!(parsed.view.narrowing.frozen);
}

#[test]
fn dot_exactly_at_the_header_limit_is_valid() {
    let mut raw = b"2;pad=".to_vec();
    raw.resize(MAX_HEADER, b'a');
    raw.push(b'.');
    let parsed = parse(&raw);
    assert!(parsed.valid);
    assert_eq!(parsed.payload_start, Some(MAX_HEADER));
}

#[test]
fn dot_one_past_the_header_limit_is_invalid() {
    let mut raw = b"2;pad=".to_vec();
    raw.resize(MAX_HEADER + 1, b'a');
    raw.push(b'.');
    let parsed = parse(&raw);
    assert!(!parsed.valid);
    assert_eq!(parsed.payload_start, None);
}

#[test]
fn n_beyond_the_header_limit_is_not_read() {
    let mut raw = b"2;pad=".to_vec();
    raw.resize(MAX_HEADER, b'a');
    raw.extend_from_slice(b";n=offline");
    let parsed = parse(&raw);
    assert!(!parsed.valid);
    assert!(!parsed.view.narrowing.offline);
}

// Cut at a multiple of four, the base64 still decodes, to a prefix of the real path.
#[test]
fn h_cut_by_the_header_bound_is_dropped() {
    let root = if cfg!(windows) { r"C:\" } else { "/" };
    let long_home = format!("{root}{}", "d".repeat(1_800));
    let raw = format!("2;n=offline;x;{}", h_segment(&long_home));
    assert_eq!((raw.find(";h=").expect("an h segment") + 3) % 4, 0);
    assert!(raw.len() > MAX_HEADER);
    let parsed = parse(raw.as_bytes());
    assert!(!parsed.valid);
    assert!(parsed.view.narrowing.offline);
    assert_eq!(parsed.view.home, None);
}

#[test]
fn emit_with_nothing_is_the_bare_major() {
    assert_eq!(emit(2, &Narrowing::default(), None), "2");
}

#[test]
fn emit_orders_names_stably() {
    assert_eq!(
        emit(2, &narrowing(true, true, true, true), None),
        "2;n=offline,frozen,no_consent,credential_helper"
    );
    assert_eq!(emit(2, &narrowing(false, true, false, false), None), "2;n=frozen");
}

#[test]
fn emit_then_parse_round_trips() {
    let home = abs_home();
    for n in [
        Narrowing::default(),
        narrowing(true, false, false, false),
        narrowing(false, true, true, false),
        narrowing(true, true, true, true),
    ] {
        for home in [None, Some(home.as_path())] {
            let value = format!("{}.", emit(2, &n, home));
            let parsed = parse(value.as_bytes());
            assert!(parsed.valid, "{value}");
            assert_eq!(parsed.view.major, Some(2));
            assert_eq!(parsed.view.narrowing, n);
            assert_eq!(parsed.view.home.as_deref(), home);
        }
    }
}

#[test]
fn emit_skips_a_relative_home() {
    assert_eq!(emit(2, &Narrowing::default(), Some(Path::new("rel/dir"))), "2");
}

#[test]
fn emit_output_stays_in_the_value_alphabet() {
    let home = abs_home();
    let header = emit(2, &narrowing(true, true, true, true), Some(&home));
    assert!(
        header
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b';' | b'=' | b',' | b'-' | b'_')),
        "{header}"
    );
}

#[test]
fn a_home_with_a_control_or_nul_character_is_dropped_and_the_rest_of_the_header_honoured() {
    let root = abs_home();
    let root = root.to_str().unwrap();
    for hostile in [
        format!("{root}\0evil"),
        format!("{root}\nevil"),
        format!("{root}\x1b[2Jevil"),
        format!("{root}\x7fevil"),
        format!("{root}\revil"),
    ] {
        let parsed = parse(format!("2;n=offline;{}.", h_segment(&hostile)).as_bytes());
        assert!(parsed.valid, "the header stays valid: {hostile:?}");
        assert_eq!(
            parsed.view.home, None,
            "a home with a control character is dropped: {hostile:?}"
        );
        assert!(
            parsed.view.narrowing.offline,
            "the narrowing is still honoured: {hostile:?}"
        );
    }
}
