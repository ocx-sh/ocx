// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::ffi::OsString;

use crate::*;

#[test]
fn get_answers_the_value() {
    let env = overrides::lock();
    env.set(&OCX_LOG_LEVEL, "debug");
    assert_eq!(OCX_LOG_LEVEL.get().as_deref(), Some("debug"));
    assert_eq!(OCX_LOG_LEVEL.get_os(), Some(OsString::from("debug")));
    assert_eq!(OCX_LOG_LEVEL.get_raw(), Some(OsString::from("debug")));
}

#[test]
fn get_treats_unset_and_empty_alike_but_get_raw_keeps_empty() {
    let env = overrides::lock();
    env.remove(&OCX_LOG_LEVEL);
    assert_eq!(OCX_LOG_LEVEL.get(), None);
    assert_eq!(OCX_LOG_LEVEL.get_os(), None);
    assert_eq!(OCX_LOG_LEVEL.get_raw(), None);
    env.set(&OCX_LOG_LEVEL, "");
    assert_eq!(OCX_LOG_LEVEL.get(), None);
    assert_eq!(OCX_LOG_LEVEL.get_os(), None);
    assert_eq!(OCX_LOG_LEVEL.get_raw(), Some(OsString::new()));
}

#[test]
fn hermetic_lock_reads_unset_for_a_variable_it_does_not_hold() {
    let env = overrides::lock();
    env.hermetic(std::env::temp_dir());
    assert_eq!(PATH.get_raw(), None);
}

#[test]
fn secret_declaration_answers_only_through_sensitive() {
    let env = overrides::lock();
    env.set(OCX_IDENTITY_TOKEN.declaration(), "hunter2");
    assert_eq!(OCX_IDENTITY_TOKEN.declaration().get(), None);
    assert_eq!(OCX_IDENTITY_TOKEN.declaration().get_os(), None);
    assert_eq!(OCX_IDENTITY_TOKEN.declaration().get_raw(), None);
    let secret = OCX_IDENTITY_TOKEN.get().unwrap().map(|value| value.expose().to_owned());
    assert_eq!(secret.as_deref(), Some("hunter2"));
    env.set(OCX_IDENTITY_TOKEN.declaration(), "");
    assert!(OCX_IDENTITY_TOKEN.get().unwrap().is_none());
}

#[test]
fn sensitive_debug_is_redacted() {
    let secret = Sensitive::new("hunter2".into());
    assert_eq!(format!("{secret:?}"), "<redacted>");
    assert_eq!(format!("{:?}", Some(secret)), "Some(<redacted>)");
}

#[test]
fn get_slot_fills_the_pattern() {
    let env = overrides::lock();
    env.set_raw("OCX_AUTH_GHCR_IO_USER", "octocat");
    env.set_raw("OCX_AUTH_GHCR_IO_TOKEN", "hunter2");
    assert_eq!(OCX_AUTH_USER.get_slot("GHCR_IO").unwrap().as_deref(), Some("octocat"));
    let token = OCX_AUTH_TOKEN
        .get_slot("GHCR_IO")
        .unwrap()
        .map(|value| value.expose().to_owned());
    assert_eq!(token.as_deref(), Some("hunter2"));
    assert_eq!(OCX_AUTH_USER.get_slot("DOCKER_IO").unwrap(), None);
    env.set_raw("OCX_AUTH_EMPTY_USER", "");
    assert_eq!(OCX_AUTH_USER.get_slot("EMPTY").unwrap(), None);
}

#[test]
fn a_pattern_has_no_plain_value_and_a_literal_has_no_slot() {
    let env = overrides::lock();
    env.set(OCX_AUTH_USER.declaration(), "literal-braces");
    env.set(&OCX_LOG_LEVEL, "debug");
    assert_eq!(OCX_AUTH_USER.get().unwrap(), None);
    assert_eq!(OCX_LOG_LEVEL.get_slot("ANY"), None);
    assert!(OCX_AUTH_USER.declaration().is_pattern());
    assert!(!OCX_LOG_LEVEL.is_pattern());
}

#[test]
fn bool_or_parses_the_boolean_vocabulary_case_insensitively() {
    let env = overrides::lock();
    for spelling in ["1", "y", "yes", "on", "true", "TRUE", "Yes", "ON"] {
        env.set(&OCX_REMOTE, spelling);
        assert_eq!(OCX_REMOTE.bool_or(false), Ok(true), "{spelling}");
    }
    for spelling in ["0", "n", "no", "off", "false", "FALSE", "No", "Off"] {
        env.set(&OCX_REMOTE, spelling);
        assert_eq!(OCX_REMOTE.bool_or(true), Ok(false), "{spelling}");
    }
}

