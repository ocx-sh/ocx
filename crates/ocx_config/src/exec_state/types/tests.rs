// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeSet;
use std::path::PathBuf;

use super::*;

fn session(scope: ScopeId) -> Owner {
    Owner::Session {
        scope,
        source: SessionSource::Composed,
    }
}

fn record(key: &str, owner: Owner) -> AppliedValue {
    AppliedValue {
        key: key.to_owned(),
        value: RecordedValue::Literal("v".to_owned()),
        kind: ValueKind::Constant,
        separator: None,
        pre_existing: false,
        owner,
        prior: Some(Prior::Unset),
    }
}

fn project(root: &std::path::Path) -> Scope {
    Scope::Project {
        root: root.to_path_buf(),
    }
}

fn collect_keys(value: &serde_json::Value, into: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                into.insert(key.clone());
                collect_keys(inner, into);
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| collect_keys(item, into)),
        _ => {}
    }
}

/// Every optional field set, every enum variant that carries data present.
fn fully_populated() -> ExecState {
    let owner_project = session(ScopeId::Project);
    ExecState {
        narrowing: Narrowing::default(),
        home: None,
        minor: 7,
        policy: Policy {
            remote: Some(true),
            no_project: Some(true),
            no_config_refresh: Some(true),
            no_update_check: Some(true),
            config_path: Some(PathBuf::from("/c")),
            no_config: Some(true),
            index_dir: Some(PathBuf::from("/i")),
            toolchain_dir: Some(PathBuf::from("/t")),
            mirrors: Some("m".to_owned()),
            patches: Some("p".to_owned()),
            patch_snapshot_path: Some(PathBuf::from("/s")),
            managed_config: Some("mc".to_owned()),
            no_verify: Some(true),
            records_dir: Some(PathBuf::from("/r")),
            records_name: Some("rn".to_owned()),
            allow_yanked: Some(true),
            default_registry: Some("reg".to_owned()),
            insecure_registries: Some("h:1".to_owned()),
        },
        applied: vec![
            AppliedValue {
                key: "K".to_owned(),
                value: RecordedValue::Literal("v".to_owned()),
                kind: ValueKind::List,
                separator: Some(":".to_owned()),
                pre_existing: true,
                owner: owner_project.clone(),
                prior: Some(Prior::Value("p".to_owned())),
            },
            record("P", Owner::Inherited),
        ],
        inherited_scopes: vec![Scope::Global, project(&PathBuf::from("/p"))],
        replay: Replay {
            env: vec![ReplayValue {
                key: "R".to_owned(),
                kind: ValueKind::List,
                separator: Some(":".to_owned()),
                value: ReplayPayload::Literal("x".to_owned()),
            }],
            launch_identities: vec![IdentityRecord {
                digest: "sha256:0".to_owned(),
                names: vec!["n".to_owned()],
                owner: owner_project,
                no_patches: true,
            }],
        },
        selection: Selection::Pinned(Pinned {
            tier: PinnedTier::Project {
                dir: PathBuf::from("/d"),
            },
            groups: vec!["g".to_owned()],
            with_names: vec!["w".to_owned()],
            env_keys: vec!["e".to_owned()],
            invocation_digest: "0".to_owned(),
            pinned_paths: Some(true),
        }),
        session: SessionCache {
            token: Some(SessionToken {
                pid: 1,
                started: Some(2),
            }),
            fp: "f".to_owned(),
            verdict: Some(Verdict::Inert),
            tiers: vec![PathBuf::from("/tier")],
            ws: "w".to_owned(),
            messages_fp: "m".to_owned(),
            project: Some(ProjectScopeHint {
                key: "k".to_owned(),
                dir: PathBuf::from("/d"),
            }),
        },
        truncated: Truncation::InheritedDropped,
    }
}

#[test]
fn payload_field_names_match_golden() {
    let mut names = BTreeSet::new();
    collect_keys(&serde_json::to_value(fully_populated()).unwrap(), &mut names);
    // The second populated shape reaches the variants the first does not.
    let mut other = fully_populated();
    other.applied.push(AppliedValue {
        value: RecordedValue::Literal("v".into()),
        ..record("S", session(ScopeId::Global))
    });
    other.selection = Selection::Pinned(Pinned {
        tier: PinnedTier::Global,
        ..match fully_populated().selection {
            Selection::Pinned(pinned) => pinned,
            Selection::FollowCwd => unreachable!(),
        }
    });
    collect_keys(&serde_json::to_value(other).unwrap(), &mut names);

    let golden: Vec<String> = serde_json::from_str(include_str!("field_names.golden.json")).unwrap();
    let golden: BTreeSet<String> = golden.into_iter().collect();
    assert_eq!(
        names, golden,
        "payload field names changed: bump MAJOR or keep the old name readable with a serde alias, then update the golden"
    );
}

