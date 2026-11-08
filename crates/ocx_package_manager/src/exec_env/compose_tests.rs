// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};

use ocx_config::exec_state::{
    AppliedValue, ComposedValue, ExecState, HeaderView, Inherited, Narrowing, Owner, PolicyLayer, Prior, RecordedValue,
    Scope, ScopeId, Selection, SessionSource, Truncation, UnreadableReason, ValueKind,
};

use super::{ExecMode, LaunchIdentity, ProjectionWarning, Spawn, compose};

/// A hermetic environment holding exactly the keys a test sets.
pub(super) fn hermetic() -> ocx_env::overrides::EnvLock {
    let lock = ocx_env::overrides::lock();
    lock.hermetic(std::env::temp_dir());
    lock
}

/// Sets `key` in the hermetic environment, through its declaration when the registry owns the name.
pub(super) fn ambient(lock: &ocx_env::overrides::EnvLock, key: &str, val: impl Into<String>) {
    match ocx_env::all().find(|var| var.name == key) {
        Some(var) => lock.set(var, val),
        None => lock.set_raw(key, val),
    }
}

/// Everything a [`Spawn`] borrows, owned, so a test can mutate one field and take the spawn.
pub(super) struct Fixture {
    pub(super) mode: ExecMode,
    pub(super) written: PolicyLayer,
    pub(super) binary_pin: Option<PathBuf>,
    pub(super) composed: Vec<ComposedValue>,
    pub(super) composed_owner: Owner,
    pub(super) composed_scope: Option<Scope>,
    pub(super) invocation: Vec<ComposedValue>,
    pub(super) replay_env: Vec<ComposedValue>,
    pub(super) launch_identities: Vec<LaunchIdentity>,
    pub(super) selection: Selection,
}

impl Fixture {
    pub(super) fn new(mode: ExecMode) -> Self {
        Self {
            mode,
            written: PolicyLayer::default(),
            binary_pin: None,
            composed: Vec::new(),
            composed_owner: Owner::Inherited,
            composed_scope: None,
            invocation: Vec::new(),
            replay_env: Vec::new(),
            launch_identities: Vec::new(),
            selection: Selection::FollowCwd,
        }
    }

    pub(super) fn spawn(&self) -> Spawn<'_> {
        Spawn {
            mode: self.mode,
            written: &self.written,
            binary_pin: self.binary_pin.as_deref(),
            composed: &self.composed,
            composed_owner: self.composed_owner.clone(),
            composed_scope: self.composed_scope.clone(),
            invocation: &self.invocation,
            replay_env: &self.replay_env,
            launch_identities: &self.launch_identities,
            selection: self.selection.clone(),
        }
    }
}

pub(super) fn value(key: &str, value: &str, kind: ValueKind) -> ComposedValue {
    ComposedValue {
        key: key.to_owned(),
        value: value.to_owned(),
        kind,
        separator: None,
    }
}

pub(super) fn constant(key: &str, val: &str) -> ComposedValue {
    value(key, val, ValueKind::Constant)
}

pub(super) fn path(key: &str, val: &str) -> ComposedValue {
    value(key, val, ValueKind::Path)
}

pub(super) fn list(key: &str, val: &str, separator: &str) -> ComposedValue {
    ComposedValue {
        separator: Some(separator.to_owned()),
        ..value(key, val, ValueKind::List)
    }
}

pub(super) fn record(key: &str, recorded: RecordedValue, kind: ValueKind) -> AppliedValue {
    AppliedValue {
        key: key.to_owned(),
        value: recorded,
        kind,
        separator: None,
        pre_existing: false,
        owner: Owner::Inherited,
        prior: None,
    }
}

pub(super) fn constant_record(key: &str, val: &str, prior: Prior) -> AppliedValue {
    AppliedValue {
        prior: Some(prior),
        ..record(key, RecordedValue::Literal(val.to_owned()), ValueKind::Constant)
    }
}

pub(super) fn path_record(key: &str, element: &str) -> AppliedValue {
    record(key, RecordedValue::Literal(element.to_owned()), ValueKind::Path)
}

pub(super) fn readable(applied: Vec<AppliedValue>) -> Inherited {
    Inherited::Readable(ExecState {
        applied,
        ..ExecState::default()
    })
}

pub(super) fn unreadable(major: Option<u32>) -> Inherited {
    Inherited::Unreadable {
        header: HeaderView {
            major,
            ..HeaderView::default()
        },
        reason: UnreadableReason::OtherMajor,
    }
}

pub(super) fn join(segments: &[&str]) -> String {
    std::env::join_paths(segments)
        .expect("fixture segments hold no separator")
        .into_string()
        .expect("fixture segments are utf-8")
}