#[test]
fn bool_or_answers_the_default_when_unset_or_empty() {
    let env = overrides::lock();
    env.remove(&OCX_OFFLINE);
    assert_eq!(OCX_OFFLINE.bool_or(true), Ok(true));
    assert_eq!(OCX_OFFLINE.bool_or(false), Ok(false));
    env.set(&OCX_OFFLINE, "");
    assert_eq!(OCX_OFFLINE.bool_or(true), Ok(true));
}

#[test]
fn bool_or_falls_back_on_an_invalid_value_unless_the_declaration_refuses() {
    let env = overrides::lock();
    env.set(&OCX_REMOTE, "maybe");
    assert_eq!(OCX_REMOTE.bool_or(true), Ok(true));
    assert_eq!(OCX_REMOTE.bool_or(false), Ok(false));
    env.set(&OCX_OFFLINE, "maybe");
    let error = OCX_OFFLINE.bool_or(false).unwrap_err();
    assert_eq!(error.key, "OCX_OFFLINE");
    assert!(!error.to_string().contains("maybe"), "{error}");
    assert!(error.to_string().contains("OCX_OFFLINE"), "{error}");
}

#[test]
fn dynamic_reads_verbatim_and_refuses_impossible_names() {
    let env = overrides::lock();
    env.set_raw("SOME_PACKAGE_VAR", "");
    assert_eq!(dynamic("SOME_PACKAGE_VAR").as_deref(), Some(""));
    env.set_raw("SOME_PACKAGE_VAR", " spaced ");
    assert_eq!(dynamic("SOME_PACKAGE_VAR").as_deref(), Some(" spaced "));
    env.remove_raw("SOME_PACKAGE_VAR");
    assert_eq!(dynamic("SOME_PACKAGE_VAR"), None);
    for name in ["", "A=B", "A\0B"] {
        assert_eq!(dynamic(name), None, "{name:?}");
    }
}

#[test]
fn dynamic_refuses_a_declared_secret_that_dynamic_secret_reads() {
    let env = overrides::lock();
    env.set(OCX_SIGNING_KEY.declaration(), "pem");
    env.set_raw("ocx_signing_key", "lowercase pem");
    env.set(CI_JOB_TOKEN.declaration(), "job");
    env.set_raw("OCX_AUTH_GHCR_IO_TOKEN", "hunter2");
    env.set_raw("OCX_AUTH_GHCR_IO_USER", "octocat");
    for secret in [
        "OCX_SIGNING_KEY",
        "ocx_signing_key",
        "CI_JOB_TOKEN",
        "OCX_AUTH_GHCR_IO_TOKEN",
    ] {
        assert_eq!(dynamic(secret), None, "{secret}");
    }
    assert_eq!(dynamic("OCX_AUTH_GHCR_IO_USER"), None);
    let read = |name| dynamic_secret(name).map(Sensitive::into_inner);
    assert_eq!(read("OCX_SIGNING_KEY").as_deref(), Some("pem"));
    assert_eq!(read("OCX_AUTH_GHCR_IO_TOKEN").as_deref(), Some("hunter2"));
    assert_eq!(read("OCX_AUTH_GHCR_IO_USER").as_deref(), Some("octocat"));
    assert_eq!(read("A=B"), None);
    let slot = OCX_AUTH_TOKEN.get_slot("GHCR_IO").unwrap().map(Sensitive::into_inner);
    assert_eq!(slot.as_deref(), Some("hunter2"));
}

#[test]
fn snapshot_applies_the_overrides() {
    let env = overrides::lock();
    env.set_raw("OCX_ENV_SNAPSHOT_PROBE", "on");
    env.remove(&PATH);
    let snapshot = snapshot();
    assert!(snapshot.contains(&("OCX_ENV_SNAPSHOT_PROBE".into(), "on".into())));
    assert!(snapshot.iter().all(|(key, _)| key != PATH.name));
    assert!(snapshot.windows(2).all(|pair| pair[0].0 < pair[1].0), "sorted by key");
}

#[test]
fn hermetic_snapshot_holds_only_the_overrides() {
    let env = overrides::lock();
    env.hermetic(std::env::temp_dir());
    env.set_raw("OCX_ENV_SNAPSHOT_PROBE", "on");
    assert_eq!(snapshot(), vec![("OCX_ENV_SNAPSHOT_PROBE".into(), "on".into())]);
}

#[test]
fn all_holds_every_declaration_including_test_seams_under_test() {
    let names: Vec<&str> = all().map(|var| var.name).collect();
    for expected in [
        OCX_HOME.name,
        OCX_IDENTITY_TOKEN.declaration().name,
        HOME.name,
        __OCX_TESTING_FAULT.name,
    ] {
        assert!(names.contains(&expected), "{expected}");
    }
}

