// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! What a spawn writes into the child's state: records, scopes, replay, identities, selection.

use std::path::PathBuf;

use ocx_config::exec_state::{
    AppliedValue, ComposedValue, ExecState, IdentityRecord, Inherited, MAX_INHERITED_SCOPES, Owner, Pinned, PinnedTier,
    Prior, RecordedValue, ReplayPayload, ReplayValue, RevertOutcome, Scope, ScopeId, Selection, SessionCache,
    SessionSource, SessionToken, Truncation, ValueKind, revert,
};

use super::compose_tests::{
    Fixture, ambient, applied, constant, constant_record, get, hermetic, join, list, path, path_record, readable,
    unreadable,
};
use super::{ExecMode, LaunchIdentity, ProjectionWarning, compose};

const SENTINEL: &str = "fixture-secret-value";
const DIGEST_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const DIGEST_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn project(root: &str) -> Scope {
    Scope::Project {
        root: PathBuf::from(root),
    }
}

fn session(scope: ScopeId, source: SessionSource) -> Owner {
    Owner::Session { scope, source }
}

fn with_owner(record: AppliedValue, owner: Owner) -> AppliedValue {
    AppliedValue { owner, ..record }
}

fn state_of(applied: Vec<AppliedValue>) -> ExecState {
    ExecState {
        applied,
        ..ExecState::default()
    }
}

fn identity(digest: &str, name: &str, owner: Owner, no_patches: bool) -> IdentityRecord {
    IdentityRecord {
        digest: digest.to_owned(),
        names: vec![name.to_owned()],
        owner,
        no_patches,
    }
}

fn launch(digest: &str, name: &str, no_patches: bool) -> LaunchIdentity {
    LaunchIdentity {
        digest: digest.to_owned(),
        names: vec![name.to_owned()],
        no_patches,
    }
}

// ── Retagging inherited session records ─────────────────────────────────────────────────────────

#[test]
fn a_layer_spawn_retags_session_records_inherited_and_adds_their_scopes() {
    let _lock = hermetic();
    let shell = ExecState {
        applied: vec![
            with_owner(
                path_record("PATH", "/global/bin"),
                session(ScopeId::Global, SessionSource::Composed),
            ),
            with_owner(
                path_record("PATH", "/project/bin"),
                session(ScopeId::Project, SessionSource::Composed),
            ),
            with_owner(
                constant_record("FROM_FLAG", "x", Prior::Unset),
                session(ScopeId::Project, SessionSource::Invocation),
            ),
        ],
        selection: Selection::Pinned(Pinned {
            tier: PinnedTier::Project {
                dir: PathBuf::from("/fixture/project"),
            },
            groups: Vec::new(),
            with_names: Vec::new(),
            env_keys: Vec::new(),
            invocation_digest: "0".repeat(64),
            pinned_paths: None,
        }),
        ..ExecState::default()
    };

    let projected = compose(&Inherited::Readable(shell), &Fixture::new(ExecMode::Layer).spawn());

    assert!(projected.state.applied.iter().all(|r| r.owner == Owner::Inherited));
    assert_eq!(
        projected.state.inherited_scopes,
        vec![Scope::Global, project("/fixture/project")]
    );
}

#[test]
fn a_shell_spawn_tags_its_own_composition_as_session_records() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_owner = session(ScopeId::Project, SessionSource::Composed);
    fixture.composed = vec![constant("FROM_TOML", "a")];
    fixture.invocation = vec![constant("FROM_FLAG", "b")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(
        applied(&projected, "FROM_TOML")[0].owner,
        session(ScopeId::Project, SessionSource::Composed)
    );
    assert_eq!(
        applied(&projected, "FROM_FLAG")[0].owner,
        session(ScopeId::Project, SessionSource::Invocation)
    );
    assert!(
        projected.state.inherited_scopes.is_empty(),
        "no composed_scope for a shell"
    );
}

// ── Coalescing ────────────────────────────────────────────────────────────────────────────

#[test]
fn a_continuous_constant_chain_merges_into_one_record_with_the_oldest_prior() {
    let lock = hermetic();
    let mut first = Fixture::new(ExecMode::Layer);
    first.composed = vec![constant("JAVA_HOME", "/a")];
    let state_after_first = compose(&Inherited::Absent, &first.spawn()).state;

    ambient(&lock, "JAVA_HOME", "/a");
    let mut second = Fixture::new(ExecMode::Layer);
    second.composed = vec![constant("JAVA_HOME", "/b")];
    let projected = compose(&Inherited::Readable(state_after_first), &second.spawn());

    let records = applied(&projected, "JAVA_HOME");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].value, RecordedValue::Literal("/b".to_owned()));
    assert_eq!(records[0].prior, Some(Prior::Unset));
}

