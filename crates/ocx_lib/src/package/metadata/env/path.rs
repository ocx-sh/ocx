// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

/// A path-type environment variable.
///
/// Path variables are prepended to any existing value of the environment variable.
/// The `${installPath}` template is replaced with the package's content directory at resolution time.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub struct Path {
    /// Whether the resolved path must exist on disk. If `true` and the path is missing, installation fails.
    /// Defaults to `false`.
    #[serde(default)]
    pub required: bool,

    /// The value template. Use `${installPath}` to reference the package content directory.
    pub value: String,
}