/// Spelled as literals: a list built from the declarations would agree with itself.
#[test]
fn credential_keys_are_exactly_the_five_scrubbed_credentials() {
    let mut keys: Vec<&str> = credential_keys().collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "OCX_ANNOUNCE_GIT_TOKEN",
            "OCX_IDENTITY_TOKEN",
            "OCX_KEY_PASSWORD",
            "OCX_SIGNING_KEY",
            "OCX_TELEMETRY_HEADERS"
        ]
    );
}

#[test]
fn reserved_and_valid_keys() {
    assert!(is_reserved_ocx_key("OCX_HOME"));
    assert!(is_reserved_ocx_key("ocx_home"));
    assert!(is_reserved_ocx_key("__OCX_ENV_STATE"));
    assert!(!is_reserved_ocx_key("HOME"));
    assert!(!is_reserved_ocx_key("_OCX_X"));
    assert!(is_valid_env_key("_A1"));
    assert!(!is_valid_env_key(""));
    assert!(!is_valid_env_key("1A"));
    assert!(!is_valid_env_key("A-B"));
}

#[test]
#[should_panic(expected = "'OCX_HOME' is a declared variable")]
fn set_raw_refuses_a_declared_name() {
    overrides::lock().set_raw("OCX_HOME", "/tmp/ocx");
}

#[test]
#[should_panic(expected = "'OCX_HOME' is a declared variable")]
fn remove_raw_refuses_a_declared_name() {
    overrides::lock().remove_raw("OCX_HOME");
}

#[test]
fn a_withdrawn_row_is_never_honoured_and_never_refuses() {
    static ROW: &[Retired] = &[Retired {
        name: "OCX_SYNTHETIC_WITHDRAWN",
        replacement: Replacement::Flag("--synthetic"),
        change: Change::Withdrawn,
        status: Status::Window {
            removal: REMOVAL_RELEASE,
        },
    }];
    // A second row aims a Withdrawn spelling at a real variable: still not read through it.
    static AIMED: &[Retired] = &[Retired {
        name: "OCX_SYNTHETIC_WITHDRAWN",
        replacement: Replacement::Env(&OCX_QUIET),
        change: Change::Withdrawn,
        status: Status::Window {
            removal: REMOVAL_RELEASE,
        },
    }];
    let env = overrides::lock();
    env.remove(&OCX_QUIET);
    env.set_raw("OCX_SYNTHETIC_WITHDRAWN", "1");

    env.retire(ROW);
    let hits = retired_hits(&snapshot());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].replacement.name(), "--synthetic");

    env.retire(AIMED);
    assert_eq!(OCX_QUIET.get(), None, "a Withdrawn spelling is not an alias");
}

// ── Secret references ──

/// Reads the carrying variable of one reference-capable declaration, as a `String`.
type Read = fn(&overrides::EnvLock, &str) -> Result<Option<String>, ReferenceError>;

fn read_secret(env: &overrides::EnvLock, value: &str) -> Result<Option<String>, ReferenceError> {
    env.set(OCX_IDENTITY_TOKEN.declaration(), value);
    Ok(OCX_IDENTITY_TOKEN.get()?.map(|secret| secret.expose().to_owned()))
}

fn read_reference(env: &overrides::EnvLock, value: &str) -> Result<Option<String>, ReferenceError> {
    env.set(OCX_ANNOUNCE_GIT_USERNAME.declaration(), value);
    OCX_ANNOUNCE_GIT_USERNAME.get()
}

/// The same table runs for a `SecretVar` and a `ReferenceVar`.
const READERS: [Read; 2] = [read_secret, read_reference];

fn each_reader(check: impl Fn(&overrides::EnvLock, Read)) {
    let env = overrides::lock();
    for read in READERS {
        check(&env, read);
    }
}

fn file_reference(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

/// A temporary file holding `content`, and the `file://` value naming it.
fn file_with(content: &[u8]) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    std::fs::write(&path, content).unwrap();
    let value = file_reference(&path);
    (dir, value)
}

#[test]
fn a_literal_is_taken_as_it_is() {
    each_reader(|env, read| {
        assert_eq!(read(env, "hunter2").unwrap().as_deref(), Some("hunter2"));
        assert_eq!(
            read(env, "Authorization=Basic%20x://y").unwrap().as_deref(),
            Some("Authorization=Basic%20x://y")
        );
        assert_eq!(read(env, "https://x").unwrap().as_deref(), Some("https://x"));
        assert_eq!(read(env, "file:/x").unwrap().as_deref(), Some("file:/x"));
    });
}

