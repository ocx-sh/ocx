// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Internal constructors mapping host-fn failures onto Starlark error kinds.
//!
//! These stay inside the firewall: no Starlark `ErrorKind` variant name nor
//! `anyhow` is referenced by `ocx_lib` code. `starlark::Error::new_native` /
//! `new_value` accept `impl Into<anyhow::Error>`; a `thiserror` error
//! satisfies that via anyhow's blanket `From<E: std::error::Error>` *inside
//! starlark*, so `ocx_lib` never names `anyhow` (it stays a test-only
//! dev-dependency per `quality-rust-errors.md`).
//!
//! - [`fail`] → native-kind error → `Failed` (exit 1): assertion failure,
//!   `expect.fail`, sandbox rejection, or a runtime host failure.
//! - [`script_type`] → value-kind error → `ScriptError` (exit 65): the script
//!   passed a wrong-typed / invalid value to a host fn.

use starlark::Error as StarlarkError;

/// Firewall-internal carrier for a host-fn failure message.
///
/// Only its `Display` text matters — `engine::classify` reads it back via
/// `error.to_string()` for the report envelope.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct HostFnError(String);

/// A host-reported runtime / assertion / sandbox failure → `Failed` (exit 1).
pub(super) fn fail(message: impl Into<String>) -> StarlarkError {
    StarlarkError::new_native(HostFnError(message.into()))
}

/// A script-supplied value was the wrong type / invalid → `ScriptError` (65).
pub(super) fn script_type(message: impl Into<String>) -> StarlarkError {
    StarlarkError::new_value(HostFnError(message.into()))
}
