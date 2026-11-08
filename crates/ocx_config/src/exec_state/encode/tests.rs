// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use super::*;
use crate::exec_state::decode::decode;
use crate::exec_state::types::{
    AppliedValue, IdentityRecord, Inherited, MAX_HEADER, MAX_INFLATED, MAX_RECORDS, MINOR, Narrowing, Pinned,
    PinnedTier, Prior, RecordedValue, ReplayPayload, ReplayValue, Scope, ScopeId, Selection, SessionSource, ValueKind,
};

fn abs(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

/// Deterministic incompressible text (6 bits of entropy per char): zlib cannot shrink it, so record
/// counts map to value sizes.
fn noise(seed: u64, len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            char::from(ALPHABET[usize::try_from(x >> 58).unwrap_or(0)])
        })
        .collect()
}

fn constant(key: &str, value: String, owner: Owner) -> AppliedValue {
    AppliedValue {
        key: key.to_string(),
        value: RecordedValue::Literal(value),
        kind: ValueKind::Constant,
        separator: None,
        pre_existing: false,
        owner,
        prior: Some(Prior::Unset),
    }
}

fn session_owner() -> Owner {
    Owner::Session {
        scope: ScopeId::Global,
        source: SessionSource::Composed,
    }
}

fn narrowing_all() -> Narrowing {
    Narrowing {
        offline: true,
        frozen: true,
        no_consent: true,
        credential_helper: true,
    }
}

fn full_state() -> ExecState {
    ExecState {
        narrowing: narrowing_all(),
        home: Some(abs("ocx home")),
        applied: vec![
            constant("FOO", "bar".into(), session_owner()),
            AppliedValue {
                key: "PATH".into(),
                value: RecordedValue::Literal(abs("tool").join("bin").to_string_lossy().into_owned()),
                kind: ValueKind::Path,
                separator: None,
                pre_existing: true,
                owner: Owner::Inherited,
                prior: None,
            },
        ],
        inherited_scopes: vec![Scope::Global, Scope::Project { root: abs("proj") }],
        replay: crate::exec_state::types::Replay {
            env: vec![ReplayValue {
                key: "LANG_TOOL".into(),
                kind: ValueKind::Constant,
                separator: None,
                value: ReplayPayload::Literal("x".into()),
            }],
            launch_identities: vec![IdentityRecord {
                digest: format!("sha256:{}", "ab".repeat(32)),
                names: vec!["example.com/tool:1".into()],
                owner: Owner::Inherited,
                no_patches: true,
            }],
        },
        selection: Selection::Pinned(Pinned {
            tier: PinnedTier::Project { dir: abs("proj") },
            groups: vec!["dev".into()],
            with_names: vec![],
            env_keys: vec![],
            invocation_digest: "0".repeat(64),
            pinned_paths: Some(true),
        }),
        ..ExecState::default()
    }
}

/// A state whose size the test steers: `inherited` and `session` records of `len` noise chars each.
fn sized(inherited: usize, session: usize, len: usize) -> ExecState {
    let mut state = ExecState {
        narrowing: narrowing_all(),
        home: Some(abs("ocx-home")),
        inherited_scopes: vec![Scope::Global],
        ..ExecState::default()
    };
    for index in 0..inherited {
        state.applied.push(constant(
            &format!("INH_{index}"),
            noise(index as u64, len),
            Owner::Inherited,
        ));
    }
    for index in 0..session {
        state.applied.push(constant(
            &format!("SES_{index}"),
            noise(1_000 + index as u64, len),
            session_owner(),
        ));
    }
    state
}

/// Replay values no rung 1 or 2 can drop: only the header survives.
fn replay_heavy() -> ExecState {
    let mut state = sized(0, 0, 0);
    state.replay.env = (0..12)
        .map(|index| ReplayValue {
            key: format!("REPLAY_{index}"),
            kind: ValueKind::Constant,
            separator: None,
            value: ReplayPayload::Literal(noise(index, 1_000)),
        })
        .collect();
    state
}

fn in_alphabet(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b';' | b'=' | b',' | b'.' | b'_' | b'-'))
}

fn readable(value: &str) -> ExecState {
    match decode(Some(std::ffi::OsStr::new(value))) {
        Inherited::Readable(state) => state,
        other => panic!("expected a readable state, got {other:?}"),
    }
}

