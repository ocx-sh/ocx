// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

/// A constant-type environment variable.
///
/// Constant variables replace any existing value of the environment variable.
/// The `${installPath}` template is replaced with the package's content directory at resolution time.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Constant {
    /// The value template. Use `${installPath}` to reference the package content directory.
    pub value: String,
}

impl Constant {
    pub fn resolve(&self, _install_path: impl AsRef<std::path::Path>) -> String {
        self.value.clone()
    }
}
