// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::ffi::{OsStr, OsString};

use crate::env::Env;

use super::{RevertOutcome, revert};
use crate::exec_state::types::{AppliedValue, Owner, Prior, RecordedValue, ValueKind};

fn record(key: &str, value: RecordedValue, kind: ValueKind) -> AppliedValue {
    AppliedValue {
        key: key.to_owned(),
        value,
        kind,
        separator: None,
        pre_existing: false,
        owner: Owner::Inherited,
        prior: None,
    }
}

fn path_record(key: &str, element: &str) -> AppliedValue {
    record(key, RecordedValue::Literal(element.to_owned()), ValueKind::Path)
}

fn list_record(key: &str, element: &str, separator: Option<&str>) -> AppliedValue {
    AppliedValue {
        separator: separator.map(str::to_owned),
        ..record(key, RecordedValue::Literal(element.to_owned()), ValueKind::List)
    }
}

fn constant_record(key: &str, value: &str, prior: Option<Prior>) -> AppliedValue {
    AppliedValue {
        prior,
        ..record(key, RecordedValue::Literal(value.to_owned()), ValueKind::Constant)
    }
}

fn joined(segments: &[&str]) -> OsString {
    std::env::join_paths(segments).expect("fixture segments hold no separator")
}

fn env_with(key: &str, value: impl Into<OsString>) -> Env {
    let mut env = Env::clean();
    env.set(key, value.into());
    env
}

// A path element is removed, the others keep their order.

#[test]
fn path_element_is_removed_and_the_others_keep_their_order() {
    let mut env = env_with("MY_TOOLS_PATH", joined(&["/ocx/a", "/user/b", "/user/c"]));
    let outcome = revert(&mut env, &path_record("MY_TOOLS_PATH", "/ocx/a"));
    assert_eq!(outcome, RevertOutcome::Reverted);
    assert_eq!(
        env.get("MY_TOOLS_PATH"),
        Some(joined(&["/user/b", "/user/c"]).as_os_str())
    );
}

#[test]
fn path_element_that_pre_existed_is_kept() {
    let mut env = env_with("MY_TOOLS_PATH", joined(&["/ocx/a", "/user/b"]));
    let record = AppliedValue {
        pre_existing: true,
        ..path_record("MY_TOOLS_PATH", "/ocx/a")
    };
    assert_eq!(revert(&mut env, &record), RevertOutcome::Kept);
    assert_eq!(
        env.get("MY_TOOLS_PATH"),
        Some(joined(&["/ocx/a", "/user/b"]).as_os_str())
    );
}

#[test]
fn path_element_that_is_gone_is_kept_and_the_value_untouched() {
    let original = joined(&["/user/b", "", "/user/c"]);
    let mut env = env_with("MY_TOOLS_PATH", original.clone());
    assert_eq!(
        revert(&mut env, &path_record("MY_TOOLS_PATH", "/ocx/a")),
        RevertOutcome::Kept
    );
    assert_eq!(env.get("MY_TOOLS_PATH"), Some(original.as_os_str()));
}

#[test]
fn path_key_that_is_gone_is_kept() {
    let mut env = Env::clean();
    assert_eq!(
        revert(&mut env, &path_record("MY_TOOLS_PATH", "/ocx/a")),
        RevertOutcome::Kept
    );
    assert_eq!(env.get("MY_TOOLS_PATH"), None);
}

#[test]
fn path_last_element_removes_the_key() {
    let mut env = env_with("MY_TOOLS_PATH", "/ocx/a");
    assert_eq!(
        revert(&mut env, &path_record("MY_TOOLS_PATH", "/ocx/a")),
        RevertOutcome::Reverted
    );
    assert_eq!(env.get("MY_TOOLS_PATH"), None);
}

#[test]
fn path_record_with_an_empty_element_is_withheld() {
    let original = joined(&["/user/b", "", "/user/c"]);
    let mut env = env_with("MY_TOOLS_PATH", original.clone());
    assert_eq!(
        revert(&mut env, &path_record("MY_TOOLS_PATH", "")),
        RevertOutcome::Withheld
    );
    assert_eq!(env.get("MY_TOOLS_PATH"), Some(original.as_os_str()));
}