#[test]
fn plain_resolves_to_its_text_verbatim() {
    each_reader(|env, read| {
        assert_eq!(read(env, "plain://hunter2").unwrap().as_deref(), Some("hunter2"));
        assert_eq!(read(env, "PLAIN://x").unwrap().as_deref(), Some("x"));
        assert_eq!(read(env, "plain://plain://x").unwrap().as_deref(), Some("plain://x"));
        assert_eq!(read(env, "plain://env://X").unwrap().as_deref(), Some("env://X"));
        assert_eq!(read(env, "plain://file:///x").unwrap().as_deref(), Some("file:///x"));
        assert_eq!(read(env, "plain:// padded ").unwrap().as_deref(), Some(" padded "));
    });
}

#[test]
fn plain_with_nothing_is_unset() {
    each_reader(|env, read| assert_eq!(read(env, "plain://").unwrap(), None));
}

#[test]
fn a_file_reference_reads_the_file() {
    let (_dir, value) = file_with(b"from-file");
    each_reader(|env, read| {
        assert_eq!(read(env, &value).unwrap().as_deref(), Some("from-file"));
        let mixed = format!("FiLe://{}", &value["file://".len()..]);
        assert_eq!(read(env, &mixed).unwrap().as_deref(), Some("from-file"));
    });
}

#[test]
fn trailing_line_endings_are_stripped_and_other_whitespace_kept() {
    for (content, expected) in [
        ("tok\n", "tok"),
        ("tok\r\n", "tok"),
        ("tok\n\n", "tok"),
        ("tok\t\n", "tok\t"),
        ("tok ", "tok "),
        (" tok", " tok"),
        ("a\nb\n", "a\nb"),
    ] {
        let (_dir, value) = file_with(content.as_bytes());
        each_reader(|env, read| assert_eq!(read(env, &value).unwrap().as_deref(), Some(expected), "{content:?}"));
    }
}

#[test]
fn an_empty_file_or_one_of_only_line_endings_is_an_error() {
    for content in ["", "\n", "\r\n\r\n"] {
        let (_dir, value) = file_with(content.as_bytes());
        each_reader(|env, read| {
            let error = read(env, &value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::FileEmpty { .. }),
                "{content:?}: {error}"
            );
        });
    }
}

#[test]
fn a_missing_file_is_unreadable_and_keeps_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let value = file_reference(&dir.path().join("absent"));
    each_reader(|env, read| {
        let error = read(env, &value).unwrap_err();
        assert!(matches!(error, ReferenceError::FileUnreadable { .. }), "{error}");
        assert!(std::error::Error::source(&error).is_some());
    });
}

#[test]
fn a_directory_is_not_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let value = file_reference(dir.path());
    each_reader(|env, read| {
        let error = read(env, &value).unwrap_err();
        assert!(matches!(error, ReferenceError::FileNotRegular { .. }), "{error}");
    });
}

#[test]
fn the_cap_is_exact() {
    let (_at, at_cap) = file_with(&vec![b'a'; MAX_REFERENCED_SECRET_BYTES]);
    let (_over, over_cap) = file_with(&vec![b'a'; MAX_REFERENCED_SECRET_BYTES + 1]);
    each_reader(|env, read| {
        assert_eq!(
            read(env, &at_cap).unwrap().map(|value| value.len()),
            Some(MAX_REFERENCED_SECRET_BYTES)
        );
        let error = read(env, &over_cap).unwrap_err();
        assert!(
            matches!(error, ReferenceError::FileTooLarge { cap, .. } if cap == MAX_REFERENCED_SECRET_BYTES),
            "{error}"
        );
    });
}

#[test]
fn a_file_that_is_not_utf8_is_refused() {
    let (_dir, value) = file_with(&[b'a', 0xff, 0xfe]);
    each_reader(|env, read| {
        let error = read(env, &value).unwrap_err();
        assert!(matches!(error, ReferenceError::FileNotUtf8 { .. }), "{error}");
    });
}

#[test]
fn a_leading_utf8_bom_is_dropped_once_and_a_later_one_kept() {
    for (content, expected) in [
        (&b"\xEF\xBB\xBFtok"[..], "tok"),
        (&b"\xEF\xBB\xBFtok\r\n"[..], "tok"),
        ("a\u{FEFF}b".as_bytes(), "a\u{FEFF}b"),
        ("\u{FEFF}\u{FEFF}tok".as_bytes(), "\u{FEFF}tok"),
    ] {
        let (_dir, value) = file_with(content);
        each_reader(|env, read| assert_eq!(read(env, &value).unwrap().as_deref(), Some(expected), "{content:?}"));
    }
}

