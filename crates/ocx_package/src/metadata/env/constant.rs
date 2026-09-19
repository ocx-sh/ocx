// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

/// A constant-type environment variable.
///
/// Constant variables replace any existing value of the environment variable.
/// Interpolation tokens in `value` are replaced at resolution time.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Constant {
    /// The value template. `${installPath}` — or its alias `${self.installPath}` — is this package's
    /// content directory, `${deps.NAME.installPath}` a declared dependency's, and `${self.env.KEY}` the
    /// resolved value of a variable declared earlier in this same list. Append `:native` or `:posix` to
    /// pick the path style. Every other `${...}` is rejected; write `$${` for a literal `${`.
    pub value: String,
}

impl Constant {
    pub fn resolve(&self, _install_path: impl AsRef<std::path::Path>) -> String {
        self.value.clone()
    }
}
