// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::Write as _;
use std::path::PathBuf;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::*;
use crate::exec_state::types::{
    IdentityRecord, MAX_HEADER, Owner, Prior, ProjectScopeHint, RecordedValue, ReplayPayload, ReplayValue, ScopeId,
    SessionSource,
};

const SECRET: &str = "fixture-secret-value";

fn abs(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

fn deflate(json: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(json).expect("write to a Vec");
    encoder.finish().expect("finish a Vec encoder")
}

fn payload_of(json: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(deflate(json))
}

fn value_of_json(json: &str) -> String {
    format!("{MAJOR}.{}", payload_of(json.as_bytes()))
}

fn value_of(state: &ExecState) -> String {
    let json = serde_json::to_vec(state).expect("state serializes");
    format!(
        "{}.{}",
        header::emit(MAJOR, &state.narrowing, state.home.as_deref()),
        payload_of(&json)
    )
}

fn decoded(value: &str) -> Inherited {
    decode(Some(OsStr::new(value)))
}

fn reason(inherited: Inherited) -> UnreadableReason {
    match inherited {
        Inherited::Unreadable { reason, .. } => reason,
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

fn readable(inherited: Inherited) -> ExecState {
    match inherited {
        Inherited::Readable(state) => state,
        other => panic!("expected Readable, got {other:?}"),
    }
}

fn record(key: &str, kind: ValueKind) -> AppliedValue {
    AppliedValue {
        key: key.to_owned(),
        value: RecordedValue::Literal("v".to_owned()),
        kind,
        separator: (kind == ValueKind::List).then(|| ":".to_owned()),
        pre_existing: false,
        owner: Owner::Inherited,
        prior: (kind == ValueKind::Constant).then_some(Prior::Unset),
    }
}

fn with_applied(record: AppliedValue) -> ExecState {
    ExecState {
        applied: vec![record],
        ..ExecState::default()
    }
}

fn invalid_field(state: &ExecState) -> &'static str {
    match reason(decoded(&value_of(state))) {
        UnreadableReason::Invalid { field } => field,
        other => panic!("expected Invalid, got {other:?}"),
    }
}

fn offline_header() -> String {
    format!("{MAJOR};n=offline")
}

// ── steps 1 and 2 ────────────────────────────────────────────────────────

#[test]
fn unset_and_empty_are_absent() {
    assert_eq!(decode(None), Inherited::Absent);
    assert_eq!(decode(Some(OsStr::new(""))), Inherited::Absent);
}

#[test]
fn no_dot_within_the_header_bound_is_header_invalid_and_keeps_narrowing() {
    let value = format!("{};{}", offline_header(), "z".repeat(MAX_HEADER));
    let inherited = decoded(&value);
    assert!(inherited.narrowing().offline);
    assert_eq!(reason(inherited), UnreadableReason::HeaderInvalid);
}

#[test]
fn eleven_digit_major_is_header_invalid() {
    assert_eq!(
        reason(decoded("12345678901;n=frozen.")),
        UnreadableReason::HeaderInvalid
    );
    assert!(decoded("12345678901;n=frozen.").narrowing().frozen);
}

#[test]
fn bad_home_is_dropped_alone() {
    let state = readable(decoded(&format!("{MAJOR};n=frozen;h=!!!.")));
    assert!(state.narrowing.frozen);
    assert_eq!(state.home, None);
}

// ── steps 3 and 4 ────────────────────────────────────────────────────────

/// EC-LEDGER-004: a value under another major is unreadable without decoding its payload, and its header is still honoured.
#[test]
fn other_major_is_unreadable_with_the_header_honoured() {
    let inherited = decoded("3;n=offline,frozen.whatever");
    assert!(inherited.narrowing().offline && inherited.narrowing().frozen);
    assert_eq!(reason(inherited), UnreadableReason::OtherMajor);
}

#[test]
fn over_length_value_is_too_long_and_still_yields_offline() {
    let value = format!("{}.{}", offline_header(), "A".repeat(MAX_ENCODED));
    let inherited = decoded(&value);
    assert!(inherited.narrowing().offline);
    assert_eq!(reason(inherited), UnreadableReason::TooLong);
}

#[cfg(unix)]
#[test]
fn non_utf8_value_is_not_utf8_and_still_yields_offline() {
    use std::os::unix::ffi::OsStrExt as _;
    let mut bytes = format!("{}.", offline_header()).into_bytes();
    bytes.extend_from_slice(&[0xff, 0xfe, 0x80]);
    let inherited = decode(Some(OsStr::from_bytes(&bytes)));
    assert!(inherited.narrowing().offline);
    assert_eq!(reason(inherited), UnreadableReason::NotUtf8);
}

// ── step 5 ───────────────────────────────────────────────────────────────

#[test]
fn rung_three_marker_is_readable_with_the_payload_dropped() {
    let home = abs("home");
    let header = header::emit(
        MAJOR,
        &Narrowing {
            offline: true,
            ..Narrowing::default()
        },
        Some(&home),
    );
    let state = readable(decoded(&format!("{header}.")));
    assert_eq!(state.truncated, Truncation::PayloadDropped);
    assert!(state.narrowing.offline);
    assert_eq!(state.home, Some(home));
    assert!(state.applied.is_empty());
}

// ── steps 6 and 7 ────────────────────────────────────────────────────────

#[test]
fn padded_base64_is_refused() {
    let padded = format!("{}=", payload_of(b"{}"));
    assert_eq!(reason(decoded(&format!("{MAJOR}.{padded}"))), UnreadableReason::Base64);
}

#[test]
fn non_base64_payload_is_refused() {
    assert_eq!(reason(decoded(&format!("{MAJOR}.a+b/c"))), UnreadableReason::Base64);
}

#[test]
fn garbage_zlib_is_an_inflate_error() {
    let payload = URL_SAFE_NO_PAD.encode(b"definitely not a zlib stream");
    assert_eq!(
        reason(decoded(&format!("{MAJOR}.{payload}"))),
        UnreadableReason::Inflate
    );
}

#[test]
fn truncated_zlib_is_an_inflate_error() {
    let mut compressed = deflate(b"{\"minor\":0,\"pad\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}");
    compressed.truncate(compressed.len() - 6);
    let payload = URL_SAFE_NO_PAD.encode(compressed);
    assert_eq!(
        reason(decoded(&format!("{MAJOR}.{payload}"))),
        UnreadableReason::Inflate
    );
}

#[test]
fn zlib_bomb_hits_the_inflate_limit_without_inflating_it_whole() {
    // Zeros deflate roughly a thousand to one, so a value inside `MAX_ENCODED` can claim several megabytes.
    let mut json = b"{\"pad\":\"".to_vec();
    json.extend(std::iter::repeat_n(b'a', 4 * 1024 * 1024));
    json.extend_from_slice(b"\"}");
    let value = value_of_json_bytes(&json);
    assert!(
        value.len() <= MAX_ENCODED,
        "fixture must fit the value cap: {}",
        value.len()
    );
    assert_eq!(reason(decoded(&value)), UnreadableReason::InflateLimit);
}

fn value_of_json_bytes(json: &[u8]) -> String {
    format!("{MAJOR}.{}", payload_of(json))
}

// Unbounded, the decoder runs into the cut-off end and reports `Inflate` instead.
#[test]
fn a_bomb_stops_at_the_inflate_limit_before_it_reaches_a_corrupt_tail() {
    let mut json = b"{\"pad\":\"".to_vec();
    json.extend(std::iter::repeat_n(b'a', 4 * 1024 * 1024));
    let mut compressed = deflate(&json);
    compressed.truncate(compressed.len() - 8);
    let value = format!("{MAJOR}.{}", URL_SAFE_NO_PAD.encode(compressed));
    assert!(
        value.len() <= MAX_ENCODED,
        "fixture must fit the value cap: {}",
        value.len()
    );
    assert_eq!(reason(decoded(&value)), UnreadableReason::InflateLimit);
}

#[test]
fn bytes_after_the_zlib_stream_are_trailing_bytes() {
    let mut compressed = deflate(b"{}");
    compressed.extend_from_slice(b"junk");
    let payload = URL_SAFE_NO_PAD.encode(compressed);
    assert_eq!(
        reason(decoded(&format!("{MAJOR}.{payload}"))),
        UnreadableReason::TrailingBytes
    );
}

// ── step 8 ───────────────────────────────────────────────────────────────

#[test]
fn json_errors_are_refused() {
    assert_eq!(reason(decoded(&value_of_json("not json"))), UnreadableReason::Json);
}

#[test]
fn duplicate_fields_are_refused() {
    assert_eq!(
        reason(decoded(&value_of_json(r#"{"minor":1,"minor":2}"#))),
        UnreadableReason::Json
    );
}

#[test]
fn an_activate_verdict_is_refused() {
    let json = r#"{"session":{"verdict":"activate"}}"#;
    assert_eq!(reason(decoded(&value_of_json(json))), UnreadableReason::Json);
}

#[test]
fn nesting_inside_a_known_field_is_refused() {
    let json = format!(r#"{{"policy":{{"mirrors":{}{}}}}}"#, "[".repeat(300), "]".repeat(300));
    assert_eq!(reason(decoded(&value_of_json(&json))), UnreadableReason::Json);
}

// serde_json skips an unknown value iteratively, so deep nesting there costs no stack; the inflate bound is its cap.
#[test]
fn nesting_inside_an_unknown_field_is_skipped_without_exhausting_the_stack() {
    let json = format!(r#"{{"x":{}{}}}"#, "[".repeat(4000), "]".repeat(4000));
    assert!(matches!(decoded(&value_of_json(&json)), Inherited::Readable(_)));
}

// ── step 9 ───────────────────────────────────────────────────────────────

#[test]
fn more_than_max_records_is_a_record_limit() {
    let one = ReplayValue {
        key: "A".to_owned(),
        kind: ValueKind::Path,
        separator: None,
        value: ReplayPayload::Literal("v".to_owned()),
    };
    let mut state = ExecState::default();
    state.replay.env = vec![one; MAX_RECORDS + 1];
    assert_eq!(reason(decoded(&value_of(&state))), UnreadableReason::RecordLimit);
}

#[test]
fn nine_tiers_are_a_tier_limit() {
    let mut state = ExecState::default();
    state.session.tiers = (0..=MAX_RECORDED_TIERS).map(|n| abs(&format!("t{n}"))).collect();
    assert_eq!(reason(decoded(&value_of(&state))), UnreadableReason::TierLimit);
}

#[test]
fn seventeen_scopes_are_a_scope_limit() {
    let state = ExecState {
        inherited_scopes: (0..=MAX_INHERITED_SCOPES)
            .map(|n| Scope::Project {
                root: abs(&format!("p{n}")),
            })
            .collect(),
        ..ExecState::default()
    };
    assert_eq!(reason(decoded(&value_of(&state))), UnreadableReason::ScopeLimit);
}

// ── step 10 ──────────────────────────────────────────────────────────────

#[test]
fn a_relative_policy_path_is_invalid() {
    let mut state = ExecState::default();
    state.policy.config_path = Some(PathBuf::from("relative/config.toml"));
    assert_eq!(invalid_field(&state), "policy.config_path");
}

/// EC-REC-006: a record whose key is not a valid variable name never enters the state.
#[test]
fn an_invalid_env_key_is_invalid() {
    assert_eq!(
        invalid_field(&with_applied(record("BAD KEY", ValueKind::Constant))),
        "applied.key"
    );
}

/// EC-LEDGER-007: a forged record claiming `PATH` as a constant is refused at decode.
#[test]
fn path_as_a_constant_is_invalid() {
    assert_eq!(
        invalid_field(&with_applied(record("PATH", ValueKind::Constant))),
        "applied.key"
    );
    assert!(matches!(
        decoded(&value_of(&with_applied(record("PATH", ValueKind::Path)))),
        Inherited::Readable(_)
    ));
}

/// A forged applied record for a reserved key could be restored as a prior by the hook.
#[test]
fn a_reserved_applied_key_is_invalid() {
    for key in ["OCX_OFFLINE", "ocx_offline", "__OCX_ENV_STATE"] {
        assert_eq!(
            invalid_field(&with_applied(record(key, ValueKind::Constant))),
            "applied.key",
            "{key}"
        );
    }
}

#[test]
fn a_list_without_a_separator_is_invalid() {
    let mut bad = record("LIST_VAR", ValueKind::List);
    bad.separator = Some(String::new());
    assert_eq!(invalid_field(&with_applied(bad)), "applied.separator");
}

#[test]
fn a_prior_outside_a_constant_is_invalid() {
    let mut bad = record("DIR_VAR", ValueKind::Path);
    bad.prior = Some(Prior::Unset);
    assert_eq!(invalid_field(&with_applied(bad)), "applied.prior");
}

/// Reserved keys fold case, as the environment does on Windows, so the lowercase spelling is
/// refused too.
#[test]
fn a_reserved_replay_key_is_invalid() {
    for key in reserved_probe_keys() {
        let mut state = ExecState::default();
        state.replay.env.push(ReplayValue {
            key: key.clone(),
            kind: ValueKind::Constant,
            separator: None,
            value: ReplayPayload::Literal("1".to_owned()),
        });
        assert_eq!(invalid_field(&state), "replay.env.key", "{key}");
    }
}

#[test]
fn path_as_a_replay_constant_is_invalid() {
    let mut state = ExecState::default();
    state.replay.env.push(ReplayValue {
        key: "PATH".to_owned(),
        kind: ValueKind::Constant,
        separator: None,
        value: ReplayPayload::Literal(abs("bin").to_string_lossy().into_owned()),
    });
    assert_eq!(invalid_field(&state), "replay.env.key");
}

#[test]
fn a_records_name_that_is_not_a_filename_is_invalid() {
    for name in ["../{time}.json", "sub/{time}.json", "sub\\{time}.json", ".", ".."] {
        let mut state = ExecState::default();
        state.policy.records_name = Some(name.to_owned());
        assert_eq!(invalid_field(&state), "policy.records_name", "{name}");
    }
    let mut state = ExecState::default();
    state.policy.records_name = Some("{time}-{pid}.json".to_owned());
    assert!(matches!(decoded(&value_of(&state)), Inherited::Readable(_)));
}

/// Both record writers skip credential slots, so a record on one is forged: reverting it would
/// drop a credential the user supplied.
#[test]
fn an_applied_record_on_a_credential_slot_is_invalid() {
    for key in reserved_probe_keys() {
        let mut bad = record(&key, ValueKind::Constant);
        bad.value = RecordedValue::Literal(SECRET.to_owned());
        bad.prior = Some(Prior::Unset);
        assert_eq!(invalid_field(&with_applied(bad)), "applied.key", "{key}");
    }
}

/// Every credential key, an `OCX_AUTH_*` key and a plain `OCX_*` key, each in both cases.
fn reserved_probe_keys() -> Vec<String> {
    ocx_env::credential_keys()
        .chain(["OCX_AUTH_EXAMPLE_TOKEN", "OCX_OFFLINE"])
        .flat_map(|key| [key.to_owned(), key.to_ascii_lowercase()])
        .collect()
}

#[test]
fn a_mirror_url_with_userinfo_is_invalid() {
    let mut state = ExecState::default();
    state.policy.mirrors = Some(format!(r#"{{"ghcr.io":"https://user:{SECRET}@mirror.example"}}"#));
    assert_eq!(invalid_field(&state), "policy.mirrors");
}

#[test]
fn a_clean_mirror_policy_decodes() {
    let mut state = ExecState::default();
    state.policy.mirrors = Some(r#"{"ghcr.io":"https://mirror.example/prefix"}"#.to_owned());
    assert!(matches!(decoded(&value_of(&state)), Inherited::Readable(_)));
}

#[test]
fn a_malformed_digest_is_invalid() {
    let mut state = ExecState::default();
    state.replay.launch_identities.push(IdentityRecord {
        digest: "sha256:abc".to_owned(),
        names: vec!["example.com/tool:1".to_owned()],
        owner: Owner::Inherited,
        no_patches: false,
    });
    assert_eq!(invalid_field(&state), "replay.launch_identities.digest");
}

#[test]
fn a_bad_group_name_is_invalid() {
    let state = ExecState {
        selection: Selection::Pinned(Pinned {
            tier: PinnedTier::Global,
            groups: vec!["../escape".to_owned()],
            with_names: Vec::new(),
            env_keys: Vec::new(),
            invocation_digest: "0".repeat(64),
            pinned_paths: None,
        }),
        ..ExecState::default()
    };
    assert_eq!(invalid_field(&state), "selection.pinned.groups");
}

#[test]
fn a_relative_project_hint_dir_is_invalid() {
    let mut state = ExecState::default();
    state.session.project = Some(ProjectScopeHint {
        key: "0".repeat(16),
        dir: PathBuf::from("relative"),
    });
    assert_eq!(invalid_field(&state), "session.project.dir");
}

// ── no downgrade step ────────────────────────────────────────────────────

#[test]
fn a_literal_under_a_secret_named_key_or_a_url_with_userinfo_decodes_verbatim() {
    let mut secret = record("MY_SERVICE_TOKEN", ValueKind::Constant);
    secret.value = RecordedValue::Literal(SECRET.to_owned());
    secret.prior = Some(Prior::Value(SECRET.to_owned()));
    let mut url = record("REMOTE_URL", ValueKind::Constant);
    url.value = RecordedValue::Literal(format!("https://user:{SECRET}@host.example/repo"));
    let mut state = with_applied(secret.clone());
    state.applied.push(url.clone());
    let replay = ReplayValue {
        key: "MY_SERVICE_TOKEN".to_owned(),
        kind: ValueKind::Constant,
        separator: None,
        value: ReplayPayload::Literal(SECRET.to_owned()),
    };
    state.replay.env.push(replay.clone());
    let decoded_state = readable(decoded(&value_of(&state)));
    assert_eq!(decoded_state.applied, vec![secret, url]);
    assert_eq!(decoded_state.replay.env, vec![replay]);
}

#[test]
fn an_ordinary_literal_is_kept() {
    let kept = readable(decoded(&value_of(&with_applied(record(
        "JAVA_HOME",
        ValueKind::Constant,
    )))));
    assert_eq!(kept.applied[0].value, RecordedValue::Literal("v".to_owned()));
}

// ── readable states and versioning ───────────────────────────────────────

#[test]
fn a_full_state_round_trips_with_header_fields() {
    let mut state = ExecState {
        narrowing: Narrowing {
            offline: true,
            no_consent: true,
            ..Narrowing::default()
        },
        home: Some(abs("ocx-home")),
        ..ExecState::default()
    };
    state.applied.push(AppliedValue {
        owner: Owner::Session {
            scope: ScopeId::Global,
            source: SessionSource::Composed,
        },
        ..record("JAVA_HOME", ValueKind::Constant)
    });
    state.inherited_scopes.push(Scope::Global);
    assert_eq!(readable(decoded(&value_of(&state))), state);
}

#[test]
fn an_unknown_field_and_a_higher_minor_decode() {
    let state = readable(decoded(&value_of_json(r#"{"minor":9,"future_field":{"a":[1,2]}}"#)));
    assert_eq!(state.minor, 9);
}

// ── Inherited accessors ──────────────────────────────────────────────────

#[test]
fn credential_helper_implies_offline_for_readable_and_unreadable() {
    let readable_state = decoded(&format!("{MAJOR};n=credential_helper."));
    let unreadable_state = decoded("9;n=credential_helper.x");
    for inherited in [readable_state, unreadable_state] {
        let narrowing = inherited.narrowing();
        assert!(narrowing.credential_helper && narrowing.offline);
    }
}

#[test]
fn absent_has_default_narrowing_and_no_home() {
    assert_eq!(Inherited::Absent.narrowing(), Narrowing::default());
    assert_eq!(Inherited::Absent.home(), None);
}

#[test]
fn home_is_honoured_for_readable_and_unreadable() {
    let home = abs("ocx-home");
    let header = header::emit(MAJOR, &Narrowing::default(), Some(&home));
    assert_eq!(decoded(&format!("{header}.")).home(), Some(home.as_path()));
    let other_major = header::emit(MAJOR + 1, &Narrowing::default(), Some(&home));
    let inherited = decoded(&format!("{other_major}.x"));
    assert_eq!(inherited.home(), Some(home.as_path()));
    assert!(matches!(inherited, Inherited::Unreadable { .. }));
}

#[test]
fn from_process_env_reads_through_the_seam() {
    let guard = ocx_env::overrides::lock();
    guard.remove(&ocx_env::__OCX_ENV_STATE);
    assert_eq!(Inherited::from_process_env(), Inherited::Absent);
    guard.set(&ocx_env::__OCX_ENV_STATE, format!("{}.", offline_header()));
    let inherited = Inherited::from_process_env();
    assert!(inherited.narrowing().offline);
    assert!(matches!(inherited, Inherited::Readable(_)));
}

/// EC-LEDGER-003: a value cut short by a platform environment-block limit never panics the decoder.
#[test]
fn a_value_truncated_at_any_byte_gets_a_verdict_and_never_panics() {
    let state = with_applied(record("FOO", ValueKind::Constant));
    let value = value_of(&state);
    for end in 0..value.len() {
        match decoded(&value[..end]) {
            Inherited::Absent => assert_eq!(end, 0, "only the empty value is absent"),
            Inherited::Unreadable { .. } => {}
            Inherited::Readable(cut) => assert_ne!(cut, state, "a cut value at {end} must not decode as the whole"),
        }
    }
}

/// The one-entry memo is invisible: a repeat answers as the uncached decode does, and a different
/// value (readable, then unreadable, then absent) is never served from the previous entry.
#[test]
fn the_repeat_decode_memo_never_serves_a_stale_value() {
    let first = value_of(&ExecState::default());
    let second = value_of(&ExecState {
        home: Some(abs("memo-home")),
        ..ExecState::default()
    });
    for value in [&first, &first, &second, &first, &second] {
        assert_eq!(
            decode(Some(OsStr::new(value))),
            decode_uncached(Some(OsStr::new(value)))
        );
    }
    let junk = OsStr::new("not-a-state");
    assert_eq!(decode(Some(junk)), decode_uncached(Some(junk)));
    assert_eq!(decode(None), Inherited::Absent);
}

/// Every fixture under `testdata/` decodes the same through the memo, cold and warm, as without it.
#[test]
fn the_memoized_decode_equals_the_uncached_decode_over_every_fixture() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/exec_state/testdata");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).expect("testdata exists") {
        let path = entry.expect("readable entry").path();
        let text = std::fs::read_to_string(&path).expect("fixture is text");
        let value = match path.extension().and_then(|e| e.to_str()) {
            Some("encoded") => text.trim().to_owned(),
            Some("json") => value_of_json(&text),
            other => panic!("unexpected fixture kind {other:?} at {}", path.display()),
        };
        let os = OsStr::new(&value);
        let uncached = decode_uncached(Some(os));
        assert_eq!(decode(Some(os)), uncached, "cold {}", path.display());
        assert_eq!(decode(Some(os)), uncached, "warm {}", path.display());
        seen += 1;
    }
    assert!(seen >= 2, "the reader found {seen} fixtures");
}

/// A value that differs in one byte from the memoized one is decoded afresh.
#[test]
fn a_changed_raw_value_never_returns_the_previous_result() {
    let mut state = with_applied(record("FOO", ValueKind::Constant));
    let before = value_of(&state);
    assert_eq!(readable(decoded(&before)), state);
    state.applied[0].value = RecordedValue::Literal("w".to_owned());
    let after = value_of(&state);
    assert_ne!(before, after);
    assert_eq!(readable(decoded(&after)), state);
}