#[test]
fn a_user_change_between_two_ocx_writes_keeps_the_records_separate_and_isolate_restores_the_users_value() {
    let lock = hermetic();
    // Depth 1: ocx sets JAVA_HOME=/a over nothing.
    let mut first = Fixture::new(ExecMode::Layer);
    first.composed = vec![constant("JAVA_HOME", "/a")];
    let state_one = compose(&Inherited::Absent, &first.spawn()).state;

    // The user sets /u, then a nested exec sets /b.
    ambient(&lock, "JAVA_HOME", "/u");
    let mut second = Fixture::new(ExecMode::Layer);
    second.composed = vec![constant("JAVA_HOME", "/b")];
    let state_two = compose(&Inherited::Readable(state_one), &second.spawn()).state;
    let records: Vec<_> = state_two.applied.iter().filter(|r| r.key == "JAVA_HOME").collect();
    assert_eq!(records.len(), 2, "the user's change breaks the chain");
    assert_eq!(records[0].prior, Some(Prior::Unset));
    assert_eq!(records[1].prior, Some(Prior::Value("/u".to_owned())));

    // Depth 3: --isolate with nothing composed restores the user's value and keeps the older record.
    ambient(&lock, "JAVA_HOME", "/b");
    let isolate = Fixture::new(ExecMode::Isolate);
    let projected = compose(&Inherited::Readable(state_two), &isolate.spawn());

    assert_eq!(get(&projected.env, "JAVA_HOME").as_deref(), Some("/u"));
    assert!(projected.warnings.contains(&ProjectionWarning::CannotFullyIsolate {
        kept_changed: 1,
        kept_withheld: 0
    }));
}

#[test]
fn a_secret_named_constant_merges_with_an_older_record_like_any_other() {
    let lock = hermetic();
    let mut first = Fixture::new(ExecMode::Layer);
    first.composed = vec![constant("GITHUB_TOKEN", SENTINEL)];
    let state = compose(&Inherited::Absent, &first.spawn()).state;

    ambient(&lock, "GITHUB_TOKEN", SENTINEL);
    let mut second = Fixture::new(ExecMode::Layer);
    second.composed = vec![constant("GITHUB_TOKEN", SENTINEL)];
    let projected = compose(&Inherited::Readable(state), &second.spawn());

    let records = applied(&projected, "GITHUB_TOKEN");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].prior, Some(Prior::Unset));
}

#[test]
fn a_session_constant_over_an_inherited_one_stays_its_own_record_so_the_hook_can_undo_it() {
    let lock = hermetic();
    let mut outer = Fixture::new(ExecMode::Layer);
    outer.composed = vec![constant("JAVA_HOME", "/a")];
    let outer_state = compose(&Inherited::Absent, &outer.spawn()).state;

    // `ocx shell` inside the outer exec composes /b as a session value.
    ambient(&lock, "JAVA_HOME", "/a");
    let mut shell = Fixture::new(ExecMode::Layer);
    shell.composed_owner = session(ScopeId::Project, SessionSource::Composed);
    shell.composed = vec![constant("JAVA_HOME", "/b")];
    let shell_state = compose(&Inherited::Readable(outer_state), &shell.spawn()).state;

    let records: Vec<_> = shell_state.applied.iter().filter(|r| r.key == "JAVA_HOME").collect();
    assert_eq!(records.len(), 2, "a chain across owners does not merge: {records:?}");
    assert_eq!(records[0].owner, Owner::Inherited);
    assert_eq!(records[0].prior, Some(Prior::Unset));
    assert_eq!(records[1].owner, session(ScopeId::Project, SessionSource::Composed));
    assert_eq!(records[1].prior, Some(Prior::Value("/a".to_owned())));

    // The hook undoing its own record restores the outer frame's value, not the one before it.
    let mut env = ocx_config::env::Env::clean();
    env.set("JAVA_HOME", "/b");
    assert_eq!(revert(&mut env, records[1]), RevertOutcome::Reverted);
    assert_eq!(get(&env, "JAVA_HOME").as_deref(), Some("/a"));

    // The next spawn retags both inherited and folds the chain into one record.
    let next = compose(
        &Inherited::Readable(shell_state.clone()),
        &Fixture::new(ExecMode::Layer).spawn(),
    );
    let folded = applied(&next, "JAVA_HOME");
    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0].value, RecordedValue::Literal("/b".to_owned()));
    assert_eq!(folded[0].prior, Some(Prior::Unset));
}

