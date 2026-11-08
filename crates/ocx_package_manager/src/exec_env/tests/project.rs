// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `project` end to end: each frame reads the previous frame's finished environment through the
//! real codec, the way a nested `ocx exec` reads what its parent handed it.
//!
//! `compose_tests` and `state_tests` pin one frame at a time; the owner run rule (an explicit
//! `ocx exec` composes what it was asked for whatever is already loaded) is a property of a chain.

use std::path::PathBuf;

use ocx_config::exec_state::{ExecState, Inherited, Owner, Prior, RecordedValue, Scope, ScopeId, SessionSource};

use super::super::compose_tests::{Fixture, ambient, constant, get, hermetic, join, list, path};
use super::super::{ChildEnvironment, ExecMode, project};

const CARRIER: &str = "__OCX_ENV_STATE";

fn scope(root: &str) -> Scope {
    Scope::Project {
        root: ocx_test_support::paths::abs(root),
    }
}

/// One spawn: the process environment is the parent's finished environment with `user` set on top,
/// and the inherited state is read back from it exactly as a spawn site reads it.
fn frame(parent: Option<&ChildEnvironment>, user: &[(&str, &str)], fixture: &Fixture) -> ChildEnvironment {
    let lock = hermetic();
    if let Some(parent) = parent {
        for (key, value) in parent.env.iter() {
            ambient(&lock, &key.to_string_lossy(), value.to_string_lossy());
        }
    }
    for (key, value) in user {
        ambient(&lock, key, *value);
    }
    let inherited = Inherited::from_process_env();
    project(&inherited, &fixture.spawn())
}

fn exec_in(mode: ExecMode, root: &str, composed: Vec<ocx_config::exec_state::ComposedValue>) -> Fixture {
    let mut fixture = Fixture::new(mode);
    fixture.composed = composed;
    fixture.composed_scope = Some(scope(root));
    fixture
}

/// The state a child's carrier decodes to; fails the test when it is not readable.
fn state_of(child: &ChildEnvironment) -> ExecState {
    let carrier = child.env.get(CARRIER).map(|value| value.to_os_string());
    match ocx_config::exec_state::decode(carrier.as_deref()) {
        Inherited::Readable(state) => state,
        other => panic!("the carrier must decode readable, got {}", describe(&other)),
    }
}

fn describe(inherited: &Inherited) -> &'static str {
    match inherited {
        Inherited::Absent => "absent",
        Inherited::Readable(_) => "readable",
        Inherited::Unreadable { .. } => "unreadable",
    }
}

fn count(joined: &str, element: &str) -> usize {
    std::env::split_paths(joined)
        .filter(|segment| segment.as_os_str() == element)
        .count()
}

// ── One frame ───────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_projected_child_carries_a_readable_state_naming_its_scope_and_the_pin() {
    let mut fixture = exec_in(ExecMode::Layer, "/fixture/a", vec![constant("G1_VAR", "g1-value")]);
    fixture.binary_pin = Some(ocx_test_support::paths::abs("/fixture/bin/ocx"));

    let child = frame(None, &[], &fixture);

    let state = state_of(&child);
    assert_eq!(state.inherited_scopes, vec![scope("/fixture/a")]);
    assert_eq!(state.applied.len(), 1);
    assert_eq!(state.applied[0].key, "G1_VAR");
    assert_eq!(state.applied[0].prior, Some(Prior::Unset));
    assert_eq!(
        get(&child.env, "OCX_BINARY_PIN").map(PathBuf::from),
        Some(ocx_test_support::paths::abs("/fixture/bin/ocx"))
    );
    assert!(child.warnings.is_empty(), "{:?}", child.warnings);
}

// ── Two projects ─────────────────────────────────────────────────────────────────────────

#[test]
fn a_second_project_layers_in_front_of_the_first_and_both_scopes_are_recorded() {
    let base = join(&["/usr/bin", "/bin"]);
    let outer = exec_in(ExecMode::Layer, "/fixture/a", vec![path("PATH", "/a/bin")]);
    let inner = exec_in(ExecMode::Layer, "/fixture/b", vec![path("PATH", "/b/bin")]);

    let first = frame(None, &[("PATH", &base)], &outer);
    let second = frame(Some(&first), &[], &inner);

    assert_eq!(
        get(&second.env, "PATH"),
        Some(join(&["/b/bin", "/a/bin", "/usr/bin", "/bin"]))
    );
    assert_eq!(
        state_of(&second).inherited_scopes,
        vec![scope("/fixture/a"), scope("/fixture/b")]
    );
}

