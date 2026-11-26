// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-prompt environment reconciler: the typed session [`plan_session`] over the received
//! `__OCX_ENV_STATE`.
//!
//! The carrier is **untrusted input**: it may only name the revert set and supply the exit
//! guard's equality operand, never a path, a consent grant, or a value for a key it is not reverting.

use ocx_package::metadata::env::entry::Entry;
use ocx_package::metadata::env::list::DEFAULT_SEPARATOR;
use ocx_package::metadata::env::modifier::ModifierKind;

mod fingerprint;
mod plan;

pub use fingerprint::{WatchSet, current_fingerprint, fingerprint, watch_paths, watch_set_fingerprint};
pub use plan::{
    Composition, PLAN_VERSION, Plan, desired_composition, may_repair_owned_prefixes, plan_session, session_summary,
    session_writable,
};

/// The keys no scope may ever declare [`ModifierKind::Constant`] for.
const NEVER_CONSTANT: [&str; 2] = ["PATH", "PATHEXT"];

/// The comparison rule for a value, **selected by the kind that wrote it**.
///
/// Never wider than the emit arm: an equality the byte-exact emitter does not share suppresses a
/// removal, and the variable accumulates both.
/// Path and Constant strip one `"` pair because Windows `split_paths` unquotes the operand. Only a
/// path also folds `/` into `\`: a constant is an opaque string whose case rule alone is the host's.
fn element_eq(left: &str, right: &str, kind: &ModifierKind) -> bool {
    element_eq_as(left, right, kind, host_windows())
}

/// Whether the element rules are the Windows ones; a test can pin it for the current thread.
fn host_windows() -> bool {
    #[cfg(test)]
    if let Some(windows) = WINDOWS_RULE.with(std::cell::Cell::get) {
        return windows;
    }
    cfg!(windows)
}

#[cfg(test)]
thread_local! {
    static WINDOWS_RULE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Run `body` with the element rules pinned to the Windows (`true`) or byte-exact (`false`) rule, so a
/// Windows-only decision of the planner is testable on any host.
#[cfg(test)]
fn with_windows_rule<R>(windows: bool, body: impl FnOnce() -> R) -> R {
    // Restored on drop, so a panicking body cannot leave the rule pinned for the thread's next test.
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WINDOWS_RULE.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(WINDOWS_RULE.with(|cell| cell.replace(Some(windows))));
    body()
}

/// [`element_eq`] under an explicit Windows-ness, so the Windows rule is testable on any host.
fn element_eq_as(left: &str, right: &str, kind: &ModifierKind, windows: bool) -> bool {
    match kind {
        ModifierKind::List => left == right,
        ModifierKind::Path => ocx_env_apply::path_element_eq(
            std::ffi::OsStr::new(unquote(left)),
            std::ffi::OsStr::new(unquote(right)),
            windows,
        ),
        ModifierKind::Constant => {
            let (left, right) = (unquote(left), unquote(right));
            if windows {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        }
    }
}

/// The hashable form of [`element_eq`]'s equivalence class, under the same kind.
fn element_norm(value: &str, kind: &ModifierKind) -> String {
    element_norm_as(value, kind, host_windows())
}

fn element_norm_as(value: &str, kind: &ModifierKind, windows: bool) -> String {
    match kind {
        ModifierKind::List => value.to_owned(),
        ModifierKind::Path => ocx_env_apply::path_element_norm(unquote(value), windows),
        ModifierKind::Constant => {
            let value = unquote(value);
            if windows {
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

/// Key equality as `EnvKey` defines it: ASCII-case-insensitive on Windows only.
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