#[test]
fn unknown_payload_fields_are_ignored_and_missing_fields_default() {
    let state: ExecState = serde_json::from_str(r#"{"minor":9,"future_field":{"a":1}}"#).unwrap();
    assert_eq!(state.minor, 9);
    assert!(state.applied.is_empty());
    assert_eq!(ExecState::default().minor, MINOR);
}

#[test]
fn retag_sessions_retags_records_and_identities_and_adds_their_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(dir.path()).unwrap();
    let mut state = ExecState {
        applied: vec![
            record("A", session(ScopeId::Global)),
            record("B", session(ScopeId::Project)),
            record("C", Owner::Inherited),
        ],
        selection: Selection::Pinned(Pinned {
            tier: PinnedTier::Project { dir: root.clone() },
            groups: vec![],
            with_names: vec![],
            env_keys: vec![],
            invocation_digest: String::new(),
            pinned_paths: None,
        }),
        replay: Replay {
            env: vec![],
            launch_identities: vec![IdentityRecord {
                digest: "sha256:0".to_owned(),
                names: vec![],
                owner: session(ScopeId::Global),
                no_patches: false,
            }],
        },
        ..ExecState::default()
    };
    state.retag_sessions();
    assert!(state.applied.iter().all(|r| r.owner == Owner::Inherited));
    assert_eq!(state.replay.launch_identities[0].owner, Owner::Inherited);
    assert_eq!(state.inherited_scopes, vec![Scope::Global, project(&root)]);
}

#[test]
fn retag_sessions_takes_the_project_root_from_the_session_hint_when_not_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(dir.path()).unwrap();
    let mut state = ExecState {
        applied: vec![record("B", session(ScopeId::Project))],
        session: SessionCache {
            project: Some(ProjectScopeHint {
                key: "k".to_owned(),
                dir: root.clone(),
            }),
            ..SessionCache::default()
        },
        ..ExecState::default()
    };
    state.retag_sessions();
    assert_eq!(state.inherited_scopes, vec![project(&root)]);
}

#[test]
fn add_scope_keeps_scopes_distinct_in_first_seen_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(dir.path()).unwrap();
    let mut state = ExecState::default();
    state.add_scope(project(&root));
    state.add_scope(Scope::Global);
    state.add_scope(project(&root));
    state.add_scope(Scope::Global);
    assert_eq!(state.inherited_scopes, vec![project(&root), Scope::Global]);
}

#[test]
fn add_scope_drops_the_oldest_above_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let roots: Vec<PathBuf> = (0..=MAX_INHERITED_SCOPES)
        .map(|n| {
            let path = dir.path().join(format!("p{n}"));
            std::fs::create_dir(&path).unwrap();
            dunce::canonicalize(path).unwrap()
        })
        .collect();
    let mut state = ExecState::default();
    for root in &roots {
        state.add_scope(project(root));
    }
    assert_eq!(roots.len(), MAX_INHERITED_SCOPES + 1);
    assert_eq!(state.inherited_scopes.len(), MAX_INHERITED_SCOPES);
    assert_eq!(state.inherited_scopes.first(), Some(&project(&roots[1])));
    assert_eq!(
        state.inherited_scopes.last(),
        Some(&project(&roots[MAX_INHERITED_SCOPES]))
    );
}

#[cfg(unix)]
#[test]
fn add_scope_keeps_a_symlinked_spelling_of_a_known_root_once() {
    let dir = tempfile::tempdir().unwrap();
    let real = dunce::canonicalize(dir.path()).unwrap().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut state = ExecState::default();
    state.add_scope(project(&real));
    state.add_scope(project(&link));
    assert_eq!(state.inherited_scopes, vec![project(&real)]);
}

/// EC-LEDGER-008: a received root is a label, never resolved or turned into a path.
/// A received root is compared only, never resolved: a stored symlinked spelling stays distinct
/// from its target, so no filesystem call reaches a path the state names.
#[cfg(unix)]
#[test]
fn add_scope_never_resolves_a_received_root() {
    let dir = tempfile::tempdir().unwrap();
    let real = dunce::canonicalize(dir.path()).unwrap().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut state = ExecState {
        inherited_scopes: vec![project(&link)],
        ..ExecState::default()
    };
    state.add_scope(project(&real));
    assert_eq!(state.inherited_scopes, vec![project(&link), project(&real)]);
}

/// The session hint dir is state-derived: retagging adds it as written, never resolved.
#[cfg(unix)]
#[test]
fn retag_sessions_adds_the_received_hint_dir_as_written() {
    let dir = tempfile::tempdir().unwrap();
    let real = dunce::canonicalize(dir.path()).unwrap().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut state = ExecState {
        applied: vec![record("B", session(ScopeId::Project))],
        session: SessionCache {
            project: Some(ProjectScopeHint {
                key: "k".to_owned(),
                dir: link.clone(),
            }),
            ..SessionCache::default()
        },
        ..ExecState::default()
    };
    state.retag_sessions();
    assert_eq!(state.inherited_scopes, vec![project(&link)]);
}

/// The header is the single source of `narrowing` and `home`: a payload naming them is ignored.
#[test]
fn a_payload_cannot_set_header_fields() {
    let state: ExecState =
        serde_json::from_str(r#"{"home":"/forged","narrowing":{"offline":true,"no_consent":true}}"#).unwrap();
    assert_eq!(state.home, None);
    assert_eq!(state.narrowing, Narrowing::default());
}