// A list element is removed on its recorded separator.

#[test]
fn list_element_is_removed_on_the_recorded_separator() {
    let mut env = env_with("MY_FLAGS", "-a,-b,-c");
    assert_eq!(
        revert(&mut env, &list_record("MY_FLAGS", "-b", Some(","))),
        RevertOutcome::Reverted
    );
    assert_eq!(env.get("MY_FLAGS"), Some(OsStr::new("-a,-c")));
}

#[test]
fn list_edge_elements_and_a_multi_character_separator() {
    let mut env = env_with("MY_FLAGS", "-a ;; -b ;; -c");
    assert_eq!(
        revert(&mut env, &list_record("MY_FLAGS", "-a", Some(" ;; "))),
        RevertOutcome::Reverted
    );
    assert_eq!(env.get("MY_FLAGS"), Some(OsStr::new("-b ;; -c")));
    assert_eq!(
        revert(&mut env, &list_record("MY_FLAGS", "-c", Some(" ;; "))),
        RevertOutcome::Reverted
    );
    assert_eq!(env.get("MY_FLAGS"), Some(OsStr::new("-b")));
}

#[test]
fn list_last_element_removes_the_key() {
    let mut env = env_with("MY_FLAGS", "-a");
    assert_eq!(
        revert(&mut env, &list_record("MY_FLAGS", "-a", Some(","))),
        RevertOutcome::Reverted
    );
    assert_eq!(env.get("MY_FLAGS"), None);
}

#[test]
fn list_element_that_pre_existed_is_kept() {
    let mut env = env_with("MY_FLAGS", "-a,-b");
    let record = AppliedValue {
        pre_existing: true,
        ..list_record("MY_FLAGS", "-a", Some(","))
    };
    assert_eq!(revert(&mut env, &record), RevertOutcome::Kept);
    assert_eq!(env.get("MY_FLAGS"), Some(OsStr::new("-a,-b")));
}

#[test]
fn list_element_that_is_gone_is_kept_and_the_value_untouched() {
    let mut env = env_with("MY_FLAGS", "-a,-c");
    assert_eq!(
        revert(&mut env, &list_record("MY_FLAGS", "-b", Some(","))),
        RevertOutcome::Kept
    );
    assert_eq!(env.get("MY_FLAGS"), Some(OsStr::new("-a,-c")));
}

#[test]
fn list_record_without_a_usable_separator_or_element_is_withheld() {
    for (element, separator) in [("-a", None), ("-a", Some("")), ("", Some(",")), ("-a,", Some(","))] {
        let mut env = env_with("MY_FLAGS", "-a,,-b");
        assert_eq!(
            revert(&mut env, &list_record("MY_FLAGS", element, separator)),
            RevertOutcome::Withheld,
            "element {element:?} separator {separator:?}"
        );
        assert_eq!(env.get("MY_FLAGS"), Some(OsStr::new("-a,,-b")));
    }
}

#[test]
fn list_record_whose_element_starts_with_its_separator_is_withheld() {
    // Without the guard, `,-a` takes the empty segment before `-a` with it.
    let mut env = env_with("MY_FLAGS", "-b,,-a");
    assert_eq!(
        revert(&mut env, &list_record("MY_FLAGS", ",-a", Some(","))),
        RevertOutcome::Withheld
    );
    assert_eq!(env.get("MY_FLAGS"), Some(OsStr::new("-b,,-a")));
}

#[cfg(unix)]
#[test]
fn list_holding_non_utf8_bytes_is_withheld_and_left_byte_exact() {
    use std::os::unix::ffi::OsStringExt;
    let bytes = OsString::from_vec(b"-a,\xff,-b".to_vec());
    let mut env = env_with("MY_FLAGS", bytes.clone());
    assert_eq!(
        revert(&mut env, &list_record("MY_FLAGS", "-a", Some(","))),
        RevertOutcome::Withheld
    );
    assert_eq!(env.get("MY_FLAGS"), Some(bytes.as_os_str()));
}