#[test]
fn a_file_of_only_a_utf8_bom_is_empty() {
    for content in [&b"\xEF\xBB\xBF"[..], &b"\xEF\xBB\xBF\r\n"[..]] {
        let (_dir, value) = file_with(content);
        each_reader(|env, read| {
            let error = read(env, &value).unwrap_err();
            assert!(matches!(error, ReferenceError::FileEmpty { .. }), "{error}");
        });
    }
}

#[test]
fn a_utf16_file_with_a_bom_is_refused_by_name_without_its_content() {
    for content in [&b"\xFF\xFEt\x00o\x00k\x00"[..], &b"\xFE\xFF\x00t\x00o\x00k"[..]] {
        let (_dir, value) = file_with(content);
        each_reader(|env, read| {
            let error = read(env, &value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::FileNotUtf8 { utf16: true, .. }),
                "{error}"
            );
            let message = error.to_string();
            assert!(message.contains("UTF-16") && message.contains("UTF-8"), "{message}");
            assert!(!message.contains("tok"), "{message}");
        });
    }
}

#[test]
fn a_file_without_a_bom_keeps_the_plain_not_utf8_message() {
    let (_dir, value) = file_with(&[b'a', 0xff, 0xfe]);
    each_reader(|env, read| {
        let error = read(env, &value).unwrap_err();
        assert!(
            matches!(error, ReferenceError::FileNotUtf8 { utf16: false, .. }),
            "{error}"
        );
        assert!(error.to_string().ends_with("is not UTF-8"), "{error}");
    });
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_is_followed_to_a_file_and_refused_to_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, "linked").unwrap();
    let to_file = dir.path().join("to_file");
    std::os::unix::fs::symlink(&target, &to_file).unwrap();
    let to_dir = dir.path().join("to_dir");
    std::os::unix::fs::symlink(dir.path(), &to_dir).unwrap();
    each_reader(|env, read| {
        assert_eq!(read(env, &file_reference(&to_file)).unwrap().as_deref(), Some("linked"));
        let error = read(env, &file_reference(&to_dir)).unwrap_err();
        assert!(matches!(error, ReferenceError::FileNotRegular { .. }), "{error}");
    });
}

#[cfg(unix)]
#[test]
fn a_device_returns_not_regular_without_reading_it() {
    each_reader(|env, read| {
        let error = read(env, "file:///dev/zero").unwrap_err();
        assert!(matches!(error, ReferenceError::FileNotRegular { .. }), "{error}");
    });
}

#[test]
fn a_relative_or_invalid_file_path_is_refused_before_any_read() {
    each_reader(|env, read| {
        for value in ["file://relative/x", "file://./x", "file://~/x"] {
            let error = read(env, value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::FilePathRelative { .. }),
                "{value}: {error}"
            );
        }
        for value in ["file://", "file://\0", "file://\\\\.\\pipe\\x", "file://\\\\?\\x"] {
            let error = read(env, value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::FilePathInvalid { .. }),
                "{value:?}: {error}"
            );
        }
    });
}

#[cfg(windows)]
#[test]
fn windows_drive_letter_and_unc_paths_are_absolute() {
    each_reader(|env, read| {
        for value in ["file://C:\\ocx-absent\\x", "file://\\\\ocx-absent.invalid\\share\\x"] {
            let error = read(env, value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::FileUnreadable { .. }),
                "{value}: {error}"
            );
        }
    });
}

#[cfg(unix)]
#[test]
fn unix_drive_letter_and_unc_paths_are_relative() {
    each_reader(|env, read| {
        for value in ["file://C:\\x", "file://\\\\host\\share\\x"] {
            let error = read(env, value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::FilePathRelative { .. }),
                "{value}: {error}"
            );
        }
    });
}

#[test]
fn an_env_reference_reads_the_target() {
    each_reader(|env, read| {
        env.set_raw("OCX_TEST_REFERENCE_TARGET", "from-target");
        assert_eq!(
            read(env, "env://OCX_TEST_REFERENCE_TARGET").unwrap().as_deref(),
            Some("from-target")
        );
        assert_eq!(
            read(env, "ENV://OCX_TEST_REFERENCE_TARGET").unwrap().as_deref(),
            Some("from-target")
        );
    });
}

#[test]
fn an_unset_or_empty_env_target_is_an_error() {
    each_reader(|env, read| {
        env.remove_raw("OCX_TEST_REFERENCE_TARGET");
        let error = read(env, "env://OCX_TEST_REFERENCE_TARGET").unwrap_err();
        assert!(matches!(error, ReferenceError::EnvTargetUnset { .. }), "{error}");
        env.set_raw("OCX_TEST_REFERENCE_TARGET", "");
        let error = read(env, "env://OCX_TEST_REFERENCE_TARGET").unwrap_err();
        assert!(matches!(error, ReferenceError::EnvTargetUnset { .. }), "{error}");
    });
}

