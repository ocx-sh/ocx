use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Constant {
    pub value: String,
}

impl Constant {
    pub fn resolve(&self, _install_path: impl AsRef<std::path::Path>) -> String {
        self.value.clone()
    }
}