// A constant is restored only while it still holds what ocx wrote.

#[test]
fn constant_that_still_holds_the_recorded_value_is_restored_to_its_prior() {
    let mut env = env_with("MY_HOME_DIR", "/ocx/value");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Value("/user/value".into())));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Reverted);
    assert_eq!(env.get("MY_HOME_DIR"), Some(OsStr::new("/user/value")));
}

#[test]
fn constant_with_set_but_empty_prior_is_restored_to_empty() {
    let mut env = env_with("MY_HOME_DIR", "/ocx/value");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Value(String::new())));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Reverted);
    assert_eq!(env.get("MY_HOME_DIR"), Some(OsStr::new("")));
}

#[test]
fn constant_with_unset_prior_is_removed() {
    let mut env = env_with("MY_HOME_DIR", "/ocx/value");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Unset));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Reverted);
    assert_eq!(env.get("MY_HOME_DIR"), None);
}

#[test]
fn constant_changed_since_ocx_wrote_it_is_kept() {
    let mut env = env_with("MY_HOME_DIR", "/user/changed");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Value("/user/value".into())));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Kept);
    assert_eq!(env.get("MY_HOME_DIR"), Some(OsStr::new("/user/changed")));
}

#[test]
fn constant_comparison_is_byte_exact() {
    let mut env = env_with("MY_HOME_DIR", "/ocx/value ");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Unset));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Kept);
    assert_eq!(env.get("MY_HOME_DIR"), Some(OsStr::new("/ocx/value ")));
}

#[cfg(unix)]
#[test]
fn constant_holding_non_utf8_bytes_is_kept() {
    use std::os::unix::ffi::OsStringExt;
    let bytes = OsString::from_vec(b"/ocx/\xff".to_vec());
    let mut env = env_with("MY_HOME_DIR", bytes.clone());
    let record = constant_record("MY_HOME_DIR", "/ocx/\u{fffd}", Some(Prior::Unset));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Kept);
    assert_eq!(env.get("MY_HOME_DIR"), Some(bytes.as_os_str()));
}

#[test]
fn constant_that_is_gone_is_kept() {
    let mut env = Env::clean();
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Value("/user/value".into())));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Kept);
    assert_eq!(env.get("MY_HOME_DIR"), None);
}

#[test]
fn constant_with_a_withheld_prior_is_withheld_and_not_touched() {
    let mut env = env_with("MY_HOME_DIR", "/ocx/value");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Withheld));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Withheld);
    assert_eq!(env.get("MY_HOME_DIR"), Some(OsStr::new("/ocx/value")));
}

#[test]
fn constant_without_a_prior_is_withheld() {
    let mut env = env_with("MY_HOME_DIR", "/ocx/value");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", None);
    assert_eq!(revert(&mut env, &record), RevertOutcome::Withheld);
    assert_eq!(env.get("MY_HOME_DIR"), Some(OsStr::new("/ocx/value")));
}

#[test]
fn newest_first_restores_the_users_value_and_the_older_record_is_kept() {
    // ocx wrote `a`, the user changed it to `b`, ocx wrote `c` over it: two records, not coalesced.
    let older = constant_record("MY_HOME_DIR", "a", Some(Prior::Unset));
    let newer = constant_record("MY_HOME_DIR", "c", Some(Prior::Value("b".into())));
    let mut env = env_with("MY_HOME_DIR", "c");
    assert_eq!(revert(&mut env, &newer), RevertOutcome::Reverted);
    assert_eq!(revert(&mut env, &older), RevertOutcome::Kept);
    assert_eq!(env.get("MY_HOME_DIR"), Some(OsStr::new("b")));
}

#[test]
fn a_continuous_chain_reverts_to_the_oldest_prior() {
    let older = constant_record("MY_HOME_DIR", "a", Some(Prior::Unset));
    let newer = constant_record("MY_HOME_DIR", "c", Some(Prior::Value("a".into())));
    let mut env = env_with("MY_HOME_DIR", "c");
    assert_eq!(revert(&mut env, &newer), RevertOutcome::Reverted);
    assert_eq!(revert(&mut env, &older), RevertOutcome::Reverted);
    assert_eq!(env.get("MY_HOME_DIR"), None);
}