#[test]
fn an_env_name_no_environment_can_hold_is_refused() {
    each_reader(|env, read| {
        for value in ["env://", "env://A=B", "env://A\0B"] {
            let error = read(env, value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::EnvNameInvalid { .. }),
                "{value:?}: {error}"
            );
        }
    });
}

#[test]
fn an_env_reference_to_itself_is_refused() {
    let env = overrides::lock();
    env.set(OCX_IDENTITY_TOKEN.declaration(), "env://OCX_IDENTITY_TOKEN");
    let error = OCX_IDENTITY_TOKEN.get().unwrap_err();
    assert!(matches!(error, ReferenceError::EnvSelfReference { .. }), "{error}");
    env.set_raw("OCX_AUTH_X_TOKEN", "env://OCX_AUTH_X_TOKEN");
    let error = OCX_AUTH_TOKEN.get_slot("X").unwrap_err();
    assert!(matches!(error, ReferenceError::EnvSelfReference { .. }), "{error}");
    env.set(
        OCX_ANNOUNCE_GIT_USERNAME.declaration(),
        "env://OCX_ANNOUNCE_GIT_USERNAME",
    );
    let error = OCX_ANNOUNCE_GIT_USERNAME.get().unwrap_err();
    assert!(matches!(error, ReferenceError::EnvSelfReference { .. }), "{error}");
}

/// Windows resolves names case-insensitively, so a lower-case spelling is the same variable there.
#[test]
fn a_lower_case_self_reference_is_refused_only_where_names_fold_case() {
    let env = overrides::lock();
    env.set(OCX_IDENTITY_TOKEN.declaration(), "env://ocx_identity_token");
    let result = OCX_IDENTITY_TOKEN.get();
    if cfg!(windows) {
        assert!(matches!(result, Err(ReferenceError::EnvSelfReference { .. })));
    } else {
        assert!(matches!(result, Err(ReferenceError::EnvTargetUnset { .. })));
    }
}

#[test]
fn a_resolved_value_is_never_parsed_again() {
    let (_dir, file) = file_with(b"env://OCX_TEST_REFERENCE_TARGET\n");
    each_reader(|env, read| {
        assert_eq!(
            read(env, &file).unwrap().as_deref(),
            Some("env://OCX_TEST_REFERENCE_TARGET")
        );
        env.set_raw("OCX_TEST_REFERENCE_TARGET", "file:///nonexistent");
        assert_eq!(
            read(env, "env://OCX_TEST_REFERENCE_TARGET").unwrap().as_deref(),
            Some("file:///nonexistent")
        );
    });
}

#[test]
fn two_variables_referencing_each_other_yield_text_not_a_loop() {
    let env = overrides::lock();
    env.set(OCX_IDENTITY_TOKEN.declaration(), "env://OCX_ANNOUNCE_TOKEN");
    env.set(OCX_ANNOUNCE_TOKEN.declaration(), "env://OCX_IDENTITY_TOKEN");
    let value = OCX_IDENTITY_TOKEN
        .get()
        .unwrap()
        .map(|secret| secret.expose().to_owned());
    assert_eq!(value.as_deref(), Some("env://OCX_IDENTITY_TOKEN"));
}

#[test]
fn a_secret_slot_reads_a_reference() {
    let (_dir, value) = file_with(b"slot-token\n");
    let env = overrides::lock();
    env.set_raw("OCX_AUTH_GHCR_IO_TOKEN", value);
    let token = OCX_AUTH_TOKEN
        .get_slot("GHCR_IO")
        .unwrap()
        .map(|secret| secret.expose().to_owned());
    assert_eq!(token.as_deref(), Some("slot-token"));
}

#[test]
fn a_non_opted_non_secret_keeps_a_file_value_literally() {
    let env = overrides::lock();
    env.set(&OCX_LOG_LEVEL, "file:///does/not/exist");
    assert_eq!(OCX_LOG_LEVEL.get().as_deref(), Some("file:///does/not/exist"));
    env.set(&OCX_INDEX, "file:///srv/x");
    assert_eq!(OCX_INDEX.get_os(), Some(OsString::from("file:///srv/x")));
}

#[test]
fn a_foreign_secret_keeps_a_file_value_literally() {
    let env = overrides::lock();
    env.set(CI_JOB_TOKEN.declaration(), "file:///does/not/exist");
    let value = CI_JOB_TOKEN.get().unwrap().map(|secret| secret.expose().to_owned());
    assert_eq!(value.as_deref(), Some("file:///does/not/exist"));
}

