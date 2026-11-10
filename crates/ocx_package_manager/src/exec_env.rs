// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one projection every child spawn goes through: the environment a child receives and the
//! execution state it inherits.

use std::ffi::OsStr;
use std::path::Path;

use ocx_config::env::{Env, EnvKey};
use ocx_config::exec_state::{
    AppliedValue, ComposedValue, ExecState, Inherited, Owner, PolicyLayer, Prior, RecordedValue, RevertOutcome, Scope,
    Selection, SessionSource, Truncation, UnreadableReason, ValueKind, WriteMode, encode, revert,
};

mod assemble;
use ocx_env::Propagation;
use ocx_package::launch::LaunchIdentities;
use ocx_package::metadata::env::entry::Entry;
use ocx_package::metadata::env::list::DEFAULT_SEPARATOR;

/// How a spawn treats the calling environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// Compose on top of the calling environment.
    Layer,
    /// Revert every value ocx applied to the calling environment, then compose.
    Isolate,
    /// Start from an empty environment holding only the composed variables.
    Clean,
}

impl ExecMode {
    /// The policy mode this spawn mode writes: only Clean starts the child's policy over.
    pub fn write_mode(self) -> WriteMode {
        match self {
            Self::Layer | Self::Isolate => WriteMode::Inherit,
            Self::Clean => WriteMode::Clean,
        }
    }
}

/// Everything one spawn contributes to its child.
pub struct Spawn<'a> {
    pub mode: ExecMode,
    /// `PolicyInputs::to_write` for this mode.
    pub written: &'a PolicyLayer,
    /// The `OCX_BINARY_PIN` value; `None` removes the variable.
    pub binary_pin: Option<&'a Path>,
    /// Application order.
    pub composed: &'a [ComposedValue],
    /// `Inherited`, or a `Session` owner for `ocx shell`.
    pub composed_owner: Owner,
    /// The scope an `Inherited` composition came from; `Some` only for `ocx exec`.
    pub composed_scope: Option<Scope>,
    /// `ocx shell --with/--env`, recorded as session invocation values.
    pub invocation: &'a [ComposedValue],
    pub replay_env: &'a [ComposedValue],
    /// `(digest, names)` pairs.
    pub launch_identities: &'a [LaunchIdentity],
    pub selection: Selection,
}

/// One package a spawn composed, as the launcher needs to know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchIdentity {
    /// `sha256:<64 hex>`.
    pub digest: String,
    /// Package names, never a digest.
    pub names: Vec<String>,
    /// The composing project opted this package out of the patch tier.
    pub no_patches: bool,
}

/// The composed child environment and the state it carries, before encoding.
#[derive(Clone)]
pub struct Projected {
    pub env: Env,
    pub state: ExecState,
    pub warnings: Vec<ProjectionWarning>,
}

/// The finished environment of a child; callers spawn with `env_clear().envs(child.env)`.
#[derive(Clone)]
pub struct ChildEnvironment {
    pub env: Env,
    pub warnings: Vec<ProjectionWarning>,
}

/// A non-fatal finding of a projection; each is one ASCII stderr line naming its fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionWarning {
    /// The inherited state was unreadable; its header was honoured and the payload dropped.
    InheritedUnreadable {
        received_major: Option<u32>,
        reason: UnreadableReason,
    },
    /// Isolate could not revert values it could not see or that changed since ocx applied them.
    CannotFullyIsolate { kept_changed: usize, kept_withheld: usize },
    /// Isolate met an unreadable inherited state and proceeded as layer.
    CannotIsolate,
    /// The state outgrew the value cap and the encoder dropped records to fit it.
    StateTruncated { truncated: Truncation, encoded_len: usize },
}

/// What a truncation rung dropped, in words; the one table the spawn warning and the hook notice share.
pub(crate) fn truncation_words(truncated: Truncation) -> &'static str {
    match truncated {
        Truncation::None => "nothing was dropped",
        Truncation::InheritedDropped => "the records inherited from outer frames were dropped",
        Truncation::AllAppliedDropped => "every applied record was dropped",
        Truncation::PayloadDropped => "the whole payload was dropped",
    }
}

