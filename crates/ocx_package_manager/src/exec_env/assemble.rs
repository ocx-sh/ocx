// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The state a child inherits: what a spawn carries forward and what it writes anew.

use std::collections::{BTreeMap, BTreeSet};

use ocx_config::exec_state::{
    AppliedValue, ComposedValue, ExecState, IdentityRecord, Inherited, Prior, RecordedValue, Replay, ReplayPayload,
    ReplayValue, SessionCache, Truncation, ValueKind,
};

use super::{ExecMode, LaunchIdentity, Spawn, effective_separator, is_never_recorded, same_key};

/// Builds the child's state from what the inherited state carries and what this spawn applied.
///
/// Layer carries the inherited records, scopes and identities forward (session ones retagged inherited);
/// Isolate and Clean start from this spawn alone. `truncated` is sticky across Layer and Isolate only.
pub(super) fn state(inherited: &Inherited, spawn: &Spawn<'_>, own_records: Vec<AppliedValue>) -> ExecState {
    let received = match inherited {
        Inherited::Readable(state) => Some(state),
        Inherited::Absent | Inherited::Unreadable { .. } => None,
    };
    let carried = match (spawn.mode, received) {
        (ExecMode::Layer, Some(state)) => {
            let mut carried = state.clone();
            carried.retag_sessions();
            Some(carried)
        }
        _ => None,
    };

    let mut applied: Vec<AppliedValue> = Vec::new();
    let carried_records = carried.iter().flat_map(|state| state.applied.iter().cloned());
    for record in carried_records.chain(own_records) {
        coalesce(&mut applied, record);
    }

    let mut state = ExecState {
        narrowing: spawn.written.narrowing,
        home: spawn.written.home.clone(),
        policy: spawn.written.policy.clone(),
        applied,
        inherited_scopes: carried.as_ref().map(|c| c.inherited_scopes.clone()).unwrap_or_default(),
        replay: Replay {
            env: replay_env(spawn.replay_env),
            launch_identities: identities(carried.as_ref(), spawn),
        },
        selection: spawn.selection.clone(),
        session: SessionCache::default(),
        truncated: match (spawn.mode, received) {
            (ExecMode::Layer | ExecMode::Isolate, Some(state)) => state.truncated,
            _ => Truncation::None,
        },
        ..ExecState::default()
    };
    if let Some(scope) = &spawn.composed_scope {
        state.add_scope(scope.clone());
    }
    state
}

/// Adds `record` to `applied`, folding it into an older record where coalescing allows.
///
/// A path or list element keeps one record per `(key, element)` with the oldest `pre_existing`. A constant
/// merges into the key's latest record only when one owner holds both and the chain is continuous: the
/// newer prior is the older literal. The merge keeps the newer value and the older prior. A record holding
/// no literal, whose prior differs (a user changed the variable in between) or whose owner differs stays
/// separate.
fn coalesce(applied: &mut Vec<AppliedValue>, record: AppliedValue) {
    match record.kind {
        ValueKind::Path | ValueKind::List => {
            let repeated = applied.iter().any(|existing| {
                existing.kind == record.kind
                    && same_key(&existing.key, &record.key)
                    && existing.value == record.value
                    && existing.separator == record.separator
            });
            if !repeated {
                applied.push(record);
            }
        }
        ValueKind::Constant => {
            let latest = applied
                .iter()
                .rposition(|existing| existing.kind == ValueKind::Constant && same_key(&existing.key, &record.key));
            // A session record merged into an inherited one would leave the hook no record of its own
            // value to refresh or undo.
            let continues = |older: &AppliedValue| {
                older.owner == record.owner
                    && match (&older.value, &record.value, &record.prior) {
                        (RecordedValue::Literal(held), RecordedValue::Literal(_), Some(Prior::Value(prior))) => {
                            held == prior
                        }
                        _ => false,
                    }
            };
            match latest {
                Some(at) if continues(&applied[at]) => {
                    applied[at] = AppliedValue {
                        prior: applied[at].prior.clone(),
                        ..record
                    };
                }
                _ => applied.push(record),
            }
        }
    }
}

/// This spawn's replay payload, in order, every value literal; a credential or `OCX_AUTH_*` key is not
/// replayed at all, since every spawn strips it.
fn replay_env(values: &[ComposedValue]) -> Vec<ReplayValue> {
    values
        .iter()
        .filter(|value| !is_never_recorded(&value.key))
        .map(|value| ReplayValue {
            key: value.key.clone(),
            kind: value.kind,
            separator: (value.kind == ValueKind::List).then(|| effective_separator(value).to_owned()),
            value: ReplayPayload::Literal(value.value.clone()),
        })
        .collect()
}

/// One record per digest, in digest order. A newer record replaces an older one for its digest whole,
/// so one owner's `no_patches` never rides along into another's record.
fn identities(carried: Option<&ExecState>, spawn: &Spawn<'_>) -> Vec<IdentityRecord> {
    let mut merged: BTreeMap<String, IdentityRecord> = BTreeMap::new();
    for record in carried.iter().flat_map(|state| state.replay.launch_identities.iter()) {
        merged.insert(record.digest.clone(), record.clone());
    }
    for (digest, (names, no_patches)) in spawn_identities(spawn.launch_identities) {
        merged.insert(
            digest.clone(),
            IdentityRecord {
                digest,
                names: names.into_iter().collect(),
                owner: spawn.composed_owner.clone(),
                no_patches,
            },
        );
    }
    merged.into_values().collect()
}

/// This spawn's identities by digest. One owner composed them all, so repeats of a digest union.
fn spawn_identities(identities: &[LaunchIdentity]) -> BTreeMap<String, (BTreeSet<String>, bool)> {
    let mut by_digest: BTreeMap<String, (BTreeSet<String>, bool)> = BTreeMap::new();
    for identity in identities {
        let entry = by_digest.entry(identity.digest.clone()).or_default();
        entry.0.extend(identity.names.iter().cloned());
        entry.1 |= identity.no_patches;
    }
    by_digest
}