// ── Same project, same group, twice ──────────────────────────────────────────────────────

fn group_one() -> Vec<ocx_config::exec_state::ComposedValue> {
    vec![
        constant("G1_VAR", "g1-value"),
        path("G1_DIRS", "/g1/dirs"),
        list("G1_LIST", "g1-item", ","),
    ]
}

#[test]
fn the_same_project_and_group_twice_composes_again_and_holds_each_value_once() {
    let outer = exec_in(ExecMode::Layer, "/fixture/a", group_one());
    let inner = exec_in(ExecMode::Layer, "/fixture/a", group_one());

    let first = frame(None, &[], &outer);
    let second = frame(Some(&first), &[], &inner);

    assert_eq!(get(&second.env, "G1_VAR").as_deref(), Some("g1-value"));
    assert_eq!(get(&second.env, "G1_DIRS").as_deref(), Some("/g1/dirs"));
    assert_eq!(get(&second.env, "G1_LIST").as_deref(), Some("g1-item"));
    let state = state_of(&second);
    assert_eq!(state.inherited_scopes, vec![scope("/fixture/a")], "one scope, not two");
    for key in ["G1_VAR", "G1_DIRS", "G1_LIST"] {
        let records: Vec<_> = state.applied.iter().filter(|record| record.key == key).collect();
        assert_eq!(records.len(), 1, "{key} must be recorded once: {records:?}");
        assert!(
            !records[0].pre_existing,
            "{key} keeps the first frame's view of the start"
        );
    }
}

#[test]
fn the_same_group_three_deep_and_then_an_isolate_leaves_nothing_of_it_behind() {
    let again = || exec_in(ExecMode::Layer, "/fixture/a", group_one());
    let first = frame(None, &[], &again());
    let second = frame(Some(&first), &[], &again());
    let third = frame(Some(&second), &[], &again());

    let isolated = frame(Some(&third), &[], &exec_in(ExecMode::Isolate, "/fixture/a", Vec::new()));

    for key in ["G1_VAR", "G1_DIRS", "G1_LIST"] {
        assert_eq!(get(&isolated.env, key), None, "{key} survived the isolate");
    }
    assert!(isolated.warnings.is_empty(), "{:?}", isolated.warnings);
}

// ── Same project, another group ──────────────────────────────────────────────────────────

#[test]
fn another_group_of_the_same_project_layers_on_top_with_its_path_in_front() {
    let outer = exec_in(
        ExecMode::Layer,
        "/fixture/a",
        vec![constant("G1_VAR", "g1-value"), path("SHARED_DIRS", "/g1/dirs")],
    );
    let inner = exec_in(
        ExecMode::Layer,
        "/fixture/a",
        vec![constant("G2_VAR", "g2-value"), path("SHARED_DIRS", "/g2/dirs")],
    );

    let first = frame(None, &[], &outer);
    let second = frame(Some(&first), &[], &inner);

    assert_eq!(get(&second.env, "G1_VAR").as_deref(), Some("g1-value"));
    assert_eq!(get(&second.env, "G2_VAR").as_deref(), Some("g2-value"));
    assert_eq!(
        get(&second.env, "SHARED_DIRS"),
        Some(join(&["/g2/dirs", "/g1/dirs"])),
        "g2 in front, g1 still reachable"
    );
    assert_eq!(state_of(&second).inherited_scopes, vec![scope("/fixture/a")]);
}

// ── A hook shell's layer is kept ─────────────────────────────────────────────────────────

fn hook_shell() -> ChildEnvironment {
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_owner = Owner::Session {
        scope: ScopeId::Project,
        source: SessionSource::Composed,
    };
    fixture.composed = vec![constant("HOOK_VAR", "hook-value"), path("PATH", "/hook/bin")];
    frame(None, &[("PATH", &join(&["/usr/bin", "/bin"]))], &fixture)
}

#[test]
fn a_layered_exec_inside_a_hook_shell_keeps_the_hook_layer_and_adds_the_group_on_top() {
    let shell = hook_shell();
    let exec = exec_in(ExecMode::Layer, "/fixture/a", group_one());

    let child = frame(Some(&shell), &[], &exec);

    assert_eq!(get(&child.env, "HOOK_VAR").as_deref(), Some("hook-value"));
    assert_eq!(get(&child.env, "PATH"), Some(join(&["/hook/bin", "/usr/bin", "/bin"])));
    assert_eq!(get(&child.env, "G1_VAR").as_deref(), Some("g1-value"));
    let state = state_of(&child);
    assert!(
        state
            .applied
            .iter()
            .all(|record| !matches!(record.owner, Owner::Session { .. })),
        "the hook's records are the child's inherited ones now: {:?}",
        state.applied
    );
}