#[test]
fn full_state_round_trips_through_decode() {
    let state = full_state();
    let encoded = encode(&state);
    assert_eq!(encoded.truncated, Truncation::None);
    let value = encoded.value.expect("a small state fits");
    assert_eq!(encoded.encoded_len, value.len());
    assert!(value.starts_with(&format!("{MAJOR};n=offline,frozen,no_consent,credential_helper;h=")));
    assert_eq!(readable(&value), state);
}

#[test]
fn default_state_round_trips() {
    let state = ExecState::default();
    let value = encode(&state).value.expect("fits");
    assert_eq!(readable(&value), state);
}

/// EC-LEDGER-015: an additive schema change keeps the recorded priors readable.
#[test]
fn unknown_field_and_higher_minor_decode() {
    let mut json = serde_json::to_value(full_state()).expect("serializes");
    json["minor"] = serde_json::json!(MINOR + 5);
    json["a_field_from_the_future"] = serde_json::json!({"nested": [1, 2, 3]});
    let bytes = serde_json::to_vec(&json).expect("serializes");
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&bytes).expect("write to a Vec");
    let payload = URL_SAFE_NO_PAD.encode(encoder.finish().expect("finish"));
    let state = readable(&format!("{MAJOR}.{payload}"));
    assert_eq!(state.minor, MINOR + 5);
    assert_eq!(state.applied, full_state().applied);
}

#[test]
fn max_encoded_is_the_cmd_line_cap_minus_the_set_wrapper() {
    let wrapper = "SET \"__OCX_ENV_STATE=\"";
    assert_eq!(wrapper.len(), 22);
    assert_eq!(MAX_ENCODED, 8_191 - wrapper.len());
    assert_eq!(MAX_ENCODED, 8_169);
}

#[test]
fn rung_one_drops_inherited_records_and_keeps_scopes() {
    // Rung 0 overflows, but the session records alone fit.
    let state = sized(8, 1, 1_000);
    assert!(state.applied.len() * 1_000 > MAX_ENCODED);
    let encoded = encode(&state);
    assert_eq!(encoded.truncated, Truncation::InheritedDropped);
    let value = encoded.value.expect("rung 1 fits");
    assert!(value.len() <= MAX_ENCODED);
    let back = readable(&value);
    assert_eq!(back.truncated, Truncation::InheritedDropped);
    assert_eq!(back.inherited_scopes, state.inherited_scopes);
    assert_eq!(back.applied.len(), 1);
    assert_eq!(back.applied[0].owner, session_owner());
    assert_eq!(back.narrowing, state.narrowing);
    assert_eq!(back.home, state.home);
}

#[test]
fn rung_one_drops_inherited_identities_too() {
    let mut state = sized(8, 0, 1_000);
    state.replay.launch_identities = vec![
        IdentityRecord {
            digest: format!("sha256:{}", "cd".repeat(32)),
            names: vec!["example.com/inherited:1".into()],
            owner: Owner::Inherited,
            no_patches: false,
        },
        IdentityRecord {
            digest: format!("sha256:{}", "ef".repeat(32)),
            names: vec!["example.com/own:1".into()],
            owner: session_owner(),
            no_patches: false,
        },
    ];
    let back = readable(&encode(&state).value.expect("fits"));
    assert_eq!(back.replay.launch_identities.len(), 1);
    assert_eq!(back.replay.launch_identities[0].owner, session_owner());
}

#[test]
fn rung_two_drops_every_applied_record_and_keeps_scopes_and_replay() {
    let mut state = sized(0, 9, 1_000);
    state.replay.env = vec![ReplayValue {
        key: "KEEP_ME".into(),
        kind: ValueKind::Constant,
        separator: None,
        value: ReplayPayload::Literal("v".into()),
    }];
    state.inherited_scopes = vec![Scope::Global, Scope::Project { root: abs("p") }];
    let encoded = encode(&state);
    assert_eq!(encoded.truncated, Truncation::AllAppliedDropped);
    let back = readable(&encoded.value.expect("rung 2 fits"));
    assert!(back.applied.is_empty());
    assert_eq!(back.truncated, Truncation::AllAppliedDropped);
    assert_eq!(back.inherited_scopes, state.inherited_scopes);
    assert_eq!(back.replay.env, state.replay.env);
}

/// EC-LEDGER-005: an over-cap state ships as a decodable header-only marker, never as nothing.
#[test]
fn rung_three_is_a_header_only_value_that_decodes_payload_dropped() {
    let state = replay_heavy();
    let encoded = encode(&state);
    assert_eq!(encoded.truncated, Truncation::PayloadDropped);
    let value = encoded.value.expect("the header fits");
    assert!(value.ends_with('.'));
    assert_eq!(encoded.encoded_len, value.len());
    let back = readable(&value);
    assert_eq!(back.truncated, Truncation::PayloadDropped);
    assert_eq!(back.narrowing, state.narrowing);
    assert_eq!(back.home, state.home);
    assert!(back.inherited_scopes.is_empty());
}

