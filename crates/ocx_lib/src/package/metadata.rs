// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

pub mod bundle;
pub mod dependency;
pub mod env;
pub mod visibility;

/// OCX package metadata.
///
/// Declares a package's type, version, extraction options, and environment variables.
/// Currently only the `bundle` type is supported.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Metadata {
    Bundle(bundle::Bundle),
}

impl Metadata {
    pub fn env(&self) -> Option<&env::Env> {
        match self {
            Metadata::Bundle(bundle) => Some(&bundle.env),
        }
    }

    pub fn dependencies(&self) -> &dependency::Dependencies {
        match self {
            Metadata::Bundle(bundle) => &bundle.dependencies,
        }
    }
}