impl std::fmt::Display for ProjectionWarning {
    /// One ASCII line naming what happened and the way out.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InheritedUnreadable {
                received_major: Some(major),
                reason: UnreadableReason::OtherMajor,
            } => write!(
                f,
                "the execution state this process received was written by an incompatible ocx (state major {major}); \
                 its policy header is honoured and its values are ignored; unset __OCX_ENV_STATE to drop it"
            ),
            // A state of this ocx's own major that failed to decode: the major is not the cause, the reason is.
            Self::InheritedUnreadable {
                received_major: Some(_),
                reason,
            } => write!(
                f,
                "the execution state this process received could not be decoded ({reason}); \
                 its policy header is honoured and its values are ignored; unset __OCX_ENV_STATE to drop it"
            ),
            Self::InheritedUnreadable {
                received_major: None, ..
            } => f.write_str(
                "the execution state this process received is unreadable; its values are ignored; \
                 unset __OCX_ENV_STATE to drop it",
            ),
            Self::CannotFullyIsolate {
                kept_changed: 0,
                kept_withheld: 0,
            } => f.write_str(
                "cannot fully isolate: the received execution state was truncated, so some values it applied stay; \
                 use --clean for a pristine environment",
            ),
            Self::CannotFullyIsolate {
                kept_changed,
                kept_withheld,
            } => write!(
                f,
                "cannot fully isolate: kept {} values that changed since ocx applied them or whose prior value was \
                 not recorded; use --clean for a pristine environment",
                kept_changed + kept_withheld
            ),
            Self::CannotIsolate => f.write_str(
                "cannot isolate: the received execution state is unreadable, so the composition layers on top; \
                 use --clean for a pristine environment",
            ),
            Self::StateTruncated { truncated, encoded_len } => write!(
                f,
                "the execution state outgrew its size limit ({encoded_len} bytes) and was truncated: {}; a later \
                 --isolate cannot fully isolate; use --clean for a pristine environment",
                truncation_words(*truncated)
            ),
        }
    }
}

/// Composes the child's environment and state without encoding the state.
///
/// What is composed never depends on `inherited`'s scopes: an explicit exec composes everything it was
/// asked for, whatever an outer frame already loaded. The inherited state decides only what Isolate reverts.
pub fn compose(inherited: &Inherited, spawn: &Spawn<'_>) -> Projected {
    let mut warnings = Vec::new();
    let mut env = match spawn.mode {
        ExecMode::Clean => {
            let mut env = Env::clean();
            if cfg!(windows) {
                carry_system_root(&mut env, &Env::new());
            }
            env
        }
        ExecMode::Layer | ExecMode::Isolate => Env::new(),
    };

    if let Inherited::Unreadable { header, reason } = inherited {
        warnings.push(ProjectionWarning::InheritedUnreadable {
            received_major: header.major,
            reason: *reason,
        });
    }
    if spawn.mode == ExecMode::Isolate {
        match inherited {
            Inherited::Readable(state) => warnings.extend(isolate(&mut env, state)),
            Inherited::Unreadable { .. } => warnings.push(ProjectionWarning::CannotIsolate),
            Inherited::Absent => {}
        }
    }

    remove_unforwarded(&mut env);
    let records = apply_composition(&mut env, spawn);
    set_binary_pin(&mut env, spawn.binary_pin);

    let state = assemble::state(inherited, spawn, records);
    Projected { env, state, warnings }
}

/// Reverts every recorded value of `state`, newest first, and reports what could not be undone.
fn isolate(env: &mut Env, state: &ExecState) -> Option<ProjectionWarning> {
    let (mut kept_changed, mut kept_withheld) = (0, 0);
    for record in state.applied.iter().rev() {
        match revert(env, record) {
            RevertOutcome::Reverted => {}
            // A pre-existing element is an expected survivor, not something that went wrong.
            RevertOutcome::Kept if record.pre_existing => {}
            RevertOutcome::Kept => kept_changed += 1,
            RevertOutcome::Withheld => kept_withheld += 1,
        }
    }
    let truncated = state.truncated != Truncation::None;
    (kept_changed > 0 || kept_withheld > 0 || truncated).then_some(ProjectionWarning::CannotFullyIsolate {
        kept_changed,
        kept_withheld,
    })
}

/// Copies the caller's `SystemRoot` into `env` unless `env` already has one.
///
/// Windows cannot start a child without it: a native arm64 `cmd.exe` exits 1 silently. Go's `os/exec`
/// adds it for the same reason.
fn carry_system_root(env: &mut Env, ambient: &Env) {
    if env.get(SYSTEM_ROOT).is_none()
        && let Some(root) = ambient.get(SYSTEM_ROOT)
    {
        env.set(SYSTEM_ROOT, root);
    }
}

const SYSTEM_ROOT: &str = "SystemRoot";

/// Removes what never reaches a child from the caller: credentials, hook-local bookkeeping and the
/// received state, which [`project`] replaces with the one it encodes.
fn remove_unforwarded(env: &mut Env) {
    let unforwarded = [Propagation::Credential, Propagation::HookLocal, Propagation::State];
    for key in unforwarded.into_iter().flat_map(ocx_env::keys_with) {
        env.remove(key);
    }
}