#[test]
fn one_record_per_path_element_and_the_oldest_pre_existing_flag_wins() {
    let lock = hermetic();
    let mut first = Fixture::new(ExecMode::Layer);
    first.composed = vec![path("PATH", "/pkg/bin"), list("OPTS", "-a", " ")];
    ambient(&lock, "PATH", "/usr/bin");
    let outer = compose(&Inherited::Absent, &first.spawn());
    assert!(!applied(&outer, "PATH")[0].pre_existing);

    // The same element again: it is already on PATH (pre_existing for the new record), but the oldest wins.
    ambient(&lock, "PATH", join(&["/pkg/bin", "/usr/bin"]));
    ambient(&lock, "OPTS", "-a");
    let mut second = Fixture::new(ExecMode::Layer);
    second.composed = vec![
        path("PATH", "/pkg/bin"),
        list("OPTS", "-a", " "),
        path("PATH", "/other/bin"),
    ];
    let projected = compose(&Inherited::Readable(outer.state), &second.spawn());

    let paths = applied(&projected, "PATH");
    assert_eq!(paths.len(), 2);
    assert_eq!(paths[0].value, RecordedValue::Literal("/pkg/bin".to_owned()));
    assert!(!paths[0].pre_existing, "the oldest record's flag stays");
    assert_eq!(applied(&projected, "OPTS").len(), 1);
}

#[test]
fn a_session_record_for_an_element_an_inherited_record_names_is_not_added_twice() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_owner = session(ScopeId::Project, SessionSource::Composed);
    fixture.composed = vec![path("PATH", "/pkg/bin")];

    let projected = compose(
        &Inherited::Readable(state_of(vec![path_record("PATH", "/pkg/bin")])),
        &fixture.spawn(),
    );

    let records = applied(&projected, "PATH");
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].owner,
        Owner::Inherited,
        "the hook must not remove an outer frame's element"
    );
}

// ── `inherited_scopes` ──────────────────────────────────────────────────────────────────

#[test]
fn layer_writes_the_inherited_scopes_the_retagged_ones_and_the_composed_scope_distinct() {
    let _lock = hermetic();
    let shell = ExecState {
        applied: vec![with_owner(
            path_record("PATH", "/global/bin"),
            session(ScopeId::Global, SessionSource::Composed),
        )],
        inherited_scopes: vec![project("/outer")],
        ..ExecState::default()
    };
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_scope = Some(project("/inner"));

    let projected = compose(&Inherited::Readable(shell), &fixture.spawn());

    assert_eq!(
        projected.state.inherited_scopes,
        vec![project("/outer"), Scope::Global, project("/inner")]
    );
}

#[test]
fn isolate_and_clean_write_the_composed_scope_only() {
    let _lock = hermetic();
    let outer = ExecState {
        inherited_scopes: vec![project("/outer"), Scope::Global],
        ..ExecState::default()
    };
    for mode in [ExecMode::Isolate, ExecMode::Clean] {
        let mut fixture = Fixture::new(mode);
        fixture.composed_scope = Some(project("/inner"));
        let projected = compose(&Inherited::Readable(outer.clone()), &fixture.spawn());
        assert_eq!(projected.state.inherited_scopes, vec![project("/inner")], "{mode:?}");

        let no_scope = compose(&Inherited::Readable(outer.clone()), &Fixture::new(mode).spawn());
        assert!(no_scope.state.inherited_scopes.is_empty(), "{mode:?}");
    }
}

#[test]
fn the_seventeenth_scope_drops_the_oldest() {
    let _lock = hermetic();
    let held: Vec<Scope> = (0..MAX_INHERITED_SCOPES)
        .map(|n| project(&format!("/fixture/p{n}")))
        .collect();
    let outer = ExecState {
        inherited_scopes: held.clone(),
        ..ExecState::default()
    };
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_scope = Some(project("/fixture/newest"));

    let projected = compose(&Inherited::Readable(outer), &fixture.spawn());

    let scopes = projected.state.inherited_scopes;
    assert_eq!(scopes.len(), MAX_INHERITED_SCOPES);
    assert_eq!(scopes.first(), Some(&held[1]), "the oldest is dropped");
    assert_eq!(scopes.last(), Some(&project("/fixture/newest")));
}