/// EC-LEDGER-006: a truncation stays reportable on every later prompt.
#[test]
fn sticky_truncation_survives_a_full_encode() {
    let state = ExecState {
        truncated: Truncation::InheritedDropped,
        ..ExecState::default()
    };
    let encoded = encode(&state);
    assert_eq!(encoded.truncated, Truncation::None);
    assert_eq!(
        readable(&encoded.value.expect("fits")).truncated,
        Truncation::InheritedDropped
    );
}

#[test]
fn the_ladder_never_exceeds_the_cap_whatever_the_size() {
    for (inherited, session, len) in [(0, 0, 0), (3, 3, 500), (8, 1, 1_000), (0, 9, 1_000), (30, 30, 2_000)] {
        let encoded = encode(&sized(inherited, session, len));
        let value = encoded.value.expect("a header always fits");
        assert!(
            value.len() <= MAX_ENCODED,
            "{inherited}/{session}/{len}: {}",
            value.len()
        );
        assert_eq!(encoded.encoded_len, value.len());
    }
}

/// A value that deflates to almost nothing but inflates past the decoder's bounds: the ladder must
/// treat the decoder's caps as its own, or every child reads the written state as `Unreadable`.
fn compressible(owner: Owner, records: usize, value_len: usize) -> ExecState {
    let mut state = sized(0, 0, 0);
    for index in 0..records {
        state
            .applied
            .push(constant(&format!("BULK_{index}"), "a".repeat(value_len), owner.clone()));
    }
    state
}

#[test]
fn a_state_over_the_inflated_cap_is_never_written_whole() {
    for (owner, rung) in [
        (Owner::Inherited, Truncation::InheritedDropped),
        (session_owner(), Truncation::AllAppliedDropped),
    ] {
        let state = compressible(owner, 200, 2_000);
        let json = serde_json::to_vec(&state).expect("serializes");
        assert!(json.len() > MAX_INFLATED, "fixture must exceed the inflated cap");
        let encoded = encode(&state);
        assert_eq!(encoded.truncated, rung);
        let value = encoded.value.expect("fits");
        assert!(
            matches!(decode(Some(std::ffi::OsStr::new(&value))), Inherited::Readable(_)),
            "{rung:?} wrote a value its own decoder refuses"
        );
    }
}

#[test]
fn a_state_over_the_record_cap_is_never_written_whole() {
    let mut state = sized(0, 0, 0);
    state.applied = vec![constant("SAME", String::new(), session_owner()); MAX_RECORDS + 1];
    let encoded = encode(&state);
    assert_eq!(encoded.truncated, Truncation::AllAppliedDropped);
    let value = encoded.value.expect("fits");
    assert!(matches!(
        decode(Some(std::ffi::OsStr::new(&value))),
        Inherited::Readable(_)
    ));
}

#[test]
fn scopes_above_the_limit_drop_the_oldest() {
    let state = ExecState {
        inherited_scopes: (0..MAX_INHERITED_SCOPES + 4)
            .map(|index| Scope::Project {
                root: abs(&format!("p{index}")),
            })
            .collect(),
        ..ExecState::default()
    };
    let back = readable(&encode(&state).value.expect("fits"));
    assert_eq!(back.inherited_scopes.len(), MAX_INHERITED_SCOPES);
    assert_eq!(back.inherited_scopes[0], state.inherited_scopes[4]);
}

#[test]
fn every_rung_stays_in_the_cmd_and_github_env_alphabet() {
    let states = [
        sized(0, 0, 0),
        sized(8, 1, 1_000),
        sized(0, 9, 1_000),
        sized(30, 30, 2_000),
        replay_heavy(),
        full_state(),
    ];
    let mut rungs = Vec::new();
    for mut state in states {
        // Every `n=` name set and a home that base64url-encodes to a non-trivial segment.
        for mask in 0u8..16 {
            state.narrowing = Narrowing {
                offline: mask & 1 != 0,
                frozen: mask & 2 != 0,
                no_consent: mask & 4 != 0,
                credential_helper: mask & 8 != 0,
            };
            for home in [None, Some(abs("home \u{e9}\"%dir"))] {
                state.home = home;
                let encoded = encode(&state);
                let value = encoded.value.expect("fits");
                assert!(in_alphabet(&value), "{value}");
                assert!(value.len() <= MAX_ENCODED);
                rungs.push(encoded.truncated);
            }
        }
    }
    for rung in [
        Truncation::None,
        Truncation::InheritedDropped,
        Truncation::AllAppliedDropped,
        Truncation::PayloadDropped,
    ] {
        assert!(rungs.contains(&rung), "rung {rung:?} was not exercised");
    }
}