/// Sets `OCX_BINARY_PIN`, or removes it when the pin is unknown. Never writes an empty value.
fn set_binary_pin(env: &mut Env, pin: Option<&Path>) {
    match pin.map(Path::as_os_str).filter(|pin| !pin.is_empty()) {
        Some(pin) => env.set(ocx_env::OCX_BINARY_PIN.name, pin),
        None => env.remove(ocx_env::OCX_BINARY_PIN.name),
    }
}

/// Applies the composed and invocation values to `env` in order; returns what to record.
///
/// Priors and `pre_existing` are read from the environment as it was before any composition, so a record
/// describes what it replaced and not what an earlier value of the same spawn left behind.
fn apply_composition(env: &mut Env, spawn: &Spawn<'_>) -> Vec<AppliedValue> {
    let start = env.clone();
    let invocation_owner = match &spawn.composed_owner {
        Owner::Session { scope, .. } => Owner::Session {
            scope: *scope,
            source: SessionSource::Invocation,
        },
        Owner::Inherited => Owner::Inherited,
    };
    let mut applied: Vec<AppliedValue> = Vec::new();
    let values = spawn
        .composed
        .iter()
        .map(|value| (value, &spawn.composed_owner))
        .chain(spawn.invocation.iter().map(|value| (value, &invocation_owner)));
    for (value, owner) in values {
        apply_value(env, value);
        if let Some(record) = record_of(&start, value, owner) {
            fold_into(&mut applied, record);
        }
    }
    applied
}

fn apply_value(env: &mut Env, value: &ComposedValue) {
    match value.kind {
        ValueKind::Path => {
            // Before the note, which derives from the value already on `PATH`.
            env.add_path(&value.key, &value.value);
            if same_key(&value.key, "PATH") {
                env.note_package_path(OsStr::new(&value.value));
            }
        }
        ValueKind::List => env.add_list(&value.key, &value.value, effective_separator(value)),
        ValueKind::Constant => env.set(&value.key, &value.value),
    }
}

fn effective_separator(value: &ComposedValue) -> &str {
    value
        .separator
        .as_deref()
        .filter(|separator| !separator.is_empty())
        .unwrap_or(DEFAULT_SEPARATOR)
}

fn same_key(a: &str, b: &str) -> bool {
    EnvKey::new(a) == EnvKey::new(b)
}

/// Whether the secret rule forbids any record for `key`: a credential, or an `OCX_AUTH_*` key.
fn is_never_recorded(key: &str) -> bool {
    const AUTH_PREFIX: &str = "OCX_AUTH_";
    ocx_env::keys_with(Propagation::Credential).any(|credential| credential.eq_ignore_ascii_case(key))
        || key
            .get(..AUTH_PREFIX.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(AUTH_PREFIX))
}

/// The record of one applied value, or `None` when it must not be recorded.
fn record_of(start: &Env, value: &ComposedValue, owner: &Owner) -> Option<AppliedValue> {
    if is_never_recorded(&value.key) {
        return None;
    }
    let record =
        |recorded: RecordedValue, pre_existing: bool, separator: Option<String>, prior: Option<Prior>| AppliedValue {
            key: value.key.clone(),
            value: recorded,
            kind: value.kind,
            separator,
            pre_existing,
            owner: owner.clone(),
            prior,
        };
    match value.kind {
        // An empty element changes nothing.
        ValueKind::Path | ValueKind::List if value.value.is_empty() => None,
        ValueKind::Path => {
            let present = start
                .get(&value.key)
                .is_some_and(|existing| path_holds(existing, &value.value));
            Some(record(RecordedValue::Literal(value.value.clone()), present, None, None))
        }
        ValueKind::List => {
            let separator = effective_separator(value);
            let present = start
                .get(&value.key)
                .is_some_and(|existing| list_holds(&existing.to_string_lossy(), &value.value, separator));
            Some(record(
                RecordedValue::Literal(value.value.clone()),
                present,
                Some(separator.to_owned()),
                None,
            ))
        }
        // A constant `PATH`/`PATHEXT` record would make the state unreadable.
        ValueKind::Constant if same_key(&value.key, "PATH") || same_key(&value.key, "PATHEXT") => None,
        ValueKind::Constant => {
            let before = start.get(&value.key);
            let prior = match before {
                None => Prior::Unset,
                Some(existing) => match existing.to_str() {
                    Some(existing) => Prior::Value(existing.to_owned()),
                    None => Prior::Withheld,
                },
            };
            Some(record(
                RecordedValue::Literal(value.value.clone()),
                false,
                None,
                Some(prior),
            ))
        }
    }
}

