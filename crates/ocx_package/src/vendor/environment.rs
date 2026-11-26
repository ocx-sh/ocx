// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::metadata::env::modifier::ModifierKind;

/// A composed environment; the only non-literal text in a value is the dir token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Environment {
    /// Opaque: `selection:default`, or `entrypoint:<digest>:<name>`.
    pub id: String,
    /// What the environment belongs to.
    pub scope: EnvironmentScope,
    /// In application order.
    pub items: Vec<EnvironmentItem>,
}

/// The owner of an [`Environment`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvironmentScope {
    /// The selection's composed environment.
    Selection {
        /// Selection id.
        selection: String,
    },
    /// An entry point's private environment.
    Entrypoint {
        /// Digest of the owning package.
        package: String,
        /// Entry-point name.
        name: String,
    },
}

/// One entry of an environment, shaped like the `ocx env` report's entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnvironmentItem {
    /// The variable name.
    pub key: String,
    /// The value; may contain the dir token.
    pub value: String,
    /// How the value combines with the existing one.
    pub kind: ModifierKind,
    /// The fold separator; `list` entries only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<String>,
}