#[test]
fn a_home_too_long_for_the_cap_is_dropped_alone() {
    let mut state = sized(0, 0, 0);
    state.home = Some(abs(&"d".repeat(MAX_ENCODED)));
    let encoded = encode(&state);
    let value = encoded.value.expect("the header without the home fits");
    assert!(value.len() <= MAX_ENCODED);
    assert!(!value.contains(";h="));
    assert!(value.split('.').next().is_some_and(|head| head.len() <= MAX_HEADER));
    assert_eq!(readable(&value).narrowing, state.narrowing);
}

#[test]
fn a_home_too_long_for_the_header_is_dropped_alone_and_the_value_stays_readable() {
    let mut state = full_state();
    state.home = Some(abs(&"d".repeat(1_800)));
    let value = encode(&state).value.expect("fits");
    assert!(value.split('.').next().is_some_and(|head| head.len() <= MAX_HEADER));
    let back = readable(&value);
    assert_eq!(back.home, None);
    assert_eq!(back.narrowing, state.narrowing);
    assert_eq!(back.applied, state.applied);
}

#[test]
fn key_spelling_folds_to_the_first_occurrence() {
    let mut state = ExecState {
        applied: vec![
            constant("Path_Like", "a".into(), Owner::Inherited),
            constant("PATH_LIKE", "b".into(), session_owner()),
            constant("other", "c".into(), Owner::Inherited),
        ],
        ..ExecState::default()
    };
    state.replay.env = vec![ReplayValue {
        key: "path_like".into(),
        kind: ValueKind::Constant,
        separator: None,
        value: ReplayPayload::Literal("v".into()),
    }];
    fold_key_spelling(&mut state);
    assert_eq!(state.applied[0].key, "Path_Like");
    assert_eq!(state.applied[1].key, "Path_Like");
    assert_eq!(state.applied[2].key, "other");
    assert_eq!(state.replay.env[0].key, "Path_Like");
}

#[cfg(unix)]
#[test]
fn fixture_decodes_to_its_json_and_matches_the_python_codec() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/exec_state/testdata");
    let value = std::fs::read_to_string(dir.join("state.encoded")).expect("fixture value");
    let json = std::fs::read(dir.join("state.json")).expect("fixture json");
    let mut expected: ExecState = serde_json::from_slice(&json).expect("fixture json is a payload");
    expected.narrowing = Narrowing {
        offline: true,
        frozen: true,
        ..Narrowing::default()
    };
    expected.home = Some(PathBuf::from("/opt/ocx"));
    assert_eq!(readable(value.trim()), expected);
    // What this encoder writes for the same state decodes back to it.
    assert_eq!(readable(&encode(&expected).value.expect("fits")), expected);
}

/// Hex text (4 bits per char) like the latency fixture's `incompressible_text`: zlib folds it to about half.
fn hex_text(seed: u64, len: usize) -> String {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            char::from(b"0123456789abcdef"[usize::try_from(x >> 60).unwrap_or(0)])
        })
        .collect()
}

/// The cap-sized state of `test/bench/shell_latency.py` `build_cap_state`: inherited hex constants of 96
/// chars, grown a record at a time until the next would pass `MAX_ENCODED - 64`.
fn cap_state(level: Compression) -> ExecState {
    let mut state = sized(0, 0, 0);
    let mut index = 0;
    loop {
        let mut grown = state.clone();
        grown.applied.push(constant(
            &format!("LATENCY_CAP_{index:03}"),
            hex_text(index, 96),
            Owner::Inherited,
        ));
        let len = assemble(
            &header::emit(MAJOR, &grown.narrowing, grown.home.as_deref()),
            &grown,
            level,
        )
        .map_or(usize::MAX, |value| value.len());
        if len > MAX_ENCODED - 64 {
            return state;
        }
        state = grown;
        index += 1;
    }
}

