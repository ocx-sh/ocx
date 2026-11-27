// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The packages a run selected and how they are exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Selection {
    /// `default` in v1.
    pub id: String,
    /// Groups as given.
    pub groups: Vec<String>,
    /// Bindings as given, resolved from `-g` included.
    pub bindings: Vec<String>,
    /// Digests of the selected packages, in selection order.
    pub roots: Vec<String>,
    /// Id of the composed environment.
    pub environment: String,
    /// `l`.
    pub launcher_dir: String,
}

/// An exposed name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Tool {
    /// The exposed name.
    pub name: String,
    /// Selection id.
    pub selection: String,
    /// Where the name comes from.
    pub kind: ToolKind,
    /// Digest of the owning package (last walked wins).
    pub package: String,
    /// The selection's environment id.
    pub environment: String,
    /// The binary file, or the package's entry-point launcher.
    pub target_path: String,
    /// `l/<name>[.exe]`; omitted when no launcher blob exists for the platform.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher_path: Option<String>,
    /// `l/<name>.launch`, the record the launcher reads; present exactly when `launcher_path` is.
    #[schemars(extend("x-ocx-absent-when-none" = true))]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_path: Option<String>,
}

/// Origin of a [`Tool`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// A binary of the package.
    Binary,
    /// An entry-point launcher.
    Entrypoint,
}