pub(super) fn get(env: &ocx_config::env::Env, key: &str) -> Option<String> {
    env.get(key).map(|v| v.to_string_lossy().into_owned())
}

pub(super) fn applied<'a>(projected: &'a super::Projected, key: &str) -> Vec<&'a AppliedValue> {
    projected.state.applied.iter().filter(|r| r.key == key).collect()
}

// ── Start environment per mode ──────────────────────────────────────────────────────────────────

#[test]
fn layer_composes_on_top_of_the_calling_environment() {
    let lock = hermetic();
    ambient(&lock, "PATH", join(&["/usr/bin", "/bin"]));
    ambient(&lock, "KEEP_ME", "ambient");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![path("PATH", "/pkg/bin"), constant("JAVA_HOME", "/jdk")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(
        get(&projected.env, "PATH"),
        Some(join(&["/pkg/bin", "/usr/bin", "/bin"]))
    );
    assert_eq!(get(&projected.env, "KEEP_ME").as_deref(), Some("ambient"));
    assert_eq!(get(&projected.env, "JAVA_HOME").as_deref(), Some("/jdk"));
    assert!(projected.warnings.is_empty());
}

#[test]
fn layer_does_not_revert_what_an_outer_frame_applied() {
    let lock = hermetic();
    ambient(&lock, "PATH", join(&["/outer/bin", "/usr/bin"]));
    let inherited = readable(vec![path_record("PATH", "/outer/bin")]);
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![path("PATH", "/inner/bin")];

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(
        get(&projected.env, "PATH"),
        Some(join(&["/inner/bin", "/outer/bin", "/usr/bin"]))
    );
    assert!(projected.warnings.is_empty());
}

#[test]
fn clean_starts_empty_and_keeps_only_what_was_composed() {
    let lock = hermetic();
    ambient(&lock, "PATH", "/usr/bin");
    ambient(&lock, "KEEP_ME", "ambient");
    lock.set(&ocx_env::OCX_REMOTE, "1");
    let mut fixture = Fixture::new(ExecMode::Clean);
    fixture.composed = vec![constant("JAVA_HOME", "/jdk"), path("PATH", "/pkg/bin")];
    fixture.binary_pin = Some(PathBuf::from("/opt/ocx/bin/ocx"));

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    let mut keys: Vec<String> = projected
        .env
        .iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    keys.sort();
    assert_eq!(keys, ["JAVA_HOME", "OCX_BINARY_PIN", "PATH"]);
    assert_eq!(get(&projected.env, "PATH").as_deref(), Some("/pkg/bin"));
}

#[test]
fn clean_does_not_revert_and_records_only_this_spawn() {
    let _lock = hermetic();
    let inherited = readable(vec![constant_record("JAVA_HOME", "/old", Prior::Unset)]);
    let mut fixture = Fixture::new(ExecMode::Clean);
    fixture.composed = vec![constant("JAVA_HOME", "/new")];

    let projected = compose(&inherited, &fixture.spawn());

    assert!(
        projected.warnings.is_empty(),
        "clean reverts nothing, so nothing is kept"
    );
    assert_eq!(projected.state.applied.len(), 1);
    assert_eq!(projected.state.applied[0].value, RecordedValue::Literal("/new".into()));
}

// ── What a child never receives ─────────────────────────────────────────────────────────────────

#[test]
fn credentials_and_hook_local_variables_are_removed_in_every_mode() {
    for mode in [ExecMode::Layer, ExecMode::Isolate, ExecMode::Clean] {
        let lock = hermetic();
        lock.set(ocx_env::OCX_SIGNING_KEY.declaration(), "fixture-secret-value");
        lock.set(&ocx_env::__OCX_ENV_PWD, "fixture-hook-bookkeeping");
        lock.set(&ocx_env::__OCX_CREDENTIAL_HELPER, "1");
        lock.set(&ocx_env::__OCX_ENV_STOOD_DOWN, "fixture-stood-down");
        let fixture = Fixture::new(mode);

        let projected = compose(&Inherited::Absent, &fixture.spawn());

        for key in ocx_env::keys_with(ocx_env::Propagation::Credential)
            .chain(ocx_env::keys_with(ocx_env::Propagation::HookLocal))
        {
            assert!(projected.env.get(key).is_none(), "{key} reached a child under {mode:?}");
        }
    }
}

#[test]
fn a_secret_input_and_a_public_variable_pass_through_layer_and_isolate_but_not_clean() {
    for (mode, passes) in [
        (ExecMode::Layer, true),
        (ExecMode::Isolate, true),
        (ExecMode::Clean, false),
    ] {
        let lock = hermetic();
        lock.set(ocx_env::OCX_ANNOUNCE_TOKEN.declaration(), "fixture-secret-value");
        ambient(&lock, "OCX_AUTH_FIXTURE_TOKEN", "fixture-secret-value");
        lock.set(&ocx_env::OCX_REMOTE, "1");
        let fixture = Fixture::new(mode);

        let projected = compose(&Inherited::Absent, &fixture.spawn());

        for key in [
            ocx_env::OCX_ANNOUNCE_TOKEN.declaration().name,
            "OCX_AUTH_FIXTURE_TOKEN",
            ocx_env::OCX_REMOTE.name,
        ] {
            assert_eq!(projected.env.get(key).is_some(), passes, "{key} under {mode:?}");
        }
    }
}

#[test]
fn compose_writes_no_public_ocx_variable_of_its_own() {
    let lock = hermetic();
    ambient(&lock, "KEEP_ME", "ambient");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.written.narrowing = Narrowing {
        offline: true,
        ..Narrowing::default()
    };
    fixture.written.policy.remote = Some(true);

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    let ocx_keys: Vec<_> = projected
        .env
        .iter()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .filter(|k| k.starts_with("OCX_") && k != "OCX_BINARY_PIN")
        .collect();
    assert!(
        ocx_keys.is_empty(),
        "policy travels in state, not in variables: {ocx_keys:?}"
    );
}

// ── The binary pin ──────────────────────────────────────────────────────────────────────────────

#[test]
fn the_pin_is_set_overwriting_an_ambient_one() {
    let lock = hermetic();
    lock.set(&ocx_env::OCX_BINARY_PIN, "/old/ocx");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.binary_pin = Some(PathBuf::from("/new/ocx"));

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(get(&projected.env, "OCX_BINARY_PIN").as_deref(), Some("/new/ocx"));
}

#[test]
fn an_unknown_pin_removes_the_variable_instead_of_leaving_a_stale_or_empty_one() {
    for pin in [None, Some(PathBuf::new())] {
        let lock = hermetic();
        lock.set(&ocx_env::OCX_BINARY_PIN, "/stale/ocx");
        let mut fixture = Fixture::new(ExecMode::Layer);
        fixture.binary_pin = pin.clone();

        let projected = compose(&Inherited::Absent, &fixture.spawn());

        assert!(
            projected.env.get("OCX_BINARY_PIN").is_none(),
            "pin {pin:?} left a value"
        );
    }
}

#[test]
fn a_composed_value_never_overrides_the_pin() {
    // A launcher runs whatever the pin names, so the projection's value is the last word.
    for pin in [Some("/pinned/ocx"), None] {
        let _lock = hermetic();
        let mut fixture = Fixture::new(ExecMode::Layer);
        fixture.binary_pin = pin.map(PathBuf::from);
        fixture.composed = vec![constant(ocx_env::OCX_BINARY_PIN.name, "/fixture/other-ocx")];

        let projected = compose(&Inherited::Absent, &fixture.spawn());

        assert_eq!(get(&projected.env, ocx_env::OCX_BINARY_PIN.name).as_deref(), pin);
    }
}

// ── Header and policy ───────────────────────────────────────────────────────────────────────────

#[test]
fn the_state_carries_the_written_header_and_policy() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.written.narrowing = Narrowing {
        offline: true,
        frozen: true,
        ..Narrowing::default()
    };
    fixture.written.home = Some(Path::new("/fixture/home").to_path_buf());
    fixture.written.policy.remote = Some(true);

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(projected.state.narrowing, fixture.written.narrowing);
    assert_eq!(projected.state.home, fixture.written.home);
    assert_eq!(projected.state.policy.remote, Some(true));
}

