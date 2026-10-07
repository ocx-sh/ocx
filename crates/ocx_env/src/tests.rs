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
    let secret = OCX_IDENTITY_TOKEN.get().map(|value| value.expose().to_owned());
    assert_eq!(secret.as_deref(), Some("hunter2"));
    env.set(OCX_IDENTITY_TOKEN.declaration(), "");
    assert!(OCX_IDENTITY_TOKEN.get().is_none());
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
    assert_eq!(OCX_AUTH_USER.get_slot("GHCR_IO").as_deref(), Some("octocat"));
    let token = OCX_AUTH_TOKEN
        .get_slot("GHCR_IO")
        .map(|value| value.expose().to_owned());
    assert_eq!(token.as_deref(), Some("hunter2"));
    assert_eq!(OCX_AUTH_USER.get_slot("DOCKER_IO"), None);
    env.set_raw("OCX_AUTH_EMPTY_USER", "");
    assert_eq!(OCX_AUTH_USER.get_slot("EMPTY"), None);
}

#[test]
fn a_pattern_has_no_plain_value_and_a_literal_has_no_slot() {
    let env = overrides::lock();
    env.set(&OCX_AUTH_USER, "literal-braces");
    env.set(&OCX_LOG_LEVEL, "debug");
    assert_eq!(OCX_AUTH_USER.get(), None);
    assert_eq!(OCX_LOG_LEVEL.get_slot("ANY"), None);
    assert!(OCX_AUTH_USER.is_pattern());
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
    assert_eq!(dynamic("OCX_AUTH_GHCR_IO_USER").as_deref(), Some("octocat"));
    let read = |name| dynamic_secret(name).map(Sensitive::into_inner);
    assert_eq!(read("OCX_SIGNING_KEY").as_deref(), Some("pem"));
    assert_eq!(read("OCX_AUTH_GHCR_IO_TOKEN").as_deref(), Some("hunter2"));
    assert_eq!(read("OCX_AUTH_GHCR_IO_USER").as_deref(), Some("octocat"));
    assert_eq!(read("A=B"), None);
    let slot = OCX_AUTH_TOKEN.get_slot("GHCR_IO").map(Sensitive::into_inner);
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
fn credential_keys_are_exactly_the_four_scrubbed_credentials() {
    let mut keys: Vec<&str> = credential_keys().collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "OCX_ANNOUNCE_GIT_TOKEN",
            "OCX_IDENTITY_TOKEN",
            "OCX_KEY_PASSWORD",
            "OCX_SIGNING_KEY"
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
