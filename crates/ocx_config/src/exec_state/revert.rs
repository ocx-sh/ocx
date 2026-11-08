// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Undoing one recorded value against an environment.

use std::ffi::{OsStr, OsString};

use crate::env::Env;

use super::types::{AppliedValue, Prior, RecordedValue, ValueKind};

/// What reverting one record did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevertOutcome {
    Reverted,
    /// Kept: the value changed since ocx applied it, or the element pre-existed.
    Kept,
    /// Kept: the prior was withheld or the record was not removable.
    Withheld,
}

/// Reverts one record in `env`: a record only undoes itself.
///
/// A path or list element is removed unless it pre-existed. A constant is restored to its prior only
/// while it still holds the recorded value, byte for byte, so a forged record cannot delete a variable
/// the user holds under another value.
///
/// Keys compare as [`Env`] folds them, which is case-insensitively on Windows.
pub fn revert(env: &mut Env, record: &AppliedValue) -> RevertOutcome {
    let RecordedValue::Literal(value) = &record.value;
    match record.kind {
        ValueKind::Path => revert_path(env, record, value),
        ValueKind::List => revert_list(env, record, value),
        ValueKind::Constant => revert_literal_constant(env, record, value),
    }
}

/// Sets `key` to `value`, or removes it when nothing is left.
fn set_or_remove(env: &mut Env, key: &str, value: OsString) {
    if value.is_empty() {
        env.remove(key);
    } else {
        env.set(key, value);
    }
}

fn revert_path(env: &mut Env, record: &AppliedValue, element: &str) -> RevertOutcome {
    if record.pre_existing {
        return RevertOutcome::Kept;
    }
    // An empty element would make the removal a wipe of the empty segments instead.
    if element.is_empty() {
        return RevertOutcome::Withheld;
    }
    let Some(existing) = env.get(&record.key) else {
        return RevertOutcome::Kept;
    };
    let remaining = ocx_util::path::remove_segment(existing, OsStr::new(element));
    let segments = |value: &OsStr| {
        std::env::split_paths(value)
            .filter(|s| !s.as_os_str().is_empty())
            .count()
    };
    if segments(&remaining) == segments(existing) {
        return RevertOutcome::Kept;
    }
    set_or_remove(env, &record.key, remaining);
    RevertOutcome::Reverted
}

fn revert_list(env: &mut Env, record: &AppliedValue, element: &str) -> RevertOutcome {
    if record.pre_existing {
        return RevertOutcome::Kept;
    }
    // `append_unique` cuts a value flanked by its separator out of its neighbours, and an empty
    // separator or element makes the removal a wipe.
    let Some(separator) = record.separator.as_deref().filter(|s| !s.is_empty()) else {
        return RevertOutcome::Withheld;
    };
    if element.is_empty() || element.starts_with(separator) || element.ends_with(separator) {
        return RevertOutcome::Withheld;
    }
    let Some(existing) = env.get(&record.key) else {
        return RevertOutcome::Kept;
    };
    // A lossy read would write back replacement characters over bytes this record never wrote.
    let Some(existing) = existing.to_str().map(str::to_owned) else {
        return RevertOutcome::Withheld;
    };
    // Removal is `append_unique` minus its re-append, so the flank rule has one copy.
    let appended = ocx_util::list::append_unique(&existing, element, separator);
    let tail = format!("{separator}{element}");
    let remaining = appended.strip_suffix(&tail).unwrap_or_default();
    if remaining == existing {
        return RevertOutcome::Kept;
    }
    set_or_remove(env, &record.key, OsString::from(remaining));
    RevertOutcome::Reverted
}

fn revert_literal_constant(env: &mut Env, record: &AppliedValue, value: &str) -> RevertOutcome {
    // Byte-exact (L-1, L-2): a value the user changed since ocx wrote it is theirs.
    if env.get(&record.key) != Some(OsStr::new(value)) {
        return RevertOutcome::Kept;
    }
    match &record.prior {
        Some(Prior::Value(prior)) => {
            env.set(record.key.as_str(), prior.as_str());
            RevertOutcome::Reverted
        }
        Some(Prior::Unset) => {
            env.remove(&record.key);
            RevertOutcome::Reverted
        }
        Some(Prior::Withheld) | None => RevertOutcome::Withheld,
    }
}

#[cfg(test)]
mod tests;