#[test]
fn same_project_and_group_twice_composes_each_value_once_and_another_group_layers_on_top_with_the_scope_already_held() {
    let lock = hermetic();
    let scope = project("/fixture/project");
    let compose_group = |group: &[ComposedValue], inherited: Inherited| {
        let mut fixture = Fixture::new(ExecMode::Layer);
        fixture.composed_scope = Some(scope.clone());
        fixture.composed = group.to_vec();
        compose(&inherited, &fixture.spawn())
    };
    let g1 = [
        path("PATH", "/g1/bin"),
        constant("G1_VAR", "g1"),
        list("G1_OPTS", "-a", " "),
    ];
    let g2 = [path("PATH", "/g2/bin"), constant("G2_VAR", "g2")];

    // Outer exec: g1, nothing inherited.
    ambient(&lock, "PATH", "/usr/bin");
    let outer = compose_group(&g1, Inherited::Absent);
    assert_eq!(outer.state.inherited_scopes, vec![scope.clone()]);

    // The child environment is the outer's; the inner exec asks for g1 again.
    for (key, value) in outer.env.iter() {
        ambient(&lock, &key.to_string_lossy(), value.to_string_lossy().into_owned());
    }
    let again = compose_group(&g1, Inherited::Readable(outer.state.clone()));
    assert_eq!(get(&again.env, "PATH"), get(&outer.env, "PATH"), "each element once");
    assert_eq!(get(&again.env, "G1_OPTS").as_deref(), Some("-a"));
    assert_eq!(
        again.state.inherited_scopes,
        vec![scope.clone()],
        "the held scope is not repeated"
    );
    assert_eq!(applied(&again, "PATH").len(), 1, "one record per element");
    assert_eq!(applied(&again, "G1_VAR").len(), 1, "a repeated constant merges");
    assert_eq!(applied(&again, "G1_OPTS").len(), 1);

    // Another group on the same project, scope held: it is composed and goes in front.
    let other = compose_group(&g2, Inherited::Readable(again.state));
    assert_eq!(
        get(&other.env, "PATH"),
        Some(join(&["/g2/bin", "/g1/bin", "/usr/bin"])),
        "g2 in front, g1 reachable"
    );
    assert_eq!(get(&other.env, "G1_VAR").as_deref(), Some("g1"));
    assert_eq!(get(&other.env, "G2_VAR").as_deref(), Some("g2"));
    assert_eq!(other.state.inherited_scopes, vec![scope]);
    assert_eq!(applied(&other, "PATH").len(), 2);
}

// ── Replay ──────────────────────────────────────────────────────────────────────────────────────

fn replay_of(state: &ExecState) -> &[ReplayValue] {
    &state.replay.env
}

#[test]
fn replay_env_is_replaced_by_this_spawns_payload() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.replay_env = vec![constant("OWN", "own"), list("OPTS", "-x", ";")];
    let inherited = ExecState {
        replay: ocx_config::exec_state::Replay {
            env: vec![ReplayValue {
                key: "OUTER".to_owned(),
                kind: ValueKind::Constant,
                separator: None,
                value: ReplayPayload::Literal("outer".to_owned()),
            }],
            launch_identities: Vec::new(),
        },
        ..ExecState::default()
    };

    let projected = compose(&Inherited::Readable(inherited), &fixture.spawn());

    let replay = replay_of(&projected.state);
    assert_eq!(replay.len(), 2);
    assert_eq!(replay[0].key, "OWN");
    assert_eq!(replay[0].value, ReplayPayload::Literal("own".to_owned()));
    assert_eq!(replay[1].separator.as_deref(), Some(";"));
}

#[test]
fn a_secret_named_replay_key_replays_literally_and_a_credential_not_at_all() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    let url = format!("https://user:{SENTINEL}@host.example/x");
    fixture.replay_env = vec![
        constant("GITHUB_TOKEN", SENTINEL),
        constant("OCX_SIGNING_KEY", SENTINEL),
        constant("OCX_AUTH_FIXTURE_TOKEN", SENTINEL),
        path("SECRET_DIRS", "/fixture/dir"),
        constant("PLAIN", "plain"),
        list("MIRROR_URLS", &url, ","),
    ];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    let replay = replay_of(&projected.state);
    let keys: Vec<_> = replay.iter().map(|r| r.key.as_str()).collect();
    assert_eq!(keys, ["GITHUB_TOKEN", "SECRET_DIRS", "PLAIN", "MIRROR_URLS"]);
    assert_eq!(replay[0].value, ReplayPayload::Literal(SENTINEL.to_owned()));
    assert_eq!(replay[1].value, ReplayPayload::Literal("/fixture/dir".to_owned()));
    assert_eq!(replay[2].value, ReplayPayload::Literal("plain".to_owned()));
    assert_eq!(replay[3].value, ReplayPayload::Literal(url));
}