// ── Composition records ─────────────────────────────────────────────────────────────────────────

#[test]
fn a_path_element_that_was_already_present_is_recorded_pre_existing() {
    let lock = hermetic();
    ambient(&lock, "PATH", join(&["/usr/bin", "/bin"]));
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![path("PATH", "/usr/bin"), path("PATH", "/pkg/bin")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(
        get(&projected.env, "PATH"),
        Some(join(&["/pkg/bin", "/usr/bin", "/bin"]))
    );
    let records = applied(&projected, "PATH");
    assert_eq!(records.len(), 2);
    assert!(records[0].pre_existing, "/usr/bin was already on PATH");
    assert!(!records[1].pre_existing);
    assert_eq!(records[1].value, RecordedValue::Literal("/pkg/bin".into()));
}

#[test]
fn a_list_element_that_was_already_present_is_recorded_pre_existing() {
    let lock = hermetic();
    ambient(&lock, "JAVA_OPTS", "-ea -Xmx1g");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![list("JAVA_OPTS", "-ea", " "), list("JAVA_OPTS", "-verbose", " ")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(get(&projected.env, "JAVA_OPTS").as_deref(), Some("-Xmx1g -ea -verbose"));
    let records = applied(&projected, "JAVA_OPTS");
    assert_eq!(records.len(), 2);
    assert!(records[0].pre_existing);
    assert!(!records[1].pre_existing);
    assert_eq!(records[1].separator.as_deref(), Some(" "));
}

/// EC-CONST-006: the prior is the live value at apply time, so a coincidence with the typed value is kept as the prior.
#[test]
fn a_constant_records_the_prior_it_replaced() {
    let lock = hermetic();
    ambient(&lock, "JAVA_HOME", "/ambient/jdk");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![constant("JAVA_HOME", "/jdk"), constant("FRESH", "x")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(
        applied(&projected, "JAVA_HOME")[0].prior,
        Some(Prior::Value("/ambient/jdk".into()))
    );
    assert_eq!(applied(&projected, "FRESH")[0].prior, Some(Prior::Unset));
}

/// EC-CONST-008: a set-but-empty prior is recorded as a value, never collapsed to unset.
#[test]
fn a_set_but_empty_prior_is_not_unset() {
    let lock = hermetic();
    ambient(&lock, "EMPTY_ONE", "");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![constant("EMPTY_ONE", "x")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(
        applied(&projected, "EMPTY_ONE")[0].prior,
        Some(Prior::Value(String::new()))
    );
}

#[test]
fn every_prior_is_unset_under_clean() {
    let lock = hermetic();
    ambient(&lock, "JAVA_HOME", "/ambient/jdk");
    let mut fixture = Fixture::new(ExecMode::Clean);
    fixture.composed = vec![constant("JAVA_HOME", "/jdk"), constant("OTHER", "y")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    for record in &projected.state.applied {
        assert_eq!(record.prior, Some(Prior::Unset), "{} under clean", record.key);
    }
}

#[test]
fn a_prior_is_captured_against_the_isolated_start_environment() {
    let lock = hermetic();
    ambient(&lock, "JAVA_HOME", "/outer");
    let inherited = readable(vec![constant_record("JAVA_HOME", "/outer", Prior::Unset)]);
    let mut fixture = Fixture::new(ExecMode::Isolate);
    fixture.composed = vec![constant("JAVA_HOME", "/inner")];

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(applied(&projected, "JAVA_HOME")[0].prior, Some(Prior::Unset));
}

#[test]
fn a_key_composed_twice_in_one_spawn_records_once_against_the_start_environment() {
    let lock = hermetic();
    ambient(&lock, "JAVA_HOME", "/ambient/jdk");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![
        constant("JAVA_HOME", "/first"),
        constant("JAVA_HOME", "/second"),
        path("PATH", "/pkg/bin"),
        path("PATH", "/pkg/bin"),
    ];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    let constants = applied(&projected, "JAVA_HOME");
    assert_eq!(constants.len(), 1);
    assert_eq!(constants[0].value, RecordedValue::Literal("/second".into()));
    assert_eq!(constants[0].prior, Some(Prior::Value("/ambient/jdk".into())));
    let paths = applied(&projected, "PATH");
    assert_eq!(paths.len(), 1);
    assert!(
        !paths[0].pre_existing,
        "the second application is not a pre-existing element"
    );
}

#[test]
fn owners_follow_the_spawn_and_the_invocation_source() {
    let _lock = hermetic();
    let session = Owner::Session {
        scope: ScopeId::Project,
        source: SessionSource::Composed,
    };
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_owner = session.clone();
    fixture.composed = vec![constant("FROM_GROUP", "g")];
    fixture.invocation = vec![constant("FROM_FLAG", "f")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(applied(&projected, "FROM_GROUP")[0].owner, session);
    assert_eq!(
        applied(&projected, "FROM_FLAG")[0].owner,
        Owner::Session {
            scope: ScopeId::Project,
            source: SessionSource::Invocation
        }
    );
}

#[test]
fn an_exec_composition_is_owned_by_the_inherited_owner() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![constant("FROM_GROUP", "g")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(applied(&projected, "FROM_GROUP")[0].owner, Owner::Inherited);
}

// ── Secrets ───────────────────────────────────────────────────────────────────────────────

#[test]
fn a_credential_or_auth_key_is_applied_but_never_recorded() {
    let lock = hermetic();
    ambient(&lock, "OCX_AUTH_FIXTURE_TOKEN", "fixture-ambient-secret");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![
        constant("OCX_SIGNING_KEY", "fixture-secret-value"),
        constant("OCX_AUTH_FIXTURE_TOKEN", "fixture-secret-value"),
    ];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert!(
        projected.state.applied.is_empty(),
        "no record for a credential or an auth key"
    );
    assert_eq!(
        get(&projected.env, "OCX_SIGNING_KEY").as_deref(),
        Some("fixture-secret-value")
    );
    assert_eq!(
        get(&projected.env, "OCX_AUTH_FIXTURE_TOKEN").as_deref(),
        Some("fixture-secret-value")
    );
}

#[test]
fn a_secret_named_constant_is_recorded_verbatim_with_its_prior() {
    let lock = hermetic();
    ambient(&lock, "ALREADY_TOKEN", "ambient-sentinel");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![
        constant("GITHUB_TOKEN", "tok-sentinel"),
        constant("ALREADY_TOKEN", "tok-sentinel"),
        constant("PLAIN_URL", "https://user:tok-sentinel@host.example/x"),
    ];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    let fresh = applied(&projected, "GITHUB_TOKEN")[0];
    assert_eq!(fresh.value, RecordedValue::Literal("tok-sentinel".into()));
    assert_eq!(fresh.prior, Some(Prior::Unset));
    let replaced = applied(&projected, "ALREADY_TOKEN")[0];
    assert_eq!(replaced.value, RecordedValue::Literal("tok-sentinel".into()));
    assert_eq!(replaced.prior, Some(Prior::Value("ambient-sentinel".into())));
    assert_eq!(
        applied(&projected, "PLAIN_URL")[0].value,
        RecordedValue::Literal("https://user:tok-sentinel@host.example/x".into())
    );
    assert_eq!(get(&projected.env, "GITHUB_TOKEN").as_deref(), Some("tok-sentinel"));
}

#[test]
fn a_secret_named_path_or_list_value_is_applied_and_recorded() {
    let lock = hermetic();
    ambient(&lock, "SECRET_DIRS", "/existing");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![path("SECRET_DIRS", "/fixture/dir"), list("PRIVATE_FLAGS", "-x", " ")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(
        applied(&projected, "SECRET_DIRS")[0].value,
        RecordedValue::Literal("/fixture/dir".into())
    );
    assert_eq!(
        applied(&projected, "PRIVATE_FLAGS")[0].value,
        RecordedValue::Literal("-x".into())
    );
    assert_eq!(
        get(&projected.env, "SECRET_DIRS"),
        Some(join(&["/fixture/dir", "/existing"]))
    );
    assert_eq!(get(&projected.env, "PRIVATE_FLAGS").as_deref(), Some("-x"));
}

#[test]
fn path_is_never_recorded_as_a_constant() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![constant("PATH", "/only/this")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(get(&projected.env, "PATH").as_deref(), Some("/only/this"));
    assert!(
        projected.state.applied.is_empty(),
        "a constant PATH record would make the state unreadable"
    );
}

#[test]
fn pathext_is_never_recorded_as_a_constant() {
    let _lock = hermetic();
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![constant("PATHEXT", ".COM;.EXE")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(get(&projected.env, "PATHEXT").as_deref(), Some(".COM;.EXE"));
    assert!(
        projected.state.applied.is_empty(),
        "a constant PATHEXT record would make the state unreadable"
    );
}

#[test]
fn a_prior_carrying_url_userinfo_is_recorded_verbatim() {
    let lock = hermetic();
    ambient(&lock, "REMOTE_URL", "https://user:tok-sentinel@host.example/x");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![constant("REMOTE_URL", "https://host.example/x")];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    let record = applied(&projected, "REMOTE_URL")[0];
    assert_eq!(record.value, RecordedValue::Literal("https://host.example/x".into()));
    assert_eq!(
        record.prior,
        Some(Prior::Value("https://user:tok-sentinel@host.example/x".into()))
    );
}

#[cfg(unix)]
#[test]
fn a_non_utf8_prior_is_withheld() {
    use std::os::unix::ffi::OsStringExt;
    // Through `record_of`: the hermetic environment holds only UTF-8 values.
    let mut start = ocx_config::env::Env::clean();
    start.set("BYTES_HOME", std::ffi::OsString::from_vec(b"/opt/\xff".to_vec()));

    let record = super::record_of(&start, &constant("BYTES_HOME", "/jdk"), &Owner::Inherited)
        .expect("an ordinary constant is recorded");

    assert_eq!(record.prior, Some(Prior::Withheld));
}

// ── Isolate ─────────────────────────────────────────────────────────────────────────────────────

#[test]
fn isolate_reverts_a_depth_two_chain_to_the_value_before_ocx() {
    let lock = hermetic();
    ambient(&lock, "JAVA_HOME", "/b");
    let inherited = readable(vec![
        constant_record("JAVA_HOME", "/a", Prior::Value("/orig".into())),
        constant_record("JAVA_HOME", "/b", Prior::Value("/a".into())),
    ]);
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(get(&projected.env, "JAVA_HOME").as_deref(), Some("/orig"));
    assert!(projected.warnings.is_empty());
}

#[test]
fn isolate_removes_a_path_element_newest_first_and_composes_on_the_pristine_value() {
    let lock = hermetic();
    ambient(&lock, "PATH", join(&["/b/bin", "/a/bin", "/usr/bin"]));
    let inherited = readable(vec![path_record("PATH", "/a/bin"), path_record("PATH", "/b/bin")]);
    let mut fixture = Fixture::new(ExecMode::Isolate);
    fixture.composed = vec![path("PATH", "/c/bin")];

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(get(&projected.env, "PATH"), Some(join(&["/c/bin", "/usr/bin"])));
    assert!(projected.warnings.is_empty());
    assert_eq!(
        projected.state.applied.len(),
        1,
        "isolate records this spawn's values only"
    );
}

#[test]
fn isolate_keeps_a_pre_existing_path_element_without_counting_it() {
    let lock = hermetic();
    ambient(&lock, "PATH", join(&["/mine/bin", "/a/bin"]));
    let mut kept = path_record("PATH", "/mine/bin");
    kept.pre_existing = true;
    let inherited = readable(vec![kept, path_record("PATH", "/a/bin")]);
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(get(&projected.env, "PATH"), Some("/mine/bin".to_owned()));
    assert!(
        projected.warnings.is_empty(),
        "a pre-existing element is an expected survivor, not a failure: {:?}",
        projected.warnings
    );
}

#[test]
fn isolate_keeps_a_constant_the_user_changed_and_counts_it() {
    let lock = hermetic();
    ambient(&lock, "JAVA_HOME", "/user/jdk");
    let inherited = readable(vec![constant_record("JAVA_HOME", "/a", Prior::Unset)]);
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(get(&projected.env, "JAVA_HOME").as_deref(), Some("/user/jdk"));
    assert_eq!(
        projected.warnings,
        [ProjectionWarning::CannotFullyIsolate {
            kept_changed: 1,
            kept_withheld: 0
        }]
    );
}

#[test]
fn isolate_restores_the_users_value_and_keeps_the_older_record_after_a_user_change() {
    // X-2: `ocx exec` sets JAVA_HOME=a, the user sets u, a nested `ocx exec` sets b; the records stay separate.
    let lock = hermetic();
    ambient(&lock, "JAVA_HOME", "/b");
    let inherited = readable(vec![
        constant_record("JAVA_HOME", "/a", Prior::Unset),
        constant_record("JAVA_HOME", "/b", Prior::Value("/u".into())),
    ]);
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(get(&projected.env, "JAVA_HOME").as_deref(), Some("/u"));
    assert_eq!(
        projected.warnings,
        [ProjectionWarning::CannotFullyIsolate {
            kept_changed: 1,
            kept_withheld: 0
        }]
    );
}

#[test]
fn isolate_keeps_a_withheld_prior_and_counts_it() {
    let lock = hermetic();
    ambient(&lock, "API_TOKEN", "fixture-secret-value");
    let inherited = readable(vec![constant_record(
        "API_TOKEN",
        "fixture-secret-value",
        Prior::Withheld,
    )]);
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(
        get(&projected.env, "API_TOKEN").as_deref(),
        Some("fixture-secret-value")
    );
    assert_eq!(
        projected.warnings,
        [ProjectionWarning::CannotFullyIsolate {
            kept_changed: 0,
            kept_withheld: 1
        }]
    );
}

#[test]
fn isolate_removes_a_secret_named_constant_only_while_it_holds_the_recorded_value() {
    let lock = hermetic();
    ambient(&lock, "GITHUB_TOKEN", "tok-sentinel");
    ambient(&lock, "OTHER_TOKEN", "mine");
    let inherited = readable(vec![
        constant_record("GITHUB_TOKEN", "tok-sentinel", Prior::Unset),
        constant_record("OTHER_TOKEN", "tok-sentinel", Prior::Unset),
    ]);
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&inherited, &fixture.spawn());

    assert!(projected.env.get("GITHUB_TOKEN").is_none());
    assert_eq!(get(&projected.env, "OTHER_TOKEN").as_deref(), Some("mine"));
    assert_eq!(
        projected.warnings,
        [ProjectionWarning::CannotFullyIsolate {
            kept_changed: 1,
            kept_withheld: 0
        }]
    );
}

#[test]
fn isolate_reverts_session_records_too() {
    let lock = hermetic();
    ambient(&lock, "SESSION_VAR", "s");
    let mut session = constant_record("SESSION_VAR", "s", Prior::Unset);
    session.owner = Owner::Session {
        scope: ScopeId::Global,
        source: SessionSource::Composed,
    };
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&readable(vec![session]), &fixture.spawn());

    assert!(projected.env.get("SESSION_VAR").is_none());
}

#[test]
fn isolate_over_a_truncated_state_warns_even_when_nothing_was_kept() {
    let _lock = hermetic();
    let inherited = Inherited::Readable(ExecState {
        truncated: Truncation::AllAppliedDropped,
        ..ExecState::default()
    });
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&inherited, &fixture.spawn());

    assert_eq!(
        projected.warnings,
        [ProjectionWarning::CannotFullyIsolate {
            kept_changed: 0,
            kept_withheld: 0
        }]
    );
}

#[test]
fn only_isolate_warns_about_a_truncated_state() {
    for mode in [ExecMode::Layer, ExecMode::Clean] {
        let _lock = hermetic();
        let inherited = Inherited::Readable(ExecState {
            truncated: Truncation::InheritedDropped,
            ..ExecState::default()
        });
        let fixture = Fixture::new(mode);

        let projected = compose(&inherited, &fixture.spawn());

        assert!(projected.warnings.is_empty(), "{mode:?}: {:?}", projected.warnings);
    }
}

#[test]
fn an_absent_state_isolates_to_the_calling_environment_without_a_warning() {
    let lock = hermetic();
    ambient(&lock, "KEEP_ME", "ambient");
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(get(&projected.env, "KEEP_ME").as_deref(), Some("ambient"));
    assert!(projected.warnings.is_empty());
}

// ── An unreadable inherited state ───────────────────────────────────────────────────────────────

#[test]
fn an_unreadable_state_warns_once_naming_its_major_in_layer_and_clean() {
    for mode in [ExecMode::Layer, ExecMode::Clean] {
        let lock = hermetic();
        ambient(&lock, "KEEP_ME", "ambient");
        let fixture = Fixture::new(mode);

        let projected = compose(&unreadable(Some(3)), &fixture.spawn());

        assert_eq!(
            projected.warnings,
            [ProjectionWarning::InheritedUnreadable {
                received_major: Some(3),
                reason: UnreadableReason::OtherMajor
            }],
            "{mode:?}"
        );
    }
}

#[test]
fn a_same_major_state_that_failed_to_decode_names_the_field_and_is_not_called_incompatible() {
    let warning = ProjectionWarning::InheritedUnreadable {
        received_major: Some(ocx_config::exec_state::MAJOR),
        reason: UnreadableReason::Invalid {
            field: "session.project.dir",
        },
    };

    let text = warning.to_string();

    assert!(text.contains("invalid session.project.dir"), "{text}");
    assert!(!text.contains("incompatible"), "{text}");
    assert!(
        text.contains("policy header is honoured") && text.ends_with("unset __OCX_ENV_STATE to drop it"),
        "{text}"
    );
}

#[test]
fn a_different_major_keeps_the_incompatible_wording() {
    let text = ProjectionWarning::InheritedUnreadable {
        received_major: Some(3),
        reason: UnreadableReason::OtherMajor,
    }
    .to_string();

    assert!(
        text.contains("written by an incompatible ocx (state major 3)"),
        "{text}"
    );
}

#[test]
fn isolate_over_an_unreadable_state_proceeds_as_layer_and_says_so() {
    let lock = hermetic();
    ambient(&lock, "KEEP_ME", "ambient");
    let fixture = Fixture::new(ExecMode::Isolate);

    let projected = compose(&unreadable(None), &fixture.spawn());

    assert_eq!(get(&projected.env, "KEEP_ME").as_deref(), Some("ambient"));
    assert_eq!(
        projected.warnings,
        [
            ProjectionWarning::InheritedUnreadable {
                received_major: None,
                reason: UnreadableReason::OtherMajor
            },
            ProjectionWarning::CannotIsolate
        ]
    );
}

// ── The explicit-exec rule: `compose` never reads `inherited_scopes` ───────────────────

fn scoped(scopes: Vec<Scope>) -> Inherited {
    Inherited::Readable(ExecState {
        inherited_scopes: scopes,
        ..ExecState::default()
    })
}

#[test]
fn the_same_project_and_group_twice_composes_each_value_once() {
    let lock = hermetic();
    let project = Scope::Project {
        root: PathBuf::from("/fixture/project"),
    };
    // The outer exec already put g1 in front and the inherited list names the scope.
    ambient(&lock, "PATH", join(&["/g1/bin", "/usr/bin"]));
    ambient(&lock, "G1_VAR", "g1");
    ambient(&lock, "G1_OPTS", "-a");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_scope = Some(project.clone());
    fixture.composed = vec![
        path("PATH", "/g1/bin"),
        constant("G1_VAR", "g1"),
        list("G1_OPTS", "-a", " "),
    ];

    let projected = compose(&scoped(vec![project]), &fixture.spawn());

    assert_eq!(
        get(&projected.env, "PATH"),
        Some(join(&["/g1/bin", "/usr/bin"])),
        "each element once"
    );
    assert_eq!(get(&projected.env, "G1_VAR").as_deref(), Some("g1"));
    assert_eq!(get(&projected.env, "G1_OPTS").as_deref(), Some("-a"));
    assert!(applied(&projected, "PATH")[0].pre_existing);
}

#[test]
fn the_same_project_with_another_group_layers_on_top_even_though_the_scope_is_inherited() {
    let lock = hermetic();
    let project = Scope::Project {
        root: PathBuf::from("/fixture/project"),
    };
    ambient(&lock, "PATH", join(&["/g1/bin", "/usr/bin"]));
    ambient(&lock, "G1_VAR", "g1");
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed_scope = Some(project.clone());
    fixture.composed = vec![path("PATH", "/g2/bin"), constant("G2_VAR", "g2")];

    let projected = compose(&scoped(vec![project]), &fixture.spawn());

    assert_eq!(
        get(&projected.env, "PATH"),
        Some(join(&["/g2/bin", "/g1/bin", "/usr/bin"])),
        "g2 in front, g1 reachable"
    );
    assert_eq!(get(&projected.env, "G1_VAR").as_deref(), Some("g1"));
    assert_eq!(get(&projected.env, "G2_VAR").as_deref(), Some("g2"));
}

#[test]
fn the_composition_does_not_depend_on_which_scopes_are_inherited() {
    let project = Scope::Project {
        root: PathBuf::from("/fixture/project"),
    };
    let compose_with = |inherited: Inherited| {
        let lock = hermetic();
        ambient(&lock, "PATH", "/usr/bin");
        let mut fixture = Fixture::new(ExecMode::Layer);
        fixture.composed_scope = Some(project.clone());
        fixture.composed = vec![path("PATH", "/pkg/bin"), constant("JAVA_HOME", "/jdk")];
        let projected = compose(&inherited, &fixture.spawn());
        (
            get(&projected.env, "PATH"),
            get(&projected.env, "JAVA_HOME"),
            projected.state.applied,
        )
    };

    let without = compose_with(Inherited::Absent);
    for scopes in [
        vec![project.clone()],
        vec![Scope::Global],
        vec![Scope::Global, project.clone()],
    ] {
        assert_eq!(compose_with(scoped(scopes)), without);
    }
}

/// EC-REC-002, EC-SCOPE-001, EC-LIST-008: the composition order is the record order, global first and
/// project second, so a path lands in front with the project leading and a list appends with the project last.
#[test]
fn the_composition_order_is_the_record_order_and_decides_each_kinds_final_value() {
    let lock = hermetic();
    ambient(&lock, "PATH", join(&["/usr/bin"]));
    let mut fixture = Fixture::new(ExecMode::Layer);
    fixture.composed = vec![
        path("PATH", "/global/bin"),
        path("PATH", "/project/bin"),
        list("OCX_FLAGS", "global", ","),
        list("OCX_FLAGS", "project", ","),
    ];

    let projected = compose(&Inherited::Absent, &fixture.spawn());

    assert_eq!(
        get(&projected.env, "PATH"),
        Some(join(&["/project/bin", "/global/bin", "/usr/bin"]))
    );
    assert_eq!(get(&projected.env, "OCX_FLAGS").as_deref(), Some("global,project"));
    let elements: Vec<_> = applied(&projected, "PATH")
        .iter()
        .map(|record| record.value.clone())
        .collect();
    assert_eq!(
        elements,
        vec![
            RecordedValue::Literal("/global/bin".into()),
            RecordedValue::Literal("/project/bin".into())
        ]
    );
}