#[test]
fn is_set_answers_from_the_raw_value_and_opens_no_file() {
    let env = overrides::lock();
    env.set(OCX_IDENTITY_TOKEN.declaration(), "file:///does/not/exist");
    assert!(OCX_IDENTITY_TOKEN.is_set());
    env.set(OCX_ANNOUNCE_GIT_USERNAME.declaration(), "file:///does/not/exist");
    assert!(OCX_ANNOUNCE_GIT_USERNAME.is_set());
    env.set_raw("OCX_AUTH_X_TOKEN", "file:///does/not/exist");
    assert!(OCX_AUTH_TOKEN.is_set_slot("X"));
    env.set_raw("OCX_AUTH_X_USER", "file:///does/not/exist");
    assert!(OCX_AUTH_USER.is_set_slot("X"));
    env.set(OCX_IDENTITY_TOKEN.declaration(), "");
    assert!(!OCX_IDENTITY_TOKEN.is_set());
    assert!(!OCX_AUTH_TOKEN.is_set_slot("UNSET"));
}

#[test]
fn a_reference_capable_user_variable_answers_none_to_every_plain_reader() {
    let env = overrides::lock();
    env.set_raw("OCX_AUTH_x_USER", "octocat");
    let slot = OCX_AUTH_USER.declaration();
    assert_eq!(slot.get_slot("x"), None);
    assert_eq!(dynamic("OCX_AUTH_x_USER"), None);
    env.set(OCX_ANNOUNCE_GIT_USERNAME.declaration(), "octocat");
    let username = OCX_ANNOUNCE_GIT_USERNAME.declaration();
    assert_eq!(username.get(), None);
    assert_eq!(username.get_os(), None);
    assert_eq!(username.get_raw(), None);
    assert_eq!(dynamic("OCX_ANNOUNCE_GIT_USERNAME"), None);
}

const SENTINEL: &str = "SENTINEL-d41d8cd9";

#[test]
fn parse_debug_prints_no_payload() {
    for value in [
        SENTINEL.to_owned(),
        format!("plain://{SENTINEL}"),
        format!("env://{SENTINEL}"),
        format!("file://{SENTINEL}"),
    ] {
        let debug = format!("{:?}", SecretReference::parse(&value));
        assert!(!debug.contains(SENTINEL), "{debug}");
    }
}

#[test]
fn no_error_prints_file_content_or_a_literal() {
    let mut oversized = SENTINEL.as_bytes().to_vec();
    oversized.resize(MAX_REFERENCED_SECRET_BYTES + 1, b'a');
    let mut not_utf8 = SENTINEL.as_bytes().to_vec();
    not_utf8.push(0xff);
    let (_big, too_large) = file_with(&oversized);
    let (_bad, bad_utf8) = file_with(&not_utf8);
    each_reader(|env, read| {
        for value in [&too_large, &bad_utf8] {
            let error = read(env, value).unwrap_err();
            let shown = format!("{error} {error:?}");
            assert!(!shown.contains(SENTINEL), "{shown}");
        }
        let plain = read(env, &format!("plain://{SENTINEL}")).unwrap();
        assert_eq!(plain.as_deref(), Some(SENTINEL));
    });
}

#[test]
fn a_long_path_is_printed_cut_to_128_characters() {
    let long = "a".repeat(200);
    each_reader(|env, read| {
        let error = read(env, &format!("file://{long}")).unwrap_err();
        let shown = error.to_string();
        assert!(shown.contains(&format!("{}…", "a".repeat(128))), "{shown}");
        assert!(!shown.contains(&"a".repeat(129)), "{shown}");
    });
}

// ── Secret reference grammar and errors ──

#[test]
fn parse_table() {
    use reference::SecretReference as R;
    let cases = [
        ("file:///a", R::File("/a")),
        ("FiLe://rel", R::File("rel")),
        ("env://NAME", R::Env("NAME")),
        ("ENV://NAME", R::Env("NAME")),
        ("plain://x", R::Plain("x")),
        ("PLAIN://file://y", R::Plain("file://y")),
        ("file://", R::File("")),
        ("a file://x", R::Literal("a file://x")),
        ("x://y", R::Literal("x://y")),
        ("file:/x", R::Literal("file:/x")),
        ("file:", R::Literal("file:")),
        ("https://x", R::Literal("https://x")),
        ("vault://x", R::Literal("vault://x")),
        ("", R::Literal("")),
    ];
    for (value, expected) in cases {
        assert!(reference::SecretReference::parse(value) == expected, "{value:?}");
    }
}