/// A project scope plus the global scope, as a shell hook records them: per-package `PATH`-style
/// records whose paths carry a 64-hex digest each, constants, and a few replay values.
fn realistic_state(packages: usize) -> ExecState {
    let mut state = sized(0, 0, 0);
    state.inherited_scopes = vec![
        Scope::Global,
        Scope::Project {
            root: abs("work/project"),
        },
    ];
    for index in 0..packages {
        let digest = hex_text(10_000 + index as u64, 64);
        let owner = if index % 2 == 0 {
            session_owner()
        } else {
            Owner::Inherited
        };
        state.applied.push(AppliedValue {
            key: if index % 3 == 0 {
                "PATH".to_owned()
            } else {
                format!("TOOL_{index}_HOME")
            },
            value: RecordedValue::Literal(format!(
                "/home/user/.ocx/packages/ocx.sh/tool-{index}/sha256/{}/{}/content/bin",
                &digest[..2],
                &digest[2..]
            )),
            kind: ValueKind::Path,
            separator: None,
            pre_existing: false,
            owner,
            prior: None,
        });
    }
    state
}

/// The fixtures the level is judged on, by name. `cap_state` is grown at level 9, the strongest
/// level, so a level that does worse on it is genuinely worse and not merely tuned to itself.
fn level_fixtures() -> Vec<(&'static str, ExecState, bool)> {
    vec![
        ("realistic 6", realistic_state(6), true),
        ("realistic 20", realistic_state(20), true),
        ("realistic 40", realistic_state(40), true),
        ("full_state", full_state(), true),
        ("sized 8/1/1000", sized(8, 1, 1_000), false),
        ("sized 0/9/1000", sized(0, 9, 1_000), false),
        ("sized 3/3/500", sized(3, 3, 500), false),
        ("sized 30/30/2000", sized(30, 30, 2_000), false),
        ("replay_heavy", replay_heavy(), false),
        ("cap_state", cap_state(Compression::new(9)), false),
    ]
}

fn rung_order(rung: Truncation) -> u8 {
    match rung {
        Truncation::None => 0,
        Truncation::InheritedDropped => 1,
        Truncation::AllAppliedDropped => 2,
        Truncation::PayloadDropped => 3,
    }
}

/// `ZLIB_LEVEL` must never push a state onto a worse rung than the strongest level gives it, and on
/// realistic states it may cost at most the stated margin in length. Timing is out of scope here.
#[test]
fn the_zlib_level_never_lands_a_state_on_a_worse_rung_than_level_nine() {
    const MARGIN_PERCENT: usize = 15;
    for (name, state, realistic) in level_fixtures() {
        let chosen = encode_at(&state, ZLIB_LEVEL);
        let best = encode_at(&state, Compression::new(9));
        assert!(
            rung_order(chosen.truncated) <= rung_order(best.truncated),
            "{name}: level {ZLIB_LEVEL:?} lands on {:?}, level 9 on {:?}",
            chosen.truncated,
            best.truncated
        );
        if realistic {
            assert_eq!(chosen.truncated, Truncation::None, "{name} must fit whole");
            assert!(
                chosen.encoded_len * 100 <= best.encoded_len * (100 + MARGIN_PERCENT),
                "{name}: {} chars at the chosen level against {} at level 9",
                chosen.encoded_len,
                best.encoded_len
            );
        }
    }
}

/// Prints the length and rung of every fixture per level; asserts nothing.
/// `cargo test --release -p ocx_config zlib_level_table -- --ignored --nocapture`
#[test]
#[ignore = "measurement, run by hand in release"]
fn zlib_level_table() {
    for level in [1, 6, 9] {
        for (name, state, _) in level_fixtures() {
            let encoded = encode_at(&state, Compression::new(level));
            println!(
                "level {level} {name}: rung {:?} len {}",
                encoded.truncated, encoded.encoded_len
            );
        }
    }
}

/// EC-LEDGER-012, EC-QUOTE-014: values holding shell syntax travel as data. The encoded text never needs
/// quoting, and nothing is escaped before encoding, so decoding returns the bytes that went in.
#[test]
fn hostile_values_round_trip_byte_identically_in_the_value_alphabet() {
    let hostile = [
        "it's",
        "a\"b",
        "$(touch x)",
        "`id`",
        "line\nbreak",
        "back\\slash",
        "100%",
        "!bang",
        "\u{e9}\u{202e}",
        "'; rm -rf / #",
    ];
    let state = ExecState {
        applied: hostile
            .iter()
            .enumerate()
            .map(|(index, value)| constant(&format!("HOSTILE_{index}"), (*value).to_owned(), Owner::Inherited))
            .collect(),
        ..ExecState::default()
    };
    let value = encode(&state).value.expect("a small state fits");
    assert!(in_alphabet(&value), "{value}");
    assert_eq!(readable(&value), state);
}