#[test]
fn a_credential_or_auth_key_leaves_no_trace_in_the_state_whichever_way_it_is_set() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    let secrets = [
        constant("OCX_AUTH_FIXTURE_TOKEN", SENTINEL),
        constant("OCX_SIGNING_KEY", SENTINEL),
    ];
    fixture.composed = secrets.to_vec();
    fixture.invocation = secrets.to_vec();
    fixture.replay_env = secrets.to_vec();

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert!(
        !format!("{:?}", projected.state).contains(SENTINEL),
        "no record, policy field or replay literal holds the value"
    );
    assert!(projected.state.applied.is_empty());
    assert!(projected.state.replay.env.is_empty());
}

// ── Launch identities ───────────────────────────────────────────────────────────────────────────

#[test]
fn identities_merge_inherited_retagged_with_this_spawns_one_record_per_digest() {
    let _lock = hermetic();
    let inherited = ExecState {
        replay: ocx_config::exec_state::Replay {
            env: Vec::new(),
            launch_identities: vec![
                identity(
                    DIGEST_B,
                    "ocx.sh/b:1",
                    session(ScopeId::Global, SessionSource::Composed),
                    false,
                ),
                identity(DIGEST_A, "ocx.sh/a:1", Owner::Inherited, true),
            ],
        },
        ..ExecState::default()
    };
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.launch_identities = vec![launch(DIGEST_A, "ocx.sh/a:2", false)];

    let projected = compose(&Inherited::Readable(inherited), &fixture.spawn());

    let records = &projected.state.replay.launch_identities;
    assert_eq!(records.len(), 2, "one record per digest");
    assert_eq!(records[0].digest, DIGEST_A);
    assert_eq!(
        records[0].names,
        ["ocx.sh/a:2"],
        "the newest record replaces the older whole"
    );
    assert!(!records[0].no_patches, "an older owner's opt-out is not ORed in");
    assert_eq!(records[1].digest, DIGEST_B);
    assert_eq!(records[1].owner, Owner::Inherited, "retagged");
}

#[test]
fn a_newer_opt_out_replaces_an_older_record_that_had_none() {
    let _lock = hermetic();
    let inherited = ExecState {
        replay: ocx_config::exec_state::Replay {
            env: Vec::new(),
            launch_identities: vec![identity(DIGEST_A, "ocx.sh/a:1", Owner::Inherited, false)],
        },
        ..ExecState::default()
    };
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.launch_identities = vec![launch(DIGEST_A, "ocx.sh/a:1", true)];

    let projected = compose(&Inherited::Readable(inherited), &fixture.spawn());

    let records = &projected.state.replay.launch_identities;
    assert_eq!(records.len(), 1);
    assert!(records[0].no_patches);
}

#[test]
fn duplicate_inherited_digests_collapse_to_the_newest_record() {
    let _lock = hermetic();
    let inherited = ExecState {
        replay: ocx_config::exec_state::Replay {
            env: Vec::new(),
            launch_identities: vec![
                identity(DIGEST_A, "ocx.sh/a:old", Owner::Inherited, true),
                identity(DIGEST_A, "ocx.sh/a:new", Owner::Inherited, false),
            ],
        },
        ..ExecState::default()
    };

    let projected = compose(&Inherited::Readable(inherited), &Fixture::new(ExecMode::Layer).spawn());

    let records = &projected.state.replay.launch_identities;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].names, ["ocx.sh/a:new"]);
    assert!(!records[0].no_patches);
}

