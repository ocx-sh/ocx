// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{constant, path};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Modifier {
    Path(path::Path),
    Constant(constant::Constant),
}

/// The modifier kind stripped of inner data — suitable for display and serialization.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModifierKind {
    Path,
    Constant,
}

impl From<&Modifier> for ModifierKind {
    fn from(modifier: &Modifier) -> Self {
        match modifier {
            Modifier::Path(_) => ModifierKind::Path,
            Modifier::Constant(_) => ModifierKind::Constant,
        }
    }
}

impl fmt::Display for ModifierKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModifierKind::Path => write!(f, "path"),
            ModifierKind::Constant => write!(f, "constant"),
        }
    }
}
