use serde::{Deserialize, Serialize};

use super::{constant, path};

pub use super::modifier::{Modifier, ModifierKind};

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