// A constant reverts by exact value, whatever its key is called.

#[test]
fn a_secret_named_constant_is_removed_only_while_it_holds_the_recorded_value() {
    for key in ["MY_SERVICE_TOKEN", "GITHUB_TOKEN", "OCX_SIGNING_KEY"] {
        let record = constant_record(key, "tok-sentinel", Some(Prior::Unset));
        let mut env = env_with(key, "tok-sentinel");
        assert_eq!(revert(&mut env, &record), RevertOutcome::Reverted, "{key}");
        assert_eq!(env.get(key), None, "{key}");
    }
}

#[test]
fn a_secret_named_constant_the_user_changed_is_kept() {
    for key in ["MY_SERVICE_TOKEN", "GITHUB_TOKEN"] {
        let record = constant_record(key, "tok-sentinel", Some(Prior::Unset));
        let mut env = env_with(key, "mine");
        assert_eq!(revert(&mut env, &record), RevertOutcome::Kept, "{key}");
        assert_eq!(env.get(key), Some(OsStr::new("mine")), "{key}");
    }
}

#[test]
fn a_secret_named_constant_restores_its_recorded_prior_on_an_exact_match() {
    let record = constant_record("GITHUB_TOKEN", "tok-sentinel", Some(Prior::Value("mine".into())));
    let mut env = env_with("GITHUB_TOKEN", "tok-sentinel");
    assert_eq!(revert(&mut env, &record), RevertOutcome::Reverted);
    assert_eq!(env.get("GITHUB_TOKEN"), Some(OsStr::new("mine")));
}

#[test]
fn a_url_with_userinfo_reverts_by_the_same_exact_value_rule() {
    let url = "https://user:tok-sentinel@host.example/repo";
    let record = constant_record("REMOTE_URL", url, Some(Prior::Unset));
    let mut kept = env_with("REMOTE_URL", "https://host.example/repo");
    assert_eq!(revert(&mut kept, &record), RevertOutcome::Kept);
    let mut reverted = env_with("REMOTE_URL", url);
    assert_eq!(revert(&mut reverted, &record), RevertOutcome::Reverted);
    assert_eq!(reverted.get("REMOTE_URL"), None);
}

#[test]
fn a_withheld_prior_is_never_restored_even_on_an_exact_match() {
    let record = constant_record("GITHUB_TOKEN", "tok-sentinel", Some(Prior::Withheld));
    let mut env = env_with("GITHUB_TOKEN", "tok-sentinel");
    assert_eq!(revert(&mut env, &record), RevertOutcome::Withheld);
    assert_eq!(env.get("GITHUB_TOKEN"), Some(OsStr::new("tok-sentinel")));
}

// Windows folds keys ASCII-case-insensitively; the env does it for the equality guard and the restore.

/// EC-CONST-009: on Windows the equality guard and the restore fold key case.
#[cfg(windows)]
#[test]
fn windows_record_key_spelling_matches_a_differently_cased_key() {
    let mut env = env_with("my_home_dir", "/ocx/value");
    let record = constant_record("MY_HOME_DIR", "/ocx/value", Some(Prior::Value("/user/value".into())));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Reverted);
    assert_eq!(env.get("My_Home_Dir"), Some(OsStr::new("/user/value")));
}

/// EC-LIST-010: an element holding the platform separator names no single segment, so the value is untouched.
#[test]
fn path_record_whose_element_holds_the_platform_separator_is_kept_and_splits_nothing() {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let original = joined(&["/user/a", "/ocx/a", "/ocx/b"]);
    let mut env = env_with("MY_TOOLS_PATH", original.clone());
    let record = path_record("MY_TOOLS_PATH", &format!("/ocx/a{separator}/ocx/b"));
    assert_eq!(revert(&mut env, &record), RevertOutcome::Kept);
    assert_eq!(env.get("MY_TOOLS_PATH"), Some(original.as_os_str()));
}
