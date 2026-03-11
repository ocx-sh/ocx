// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

pub mod bundle;
pub mod env;

#[derive(Debug, Clone, Deserialize, Serialize)]
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
}
