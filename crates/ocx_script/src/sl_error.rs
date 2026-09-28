// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Internal constructors mapping host-fn failures onto Starlark error kinds.

use starlark::Error as StarlarkError;

/// Carrier for a host-fn failure message; `engine::classify` reads back only its `Display` text.
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
