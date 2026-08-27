// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-prompt environment reconciler: the `__OCX_ENV_STATE` ledger, its
//! envelope codec, and the typed three-way [`plan`].
//!
//! The carrier is **untrusted input** (C-007): its only permitted effects are
//! naming the revert set and supplying the equality operand for the exit
//! guard. Nothing here constructs a path from it, re-grants consent, or
//! selects a value for a key it is not reverting.
//!
//! Split by concept ([ocx-sh/ocx#345](https://github.com/ocx-sh/ocx/issues/345)):
//! [`ledger`] is the carrier format, [`plan`] is the three-way planner, and
//! [`fingerprint`] is the watch-set fingerprint. This module retains only
//! what genuinely spans them — the key/element comparison and equivalence
//! primitives both the carrier's decode-time forging guard and the planner's
//! revert-set membership tests share.
//!
//! All three are **pure**, and that is what keeps `shell/` independent of
//! `project/`. The sequencing that binds them into one prompt's answer — and
//! which does read consent — lives at [`crate::activation`]; see its module
//! docs for why it is not a submodule here.

use std::ffi::OsStr;

use crate::package::metadata::env::entry::Entry;
use crate::package::metadata::env::list::DEFAULT_SEPARATOR;
use crate::package::metadata::env::modifier::ModifierKind;

mod fingerprint;
mod ledger;
mod plan;

pub use fingerprint::{current_fingerprint, fingerprint, watch_paths, watch_set_fingerprint};
pub use ledger::{
    Applied, CARRIER_KEY, LEDGER_VERSION, Ledger, LedgerEntry, MAX_CARRIER_BYTES, Prior, Priors, ProjectScope, ScopeId,
    Scopes, Verdict,
};
pub use plan::{PLAN_VERSION, Plan, capture_priors, emittable_entries, plan, summary};

// ---------------------------------------------------------------------------
// Shared primitives — span the carrier format (ledger) and the planner (plan)
// ---------------------------------------------------------------------------

/// The keys no scope may ever declare [`ModifierKind::Constant`] for (A-02).
const NEVER_CONSTANT: [&str; 2] = ["PATH", "PATHEXT"];

/// The comparison rule for a value, **selected by the kind that wrote it**.
///
/// The two rules are the emitters' own, and the planner must not be wider than
/// the arm that will render its decision — a comparison that calls two spellings
/// equal suppresses a removal the emitter would have applied byte-exact, and the
/// variable then accumulates both.
///
/// - [`ModifierKind::List`] — **byte-exact, case-sensitive on every platform**
///   ([`crate::shell::Shell::remove_list_element`], [`crate::shell::Shell::export_list`]).
///   A list element is an opaque option string: `-DFOO=1` and `-Dfoo=1` are
///   different options, and a `"` inside one is part of the option, never a
///   quoting artefact.
/// - [`ModifierKind::Path`] — A-19: segment-exact after stripping one
///   surrounding pair of `"` (`std::env::split_paths` unquotes on Windows, so
///   the operand ocx sees may carry a pair its own emit did not write),
///   case-sensitive on Unix and ASCII-case-insensitive on Windows.
/// - [`ModifierKind::Constant`] — the `C == L.applied` exit guard, which the ADR
///   pins to the same predicate as A-19 (ASCII-case-insensitive on Windows).
///
/// The stored string is never normalised — only the comparison is (C-008).
fn element_eq(left: &str, right: &str, kind: &ModifierKind) -> bool {
    match kind {
        ModifierKind::List => left == right,
        ModifierKind::Path | ModifierKind::Constant => {
            let (left, right) = (unquote(left), unquote(right));
            if cfg!(windows) {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        }
    }
}

/// The hashable form of [`element_eq`]'s equivalence class, under the same kind.
fn element_norm(value: &str, kind: &ModifierKind) -> String {
    match kind {
        ModifierKind::List => value.to_owned(),
        ModifierKind::Path | ModifierKind::Constant => {
            let value = unquote(value);
            if cfg!(windows) {
                value.to_ascii_lowercase()
            } else {
                value.to_owned()
            }
        }
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
}

/// Key equality as `EnvKey` already defines it: case-insensitive on Windows,
/// where `$env:Path` and `$env:PATH` are one variable, exact elsewhere.
fn key_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn key_norm(key: &str) -> String {
    if cfg!(windows) {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    }
}

fn is_never_constant(key: &str) -> bool {
    NEVER_CONSTANT.iter().any(|reserved| key_eq(key, reserved))
}

fn effective_separator(entry: &Entry) -> String {
    entry.separator.clone().unwrap_or_else(|| DEFAULT_SEPARATOR.to_owned())
}

fn os_to_string(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}