#[test]
fn the_chain_started_from_a_hook_shell_gets_both_groups_and_the_hook_layer() {
    let shell = hook_shell();
    let outer = exec_in(ExecMode::Layer, "/fixture/a", vec![constant("G1_VAR", "g1-value")]);
    let inner = exec_in(ExecMode::Layer, "/fixture/a", vec![constant("G2_VAR", "g2-value")]);

    let first = frame(Some(&shell), &[], &outer);
    let second = frame(Some(&first), &[], &inner);

    for (key, value) in [
        ("HOOK_VAR", "hook-value"),
        ("G1_VAR", "g1-value"),
        ("G2_VAR", "g2-value"),
    ] {
        assert_eq!(get(&second.env, key).as_deref(), Some(value), "{key}");
    }
}

// ── Isolate ──────────────────────────────────────────────────────────────────────────────

#[test]
fn isolate_at_depth_two_removes_what_the_chain_applied_and_keeps_the_users_own_path_entry() {
    let user_path = join(&["/user/own/bin", "/usr/bin"]);
    let outer = exec_in(
        ExecMode::Layer,
        "/fixture/a",
        vec![constant("G1_VAR", "g1-value"), path("PATH", "/g1/bin")],
    );
    let inner = exec_in(
        ExecMode::Isolate,
        "/fixture/a",
        vec![constant("G2_VAR", "g2-value"), path("PATH", "/g2/bin")],
    );

    let first = frame(None, &[("PATH", &user_path)], &outer);
    let second = frame(Some(&first), &[], &inner);

    assert_eq!(
        get(&second.env, "G1_VAR"),
        None,
        "the outer frame's constant is reverted"
    );
    assert_eq!(get(&second.env, "G2_VAR").as_deref(), Some("g2-value"));
    assert_eq!(
        get(&second.env, "PATH"),
        Some(join(&["/g2/bin", "/user/own/bin", "/usr/bin"])),
        "the user's own entries survive, the outer frame's element does not"
    );
    assert_eq!(state_of(&second).inherited_scopes, vec![scope("/fixture/a")]);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
}

#[test]
fn isolate_restores_the_value_the_user_had_before_the_chain_replaced_it() {
    let outer = exec_in(ExecMode::Layer, "/fixture/a", vec![constant("G1_VAR", "g1-value")]);
    let inner = exec_in(ExecMode::Isolate, "/fixture/a", Vec::new());

    let first = frame(None, &[("G1_VAR", "users-own-value")], &outer);
    let second = frame(Some(&first), &[], &inner);

    assert_eq!(get(&first.env, "G1_VAR").as_deref(), Some("g1-value"));
    assert_eq!(get(&second.env, "G1_VAR").as_deref(), Some("users-own-value"));
}

#[test]
fn isolate_keeps_a_path_element_the_user_already_had_before_ocx_composed_it() {
    let user_path = join(&["/shared/bin", "/usr/bin"]);
    let outer = exec_in(ExecMode::Layer, "/fixture/a", vec![path("PATH", "/shared/bin")]);
    let inner = exec_in(ExecMode::Isolate, "/fixture/a", Vec::new());

    let first = frame(None, &[("PATH", &user_path)], &outer);
    let second = frame(Some(&first), &[], &inner);

    let path = get(&second.env, "PATH").expect("PATH is set");
    assert_eq!(
        count(&path, "/shared/bin"),
        1,
        "the user's own element survives: {path}"
    );
}

#[test]
fn isolate_under_a_hook_shell_removes_the_hook_layer_and_composes_only_this_spawn() {
    let shell = hook_shell();
    let isolated = exec_in(ExecMode::Isolate, "/fixture/b", vec![constant("B_VAR", "b-value")]);

    let child = frame(Some(&shell), &[], &isolated);

    assert_eq!(get(&child.env, "HOOK_VAR"), None);
    assert_eq!(get(&child.env, "PATH"), Some(join(&["/usr/bin", "/bin"])));
    assert_eq!(get(&child.env, "B_VAR").as_deref(), Some("b-value"));
    assert_eq!(state_of(&child).inherited_scopes, vec![scope("/fixture/b")]);
}

