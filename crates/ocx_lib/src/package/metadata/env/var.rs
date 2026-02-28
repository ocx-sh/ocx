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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Var {
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