#[test]
fn display_wording_of_every_variant() {
    let v = || "VAR".to_string();
    let p = || "/p".to_string();
    let cases = [
        (
            ReferenceError::EnvNameInvalid { variable: v() },
            "'VAR' references an environment variable by a name no environment can hold",
        ),
        (
            ReferenceError::EnvSelfReference { variable: v() },
            "'VAR' references itself",
        ),
        (
            ReferenceError::ValueNotUnicode { variable: v() },
            "'VAR' holds a reference that is not valid Unicode",
        ),
        (
            ReferenceError::FilePathInvalid { variable: v() },
            "'VAR' references a file with an empty or NUL-bearing path",
        ),
        (
            ReferenceError::FilePathRelative {
                variable: v(),
                path: "r".into(),
            },
            "'VAR' references the relative path 'r'; a file reference needs an absolute path",
        ),
        (
            ReferenceError::EnvTargetUnset {
                variable: v(),
                target: "T".into(),
            },
            "'VAR' references 'T', which is unset or empty",
        ),
        (
            ReferenceError::FileUnreadable {
                variable: v(),
                path: p(),
                source: std::sync::Arc::new(std::io::Error::other("x")),
            },
            "cannot read '/p', referenced by 'VAR'",
        ),
        (
            ReferenceError::FileNotRegular {
                variable: v(),
                path: p(),
            },
            "'/p', referenced by 'VAR', is not a regular file",
        ),
        (
            ReferenceError::FileEmpty {
                variable: v(),
                path: p(),
            },
            "'/p', referenced by 'VAR', is empty",
        ),
        (
            ReferenceError::FileTooLarge {
                variable: v(),
                path: p(),
                cap: 7,
            },
            "'/p', referenced by 'VAR', is larger than 7 bytes",
        ),
        (
            ReferenceError::FileNotUtf8 {
                variable: v(),
                path: p(),
                utf16: false,
            },
            "'/p', referenced by 'VAR', is not UTF-8",
        ),
        (
            ReferenceError::FileNotUtf8 {
                variable: v(),
                path: p(),
                utf16: true,
            },
            "'/p', referenced by 'VAR', is UTF-16; save it as UTF-8",
        ),
    ];
    for (error, message) in cases {
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn a_long_multibyte_path_is_cut_on_a_character_boundary_in_display_and_debug() {
    let path = "é".repeat(200);
    let error = ReferenceError::FileEmpty {
        variable: "V".into(),
        path: path.clone(),
    };
    let expected = format!("{}…", "é".repeat(128));
    assert!(error.to_string().contains(&expected));
    assert!(!error.to_string().contains(&"é".repeat(129)));
    assert!(!format!("{error:?}").contains(&"é".repeat(129)));
    assert!(format!("{error:?}").contains(&expected));
}

#[test]
fn a_lone_trailing_carriage_return_is_stripped() {
    each_reader(|env, read| {
        let (_dir, value) = file_with(b"tok\r");
        assert_eq!(read(env, &value).unwrap().as_deref(), Some("tok"));
    });
}

#[test]
fn forward_slash_device_namespaces_are_refused() {
    each_reader(|env, read| {
        for value in ["file:////./pipe/x", "file:////?/GLOBALROOT/x"] {
            let error = read(env, value).unwrap_err();
            assert!(
                matches!(error, ReferenceError::FilePathInvalid { .. }),
                "{value}: {error}"
            );
        }
    });
}

/// The overrides carry `String` only and `set_var` is disallowed, so a non-Unicode raw value goes to the
/// shared raw-value seam the readers and `is_set` both use.
#[cfg(unix)]
mod not_unicode {
    use std::os::unix::ffi::OsStringExt as _;

    use super::*;

    fn raw(bytes: &[u8]) -> Option<OsString> {
        Some(OsString::from_vec(bytes.to_vec()))
    }

    #[test]
    fn a_non_unicode_prefixed_value_is_refused_and_counts_as_set() {
        let var = OCX_IDENTITY_TOKEN.declaration();
        for bytes in [&b"file://\xff\xfe"[..], b"ENV://\xff", b"plain://\xff"] {
            assert!(
                matches!(
                    var.resolve("V", raw(bytes)),
                    Err(ReferenceError::ValueNotUnicode { .. })
                ),
                "{bytes:?}"
            );
            assert!(var.is_set_raw(raw(bytes)), "{bytes:?}");
        }
    }

    #[test]
    fn a_non_unicode_plain_value_reads_unset() {
        let var = OCX_IDENTITY_TOKEN.declaration();
        assert!(matches!(var.resolve("V", raw(b"tok\xff")), Ok(None)));
        assert!(!var.is_set_raw(raw(b"tok\xff")));
    }

    #[test]
    fn a_non_unicode_value_of_a_declaration_without_references_reads_unset() {
        let var = OCX_LOG_LEVEL.declaration();
        assert!(matches!(var.resolve("V", raw(b"file://\xff")), Ok(None)));
        assert!(!var.is_set_raw(raw(b"file://\xff")));
    }
}