fn path_holds(existing: &OsStr, element: &str) -> bool {
    let remaining = ocx_util::path::remove_segment(existing, OsStr::new(element));
    let segments = |value: &OsStr| {
        std::env::split_paths(value)
            .filter(|segment| !segment.as_os_str().is_empty())
            .count()
    };
    segments(&remaining) != segments(existing)
}

fn list_holds(existing: &str, element: &str, separator: &str) -> bool {
    format!("{separator}{existing}{separator}").contains(&format!("{separator}{element}{separator}"))
}

/// Adds `record` to this spawn's records, folding a repeat of the same key.
///
/// A path or list element applied twice is one record (the first, whose `pre_existing` describes the
/// start environment). A constant applied twice keeps the newer value and the start environment's prior,
/// which is what coalescing two continuous records would give.
fn fold_into(applied: &mut Vec<AppliedValue>, record: AppliedValue) {
    let same = |existing: &AppliedValue| existing.kind == record.kind && same_key(&existing.key, &record.key);
    match record.kind {
        ValueKind::Constant => {
            if let Some(at) = applied.iter().position(same) {
                applied.remove(at);
            }
        }
        ValueKind::Path | ValueKind::List => {
            if applied
                .iter()
                .any(|existing| same(existing) && existing.value == record.value)
            {
                return;
            }
        }
    }
    applied.push(record);
}

/// [`compose`], then encodes the state into `__OCX_ENV_STATE` and sets the binary pin.
///
/// The state handed to the encoder already carries the inherited truncation under Layer and Isolate and
/// none under Clean, so the rung the encoder reports is sticky exactly where the mode table says. A rung
/// beyond what the state already carried is the spawn's own and is reported once.
pub fn project(inherited: &Inherited, spawn: &Spawn<'_>) -> ChildEnvironment {
    let Projected {
        mut env,
        state,
        mut warnings,
    } = compose(inherited, spawn);
    let encoded = encode(&state);
    match encoded.value {
        Some(value) => env.set(ocx_env::__OCX_ENV_STATE.name, value),
        None => env.remove(ocx_env::__OCX_ENV_STATE.name),
    }
    if encoded.truncated > state.truncated {
        warnings.push(ProjectionWarning::StateTruncated {
            truncated: encoded.truncated,
            encoded_len: encoded.encoded_len,
        });
    }
    ChildEnvironment { env, warnings }
}

/// Inputs of [`apply_child_env`] in the shapes the spawn sites hold today.
pub struct ChildEnvInputs<'a> {
    pub mode: ExecMode,
    pub written: &'a PolicyLayer,
    pub binary_pin: Option<&'a Path>,
    pub composed: &'a [Entry],
    pub composed_owner: Owner,
    pub composed_scope: Option<Scope>,
    /// `ocx shell --with/--env`, recorded as session invocation values; empty for every other site.
    pub invocation: &'a [Entry],
    pub replay_env: &'a [Entry],
    pub identities: Option<&'a LaunchIdentities>,
    pub selection: Selection,
}

/// Adapter for the spawn sites: converts entries and identities, then calls [`project`].
///
/// A spawn site runs the child with `env_clear().envs(child.env)`.
pub fn apply_child_env(inherited: &Inherited, inputs: &ChildEnvInputs<'_>) -> ChildEnvironment {
    let composed: Vec<ComposedValue> = inputs.composed.iter().map(ComposedValue::from).collect();
    let invocation: Vec<ComposedValue> = inputs.invocation.iter().map(ComposedValue::from).collect();
    let replay_env: Vec<ComposedValue> = inputs.replay_env.iter().map(ComposedValue::from).collect();
    let launch_identities = launch_identities_of(inputs.identities);
    let spawn = Spawn {
        mode: inputs.mode,
        written: inputs.written,
        binary_pin: inputs.binary_pin,
        composed: &composed,
        composed_owner: inputs.composed_owner.clone(),
        composed_scope: inputs.composed_scope.clone(),
        invocation: &invocation,
        replay_env: &replay_env,
        launch_identities: &launch_identities,
        selection: inputs.selection.clone(),
    };
    project(inherited, &spawn)
}

/// The identities a spawn composed, as the projection takes them; none when the site composes none.
fn launch_identities_of(identities: Option<&LaunchIdentities>) -> Vec<LaunchIdentity> {
    identities
        .map(|identities| {
            identities
                .to_records(Owner::Inherited)
                .into_iter()
                .map(|record| LaunchIdentity {
                    digest: record.digest,
                    names: record.names,
                    no_patches: record.no_patches,
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod adapter_tests;
#[cfg(test)]
mod baseline_tests;
#[cfg(test)]
mod compose_tests;
#[cfg(test)]
mod registry_tests;
#[cfg(test)]
mod state_tests;
#[cfg(test)]
mod tests {
    mod project;
}