#[test]
fn isolate_and_clean_keep_only_this_spawns_identities_owned_like_its_composition() {
    let _lock = hermetic();
    let inherited = ExecState {
        replay: ocx_config::exec_state::Replay {
            env: Vec::new(),
            launch_identities: vec![identity(DIGEST_B, "ocx.sh/b:1", Owner::Inherited, false)],
        },
        ..ExecState::default()
    };
    for mode in [ExecMode::Isolate, ExecMode::Clean] {
        let mut fixture = Fixture::new(mode);
        fixture.composed_owner = session(ScopeId::Global, SessionSource::Composed);
        fixture.launch_identities = vec![launch(DIGEST_A, "ocx.sh/a:1", false)];

        let projected = compose(&Inherited::Readable(inherited.clone()), &fixture.spawn());

        let records = &projected.state.replay.launch_identities;
        assert_eq!(records.len(), 1, "{mode:?}");
        assert_eq!(records[0].digest, DIGEST_A);
        assert_eq!(records[0].owner, session(ScopeId::Global, SessionSource::Composed));
    }
}

// ── Selection, session and truncation ───────────────────────────────────────────────────────────

#[test]
fn the_selection_is_the_spawns_and_the_session_cache_is_reset() {
    let _lock = hermetic();
    let pinned = Selection::Pinned(Pinned {
        tier: PinnedTier::Global,
        groups: vec!["g".to_owned()],
        with_names: Vec::new(),
        env_keys: Vec::new(),
        invocation_digest: "1".repeat(64),
        pinned_paths: Some(true),
    });
    let inherited = ExecState {
        selection: Selection::FollowCwd,
        session: SessionCache {
            token: Some(SessionToken {
                pid: 7,
                started: Some(9),
            }),
            fp: "fixture-fingerprint".to_owned(),
            ..SessionCache::default()
        },
        ..ExecState::default()
    };
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.selection = pinned.clone();

    let projected = compose(&Inherited::Readable(inherited), &fixture.spawn());

    assert_eq!(projected.state.selection, pinned);
    assert_eq!(projected.state.session, SessionCache::default());
}

#[test]
fn truncation_is_sticky_under_layer_and_isolate_and_own_under_clean() {
    let _lock = hermetic();
    let truncated = ExecState {
        truncated: Truncation::AllAppliedDropped,
        ..ExecState::default()
    };
    for (mode, expected) in [
        (ExecMode::Layer, Truncation::AllAppliedDropped),
        (ExecMode::Isolate, Truncation::AllAppliedDropped),
        (ExecMode::Clean, Truncation::None),
    ] {
        let projected = compose(&Inherited::Readable(truncated.clone()), &Fixture::new(mode).spawn());
        assert_eq!(projected.state.truncated, expected, "{mode:?}");
    }
    let unreadable = compose(&unreadable(Some(3)), &Fixture::new(ExecMode::Layer).spawn());
    assert_eq!(unreadable.state.truncated, Truncation::None);
}

#[test]
fn an_unreadable_state_yields_a_fresh_payload_without_inherited_records_or_scopes() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![constant("OWN", "own")];
    fixture.composed_scope = Some(project("/fixture/project"));

    let projected = compose(&unreadable(Some(1)), &fixture.spawn());

    assert_eq!(projected.state.applied.len(), 1);
    assert_eq!(projected.state.inherited_scopes, vec![project("/fixture/project")]);
}

// ── Warnings ────────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_secret_named_path_or_list_value_raises_no_warning() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![
        path("SECRET_DIRS", "/fixture/dir"),
        list("PRIVATE_FLAGS", "-x", " "),
        constant("GITHUB_TOKEN", SENTINEL),
    ];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert!(projected.warnings.is_empty(), "{:?}", projected.warnings);
}

#[test]
fn a_list_value_carrying_url_userinfo_is_applied_and_recorded_verbatim() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    let url = format!("https://user:{SENTINEL}@host.example/x");
    fixture.composed = vec![list("MIRROR_URLS", &url, ",")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(get(&projected.env, "MIRROR_URLS").as_deref(), Some(url.as_str()));
    assert_eq!(applied(&projected, "MIRROR_URLS")[0].value, RecordedValue::Literal(url));
}

#[test]
fn an_unreadable_state_in_isolate_names_the_major_and_that_it_cannot_isolate() {
    let _lock = hermetic();
    let projected = compose(&unreadable(Some(3)), &Fixture::new(ExecMode::Isolate).spawn());

    assert_eq!(
        projected.warnings,
        [
            ProjectionWarning::InheritedUnreadable {
                received_major: Some(3),
                reason: ocx_config::exec_state::UnreadableReason::OtherMajor
            },
            ProjectionWarning::CannotIsolate
        ]
    );
}

#[test]
fn a_readable_helper_is_exercised() {
    // Keeps `readable` linked for tests that build a state from records alone.
    let state = readable(vec![path_record("PATH", "/x")]);
    assert!(matches!(state, Inherited::Readable(_)));
}