// ── Clean ────────────────────────────────────────────────────────────────────────────────

#[test]
fn clean_hands_the_next_frame_only_what_was_composed_and_the_pin() {
    let mut clean = exec_in(ExecMode::Clean, "/fixture/a", vec![constant("G1_VAR", "g1-value")]);
    clean.binary_pin = Some(ocx_test_support::paths::abs("/fixture/bin/ocx"));
    clean.written.home = Some(ocx_test_support::paths::abs("/fixture/home"));

    let child = frame(None, &[("AMBIENT_ONLY", "ambient"), ("PATH", "/usr/bin")], &clean);

    let mut keys: Vec<String> = child
        .env
        .iter()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();
    keys.sort();
    assert_eq!(keys, ["G1_VAR", "OCX_BINARY_PIN", CARRIER]);
    let state = state_of(&child);
    assert_eq!(
        state.home,
        Some(ocx_test_support::paths::abs("/fixture/home")),
        "the home rides in the header"
    );
    assert_eq!(state.applied[0].value, RecordedValue::Literal("g1-value".into()));
    assert_eq!(state.applied[0].prior, Some(Prior::Unset));
}

#[test]
fn a_layer_exec_under_a_clean_parent_composes_on_top_of_the_clean_environment() {
    let clean = exec_in(ExecMode::Clean, "/fixture/a", vec![constant("G1_VAR", "g1-value")]);
    let inner = exec_in(ExecMode::Layer, "/fixture/b", vec![constant("B_VAR", "b-value")]);

    let first = frame(None, &[("AMBIENT_ONLY", "ambient")], &clean);
    let second = frame(Some(&first), &[], &inner);

    assert_eq!(get(&second.env, "G1_VAR").as_deref(), Some("g1-value"));
    assert_eq!(get(&second.env, "B_VAR").as_deref(), Some("b-value"));
    assert_eq!(get(&second.env, "AMBIENT_ONLY"), None, "clean dropped it for good");
    assert_eq!(
        state_of(&second).inherited_scopes,
        vec![scope("/fixture/a"), scope("/fixture/b")]
    );
}

// ── Warning text ────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_truncation_warning_names_the_rung_and_the_size() {
    use super::super::ProjectionWarning;
    use ocx_config::exec_state::Truncation;

    let line = |truncated| {
        ProjectionWarning::StateTruncated {
            truncated,
            encoded_len: 4321,
        }
        .to_string()
    };
    for (truncated, words) in [
        (
            Truncation::InheritedDropped,
            "the records inherited from outer frames were dropped",
        ),
        (Truncation::AllAppliedDropped, "every applied record was dropped"),
        (Truncation::PayloadDropped, "the whole payload was dropped"),
    ] {
        assert_eq!(
            line(truncated),
            format!(
                "the execution state outgrew its size limit (4321 bytes) and was truncated: {words}; a later \
                 --isolate cannot fully isolate; use --clean for a pristine environment"
            )
        );
    }
}

#[test]
fn every_projection_warning_is_one_ascii_line_naming_a_way_out() {
    use super::super::ProjectionWarning;
    use ocx_config::exec_state::{Truncation, UnreadableReason};

    let warnings = [
        ProjectionWarning::InheritedUnreadable {
            received_major: Some(9),
            reason: UnreadableReason::OtherMajor,
        },
        ProjectionWarning::InheritedUnreadable {
            received_major: Some(2),
            reason: UnreadableReason::Invalid { field: "session.tiers" },
        },
        ProjectionWarning::InheritedUnreadable {
            received_major: None,
            reason: UnreadableReason::HeaderInvalid,
        },
        ProjectionWarning::CannotFullyIsolate {
            kept_changed: 2,
            kept_withheld: 1,
        },
        ProjectionWarning::CannotFullyIsolate {
            kept_changed: 0,
            kept_withheld: 0,
        },
        ProjectionWarning::CannotIsolate,
        ProjectionWarning::StateTruncated {
            truncated: Truncation::InheritedDropped,
            encoded_len: 4096,
        },
    ];
    for warning in warnings {
        let line = warning.to_string();
        assert!(line.is_ascii() && !line.contains('\n'), "{line:?}");
        assert!(
            line.contains("--clean") || line.contains("unset __OCX_ENV_STATE"),
            "{line}"
        );
    }
}
