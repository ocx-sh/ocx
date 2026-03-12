// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use super::{constant, path};

pub use super::modifier::{Modifier, ModifierKind};

/// An environment variable declaration.
///
/// Each variable has a key (the variable name) and a modifier that determines
/// how the value is resolved. The modifier's type and fields are flattened into
/// this object in JSON.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "jsonschema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub struct Var {
    /// The environment variable name (e.g. `PATH`, `JAVA_HOME`).
    pub key: String,

    #[serde(flatten)]
    pub modifier: Modifier,
}

impl Var {
    pub fn new_path(key: impl ToString, value: impl ToString, required: bool) -> Self {
        Var {
            key: key.to_string(),
            modifier: Modifier::Path(path::Path {
                required,
                value: value.to_string(),
            }),
        }
    }

    pub fn new_constant(key: impl ToString, value: impl ToString) -> Self {
        Var {
            key: key.to_string(),
            modifier: Modifier::Constant(constant::Constant {
                value: value.to_string(),
            }),
        }
    }

    pub fn value(&self) -> Option<&str> {
        match &self.modifier {
            Modifier::Path(path_var) => Some(&path_var.value),
            Modifier::Constant(constant_var) => Some(&constant_var.value),
        }
    }
}
